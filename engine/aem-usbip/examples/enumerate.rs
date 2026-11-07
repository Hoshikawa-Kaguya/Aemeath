//! 冒烟测试：导出指定的开发槽位，只应答控制请求，让 Windows 完成枚举，然后运行指定的秒数后退出。
//!
//! 不处理等时数据（那是引擎调度线程的工作），所以只用于检查描述符和设备身份：
//! 挂载后设备管理器里应出现已有的 `Aemeath Dev In/Out N` 实例，名称不带"2- "后缀，也不产生新实例。
//!
//! 用法：`cargo run -p aem-usbip --example enumerate -- <端口> <秒数> <槽位>...`，槽位写作 `in1`、`out2`。
//! 挂载与卸下另用 `usbip.exe` 完成。

use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aem_protocol::ids::{SlotDirection, SlotId, SlotNumber};
use aem_usbip::identity::IdentityProfile;
use aem_usbip::server::{ServerConfig, ServerEvent, UsbipServer};

fn parse_slot(text: &str) -> Option<SlotId> {
    let (direction, number) = if let Some(number) = text.strip_prefix("in") {
        (SlotDirection::In, number)
    } else {
        (SlotDirection::Out, text.strip_prefix("out")?)
    };
    let number = SlotNumber::new(number.parse().ok()?).ok()?;
    Some(SlotId { direction, number })
}

fn main() -> ExitCode {
    tracing_subscriber::fmt().with_max_level(tracing::Level::DEBUG).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(port), Some(seconds)) = (
        args.first().and_then(|port| port.parse().ok()),
        args.get(1).and_then(|seconds| seconds.parse().ok()),
    ) else {
        eprintln!("usage: enumerate <port> <seconds> <slot>...");
        return ExitCode::FAILURE;
    };
    let Some(slots) = args[2..]
        .iter()
        .map(|text| parse_slot(text))
        .collect::<Option<BTreeSet<_>>>()
    else {
        eprintln!("slots are written as in1..in8 or out1..out8");
        return ExitCode::FAILURE;
    };

    let (events, receiver) = mpsc::channel();
    let config = ServerConfig {
        port,
        profile: IdentityProfile::Development,
    };
    let server = match UsbipServer::start(config, events) {
        Ok(server) => server,
        Err(error) => {
            eprintln!("cannot start the server: {error}");
            return ExitCode::FAILURE;
        }
    };
    server.set_exported(slots);

    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut writers = HashMap::new();
    let mut iso_submits = 0u64;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let Ok(event) = receiver.recv_timeout(remaining) else {
            continue;
        };
        match event {
            ServerEvent::Imported {
                slot,
                connection,
                writer,
            } => {
                println!("imported {slot}");
                writers.insert(connection, writer);
            }
            ServerEvent::Released { slot, connection } => {
                println!("released {slot}");
                writers.remove(&connection);
            }
            ServerEvent::Reply { connection, bytes } => {
                if let Some(writer) = writers.get_mut(&connection) {
                    let _ = writer.write_all(&bytes);
                }
            }
            ServerEvent::StreamChanged { slot, change, .. } => println!("{slot}: {change:?}"),
            ServerEvent::IsoSubmit { .. } => iso_submits += 1,
            ServerEvent::Unlink { .. } => {}
        }
    }
    println!("done; {iso_submits} isochronous URBs left unanswered");
    ExitCode::SUCCESS
}
