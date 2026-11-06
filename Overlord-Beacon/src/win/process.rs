use std::mem;

use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        },
        ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::{
            OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SUSPEND_RESUME, PROCESS_TERMINATE,
            PROCESS_VM_READ,
        },
    },
};

#[derive(Clone, Debug)]
pub struct ProcessEntry {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub exe_path: String,
    pub memory: u64,
}

fn process_name(entry: &PROCESSENTRY32W) -> String {
    let end = entry
        .szExeFile
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(entry.szExeFile.len());
    String::from_utf16_lossy(&entry.szExeFile[..end])
}

fn exe_path(pid: u32) -> String {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return String::new();
        }
        let mut buffer = [0u16; 2048];
        let mut length = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut length);
        CloseHandle(handle);
        if ok == 0 {
            String::new()
        } else {
            String::from_utf16_lossy(&buffer[..length as usize])
        }
    }
}

fn memory(pid: u32) -> u64 {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid);
        if handle.is_null() {
            return 0;
        }
        let mut counters: PROCESS_MEMORY_COUNTERS = mem::zeroed();
        counters.cb = mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = K32GetProcessMemoryInfo(handle, &mut counters, counters.cb);
        CloseHandle(handle);
        if ok == 0 {
            0
        } else {
            counters.WorkingSetSize as u64
        }
    }
}

pub fn list() -> Vec<ProcessEntry> {
    let mut result = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() || snapshot as isize == -1isize {
            return result;
        }
        let mut entry: PROCESSENTRY32W = mem::zeroed();
        entry.dwSize = mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snapshot, &mut entry);
        while ok != 0 {
            let pid = entry.th32ProcessID;
            let name = process_name(&entry);
            result.push(ProcessEntry {
                pid,
                ppid: entry.th32ParentProcessID,
                name,
                exe_path: String::new(),
                memory: 0,
            });
            ok = Process32NextW(snapshot, &mut entry);
        }
        CloseHandle(snapshot);
    }
    for entry in &mut result {
        entry.exe_path = exe_path(entry.pid);
        entry.memory = memory(entry.pid);
    }
    result
}

pub fn kill(pid: u32) -> Result<(), String> {
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle.is_null() {
            return Err("open process failed".to_owned());
        }
        let ok = TerminateProcess(handle, 1);
        CloseHandle(handle);
        if ok == 0 {
            Err("terminate process failed".to_owned())
        } else {
            Ok(())
        }
    }
}

fn ntdll_process_call(pid: u32, name: &[u8]) -> Result<(), String> {
    unsafe {
        let module = windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(
            super::wide("ntdll.dll").as_ptr(),
        );
        if module.is_null() {
            return Err("ntdll not found".to_owned());
        }
        let address = windows_sys::Win32::System::LibraryLoader::GetProcAddress(module, name.as_ptr());
        let Some(address) = address else {
            return Err("procedure not found".to_owned());
        };
        let function: unsafe extern "system" fn(HANDLE) -> i32 = std::mem::transmute(address);
        let handle = OpenProcess(PROCESS_SUSPEND_RESUME, 0, pid);
        if handle.is_null() {
            return Err("open process failed".to_owned());
        }
        let status = function(handle);
        CloseHandle(handle);
        if status < 0 {
            Err(format!("nt status {status:#x}"))
        } else {
            Ok(())
        }
    }
}

pub fn suspend(pid: u32) -> Result<(), String> {
    ntdll_process_call(pid, b"NtSuspendProcess\0")
}

pub fn resume(pid: u32) -> Result<(), String> {
    ntdll_process_call(pid, b"NtResumeProcess\0")
}

pub fn self_pid() -> u32 {
    std::process::id()
}
