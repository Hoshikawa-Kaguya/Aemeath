//! 录音文件的写入（M3 计划 R2、R3、R8）：WAV（`hound`）和 FLAC（`flacenc`，纯 Rust，不引入 C 构建）。
//!
//! 崩溃后文件仍然可读：WAV 每秒更新一次文件头；FLAC 边录边按帧写出，文件头里的总样本数在结束时才回填，
//! 崩溃时为 0（规范中表示"未知"，解码器按帧读到文件末尾。2026-11-08 用 claxon 核实，最后一帧被截断时前面的帧仍可解码）。

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use aemeath_dsp::resample::{FixedSide, StereoResampler};
use aemeath_protocol::topology::{FftPreset, RecordDepth, RecordFormat, RecorderSettings, Resampler};
use aemeath_protocol::units::SampleRate;
use flacenc::bitsink::ByteSink;
use flacenc::component::{BitRepr, StreamInfo};
use flacenc::config;
use flacenc::error::{Verified, Verify};
use flacenc::source::{Fill, FrameBuf};

use crate::STEREO;

/// WAV 的数据量上限：文件大小字段是 32 位，留出文件头的余量后分段（M3 计划 R3）
pub const WAV_SEGMENT_LIMIT: u64 = 4_000_000_000;
/// FLAC 每帧的样本数（每声道），48 kHz 下约 85 ms
const FLAC_BLOCK_FRAMES: usize = 4096;
/// `fLaC` 标记加上元数据块头的长度：文件头回填时 STREAMINFO 从这里开始
const FLAC_MARKER: &[u8] = b"fLaC";
const STREAMINFO_LAST_BLOCK: u8 = 0x80;
/// `flacenc` 接受的最高采样率（2026-11-10 实测 192 kHz 建文件失败）
const FLAC_MAX_HZ: u32 = 96_000;
/// 超过上限时的目标采样率：44.1 kHz 系用 88.2 kHz，其余用 96 kHz，比率都是整数
const CD_FAMILY_HZ: u32 = 44_100;
const FLAC_CD_FAMILY_TARGET_HZ: u32 = 88_200;
/// 降采样每块的时长：1 ms
const DOWNSAMPLE_CHUNKS_PER_SECOND: u32 = 1000;

fn bits_of(depth: RecordDepth) -> u16 {
    match depth {
        RecordDepth::Int16 => 16,
        RecordDepth::Int24 => 24,
        RecordDepth::Float32 => 32,
    }
}

/// 浮点样本量化成 `bits` 位整数。
fn quantize(sample: f32, bits: u16) -> i32 {
    let full_scale = (1i64 << (bits - 1)) as f64;
    let scaled = (f64::from(sample) * full_scale).round() as i64;
    scaled.clamp(-(1i64 << (bits - 1)), (1i64 << (bits - 1)) - 1) as i32
}

fn to_io(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

fn wav_error(error: hound::Error) -> io::Error {
    match error {
        hound::Error::IoError(error) => error,
        other => to_io(other),
    }
}

/// 第 `part` 段的路径：第一段就是 `base`，之后是 `名字_part2.扩展名`。
fn part_path(base: &Path, part: u32) -> PathBuf {
    if part <= 1 {
        return base.to_path_buf();
    }
    let stem = base.file_stem().map(|stem| stem.to_string_lossy()).unwrap_or_default();
    let extension = base
        .extension()
        .map(|extension| format!(".{}", extension.to_string_lossy()))
        .unwrap_or_default();
    base.with_file_name(format!("{stem}_part{part}{extension}"))
}

struct WavFile {
    writer: hound::WavWriter<BufWriter<File>>,
    /// 这一段已写的数据字节数
    written: u64,
}

struct WavSink {
    base: PathBuf,
    part: u32,
    spec: hound::WavSpec,
    limit: u64,
    file: Option<WavFile>,
    /// 前面各段的字节数
    finished_bytes: u64,
}

impl WavSink {
    fn open(base: &Path, depth: RecordDepth, rate: SampleRate, limit: u64) -> io::Result<Self> {
        let spec = hound::WavSpec {
            channels: STEREO as u16,
            sample_rate: rate.hz(),
            bits_per_sample: bits_of(depth),
            sample_format: if depth == RecordDepth::Float32 {
                hound::SampleFormat::Float
            } else {
                hound::SampleFormat::Int
            },
        };
        let mut sink = Self {
            base: base.to_path_buf(),
            part: 1,
            spec,
            limit,
            file: None,
            finished_bytes: 0,
        };
        sink.open_part()?;
        Ok(sink)
    }

    /// 不覆盖已有文件：同一秒开始两次录音时报错，而不是把上一个文件写坏。
    fn open_part(&mut self) -> io::Result<()> {
        let file = File::create_new(part_path(&self.base, self.part))?;
        let writer = hound::WavWriter::new(BufWriter::new(file), self.spec).map_err(wav_error)?;
        self.file = Some(WavFile { writer, written: 0 });
        Ok(())
    }

    fn write(&mut self, stereo: &[f32]) -> io::Result<()> {
        let frame_bytes = u64::from(self.spec.bits_per_sample / 8) * STEREO as u64;
        for frame in stereo.as_chunks::<STEREO>().0 {
            if self
                .file
                .as_ref()
                .is_some_and(|file| file.written + frame_bytes > self.limit)
            {
                self.roll()?;
            }
            let Some(file) = self.file.as_mut() else {
                return Err(to_io("the recording file is closed"));
            };
            for &sample in frame {
                if self.spec.sample_format == hound::SampleFormat::Float {
                    file.writer.write_sample(sample).map_err(wav_error)?;
                } else {
                    file.writer
                        .write_sample(quantize(sample, self.spec.bits_per_sample))
                        .map_err(wav_error)?;
                }
            }
            file.written += frame_bytes;
        }
        Ok(())
    }

    fn roll(&mut self) -> io::Result<()> {
        self.close_part()?;
        self.part += 1;
        self.open_part()
    }

    fn close_part(&mut self) -> io::Result<()> {
        if let Some(file) = self.file.take() {
            self.finished_bytes += file.written;
            file.writer.finalize().map_err(wav_error)?;
        }
        Ok(())
    }

    /// 更新文件头并写到磁盘，崩溃后文件仍然完整。
    fn sync(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(file) => file.writer.flush().map_err(wav_error),
            None => Ok(()),
        }
    }

    fn bytes(&self) -> u64 {
        self.finished_bytes + self.file.as_ref().map_or(0, |file| file.written)
    }
}

struct FlacSink {
    file: File,
    config: Verified<config::Encoder>,
    info: StreamInfo,
    bits: u16,
    pending: Vec<i32>,
    frame_number: usize,
    bytes: u64,
}

impl FlacSink {
    fn open(base: &Path, depth: RecordDepth, rate: SampleRate) -> io::Result<Self> {
        let bits = bits_of(depth);
        let mut info = StreamInfo::new(rate.hz() as usize, STEREO, usize::from(bits)).map_err(to_io)?;
        info.set_block_sizes(FLAC_BLOCK_FRAMES, FLAC_BLOCK_FRAMES)
            .map_err(to_io)?;
        let mut file = File::create_new(base)?;
        write_flac_header(&mut file, &info)?;
        let config = config::Encoder::default()
            .into_verified()
            .map_err(|(_, error)| to_io(error))?;
        Ok(Self {
            file,
            config,
            info,
            bits,
            pending: Vec::with_capacity(FLAC_BLOCK_FRAMES * STEREO),
            frame_number: 0,
            bytes: 0,
        })
    }

    fn write(&mut self, stereo: &[f32]) -> io::Result<()> {
        for &sample in stereo {
            self.pending.push(quantize(sample, self.bits));
            if self.pending.len() == FLAC_BLOCK_FRAMES * STEREO {
                self.encode_pending()?;
            }
        }
        Ok(())
    }

    fn encode_pending(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let mut buffer = FrameBuf::with_size(STEREO, self.pending.len() / STEREO).map_err(to_io)?;
        buffer.fill_interleaved(&self.pending).map_err(to_io)?;
        let frame =
            flacenc::encode_fixed_size_frame(&self.config, &buffer, self.frame_number, &self.info).map_err(to_io)?;
        self.info.update_frame_info(&frame);
        let mut sink = ByteSink::new();
        frame.write(&mut sink).map_err(to_io)?;
        self.file.write_all(sink.as_slice())?;
        self.bytes += sink.as_slice().len() as u64;
        self.frame_number += 1;
        self.pending.clear();
        Ok(())
    }

    /// 写完剩余样本，回填文件头里的总样本数（MD5 留空，规范允许）。
    fn finish(&mut self) -> io::Result<()> {
        self.encode_pending()?;
        self.info
            .set_block_sizes(FLAC_BLOCK_FRAMES, FLAC_BLOCK_FRAMES)
            .map_err(to_io)?;
        write_flac_header(&mut self.file, &self.info)?;
        self.file.flush()
    }
}

fn write_flac_header(file: &mut File, info: &StreamInfo) -> io::Result<()> {
    let mut sink = ByteSink::new();
    info.write(&mut sink).map_err(to_io)?;
    let block = sink.as_slice();
    let length = block.len() as u32;
    let [_, high, middle, low] = length.to_be_bytes();
    let position = file.stream_position()?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(FLAC_MARKER)?;
    file.write_all(&[STREAMINFO_LAST_BLOCK, high, middle, low])?;
    file.write_all(block)?;
    // 第一次写头时 position 是 0：留在文件头之后接着写帧；结束时回到文件尾，不影响已写的帧
    if position != 0 {
        file.seek(SeekFrom::Start(position))?;
    }
    Ok(())
}

/// FLAC 文件的采样率：不超过编码器上限时就是引擎采样率。
fn flac_rate(engine: SampleRate) -> io::Result<SampleRate> {
    let hz = engine.hz();
    if hz <= FLAC_MAX_HZ {
        return Ok(engine);
    }
    let target = if hz.is_multiple_of(CD_FAMILY_HZ) {
        FLAC_CD_FAMILY_TARGET_HZ
    } else {
        FLAC_MAX_HZ
    };
    SampleRate::new(target).map_err(to_io)
}

/// 引擎采样率超过 FLAC 上限时，写入前先降采样。在录音的写文件线程上运行，不影响调度线程。
struct Downsampler {
    resampler: StereoResampler,
    /// 等待凑够一块的输入
    input: Vec<f32>,
    output: Vec<f32>,
    /// 把滤波器里的样本全部推出来所需的输入帧数
    flush_frames: usize,
}

impl Downsampler {
    fn new(from: SampleRate, to: SampleRate) -> io::Result<Self> {
        let chunk_frames = (from.hz() / DOWNSAMPLE_CHUNKS_PER_SECOND) as usize;
        let quality = Resampler::FftSync {
            preset: FftPreset::Balanced,
        };
        let resampler = StereoResampler::new(quality, from, to, chunk_frames, FixedSide::Input).map_err(to_io)?;
        let delay_input = (resampler.output_delay_frames() as u64 * u64::from(from.hz())).div_ceil(u64::from(to.hz()));
        let flush_frames = usize::try_from(delay_input).map_err(to_io)? + resampler.input_frames_next();
        Ok(Self {
            resampler,
            input: Vec::new(),
            output: Vec::new(),
            flush_frames,
        })
    }

    fn convert(&mut self, stereo: &[f32]) -> io::Result<&[f32]> {
        self.output.clear();
        self.input.extend_from_slice(stereo);
        let mut consumed = 0;
        loop {
            let needed = self.resampler.input_frames_next() * STEREO;
            if self.input.len() - consumed < needed {
                break;
            }
            self.resampler
                .process(&self.input[consumed..consumed + needed], &mut self.output)
                .map_err(to_io)?;
            consumed += needed;
        }
        self.input.drain(..consumed);
        Ok(&self.output)
    }

    /// 录音结束时用静音把还留在滤波器里的样本推出来，否则结尾会少几毫秒。
    fn flush(&mut self) -> io::Result<&[f32]> {
        let silence = vec![0.0; self.flush_frames * STEREO];
        self.convert(&silence)
    }
}

enum Writer {
    Wav(WavSink),
    Flac(Box<FlacSink>),
}

/// 一个正在写的录音文件。
pub struct RecordingFile {
    writer: Writer,
    /// 只有 FLAC 且引擎采样率超过上限时存在
    downsampler: Option<Downsampler>,
    /// 写入的帧数（引擎采样率），用来计算时长
    frames: u64,
}

impl RecordingFile {
    /// `limit` 是 WAV 每段的数据字节数上限（测试时调小）。
    pub fn open(path: &Path, settings: RecorderSettings, rate: SampleRate, limit: u64) -> io::Result<Self> {
        let (writer, downsampler) = match settings.format {
            RecordFormat::Wav => (Writer::Wav(WavSink::open(path, settings.depth, rate, limit)?), None),
            RecordFormat::Flac => {
                let file_rate = flac_rate(rate)?;
                // 先建降采样器：失败时不留下空文件
                let downsampler = (file_rate != rate)
                    .then(|| Downsampler::new(rate, file_rate))
                    .transpose()?;
                let sink = FlacSink::open(path, settings.depth, file_rate)?;
                (Writer::Flac(Box::new(sink)), downsampler)
            }
        };
        Ok(Self {
            writer,
            downsampler,
            frames: 0,
        })
    }

    pub fn write(&mut self, stereo: &[f32]) -> io::Result<()> {
        match (&mut self.writer, &mut self.downsampler) {
            (Writer::Wav(sink), _) => sink.write(stereo)?,
            (Writer::Flac(sink), Some(downsampler)) => sink.write(downsampler.convert(stereo)?)?,
            (Writer::Flac(sink), None) => sink.write(stereo)?,
        }
        self.frames += (stereo.len() / STEREO) as u64;
        Ok(())
    }

    /// 每秒调用一次。
    pub fn sync(&mut self) -> io::Result<()> {
        match &mut self.writer {
            Writer::Wav(sink) => sink.sync(),
            Writer::Flac(_) => Ok(()),
        }
    }

    pub fn finish(mut self) -> io::Result<()> {
        match &mut self.writer {
            Writer::Wav(sink) => sink.close_part(),
            Writer::Flac(sink) => {
                if let Some(downsampler) = &mut self.downsampler {
                    sink.write(downsampler.flush()?)?;
                }
                sink.finish()
            }
        }
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn bytes(&self) -> u64 {
        match &self.writer {
            Writer::Wav(sink) => sink.bytes(),
            Writer::Flac(sink) => sink.bytes,
        }
    }

    /// 当前正在写的文件（分段后是最新一段）。
    pub fn current_path(&self, base: &Path) -> PathBuf {
        match &self.writer {
            Writer::Wav(sink) => part_path(base, sink.part),
            Writer::Flac(_) => base.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aemeath-recording-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn settings(format: RecordFormat, depth: RecordDepth) -> RecorderSettings {
        RecorderSettings { format, depth }
    }

    fn ramp(frames: usize) -> Vec<f32> {
        (0..frames * STEREO)
            .map(|index| (index as f32 * 0.0001).sin())
            .collect()
    }

    #[test]
    fn part_names_follow_the_base_name() {
        let base = Path::new(r"C:\rec\mic_2026.wav");
        assert_eq!(part_path(base, 1), base);
        assert_eq!(part_path(base, 3), Path::new(r"C:\rec\mic_2026_part3.wav"));
    }

    #[test]
    fn quantization_clamps_at_full_scale() {
        assert_eq!(quantize(2.0, 16), i32::from(i16::MAX));
        assert_eq!(quantize(-2.0, 16), i32::from(i16::MIN));
        assert_eq!(quantize(0.0, 24), 0);
    }

    #[test]
    fn wav_splits_into_parts_and_every_part_is_readable() {
        let dir = temp_dir("wav");
        let base = dir.join("take.wav");
        let frame_bytes = 4u64;
        let limit = 1000 * frame_bytes;
        let mut file = RecordingFile::open(
            &base,
            settings(RecordFormat::Wav, RecordDepth::Int16),
            SampleRate::HZ_48000,
            limit,
        )
        .unwrap();
        file.write(&ramp(2500)).unwrap();
        assert_eq!(file.frames(), 2500);
        assert_eq!(file.bytes(), 2500 * frame_bytes);
        assert_eq!(file.current_path(&base), dir.join("take_part3.wav"));
        file.finish().unwrap();
        let lengths: Vec<u32> = ["take.wav", "take_part2.wav", "take_part3.wav"]
            .iter()
            .map(|name| hound::WavReader::open(dir.join(name)).unwrap().duration())
            .collect();
        assert_eq!(lengths, vec![1000, 1000, 500]);
    }

    #[test]
    fn wav_is_readable_before_it_is_finished() {
        let dir = temp_dir("crash");
        let base = dir.join("take.wav");
        let mut file = RecordingFile::open(
            &base,
            settings(RecordFormat::Wav, RecordDepth::Int24),
            SampleRate::HZ_48000,
            WAV_SEGMENT_LIMIT,
        )
        .unwrap();
        file.write(&ramp(480)).unwrap();
        file.sync().unwrap();
        // 不调用 finish，模拟进程被结束
        std::mem::forget(file);
        assert_eq!(hound::WavReader::open(&base).unwrap().duration(), 480);
    }

    #[test]
    fn wav_float_round_trips() {
        let dir = temp_dir("float");
        let base = dir.join("take.wav");
        let samples = ramp(100);
        let mut file = RecordingFile::open(
            &base,
            settings(RecordFormat::Wav, RecordDepth::Float32),
            SampleRate::HZ_48000,
            WAV_SEGMENT_LIMIT,
        )
        .unwrap();
        file.write(&samples).unwrap();
        file.finish().unwrap();
        let decoded: Vec<f32> = hound::WavReader::open(&base)
            .unwrap()
            .samples::<f32>()
            .map(Result::unwrap)
            .collect();
        assert_eq!(decoded, samples);
    }

    #[test]
    fn flac_round_trips_sample_exactly_and_records_the_length() {
        let dir = temp_dir("flac");
        let base = dir.join("take.flac");
        let samples = ramp(FLAC_BLOCK_FRAMES * 3 + 100);
        let mut file = RecordingFile::open(
            &base,
            settings(RecordFormat::Flac, RecordDepth::Int24),
            SampleRate::HZ_48000,
            WAV_SEGMENT_LIMIT,
        )
        .unwrap();
        file.write(&samples).unwrap();
        file.finish().unwrap();
        let mut reader = claxon::FlacReader::open(&base).unwrap();
        assert_eq!(reader.streaminfo().samples, Some((samples.len() / STEREO) as u64));
        let decoded: Vec<i32> = reader.samples().map(Result::unwrap).collect();
        let expected: Vec<i32> = samples.iter().map(|sample| quantize(*sample, 24)).collect();
        assert_eq!(decoded, expected);
    }

    /// 1 秒 1 kHz 正弦（幅度 0.5），分成不规则的块写入，返回解码出的 (采样率, 左声道)
    fn record_flac_at(engine_hz: u32) -> (u32, Vec<f32>) {
        let dir = temp_dir(&format!("flac-{engine_hz}"));
        let base = dir.join("take.flac");
        let frames = engine_hz as usize;
        let samples: Vec<f32> = (0..frames)
            .flat_map(|frame| {
                let phase = std::f64::consts::TAU * 1000.0 * frame as f64 / f64::from(engine_hz);
                let value = (phase.sin() * 0.5) as f32;
                [value, value]
            })
            .collect();
        let mut file = RecordingFile::open(
            &base,
            settings(RecordFormat::Flac, RecordDepth::Int24),
            SampleRate::new(engine_hz).unwrap(),
            WAV_SEGMENT_LIMIT,
        )
        .unwrap();
        for chunk in samples.chunks(1234) {
            file.write(chunk).unwrap();
        }
        assert_eq!(file.frames(), frames as u64, "the duration counts engine frames");
        file.finish().unwrap();
        let mut reader = claxon::FlacReader::open(&base).unwrap();
        let rate = reader.streaminfo().sample_rate;
        let full_scale = (1i64 << 23) as f32;
        let left = reader
            .samples()
            .map(|sample| sample.unwrap() as f32 / full_scale)
            .step_by(STEREO)
            .collect();
        (rate, left)
    }

    #[test]
    fn flac_above_the_encoder_limit_is_downsampled() {
        for (engine_hz, expected_hz) in [
            (192_000, 96_000),
            (176_400, 88_200),
            (768_000, 96_000),
            (705_600, 88_200),
        ] {
            let (rate, left) = record_flac_at(engine_hz);
            assert_eq!(rate, expected_hz, "{engine_hz}");
            let frames = expected_hz as usize;
            assert!(
                left.len() >= frames && left.len() <= frames + frames / 10,
                "{engine_hz}: {} frames",
                left.len()
            );
            // 跳过开头滤波器的建立时间，只看稳定段
            let peak = left[frames / 4..frames * 3 / 4]
                .iter()
                .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            assert!((peak - 0.5).abs() < 0.01, "{engine_hz}: peak {peak}");
        }
    }

    #[test]
    fn flac_up_to_the_limit_keeps_the_engine_rate() {
        let (rate, left) = record_flac_at(96_000);
        assert_eq!(rate, 96_000);
        assert_eq!(left.len(), 96_000);
    }

    #[test]
    fn wav_keeps_a_rate_flac_cannot_store() {
        let dir = temp_dir("wav-192k");
        let base = dir.join("take.wav");
        let mut file = RecordingFile::open(
            &base,
            settings(RecordFormat::Wav, RecordDepth::Int24),
            SampleRate::new(192_000).unwrap(),
            WAV_SEGMENT_LIMIT,
        )
        .unwrap();
        file.write(&ramp(1000)).unwrap();
        file.finish().unwrap();
        let reader = hound::WavReader::open(&base).unwrap();
        assert_eq!(reader.spec().sample_rate, 192_000);
        assert_eq!(reader.duration(), 1000);
    }

    #[test]
    fn flac_is_readable_when_the_process_is_killed() {
        let dir = temp_dir("flac-crash");
        let base = dir.join("take.flac");
        let mut file = RecordingFile::open(
            &base,
            settings(RecordFormat::Flac, RecordDepth::Int16),
            SampleRate::HZ_48000,
            WAV_SEGMENT_LIMIT,
        )
        .unwrap();
        file.write(&ramp(FLAC_BLOCK_FRAMES * 2 + 10)).unwrap();
        std::mem::forget(file);
        let mut reader = claxon::FlacReader::open(&base).unwrap();
        assert_eq!(reader.streaminfo().samples, None, "the header has not been patched");
        let decoded = reader.samples().map_while(Result::ok).count();
        assert_eq!(decoded, FLAC_BLOCK_FRAMES * 2 * STEREO);
    }

    #[test]
    fn an_existing_file_is_not_overwritten() {
        let dir = temp_dir("exists");
        let base = dir.join("take.flac");
        std::fs::write(&base, b"keep me").unwrap();
        let result = RecordingFile::open(
            &base,
            settings(RecordFormat::Flac, RecordDepth::Int16),
            SampleRate::HZ_48000,
            WAV_SEGMENT_LIMIT,
        );
        assert_eq!(
            result.err().map(|error| error.kind()),
            Some(io::ErrorKind::AlreadyExists)
        );
        assert_eq!(std::fs::read(&base).unwrap(), b"keep me");
    }
}
