//! Framework-independent audio normalization used by native recorder adapters.

pub const DICTATION_SAMPLE_RATE: u32 = 16_000;
const LEVEL_RMS_GAIN: f32 = 4.0;

#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedPcmChunk {
    pub pcm_i16_le: Vec<u8>,
    pub level: f32,
}

/// Stateful interleaved-audio normalizer.
///
/// Native adapters convert their device sample format to `f32` and pass each
/// callback here. The output contract is always 16 kHz, mono, signed 16-bit
/// little-endian PCM plus the UI level used by both hosts.
#[derive(Debug, Default)]
pub struct PcmNormalizer {
    resample_phase: f64,
    last_sample: f32,
}

pub fn encode_dictation_wav(pcm_i16_le: &[u8]) -> Result<Vec<u8>, crate::BackendError> {
    if !pcm_i16_le.len().is_multiple_of(2) {
        return Err(crate::BackendError::new(
            crate::BackendErrorCode::InvalidArgument,
            "dictation PCM length must be a multiple of two bytes",
        ));
    }
    let data_size = u32::try_from(pcm_i16_le.len()).map_err(|_| {
        crate::BackendError::new(
            crate::BackendErrorCode::InvalidArgument,
            "dictation PCM is too large for a WAV container",
        )
    })?;
    let riff_size = 36_u32.checked_add(data_size).ok_or_else(|| {
        crate::BackendError::new(
            crate::BackendErrorCode::InvalidArgument,
            "dictation WAV size overflow",
        )
    })?;
    let byte_rate = DICTATION_SAMPLE_RATE * 2;
    let mut wav = Vec::with_capacity(44 + pcm_i16_le.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_size.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes());
    wav.extend_from_slice(&DICTATION_SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&2_u16.to_le_bytes());
    wav.extend_from_slice(&16_u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav.extend_from_slice(pcm_i16_le);
    Ok(wav)
}

impl PcmNormalizer {
    pub fn process(
        &mut self,
        interleaved: &[f32],
        channels: usize,
        input_sample_rate: u32,
    ) -> Option<NormalizedPcmChunk> {
        if interleaved.is_empty() || channels == 0 || input_sample_rate == 0 {
            return None;
        }
        let mono = downmix_to_mono(interleaved, channels);
        let resampled = self.resample(&mono, input_sample_rate, DICTATION_SAMPLE_RATE);
        if resampled.is_empty() {
            return None;
        }
        let (pcm_i16_le, rms) = quantize_to_i16_le(&resampled);
        Some(NormalizedPcmChunk {
            pcm_i16_le,
            level: (rms * LEVEL_RMS_GAIN).clamp(0.0, 1.0),
        })
    }

    fn resample(&mut self, samples: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
        if samples.is_empty() {
            return Vec::new();
        }
        if source_rate == target_rate {
            self.last_sample = *samples.last().unwrap_or(&0.0);
            return samples.to_vec();
        }

        let step = source_rate as f64 / target_rate as f64;
        let mut phase = self.resample_phase;
        let mut output = Vec::with_capacity(
            ((samples.len() as f64) / step).ceil() as usize + usize::from(step < 1.0),
        );
        while phase < samples.len() as f64 {
            let floor = phase.floor() as isize;
            let fraction = (phase - phase.floor()) as f32;
            let left = if floor < 0 {
                self.last_sample
            } else {
                samples[floor as usize]
            };
            let right_index = (floor + 1) as usize;
            if right_index >= samples.len() {
                output.push(left);
                phase += step;
                break;
            }
            let right = samples[right_index];
            output.push(left + (right - left) * fraction);
            phase += step;
        }
        self.resample_phase = (phase - samples.len() as f64).max(0.0);
        self.last_sample = *samples.last().unwrap_or(&0.0);
        output
    }
}

/// 送识别前的语音门限(原始 RMS 刻度)。与 UI 电平、静音自动停止共用同一语义:
/// `SPEECH_LEVEL_THRESHOLD` 定义在 UI 电平(RMS × LEVEL_RMS_GAIN)刻度上,
/// 换算回原始 RMS 即本值,两处判定不会出现"UI 认为在说话、整流认为全静音"的裂缝。
pub const SPEECH_RMS_LEVEL: f32 = crate::silence_auto_stop::SPEECH_LEVEL_THRESHOLD / LEVEL_RMS_GAIN;

/// 语音判定窗口长度(30 ms @ 16 kHz)。按窗口而不是按单个样本判 RMS,
/// 避免辅音里正常的低幅度样本把语音段切碎。
const SPEECH_WINDOW_SAMPLES: usize = 480;
/// 裁剪余量:语音前保留 200 ms、语音后保留 320 ms,避免削掉辅音起头和拖尾元音。
const SPEECH_PAD_SAMPLES_BEFORE: usize = DICTATION_SAMPLE_RATE as usize * 200 / 1000;
const SPEECH_PAD_SAMPLES_AFTER: usize = DICTATION_SAMPLE_RATE as usize * 320 / 1000;

/// [`condition_dictation_pcm`] 的结果。`has_speech` 为 false 时 `pcm` 为空
/// (调用方按"未检测到语音"处理,不再送识别,无需原始音频)。
pub struct ConditionedPcm {
    pub pcm: Vec<u8>,
    pub has_speech: bool,
}

/// 裁掉录音首尾静音;整段都低于语音门限时判定为无语音。
///
/// 中间的停顿不动,只裁首尾;输入是规范的 16 kHz 单声道 s16le PCM,
/// 奇数结尾字节按半样本丢弃。这是送识别前的确定性预处理,不改变语音内容。
pub fn condition_dictation_pcm(pcm_i16_le: &[u8]) -> ConditionedPcm {
    let samples: Vec<i16> = pcm_i16_le
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    let mut first_speech: Option<usize> = None;
    let mut last_speech_end: usize = 0;
    for (index, window) in samples.chunks(SPEECH_WINDOW_SAMPLES).enumerate() {
        let square_sum: f64 = window
            .iter()
            .map(|&sample| {
                let normalized = f64::from(sample) / f64::from(i16::MAX);
                normalized * normalized
            })
            .sum();
        let rms = (square_sum / window.len() as f64).sqrt() as f32;
        if rms >= SPEECH_RMS_LEVEL {
            first_speech.get_or_insert(index * SPEECH_WINDOW_SAMPLES);
            last_speech_end = (index + 1) * SPEECH_WINDOW_SAMPLES;
        }
    }
    let Some(speech_start) = first_speech else {
        // 无语音时不拷贝原始音频:唯一调用方只看 has_speech,整段拷贝纯浪费。
        return ConditionedPcm {
            pcm: Vec::new(),
            has_speech: false,
        };
    };
    let trimmed_start = speech_start.saturating_sub(SPEECH_PAD_SAMPLES_BEFORE);
    let trimmed_end = samples
        .len()
        .min(last_speech_end + SPEECH_PAD_SAMPLES_AFTER);
    let mut pcm = Vec::with_capacity((trimmed_end - trimmed_start) * 2);
    for sample in &samples[trimmed_start..trimmed_end] {
        pcm.extend_from_slice(&sample.to_le_bytes());
    }
    ConditionedPcm {
        pcm,
        has_speech: true,
    }
}

fn downmix_to_mono(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels == 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|frame| frame.iter().copied().sum::<f32>() / channels as f32)
        .collect()
}

fn quantize_to_i16_le(samples: &[f32]) -> (Vec<u8>, f32) {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    let mut square_sum = 0.0_f64;
    for sample in samples {
        let normalized = sample.clamp(-1.0, 1.0);
        let quantized = (normalized * i16::MAX as f32) as i16;
        bytes.extend_from_slice(&quantized.to_le_bytes());
        square_sum += f64::from(normalized) * f64::from(normalized);
    }
    let rms = (square_sum / samples.len() as f64).sqrt() as f32;
    (bytes, rms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Vec<i16> {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
            .collect()
    }

    #[test]
    fn normalizer_downmixes_quantizes_and_scales_level() {
        let mut normalizer = PcmNormalizer::default();
        let output = normalizer
            .process(&[1.0, -1.0, 0.5, 0.5], 2, DICTATION_SAMPLE_RATE)
            .unwrap();
        assert_eq!(decode(&output.pcm_i16_le), vec![0, 16383]);
        assert_eq!(output.level, 1.0);
    }

    #[test]
    fn normalizer_resamples_to_sixteen_kilohertz_across_callbacks() {
        let mut normalizer = PcmNormalizer::default();
        let first = normalizer.process(&[0.0, 1.0], 1, 8_000).unwrap();
        let second = normalizer.process(&[1.0, 0.0], 1, 8_000).unwrap();
        assert_eq!(decode(&first.pcm_i16_le), vec![0, 16383, 32767]);
        assert_eq!(decode(&second.pcm_i16_le), vec![32767, 16383, 0]);
    }

    #[test]
    fn normalizer_rejects_empty_or_invalid_input_without_emitting_pcm() {
        let mut normalizer = PcmNormalizer::default();
        assert!(normalizer.process(&[], 1, 48_000).is_none());
        assert!(normalizer.process(&[0.0], 0, 48_000).is_none());
        assert!(normalizer.process(&[0.0], 1, 0).is_none());
    }

    #[test]
    fn wav_encoder_preserves_canonical_pcm_and_rejects_partial_samples() {
        let pcm = [1, 0, 255, 127];
        let wav = encode_dictation_wav(&pcm).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(&wav[44..], &pcm);
        assert_eq!(
            encode_dictation_wav(&[1]).unwrap_err().code,
            crate::BackendErrorCode::InvalidArgument
        );
    }

    #[test]
    fn speech_threshold_stays_consistent_with_ui_level_scale() {
        assert_eq!(
            SPEECH_RMS_LEVEL * LEVEL_RMS_GAIN,
            crate::silence_auto_stop::SPEECH_LEVEL_THRESHOLD
        );
    }

    #[test]
    fn all_silent_pcm_reports_no_speech_without_copying_input() {
        let pcm = vec![0u8; 32_000];
        let conditioned = condition_dictation_pcm(&pcm);
        assert!(!conditioned.has_speech);
        assert!(conditioned.pcm.is_empty());
    }

    #[test]
    fn conditioning_trims_leading_and_trailing_silence_only() {
        let silence_1s = vec![0u8; 16_000 * 2];
        let speech_500ms: Vec<u8> = vec![0x39, 0x19].repeat(8_000); // 恒定幅度约 0.2,窗口 RMS 远超门限
        let mut pcm = silence_1s.clone();
        pcm.extend_from_slice(&speech_500ms);
        pcm.extend_from_slice(&silence_1s);

        let conditioned = condition_dictation_pcm(&pcm);
        assert!(conditioned.has_speech);
        // 语音区间按窗口对齐,边界各可能多出不足一个窗口的余量。
        let trimmed_samples = conditioned.pcm.len() / 2;
        let speech_and_pads = SPEECH_PAD_SAMPLES_BEFORE + 8_000 + SPEECH_PAD_SAMPLES_AFTER;
        assert!(trimmed_samples >= speech_and_pads);
        assert!(trimmed_samples <= speech_and_pads + 2 * SPEECH_WINDOW_SAMPLES);
    }

    #[test]
    fn speech_at_clip_edges_keeps_padding_within_bounds() {
        let speech_300ms: Vec<u8> = vec![0x39, 0x19].repeat(4_800);
        let conditioned = condition_dictation_pcm(&speech_300ms);
        assert!(conditioned.has_speech);
        assert_eq!(conditioned.pcm, speech_300ms);
    }

    #[test]
    fn conditioning_ignores_odd_trailing_byte() {
        let mut pcm = vec![0u8; 2_000];
        pcm.extend_from_slice(&[0x39, 0x19]);
        pcm.push(0x00);
        let conditioned = condition_dictation_pcm(&pcm);
        assert!(conditioned.has_speech);
        assert_eq!(conditioned.pcm.len() % 2, 0);
    }
}
