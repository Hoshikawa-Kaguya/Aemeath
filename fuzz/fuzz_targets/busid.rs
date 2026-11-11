//! 导入请求里的 busid：定长、补零的字段，内容完全由对方决定。
#![no_main]

use aemeath_usbip::wire::{BUSID_LEN, BusId};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(raw) = data.first_chunk::<BUSID_LEN>()
        && let Ok(busid) = BusId::parse(raw)
    {
        // 解析成功的 busid 必须能原样再解析一次
        assert_eq!(BusId::new(busid.as_str()).ok().as_ref(), Some(&busid));
    }
});
