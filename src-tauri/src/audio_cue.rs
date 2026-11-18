//! 听写提示音的宿主侧发声：cpal 输出流直接合成两枚短音，不经 WebKit Web Audio。
//!
//! 为什么放在 Rust 侧（2026-11-03 用户批准的结构性偏离，见 docs/upstream-sync.md）：
//! WKWebView 渲染进程的音频输出通道会在多次听写（麦克风反复开关、音频会话被反复
//! 抢占）后卡死——卡死后 `AudioContext.state` 照常报 running、时钟照常走、节点照常
//! 「正常结束」，网页侧既探测不到也无法恢复，只有重启整个进程才能恢复。现场证据：
//! 同一会话响几十次后提示音与设置页试听双双静默，重启即愈，且 Web Audio 的
//! suspended/interrupted 自愈逻辑（已废弃的旧实现）没能拦住。宿主进程自己的
//! CoreAudio 通道（听写收音）全程稳定，所以发声换到这一侧。
//!
//! 一次播放 = 一个专属输出流，播完或被取代后立即关闭，不常驻占用输出设备。
//! 新播放推进全局 generation：旧流回调比对失败即只写静音，监视线程随即收流——
//! 与旧 Web Audio 版「新播放先停上一轮」的去叠音行为一致。任何失败（无输出设备、
//! 打流失败）只 log::warn 并放弃：提示音是非关键 UI 反馈，绝不影响听写主流程。

use cpal::traits::{DeviceTrait, HostTrait};
use std::f32::consts::TAU;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 全局播放代号：play/stop 各推进一次。持有旧代号的流一律静音收线。
static CUE_GENERATION: AtomicU64 = AtomicU64::new(0);

/// 指数包络的收尾幅度（与旧 Web Audio 版 `exponentialRampToValueAtTime(0.0001)` 对齐）：
/// 非 0 是指数 ramp 的数学要求，-80 dB 也已不可闻，收尾无爆音。
const ENVELOPE_FLOOR: f32 = 0.0001;
/// 起音时长（旧版 5ms attack，避免 click 爆音）。
const ATTACK_SECS: f32 = 0.005;
/// 末音结束后的静音尾量（旧版 `osc.stop(tEnd + 0.02)`）。
const TAIL_SECS: f32 = 0.02;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CueKind {
    /// 开始录音：880 Hz → 1108.73 Hz 上行「叮咚」。
    Start,
    /// 正常结束录音：1108.73 Hz → 880 Hz 下行「咚叮」，时间结构镜像 start。
    Stop,
}

impl CueKind {
    /// 前端 IPC 入参解析；未知值返回 None，由命令层转成错误。
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "start" => Some(Self::Start),
            "stop" => Some(Self::Stop),
            _ => None,
        }
    }

    fn tones(self) -> [CueTone; 2] {
        match self {
            Self::Start => [
                CueTone { freq_hz: 880.0, start_secs: 0.0, duration_secs: 0.130, peak_gain: 0.16 },
                CueTone { freq_hz: 1108.73, start_secs: 0.095, duration_secs: 0.170, peak_gain: 0.18 },
            ],
            Self::Stop => [
                CueTone { freq_hz: 1108.73, start_secs: 0.0, duration_secs: 0.130, peak_gain: 0.16 },
                CueTone { freq_hz: 880.0, start_secs: 0.095, duration_secs: 0.170, peak_gain: 0.18 },
            ],
        }
    }
}

struct CueTone {
    freq_hz: f32,
    start_secs: f32,
    duration_secs: f32,
    peak_gain: f32,
}

/// 把提示音合成成单声道 f32 采样（按输出设备当前采样率），两音交叠区直接相加。
fn synth_cue_samples(kind: CueKind, sample_rate: u32) -> Vec<f32> {
    let sample_rate = sample_rate as f32;
    let tones = kind.tones();
    let total_secs = tones
        .iter()
        .map(|tone| tone.start_secs + tone.duration_secs)
        .fold(0.0_f32, f32::max)
        + TAIL_SECS;
    let total_samples = (total_secs * sample_rate).ceil() as usize;
    let mut out = vec![0.0_f32; total_samples];
    for tone in &tones {
        let start_index = (tone.start_secs * sample_rate) as usize;
        let tone_samples = (tone.duration_secs * sample_rate) as usize;
        let attack_samples = ((ATTACK_SECS * sample_rate) as usize).max(1);
        // 每个音的衰减斜率固定：从峰值指数衰减到 ENVELOPE_FLOOR。
        let decay_rate = (ENVELOPE_FLOOR / tone.peak_gain).ln();
        for i in 0..tone_samples {
            let envelope = if i < attack_samples {
                // 线性起音：ENVELOPE_FLOOR → 峰值（旧版 setValueAtTime(0.0001) + 5ms ramp）。
                ENVELOPE_FLOOR
                    + (tone.peak_gain - ENVELOPE_FLOOR) * (i as f32 / attack_samples as f32)
            } else {
                tone.peak_gain
                    * (decay_rate * ((i - attack_samples) as f32
                        / (tone_samples - attack_samples) as f32))
                        .exp()
            };
            let phase = TAU * tone.freq_hz * ((start_index + i) as f32 / sample_rate);
            let index = start_index + i;
            if index < out.len() {
                out[index] += envelope * phase.sin();
            }
        }
    }
    for sample in &mut out {
        *sample = sample.clamp(-1.0, 1.0);
    }
    out
}

/// 播放提示音。立即返回，发声在后台线程；同一时刻最多一声，新播放截断上一轮。
pub fn play(kind: CueKind) {
    let generation = CUE_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    if let Err(err) = std::thread::Builder::new()
        .name("openless-audio-cue".into())
        .spawn(move || run_playback(kind, generation))
    {
        log::warn!("[audio-cue] spawn playback thread failed: {err}");
    }
}

/// 截断当前在播的提示音（不另起新音）。
pub fn stop() {
    CUE_GENERATION.fetch_add(1, Ordering::AcqRel);
}

fn run_playback(kind: CueKind, generation: u64) {
    // 与 recorder.rs 同一取设备路径：默认宿主 → 默认输出设备。每次播放现取，
    // 不缓存设备句柄——输出设备被切走后旧句柄正是 WebKit 卡死同类的失效源。
    let Some(device) = cpal::default_host().default_output_device() else {
        log::warn!("[audio-cue] no default output device; cue skipped");
        return;
    };
    let config = match device.default_output_config() {
        Ok(config) => config,
        Err(err) => {
            log::warn!("[audio-cue] query default output config failed: {err}");
            return;
        }
    };
    if config.sample_format() != cpal::SampleFormat::F32 {
        log::warn!(
            "[audio-cue] unsupported output sample format {:?}; cue skipped",
            config.sample_format()
        );
        return;
    }
    let samples = Arc::new(synth_cue_samples(kind, config.sample_rate().0));
    if samples.is_empty() {
        return;
    }
    let total_len = samples.len();
    let position = Arc::new(AtomicUsize::new(0));
    let stream_config: cpal::StreamConfig = config.clone().into();
    let callback_samples = Arc::clone(&samples);
    let callback_position = Arc::clone(&position);
    let stream = match device.build_output_stream(
        &stream_config,
        move |data: &mut [f32], _info: &cpal::OutputCallbackInfo| {
            // 被新一代播放/截断取代：只写静音，让监视线程去收流。
            if CUE_GENERATION.load(Ordering::Acquire) != generation {
                data.fill(0.0);
                return;
            }
            let start = callback_position.load(Ordering::Acquire);
            let take = total_len.saturating_sub(start).min(data.len());
            data[..take].copy_from_slice(&callback_samples[start..start + take]);
            data[take..].fill(0.0);
            callback_position.store(start + take, Ordering::Release);
        },
        |err| log::warn!("[audio-cue] output stream error: {err}"),
        None,
    ) {
        Ok(stream) => stream,
        Err(err) => {
            log::warn!("[audio-cue] open output stream failed: {err}");
            return;
        }
    };
    // 播完（或被取代、或兜底超时）后收流，交还输出设备。兜底超时防监视线程泄漏。
    let deadline = Instant::now()
        + Duration::from_secs_f32(total_len as f32 / stream_config.sample_rate.0 as f32 + 1.0);
    loop {
        if CUE_GENERATION.load(Ordering::Acquire) != generation {
            break;
        }
        if position.load(Ordering::Acquire) >= total_len {
            // 位置走完不等于设备缓冲播完：留 50ms 余量再收流，尾巴已在
            // ENVELOPE_FLOOR 幅度，多等也只是静音。
            std::thread::sleep(Duration::from_millis(50));
            break;
        }
        if Instant::now() >= deadline {
            log::warn!("[audio-cue] playback monitor timed out; dropping stream");
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(stream);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_only_start_and_stop() {
        assert_eq!(CueKind::parse("start"), Some(CueKind::Start));
        assert_eq!(CueKind::parse("stop"), Some(CueKind::Stop));
        assert_eq!(CueKind::parse(""), None);
        assert_eq!(CueKind::parse("Start"), None);
        assert_eq!(CueKind::parse("cancel"), None);
    }

    /// start 上行（叮→咚）、stop 下行（咚→叮），时间结构互为镜像。
    #[test]
    fn start_ascends_and_stop_descends_with_mirrored_timing() {
        let start = CueKind::Start.tones();
        assert!(start[1].freq_hz > start[0].freq_hz, "start cue must rise");
        let stop = CueKind::Stop.tones();
        assert!(stop[1].freq_hz < stop[0].freq_hz, "stop cue must fall");
        assert_eq!(start[0].freq_hz, stop[1].freq_hz, "mirror shares frequencies");
        assert_eq!(start[1].freq_hz, stop[0].freq_hz, "mirror shares frequencies");
        for (a, b) in start.iter().zip(stop.iter()) {
            assert_eq!(a.start_secs, b.start_secs, "mirror keeps timing");
            assert_eq!(a.duration_secs, b.duration_secs, "mirror keeps timing");
            assert!(a.peak_gain > 0.0 && a.peak_gain <= 1.0, "gain in (0, 1]");
            assert_eq!(a.start_secs + a.duration_secs, b.start_secs + b.duration_secs);
        }
    }

    #[test]
    fn samples_length_matches_tone_span_plus_tail() {
        // 总时长 0.265s + 0.02s 尾量；48 kHz ≈ 13680，44.1 kHz ≈ 12569。留出
        // ceil 舍入余量的宽松区间，避免把实现公式抄进断言。
        let len_48k = synth_cue_samples(CueKind::Start, 48_000).len();
        assert!(
            (13_400..=13_900).contains(&len_48k),
            "unexpected 48k length: {len_48k}"
        );
        let len_44_1k = synth_cue_samples(CueKind::Stop, 44_100).len();
        assert!(
            (12_300..=12_800).contains(&len_44_1k),
            "unexpected 44.1k length: {len_44_1k}"
        );
    }

    /// 包络健全性：无 NaN、整体不超过两音峰值之和、首尾样本近似静音（无爆音）、
    /// 响度在可闻范围（峰值至少 0.14，接近较响那枚音的峰值）。
    #[test]
    fn samples_envelope_is_click_free_and_audible() {
        for kind in [CueKind::Start, CueKind::Stop] {
            let samples = synth_cue_samples(kind, 48_000);
            assert!(!samples.is_empty());
            let peak = samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
            assert!(peak > 0.14, "{kind:?} peak too quiet: {peak}");
            assert!(peak <= 0.35, "{kind:?} peak beyond summed gains: {peak}");
            assert!(
                samples.iter().all(|s| s.is_finite()),
                "{kind:?} produced non-finite samples"
            );
            assert!(
                samples[0].abs() < 0.01,
                "{kind:?} first sample clicks: {}",
                samples[0]
            );
            assert!(
                samples[samples.len() - 1].abs() < 0.001,
                "{kind:?} last sample clicks: {}",
                samples[samples.len() - 1]
            );
        }
    }

    /// 两音交叠（第二枚起音早于第一枚结束）——听感连成一段而非两声独立 beep。
    #[test]
    fn tones_overlap_into_one_chime() {
        for kind in [CueKind::Start, CueKind::Stop] {
            let tones = kind.tones();
            assert!(
                tones[1].start_secs < tones[0].start_secs + tones[0].duration_secs,
                "{kind:?} tones must overlap"
            );
        }
    }
}
