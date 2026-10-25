//! 最小但完整的 WAV 解码：PCM 8/16/24/32 bit 与 IEEE float 32/64。
//!
//! 只认 RIFF/WAVE；不支持压缩格式（ADPCM 等）。未知块按大小跳过，
//! `fmt ` 在 `data` 之后也接受——真实文件里两种顺序都出现过。

use std::sync::Arc;

/// 解码后的音频片段（交错采样，范围约 `[-1, 1]`）。
#[derive(Clone, Debug)]
pub struct AudioClip {
    sample_rate: u32,
    channels: u16,
    samples: Arc<[f32]>,
}

impl AudioClip {
    /// 从完整 WAV 字节解码。
    pub fn from_wav(bytes: &[u8]) -> Result<Self, AudioError> {
        if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return Err(AudioError::new("不是 RIFF/WAVE 文件"));
        }
        let mut pos = 12usize;
        let mut fmt: Option<Fmt> = None;
        let mut data: Option<&[u8]> = None;
        while pos + 8 <= bytes.len() {
            let id = &bytes[pos..pos + 4];
            let size = u32_at(bytes, pos + 4) as usize;
            let body_start = pos + 8;
            let body_end = body_start
                .checked_add(size)
                .ok_or_else(|| AudioError::new("WAV 块长度溢出"))?;
            if body_end > bytes.len() {
                return Err(AudioError::new("WAV 块长度超出文件"));
            }
            let body = &bytes[body_start..body_end];
            match id {
                b"fmt " => fmt = Some(parse_fmt(body)?),
                b"data" => data = Some(body),
                _ => {}
            }
            pos = body_end + (size & 1);
        }
        let fmt = fmt.ok_or_else(|| AudioError::new("缺少 fmt 块"))?;
        let data = data.ok_or_else(|| AudioError::new("缺少 data 块"))?;
        let bytes_per_sample = fmt.bits_per_sample / 8;
        if bytes_per_sample == 0 || fmt.channels == 0 || fmt.sample_rate == 0 {
            return Err(AudioError::new("WAV 格式参数为零"));
        }
        let frame_bytes = bytes_per_sample as usize * fmt.channels as usize;
        if frame_bytes == 0 || data.len() % frame_bytes != 0 {
            return Err(AudioError::new("data 长度不是整帧"));
        }
        let mut samples = Vec::with_capacity(data.len() / bytes_per_sample as usize);
        for chunk in data.chunks_exact(bytes_per_sample as usize) {
            let v = match (fmt.format, fmt.bits_per_sample) {
                (1, 8) => (chunk[0] as f32 - 128.0) / 128.0,
                (1, 16) => i16::from_le_bytes([chunk[0], chunk[1]]) as f32 / 32768.0,
                (1, 24) => {
                    let v =
                        (chunk[0] as i32) | ((chunk[1] as i32) << 8) | ((chunk[2] as i32) << 16);
                    let v = (v << 8) >> 8;
                    v as f32 / 8_388_608.0
                }
                (1, 32) => {
                    i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f32
                        / 2_147_483_648.0
                }
                (3, 32) => f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
                (3, 64) => f64::from_le_bytes([
                    chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
                ]) as f32,
                _ => {
                    return Err(AudioError::new(format!(
                        "不支持的 WAV 格式：format={} bits={}",
                        fmt.format, fmt.bits_per_sample
                    )))
                }
            };
            if v.is_finite() {
                samples.push(v.clamp(-1.0, 1.0));
            } else {
                samples.push(0.0);
            }
        }
        Ok(Self {
            sample_rate: fmt.sample_rate,
            channels: fmt.channels,
            samples: samples.into(),
        })
    }

    /// 从交错采样直接创建（采样率/声道/长度必须合法）。
    pub fn from_samples(
        sample_rate: u32,
        channels: u16,
        samples: Vec<f32>,
    ) -> Result<Self, AudioError> {
        if sample_rate == 0 || channels == 0 || channels > 32 {
            return Err(AudioError::new("采样率/声道数非法"));
        }
        if !samples.len().is_multiple_of(channels as usize) {
            return Err(AudioError::new("采样长度不是整帧"));
        }
        if samples.iter().any(|s| !s.is_finite()) {
            return Err(AudioError::new("采样包含 NaN/Inf"));
        }
        Ok(Self {
            sample_rate,
            channels,
            samples: samples.into(),
        })
    }

    /// 采样率（Hz）。
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// 声道数。
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// 帧数（每帧包含 `channels` 个采样）。
    pub fn frame_count(&self) -> usize {
        self.samples.len() / self.channels as usize
    }

    /// 时长（秒）。
    pub fn duration(&self) -> f32 {
        self.frame_count() as f32 / self.sample_rate as f32
    }

    /// 读取一帧的某个声道；越界返回 0。
    pub fn sample(&self, frame: usize, channel: usize) -> f32 {
        if channel >= self.channels as usize {
            return 0.0;
        }
        self.samples
            .get(frame * self.channels as usize + channel)
            .copied()
            .unwrap_or(0.0)
    }

    /// 交错采样切片（只读）。
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// 编码为 16-bit PCM WAV（测试/离线导出用）。
    pub fn to_wav_pcm16(&self) -> Vec<u8> {
        let data_len = self.samples.len() * 2;
        let mut out = Vec::with_capacity(44 + data_len);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&self.channels.to_le_bytes());
        out.extend_from_slice(&self.sample_rate.to_le_bytes());
        out.extend_from_slice(&(self.sample_rate * self.channels as u32 * 2).to_le_bytes());
        out.extend_from_slice(&(self.channels * 2).to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data_len as u32).to_le_bytes());
        for s in self.samples.iter() {
            let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }
}

#[derive(Clone, Copy, Debug)]
struct Fmt {
    format: u16,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
}

fn parse_fmt(body: &[u8]) -> Result<Fmt, AudioError> {
    if body.len() < 16 {
        return Err(AudioError::new("fmt 块太短"));
    }
    let mut format = u16_at(body, 0);
    let channels = u16_at(body, 2);
    let sample_rate = u32_at(body, 4);
    let bits_per_sample = u16_at(body, 14);
    if format == 0xFFFE {
        if body.len() < 40 {
            return Err(AudioError::new("WAVE_FORMAT_EXTENSIBLE 太短"));
        }
        // SubFormat GUID 的前 2 字节就是真正的 format tag。
        format = u16_at(body, 24);
    }
    if channels == 0 || channels > 32 {
        return Err(AudioError::new("声道数不在 1..=32"));
    }
    if sample_rate == 0 {
        return Err(AudioError::new("采样率为零"));
    }
    Ok(Fmt {
        format,
        channels,
        sample_rate,
        bits_per_sample,
    })
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// WAV 解码错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioError {
    message: String,
}

impl AudioError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// 可读原因。
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AudioError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav16(channels: u16, rate: u32, samples: &[i16]) -> Vec<u8> {
        let data_len = samples.len() * 2;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        out.extend_from_slice(&(channels * 2).to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data_len as u32).to_le_bytes());
        for s in samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn decodes_pcm16_mono_and_stereo() {
        let clip = AudioClip::from_wav(&wav16(1, 48_000, &[0, 16384, -32768])).unwrap();
        assert_eq!(clip.channels(), 1);
        assert_eq!(clip.sample_rate(), 48_000);
        assert_eq!(clip.frame_count(), 3);
        assert!((clip.sample(0, 0) - 0.0).abs() < 1e-6);
        assert!((clip.sample(1, 0) - 0.5).abs() < 1e-4);
        assert!((clip.sample(2, 0) + 1.0).abs() < 1e-6);
        let stereo = AudioClip::from_wav(&wav16(2, 44_100, &[100, -100, 200, -200])).unwrap();
        assert_eq!(stereo.channels(), 2);
        assert_eq!(stereo.frame_count(), 2);
        assert!(stereo.sample(0, 0) > 0.0 && stereo.sample(0, 1) < 0.0);
    }

    #[test]
    fn pcm16_round_trip_is_close() {
        let clip = AudioClip::from_wav(&wav16(1, 8_000, &[0, 8192, -8192])).unwrap();
        let again = AudioClip::from_wav(&clip.to_wav_pcm16()).unwrap();
        assert_eq!(again.frame_count(), clip.frame_count());
        for i in 0..clip.frame_count() {
            assert!((again.sample(i, 0) - clip.sample(i, 0)).abs() < 1e-4);
        }
    }

    #[test]
    fn rejects_bad_and_unsupported_files() {
        assert!(AudioClip::from_wav(b"nope").is_err());
        let mut bad = wav16(1, 8_000, &[0, 1]);
        bad[20] = 9; // 伪造 format tag
        assert!(AudioClip::from_wav(&bad).is_err());
        // 截断 data 长度
        let mut cut = wav16(1, 8_000, &[0, 1, 2]);
        cut.truncate(cut.len() - 1);
        assert!(AudioClip::from_wav(&cut).is_err());
    }

    #[test]
    fn decodes_24_bit_signed_samples() {
        // +0.5 与 -0.5 的 24-bit 补码。
        let vals: [i32; 2] = [4_194_304, -4_194_304];
        let mut data = Vec::new();
        for v in vals {
            data.extend_from_slice(&[
                (v & 0xFF) as u8,
                ((v >> 8) & 0xFF) as u8,
                ((v >> 16) & 0xFF) as u8,
            ]);
        }
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&8_000u32.to_le_bytes());
        out.extend_from_slice(&(8_000u32 * 3).to_le_bytes());
        out.extend_from_slice(&3u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        let clip = AudioClip::from_wav(&out).unwrap();
        assert!((clip.sample(0, 0) - 0.5).abs() < 1e-5);
        assert!((clip.sample(1, 0) + 0.5).abs() < 1e-5);
    }

    #[test]
    fn decodes_float32() {
        let mut out = Vec::new();
        let samples = [0.25f32, -0.5f32];
        let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&3u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&48_000u32.to_le_bytes());
        out.extend_from_slice(&(48_000u32 * 4).to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        let clip = AudioClip::from_wav(&out).unwrap();
        assert_eq!(clip.frame_count(), 2);
        assert!((clip.sample(0, 0) - 0.25).abs() < 1e-6);
        assert!((clip.sample(1, 0) + 0.5).abs() < 1e-6);
    }
}
