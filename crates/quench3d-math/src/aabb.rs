//! 轴对齐包围盒（AABB）—— 宽相碰撞、视锥剔除与拾取的基础。

use crate::{Mat4, Vec3};

/// 轴对齐包围盒。`min` 的每个分量都不大于 `max`。
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "bytemuck", derive(bytemuck::Pod, bytemuck::Zeroable))]
#[repr(C)]
pub struct Aabb {
    /// 最小角。
    pub min: Vec3,
    /// 最大角。
    pub max: Vec3,
}

impl Aabb {
    /// 空包围盒（参与 `union` 时不影响结果）。
    pub const EMPTY: Self = Self {
        min: Vec3::INFINITY,
        max: Vec3::NEG_INFINITY,
    };

    /// 由最小/最大角构造。
    #[inline]
    pub const fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    /// 由中心与半尺寸构造。
    #[inline]
    pub fn from_center_half_extents(center: Vec3, half: Vec3) -> Self {
        Self::new(center - half, center + half)
    }

    /// 由点集构造。
    pub fn from_points(points: &[Vec3]) -> Self {
        let mut b = Self::EMPTY;
        for p in points {
            b.expand_point(*p);
        }
        b
    }

    /// 是否为有效（非空）包围盒。
    #[inline]
    pub fn is_valid(&self) -> bool {
        self.min.x <= self.max.x && self.min.y <= self.max.y && self.min.z <= self.max.z
    }

    /// 中心。
    #[inline]
    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    /// 半尺寸。
    #[inline]
    pub fn half_extents(&self) -> Vec3 {
        (self.max - self.min) * 0.5
    }

    /// 全尺寸。
    #[inline]
    pub fn size(&self) -> Vec3 {
        self.max - self.min
    }

    /// 体积。
    #[inline]
    pub fn volume(&self) -> f32 {
        let s = self.size();
        s.x * s.y * s.z
    }

    /// 表面积（用于 SAH 与BVH 代价估计）。
    #[inline]
    pub fn surface_area(&self) -> f32 {
        let s = self.size();
        2.0 * (s.x * s.y + s.y * s.z + s.z * s.x)
    }

    /// 把点纳入包围盒。
    #[inline]
    pub fn expand_point(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    /// 均匀外扩。
    #[inline]
    pub fn grown(&self, margin: f32) -> Self {
        Self::new(
            self.min - Vec3::splat(margin),
            self.max + Vec3::splat(margin),
        )
    }

    /// 是否与另一个包围盒相交（含相切）。
    #[inline]
    pub fn intersects(&self, o: &Self) -> bool {
        self.min.x <= o.max.x
            && self.max.x >= o.min.x
            && self.min.y <= o.max.y
            && self.max.y >= o.min.y
            && self.min.z <= o.max.z
            && self.max.z >= o.min.z
    }

    /// 是否包含点。
    #[inline]
    pub fn contains_point(&self, p: Vec3) -> bool {
        p.x >= self.min.x
            && p.x <= self.max.x
            && p.y >= self.min.y
            && p.y <= self.max.y
            && p.z >= self.min.z
            && p.z <= self.max.z
    }

    /// 合并两个包围盒。
    #[inline]
    pub fn union(&self, o: &Self) -> Self {
        Self::new(self.min.min(o.min), self.max.max(o.max))
    }

    /// 把另一个包围盒并入自身。
    #[inline]
    pub fn merge(&mut self, o: &Self) {
        *self = self.union(o);
    }

    /// 包围盒内离 `p` 最近的点。
    #[inline]
    pub fn closest_point(&self, p: Vec3) -> Vec3 {
        p.clamp(self.min, self.max)
    }

    /// 到点的最近距离平方。
    #[inline]
    pub fn distance_squared_to_point(&self, p: Vec3) -> f32 {
        (p - self.closest_point(p)).length_squared()
    }

    /// 最长的轴索引（0/1/2）。
    #[inline]
    pub fn longest_axis(&self) -> usize {
        let s = self.size();
        if s.x > s.y && s.x > s.z {
            0
        } else if s.y > s.z {
            1
        } else {
            2
        }
    }

    /// 八个角点。
    pub fn corners(&self) -> [Vec3; 8] {
        [
            Vec3::new(self.min.x, self.min.y, self.min.z),
            Vec3::new(self.max.x, self.min.y, self.min.z),
            Vec3::new(self.min.x, self.max.y, self.min.z),
            Vec3::new(self.max.x, self.max.y, self.min.z),
            Vec3::new(self.min.x, self.min.y, self.max.z),
            Vec3::new(self.max.x, self.min.y, self.max.z),
            Vec3::new(self.min.x, self.max.y, self.max.z),
            Vec3::new(self.max.x, self.max.y, self.max.z),
        ]
    }

    /// 用变换矩阵变换后重新求包围盒（变换 8 个角点，结果仍然轴对齐）。
    pub fn transformed(&self, m: &Mat4) -> Self {
        let mut out = Self::EMPTY;
        for c in self.corners() {
            out.expand_point(m.transform_point3(c));
        }
        out
    }

    /// 沿变换后的轴做保守扩展（比 `transformed` 便宜，适合动态物体代理）。
    pub fn transformed_conservative(&self, m: &Mat4) -> Self {
        let c = m.transform_point3(self.center());
        let e = self.half_extents();
        let t = m.to_mat3();
        let new_half = Vec3::new(
            t.cols[0].x.abs() * e.x + t.cols[1].x.abs() * e.y + t.cols[2].x.abs() * e.z,
            t.cols[0].y.abs() * e.x + t.cols[1].y.abs() * e.y + t.cols[2].y.abs() * e.z,
            t.cols[0].z.abs() * e.x + t.cols[1].z.abs() * e.y + t.cols[2].z.abs() * e.z,
        );
        Self::from_center_half_extents(c, new_half)
    }

    /// 与射线求交（返回命中距离）；无命中返回 `None`。
    pub fn intersect_ray(&self, origin: Vec3, dir: Vec3) -> Option<f32> {
        let inv = Vec3::new(
            1.0 / if dir.x == 0.0 { 1e-20 } else { dir.x },
            1.0 / if dir.y == 0.0 { 1e-20 } else { dir.y },
            1.0 / if dir.z == 0.0 { 1e-20 } else { dir.z },
        );
        let t0 = (self.min - origin) * inv;
        let t1 = (self.max - origin) * inv;
        let tmin = t0.min(t1).max_element();
        let tmax = t0.max(t1).min_element();
        if tmax >= tmin.max(0.0) {
            Some(if tmin >= 0.0 { tmin } else { 0.0 })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vec3;

    #[test]
    fn basics() {
        let b = Aabb::new(vec3(-1.0, -2.0, -3.0), vec3(1.0, 2.0, 3.0));
        assert_eq!(b.center(), Vec3::ZERO);
        assert_eq!(b.size(), vec3(2.0, 4.0, 6.0));
        assert_eq!(b.longest_axis(), 2);
        assert!(b.contains_point(vec3(0.5, 0.5, 0.5)));
        assert!(!b.contains_point(vec3(2.0, 0.0, 0.0)));
        assert_eq!(b.closest_point(vec3(5.0, 0.0, 0.0)), vec3(1.0, 0.0, 0.0));
    }

    #[test]
    fn ray_hits() {
        let b = Aabb::new(vec3(-1.0, -1.0, -1.0), vec3(1.0, 1.0, 1.0));
        assert_eq!(b.intersect_ray(vec3(0.0, 0.0, -5.0), Vec3::Z), Some(4.0));
        assert_eq!(b.intersect_ray(vec3(0.0, 0.0, -5.0), Vec3::NEG_Z), None);
        assert_eq!(b.intersect_ray(vec3(0.0, 0.0, 0.0), Vec3::X), Some(0.0));
    }

    #[test]
    fn union_and_empty() {
        let mut b = Aabb::EMPTY;
        assert!(!b.is_valid());
        b.expand_point(Vec3::ZERO);
        b.expand_point(vec3(2.0, 2.0, 2.0));
        assert!(b.is_valid());
        assert_eq!(b.max, Vec3::splat(2.0));
    }

    #[test]
    fn transformed_box_matches_corners() {
        let b = Aabb::new(vec3(-1.0, -1.0, -1.0), vec3(1.0, 1.0, 1.0));
        let m = Mat4::from_rotation_y(0.5) * Mat4::from_translation(vec3(3.0, 0.0, 0.0));
        let t = b.transformed(&m);
        let c = b.transformed_conservative(&m);
        // 保守包围盒应包含精确包围盒。
        assert!(c.min.x <= t.min.x + 1e-4 && c.max.x >= t.max.x - 1e-4);
        assert!(c.min.y <= t.min.y + 1e-4 && c.max.y >= t.max.y - 1e-4);
        assert!(c.min.z <= t.min.z + 1e-4 && c.max.z >= t.max.z - 1e-4);
    }
}
