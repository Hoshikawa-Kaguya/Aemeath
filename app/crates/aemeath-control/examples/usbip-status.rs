//! 打印 usbip-win2 的版本和已挂载设备（标出哪些属于 Aemeath），用于排查挂载问题。
//!
//! ```text
//! cargo run -p aemeath-control --example usbip-status [-- <Aemeath 服务端口>]
//! ```

use aemeath_control::usbip_cli::UsbipCli;

/// 开发版引擎的默认 USB/IP 端口（aemeath-host 的默认值）
const DEFAULT_SERVER_PORT: u16 = aemeath_usbip::identity::DEFAULT_USBIP_PORT;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server_port = match std::env::args().nth(1) {
        Some(port) => port.parse()?,
        None => DEFAULT_SERVER_PORT,
    };
    let program = UsbipCli::default_program().ok_or("%ProgramFiles% is not set")?;
    let cli = UsbipCli::new(program, server_port);
    println!("usbip-win2 {}", cli.version().await?);
    let devices = cli.imported().await?;
    if devices.is_empty() {
        println!("no imported devices");
    }
    for device in devices {
        let owner = if device.is_served_by(server_port) {
            "Aemeath"
        } else {
            "other"
        };
        println!(
            "port {:>2}  {}:{}/{}  ({owner})",
            device.port, device.host, device.server_port, device.busid
        );
    }
    Ok(())
}
