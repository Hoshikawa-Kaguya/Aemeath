//! COM 初始化。

use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// COM 调用的错误（HRESULT 加说明）。其他 crate 经由这个名字使用，不必直接依赖 windows crate。
pub use windows::core::Error as ComError;

/// 让当前线程进入多线程套间（MTA），离开作用域时退出。
///
/// 线程模型：COM 接口指针（这个模块之外的端点、格式对象）只在创建它们的线程上使用，
/// 所以每个使用它们的线程各持有一个守卫；守卫本身不能跨线程移动。
#[derive(Debug)]
pub struct ComApartment {
    /// 是否需要在 Drop 时调用 CoUninitialize（线程已经是别的套间时不需要）
    owns: bool,
    _not_send: std::marker::PhantomData<*const ()>,
}

impl ComApartment {
    /// 已经初始化为 MTA 时也算成功；已经是 STA 时（例如 UI 线程）仍可使用 COM，只是不负责退出。
    pub fn multithreaded() -> windows::core::Result<Self> {
        // SAFETY: 没有指针参数；与 Drop 中的 CoUninitialize 成对调用，且守卫不能跨线程移动
        let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if result == RPC_E_CHANGED_MODE {
            return Ok(Self {
                owns: false,
                _not_send: std::marker::PhantomData,
            });
        }
        result.ok()?;
        Ok(Self {
            owns: true,
            _not_send: std::marker::PhantomData,
        })
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owns {
            // SAFETY: 与 multithreaded 中成功的 CoInitializeEx 成对，在同一线程上调用
            unsafe { CoUninitialize() };
        }
    }
}
