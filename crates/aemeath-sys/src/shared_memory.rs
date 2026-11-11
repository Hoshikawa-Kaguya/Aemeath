//! 只允许当前用户访问的命名共享内存，以 `AtomicU32` 数组的形式访问（电平等高频数据）。
//!
//! 两个进程只通过原子操作读写这块内存，内容的布局由调用方约定（见 `aemeath_protocol::meters`）。

use std::io;
use std::sync::atomic::AtomicU32;

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_READ, FILE_MAP_WRITE, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW,
    PAGE_READWRITE, UnmapViewOfFile,
};
use windows::core::HSTRING;

use crate::security::CurrentUserOnly;

const WORD_BYTES: usize = std::mem::size_of::<AtomicU32>();
const HIGH_DWORD_SHIFT: u32 = 32;

/// 一块映射进本进程的共享内存。可以在线程间共享：所有访问都经由原子类型。
pub struct SharedMemory {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    words: usize,
    existed: bool,
}

// SAFETY: 句柄和映射地址可以在任意线程使用；映射的内容只以 AtomicU32 访问，并发读写是定义良好的。
unsafe impl Send for SharedMemory {}
// SAFETY: 同上，&SharedMemory 只能取得 &[AtomicU32]。
unsafe impl Sync for SharedMemory {}

impl std::fmt::Debug for SharedMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedMemory")
            .field("words", &self.words)
            .field("existed", &self.existed)
            .finish_non_exhaustive()
    }
}

impl SharedMemory {
    /// 创建（或打开已存在的）共享内存，大小为 `words` 个 32 位字。新建时内容全为 0。
    ///
    /// 已经存在时（例如引擎重启时主程序仍开着旧的映射）直接沿用，[`Self::existed`] 为 true；
    /// 已存在的对象比要求的小时失败。
    pub fn create(name: &str, words: usize) -> io::Result<Self> {
        let bytes = byte_len(words)?;
        let mut security = CurrentUserOnly::new()?;
        let name = HSTRING::from(name);
        let size = bytes as u64;
        // SAFETY: 以页面文件为后备（INVALID_HANDLE_VALUE）；安全属性在调用期间有效；name 是有效的宽字符串
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                Some(security.attributes().cast_const()),
                PAGE_READWRITE,
                (size >> HIGH_DWORD_SHIFT) as u32,
                size as u32,
                &name,
            )
        }?;
        // SAFETY: 紧接在 CreateFileMappingW 之后读取它设置的错误码
        let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
        Self::map(handle, words, existed)
    }

    /// 打开另一个进程创建的共享内存。不存在时返回 `NotFound`。
    pub fn open(name: &str, words: usize) -> io::Result<Self> {
        let name = HSTRING::from(name);
        // SAFETY: name 是有效的宽字符串；成功后句柄由 map 接管
        let handle = unsafe { OpenFileMappingW((FILE_MAP_READ | FILE_MAP_WRITE).0, false, &name) }?;
        Self::map(handle, words, true)
    }

    /// 把映射对象映射进本进程；`existed` 记下这块内存是不是别的进程先建的。
    fn map(handle: HANDLE, words: usize, existed: bool) -> io::Result<Self> {
        let bytes = byte_len(words)?;
        // SAFETY: handle 是有效的映射对象句柄；映射的长度不超过对象大小，否则调用失败并返回空地址
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, bytes) };
        if view.Value.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: handle 有效，映射失败后不再使用
            let _ = unsafe { CloseHandle(handle) };
            return Err(error);
        }
        Ok(Self {
            handle,
            view,
            words,
            existed,
        })
    }

    /// 创建时对象是否已经存在（内容可能是上一个写入者留下的）。
    pub fn existed(&self) -> bool {
        self.existed
    }

    /// 以原子量数组访问整块共享内存。
    pub fn words(&self) -> &[AtomicU32] {
        // SAFETY: 映射至少有 words 个字，按页对齐（满足 AtomicU32 的对齐）；映射在 self 存活期间有效；
        // 内容只以原子类型访问，其他进程的并发写入不会造成数据竞争
        unsafe { std::slice::from_raw_parts(self.view.Value.cast::<AtomicU32>(), self.words) }
    }
}

impl Drop for SharedMemory {
    fn drop(&mut self) {
        // SAFETY: view 和 handle 由 map 取得，只在这里释放一次；此后不再有引用（words 借用了 self）
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.handle);
        }
    }
}

fn byte_len(words: usize) -> io::Result<usize> {
    words
        .checked_mul(WORD_BYTES)
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid shared memory size"))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;

    const WORDS: usize = 16;

    fn name(test: &str) -> String {
        format!(r"Local\aemeath-sys-test-{test}-{}", std::process::id())
    }

    #[test]
    fn two_mappings_see_the_same_words() {
        let name = name("shared");
        let writer = SharedMemory::create(&name, WORDS).unwrap();
        assert!(!writer.existed());
        let reader = SharedMemory::open(&name, WORDS).unwrap();
        writer.words()[3].store(42, Ordering::Relaxed);
        assert_eq!(reader.words()[3].load(Ordering::Relaxed), 42);

        // 写入者重建时沿用仍被读取方打开的对象
        drop(writer);
        let again = SharedMemory::create(&name, WORDS).unwrap();
        assert!(again.existed());
        assert_eq!(again.words()[3].load(Ordering::Relaxed), 42);
    }

    #[test]
    fn opening_a_missing_or_smaller_mapping_fails() {
        let name = name("missing");
        assert!(SharedMemory::open(&name, WORDS).is_err());
        let _small = SharedMemory::create(&name, WORDS).unwrap();
        // 已存在的对象只有一页，映射远超一页的长度会失败
        assert!(SharedMemory::open(&name, WORDS * 4096).is_err());
    }
}
