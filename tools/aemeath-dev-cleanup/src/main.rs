//! 开发工具：完全卸载开发身份的 16 个槽位设备实例（S1 的结论产品化，ADR-0009）。
//!
//! ```text
//! aemeath-dev-cleanup            预演：列出将要卸载的实例（不需要管理员权限）
//! aemeath-dev-cleanup --apply    在管理员终端里执行卸载
//! ```
//!
//! 卸载后再次启动 Aemeath 时会重新注册槽位，设备名不带"2- "之类的后缀（S1 实测）。
//! 执行前必须先退出 Aemeath：仍有指向 Aemeath 的已挂载设备时拒绝执行。

use std::process::ExitCode;

use aemeath_control::slots::uninstall::{self, SlotInstance, Stage};
use aemeath_control::usbip_cli::UsbipCli;
use aemeath_sys::elevation;
use aemeath_usbip::identity::{DEFAULT_USBIP_PORT, IdentityProfile};

const APPLY_FLAG: &str = "--apply";

fn main() -> ExitCode {
    let mut apply = false;
    for argument in std::env::args().skip(1) {
        if argument == APPLY_FLAG {
            apply = true;
        } else {
            eprintln!("unknown argument {argument}\nusage: aemeath-dev-cleanup [{APPLY_FLAG}]");
            return ExitCode::FAILURE;
        }
    }

    let instances = match uninstall::find_instances(IdentityProfile::Development) {
        Ok(instances) => instances,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    if instances.is_empty() {
        println!("No development slot devices are installed.");
        return ExitCode::SUCCESS;
    }
    print_plan(&instances);
    if !apply {
        println!("\nDry run. Run again with {APPLY_FLAG} from an administrator terminal to uninstall these devices.");
        return ExitCode::SUCCESS;
    }

    match elevation::is_elevated() {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("\nUninstalling devices needs administrator rights. Run this from an administrator terminal.");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("cannot check administrator rights: {error}");
            return ExitCode::FAILURE;
        }
    }
    match attached_aemeath_devices() {
        Ok(0) => {}
        Ok(count) => {
            eprintln!("\n{count} Aemeath device(s) are still attached. Quit Aemeath (and its engine) first.");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("cannot list attached devices: {error}");
            return ExitCode::FAILURE;
        }
    }

    let removals = match remove_all() {
        Ok(removals) => removals,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let mut failures = 0;
    for removal in &removals {
        match &removal.result {
            Ok(()) => println!("removed  {}", removal.instance.instance_id),
            Err(error) => {
                failures += 1;
                eprintln!("FAILED   {error}");
            }
        }
    }
    if failures == 0 {
        println!("\nAll {} device instances were removed.", instances.len());
        ExitCode::SUCCESS
    } else {
        eprintln!("\n{failures} of {} removals failed.", instances.len());
        ExitCode::FAILURE
    }
}

fn print_plan(instances: &[SlotInstance]) {
    for stage in [Stage::Endpoint, Stage::Function, Stage::Composite] {
        let group: Vec<&SlotInstance> = instances.iter().filter(|instance| instance.stage == stage).collect();
        let title = match stage {
            Stage::Endpoint => "Audio endpoints",
            Stage::Function => "Audio functions",
            Stage::Composite => "USB composite devices",
        };
        println!("{title} ({}):", group.len());
        for instance in group {
            println!("  {:<44} {}", instance.description, instance.instance_id);
        }
    }
}

fn remove_all() -> Result<Vec<uninstall::Removal>, String> {
    let program = UsbipCli::default_program().ok_or("%ProgramFiles% is not set")?;
    let cli = UsbipCli::new(program, DEFAULT_USBIP_PORT);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime
        .block_on(uninstall::detach_and_remove(IdentityProfile::Development, &cli))
        .map_err(|error| error.to_string())
}

/// 开发身份的已挂载设备数（引擎还在运行时不为 0）
fn attached_aemeath_devices() -> Result<usize, String> {
    let program = UsbipCli::default_program().ok_or("%ProgramFiles% is not set")?;
    let cli = UsbipCli::new(program, DEFAULT_USBIP_PORT);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    let devices = runtime.block_on(cli.imported()).map_err(|error| error.to_string())?;
    let identities = IdentityProfile::Development.all().map_err(|error| error.to_string())?;
    Ok(devices.iter().filter(|device| device.belongs_to(&identities)).count())
}
