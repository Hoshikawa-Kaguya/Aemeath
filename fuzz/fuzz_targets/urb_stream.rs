//! URB 阶段的字节流。除了不能崩溃，还检查增量解码与分块方式无关：
//! 同一份字节一次性喂入和按任意大小分块喂入，解出的命令必须相同。
#![no_main]

use aemeath_usbip::wire::{UrbCommand, UrbDecoder};
use libfuzzer_sys::fuzz_target;

/// 按 `step` 字节一块喂入，取出全部命令；遇到错误就停止（连接会被断开），返回错误之前解出的命令。
fn decode(bytes: &[u8], step: usize) -> (Vec<UrbCommand>, bool) {
    let mut decoder = UrbDecoder::new();
    let mut commands = Vec::new();
    for chunk in bytes.chunks(step) {
        decoder.push(chunk);
        loop {
            match decoder.next_command() {
                Ok(Some(command)) => commands.push(command),
                Ok(None) => break,
                Err(_) => return (commands, true),
            }
        }
    }
    (commands, false)
}

fuzz_target!(|data: &[u8]| {
    let Some((&step, stream)) = data.split_first() else {
        return;
    };
    let whole = decode(stream, stream.len().max(1));
    let chunked = decode(stream, usize::from(step).max(1));
    assert_eq!(whole, chunked);
});
