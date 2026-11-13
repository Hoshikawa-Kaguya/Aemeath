//! 外部设备能力的探测：独占时接受的格式、共享格式、默认和最小周期。
//!
//! 独占格式的探测用 wasapi 的 [`CapabilityProbe`]（只调用 `IsFormatSupported`，不初始化流），
//! 声道数固定为设备共享格式的声道数，采样率逐个试标准值。
//!
//! **探测会真的切换 USB 音频设备的格式**（实测 `IsFormatSupported` 让开发槽位依次切到了
//! 44.1k、88.2k、96k……），对实体 DAC 可能造成咔哒声。所以：
//! - 全量探测只在主程序明确请求、且设备没有被 Aemeath 打开时做，结果按设备缓存，引擎运行期间只探测一次；
//! - 打开设备时不做全量探测：推荐格式优先用缓存，没有缓存时只试少数几个候选（[`quick_recommended`]）；
//! - 设备正被 Aemeath 打开、又没有缓存时，只回答当前格式和周期。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use aemeath_protocol::ids::EndpointId;
use aemeath_protocol::message::{DeviceCapabilities, DeviceFormat, EndpointDirection, SampleFormat};
use aemeath_protocol::units::SampleRate;
use wasapi::{AudioClient, CapabilityProbe, DeviceEnumerator, WasapiError};

use crate::IoDirection;
use crate::format::{to_device_format, to_wave_format};

/// 快速选择推荐格式时依次尝试的样本格式（与 [`DeviceCapabilities::recommended`] 的优先级一致）
const QUICK_FORMATS: [SampleFormat; 5] = [
    SampleFormat::Int32,
    SampleFormat::Float32,
    SampleFormat::Int24In32,
    SampleFormat::Int24,
    SampleFormat::Int16,
];
const HNS_PER_MICROSECOND: i64 = 10;

/// 全量探测一台设备。
pub fn probe_device(device: &wasapi::Device) -> Result<DeviceCapabilities, WasapiError> {
    let client = device.get_iaudioclient()?;
    let mix = client.get_mixformat()?;
    let channels = usize::from(mix.get_nchannels());
    let (default_period, minimum_period) = client.get_device_period()?;
    let mut probe = CapabilityProbe::new(device)?;
    // 逐个探测标准采样率
    let mut exclusive_formats: Vec<_> = SampleRate::STANDARD_HZ
        .iter()
        .flat_map(|rate| probe.supported_formats(*rate as usize, channels))
        .filter_map(|format| to_device_format(&format))
        .collect();
    exclusive_formats.dedup();
    Ok(DeviceCapabilities {
        exclusive_formats,
        shared_format: to_device_format(&mix),
        default_period_us: microseconds(default_period),
        minimum_period_us: microseconds(minimum_period),
    })
}

/// 没有探测缓存时选推荐格式：先在引擎采样率、再在共享格式的采样率上，按优先级逐个试，试到第一个可用的就停。
/// 只做少数几次 `IsFormatSupported`，避免反复切换设备的格式。
pub fn quick_recommended(
    client: &AudioClient,
    engine_rate: SampleRate,
    shared: Option<DeviceFormat>,
) -> Option<DeviceFormat> {
    let channels = shared.map_or(2, |format| format.channels);
    let mut rates = vec![engine_rate];
    if let Some(shared) = shared
        && shared.sample_rate != engine_rate
    {
        rates.push(shared.sample_rate);
    }
    rates.into_iter().find_map(|sample_rate| {
        QUICK_FORMATS.into_iter().find_map(|sample_format| {
            let format = DeviceFormat {
                sample_rate,
                sample_format,
                channels,
            };
            client
                .is_supported_exclusive_with_quirks(&to_wave_format(format))
                .ok()
                .map(|_| format)
        })
    })
}

pub(crate) fn microseconds(hns: i64) -> u32 {
    u32::try_from(hns / HNS_PER_MICROSECOND).unwrap_or(u32::MAX)
}

type DeviceKey = (EndpointId, IoDirection);

#[derive(Debug, Default)]
struct Known {
    /// 全量探测的结果（引擎运行期间保留）
    probed: HashMap<DeviceKey, DeviceCapabilities>,
    /// 正被 Aemeath 打开的设备：当前的格式与周期
    open: HashMap<DeviceKey, DeviceCapabilities>,
}

/// 设备能力的缓存。IO 线程登记正在打开的设备并读取缓存，[`DeviceProber`] 读写。
#[derive(Debug, Clone, Default)]
pub struct CapabilityCache {
    known: Arc<Mutex<Known>>,
}

impl CapabilityCache {
    /// 全量探测的缓存结果
    pub(crate) fn probed(&self, endpoint: &EndpointId, direction: IoDirection) -> Option<DeviceCapabilities> {
        self.lock().probed.get(&(endpoint.clone(), direction)).cloned()
    }

    pub(crate) fn opened(&self, endpoint: &EndpointId, direction: IoDirection, current: DeviceCapabilities) {
        self.lock().open.insert((endpoint.clone(), direction), current);
    }

    pub(crate) fn closed(&self, endpoint: &EndpointId, direction: IoDirection) {
        self.lock().open.remove(&(endpoint.clone(), direction));
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Known> {
        // 锁内只有 HashMap 的增删查，不会在持锁时 panic；即使中毒，数据仍然完整，继续使用
        self.known.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// 回答主程序的 `ProbeDevice`（在阻塞线程上调用）。
#[derive(Debug, Clone)]
pub struct DeviceProber {
    cache: CapabilityCache,
}

impl DeviceProber {
    pub fn new(cache: CapabilityCache) -> Self {
        Self { cache }
    }

    /// 设备打不开、不存在时返回 None。
    pub fn probe(&self, endpoint: &EndpointId, direction: EndpointDirection) -> Option<DeviceCapabilities> {
        let direction = match direction {
            EndpointDirection::Input => IoDirection::Input,
            EndpointDirection::Output => IoDirection::Output,
        };
        let key = (endpoint.clone(), direction);
        {
            let known = self.cache.lock();
            if let Some(capabilities) = known.probed.get(&key).or_else(|| known.open.get(&key)) {
                return Some(capabilities.clone());
            }
        }
        // 阻塞线程池的线程可能还没有初始化 COM；重复初始化同一种套间是无害的
        let _ = wasapi::initialize_mta();
        let device = DeviceEnumerator::new()
            .and_then(|enumerator| enumerator.get_device(&endpoint.0))
            .ok()?;
        let capabilities = probe_device(&device)
            .inspect_err(|error| tracing::warn!(endpoint = %endpoint.0, %error, "cannot probe the device"))
            .ok()?;
        tracing::info!(endpoint = %endpoint.0, formats = capabilities.exclusive_formats.len(), "device probed");
        self.cache.lock().probed.insert(key, capabilities.clone());
        Some(capabilities)
    }
}
