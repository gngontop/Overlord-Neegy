use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{LazyLock, Mutex},
};

use crate::{
    commands::{Ctx, net},
    protocol::{self, Obj, Value},
    state::Outbound,
};

#[derive(Clone, Debug, Default)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub native_loader: String,
    pub on_load: String,
    pub on_event: String,
    pub on_unload: String,
    pub set_callback: String,
    pub get_runtime: String,
}

pub struct LoadedPlugin {
    #[cfg(windows)]
    library: libloading::Library,
    #[cfg(windows)]
    on_event: Option<unsafe extern "system" fn(*const u8, usize, *const u8, usize) -> i32>,
    #[cfg(windows)]
    on_unload: Option<unsafe extern "system" fn() -> i32>,
    #[cfg(not(windows))]
    _private: (),
    pub path: PathBuf,
}

#[derive(Default)]
pub struct PluginManager {
    plugins: HashMap<String, LoadedPlugin>,
}

impl PluginManager {
    pub fn dispatch(&self, id: &str, event: &str, payload: &[u8]) -> Result<(), String> {
        let Some(plugin) = self.plugins.get(id) else {
            return Err("plugin not loaded".to_owned());
        };
        #[cfg(windows)]
        unsafe {
            if let Some(on_event) = plugin.on_event {
                let status = on_event(event.as_ptr(), event.len(), payload.as_ptr(), payload.len());
                if status == 0 {
                    return Ok(());
                }
                return Err(format!("onEvent returned {status}"));
            }
        }
        let _ = (event, payload);
        Err("plugin has no event entrypoint".to_owned())
    }
}

#[cfg(windows)]
type HostCallback = unsafe extern "system" fn(*const u8, usize, *const u8, usize);
#[cfg(not(windows))]
type HostCallback = unsafe extern "C" fn(usize, *const u8, i32, *const u8, i32);

static PLUGIN_SINK: LazyLock<Mutex<Option<(String, Outbound)>>> = LazyLock::new(|| Mutex::new(None));

#[cfg(windows)]
unsafe extern "system" fn plugin_callback(
    event: *const u8,
    event_len: usize,
    payload: *const u8,
    payload_len: usize,
) {
    let event = if event.is_null() || event_len == 0 {
        String::new()
    } else {
        String::from_utf8_lossy(std::slice::from_raw_parts(event, event_len)).into_owned()
    };
    let payload = if payload.is_null() || payload_len == 0 {
        Vec::new()
    } else {
        std::slice::from_raw_parts(payload, payload_len).to_vec()
    };
    let guard = PLUGIN_SINK.lock().ok();
    let Some(guard) = guard else { return };
    let Some((plugin_id, out)) = guard.as_ref() else {
        return;
    };
    out.blocking_send(
        Obj::new()
            .put(protocol::key_type(), protocol::s("plugin_event"))
            .put(protocol::key_plugin_id(), protocol::s(plugin_id))
            .put(protocol::key_event(), protocol::s(&event))
            .put(protocol::key_payload(), protocol::bin(payload))
            .value(),
    );
}

#[cfg(not(windows))]
unsafe extern "C" fn plugin_callback(
    _ctx: usize,
    _event: *const u8,
    _event_len: i32,
    _payload: *const u8,
    _payload_len: i32,
) {
}

pub fn manifest_from_value(value: &Value) -> Result<Manifest, String> {
    let id = protocol::string_field(value, "id").unwrap_or("").to_owned();
    if id.is_empty() {
        return Err("missing plugin id".to_owned());
    }
    let mut manifest = Manifest {
        name: protocol::string_field(value, "name").unwrap_or(&id).to_owned(),
        version: protocol::string_field(value, "version").unwrap_or("").to_owned(),
        native_loader: protocol::string_field(value, "nativeLoader").unwrap_or("").to_owned(),
        id,
        on_load: "PluginOnLoad".to_owned(),
        on_event: "PluginOnEvent".to_owned(),
        on_unload: "PluginOnUnload".to_owned(),
        set_callback: "PluginSetCallback".to_owned(),
        get_runtime: "PluginGetRuntime".to_owned(),
    };
    if let Some(entries) = protocol::field(value, "nativeEntrypoints") {
        if let Some(value) = protocol::string_field(entries, "onLoad") {
            if !value.is_empty() {
                manifest.on_load = value.to_owned();
            }
        }
        if let Some(value) = protocol::string_field(entries, "onEvent") {
            if !value.is_empty() {
                manifest.on_event = value.to_owned();
            }
        }
        if let Some(value) = protocol::string_field(entries, "onUnload") {
            if !value.is_empty() {
                manifest.on_unload = value.to_owned();
            }
        }
        if let Some(value) = protocol::string_field(entries, "setCallback") {
            if !value.is_empty() {
                manifest.set_callback = value.to_owned();
            }
        }
        if let Some(value) = protocol::string_field(entries, "getRuntime") {
            if !value.is_empty() {
                manifest.get_runtime = value.to_owned();
            }
        }
    }
    Ok(manifest)
}

pub async fn handle(ctx: Ctx) {
    match ctx.command.name.as_str() {
        "plugin_load" => {
            let Some(manifest_value) = protocol::field(ctx.payload(), "manifest") else {
                ctx.out
                    .result(&ctx.command, false, Some("missing payload"))
                    .await;
                return;
            };
            let manifest = match manifest_from_value(manifest_value) {
                Ok(manifest) => manifest,
                Err(error) => {
                    ctx.out.result(&ctx.command, false, Some(&error)).await;
                    return;
                }
            };
            let binary = ctx.bytes("binary").unwrap_or(&[]).to_vec();
            load_plugin(&ctx, manifest, binary).await;
        }
        "plugin_load_http" => {
            let Some(manifest_value) = protocol::field(ctx.payload(), "manifest") else {
                ctx.out
                    .result(&ctx.command, false, Some("missing payload"))
                    .await;
                return;
            };
            let manifest = match manifest_from_value(manifest_value) {
                Ok(manifest) => manifest,
                Err(error) => {
                    ctx.out.result(&ctx.command, false, Some(&error)).await;
                    return;
                }
            };
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
                Ok(binary) => load_plugin(&ctx, manifest, binary).await,
                Err(error) => {
                    ctx.out
                        .send(plugin_event(&manifest.id, "error", Some(&error)))
                        .await;
                    ctx.out.result(&ctx.command, false, Some(&error)).await;
                }
            }
        }
        "plugin_unload" => {
            let id = ctx.string("pluginId").unwrap_or("").to_owned();
            if id.is_empty() {
                return;
            }
            unload_plugin(&ctx, &id).await;
        }
        _ => {}
    }
}

async fn load_plugin(ctx: &Ctx, manifest: Manifest, binary: Vec<u8>) {
    let state = ctx.state.clone();
    let out = ctx.out.clone();
    let command = ctx.command.clone();
    let manifest_for_task = manifest.clone();
    let result = tokio::task::spawn_blocking(move || {
        #[cfg(windows)]
        {
            let directory = state.config.state_dir.join("plugins");
            std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
            let extension = if manifest_for_task.native_loader.is_empty() { "dll" } else { "dll" };
            let path = directory.join(format!("{}.{}", sanitize(&manifest_for_task.id), extension));
            std::fs::write(&path, &binary).map_err(|error| error.to_string())?;

            unsafe {
                let library = libloading::Library::new(&path).map_err(|error| error.to_string())?;
                let set_callback: libloading::Symbol<unsafe extern "system" fn(HostCallback)> =
                    library
                        .get(manifest_for_task.set_callback.as_bytes())
                        .map_err(|error| error.to_string())?;
                *PLUGIN_SINK.lock().unwrap() = Some((manifest_for_task.id.clone(), out.clone()));
                set_callback(plugin_callback);

                let on_load: libloading::Symbol<
                    unsafe extern "system" fn(*const u8, usize, HostCallback) -> i32,
                > = library
                    .get(manifest_for_task.on_load.as_bytes())
                    .map_err(|error| error.to_string())?;
                let info = serde_json::json!({
                    "clientId": crate::util::hostname(),
                    "os": crate::util::wire_os(),
                    "arch": crate::util::wire_arch(),
                    "version": "1.0.0",
                });
                let info = info.to_string();
                let status = on_load(info.as_ptr(), info.len(), plugin_callback);
                if status != 0 {
                    return Err(format!("onLoad returned {status}"));
                }

                let on_event = library
                    .get::<unsafe extern "system" fn(*const u8, usize, *const u8, usize) -> i32>(
                        manifest_for_task.on_event.as_bytes(),
                    )
                    .ok()
                    .map(|symbol| *symbol);
                let on_unload = library
                    .get::<unsafe extern "system" fn() -> i32>(manifest_for_task.on_unload.as_bytes())
                    .ok()
                    .map(|symbol| *symbol);

                let mut plugins = state.plugins.lock().unwrap();
                plugins.plugins.insert(
                    manifest_for_task.id.clone(),
                    LoadedPlugin {
                        library,
                        on_event,
                        on_unload,
                        path,
                    },
                );
            }
            Ok::<(), String>(())
        }
        #[cfg(not(windows))]
        {
            let _ = state;
            let _ = out;
            let _ = binary;
            let _ = manifest_for_task;
            Err("native plugins are only supported on Windows".to_owned())
        }
    })
    .await;

    match result {
        Ok(Ok(())) => {
            ctx.out
                .send(plugin_event(&manifest.id, "loaded", None))
                .await;
            ctx.out.result(&command, true, None).await;
        }
        Ok(Err(error)) => {
            ctx.out
                .send(plugin_event(&manifest.id, "error", Some(&error)))
                .await;
            ctx.out.result(&command, false, Some(&error)).await;
        }
        Err(error) => {
            ctx.out
                .send(plugin_event(&manifest.id, "error", Some(&error.to_string())))
                .await;
            ctx.out.result(&command, false, Some(&error.to_string())).await;
        }
    }
}

async fn unload_plugin(ctx: &Ctx, id: &str) {
    let removed = {
        let state = ctx.state.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut plugins = state.plugins.lock().unwrap();
            plugins.plugins.remove(&id)
        })
        .await
        .ok()
        .flatten()
    };
    if let Some(plugin) = removed {
        #[cfg(windows)]
        unsafe {
            if let Some(unload) = plugin.on_unload {
                unload();
            }
            drop(plugin.library);
        }
        let _ = std::fs::remove_file(&plugin.path);
        if let Ok(mut sink) = PLUGIN_SINK.lock() {
            *sink = None;
        }
        ctx.out
            .send(plugin_event(id, "unloaded", None))
            .await;
    }
}

fn plugin_event(id: &str, event: &str, error: Option<&str>) -> Value {
    let mut object = Obj::new()
        .put(protocol::key_type(), protocol::s("plugin_event"))
        .put(protocol::key_plugin_id(), protocol::s(id))
        .put(protocol::key_event(), protocol::s(event));
    if let Some(error) = error {
        object = object.put(protocol::key_error(), protocol::s(error));
    }
    object.value()
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-' || *character == '_')
        .collect()
}
