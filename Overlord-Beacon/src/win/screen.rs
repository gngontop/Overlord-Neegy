use windows_sys::Win32::{
    Foundation::{HANDLE, HWND},
    Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        EnumDisplayMonitors, GetDIBits, GetMonitorInfoW, ReleaseDC, SelectObject, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HDC, HGDIOBJ, HMONITOR, MONITORINFO, SRCCOPY,
    },
    UI::WindowsAndMessaging::{
        GetCursorInfo, GetSystemMetrics, CURSOR_SHOWING, CURSORINFO, SM_CXVIRTUALSCREEN,
        SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    },
};

#[derive(Clone, Copy, Debug)]
pub struct Monitor {
    pub index: usize,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    pub primary: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Cursor {
    pub x: i32,
    pub y: i32,
    pub visible: bool,
}

pub struct Capture {
    pub width: i32,
    pub height: i32,
    pub rgb: Vec<u8>,
}

static MONITORS: std::sync::LazyLock<std::sync::Mutex<Vec<Monitor>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

unsafe extern "system" fn monitor_callback(
    monitor: HMONITOR,
    _hdc: HDC,
    _rect: *mut windows_sys::Win32::Foundation::RECT,
    _data: isize,
) -> i32 {
    let mut info: MONITORINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(monitor, &mut info) != 0 {
        if let Ok(mut list) = MONITORS.lock() {
            let index = list.len();
            list.push(Monitor {
                index,
                x: info.rcMonitor.left,
                y: info.rcMonitor.top,
                width: info.rcMonitor.right - info.rcMonitor.left,
                height: info.rcMonitor.bottom - info.rcMonitor.top,
                primary: (info.dwFlags & 1) != 0,
            });
        }
    }
    1
}

fn collect_monitors() -> Vec<Monitor> {
    if let Ok(mut list) = MONITORS.lock() {
        list.clear();
    }
    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(monitor_callback),
            0,
        );
    }
    MONITORS
        .lock()
        .map(|list| list.clone())
        .unwrap_or_default()
}

pub fn monitors() -> Vec<Monitor> {
    let list = collect_monitors();
    if list.is_empty() {
        unsafe {
            let x = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let y = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            return vec![Monitor {
                index: 0,
                x,
                y,
                width: width.max(1),
                height: height.max(1),
                primary: true,
            }];
        }
    }
    list
}

pub fn virtual_bounds() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    }
}

pub fn capture(display: usize, all_displays: bool) -> Option<Capture> {
    let monitors = monitors();
    let (x, y, width, height) = if all_displays || monitors.is_empty() {
        virtual_bounds()
    } else {
        let monitor = monitors.get(display).copied().unwrap_or(monitors[0]);
        (monitor.x, monitor.y, monitor.width, monitor.height)
    };
    if width <= 0 || height <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(std::ptr::null_mut());
        if screen.is_null() {
            return None;
        }
        let memory = CreateCompatibleDC(screen);
        if memory.is_null() {
            ReleaseDC(std::ptr::null_mut(), screen);
            return None;
        }
        let bitmap = CreateCompatibleBitmap(screen, width, height);
        if bitmap.is_null() {
            DeleteDC(memory);
            ReleaseDC(std::ptr::null_mut(), screen);
            return None;
        }
        let previous: HGDIOBJ = SelectObject(memory, bitmap);
        let copied = BitBlt(memory, 0, 0, width, height, screen, x, y, SRCCOPY);

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
        let mut buffer = vec![0u8; (width as usize).saturating_mul(height as usize).saturating_mul(4)];
        let lines = if copied != 0 {
            GetDIBits(
                memory,
                bitmap,
                0,
                height as u32,
                buffer.as_mut_ptr() as *mut _,
                &mut info,
                DIB_RGB_COLORS,
            )
        } else {
            0
        };

        SelectObject(memory, previous);
        DeleteObject(bitmap);
        DeleteDC(memory);
        ReleaseDC(std::ptr::null_mut(), screen);

        if lines == 0 {
            return None;
        }
        let mut rgb = vec![0u8; (width as usize) * (height as usize) * 3];
        for (index, pixel) in buffer.chunks_exact(4).enumerate() {
            rgb[index * 3] = pixel[2];
            rgb[index * 3 + 1] = pixel[1];
            rgb[index * 3 + 2] = pixel[0];
        }
        Some(Capture {
            width,
            height,
            rgb,
        })
    }
}

pub fn cursor_position() -> Cursor {
    unsafe {
        let mut info: CURSORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<CURSORINFO>() as u32;
        if GetCursorInfo(&mut info) != 0 {
            Cursor {
                x: info.ptScreenPos.x,
                y: info.ptScreenPos.y,
                visible: (info.flags & CURSOR_SHOWING) != 0,
            }
        } else {
            Cursor {
                x: 0,
                y: 0,
                visible: false,
            }
        }
    }
}

pub fn display_origin(display: usize) -> (i32, i32, i32, i32) {
    let monitors = monitors();
    match monitors.get(display).copied().or_else(|| monitors.first().copied()) {
        Some(monitor) => (monitor.x, monitor.y, monitor.width, monitor.height),
        None => virtual_bounds(),
    }
}

pub fn bgra_to_rgb(bgra: &[u8], width: i32, height: i32) -> Vec<u8> {
    let count = (width.max(0) as usize) * (height.max(0) as usize);
    let mut rgb = vec![0u8; count * 3];
    for (index, pixel) in bgra.chunks_exact(4).take(count).enumerate() {
        rgb[index * 3] = pixel[2];
        rgb[index * 3 + 1] = pixel[1];
        rgb[index * 3 + 2] = pixel[0];
    }
    rgb
}

pub fn resize_rgb(rgb: &[u8], width: i32, height: i32, max_height: i32) -> (i32, i32, Vec<u8>) {
    if max_height <= 0 || height <= max_height || width <= 0 || height <= 0 {
        return (width, height, rgb.to_vec());
    }
    let scale = max_height as f64 / height as f64;
    let target_w = ((width as f64 * scale).round() as i32).max(1);
    let target_h = max_height.max(1);
    let mut out = vec![0u8; (target_w as usize) * (target_h as usize) * 3];
    for y in 0..target_h {
        let src_y = ((y as f64) * (height as f64) / (target_h as f64)) as i32;
        let src_y = src_y.clamp(0, height - 1);
        for x in 0..target_w {
            let src_x = ((x as f64) * (width as f64) / (target_w as f64)) as i32;
            let src_x = src_x.clamp(0, width - 1);
            let src = ((src_y as usize) * (width as usize) + src_x as usize) * 3;
            let dst = ((y as usize) * (target_w as usize) + x as usize) * 3;
            out[dst..dst + 3].copy_from_slice(&rgb[src..src + 3]);
        }
    }
    (target_w, target_h, out)
}

#[allow(dead_code)]
fn _keep(_: HWND, _: HANDLE) {}
