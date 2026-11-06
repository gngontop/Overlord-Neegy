use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub fn hostname() -> String {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemInformation::ComputerNameDnsHostname;
        use windows_sys::Win32::System::SystemInformation::GetComputerNameExW;
        let mut buffer = [0u16; 256];
        let mut size = buffer.len() as u32;
        let ok = unsafe {
            GetComputerNameExW(ComputerNameDnsHostname, buffer.as_mut_ptr(), &mut size)
        };
        if ok != 0 {
            return String::from_utf16_lossy(&buffer[..size as usize]);
        }
    }
    std::env::var(if cfg!(windows) {
        "COMPUTERNAME"
    } else {
        "HOSTNAME"
    })
    .ok()
    .filter(|value| !value.trim().is_empty())
    .unwrap_or_else(|| "DESKTOP".to_owned())
}

pub fn username() -> String {
    std::env::var(if cfg!(windows) { "USERNAME" } else { "USER" })
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "user".to_owned())
}

pub fn wire_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

pub fn pretty_os() -> String {
    #[cfg(windows)]
    {
        return "Windows 11".to_owned();
    }
    #[allow(unreachable_code)]
    wire_os().to_owned()
}

pub fn wire_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "x86" => "386",
        other => other,
    }
}

pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn jitter(base: Duration, percent: u64) -> Duration {
    let millis = base.as_millis() as u64;
    if millis == 0 || percent == 0 {
        return base;
    }
    let spread = millis.saturating_mul(percent) / 100;
    if spread == 0 {
        return base;
    }
    let mut seed = (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64)
        ^ (std::process::id() as u64) << 17;
    seed ^= seed >> 13;
    seed = seed.wrapping_mul(0x2545_f491_4f6c_dd1d);
    let offset = seed % (spread.saturating_mul(2).saturating_add(1));
    Duration::from_millis(millis.saturating_sub(spread).saturating_add(offset))
}

static PERF_LOG_PATH: std::sync::LazyLock<Option<std::ffi::OsString>> =
    std::sync::LazyLock::new(|| std::env::var_os(crate::obfstr!("OVERLORD_BEACON_PERF_LOG")));

/// Opt-in diagnostic sink; disabled unless the build-time perf-log variable is set.
pub fn perf_log(message: &str) {
    if let Some(path) = PERF_LOG_PATH.as_ref() {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{message}");
        }
    }
}

pub fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub fn unix_millis_of(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

pub fn shell_is_windows() -> bool {
    cfg!(windows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_near_base() {
        let base = Duration::from_secs(30);
        for _ in 0..32 {
            let value = jitter(base, 20);
            assert!(value >= Duration::from_secs(24));
            assert!(value <= Duration::from_secs(36));
        }
    }

    #[test]
    fn truncates_on_char_boundary() {
        assert_eq!(truncate_utf8("a☃b", 2), "a");
        assert_eq!(truncate_utf8("abc", 3), "abc");
    }
}
