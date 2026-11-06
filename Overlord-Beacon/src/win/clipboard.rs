use std::ptr;

use windows_sys::Win32::{
    Foundation::{GlobalFree, HANDLE},
    System::{
        DataExchange::{
            CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable,
            OpenClipboard, SetClipboardData,
        },
        Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE},
    },
};

const CF_UNICODETEXT: u32 = 13;

fn open_clipboard() -> bool {
    for _ in 0..5 {
        let opened = unsafe { OpenClipboard(ptr::null_mut()) };
        if opened != 0 {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

pub fn get_text() -> Option<String> {
    if !open_clipboard() {
        return None;
    }
    let result = unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
            None
        } else {
            let handle = GetClipboardData(CF_UNICODETEXT);
            if handle.is_null() {
                None
            } else {
                let pointer = GlobalLock(handle) as *const u16;
                if pointer.is_null() {
                    None
                } else {
                    let mut length = 0usize;
                    while *pointer.add(length) != 0 && length < 32 * 1024 * 1024 {
                        length += 1;
                    }
                    let slice = std::slice::from_raw_parts(pointer, length);
                    let text = String::from_utf16_lossy(slice);
                    GlobalUnlock(handle);
                    Some(text)
                }
            }
        }
    };
    unsafe {
        CloseClipboard();
    }
    result
}

pub fn set_text(text: &str) -> bool {
    if !open_clipboard() {
        return false;
    }
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let bytes = wide.len() * 2;
    let ok = unsafe {
        if EmptyClipboard() == 0 {
            false
        } else {
            let handle: HANDLE = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if handle.is_null() {
                false
            } else {
                let pointer = GlobalLock(handle);
                if pointer.is_null() {
                    GlobalFree(handle);
                    false
                } else {
                    ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, pointer as *mut u8, bytes);
                    GlobalUnlock(handle);
                    if SetClipboardData(CF_UNICODETEXT, handle).is_null() {
                        GlobalFree(handle);
                        false
                    } else {
                        true
                    }
                }
            }
        }
    };
    unsafe {
        CloseClipboard();
    }
    ok
}
