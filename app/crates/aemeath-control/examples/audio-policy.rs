//! 只读检查：列出正在出声的程序及其落点、两个方向的活动端点及默认设备。不改动任何系统设置。
//!
//! 用法：`cargo run -p aemeath-control --example audio-policy`

use aemeath_control::audio_policy::AudioPolicy;
use aemeath_sys::audio_endpoints::AudioFlow;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let policy = AudioPolicy::start()?;
    println!("sessions:");
    for session in policy.sessions().await? {
        println!(
            "  pid {:>6}  system={}  {:?}  name={:?}  on {}",
            session.process_id, session.system_sounds, session.executable, session.display_name, session.endpoint_id
        );
    }
    let _com = aemeath_sys::com::ComApartment::multithreaded()?;
    let formats = aemeath_sys::policy_config::PolicyConfig::new()?;
    for flow in [AudioFlow::Render, AudioFlow::Capture] {
        println!("{flow:?} endpoints:");
        for endpoint in policy.endpoints(flow).await? {
            let format = formats.device_format(&endpoint.id).ok();
            println!(
                "  default={} communications={}  {}  ({})  format={format:?}",
                endpoint.is_default, endpoint.is_communications, endpoint.name, endpoint.id
            );
        }
    }
    Ok(())
}
