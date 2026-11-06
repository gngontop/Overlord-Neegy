#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::Result;
use overlord_lite::{config::Config, identity::Identity, session};

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env()?;
    let identity = Identity::load_or_create(&config.state_dir)?;

    #[cfg(windows)]
    unsafe {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn FreeConsole() -> i32;
        }
        FreeConsole();
    }

    let mut server_index = 0usize;
    loop {
        let server = &config.servers[server_index % config.servers.len()];

        tokio::select! {
            result = session::run(&config, &identity, server) => {
                let _ = result;
            }
            _ = tokio::signal::ctrl_c() => {
                return Ok(());
            }
        }

        server_index = (server_index + 1) % config.servers.len();
        let base_ms = config.reconnect_delay.as_millis() as i64;
        let spread = base_ms / 4;
        let offset = if spread <= 0 {
            0i64
        } else {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos() as u64;
            (nanos % (spread as u64 * 2 + 1)) as i64 - spread
        };
        let delay = std::time::Duration::from_millis((base_ms + offset).max(1_000) as u64);
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}
