//! 刚体位姿的确定性录制/回放。
//!
//! 只记录**渲染需要的刚体位姿**（位置/旋转/缩放），不记录速度、流体粒子或关节。
//! 二进制格式带 magic、版本号与 FNV-1a 校验；解码是纯安全 Rust，截断/改字节/
//! 版本不符都会返回错误而不是 panic。回放默认按固定步长采样，位置线性插值、
//! 旋转 slerp；`apply` 只写位姿，不推进物理，也不清速度。

use crate::{BodyHandle, PhysicsWorld};
use quench3d_math::{Quat, Transform, Vec3};
use std::collections::BTreeMap;
use std::fmt;

/// 回放文件 magic。
pub const REPLAY_MAGIC: [u8; 4] = *b"Q3RP";
/// 当前回放格式版本。
pub const REPLAY_VERSION: u16 = 1;

const HEADER_BYTES: usize = 16;
const CHECKSUM_BYTES: usize = 4;
const POSE_BYTES: usize = 48;
const MAX_FRAMES: u32 = 1_000_000;
const MAX_BODIES: u32 = 1_000_000;

/// 回放中的一个刚体位姿。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReplayPose {
    /// 刚体句柄（index + generation）。
    pub handle: BodyHandle,
    /// 世界位姿。
    pub transform: Transform,
}

/// 回放的一帧。
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayFrame {
    /// 录制时的物理步数。
    pub step: u64,
    /// 这一帧的刚体位姿，按 handle 升序。
    pub poses: Vec<ReplayPose>,
}

/// 一段刚体位姿回放。
#[derive(Clone, Debug, PartialEq)]
pub struct ReplayClip {
    /// 采样用的固定步长（秒）。
    pub fixed_dt: f32,
    /// 按录制顺序保存的帧。
    pub frames: Vec<ReplayFrame>,
}

/// 回放编解码错误。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplayError {
    /// magic 不是 `Q3RP`。
    BadMagic,
    /// 格式版本不支持。
    UnsupportedVersion(u16),
    /// 字节流被截断。
    Truncated,
    /// 尾部校验和不匹配。
    BadChecksum,
    /// 帧数/刚体数超过上限。
    TooLarge,
    /// 出现 NaN/Inf 或非法固定步长。
    NonFinite,
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => write!(f, "不是 Quench3D 回放文件（magic 不是 Q3RP）"),
            Self::UnsupportedVersion(v) => {
                write!(f, "不支持的回放版本 {v}（当前 {REPLAY_VERSION}）")
            }
            Self::Truncated => write!(f, "回放字节流被截断"),
            Self::BadChecksum => write!(f, "回放校验和不匹配（文件损坏）"),
            Self::TooLarge => write!(f, "回放帧数/刚体数超过上限"),
            Self::NonFinite => write!(f, "回放里有 NaN/Inf 或非法固定步长"),
        }
    }
}

impl std::error::Error for ReplayError {}

impl ReplayClip {
    /// 新建空回放。非法/非正步长退回 1/60 s。
    pub fn new(fixed_dt: f32) -> Self {
        let fixed_dt = if fixed_dt.is_finite() && fixed_dt > 0.0 {
            fixed_dt
        } else {
            1.0 / 60.0
        };
        Self {
            fixed_dt,
            frames: Vec::new(),
        }
    }

    /// 记录当前世界的所有刚体位姿（按槽位升序，确定）。
    pub fn record(&mut self, world: &PhysicsWorld) {
        let mut poses: Vec<ReplayPose> = world
            .bodies()
            .map(|(handle, body)| ReplayPose {
                handle,
                transform: body.transform(),
            })
            .collect();
        poses.sort_by_key(|p| (p.handle.index, p.handle.generation));
        self.frames.push(ReplayFrame {
            step: world.step_count,
            poses,
        });
    }

    /// 只读帧列表。
    pub fn frames(&self) -> &[ReplayFrame] {
        &self.frames
    }

    /// 清空帧，保留步长。
    pub fn clear(&mut self) {
        self.frames.clear();
    }

    /// 回放时长（秒）；少于两帧为 0。
    pub fn duration(&self) -> f32 {
        self.frames.len().saturating_sub(1) as f32 * self.fixed_dt
    }

    /// 按时间采样插值位姿。
    ///
    /// 时间超出末尾时停在最后一帧；空回放返回 `None`。只对同时出现在相邻帧的
    /// 句柄插值，消失的句柄按前一帧返回。
    pub fn sample(&self, time: f32) -> Option<Vec<ReplayPose>> {
        if self.frames.is_empty() {
            return None;
        }
        let t = if time.is_finite() {
            (time.max(0.0) / self.fixed_dt).max(0.0)
        } else {
            0.0
        };
        let index = t.floor() as usize;
        if index >= self.frames.len() {
            return Some(self.frames.last()?.poses.clone());
        }
        let lower = &self.frames[index];
        let frac = t - index as f32;
        if frac <= 0.0 || index + 1 >= self.frames.len() {
            return Some(lower.poses.clone());
        }
        let upper = &self.frames[index + 1];
        let map: BTreeMap<(u32, u32), ReplayPose> = upper
            .poses
            .iter()
            .map(|p| ((p.handle.index, p.handle.generation), *p))
            .collect();
        let mut out = Vec::with_capacity(lower.poses.len());
        for lo in &lower.poses {
            let key = (lo.handle.index, lo.handle.generation);
            let pose = if let Some(hi) = map.get(&key) {
                let a = lo.transform;
                let b = hi.transform;
                ReplayPose {
                    handle: lo.handle,
                    transform: Transform {
                        position: a.position.lerp(b.position, frac),
                        rotation: a.rotation.slerp(b.rotation, frac),
                        scale: a.scale.lerp(b.scale, frac),
                    },
                }
            } else {
                *lo
            };
            out.push(pose);
        }
        Some(out)
    }

    /// 把某个时刻的位姿写回已存在的刚体；返回真正写回的个数。
    ///
    /// 只写位姿，不写速度、不唤醒、不推进物理。播放时应暂停物理。
    pub fn apply(&self, world: &mut PhysicsWorld, time: f32) -> usize {
        let Some(poses) = self.sample(time) else {
            return 0;
        };
        let mut applied = 0;
        for pose in poses {
            if let Some(body) = world.body_mut(pose.handle) {
                body.transform = pose.transform;
                applied += 1;
            }
        }
        applied
    }

    /// 编码成带 magic/版本/校验和的二进制。
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            HEADER_BYTES
                + CHECKSUM_BYTES
                + self.frames.len() * 12
                + self
                    .frames
                    .iter()
                    .map(|f| f.poses.len() * POSE_BYTES)
                    .sum::<usize>(),
        );
        out.extend_from_slice(&REPLAY_MAGIC);
        out.extend_from_slice(&REPLAY_VERSION.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // flags，留给以后
        out.extend_from_slice(&self.fixed_dt.to_le_bytes());
        out.extend_from_slice(&(self.frames.len() as u32).to_le_bytes());
        for frame in &self.frames {
            out.extend_from_slice(&frame.step.to_le_bytes());
            out.extend_from_slice(&(frame.poses.len() as u32).to_le_bytes());
            for pose in &frame.poses {
                out.extend_from_slice(&pose.handle.index.to_le_bytes());
                out.extend_from_slice(&pose.handle.generation.to_le_bytes());
                push_vec3(&mut out, pose.transform.position);
                push_quat(&mut out, pose.transform.rotation);
                push_vec3(&mut out, pose.transform.scale);
            }
        }
        let checksum = fnv1a(&out);
        out.extend_from_slice(&checksum.to_le_bytes());
        out
    }

    /// 解码并校验。截断、magic/版本错误、校验和不匹配、非有限值都返回错误。
    pub fn decode(bytes: &[u8]) -> Result<Self, ReplayError> {
        if bytes.len() < HEADER_BYTES + CHECKSUM_BYTES {
            return Err(ReplayError::Truncated);
        }
        let (payload, tail) = bytes.split_at(bytes.len() - CHECKSUM_BYTES);
        let stored = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
        if fnv1a(payload) != stored {
            return Err(ReplayError::BadChecksum);
        }
        let mut r = Reader {
            bytes: payload,
            at: 0,
        };
        if r.take(4)? != REPLAY_MAGIC {
            return Err(ReplayError::BadMagic);
        }
        let version = r.u16()?;
        if version != REPLAY_VERSION {
            return Err(ReplayError::UnsupportedVersion(version));
        }
        let _flags = r.u16()?;
        let fixed_dt = r.f32()?;
        if !(fixed_dt.is_finite() && fixed_dt > 0.0) {
            return Err(ReplayError::NonFinite);
        }
        let frame_count = r.u32()?;
        if frame_count > MAX_FRAMES {
            return Err(ReplayError::TooLarge);
        }
        let mut frames = Vec::with_capacity(frame_count as usize);
        for _ in 0..frame_count {
            let step = r.u64()?;
            let body_count = r.u32()?;
            if body_count > MAX_BODIES {
                return Err(ReplayError::TooLarge);
            }
            let mut poses = Vec::with_capacity(body_count as usize);
            for _ in 0..body_count {
                let index = r.u32()?;
                let generation = r.u32()?;
                let position = r.vec3()?;
                let rotation = r.quat()?;
                let scale = r.vec3()?;
                if !position.is_finite() || !scale.is_finite() {
                    return Err(ReplayError::NonFinite);
                }
                poses.push(ReplayPose {
                    handle: BodyHandle::new(index, generation),
                    transform: Transform {
                        position,
                        rotation,
                        scale,
                    },
                });
            }
            frames.push(ReplayFrame { step, poses });
        }
        Ok(Self { fixed_dt, frames })
    }
}

fn push_vec3(out: &mut Vec<u8>, v: Vec3) {
    out.extend_from_slice(&v.x.to_le_bytes());
    out.extend_from_slice(&v.y.to_le_bytes());
    out.extend_from_slice(&v.z.to_le_bytes());
}

fn push_quat(out: &mut Vec<u8>, q: Quat) {
    out.extend_from_slice(&q.x.to_le_bytes());
    out.extend_from_slice(&q.y.to_le_bytes());
    out.extend_from_slice(&q.z.to_le_bytes());
    out.extend_from_slice(&q.w.to_le_bytes());
}

fn fnv1a(bytes: &[u8]) -> u32 {
    let mut hash = 0x811c9dc5u32;
    for b in bytes {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ReplayError> {
        let end = self.at.checked_add(n).ok_or(ReplayError::Truncated)?;
        let slice = self.bytes.get(self.at..end).ok_or(ReplayError::Truncated)?;
        self.at = end;
        Ok(slice)
    }
    fn u16(&mut self) -> Result<u16, ReplayError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }
    fn u32(&mut self) -> Result<u32, ReplayError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn u64(&mut self) -> Result<u64, ReplayError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
    fn f32(&mut self) -> Result<f32, ReplayError> {
        Ok(f32::from_bits(self.u32()?))
    }
    fn vec3(&mut self) -> Result<Vec3, ReplayError> {
        Ok(Vec3::new(self.f32()?, self.f32()?, self.f32()?))
    }
    fn quat(&mut self) -> Result<Quat, ReplayError> {
        let q = Quat::new(self.f32()?, self.f32()?, self.f32()?, self.f32()?);
        if !q.is_finite() || q.length_squared() < 1e-12 {
            return Err(ReplayError::NonFinite);
        }
        Ok(q.normalize_or_identity())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip_with_two_frames() -> (ReplayClip, BodyHandle, PhysicsWorld) {
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(0.1);
        let handle = world.add_builder(
            crate::BodyBuilder::dynamic(crate::Shape::Sphere { radius: 0.1 })
                .position(Vec3::new(0.0, 10.0, 0.0))
                .linear_velocity(Vec3::new(0.0, 10.0, 0.0))
                .gravity_scale(0.0),
        );
        let mut clip = ReplayClip::new(world.fixed_dt());
        clip.record(&world);
        world.step(0.1);
        clip.record(&world);
        (clip, handle, world)
    }

    #[test]
    fn encode_decode_preserves_frames_and_checksum() {
        let (clip, _, _) = clip_with_two_frames();
        let bytes = clip.encode();
        let back = ReplayClip::decode(&bytes).expect("解码");
        assert_eq!(back, clip);
        assert!((back.duration() - 0.1).abs() < 1e-6);
    }

    #[test]
    fn corrupt_truncated_and_future_versions_are_rejected() {
        let (clip, _, _) = clip_with_two_frames();
        let mut bytes = clip.encode();
        let last = bytes.len() - 5;
        bytes[last] ^= 0x40;
        assert_eq!(ReplayClip::decode(&bytes), Err(ReplayError::BadChecksum));
        assert_eq!(
            ReplayClip::decode(&bytes[..10]),
            Err(ReplayError::Truncated)
        );
        let mut future = clip.encode();
        future[4..6].copy_from_slice(&(REPLAY_VERSION + 1).to_le_bytes());
        // 版本在 payload 内，改了会先被校验和挡住；重新算校验和才能测版本分支。
        let checksum = fnv1a(&future[..future.len() - CHECKSUM_BYTES]);
        let n = future.len();
        future[n - CHECKSUM_BYTES..].copy_from_slice(&checksum.to_le_bytes());
        assert_eq!(
            ReplayClip::decode(&future),
            Err(ReplayError::UnsupportedVersion(REPLAY_VERSION + 1))
        );
    }

    #[test]
    fn empty_clip_encodes_and_decodes() {
        let clip = ReplayClip::new(0.0);
        assert_eq!(clip.fixed_dt, 1.0 / 60.0);
        assert!(clip.sample(0.0).is_none());
        let back = ReplayClip::decode(&clip.encode()).expect("空回放");
        assert!(back.frames.is_empty());
    }
}
