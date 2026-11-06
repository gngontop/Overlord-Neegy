use std::time::Duration;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tokio_rustls::TlsConnector;

use crate::tls;

const MAX_BODY: usize = 512 * 1024 * 1024;
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

pub fn resolve_url(server: &str, value: &str) -> String {
    if value.starts_with("http://") || value.starts_with("https://") {
        return value.to_owned();
    }
    let base = server.trim_end_matches('/');
    let base = if let Some(rest) = base.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = base.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        base.to_owned()
    };
    if value.starts_with('/') {
        format!("{base}{value}")
    } else {
        format!("{base}/{value}")
    }
}

pub async fn get(
    url: &str,
    insecure: bool,
    pins: &[[u8; 32]],
) -> Result<Vec<u8>, String> {
    let parsed = url::Url::parse(url).map_err(|error| error.to_string())?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err("unsupported URL scheme".to_owned());
    }
    let host = parsed.host_str().ok_or_else(|| "missing host".to_owned())?;
    let tls_enabled = scheme == "https";
    let port = parsed
        .port()
        .unwrap_or(if tls_enabled { 443 } else { 80 });
    let mut path = parsed.path().to_owned();
    if path.is_empty() {
        path.push('/');
    }
    if let Some(query) = parsed.query() {
        path.push('?');
        path.push_str(query);
    }
    let host_header = if (tls_enabled && port == 443) || (!tls_enabled && port == 80) {
        host.to_owned()
    } else {
        format!("{host}:{port}")
    };

    let stream = tokio::time::timeout(Duration::from_secs(30), TcpStream::connect((host, port)))
        .await
        .map_err(|_| "connect timeout".to_owned())?
        .map_err(|error| error.to_string())?;
    stream
        .set_nodelay(true)
        .map_err(|error| error.to_string())?;

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: {USER_AGENT}\r\nAccept: */*\r\nAccept-Language: en-US,en;q=0.9\r\nConnection: close\r\n\r\n"
    );

    let mut buffer = Vec::new();
    if tls_enabled {
        let config = tls::build_client_config(insecure, pins);
        let connector = TlsConnector::from(config);
        let server_name = rustls::pki_types::ServerName::try_from(host.to_owned())
            .map_err(|error| error.to_string())?;
        let mut tls_stream = tokio::time::timeout(
            Duration::from_secs(30),
            connector.connect(server_name, stream),
        )
        .await
        .map_err(|_| "tls handshake timeout".to_owned())?
        .map_err(|error| error.to_string())?;
        tls_stream
            .write_all(request.as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        read_response(&mut tls_stream, &mut buffer).await?;
    } else {
        let mut plain = stream;
        plain
            .write_all(request.as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        read_response(&mut plain, &mut buffer).await?;
    }
    parse_response(&buffer)
}

async fn read_response<S>(stream: &mut S, buffer: &mut Vec<u8>) -> Result<(), String>
where
    S: AsyncReadExt + Unpin,
{
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > MAX_BODY {
            return Err("response too large".to_owned());
        }
    }
    Ok(())
}

fn parse_response(buffer: &[u8]) -> Result<Vec<u8>, String> {
    let header_end = find_subslice(buffer, b"\r\n\r\n").ok_or_else(|| "malformed response".to_owned())?;
    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = headers.lines();
    let status = lines.next().unwrap_or_default();
    if !status.contains(" 200") && !status.contains(" 201") && !status.contains(" 206") && !status.contains(" 204") {
        return Err(format!("http status {status}"));
    }
    let mut chunked = false;
    let mut content_length: Option<usize> = None;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
        if let Some(value) = lower.strip_prefix("content-length:") {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    let body = &buffer[header_end + 4..];
    if chunked {
        decode_chunked(body)
    } else if let Some(length) = content_length {
        Ok(body[..body.len().min(length)].to_vec())
    } else {
        Ok(body.to_vec())
    }
}

fn decode_chunked(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    loop {
        let line_end = find_subslice(body, b"\r\n").ok_or_else(|| "malformed chunk".to_owned())?;
        let size_text = String::from_utf8_lossy(&body[..line_end]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("0").trim(), 16)
            .map_err(|_| "invalid chunk size".to_owned())?;
        body = &body[line_end + 2..];
        if size == 0 {
            break;
        }
        if body.len() < size {
            return Err("truncated chunk".to_owned());
        }
        output.extend_from_slice(&body[..size]);
        body = &body[size..];
        if body.starts_with(b"\r\n") {
            body = &body[2..];
        }
        if output.len() > MAX_BODY {
            return Err("response too large".to_owned());
        }
    }
    Ok(output)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

pub async fn post(
    url: &str,
    content_type: &str,
    body: &[u8],
    bearer: Option<&str>,
    insecure: bool,
    pins: &[[u8; 32]],
) -> Result<(u16, Vec<u8>), String> {
    let parsed = url::Url::parse(url).map_err(|error| error.to_string())?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err("unsupported URL scheme".to_owned());
    }
    let host = parsed.host_str().ok_or_else(|| "missing host".to_owned())?;
    let tls_enabled = scheme == "https";
    let port = parsed.port().unwrap_or(if tls_enabled { 443 } else { 80 });
    let mut path = parsed.path().to_owned();
    if path.is_empty() {
        path.push('/');
    }
    if let Some(query) = parsed.query() {
        path.push('?');
        path.push_str(query);
    }
    let host_header = if (tls_enabled && port == 443) || (!tls_enabled && port == 80) {
        host.to_owned()
    } else {
        format!("{host}:{port}")
    };
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: {USER_AGENT}\r\nAccept: */*\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(token) = bearer {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    request.push_str("\r\n");

    let stream = tokio::time::timeout(Duration::from_secs(30), TcpStream::connect((host, port)))
        .await
        .map_err(|_| "connect timeout".to_owned())?
        .map_err(|error| error.to_string())?;
    let mut buffer = Vec::new();
    if tls_enabled {
        let config = tls::build_client_config(insecure, pins);
        let connector = TlsConnector::from(config);
        let server_name = rustls::pki_types::ServerName::try_from(host.to_owned())
            .map_err(|error| error.to_string())?;
        let mut tls_stream = tokio::time::timeout(
            Duration::from_secs(30),
            connector.connect(server_name, stream),
        )
        .await
        .map_err(|_| "tls handshake timeout".to_owned())?
        .map_err(|error| error.to_string())?;
        tls_stream
            .write_all(request.as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        tls_stream
            .write_all(body)
            .await
            .map_err(|error| error.to_string())?;
        read_response(&mut tls_stream, &mut buffer).await?;
    } else {
        let mut plain = stream;
        plain
            .write_all(request.as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        plain
            .write_all(body)
            .await
            .map_err(|error| error.to_string())?;
        read_response(&mut plain, &mut buffer).await?;
    }
    let (status, _) = parse_status(&buffer)?;
    let body = parse_response(&buffer)?;
    Ok((status, body))
}

fn parse_status(buffer: &[u8]) -> Result<(u16, String), String> {
    let header_end =
        find_subslice(buffer, b"\r\n\r\n").ok_or_else(|| "malformed response".to_owned())?;
    let headers = String::from_utf8_lossy(&buffer[..header_end]);
    let status_line = headers.lines().next().unwrap_or_default().to_owned();
    let mut parts = status_line.split_whitespace();
    let _version = parts.next();
    let status = parts
        .next()
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| format!("malformed status line: {status_line}"))?;
    Ok((status, status_line))
}
