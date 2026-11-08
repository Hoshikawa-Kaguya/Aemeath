//! 列出设备接口名包含给定文字的音频端点（含不在线的）：状态、名称、设备格式和端点 ID。只读，不修改任何设置。
//!
//! ```text
//! cargo run -p aemeath-sys --example audio-endpoints [-- <接口名过滤，默认 Aemeath>]
//! ```

use aemeath_sys::audio_endpoints::{AudioEndpoints, AudioFlow};
use aemeath_sys::com::ComApartment;
use aemeath_sys::policy_config::PolicyConfig;

const DEFAULT_FILTER: &str = "Aemeath";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let filter = std::env::args().nth(1).unwrap_or_else(|| DEFAULT_FILTER.to_owned());
    let _com = ComApartment::multithreaded()?;
    let endpoints = AudioEndpoints::new()?;
    let policy = PolicyConfig::new()?;
    for flow in [AudioFlow::Render, AudioFlow::Capture] {
        let mut list = endpoints.list(flow)?;
        list.retain(|endpoint| {
            endpoint
                .interface_name
                .as_deref()
                .is_some_and(|name| name.contains(&filter))
        });
        list.sort_by(|a, b| a.interface_name.cmp(&b.interface_name));
        for endpoint in list {
            let format = match policy.device_format(&endpoint.id) {
                Ok(format) => format!(
                    "{} Hz {:?} {}ch",
                    format.sample_rate.hz(),
                    format.sample_format,
                    format.channels
                ),
                Err(error) => format!("({error})"),
            };
            println!(
                "{flow:?} {:<10?} {:<22} {:<34} {format}  {}",
                endpoint.state,
                endpoint.interface_name.unwrap_or_default(),
                endpoint.name.unwrap_or_default(),
                endpoint.id,
            );
        }
    }
    Ok(())
}
