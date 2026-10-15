//! 射线与平面：拾取、射线检测（raycast）、地形与半空间碰撞的基础。

use crate::{Aabb, Vec3, EPSILON};

/// 射线：`origin + dir * t`，通常 `dir` 为单位向量。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray {
    /// 起点。
    pub origin: Vec3,
    /// 方向（不要求归一化，但射线检测通常假设已归一化）。
    pub dir: Vec3,
}

impl Ray {
    /// 构造。
    #[inline]
    pub const fn new(origin: Vec3, dir: Vec3) -> Self {
        Self { origin, dir }
    }

    /// 从两点构造（方向自动归一化）。
    #[inline]
    pub fn from_points(from: Vec3, to: Vec3) -> Self {
        Self::new(from, (to - from).normalize_or_zero())
    }

    /// 射线上的点。
    #[inline]
    pub fn point_at(&self, t: f32) -> Vec3 {
        self.origin + self.dir * t
    }

    /// 与 AABB 求交，返回最近命中距离。
    #[inline]
    pub fn intersect_aabb(&self, b: &Aabb) -> Option<f32> {
        b.intersect_ray(self.origin, self.dir)
    }

    /// 与球求交，返回最近命中距离。
    pub fn intersect_sphere(&self, center: Vec3, radius: f32) -> Option<f32> {
        let oc = self.origin - center;
        let a = self.dir.length_squared();
        if a < EPSILON {
            return None;
        }
        let b = oc.dot(self.dir);
        let c = oc.length_squared() - radius * radius;
        let disc = b * b - a * c;
        if disc < 0.0 {
            return None;
        }
        let sq = disc.sqrt();
        let t0 = (-b - sq) / a;
        let t1 = (-b + sq) / a;
        if t0 >= 0.0 {
            Some(t0)
        } else if t1 >= 0.0 {
            Some(t1)
        } else {
            None
        }
    }

    /// 与平面求交。
    pub fn intersect_plane(&self, plane: &Plane) -> Option<f32> {
        let denom = plane.normal.dot(self.dir);
        if denom.abs() < EPSILON {
            return None;
        }
        let t = -(plane.normal.dot(self.origin) + plane.d) / denom;
        if t >= 0.0 {
            Some(t)
        } else {
            None
        }
    }

    /// 与三角形求交（Möller–Trumbore，双向）。
    pub fn intersect_triangle(&self, a: Vec3, b: Vec3, c: Vec3) -> Option<f32> {
        let e1 = b - a;
        let e2 = c - a;
        let p = self.dir.cross(e2);
        let det = e1.dot(p);
        if det.abs() < EPSILON {
            return None;
        }
        let inv_det = 1.0 / det;
        let tvec = self.origin - a;
        let u = tvec.dot(p) * inv_det;
        if !(-EPSILON..=1.0 + EPSILON).contains(&u) {
            return None;
        }
        let q = tvec.cross(e1);
        let v = self.dir.dot(q) * inv_det;
        if v < -EPSILON || u + v > 1.0 + EPSILON {
            return None;
        }
        let t = e2.dot(q) * inv_det;
        if t > EPSILON {
            Some(t)
        } else {
            None
        }
    }

    /// 与凸多面体（以平面集合表示）求交，返回 `(进入距离, 离开距离)`。
    pub fn intersect_convex(&self, planes: &[Plane]) -> Option<(f32, f32)> {
        let mut t_min = f32::NEG_INFINITY;
        let mut t_max = f32::INFINITY;
        for p in planes {
            let denom = p.normal.dot(self.dir);
            let dist = p.normal.dot(self.origin) + p.d;
            if denom.abs() < EPSILON {
                if dist > 0.0 {
                    return None; // 与该面平行且在外部
                }
            } else {
                let t = -dist / denom;
                if denom < 0.0 {
                    t_min = t_min.max(t);
                } else {
                    t_max = t_max.min(t);
                }
            }
            if t_min > t_max {
                return None;
            }
        }
        Some((t_min, t_max))
    }
}

/// 平面：满足 `normal · p + d = 0`，`normal` 为单位向量时 `d` 是到原点的有符号距离。
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "bytemuck", derive(bytemuck::Pod, bytemuck::Zeroable))]
#[repr(C)]
pub struct Plane {
    /// 单位法线（指向正半空间）。
    pub normal: Vec3,
    /// 平面常数项。
    pub d: f32,
}

impl Plane {
    /// 构造。
    #[inline]
    pub const fn new(normal: Vec3, d: f32) -> Self {
        Self { normal, d }
    }

    /// 由点与法线构造。
    #[inline]
    pub fn from_point_normal(point: Vec3, normal: Vec3) -> Self {
        let n = normal.normalize_or_zero();
        Self::new(n, -n.dot(point))
    }

    /// 由三点构造（逆时针为正面）。
    #[inline]
    pub fn from_points(a: Vec3, b: Vec3, c: Vec3) -> Self {
        let n = (b - a).cross(c - a).normalize_or_zero();
        Self::from_point_normal(a, n)
    }

    /// 有符号距离：正值表示点在法线指向的一侧。
    #[inline]
    pub fn signed_distance(&self, p: Vec3) -> f32 {
        self.normal.dot(p) + self.d
    }

    /// 点到平面的投影。
    #[inline]
    pub fn project_point(&self, p: Vec3) -> Vec3 {
        p - self.normal * self.signed_distance(p)
    }

    /// 翻转朝向。
    #[inline]
    pub fn flipped(&self) -> Self {
        Self::new(-self.normal, -self.d)
    }

    /// 射线求交。
    #[inline]
    pub fn intersect_ray(&self, ray: &Ray) -> Option<f32> {
        ray.intersect_plane(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vec3;

    #[test]
    fn sphere_hit() {
        let r = Ray::new(vec3(0.0, 0.0, -5.0), Vec3::Z);
        assert_eq!(r.intersect_sphere(Vec3::ZERO, 1.0), Some(4.0));
        assert_eq!(r.intersect_sphere(vec3(10.0, 0.0, 0.0), 1.0), None);
        // 起点在球内 → 返回离开点。
        let inside = Ray::new(Vec3::ZERO, Vec3::Z);
        assert_eq!(inside.intersect_sphere(Vec3::ZERO, 1.0), Some(1.0));
    }

    #[test]
    fn triangle_hit() {
        let r = Ray::new(vec3(0.25, 0.25, -1.0), Vec3::Z);
        assert!(r
            .intersect_triangle(Vec3::ZERO, vec3(1.0, 0.0, 0.0), vec3(0.0, 1.0, 0.0))
            .is_some());
        let miss = Ray::new(vec3(2.0, 2.0, -1.0), Vec3::Z);
        assert!(miss
            .intersect_triangle(Vec3::ZERO, vec3(1.0, 0.0, 0.0), vec3(0.0, 1.0, 0.0))
            .is_none());
    }

    #[test]
    fn plane_distance_and_project() {
        // y = 2 平面
        let p = Plane::from_point_normal(vec3(0.0, 2.0, 0.0), Vec3::Y);
        assert!((p.signed_distance(vec3(0.0, 5.0, 0.0)) - 3.0).abs() < 1e-6);
        assert!((p.signed_distance(vec3(0.0, -1.0, 0.0)) + 3.0).abs() < 1e-6);
        assert_eq!(p.project_point(vec3(1.0, 5.0, 1.0)), vec3(1.0, 2.0, 1.0));
    }

    #[test]
    fn convex_slab() {
        // 单位立方体：6 个面
        let planes = [
            Plane::from_point_normal(vec3(1.0, 0.0, 0.0), Vec3::X),
            Plane::from_point_normal(vec3(-1.0, 0.0, 0.0), Vec3::NEG_X),
            Plane::from_point_normal(vec3(0.0, 1.0, 0.0), Vec3::Y),
            Plane::from_point_normal(vec3(0.0, -1.0, 0.0), Vec3::NEG_Y),
            Plane::from_point_normal(vec3(0.0, 0.0, 1.0), Vec3::Z),
            Plane::from_point_normal(vec3(0.0, 0.0, -1.0), Vec3::NEG_Z),
        ];
        let r = Ray::new(vec3(0.0, 0.0, -5.0), Vec3::Z);
        let (enter, exit) = r.intersect_convex(&planes).unwrap();
        assert!((enter - 4.0).abs() < 1e-5 && (exit - 6.0).abs() < 1e-5);
    }
}
