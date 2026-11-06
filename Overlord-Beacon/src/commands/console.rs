use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::Mutex,
    thread,
};

use anyhow::{Context, Result, anyhow, bail};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::{
    commands::Ctx,
    protocol::{self, Obj},
    state::Outbound,
};

const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 36;
const MAX_DIMENSION: u16 = 1_000;

struct ConsoleSession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

pub struct ConsoleHub {
    sessions: Mutex<HashMap<String, ConsoleSession>>,
}

impl Default for ConsoleHub {
    fn default() -> Self {
        Self::new()
    }
}

impl ConsoleHub {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn start(&self, session_id: &str, cols: i64, rows: i64, out: Outbound) -> Result<()> {
        validate_session_id(session_id)?;
        self.stop(session_id);

        let size = terminal_size(cols, rows);
        let pair = native_pty_system().openpty(size).context("open pty")?;
        #[allow(unused_mut)]
        let mut command = shell_command();
        #[cfg(unix)]
        command.env("TERM", "xterm-256color");
        let child = pair.slave.spawn_command(command).context("spawn shell")?;
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .context("open pty reader")?;
        let writer = pair.master.take_writer().context("open pty writer")?;
        let killer = child.clone_killer();

        spawn_output_forwarder(session_id.to_owned(), reader, child, out);
        self.sessions.lock().unwrap().insert(
            session_id.to_owned(),
            ConsoleSession {
                master: pair.master,
                writer,
                killer,
            },
        );
        Ok(())
    }

    pub fn write(&self, session_id: &str, data: &str) -> Result<()> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        session
            .writer
            .write_all(data.as_bytes())
            .context("write pty")?;
        session.writer.flush().context("flush pty")
    }

    pub fn resize(&self, session_id: &str, cols: i64, rows: i64) -> Result<()> {
        let sessions = self.sessions.lock().unwrap();
        let session = sessions
            .get(session_id)
            .ok_or_else(|| anyhow!("session not found"))?;
        session
            .master
            .resize(terminal_size(cols, rows))
            .context("resize pty")
    }

    pub fn stop(&self, session_id: &str) {
        if let Some(mut session) = self.sessions.lock().unwrap().remove(session_id) {
            let _ = session.killer.kill();
        }
    }

    pub fn stop_all(&self) {
        let sessions = std::mem::take(&mut *self.sessions.lock().unwrap());
        for (_, mut session) in sessions {
            let _ = session.killer.kill();
        }
    }
}

fn console_output(
    out: &Outbound,
    session_id: &str,
    data: Option<&[u8]>,
    exit_code: Option<i32>,
    error: Option<&str>,
) {
    let mut object = Obj::new()
        .put(protocol::key_type(), protocol::s("console_output"))
        .put(protocol::key_session_id(), protocol::s(session_id));
    if let Some(data) = data {
        object = object.put(protocol::key_data(), protocol::bin(data.to_vec()));
    }
    if let Some(code) = exit_code {
        object = object.put(protocol::key_exit_code(), protocol::n(code as i64));
    }
    if let Some(error) = error {
        object = object.put(protocol::key_error(), protocol::s(error));
    }
    out.blocking_send(object.value());
}

fn spawn_output_forwarder(
    session_id: String,
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
    out: Outbound,
) {
    thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(size) => console_output(&out, &session_id, Some(&buffer[..size]), None, None),
                Err(error) => {
                    console_output(&out, &session_id, None, None, Some(&error.to_string()));
                    break;
                }
            }
        }
        match child.wait() {
            Ok(status) => {
                let code = i32::try_from(status.exit_code()).unwrap_or(i32::MAX);
                console_output(&out, &session_id, None, Some(code), None);
            }
            Err(error) => console_output(&out, &session_id, None, None, Some(&error.to_string())),
        }
    });
}

fn shell_command() -> CommandBuilder {
    if cfg!(windows) {
        return CommandBuilder::new(
            std::env::var_os("COMSPEC").unwrap_or_else(|| "cmd.exe".into()),
        );
    }
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| {
        if cfg!(target_os = "macos") {
            "/bin/zsh".into()
        } else {
            "/bin/bash".into()
        }
    });
    let mut command = CommandBuilder::new(shell);
    command.arg("-l");
    command
}

fn validate_session_id(session_id: &str) -> Result<()> {
    if session_id.is_empty() {
        bail!("missing session id");
    }
    if session_id.len() > 128 {
        bail!("session id too long");
    }
    Ok(())
}

fn terminal_size(cols: i64, rows: i64) -> PtySize {
    PtySize {
        cols: dimension(cols, DEFAULT_COLS),
        rows: dimension(rows, DEFAULT_ROWS),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn dimension(value: i64, default: u16) -> u16 {
    if value <= 0 {
        default
    } else {
        u16::try_from(value)
            .unwrap_or(MAX_DIMENSION)
            .min(MAX_DIMENSION)
    }
}

pub async fn handle(ctx: Ctx) {
    let session_id = ctx.string("sessionId").unwrap_or_default().to_owned();
    let outcome: Result<()> = match ctx.command.name.as_str() {
        "console_start" => {
            if session_id.is_empty() {
                Err(anyhow!("missing session id"))
            } else {
                ctx.state.consoles.start(
                    &session_id,
                    ctx.integer("cols").unwrap_or(0),
                    ctx.integer("rows").unwrap_or(0),
                    ctx.out.clone(),
                )
            }
        }
        "console_input" => {
            if session_id.is_empty() {
                Ok(())
            } else {
                let data = ctx.string("data").unwrap_or_default();
                ctx.state.consoles.write(&session_id, data)
            }
        }
        "console_resize" => {
            if session_id.is_empty() {
                Ok(())
            } else {
                ctx.state.consoles.resize(
                    &session_id,
                    ctx.integer("cols").unwrap_or(0),
                    ctx.integer("rows").unwrap_or(0),
                )
            }
        }
        "console_stop" => {
            if !session_id.is_empty() {
                ctx.state.consoles.stop(&session_id);
            }
            Ok(())
        }
        _ => Ok(()),
    };

    match outcome {
        Ok(()) => {
            ctx.out
                .result_with_code(&ctx.command, true, None, None)
                .await
        }
        Err(error) => {
            let message = error.to_string();
            ctx.out
                .result_with_code(&ctx.command, false, Some(&message), Some("console_error"))
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_dimensions_are_defaulted_and_bounded() {
        assert_eq!(terminal_size(0, -1).cols, DEFAULT_COLS);
        assert_eq!(terminal_size(2_000, 50).cols, MAX_DIMENSION);
    }

    #[test]
    fn validates_session_ids() {
        assert!(validate_session_id("").is_err());
        assert!(validate_session_id(&"x".repeat(129)).is_err());
        assert!(validate_session_id("c1").is_ok());
    }
}
