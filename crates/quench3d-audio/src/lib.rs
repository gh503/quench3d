//! 纯 Rust 音频基础层：WAV 解码、3D 距离衰减/声像、可复现的离线混音。
//!
//! 这一层**不链接任何音频设备库**：`AudioMixer::mix` 只写调用方给的交错立体声
//! 缓冲，桌面/浏览器/CI 都能跑。真正的实时输出（cpal/WebAudio）是宿主适配层，
//! 不在这里假装已经完成。
//!
//! ```no_run
//! use quench3d_audio::{AudioClip, AudioMixer};
//! let bytes = std::fs::read("music.wav").unwrap();
//! let clip = AudioClip::from_wav(&bytes).unwrap();
//! let mut mixer = AudioMixer::new(48_000);
//! mixer.play(clip, quench3d_math::Vec3::new(1.0, 0.0, 0.0));
//! let mut out = vec![0.0f32; 48_000 * 2];
//! mixer.mix(&mut out, 48_000);
//! ```

#![forbid(unsafe_code)]

mod mixer;
mod wav;

pub use mixer::{AudioListener, AudioMixer, AudioSource, SourceId};
pub use wav::{AudioClip, AudioError};
