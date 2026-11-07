//! 分帧：每条消息是 4 字节小端长度，后跟这么多字节的 UTF-8 JSON。
//!
//! 这里只处理字节，不做 IO：发送方用 [`encode`] 得到要写出的字节；接收方把读到的字节交给
//! [`FrameDecoder::push`]，再用 [`FrameDecoder::next_message`] 逐条取出。同步和异步的命名管道都能用。
//!
//! 管道只允许当前用户访问，但收到的内容仍一律校验（ADR-0008）：超长的帧直接报错，调用方应断开连接。

use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

/// 单帧上限。路由计划通常只有几 KB；上限只用来拒绝异常输入，防止对方让我们分配大量内存。
pub const MAX_FRAME_LEN: usize = 1024 * 1024;

const LENGTH_PREFIX_LEN: usize = 4;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame of {0} bytes exceeds the limit of {MAX_FRAME_LEN} bytes")]
    TooLarge(usize),
    #[error("invalid message JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// 把一条消息编码成一帧。
pub fn encode<T: Serialize>(message: &T) -> Result<Vec<u8>, FrameError> {
    let json = serde_json::to_vec(message)?;
    let len = u32::try_from(json.len())
        .ok()
        .filter(|len| *len as usize <= MAX_FRAME_LEN)
        .ok_or(FrameError::TooLarge(json.len()))?;
    let mut frame = Vec::with_capacity(LENGTH_PREFIX_LEN + json.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&json);
    Ok(frame)
}

/// 增量解码器：可以按任意大小分块喂入字节。
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// 追加从管道读到的字节。
    pub fn push(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
    }

    /// 取出下一条完整的消息；数据还不够一帧时返回 `Ok(None)`。
    ///
    /// 返回错误后连接已不可信，调用方应断开，不要继续使用这个解码器。
    pub fn next_message<T: DeserializeOwned>(&mut self) -> Result<Option<T>, FrameError> {
        let Some(prefix) = self.buffer.first_chunk::<LENGTH_PREFIX_LEN>() else {
            return Ok(None);
        };
        let len = u32::from_le_bytes(*prefix) as usize;
        // 在等待整帧到齐之前就检查长度，避免为超长帧缓存数据
        if len > MAX_FRAME_LEN {
            return Err(FrameError::TooLarge(len));
        }
        let end = LENGTH_PREFIX_LEN + len;
        if self.buffer.len() < end {
            return Ok(None);
        }

        let result = serde_json::from_slice(&self.buffer[LENGTH_PREFIX_LEN..end]);
        self.buffer.drain(..end);
        Ok(Some(result?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::RequestId;
    use crate::message::AppToEngine;

    fn ping(id: u64) -> AppToEngine {
        AppToEngine::Ping { request: RequestId(id) }
    }

    #[test]
    fn round_trips_several_messages_fed_byte_by_byte() {
        let mut bytes = encode(&ping(1)).unwrap();
        bytes.extend(encode(&ping(2)).unwrap());

        let mut decoder = FrameDecoder::new();
        let mut received = Vec::new();
        for byte in bytes {
            decoder.push(&[byte]);
            while let Some(message) = decoder.next_message::<AppToEngine>().unwrap() {
                received.push(message);
            }
        }
        assert_eq!(received, vec![ping(1), ping(2)]);
    }

    #[test]
    fn waits_for_a_complete_frame() {
        let frame = encode(&ping(7)).unwrap();
        let mut decoder = FrameDecoder::new();
        decoder.push(&frame[..frame.len() - 1]);
        assert!(decoder.next_message::<AppToEngine>().unwrap().is_none());
        decoder.push(&frame[frame.len() - 1..]);
        assert_eq!(decoder.next_message::<AppToEngine>().unwrap(), Some(ping(7)));
    }

    #[test]
    fn rejects_an_oversized_frame_before_it_arrives() {
        let mut decoder = FrameDecoder::new();
        let oversized = (MAX_FRAME_LEN as u32 + 1).to_le_bytes();
        decoder.push(&oversized);
        assert!(matches!(
            decoder.next_message::<AppToEngine>(),
            Err(FrameError::TooLarge(len)) if len == MAX_FRAME_LEN + 1
        ));
    }

    #[test]
    fn rejects_malformed_json_and_skips_the_frame() {
        let garbage = b"{not json";
        let mut frame = (garbage.len() as u32).to_le_bytes().to_vec();
        frame.extend_from_slice(garbage);
        let mut decoder = FrameDecoder::new();
        decoder.push(&frame);
        assert!(matches!(
            decoder.next_message::<AppToEngine>(),
            Err(FrameError::Json(_))
        ));
    }
}
