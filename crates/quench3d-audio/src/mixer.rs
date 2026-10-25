//! 多源混音与 3D 距离衰减/声像。纯 CPU、无设备依赖。
//!
//! 输出是**交错立体声** `[L, R, L, R, …]`。每个输出帧按
//! `clip.sample_rate / output_rate * pitch` 推进播放头，线性插值；循环源回绕，
//! 非循环源到结尾后 `playing = false`。采样与声像计算全部是确定性的。

use std::sync::Arc;

use crate::wav::AudioClip;
use quench3d_math::Vec3;
use std::f32::consts::FRAC_PI_4;

/// 声源句柄（混音器内部下标，不复用）。
pub type SourceId = u32;

/// 听者：位置 + 朝向 + 上方向。右方向由 `forward × up` 求出。
#[derive(Clone, Copy, Debug)]
pub struct AudioListener {
    /// 世界位置。
    pub position: Vec3,
    /// 前方（单位向量；缺省 -Z）。
    pub forward: Vec3,
    /// 上方（单位向量；缺省 +Y）。
    pub up: Vec3,
}

impl Default for AudioListener {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            forward: Vec3::NEG_Z,
            up: Vec3::Y,
        }
    }
}

/// 一个播放中的声源。
#[derive(Clone, Debug)]
pub struct AudioSource {
    /// 音频数据（`Arc` 共享，多个声源可复用同一片段）。
    pub clip: Arc<AudioClip>,
    /// 世界位置。
    pub position: Vec3,
    /// 线性音量（0~N；>1 会放大）。
    pub volume: f32,
    /// 播放速度倍率（0.01~8；1 = 原速）。
    pub pitch: f32,
    /// 是否循环。
    pub looping: bool,
    /// 是否正在播放。
    pub playing: bool,
    /// 播放头（帧，可带小数）。
    pub cursor: f64,
}

/// 离线/实时混音器。`mix` 只写缓冲，不碰设备。
#[derive(Debug)]
pub struct AudioMixer {
    output_rate: u32,
    listener: AudioListener,
    sources: Vec<Option<AudioSource>>,
    master: f32,
    /// 距离衰减系数（1/m）；0 = 不衰减。
    pub rolloff: f32,
}

impl AudioMixer {
    /// 新建混音器；`output_rate` 为 0 时用 48 kHz。
    pub fn new(output_rate: u32) -> Self {
        Self {
            output_rate: if output_rate == 0 {
                48_000
            } else {
                output_rate
            },
            listener: AudioListener::default(),
            sources: Vec::new(),
            master: 1.0,
            rolloff: 0.5,
        }
    }

    /// 输出采样率。
    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }

    /// 主音量（0~1 之外也允许，但混音末尾会 clamp 到 [-1,1]）。
    pub fn master_volume(&self) -> f32 {
        self.master
    }

    /// 设置主音量。
    pub fn set_master_volume(&mut self, volume: f32) {
        self.master = if volume.is_finite() {
            volume.max(0.0)
        } else {
            1.0
        };
    }

    /// 当前听者。
    pub fn listener(&self) -> AudioListener {
        self.listener
    }

    /// 设置听者；非有限值/零向量会被拒绝或归一化。
    pub fn set_listener(&mut self, mut listener: AudioListener) {
        if !listener.position.is_finite() {
            return;
        }
        if listener.forward.length_squared() < 1e-12 {
            listener.forward = Vec3::NEG_Z;
        }
        if listener.up.length_squared() < 1e-12 {
            listener.up = Vec3::Y;
        }
        listener.forward = listener.forward.normalize();
        listener.up = listener.up.normalize();
        self.listener = listener;
    }

    /// 播放一个片段；返回句柄。片段为空时仍返回句柄，但会立即结束。
    pub fn play(&mut self, clip: AudioClip, position: Vec3) -> SourceId {
        let source = AudioSource {
            clip: Arc::new(clip),
            position,
            volume: 1.0,
            pitch: 1.0,
            looping: false,
            playing: true,
            cursor: 0.0,
        };
        self.sources.push(Some(source));
        (self.sources.len() - 1) as SourceId
    }

    /// 播放一个已共享的片段。
    pub fn play_shared(&mut self, clip: Arc<AudioClip>, position: Vec3) -> SourceId {
        let source = AudioSource {
            clip,
            position,
            volume: 1.0,
            pitch: 1.0,
            looping: false,
            playing: true,
            cursor: 0.0,
        };
        self.sources.push(Some(source));
        (self.sources.len() - 1) as SourceId
    }

    /// 取声源。
    pub fn source(&self, id: SourceId) -> Option<&AudioSource> {
        self.sources.get(id as usize).and_then(Option::as_ref)
    }

    /// 取可改声源。
    pub fn source_mut(&mut self, id: SourceId) -> Option<&mut AudioSource> {
        self.sources.get_mut(id as usize).and_then(Option::as_mut)
    }

    /// 停止并移除一个声源。
    pub fn stop(&mut self, id: SourceId) {
        if let Some(slot) = self.sources.get_mut(id as usize) {
            *slot = None;
        }
    }

    /// 活跃声源数（`playing` 为 true）。
    pub fn active_sources(&self) -> usize {
        self.sources.iter().flatten().filter(|s| s.playing).count()
    }

    /// 清空所有声源。
    pub fn clear(&mut self) {
        self.sources.clear();
    }

    /// 混一段交错立体声到 `out`；`out.len()` 必须是偶数。
    ///
    /// 这是纯函数式推进：同样的初始状态/输出长度/采样率结果确定。
    pub fn mix(&mut self, out: &mut [f32], output_rate: u32) {
        if out.is_empty() {
            return;
        }
        let rate = if output_rate == 0 {
            self.output_rate
        } else {
            output_rate
        };
        let listener = self.listener;
        let right = listener.forward.cross(listener.up);
        let right = if right.length_squared() > 1e-12 {
            right.normalize()
        } else {
            Vec3::X
        };
        let master = self.master;
        for frame in out.as_chunks_mut::<2>().0 {
            let mut l = 0.0f32;
            let mut r = 0.0f32;
            for slot in &mut self.sources {
                let Some(src) = slot.as_mut() else {
                    continue;
                };
                if !src.playing {
                    continue;
                }
                let total = src.clip.frame_count();
                if total == 0 {
                    src.playing = false;
                    continue;
                }
                let step = src.clip.sample_rate() as f64 / rate as f64 * src.pitch as f64;
                if !step.is_finite() || step <= 0.0 {
                    src.playing = false;
                    continue;
                }
                // 播放头可能因循环回绕；取当前帧与下一帧做线性插值。
                if src.cursor >= total as f64 {
                    if src.looping {
                        src.cursor %= total as f64;
                    } else {
                        src.playing = false;
                        continue;
                    }
                }
                let i0 = src.cursor.floor() as usize;
                let t = (src.cursor - i0 as f64) as f32;
                let i1 = if i0 + 1 < total { i0 + 1 } else { 0 };
                let (sl0, sr0) = stereo_sample(&src.clip, i0);
                let (sl1, sr1) = stereo_sample(&src.clip, i1);
                let sl = sl0 + (sl1 - sl0) * t;
                let sr = sr0 + (sr1 - sr0) * t;

                let delta = src.position - listener.position;
                let dist = delta.length();
                let atten = if self.rolloff > 0.0 {
                    1.0 / (1.0 + dist * self.rolloff)
                } else {
                    1.0
                };
                let pan = if dist > 1e-6 {
                    (delta.dot(right) / dist).clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                let angle = (pan + 1.0) * FRAC_PI_4;
                let gl = angle.cos() * atten * src.volume;
                let gr = angle.sin() * atten * src.volume;
                l += sl * gl;
                r += sr * gr;
                src.cursor += step;
                if src.cursor >= total as f64 && !src.looping {
                    // 让下一次 mix 看到 finished；本帧仍输出最后一点。
                    src.cursor = total as f64;
                }
            }
            frame[0] = (l * master).clamp(-1.0, 1.0);
            frame[1] = (r * master).clamp(-1.0, 1.0);
        }
    }

    /// 生成 `seconds` 秒的交错立体声。
    pub fn mix_seconds(&mut self, seconds: f32) -> Vec<f32> {
        let frames = (seconds.max(0.0) * self.output_rate as f32).round() as usize;
        let mut out = vec![0.0; frames * 2];
        self.mix(&mut out, self.output_rate);
        out
    }
}

fn stereo_sample(clip: &AudioClip, frame: usize) -> (f32, f32) {
    match clip.channels() {
        1 => (clip.sample(frame, 0), clip.sample(frame, 0)),
        2 => (clip.sample(frame, 0), clip.sample(frame, 1)),
        n => {
            // 多声道：前两声道作为左右，其余忽略。
            let ch = n as usize;
            let mut l = 0.0;
            let mut r = 0.0;
            for c in 0..ch {
                if c % 2 == 0 {
                    l += clip.sample(frame, c);
                } else {
                    r += clip.sample(frame, c);
                }
            }
            let half = (ch as f32 * 0.5).max(1.0);
            (l / half, r / half)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(channels: u16, samples: &[f32]) -> AudioClip {
        AudioClip::from_samples(8_000, channels, samples.to_vec()).unwrap()
    }

    #[test]
    fn mono_centered_plays_at_expected_level() {
        let c = clip(1, &[0.5; 80]);
        let mut m = AudioMixer::new(8_000);
        m.play(c, Vec3::ZERO);
        let mut out = vec![0.0; 160];
        m.mix(&mut out, 8_000);
        for f in out.as_chunks::<2>().0 {
            assert!((f[0] - 0.5 * FRAC_PI_4.cos()).abs() < 1e-5);
            assert!((f[1] - 0.5 * FRAC_PI_4.sin()).abs() < 1e-5);
        }
    }

    #[test]
    fn source_on_the_right_pans_right_and_distance_attenuates() {
        let c = clip(1, &[0.5; 800]);
        let mut m = AudioMixer::new(8_000);
        let near = m.play(c.clone(), Vec3::new(0.5, 0.0, 0.0));
        let mut out = vec![0.0; 2];
        m.mix(&mut out, 8_000);
        assert!(out[1] > out[0], "右侧声源应右声道更响：{out:?}");
        let near_gain = out[1];

        m.stop(near);
        let far = m.play(c, Vec3::new(8.0, 0.0, 0.0));
        let mut out2 = vec![0.0; 2];
        m.mix(&mut out2, 8_000);
        assert!(
            out2[1] < near_gain,
            "远处应更轻：{} vs {}",
            out2[1],
            near_gain
        );
        m.stop(far);
    }

    #[test]
    fn looping_wraps_and_non_looping_stops() {
        let c = clip(1, &[1.0, -1.0]);
        let mut m = AudioMixer::new(8_000);
        m.play(c.clone(), Vec3::ZERO);
        let mut out = vec![0.0; 20];
        m.mix(&mut out, 8_000);
        assert!(m.active_sources() == 0, "非循环源播完应停止");
        assert!(out[10..].iter().all(|v| v.abs() < 1e-6));

        let id = m.play(c, Vec3::ZERO);
        m.source_mut(id).unwrap().looping = true;
        let mut out = vec![0.0; 20];
        m.mix(&mut out, 8_000);
        assert!(m.active_sources() == 1);
        assert!(out[10..].iter().any(|v| v.abs() > 0.1), "循环源应继续出声");
    }

    #[test]
    fn pitch_changes_playhead_speed_and_listener_setter_normalizes() {
        let c = clip(1, &[0.5; 100]);
        let mut m = AudioMixer::new(8_000);
        let id = m.play(c, Vec3::ZERO);
        m.source_mut(id).unwrap().pitch = 2.0;
        m.set_listener(AudioListener {
            forward: Vec3::Z * 3.0,
            up: Vec3::ZERO,
            ..AudioListener::default()
        });
        assert!((m.listener().forward.length() - 1.0).abs() < 1e-5);
        assert_eq!(m.listener().up, Vec3::Y);
        let mut out = vec![0.0; 10];
        m.mix(&mut out, 8_000);
        assert!(m.source(id).unwrap().cursor > 5.0);
    }

    #[test]
    fn master_volume_is_clamped_and_output_has_no_nan() {
        let c = clip(2, &[0.8, -0.8, 0.2, 0.2]);
        let mut m = AudioMixer::new(8_000);
        m.set_master_volume(10.0);
        m.play(c, Vec3::new(1.0, 2.0, 3.0));
        let out = m.mix_seconds(0.01);
        assert!(out.iter().all(|v| v.is_finite()));
        assert!(out.iter().all(|v| v.abs() <= 1.0));
    }
}
