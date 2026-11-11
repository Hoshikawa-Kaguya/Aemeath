//! 控制请求：在一个模拟槽位上依次执行一串 SETUP 包，状态（配置、备用设置、采样率）在请求之间保留。
//!
//! 输入格式：第 1 字节选槽位（最低位是方向，其余位取模得编号）；之后每条请求是 8 字节 SETUP、
//! 1 字节数据长度，再跟这么多字节的主机数据（不够时有多少用多少）。
#![no_main]

use aemeath_protocol::ids::{SlotDirection, SlotId, SlotNumber};
use aemeath_usbip::descriptors::Descriptors;
use aemeath_usbip::function::{Setup, Uac2Function};
use aemeath_usbip::identity::IdentityProfile;
use libfuzzer_sys::fuzz_target;

const SETUP_LEN: usize = 8;

fuzz_target!(|data: &[u8]| {
    let Some((&selector, mut rest)) = data.split_first() else {
        return;
    };
    let direction = if selector & 1 == 0 {
        SlotDirection::In
    } else {
        SlotDirection::Out
    };
    let Ok(number) = SlotNumber::new((selector >> 1) % SlotNumber::LAST + SlotNumber::FIRST) else {
        return;
    };
    let Ok(identity) = IdentityProfile::Development.identity(SlotId { direction, number }) else {
        return;
    };
    let mut function = Uac2Function::new(direction, Descriptors::build(&identity));
    while let Some((setup, after)) = rest.split_first_chunk::<SETUP_LEN>() {
        let Some((&length, after)) = after.split_first() else {
            break;
        };
        let (out_data, after) = after.split_at(usize::from(length).min(after.len()));
        let _ = function.control(&Setup::parse(setup), out_data);
        let _ = function.stream_format();
        rest = after;
    }
});
