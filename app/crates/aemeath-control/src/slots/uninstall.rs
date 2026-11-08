//! 完全卸载槽位的设备实例（ADR-0009；S1 的结论）：按"音频端点 → 音频功能子设备 → USB 复合设备"的顺序，
//! 用 `pnputil /remove-device` 卸载，需要管理员权限。不动 MMDevices 记录（S1：留下的只是无害的历史条目）。
//!
//! 开发工具（`tools/aemeath-dev-cleanup`）、安装版的卸载钩子和便携版的"移除虚拟设备"（M4）调用同一段逻辑。
//!
//! 只匹配指定身份的设备：复合设备按 VID/PID 和序列号精确匹配，功能子设备按父实例归属，
//! 音频端点按设备接口名（USB 产品名）精确匹配。

use std::os::windows::process::CommandExt;
use std::process::Command;

use aemeath_sys::audio_endpoints::{AudioEndpoints, AudioFlow};
use aemeath_sys::com::ComApartment;
use aemeath_sys::devices;
use aemeath_usbip::identity::{IdentityProfile, SlotIdentity};
use thiserror::Error;

const USB_ENUMERATOR: &str = "USB";
const ENDPOINT_PREFIX: &str = r"SWD\MMDEVAPI\";
const FUNCTION_MARKER: &str = "&MI_";
const PNPUTIL: &str = "pnputil.exe";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 卸载的阶段，也是卸载的顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Stage {
    /// 音频端点（`SWD\MMDEVAPI\…`）
    Endpoint,
    /// 音频功能子设备（`USB\VID_…&PID_…&MI_00\…`）
    Function,
    /// USB 复合设备（`USB\VID_…&PID_…\<序列号>`）
    Composite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotInstance {
    pub stage: Stage,
    pub instance_id: String,
    /// 便于阅读的说明（端点名或设备名）
    pub description: String,
}

#[derive(Debug, Error)]
pub enum UninstallError {
    #[error("cannot list devices: {0}")]
    Devices(String),
    #[error("cannot list audio endpoints: {0}")]
    Endpoints(String),
    #[error("pnputil failed for {instance}: {message}")]
    Remove { instance: String, message: String },
}

/// 找出某套身份的全部设备实例（包括不在线的），按卸载顺序排列。
pub fn find_instances(identity: IdentityProfile) -> Result<Vec<SlotInstance>, UninstallError> {
    let identities = identity
        .all()
        .map_err(|error| UninstallError::Devices(error.to_string()))?;
    let usb = devices::instance_ids(USB_ENUMERATOR).map_err(|error| UninstallError::Devices(error.to_string()))?;
    let composites: Vec<(String, &SlotIdentity)> = usb
        .iter()
        .filter_map(|id| composite_of(id, &identities).map(|identity| (id.clone(), identity)))
        .collect();
    let mut instances = Vec::new();
    for id in &usb {
        if !is_function_candidate(id, &identities) {
            continue;
        }
        let parent = devices::parent_instance_id(id).map_err(|error| UninstallError::Devices(error.to_string()))?;
        if let Some((_, identity)) = composites.iter().find(|(composite, _)| {
            parent
                .as_deref()
                .is_some_and(|parent| parent.eq_ignore_ascii_case(composite))
        }) {
            instances.push(SlotInstance {
                stage: Stage::Function,
                instance_id: id.clone(),
                description: identity.product_name.clone(),
            });
        }
    }
    for (id, identity) in &composites {
        instances.push(SlotInstance {
            stage: Stage::Composite,
            instance_id: id.clone(),
            description: identity.product_name.clone(),
        });
    }
    instances.extend(endpoint_instances(&identities)?);
    instances.sort_by(|a, b| a.stage.cmp(&b.stage).then_with(|| a.instance_id.cmp(&b.instance_id)));
    Ok(instances)
}

/// 卸载一个实例（需要管理员权限）。
pub fn remove(instance: &SlotInstance) -> Result<(), UninstallError> {
    let output = Command::new(PNPUTIL)
        .args(["/remove-device", &instance.instance_id])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| UninstallError::Remove {
            instance: instance.instance_id.clone(),
            message: error.to_string(),
        })?;
    if output.status.success() {
        Ok(())
    } else {
        // pnputil 的输出随系统语言本地化，只用于说明
        Err(UninstallError::Remove {
            instance: instance.instance_id.clone(),
            message: String::from_utf8_lossy(&output.stdout).trim().to_owned(),
        })
    }
}

fn endpoint_instances(identities: &[SlotIdentity]) -> Result<Vec<SlotInstance>, UninstallError> {
    let error = |error: aemeath_sys::com::ComError| UninstallError::Endpoints(error.to_string());
    let _com = ComApartment::multithreaded().map_err(error)?;
    let endpoints = AudioEndpoints::new().map_err(error)?;
    let mut instances = Vec::new();
    for flow in [AudioFlow::Render, AudioFlow::Capture] {
        for endpoint in endpoints.list(flow).map_err(error)? {
            let belongs = endpoint
                .interface_name
                .as_deref()
                .is_some_and(|name| identities.iter().any(|identity| identity.product_name == name));
            if belongs {
                instances.push(SlotInstance {
                    stage: Stage::Endpoint,
                    instance_id: format!("{ENDPOINT_PREFIX}{}", endpoint.id),
                    description: endpoint.name.unwrap_or(endpoint.id),
                });
            }
        }
    }
    Ok(instances)
}

/// `USB\VID_1209&PID_0001` 这样的前缀（大写，与实例 ID 比较时忽略大小写）
fn hardware_prefix(identity: &SlotIdentity) -> String {
    format!(r"USB\VID_{:04X}&PID_{:04X}", identity.vendor_id, identity.product_id)
}

/// 复合设备：`USB\VID_xxxx&PID_yyyy\<序列号>`，序列号属于这套身份
fn composite_of<'a>(id: &str, identities: &'a [SlotIdentity]) -> Option<&'a SlotIdentity> {
    let (hardware, serial) = id.rsplit_once('\\')?;
    identities.iter().find(|identity| {
        hardware.eq_ignore_ascii_case(&hardware_prefix(identity))
            && serial.eq_ignore_ascii_case(&identity.serial_number)
    })
}

/// 功能子设备的候选：`USB\VID_xxxx&PID_yyyy&MI_nn\…`（再按父实例确认归属）
fn is_function_candidate(id: &str, identities: &[SlotIdentity]) -> bool {
    let upper = id.to_ascii_uppercase();
    identities
        .iter()
        .any(|identity| upper.starts_with(&format!("{}{FUNCTION_MARKER}", hardware_prefix(identity))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identities() -> Vec<SlotIdentity> {
        IdentityProfile::Development.all().unwrap()
    }

    #[test]
    fn composites_match_vid_pid_and_serial_exactly() {
        let identities = identities();
        assert_eq!(
            composite_of(r"USB\VID_1209&PID_0001\AEMDEVIN01", &identities)
                .map(|identity| identity.product_name.as_str()),
            Some("Aemeath Dev In 1")
        );
        assert_eq!(
            composite_of(r"usb\vid_1209&pid_0002\aemdevout08", &identities)
                .map(|identity| identity.product_name.as_str()),
            Some("Aemeath Dev Out 8")
        );
        // 探针等其他设备：同一个测试 PID，但序列号不属于开发槽位
        assert!(composite_of(r"USB\VID_1209&PID_0001\AEMPROBE01", &identities).is_none());
        assert!(composite_of(r"USB\VID_1209&PID_0003\AEMDEVIN01", &identities).is_none());
        assert!(composite_of(r"USB\VID_046D&PID_0001\AEMDEVIN01", &identities).is_none());
    }

    #[test]
    fn function_candidates_are_interfaces_of_the_same_hardware() {
        let identities = identities();
        assert!(is_function_candidate(
            r"USB\VID_1209&PID_0001&MI_00\6&1A2B&0&0000",
            &identities
        ));
        assert!(!is_function_candidate(r"USB\VID_1209&PID_0001\AEMDEVIN01", &identities));
        assert!(!is_function_candidate(
            r"USB\VID_1209&PID_0003&MI_00\6&1A2B&0&0000",
            &identities
        ));
    }

    #[test]
    fn stages_sort_in_removal_order() {
        let mut stages = [Stage::Composite, Stage::Endpoint, Stage::Function];
        stages.sort();
        assert_eq!(stages, [Stage::Endpoint, Stage::Function, Stage::Composite]);
    }
}
