//! 录音文件名（M3 计划 R6）：`〈节点名〉_YYYY-MM-DD_HH-mm-ss.〈扩展名〉`。

use aemeath_protocol::topology::RecordFormat;

/// Windows 文件名不允许的字符
const FORBIDDEN: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
const REPLACEMENT: char = '_';

/// `timestamp` 形如 `2026-11-08_09-05-03`。
pub fn recording_file_name(node_name: &str, timestamp: &str, format: RecordFormat) -> String {
    let safe: String = node_name
        .chars()
        .map(|character| {
            if FORBIDDEN.contains(&character) || character.is_control() {
                REPLACEMENT
            } else {
                character
            }
        })
        .collect();
    let extension = match format {
        RecordFormat::Wav => "wav",
        RecordFormat::Flac => "flac",
    };
    format!("{safe}_{timestamp}.{extension}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: &str = "2026-11-08_09-05-03";

    #[test]
    fn builds_the_name_and_replaces_forbidden_characters() {
        assert_eq!(
            recording_file_name(r#"麦克风: A/B\C?"#, AT, RecordFormat::Flac),
            "麦克风_ A_B_C__2026-11-08_09-05-03.flac"
        );
        assert_eq!(
            recording_file_name("Mix", AT, RecordFormat::Wav),
            "Mix_2026-11-08_09-05-03.wav"
        );
    }
}
