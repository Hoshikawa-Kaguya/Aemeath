//! 进程快照、进程路径、进程退出检测与文件说明（应用源按路径寻找进程），以及本进程的 CPU 时间。

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, MAX_PATH, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

/// 快照中的一个进程。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEntry {
    pub process_id: u32,
    pub parent_id: u32,
    /// 可执行文件名（不含路径）
    pub exe_name: String,
}

/// 系统当前的全部进程。
pub fn process_snapshot() -> windows::core::Result<Vec<ProcessEntry>> {
    // SAFETY: 标准调用；返回的句柄在下面关闭
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)? };
    let mut entries = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    // SAFETY: snapshot 有效，entry.dwSize 已按要求填写
    let mut next = unsafe { Process32FirstW(snapshot, &mut entry) };
    while next.is_ok() {
        let length = entry
            .szExeFile
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(entry.szExeFile.len());
        entries.push(ProcessEntry {
            process_id: entry.th32ProcessID,
            parent_id: entry.th32ParentProcessID,
            exe_name: String::from_utf16_lossy(&entry.szExeFile[..length]),
        });
        // SAFETY: 同上
        next = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    // SAFETY: snapshot 是上面创建的有效句柄，此后不再使用
    let _ = unsafe { CloseHandle(snapshot) };
    Ok(entries)
}

/// 当前进程累计使用的 CPU 时间（内核 + 用户，所有线程合计），单位 100 ns；查询失败时为 None。
pub fn current_process_cpu_hns() -> Option<u64> {
    let mut times = [FILETIME::default(); 4];
    let [creation, exit, kernel, user] = &mut times;
    // SAFETY: GetCurrentProcess 返回伪句柄，无需关闭；四个指针都指向有效的可写 FILETIME
    unsafe { GetProcessTimes(GetCurrentProcess(), creation, exit, kernel, user) }.ok()?;
    let hns = |time: &FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    Some(hns(kernel) + hns(user))
}

/// 进程可执行文件的完整路径；进程已退出或无权查询时为 None。
pub fn process_path(process_id: u32) -> Option<PathBuf> {
    if process_id == 0 {
        return None;
    }
    // SAFETY: 只请求查询信息的最小权限；句柄在下面关闭
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id) }.ok()?;
    let mut buffer = vec![0u16; MAX_PATH as usize * 4];
    let mut length = buffer.len() as u32;
    // SAFETY: buffer 与 length 描述同一块可写缓冲；成功时 length 是写入的字符数
    let result =
        unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut length) };
    // SAFETY: process 是上面打开的有效句柄，此后不再使用
    let _ = unsafe { CloseHandle(process) };
    result.ok()?;
    let path = String::from_utf16(&buffer[..length as usize]).ok()?;
    Some(PathBuf::from(path))
}

/// 检测进程是否退出。持有句柄期间 PID 不会被系统复用。
#[derive(Debug)]
pub struct ProcessExit {
    handle: HANDLE,
}

// SAFETY: 进程句柄可以在任意线程上等待和关闭
unsafe impl Send for ProcessExit {}

impl ProcessExit {
    /// 打开进程句柄，只请求等待退出所需的权限。进程已经退出或无权访问时报错。
    pub fn open(process_id: u32) -> windows::core::Result<Self> {
        // SAFETY: 只请求等待所需的权限；句柄在 Drop 中关闭
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, process_id)? };
        Ok(Self { handle })
    }

    /// 进程是否已经退出，不阻塞。
    pub fn has_exited(&self) -> bool {
        // SAFETY: handle 在 self 存活期间有效；超时为 0，不阻塞
        unsafe { WaitForSingleObject(self.handle, 0) == WAIT_OBJECT_0 }
    }
}

impl Drop for ProcessExit {
    fn drop(&mut self) {
        // SAFETY: handle 由 open 取得，只在这里关闭
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Translation {
    language: u16,
    code_page: u16,
}

/// 可执行文件的"文件说明"（`FileDescription`），例如 "Google Chrome"；读不到或为空时为 None。
pub fn file_description(path: &Path) -> Option<String> {
    let wide = HSTRING::from(path.as_os_str());
    // SAFETY: wide 是以 NUL 结尾的路径
    let size = unsafe { GetFileVersionInfoSizeW(&wide, None) };
    if size == 0 {
        return None;
    }
    let mut data = vec![0u8; size as usize];
    // SAFETY: data 的长度正是 size
    unsafe { GetFileVersionInfoW(&wide, None, size, data.as_mut_ptr().cast()) }.ok()?;

    let translations = query_value(&data, r"\VarFileInfo\Translation")?;
    if translations.1 < size_of::<Translation>() as u32 {
        return None;
    }
    // SAFETY: 系统返回的指针在 data 内，长度已检查；用 read_unaligned 避开对齐要求
    let first = unsafe { translations.0.cast::<Translation>().read_unaligned() };
    let block = format!(
        r"\StringFileInfo\{:04x}{:04x}\FileDescription",
        first.language, first.code_page
    );
    let (text, chars) = query_value(&data, &block)?;
    // SAFETY: 这是字符串值，系统返回的指针在 data 内，chars 是字符数（含结尾的 NUL）
    let units = unsafe { std::slice::from_raw_parts(text.cast::<u16>(), chars as usize) };
    let end = units.iter().position(|unit| *unit == 0).unwrap_or(units.len());
    let description = String::from_utf16_lossy(&units[..end]).trim().to_owned();
    (!description.is_empty()).then_some(description)
}

/// 返回值指向 `data` 内部。
fn query_value(data: &[u8], sub_block: &str) -> Option<(*const c_void, u32)> {
    let name = HSTRING::from(sub_block);
    let mut value: *mut c_void = std::ptr::null_mut();
    let mut length = 0u32;
    // SAFETY: data 是 GetFileVersionInfoW 填好的版本信息块，name 以 NUL 结尾，两个输出参数都有效
    let found = unsafe { VerQueryValueW(data.as_ptr().cast(), PCWSTR(name.as_ptr()), &mut value, &mut length) };
    (found.as_bool() && !value.is_null()).then_some((value.cast_const(), length))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_current_process_in_the_snapshot() {
        let own = std::process::id();
        let snapshot = process_snapshot().unwrap();
        assert!(snapshot.iter().any(|entry| entry.process_id == own));
        assert_eq!(process_path(own), std::env::current_exe().ok());
        assert!(!ProcessExit::open(own).unwrap().has_exited());
    }

    #[test]
    fn reads_the_description_of_a_system_program() {
        let notepad = Path::new(r"C:\Windows\System32\notepad.exe");
        assert!(file_description(notepad).is_some_and(|text| !text.is_empty()));
        assert_eq!(file_description(Path::new(r"C:\does\not\exist.exe")), None);
    }
}
