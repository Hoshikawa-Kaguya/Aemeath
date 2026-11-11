//! 操作阶段的请求头：连接建立后读到的第一个报文。
#![no_main]

use aemeath_usbip::wire::{OP_HEADER_LEN, parse_op_header};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(header) = data.first_chunk::<OP_HEADER_LEN>() {
        let _ = parse_op_header(header);
    }
});
