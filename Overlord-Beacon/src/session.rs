use std::{
    sync::{
        Arc,
        atomic::Ordering,
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderName, HeaderValue},
    },
};
use tokio_util::sync::CancellationToken;

use crate::{
    commands::{Ctx, dispatch},
    config::{Config, ServerUrl},
    identity::Identity,
    protocol::{self, Obj, Value},
    state::{Command, Outbound, State},
    tls,
};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

pub async fn run(
    config: &Config,
    identity: &Identity,
    server: &ServerUrl,
    state: Arc<State>,
) -> Result<bool> {
    let socket_url = format!(
        "{}/api/clients/{}/stream/ws?role=client",
        server.as_str().trim_end_matches('/'),
        identity.client_id,
    );
    let mut request = socket_url.into_client_request()?;
    request.headers_mut().insert(
        HeaderName::from_static("sec-websocket-protocol"),
        HeaderValue::from_static("binary"),
    );
    request.headers_mut().insert(
        HeaderName::from_static("user-agent"),
        HeaderValue::from_static(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        ),
    );
    request.headers_mut().insert(
        HeaderName::from_static("accept-language"),
        HeaderValue::from_static("en-US,en;q=0.9"),
    );
    if let Some(token) = &config.agent_token {
        request.headers_mut().insert(
            HeaderName::from_static("x-agent-token"),
            HeaderValue::from_str(token)?,
        );
    }

    let connector = tls::websocket_connector(config.insecure_tls, &config.tls_spki_pins);
    let (mut socket, _) = connect_async_tls_with_config(request, None, true, connector)
        .await
        .with_context(|| format!("connect to {server}"))?;

    let first = tokio::time::timeout(Duration::from_secs(30), receive_envelope(&mut socket))
        .await
        .context("timed out waiting for enrollment challenge")??;
    let first_type = protocol::required_string(&first, "type")?;
    let (public_key, signature, needs_ack) = if first_type == "enrollment_challenge" {
        let nonce = protocol::required_string(&first, "nonce")?;
        (
            identity.public_key_base64(),
            identity.sign_base64(nonce)?,
            true,
        )
    } else if first_type == "hello_ack" {
        (String::new(), String::new(), false)
    } else {
        bail!("unexpected first server message");
    };

    let (monitors, monitor_values) = monitor_info();
    let hello = Obj::new()
        .put(protocol::key_type(), protocol::s("hello"))
        .put(protocol::key_id(), protocol::s(&identity.client_id))
        .put(protocol::key_hwid(), protocol::s(&identity.client_id))
        .put(protocol::key_host(), protocol::owned(crate::util::hostname()))
        .put(protocol::key_os(), protocol::owned(crate::util::pretty_os()))
        .put(protocol::key_arch(), protocol::s(crate::util::wire_arch()))
        .put(protocol::key_host_arch(), protocol::s(crate::util::wire_arch()))
        .put(protocol::key_version(), protocol::s("3.2.2"))
        .put(protocol::key_user(), protocol::owned(crate::util::username()))
        .put(protocol::key_monitors(), protocol::n(monitors as i64))
        .put(protocol::key_monitor_info(), protocol::arr(monitor_values))
        .put_opt(
            protocol::key_build_tag(),
            config.build_tag.as_deref().map(protocol::s),
        )
        .put(protocol::key_public_key(), protocol::s(&public_key))
        .put(protocol::key_signature(), protocol::s(&signature))
        .put(protocol::key_cpu(), protocol::owned(cpu_name()))
        .put(protocol::key_gpu(), protocol::s(""))
        .put(protocol::key_ram(), protocol::owned(ram_size()))
        .put(protocol::key_is_admin(), protocol::b(is_admin()))
        .put(
            protocol::key_elevation(),
            protocol::s(if is_admin() { "admin" } else { "user" }),
        )
        .put(protocol::key_protocol_version(), protocol::n(protocol::WIRE_PROTOCOL_VERSION as i64))
        .put(protocol::key_command_versions(), protocol::command_versions())
        .value();
    send_value(&mut socket, &hello).await?;

    if needs_ack {
        loop {
            let response = tokio::time::timeout(Duration::from_secs(30), receive_envelope(&mut socket))
                .await
                .context("timed out waiting for enrollment status")??;
            let response_type = protocol::required_string(&response, "type")?;
            if response_type == "hello_ack" {
                break;
            } else if response_type == "enrollment_status" {
                let status = protocol::string_field(&response, "status").unwrap_or("unknown");
                bail!("enrollment status is {status}");
            } else if response_type == "ping" {
                send_pong(&mut socket, &response).await?;
            } else if response_type != "pong" {
                bail!("unexpected enrollment response");
            }
        }
    }

    send_value(
        &mut socket,
        &Obj::new()
            .put(protocol::key_type(), protocol::s("ping"))
            .put(protocol::key_ts(), protocol::n(protocol::now_millis()))
            .value(),
    )
    .await?;

    let session_cancel = CancellationToken::new();
    if let Ok(mut guard) = state.session_cancel.lock() {
        *guard = Some(session_cancel.clone());
    }

    let (outbound, receiver) = Outbound::new(1024);
    let (mut sink, mut stream) = socket.split();
    let writer = tokio::spawn(async move {
        let mut receiver = receiver;
        while let Some(value) = receiver.recv().await {
            match protocol::encode(&value) {
                Ok(bytes) => {
                    if sink.send(Message::Binary(bytes)).await.is_err() {
                        break;
                    }
                }
                Err(_) => continue,
            }
        }
    });

    let heartbeat_base = config.heartbeat_interval.max(Duration::from_secs(5));
    #[allow(unused_mut)]
    let mut heartbeat = tokio::time::sleep(crate::util::jitter(heartbeat_base, 25));
    tokio::pin!(heartbeat);

    let result: Result<bool> = loop {
        tokio::select! {
            _ = session_cancel.cancelled() => {
                break Ok(state.exit_requested.load(Ordering::Relaxed));
            }
            _ = &mut heartbeat => {
                outbound.send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("ping"))
                        .put(protocol::key_ts(), protocol::n(protocol::now_millis()))
                        .value(),
                ).await;
                heartbeat.as_mut().reset(tokio::time::Instant::now() + crate::util::jitter(heartbeat_base, 25));
            }
            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Binary(bytes))) => {
                        match protocol::decode(&bytes) {
                            Ok(envelope) => {
                                if handle_envelope(&state, &outbound, envelope).await.is_err() {
                                    continue;
                                }
                            }
                            Err(_) => continue,
                        }
                    }
                    Some(Ok(Message::Ping(bytes))) => {
                        let _ = bytes;
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        break Ok(false);
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        break Err(anyhow!("websocket error: {error}"));
                    }
                }
            }
        }
    };

    state.stop_streams();
    state.consoles.stop_all();
    if let Ok(mut guard) = state.session_cancel.lock() {
        *guard = None;
    }
    writer.abort();
    result
}

async fn handle_envelope(state: &Arc<State>, outbound: &Outbound, envelope: Value) -> Result<()> {
    let message_type = protocol::string_field(&envelope, "type").unwrap_or("");
    match message_type {
        "ping" => {
            outbound
                .send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("pong"))
                        .put(
                            protocol::key_ts(),
                            protocol::n(protocol::integer_field(&envelope, "ts").unwrap_or_else(protocol::now_millis)),
                        )
                        .value(),
                )
                .await;
        }
        "pong" => {}
        "command_abort" => {
            if let Some(id) = protocol::string_field(&envelope, "commandId") {
                state.abort(id);
            }
        }
        "frame_ack" => {}
        "notification_config" => {}
        "plugin_event" => {
            let plugin_id = protocol::string_field(&envelope, "pluginId").unwrap_or("").to_owned();
            let event = protocol::string_field(&envelope, "event").unwrap_or("").to_owned();
            let payload = protocol::field(&envelope, "payload")
                .and_then(|value| protocol::encode(value).ok())
                .unwrap_or_default();
            if !plugin_id.is_empty() && !event.is_empty() {
                let state = state.clone();
                tokio::task::spawn_blocking(move || {
                    let manager = state.plugins.lock().unwrap();
                    let _ = manager.dispatch(&plugin_id, &event, &payload);
                })
                .await
                .ok();
            }
        }
        "command" => {
            let name = protocol::string_field(&envelope, "commandType").unwrap_or("");
            if protocol::command_version_range(name).is_none() {
                return Ok(());
            }
            let id = protocol::string_field(&envelope, "id").unwrap_or("").to_owned();
            let version = protocol::integer_field(&envelope, "commandVersion")
                .unwrap_or(1)
                .max(1) as u16;
            let payload = protocol::field(&envelope, "payload")
                .cloned()
                .unwrap_or(Value::Nil);
            let command = Command {
                id,
                name: name.to_owned(),
                version,
                payload,
            };
            let ctx = Ctx {
                command,
                out: outbound.clone(),
                state: state.clone(),
            };
            tokio::spawn(async move {
                dispatch(ctx).await;
            });
        }
        _ => {}
    }
    Ok(())
}

async fn receive_envelope(socket: &mut Socket) -> Result<Value> {
    loop {
        match socket
            .next()
            .await
            .ok_or_else(|| anyhow!("server closed the WebSocket"))??
        {
            Message::Binary(bytes) => return protocol::decode(&bytes),
            Message::Ping(bytes) => socket.send(Message::Pong(bytes)).await?,
            Message::Pong(_) | Message::Text(_) | Message::Frame(_) => {}
            Message::Close(_) => bail!("server closed the session"),
        }
    }
}

async fn send_value(socket: &mut Socket, value: &Value) -> Result<()> {
    socket
        .send(Message::Binary(protocol::encode(value)?.into()))
        .await?;
    Ok(())
}

async fn send_pong(socket: &mut Socket, incoming: &Value) -> Result<()> {
    send_value(
        socket,
        &Obj::new()
            .put(protocol::key_type(), protocol::s("pong"))
            .put(
                protocol::key_ts(),
                protocol::n(protocol::integer_field(incoming, "ts").unwrap_or_else(protocol::now_millis)),
            )
            .value(),
    )
    .await
}

fn monitor_info() -> (usize, Vec<Value>) {
    #[cfg(windows)]
    {
        let monitors = crate::win::screen::monitors();
        let values = monitors
            .iter()
            .map(|monitor| {
                Obj::new()
                    .put(protocol::key_width(), protocol::n(monitor.width as i64))
                    .put(protocol::key_height(), protocol::n(monitor.height as i64))
                    .value()
            })
            .collect();
        (monitors.len().max(1), values)
    }
    #[cfg(not(windows))]
    {
        (1, vec![Obj::new().put(protocol::key_width(), protocol::n(1920)).put(protocol::key_height(), protocol::n(1080)).value()])
    }
}

fn is_admin() -> bool {
    #[cfg(windows)]
    {
        crate::win::is_admin()
    }
    #[cfg(not(windows))]
    {
        unsafe { libc_geteuid() == 0 }
    }
}

#[cfg(not(windows))]
fn libc_geteuid() -> u32 {
    // Avoid a libc dependency; uid 0 is only used for the elevation label.
    1000
}

fn cpu_name() -> String {
    std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_default()
}

fn ram_size() -> String {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if unsafe { GlobalMemoryStatusEx(&mut status) } != 0 {
            return format!("{} GB", status.ullTotalPhys / 1024 / 1024 / 1024);
        }
    }
    String::new()
}
