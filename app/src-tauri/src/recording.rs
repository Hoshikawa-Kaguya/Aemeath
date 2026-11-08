//! 录音节点在界面上的表示，以及录音文件路径的拼接（M3 计划 R6）。

use std::path::{Path, PathBuf};

use aemeath_core::naming::recording_file_name;
use aemeath_core::preferences::Preferences;
use aemeath_protocol::message::{RecorderState, RecordingFailure};
use aemeath_protocol::topology::{RecordDepth, RecordFormat, RecorderSettings};
use aemeath_sys::clock::local_date_time;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordFormatView {
    Wav,
    Flac,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordDepthView {
    Int16,
    Int24,
    Float32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecorderSettingsView {
    pub format: RecordFormatView,
    pub depth: RecordDepthView,
}

impl From<RecorderSettings> for RecorderSettingsView {
    fn from(settings: RecorderSettings) -> Self {
        Self {
            format: match settings.format {
                RecordFormat::Wav => RecordFormatView::Wav,
                RecordFormat::Flac => RecordFormatView::Flac,
            },
            depth: match settings.depth {
                RecordDepth::Int16 => RecordDepthView::Int16,
                RecordDepth::Int24 => RecordDepthView::Int24,
                RecordDepth::Float32 => RecordDepthView::Float32,
            },
        }
    }
}

impl From<RecorderSettingsView> for RecorderSettings {
    fn from(view: RecorderSettingsView) -> Self {
        Self {
            format: match view.format {
                RecordFormatView::Wav => RecordFormat::Wav,
                RecordFormatView::Flac => RecordFormat::Flac,
            },
            depth: match view.depth {
                RecordDepthView::Int16 => RecordDepth::Int16,
                RecordDepthView::Int24 => RecordDepth::Int24,
                RecordDepthView::Float32 => RecordDepth::Float32,
            },
        }
    }
}

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
    pub settings: RecorderSettingsView,
    pub state: RecorderStateView,
}

/// 录音页的全局设置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingView {
    pub folder: String,
    pub defaults: RecorderSettingsView,
}

/// 录音文件夹：用户设置的，否则是 `%USERPROFILE%\Music\Aemeath`（M3 计划 R6）。
pub fn recording_folder(preferences: &Preferences) -> PathBuf {
    match &preferences.recording_folder {
        Some(folder) => PathBuf::from(folder),
        None => std::env::var_os("USERPROFILE")
            .map_or_else(PathBuf::new, PathBuf::from)
            .join("Music")
            .join("Aemeath"),
    }
}

/// 确保文件夹存在，并拼出这次录音的完整路径。
pub fn prepare_recording_path(folder: &Path, node_name: &str, settings: RecorderSettings) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(folder)?;
    let now = local_date_time();
    let timestamp = format!(
        "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
        now.year, now.month, now.day, now.hour, now.minute, now.second
    );
    Ok(folder.join(recording_file_name(node_name, &timestamp, settings.format)))
}
