//! 按应用指派的读写验证：只动测试播放程序这一个 exe，结束时清除。
//!
//! 需要两个开发槽位的播放端点已经存在（先用 `app_source` 之类的示例挂上，或在 Aemeath 运行时执行）。
//! 流程：把进程的持久指派设到 A，读回应为 A；改到 B，读回应为 B；清除，读回应为空。
//!
//! 用法：`cargo run -p aemeath-control --example landing-check -- <测试播放程序的 PID> <端点名片段 A> <端点名片段 B>`

use aemeath_control::audio_policy::AudioPolicy;
use aemeath_sys::audio_endpoints::AudioFlow;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let pid: u32 = args.next().ok_or("missing the PID")?.parse()?;
    let first = args.next().ok_or("missing endpoint A")?;
    let second = args.next().ok_or("missing endpoint B")?;

    let policy = AudioPolicy::start()?;
    let endpoints = policy.endpoints(AudioFlow::Render).await?;
    let find = |name: &str| {
        endpoints
            .iter()
            .find(|entry| entry.name.contains(name))
            .map(|entry| entry.id.clone())
            .ok_or_else(|| format!("no endpoint named {name}"))
    };
    let (a, b) = (find(&first)?, find(&second)?);

    let mut passed = true;
    for (label, wanted) in [("A", Some(a.clone())), ("B", Some(b.clone())), ("cleared", None)] {
        policy.set_landing(pid, wanted.clone()).await?;
        let read = policy.landing(pid).await?;
        let ok = read == wanted;
        println!("{} set {label}: read {read:?}", if ok { "OK  " } else { "FAIL" });
        passed &= ok;
    }
    println!("{}", if passed { "PASS" } else { "FAIL" });
    if !passed {
        std::process::exit(1);
    }
    Ok(())
}
