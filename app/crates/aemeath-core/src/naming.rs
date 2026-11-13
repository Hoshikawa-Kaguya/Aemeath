//! 录音文件名：`〈节点名〉_YYYY-MM-DD_HH-mm-ss.〈扩展名〉`；有同名录音节点时是 `〈节点名〉 (〈节点 ID〉)_…`。

use aemeath_protocol::topology::RecordFormat;

/// Windows 文件名不允许的字符
const FORBIDDEN: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
const REPLACEMENT: char = '_';

fn safe_name(node_name: &str) -> String {
    node_name
        .chars()
        .map(|character| {
            if FORBIDDEN.contains(&character) || character.is_control() {
                REPLACEMENT
            } else {
                character
            }
        })
        .collect()
}

/// `timestamp` 形如 `2026-11-08_09-05-03`。`node_id` 只在有同名录音节点时给，用来区分同时开始的录音。
pub fn recording_file_name(node_name: &str, node_id: Option<u32>, timestamp: &str, format: RecordFormat) -> String {
    let safe = safe_name(node_name);
    let id = node_id.map(|id| format!(" ({id})")).unwrap_or_default();
    let extension = match format {
        RecordFormat::Wav => "wav",
        RecordFormat::Flac => "flac",
    };
    format!("{safe}{id}_{timestamp}.{extension}")
}

/// 两个节点名做成的录音文件名是否会相同（Windows 文件名不分大小写）。
pub fn same_file_name(first: &str, second: &str) -> bool {
    safe_name(first).to_lowercase() == safe_name(second).to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: &str = "2026-11-08_09-05-03";

    #[test]
    fn builds_the_name_and_replaces_forbidden_characters() {
        assert_eq!(
            recording_file_name(r#"麦克风: A/B\C?"#, None, AT, RecordFormat::Flac),
            "麦克风_ A_B_C__2026-11-08_09-05-03.flac"
        );
        assert_eq!(
            recording_file_name("Mix", None, AT, RecordFormat::Wav),
            "Mix_2026-11-08_09-05-03.wav"
        );
    }

    #[test]
    fn same_named_recorders_get_their_id() {
        assert_eq!(
            recording_file_name("Mix", Some(7), AT, RecordFormat::Wav),
            "Mix (7)_2026-11-08_09-05-03.wav"
        );
        assert!(same_file_name("Mix", "mix"));
        assert!(same_file_name("A:B", "A?B"), "both become A_B");
        assert!(!same_file_name("Mix", "Mix 2"));
    }
}
