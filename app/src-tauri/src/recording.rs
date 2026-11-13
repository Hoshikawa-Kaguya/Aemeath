//! 录音节点在界面上的表示，以及录音文件路径的拼接。

use std::path::{Path, PathBuf};

use aemeath_core::naming::recording_file_name;
use aemeath_core::preferences::Preferences;
use aemeath_protocol::message::{RecorderState, RecordingFailure};
use aemeath_protocol::topology::RecorderSettings;
use aemeath_sys::clock::local_date_time;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordingFailureView {
    DiskFull,
    WriteFailed,
}

impl From<RecordingFailure> for RecordingFailureView {
    fn from(reason: RecordingFailure) -> Self {
        match reason {
            RecordingFailure::DiskFull => Self::DiskFull,
            RecordingFailure::WriteFailed => Self::WriteFailed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum RecorderStateView {
    Idle,
    Recording { file: String, frames: u64, bytes: u64 },
    Failed { reason: RecordingFailureView },
}

impl From<&RecorderState> for RecorderStateView {
    fn from(state: &RecorderState) -> Self {
        match state {
            RecorderState::Idle => Self::Idle,
            RecorderState::Recording { file, frames, bytes } => Self::Recording {
                file: file.clone(),
                frames: *frames,
                bytes: *bytes,
            },
            RecorderState::Failed { reason } => Self::Failed {
                reason: (*reason).into(),
            },
        }
    }
}

/// 录音节点的设置与状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecorderView {
    /// 领域类型直接发给界面，JSON 与界面的类型一致（见 `view.rs` 的测试）
    pub settings: RecorderSettings,
    pub state: RecorderStateView,
}

/// 录音页的全局设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingView {
    pub folder: String,
    pub defaults: RecorderSettings,
}

/// 录音文件夹：用户设置的，否则是 `%USERPROFILE%\Music\Aemeath`。
pub fn recording_folder(preferences: &Preferences) -> PathBuf {
    match &preferences.recording_folder {
        Some(folder) => PathBuf::from(folder),
        None => std::env::var_os("USERPROFILE")
            .map_or_else(PathBuf::new, PathBuf::from)
            .join("Music")
            .join("Aemeath"),
    }
}

/// 确保文件夹存在，并拼出这次录音的完整路径。`node_id` 只在有同名录音节点时给。
pub fn prepare_recording_path(
    folder: &Path,
    node_name: &str,
    node_id: Option<u32>,
    settings: RecorderSettings,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(folder)?;
    let now = local_date_time();
    let timestamp = format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        now.year, now.month, now.day, now.hour, now.minute, now.second
    );
    Ok(folder.join(recording_file_name(node_name, node_id, &timestamp, settings.format)))
}
