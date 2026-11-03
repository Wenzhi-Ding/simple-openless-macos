//! 共享 PCM 时长计算。
//!
//! 录音统一是 16 kHz / 单声道 / 16-bit 小端 PCM，时长换算
//! `(字节数 / 2) * 1000 / 16000`，由本模块提供唯一实现。

/// 每个采样的字节数（16-bit → 2 字节）。
const PCM_BYTES_PER_SAMPLE: u64 = 2;
/// 采样率（16 kHz）。
const PCM_SAMPLE_RATE_HZ: u64 = 16_000;

/// 由原始字节数计算 16 kHz / 单声道 / 16-bit PCM 的时长（毫秒）。
pub fn pcm_duration_ms_from_bytes(bytes: u64) -> u64 {
    (bytes / PCM_BYTES_PER_SAMPLE) * 1000 / PCM_SAMPLE_RATE_HZ
}

/// 由 PCM 字节切片计算 16 kHz / 单声道 / 16-bit PCM 的时长（毫秒）。
pub fn pcm_duration_ms(pcm: &[u8]) -> u64 {
    pcm_duration_ms_from_bytes(pcm.len() as u64)
}

/// 按最长时长把 16 kHz / 单声道 / 16-bit PCM 切成连续的块。
///
/// 返回的是原切片的子切片，不复制数据。块边界按采样对齐，每块都是完整的
/// int16 采样序列；最后一块装余数。`max_chunk_duration_ms` 为 0、或音频总长
/// 不超过上限时返回整段（短音频行为不变）。
pub fn split_pcm_by_duration(pcm: &[u8], max_chunk_duration_ms: u64) -> Vec<&[u8]> {
    let samples_per_chunk = PCM_SAMPLE_RATE_HZ.saturating_mul(max_chunk_duration_ms) / 1_000;
    let bytes_per_chunk = samples_per_chunk.saturating_mul(PCM_BYTES_PER_SAMPLE);
    if bytes_per_chunk == 0 || pcm.len() as u64 <= bytes_per_chunk {
        return vec![pcm];
    }
    let Ok(bytes_per_chunk) = usize::try_from(bytes_per_chunk) else {
        return vec![pcm];
    };
    pcm.chunks(bytes_per_chunk).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_second_of_16k_i16_pcm_is_1000ms() {
        // 16000 采样 × 2 字节 = 32000 字节 = 1 秒
        assert_eq!(pcm_duration_ms(&vec![0u8; 32_000]), 1000);
        assert_eq!(pcm_duration_ms_from_bytes(32_000), 1000);
    }

    #[test]
    fn odd_trailing_byte_is_floored() {
        // 末尾半个采样向下取整，与历史行为一致
        assert_eq!(pcm_duration_ms(&[0u8; 33]), pcm_duration_ms(&[0u8; 32]));
    }

    #[test]
    fn split_keeps_audio_at_the_limit_as_one_chunk() {
        // 恰好 30 秒：不超过上限，保持单块
        let thirty_seconds = vec![0u8; 32_000 * 30];
        assert_eq!(
            split_pcm_by_duration(&thirty_seconds, 30_000),
            vec![thirty_seconds.as_slice()]
        );
    }

    #[test]
    fn split_uses_sample_aligned_30_second_boundaries() {
        // 65 秒 = 30 秒 + 30 秒 + 5 秒，块边界落在完整采样上
        let pcm = vec![0u8; 32_000 * 65];
        let chunks = split_pcm_by_duration(&pcm, 30_000);

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), 32_000 * 30);
        assert_eq!(chunks[1].len(), 32_000 * 30);
        assert_eq!(chunks[2].len(), 32_000 * 5);
        assert_eq!(chunks[0].len() % 2, 0);
        assert_eq!(chunks[2].len() % 2, 0);
    }

    #[test]
    fn split_with_zero_limit_falls_back_to_one_chunk() {
        let pcm = vec![0u8; 96_000];
        assert_eq!(split_pcm_by_duration(&pcm, 0), vec![pcm.as_slice()]);
    }

    #[test]
    fn split_of_short_audio_is_unchanged() {
        let pcm = vec![7u8; 16_000];
        assert_eq!(split_pcm_by_duration(&pcm, 30_000), vec![pcm.as_slice()]);
    }
}
