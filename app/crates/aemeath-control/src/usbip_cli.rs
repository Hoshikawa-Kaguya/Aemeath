//! `usbip.exe` 命令行封装（契约见 `agent_tmp/docs/usbip-win2集成与升级.md` 第 3、4、7 节）。
//!
//! 只使用 5 种调用：`--version`、`port`、`attach --once`、`attach --stop`、`detach -p`。
//! `--stop-all`、`detach --all`、`port --stash`、`attach --persistent` 会影响其他 usbip-win2 用户
//! 或已被证明不可靠（ADR-0007），这里不提供。
//!
//! - 只按退出码判断成败：错误文本会随系统语言本地化，只写日志。
//! - "设备已挂载"和"busid 不存在"的输出完全相同，挂载是否成功由调用方以服务端实际收到 import 为准。
//! - 解析不认识的输出时不报错，只记日志；`port` 中缺少 `usbip://` 行的条目整条忽略。

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::str::FromStr;
use std::time::{Duration, Instant};

use aemeath_usbip::wire::BusId;
use thiserror::Error;
use tracing::{debug, warn};

/// Aemeath 的 USB/IP 服务端只监听本机回环地址
pub const SERVER_HOST: &str = "127.0.0.1";
/// 每次调用的超时：正常 16–38 ms，服务端不在线时 `attach` 约 2 s 后失败
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// 不为子进程创建控制台窗口（主程序是窗口程序，否则每次调用都会闪一下黑框）
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// usbip-win2 默认安装位置（相对 `%ProgramFiles%`）
const DEFAULT_INSTALL_DIR: &str = "USBip";
const EXECUTABLE: &str = "usbip.exe";
const PROGRAM_FILES_VARIABLE: &str = "ProgramFiles";

/// vhci 的端口号（`usbip port` 中的 `Port NN`，从 1 开始）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VhciPort(pub u16);

impl fmt::Display for VhciPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// `usbip port` 中的一个已挂载设备。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedDevice {
    pub port: VhciPort,
    pub host: String,
    pub server_port: u16,
    pub busid: String,
}

impl ImportedDevice {
    /// 是否指向 Aemeath 的服务端。其他条目属于别的软件（如 DS4Windows、VIIPER），一律不碰。
    pub fn is_served_by(&self, server_port: u16) -> bool {
        self.host == SERVER_HOST && self.server_port == server_port
    }
}

/// usbip-win2 的应用版本，例如 `0.9.8.1`（驱动包版本是另一套编号）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UsbipVersion {
    pub major: u16,
    pub minor: u16,
    pub patch: u16,
    pub build: u16,
}

impl UsbipVersion {
    /// Aemeath 随包附带并测试过的版本（ADR-0011）
    pub const BUNDLED: Self = Self {
        major: 0,
        minor: 9,
        patch: 8,
        build: 1,
    };
    /// 支持的最低版本（集成文档第 5 节）
    pub const MINIMUM: Self = Self {
        major: 0,
        minor: 9,
        patch: 8,
        build: 0,
    };
    /// 已知有问题的版本（低于最低版本，单独列出以便在日志和说明里点名）
    pub const KNOWN_BROKEN: Self = Self {
        major: 0,
        minor: 9,
        patch: 7,
        build: 8,
    };
}

impl FromStr for UsbipVersion {
    type Err = ParseVersionError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut parts = text.trim().split('.').map(|part| part.parse::<u16>());
        let mut next = || parts.next().and_then(Result::ok).ok_or(ParseVersionError);
        let version = Self {
            major: next()?,
            minor: next()?,
            patch: next()?,
            build: next()?,
        };
        match parts.next() {
            None => Ok(version),
            Some(_) => Err(ParseVersionError),
        }
    }
}

impl fmt::Display for UsbipVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}.{}", self.major, self.minor, self.patch, self.build)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("not a four-part version number")]
pub struct ParseVersionError;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("cannot start {program}: {source}")]
    Spawn {
        program: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("usbip.exe did not finish within {CALL_TIMEOUT:?}")]
    Timeout,
    /// 退出码不为 0。`stderr` 可能是本地化文本，只用于日志。
    #[error("usbip.exe failed (exit code {exit_code:?}): {stderr}")]
    Failed { exit_code: Option<i32>, stderr: String },
    /// 退出码为 0，但输出无法识别（命令行与 Aemeath 不兼容）
    #[error("unrecognized usbip.exe output: {stdout:?}")]
    Unrecognized { stdout: String },
}

/// 一次调用的结果。
struct Output {
    stdout: String,
}

/// `usbip.exe` 的调用者。可以在多个任务间共享（每次调用启动一个独立进程）。
#[derive(Debug, Clone)]
pub struct UsbipCli {
    program: PathBuf,
    server_port: u16,
}

impl UsbipCli {
    /// `server_port` 是 Aemeath USB/IP 服务端的端口（挂载时传给 `-t`，也用来识别自己的条目）。
    pub fn new(program: PathBuf, server_port: u16) -> Self {
        Self { program, server_port }
    }

    /// 默认安装位置的 `usbip.exe`。不检查文件是否存在。
    pub fn default_program() -> Option<PathBuf> {
        let program_files = std::env::var_os(PROGRAM_FILES_VARIABLE)?;
        Some(Path::new(&program_files).join(DEFAULT_INSTALL_DIR).join(EXECUTABLE))
    }

    pub fn server_port(&self) -> u16 {
        self.server_port
    }

    /// `usbip.exe --version`
    pub async fn version(&self) -> Result<UsbipVersion, CliError> {
        let output = self.run(&["--version"]).await?;
        parse_version(&output.stdout).ok_or(CliError::Unrecognized { stdout: output.stdout })
    }

    /// `usbip.exe port`：全部已挂载设备，包括别的软件的。用 [`ImportedDevice::is_served_by`] 筛出自己的。
    pub async fn imported(&self) -> Result<Vec<ImportedDevice>, CliError> {
        let output = self.run(&["port"]).await?;
        Ok(parse_port_list(&output.stdout))
    }

    /// 启动自检（集成文档第 7 节）：`port` 的输出要么为空，要么至少能解析出一个条目。
    /// 否则说明命令行与 Aemeath 不兼容，Aemeath 无法管理虚拟设备。
    pub async fn self_test(&self) -> Result<(), CliError> {
        let output = self.run(&["port"]).await?;
        if output.stdout.trim().is_empty() || !parse_port_list(&output.stdout).is_empty() {
            Ok(())
        } else {
            Err(CliError::Unrecognized { stdout: output.stdout })
        }
    }

    /// `attach --once`：挂载一次，返回 vhci 端口号。成功只说明驱动接受了请求，
    /// 是否真正挂上要看服务端有没有收到 import（ADR-0007）。
    pub async fn attach(&self, busid: &BusId) -> Result<VhciPort, CliError> {
        let port = self.server_port.to_string();
        let args = [
            "-t",
            port.as_str(),
            "attach",
            "-r",
            SERVER_HOST,
            "-b",
            busid.as_str(),
            "--once",
            "--terse",
            "--receive-mode",
            "zero-copy",
        ];
        let output = self.run(&args).await?;
        parse_attach_terse(&output.stdout).ok_or(CliError::Unrecognized { stdout: output.stdout })
    }

    /// `attach --stop`：只停掉这台设备自己的后台重试（不能用 `--stop-all`）。本来没有重试也返回成功。
    pub async fn stop_retry(&self, busid: &BusId) -> Result<(), CliError> {
        let port = self.server_port.to_string();
        let args = [
            "-t",
            port.as_str(),
            "attach",
            "-r",
            SERVER_HOST,
            "-b",
            busid.as_str(),
            "--stop",
        ];
        self.run(&args).await.map(drop)
    }

    /// `detach -p`：卸下一个 vhci 端口。
    pub async fn detach(&self, port: VhciPort) -> Result<(), CliError> {
        let port = port.to_string();
        self.run(&["detach", "-p", port.as_str()]).await.map(drop)
    }

    async fn run(&self, args: &[&str]) -> Result<Output, CliError> {
        let started = Instant::now();
        let child = tokio::process::Command::new(&self.program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            // 超时后 future 被丢弃，同时结束进程
            .kill_on_drop(true)
            .spawn()
            .map_err(|source| CliError::Spawn {
                program: self.program.clone(),
                source,
            })?;
        let output = match tokio::time::timeout(CALL_TIMEOUT, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(source)) => {
                return Err(CliError::Spawn {
                    program: self.program.clone(),
                    source,
                });
            }
            Err(_) => {
                warn!(?args, "usbip.exe timed out and was killed");
                return Err(CliError::Timeout);
            }
        };
        let elapsed_ms = started.elapsed().as_millis();
        // 错误文本是系统语言，不保证是 UTF-8；只用于日志，按有损方式解码
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if output.status.success() {
            debug!(?args, elapsed_ms, "usbip.exe succeeded");
            Ok(Output { stdout })
        } else {
            let exit_code = output.status.code();
            debug!(?args, elapsed_ms, ?exit_code, %stderr, "usbip.exe failed");
            Err(CliError::Failed { exit_code, stderr })
        }
    }
}

/// `--version` 的输出：只有一行版本号。
pub fn parse_version(stdout: &str) -> Option<UsbipVersion> {
    let mut lines = stdout.lines().map(str::trim).filter(|line| !line.is_empty());
    let version = lines.next()?.parse().ok()?;
    lines.next().is_none().then_some(version)
}

/// `attach --terse` 的输出：只有端口号。
pub fn parse_attach_terse(stdout: &str) -> Option<VhciPort> {
    stdout.trim().parse().ok().map(VhciPort)
}

/// `usbip port` 的输出。
///
/// ```text
/// Port 01: device in use at High Speed(480Mbps)
///          Generic : pid.codes Test PID (1209:0001)
///            -> usbip://127.0.0.1:3241/1-1
///            -> remote bus/dev: 001/001
/// ```
///
/// 只认 `Port NN:` 和 `-> usbip://主机:端口/busid` 两种行，其余行（速度、厂商名、serial、mode）不解析。
pub fn parse_port_list(stdout: &str) -> Vec<ImportedDevice> {
    let mut devices = Vec::new();
    let mut current: Option<VhciPort> = None;
    let mut found_url = false;
    for line in stdout.lines().map(str::trim) {
        if let Some(port) = parse_port_header(line) {
            if let Some(previous) = current
                && !found_url
            {
                debug!(%previous, "usbip port entry without a usbip:// line ignored");
            }
            current = Some(port);
            found_url = false;
        } else if let Some(port) = current
            && !found_url
            && let Some((host, server_port, busid)) = parse_url_line(line)
        {
            devices.push(ImportedDevice {
                port,
                host,
                server_port,
                busid,
            });
            found_url = true;
        }
    }
    if let Some(previous) = current
        && !found_url
    {
        debug!(%previous, "usbip port entry without a usbip:// line ignored");
    }
    devices
}

/// `Port 01: ...` → 端口号（有前导零）
fn parse_port_header(line: &str) -> Option<VhciPort> {
    let rest = line.strip_prefix("Port")?;
    let (number, _) = rest.split_once(':')?;
    let number = number.trim();
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    number.parse().ok().map(VhciPort)
}

/// `-> usbip://127.0.0.1:3241/1-1` → (主机, 端口, busid)
fn parse_url_line(line: &str) -> Option<(String, u16, String)> {
    let url = line.strip_prefix("->")?.trim().strip_prefix("usbip://")?;
    let (authority, busid) = url.split_once('/')?;
    let (host, port) = authority.rsplit_once(':')?;
    let busid = busid.split_whitespace().next()?;
    if host.is_empty() {
        return None;
    }
    Some((host.to_owned(), port.parse().ok()?, busid.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 样本格式：`# 命令`、`# exit=...`、`## stdout`、输出、`## stderr`、输出（S3 的录制脚本）
    fn stdout_of(sample: &str) -> &str {
        let start = sample
            .find("## stdout\n")
            .map(|index| index + "## stdout\n".len())
            .unwrap();
        let end = sample.find("\n## stderr").unwrap();
        &sample[start..end]
    }

    macro_rules! sample {
        ($name:literal) => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/testdata/usbip-cli/0.9.8.1/",
                $name,
                ".txt"
            ))
        };
    }

    const AEMEATH_PORT: u16 = 3241;

    #[test]
    fn samples_keep_their_crlf_line_endings() {
        assert!(stdout_of(sample!("attached-port-all")).contains("\r\n"));
    }

    #[test]
    fn parses_all_sixteen_imported_devices() {
        let devices = parse_port_list(stdout_of(sample!("attached-port-all")));
        assert_eq!(devices.len(), 16);
        for (index, device) in devices.iter().enumerate() {
            let number = u16::try_from(index).unwrap() + 1;
            assert_eq!(device.port, VhciPort(number));
            assert_eq!(device.busid, format!("1-{number}"));
            assert!(device.is_served_by(AEMEATH_PORT));
        }
    }

    #[test]
    fn parses_a_single_port_and_an_empty_list() {
        let devices = parse_port_list(stdout_of(sample!("attached-port-one")));
        assert_eq!(
            devices,
            vec![ImportedDevice {
                port: VhciPort(1),
                host: SERVER_HOST.to_owned(),
                server_port: AEMEATH_PORT,
                busid: "1-1".to_owned(),
            }]
        );
        assert!(parse_port_list(stdout_of(sample!("detached-port-all"))).is_empty());
    }

    #[test]
    fn accepts_lf_line_endings_and_other_software() {
        let stdout = "Imported USB devices\n====================\n\
            Port 03: device in use at Full Speed(12Mbps)\n\
            \x20        Sony : DualShock 4 (054c:09cc)\n\
            \x20          -> usbip://192.168.1.20:3240/1-4\n\
            Port 07: device in use at High Speed(480Mbps)\n\
            \x20          -> usbip://127.0.0.1:3241/1-10\n";
        let devices = parse_port_list(stdout);
        assert_eq!(devices.len(), 2);
        assert!(!devices[0].is_served_by(AEMEATH_PORT));
        assert_eq!(devices[1].port, VhciPort(7));
        assert_eq!(devices[1].busid, "1-10");
        assert!(devices[1].is_served_by(AEMEATH_PORT));
    }

    #[test]
    fn ignores_entries_without_a_url_and_unknown_lines() {
        let stdout = "Port 01: device in use\r\n   -> remote bus/dev: 001/001\r\n\
            something new\r\nPort xx: odd\r\nPort 02: ok\r\n  -> usbip://127.0.0.1:3241/1-2\r\n";
        let devices = parse_port_list(stdout);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].port, VhciPort(2));
    }

    #[test]
    fn parses_versions() {
        assert_eq!(
            parse_version(stdout_of(sample!("attached-version"))),
            Some(UsbipVersion::BUNDLED)
        );
        assert_eq!(
            parse_version(stdout_of(sample!("detached-version"))),
            Some(UsbipVersion::BUNDLED)
        );
        assert_eq!(parse_version("0.9.8\r\n"), None);
        assert_eq!(parse_version("0.9.8.1\r\nextra\r\n"), None);
        assert!(UsbipVersion::BUNDLED > "0.9.7.8".parse().unwrap());
        assert_eq!(UsbipVersion::BUNDLED.to_string(), "0.9.8.1");
    }

    #[test]
    fn parses_the_terse_attach_output() {
        assert_eq!(
            parse_attach_terse(stdout_of(sample!("attached-attach-ok-terse"))),
            Some(VhciPort(1))
        );
        assert_eq!(
            parse_attach_terse(stdout_of(sample!("attached-attach-already-imported"))),
            None
        );
    }

    #[test]
    fn default_program_is_under_program_files() {
        if let Some(program) = UsbipCli::default_program() {
            assert!(program.ends_with(Path::new(DEFAULT_INSTALL_DIR).join(EXECUTABLE)));
        }
    }
}
