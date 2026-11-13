//! 进程快照、进程路径、进程退出检测与文件说明（应用源按路径寻找进程），以及本进程的 CPU 时间。

use std::ffi::{OsString, c_void};
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, MAX_PATH, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SYNCHRONIZE, QueryFullProcessImageNameW, WaitForSingleObject,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

/// 启动子进程时不为它创建控制台窗口（`CommandExt::creation_flags`）：窗口程序调用命令行工具时不闪黑框
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Windows 的系统目录（通常是 `C:\Windows\System32`）。以管理员身份启动系统程序时用它拼绝对路径，
/// 只给文件名的话会先在程序自己的目录里找，那里普通用户可写。
pub fn system_directory() -> windows::core::Result<PathBuf> {
    let mut buffer = [0u16; MAX_PATH as usize];
    // SAFETY: buffer 可写，长度由切片给出
    let length = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
    if length == 0 || length > buffer.len() {
        return Err(windows::core::Error::from_thread());
    }
    Ok(PathBuf::from(OsString::from_wide(&buffer[..length])))
}

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
        entries.push(ProcessEntry {
            process_id: entry.th32ProcessID,
            parent_id: entry.th32ParentProcessID,
            exe_name: crate::wide_until_nul(&entry.szExeFile),
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

    // 翻译表：每项是语言（u16）和代码页（u16），小端
    let translation = query_value(&data, r"\VarFileInfo\Translation", 1)?;
    let [language_low, language_high, page_low, page_high, ..] = *translation else {
        return None;
    };
    let block = format!(
        r"\StringFileInfo\{:04x}{:04x}\FileDescription",
        u16::from_le_bytes([language_low, language_high]),
        u16::from_le_bytes([page_low, page_high])
    );
    // 字符串值的长度按 UTF-16 字符计（含结尾的 NUL）
    let text = query_value(&data, &block, 2)?;
    let units: Vec<u16> = text
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .take_while(|unit| *unit != 0)
        .collect();
    let description = String::from_utf16_lossy(&units).trim().to_owned();
    (!description.is_empty()).then_some(description)
}

/// 查一个值，返回它在 `data` 里的字节。长度来自文件本身，可能是错的：超出 `data` 的部分不要，
/// 所以按偏移从 `data` 里取，不直接用系统返回的指针和长度建切片。`unit` 是长度的单位（字节数）。
fn query_value<'a>(data: &'a [u8], sub_block: &str, unit: usize) -> Option<&'a [u8]> {
    let name = HSTRING::from(sub_block);
    let mut value: *mut c_void = std::ptr::null_mut();
    let mut length = 0u32;
    // SAFETY: data 是 GetFileVersionInfoW 填好的版本信息块，name 以 NUL 结尾，两个输出参数都有效
    let found = unsafe { VerQueryValueW(data.as_ptr().cast(), PCWSTR(name.as_ptr()), &mut value, &mut length) };
    if !found.as_bool() || value.is_null() {
        return None;
    }
    bytes_within(data, value.cast_const().cast(), length as usize * unit)
}

/// `pointer` 指向 `data` 内时，返回从那里开始、最多 `len` 字节、不超出 `data` 的部分。
fn bytes_within(data: &[u8], pointer: *const u8, len: usize) -> Option<&[u8]> {
    let offset = (pointer as usize).checked_sub(data.as_ptr() as usize)?;
    let end = offset.saturating_add(len).min(data.len());
    data.get(offset..end)
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
    fn the_system_directory_holds_system_programs() {
        let system = system_directory().unwrap();
        assert!(system.is_absolute());
        assert!(system.join("pnputil.exe").is_file());
    }

    #[test]
    fn values_are_cut_at_the_end_of_the_version_block() {
        let data = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        let inside = data[8..].as_ptr();
        assert_eq!(bytes_within(&data, inside, 2), Some(&data[8..10]));
        assert_eq!(
            bytes_within(&data, inside, 1000),
            Some(&data[8..10]),
            "a wrong length stays inside"
        );
        assert_eq!(
            bytes_within(&data, data.as_ptr().wrapping_add(20), 2),
            None,
            "past the end"
        );
    }

    #[test]
    fn reads_the_description_of_a_system_program() {
        let notepad = Path::new(r"C:\Windows\System32\notepad.exe");
        assert!(file_description(notepad).is_some_and(|text| !text.is_empty()));
        assert_eq!(file_description(Path::new(r"C:\does\not\exist.exe")), None);
    }
}
