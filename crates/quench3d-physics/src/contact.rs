//! 接触点与接触流形。

use quench3d_math::*;

use crate::body::BodyHandle;
use crate::small_vec::SmallVec;

/// 单个接触点。`normal` 由 A 指向 B，`penetration` 为正表示互相嵌入的深度。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContactPoint {
    /// 世界空间接触位置（两形状重叠区的中点）。
    pub position: Vec3,
    /// 单位法线，由 A 指向 B。
    pub normal: Vec3,
    /// 穿透深度（正值）。
    pub penetration: f32,
    /// 接触特征 ID：由**两端局部坐标量化**得到的稳定标识（0 = 未知）。
    ///
    /// 为什么需要它：求解器的热启动缓存按"刚体对 + 接触点"匹配冲量，靠坐标距离匹配
    /// 在物体滑动/旋转时会把冲量沿用到**另一个**特征点上（或者干脆匹配不上），
    /// 表现为"慢慢滑的箱子忽软忽硬"。ID 由局部坐标量化而来，同一个几何特征
    /// 在连续帧里得到同一个 ID，与点顺序、是否新增/消失都无关。
    pub feature_id: u32,
}

impl Default for ContactPoint {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            normal: Vec3::Y,
            penetration: 0.0,
            feature_id: 0,
        }
    }
}

/// 接触特征 ID：两端**局部坐标**按 0.5 mm 量化后哈希（0 = 未知）。
///
/// 同一个几何特征在连续帧里落在同一格 → 得到同一个 ID，与接触点的顺序、
/// 是否新增/消失无关。宽相/窄相与求解器用**同一个**函数算它，所以缓存一定能对上。
///
/// 量化尺度是取舍：太大（1 cm）会让同一物体上相邻的特征点撞成同一个 ID；
/// 太小（1 µm）会让浮点噪声把同一个点算成两个 ID。0.5 mm 对 0.1~10 m 的场景都合适。
pub(crate) fn feature_id_from_local(local_a: Vec3, local_b: Vec3) -> u32 {
    const SCALE: f32 = 2048.0; // 1/2048 m ≈ 0.49 mm
    let mut h: u32 = 2166136261;
    for v in [
        local_a.x, local_a.y, local_a.z, local_b.x, local_b.y, local_b.z,
    ] {
        if !v.is_finite() {
            return 0;
        }
        let q = (v * SCALE) as i32 as u32;
        h = (h ^ q).wrapping_mul(16777619);
    }
    if h == 0 {
        1
    } else {
        h
    }
}

/// 一个形状对之间的接触流形，最多 [`ContactManifold::MAX_POINTS`] 个点。
///
/// 接触点用内联小向量存放（容量就是 [`ContactManifold::MAX_POINTS`]），
/// 因此**每个接触对都不需要堆分配**——这是物理热路径上最要紧的一处优化。
#[derive(Clone, Debug, Default)]
pub struct ContactManifold {
    /// 接触点列表（内联存放，容量 [`ContactManifold::MAX_POINTS`]）。
    pub points: SmallVec<ContactPoint, { ContactManifold::MAX_POINTS }>,
}

impl ContactManifold {
    /// 流形允许的最大接触点数（保证求解器工作量有界）。
    pub const MAX_POINTS: usize = 4;

    /// 空流形（不分配内存）。
    pub fn new() -> Self {
        Self {
            points: SmallVec::new(),
        }
    }

    /// 是否没有接触点。
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// 接触点数量。
    #[inline]
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// 追加一个接触点（非法数据会被丢弃）。
    pub fn push(&mut self, p: ContactPoint) {
        if !p.position.is_finite() || !p.normal.is_finite() || !p.penetration.is_finite() {
            return;
        }
        if self.points.len() < Self::MAX_POINTS {
            self.points.push(p);
        }
    }

    /// 按穿透深度降序裁剪到 `max` 个点（确定性排序：深度相同时保留下标小的）。
    pub fn truncate_by_depth(&mut self, max: usize) {
        if self.points.len() <= max {
            return;
        }
        self.points.sort_by(|a, b| {
            b.penetration
                .partial_cmp(&a.penetration)
                .unwrap_or(core::cmp::Ordering::Equal)
        });
        self.points.truncate(max);
    }

    /// 反转朝向（把 A↔B 的流形翻过来）。
    pub fn flipped(&self) -> Self {
        Self {
            points: self
                .points
                .iter()
                .map(|p| ContactPoint {
                    position: p.position,
                    normal: -p.normal,
                    penetration: p.penetration,
                    feature_id: 0,
                })
                .collect(),
        }
    }
}

/// 对外汇报的一对刚体接触（每个流形压缩成一个代表点）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContactPair {
    /// 第一具刚体。
    pub a: BodyHandle,
    /// 第二具刚体。
    pub b: BodyHandle,
    /// 法线（由 A 指向 B）。
    pub normal: Vec3,
    /// 代表接触位置。
    pub point: Vec3,
    /// 本步累计法向冲量大小。
    ///
    /// 传感器（`sensor: true`）参与的接触永远不产生冲量，这里恒为 0。
    pub impulse: f32,
    /// 这一对里是否有传感器。
    ///
    /// 传感器对**只做检测、不做求解**（不产生冲量、不唤醒睡眠体），
    /// 但仍出现在 [`crate::PhysicsWorld::contacts`] 与传感器事件里。
    pub sensor: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifold_truncation_is_deterministic() {
        let mut m = ContactManifold::new();
        for i in 0..8 {
            m.push(ContactPoint {
                position: vec3(i as f32, 0.0, 0.0),
                normal: Vec3::Y,
                penetration: i as f32,
                feature_id: 0,
            });
        }
        assert_eq!(m.len(), 4, "默认最多 4 个点");
        let mut m2 = ContactManifold::new();
        for i in 0..8 {
            m2.push(ContactPoint {
                position: vec3(i as f32, 0.0, 0.0),
                normal: Vec3::Y,
                penetration: (i % 3) as f32,
                feature_id: 0,
            });
        }
        m2.truncate_by_depth(3);
        assert_eq!(m2.len(), 3);
        let first = m2.points[0].position.x;
        let mut m3 = m2.clone();
        m3.truncate_by_depth(3);
        assert_eq!(m3.points[0].position.x, first);
    }

    #[test]
    fn flipped_normal() {
        let mut m = ContactManifold::new();
        m.push(ContactPoint {
            position: Vec3::ZERO,
            normal: Vec3::Y,
            penetration: 0.1,
            feature_id: 0,
        });
        assert_eq!(m.flipped().points[0].normal, -Vec3::Y);
    }
}
