use std::time::Duration;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

use crate::{
    commands::Ctx,
    protocol::{self, Obj, Value},
};

pub struct ProxyHandle {
    pub write: mpsc::Sender<Vec<u8>>,
    pub cancel: CancellationToken,
}

pub async fn handle(ctx: Ctx) {
    match ctx.command.name.as_str() {
        "proxy_connect" => proxy_connect(ctx).await,
        "proxy_data" => proxy_data(ctx).await,
        "proxy_close" => proxy_close(ctx).await,
        _ => {}
    }
}

fn proxy_close_message(id: &str) -> Value {
    Obj::new()
        .put(protocol::key_type(), protocol::s("proxy_close"))
        .put(protocol::key_connection_id(), protocol::s(id))
        .value()
}

async fn proxy_connect(ctx: Ctx) {
    let host = ctx.string("host").unwrap_or("").to_owned();
    let port = ctx.integer("port").unwrap_or(0);
    if host.is_empty() || !(1..=65535).contains(&port) {
        ctx.out
            .result(&ctx.command, false, Some("Invalid host or port"))
            .await;
        return;
    }
    let address = format!("{host}:{port}");
    let stream = tokio::time::timeout(Duration::from_secs(20), TcpStream::connect(&address)).await;
    let stream = match stream {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            ctx.out
                .result(&ctx.command, false, Some(&format!("Connection refused: {error}")))
                .await;
            return;
        }
        Err(_) => {
            ctx.out
                .result(&ctx.command, false, Some("Connection refused: timeout"))
                .await;
            return;
        }
    };
    let _ = stream.set_nodelay(true);
    let (mut read_half, mut write_half) = stream.into_split();
    let cancel = CancellationToken::new();
    let (write_tx, mut write_rx) = mpsc::channel::<Vec<u8>>(256);
    let id = ctx.command.id.clone();
    {
        let mut proxies = ctx.state.proxies.lock().unwrap();
        proxies.insert(
            id.clone(),
            ProxyHandle {
                write: write_tx,
                cancel: cancel.clone(),
            },
        );
    }
    ctx.out
        .result_with_code(&ctx.command, true, Some("Connected"), None)
        .await;

    let out = ctx.out.clone();
    let read_cancel = cancel.clone();
    let read_id = id.clone();
    tokio::spawn(async move {
        let mut buffer = vec![0u8; 32 * 1024];
        loop {
            tokio::select! {
                _ = read_cancel.cancelled() => break,
                read = read_half.read(&mut buffer) => {
                    match read {
                        Ok(0) => break,
                        Ok(size) => {
                            out.send(
                                Obj::new()
                                    .put(protocol::key_type(), protocol::s("proxy_data"))
                                    .put(protocol::key_connection_id(), protocol::s(&read_id))
                                    .put(protocol::key_data(), protocol::bin(buffer[..size].to_vec()))
                                    .value(),
                            ).await;
                        }
                        Err(_) => break,
                    }
                }
            }
        }
        out.send(proxy_close_message(&read_id)).await;
    });

    let write_id = id.clone();
    let write_cancel = cancel.clone();
    let state = ctx.state.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = write_cancel.cancelled() => break,
                data = write_rx.recv() => {
                    match data {
                        Some(data) => {
                            if write_half.write_all(&data).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
        let _ = write_half.shutdown().await;
        if let Ok(mut proxies) = state.proxies.lock() {
            proxies.remove(&write_id);
        }
    });
}

async fn proxy_data(ctx: Ctx) {
    let id = ctx.command.id.clone();
    let data = ctx.bytes("data").unwrap_or(&[]).to_vec();
    let sender = {
        let proxies = ctx.state.proxies.lock().unwrap();
        proxies.get(&id).map(|handle| handle.write.clone())
    };
    if let Some(sender) = sender {
        let _ = sender.send(data).await;
    }
}

async fn proxy_close(ctx: Ctx) {
    let id = ctx.command.id.clone();
    let handle = {
        let mut proxies = ctx.state.proxies.lock().unwrap();
        proxies.remove(&id)
    };
    if let Some(handle) = handle {
        handle.cancel.cancel();
        ctx.out.send(proxy_close_message(&id)).await;
    }
}
