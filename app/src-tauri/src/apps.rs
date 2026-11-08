//! 应用页（选型报告 5.5 节、S7）：列出正在出声的程序和它们的落点（GLOSSARY：落点），以及可选的落点。
//!
//! 候选落点：工作台上的虚拟输入节点（它们的槽位端点）和外部输出。被 Aemeath 独占的外部输出不可选——
//! 应用打不开它（ADR-0014；2026-11-08 维护者决定）。

use std::collections::BTreeMap;
use std::path::Path;

use aemeath_control::audio_policy::{AppSession, EndpointEntry};
use aemeath_control::slots::endpoints::{ExternalDevice, SlotEndpoint};
use aemeath_core::model::NodeKind;
use aemeath_core::workspace::Workspace;
use aemeath_protocol::ids::SlotId;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppsView {
    pub apps: Vec<AppView>,
    pub choices: Vec<LandingChoice>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppView {
    pub process_id: u32,
    /// 程序名：会话显示名，或者可执行文件名；系统声音为空（界面显示本地化的名称）
    pub name: String,
    pub executable: Option<String>,
    /// 会话当前所在的端点（实际落点）
    pub landing: String,
    /// Aemeath 记录的指派；与 `landing` 不同时，提示"重启该应用后生效"
    pub assigned: Option<String>,
    pub system: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LandingKind {
    VirtualInput,
    ExternalOutput,
    /// 不是工作台上的落点（例如别的虚拟声卡），只用来显示当前落点的名字
    Other,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LandingChoice {
    pub endpoint_id: String,
    pub kind: LandingKind,
    /// 虚拟输入是节点名，外部输出是设备名
    pub name: String,
    /// 补充说明：虚拟输入的槽位设备名
    pub detail: Option<String>,
    /// Aemeath 正在独占这台设备，应用打不开它
    pub exclusive_by_aemeath: bool,
}

/// 由会话、端点和工作台整理出应用页的内容。
pub fn build(
    sessions: Vec<AppSession>,
    slots: &BTreeMap<SlotId, SlotEndpoint>,
    outputs: Vec<ExternalDevice>,
    all_render: &[EndpointEntry],
    workspace: &Workspace,
    assignments: &BTreeMap<String, String>,
) -> AppsView {
    let mut choices: Vec<LandingChoice> = workspace
        .nodes()
        .iter()
        .filter_map(|node| match node.kind {
            NodeKind::VirtualInput { slot } => {
                let endpoint = slots.get(&slot)?;
                Some(LandingChoice {
                    endpoint_id: endpoint.id.clone(),
                    kind: LandingKind::VirtualInput,
                    name: node.name.as_str().to_owned(),
                    detail: endpoint.name.clone(),
                    exclusive_by_aemeath: false,
                })
            }
            _ => None,
        })
        .collect();
    for device in outputs {
        let exclusive_by_aemeath = workspace.nodes().iter().any(|node| {
            matches!(&node.kind, NodeKind::ExternalOutput { device: own } if own.endpoint.0 == device.endpoint_id)
                && node
                    .device_settings()
                    .is_some_and(|settings| settings.access.is_exclusive())
        });
        choices.push(LandingChoice {
            endpoint_id: device.endpoint_id,
            kind: LandingKind::ExternalOutput,
            name: device.name,
            detail: None,
            exclusive_by_aemeath,
        });
    }

    // 同一个进程可能有多个会话：每个进程一张卡片，落点取第一个会话的端点
    let mut apps: Vec<AppView> = Vec::new();
    for session in sessions {
        if apps
            .iter()
            .any(|app| app.process_id == session.process_id && !session.system_sounds)
        {
            continue;
        }
        let executable = session.executable.as_deref().map(|path| path.display().to_string());
        let assigned = executable.as_ref().and_then(|path| assignments.get(path).cloned());
        apps.push(AppView {
            process_id: session.process_id,
            name: app_name(&session),
            executable,
            landing: session.endpoint_id,
            assigned,
            system: session.system_sounds,
        });
    }
    // 系统声音不能指派（没有自己的进程），放在最后
    apps.sort_by(|a, b| {
        a.system
            .cmp(&b.system)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    apps.dedup_by(|a, b| a.system && b.system);

    // 当前落点不在候选里（例如落在一个没有接进工作台的虚拟槽位，或者别的虚拟声卡上）：补上名字，只用于显示
    for app in &apps {
        if !choices.iter().any(|choice| choice.endpoint_id == app.landing)
            && let Some(entry) = all_render.iter().find(|entry| entry.id == app.landing)
        {
            choices.push(LandingChoice {
                endpoint_id: entry.id.clone(),
                kind: LandingKind::Other,
                name: entry.name.clone(),
                detail: None,
                exclusive_by_aemeath: false,
            });
        }
    }
    AppsView { apps, choices }
}

/// 会话显示名（不是资源引用 `@...` 时）优先，否则用可执行文件名（不带扩展名）。
fn app_name(session: &AppSession) -> String {
    if !session.display_name.is_empty() && !session.display_name.starts_with('@') {
        return session.display_name.clone();
    }
    session
        .executable
        .as_deref()
        .and_then(Path::file_stem)
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn session(pid: u32, exe: &str, endpoint: &str) -> AppSession {
        AppSession {
            process_id: pid,
            executable: Some(PathBuf::from(exe)),
            display_name: String::new(),
            endpoint_id: endpoint.to_owned(),
            system_sounds: false,
        }
    }

    #[test]
    fn one_card_per_process_and_system_sounds_last() {
        let system = AppSession {
            process_id: 0,
            executable: None,
            display_name: "@%SystemRoot%\\System32\\AudioSrv.Dll,-202".to_owned(),
            endpoint_id: "speakers".to_owned(),
            system_sounds: true,
        };
        let sessions = vec![
            system,
            session(20, r"C:\Games\game.exe", "speakers"),
            session(10, r"C:\Music\music.exe", "speakers"),
            session(10, r"C:\Music\music.exe", "headphones"),
        ];
        let assignments = BTreeMap::from([(r"C:\Music\music.exe".to_owned(), "in1".to_owned())]);
        let workspace = Workspace::new(aemeath_core::model::EngineSettings::default());
        let view = build(sessions, &BTreeMap::new(), Vec::new(), &[], &workspace, &assignments);
        let names: Vec<&str> = view.apps.iter().map(|app| app.name.as_str()).collect();
        assert_eq!(names, vec!["game", "music", ""]);
        assert_eq!(view.apps[1].assigned.as_deref(), Some("in1"));
        assert!(view.apps[2].system);
    }
}
