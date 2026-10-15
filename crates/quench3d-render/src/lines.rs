//! 调试线段绘制：坐标轴、包围盒、球体线框、接触点、射线、轨迹。
//!
//! 每帧 `clear()` 后重新添加，绘制在场景之上（带深度测试）。

use bytemuck::{Pod, Zeroable};
use quench3d_math::{Aabb, Mat4, Quat, Transform, Vec3, Vec4};

use crate::color;

/// 线段顶点。
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct LineVertex {
    /// 世界坐标。
    pub position: [f32; 3],
    /// 线性空间 RGBA。
    pub color: [f32; 4],
}

/// 线段收集器。
#[derive(Clone, Debug, Default)]
pub struct DebugLines {
    /// 顶点列表（每两个顶点一条线段）。
    pub vertices: Vec<LineVertex>,
    /// 是否启用。
    pub enabled: bool,
}

impl DebugLines {
    /// 新建（默认启用）。
    pub fn new() -> Self {
        Self {
            vertices: Vec::new(),
            enabled: true,
        }
    }

    /// 清空。
    #[inline]
    pub fn clear(&mut self) {
        self.vertices.clear();
    }

    /// 线段数量。
    #[inline]
    pub fn line_count(&self) -> usize {
        self.vertices.len() / 2
    }

    /// 是否为空。
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty()
    }

    /// 添加一条线段。
    pub fn line(&mut self, a: Vec3, b: Vec3, color: Vec4) {
        self.vertices.push(LineVertex {
            position: a.to_array(),
            color: color.to_array(),
        });
        self.vertices.push(LineVertex {
            position: b.to_array(),
            color: color.to_array(),
        });
    }

    /// 添加一条由两端点配色的渐变线。
    pub fn line_gradient(&mut self, a: Vec3, b: Vec3, ca: Vec4, cb: Vec4) {
        self.vertices.push(LineVertex {
            position: a.to_array(),
            color: ca.to_array(),
        });
        self.vertices.push(LineVertex {
            position: b.to_array(),
            color: cb.to_array(),
        });
    }

    /// 添加折线。
    pub fn polyline(&mut self, points: &[Vec3], color: Vec4) {
        for w in points.windows(2) {
            self.line(w[0], w[1], color);
        }
    }

    /// 添加闭合折线。
    pub fn closed_polyline(&mut self, points: &[Vec3], color: Vec4) {
        self.polyline(points, color);
        if points.len() > 1 {
            self.line(*points.last().unwrap(), points[0], color);
        }
    }

    /// 坐标轴十字（位置 + 长度）。
    pub fn axes(&mut self, origin: Vec3, length: f32) {
        let scale = length;
        self.line(
            origin,
            origin + Vec3::X * scale,
            color::rgba(0.9, 0.25, 0.25, 1.0),
        );
        self.line(
            origin,
            origin + Vec3::Y * scale,
            color::rgba(0.3, 0.9, 0.3, 1.0),
        );
        self.line(
            origin,
            origin + Vec3::Z * scale,
            color::rgba(0.3, 0.5, 0.95, 1.0),
        );
    }

    /// 世界坐标轴（在原点处画三条长轴）。
    pub fn world_axes(&mut self, length: f32, alpha: f32) {
        self.line_gradient(
            -Vec3::X * length,
            Vec3::X * length,
            color::rgba(0.9, 0.3, 0.3, alpha),
            color::rgba(0.9, 0.3, 0.3, alpha),
        );
        self.line_gradient(
            Vec3::ZERO,
            Vec3::Y * length,
            color::rgba(0.3, 0.9, 0.3, alpha),
            color::rgba(0.3, 0.9, 0.3, alpha),
        );
        self.line_gradient(
            -Vec3::Z * length,
            Vec3::Z * length,
            color::rgba(0.3, 0.5, 0.95, alpha),
            color::rgba(0.3, 0.5, 0.95, alpha),
        );
    }

    /// 线段（带变换）。
    pub fn line_transformed(&mut self, m: &Mat4, a: Vec3, b: Vec3, color: Vec4) {
        self.line(m.transform_point3(a), m.transform_point3(b), color);
    }

    /// 轴对齐包围盒线框。
    pub fn aabb(&mut self, bounds: &Aabb, color: Vec4) {
        let c = bounds.corners();
        // 底面 / 顶面
        for i in 0..4 {
            let j = (i + 1) % 4;
            self.line(c[i], c[j], color);
            self.line(c[i + 4], c[j + 4], color);
        }
        // 竖直边
        for i in 0..4 {
            self.line(c[i], c[i + 4], color);
        }
    }

    /// 变换后的盒子线框（OBB）。
    pub fn obb(&mut self, transform: &Transform, half_extents: Vec3, color: Vec4) {
        let h = half_extents;
        let corners = [
            Vec3::new(-h.x, -h.y, -h.z),
            Vec3::new(h.x, -h.y, -h.z),
            Vec3::new(h.x, -h.y, h.z),
            Vec3::new(-h.x, -h.y, h.z),
            Vec3::new(-h.x, h.y, -h.z),
            Vec3::new(h.x, h.y, -h.z),
            Vec3::new(h.x, h.y, h.z),
            Vec3::new(-h.x, h.y, h.z),
        ];
        let p: Vec<Vec3> = corners
            .iter()
            .map(|c| transform.transform_point(*c))
            .collect();
        for i in 0..4 {
            let j = (i + 1) % 4;
            self.line(p[i], p[j], color);
            self.line(p[i + 4], p[j + 4], color);
            self.line(p[i], p[i + 4], color);
        }
    }

    /// 球体线框（三个正交圆环）。
    pub fn sphere(&mut self, center: Vec3, radius: f32, color: Vec4, segments: u32) {
        let n = segments.max(6);
        for axis in 0..3 {
            let mut prev = Vec3::ZERO;
            for i in 0..=n {
                let a = i as f32 / n as f32 * core::f32::consts::TAU;
                let (s, c) = a.sin_cos();
                let p = match axis {
                    0 => Vec3::new(0.0, c, s),
                    1 => Vec3::new(c, 0.0, s),
                    _ => Vec3::new(c, s, 0.0),
                } * radius
                    + center;
                if i > 0 {
                    self.line(prev, p, color);
                }
                prev = p;
            }
        }
    }

    /// 圆环。
    pub fn circle(&mut self, center: Vec3, normal: Vec3, radius: f32, color: Vec4, segments: u32) {
        let n = normal.normalize_or_zero();
        let (t, b) = n.any_orthonormal_pair();
        let count = segments.max(6);
        let mut prev = center + t * radius;
        for i in 1..=count {
            let a = i as f32 / count as f32 * core::f32::consts::TAU;
            let (s, c) = a.sin_cos();
            let p = center + t * (radius * c) + b * (radius * s);
            self.line(prev, p, color);
            prev = p;
        }
    }

    /// 射线（从起点到终点）。
    pub fn ray(&mut self, origin: Vec3, direction: Vec3, length: f32, color: Vec4) {
        self.line(
            origin,
            origin + direction.normalize_or_zero() * length,
            color,
        );
    }

    /// 箭头（常用于法线/速度可视化）。
    pub fn arrow(&mut self, from: Vec3, to: Vec3, color: Vec4) {
        self.line(from, to, color);
        let dir = (to - from).normalize_or_zero();
        let (t, b) = dir.any_orthonormal_pair();
        let len = (to - from).length() * 0.15;
        let base = to - dir * len;
        for offset in [t * len * 0.5, -t * len * 0.5, b * len * 0.5, -b * len * 0.5] {
            self.line(to, base + offset, color);
        }
    }

    /// 接触点（小十字 + 法线箭头）。
    pub fn contact_point(&mut self, position: Vec3, normal: Vec3, size: f32, color: Vec4) {
        let n = normal.normalize_or_zero();
        let (t, b) = n.any_orthonormal_pair();
        self.line(position - t * size, position + t * size, color);
        self.line(position - b * size, position + b * size, color);
        self.arrow(position, position + n * size * 2.5, color);
    }

    /// 相机视锥线框。
    pub fn frustum(&mut self, inv_view_proj: &Mat4, color: Vec4) {
        let corners_ndc = [
            Vec3::new(-1.0, -1.0, 0.0),
            Vec3::new(1.0, -1.0, 0.0),
            Vec3::new(1.0, 1.0, 0.0),
            Vec3::new(-1.0, 1.0, 0.0),
            Vec3::new(-1.0, -1.0, 1.0),
            Vec3::new(1.0, -1.0, 1.0),
            Vec3::new(1.0, 1.0, 1.0),
            Vec3::new(-1.0, 1.0, 1.0),
        ];
        let p: Vec<Vec3> = corners_ndc
            .iter()
            .map(|c| inv_view_proj.project_point3(*c))
            .collect();
        for i in 0..4 {
            let j = (i + 1) % 4;
            self.line(p[i], p[j], color);
            self.line(p[i + 4], p[j + 4], color);
            self.line(p[i], p[i + 4], color);
        }
    }

    /// 网格地面（有限大小的方格）。
    pub fn grid(&mut self, size: f32, step: f32, color: Vec4, axis_color: Vec4) {
        let half = size * 0.5;
        let count = (size / step).round() as i32;
        for i in -count..=count {
            let v = i as f32 * step;
            if v.abs() < 1e-4 {
                continue;
            }
            self.line(
                Vec3::new(v, 0.0, -half),
                Vec3::new(v, 0.0, half),
                if i % 5 == 0 { axis_color } else { color },
            );
            self.line(
                Vec3::new(-half, 0.0, v),
                Vec3::new(half, 0.0, v),
                if i % 5 == 0 { axis_color } else { color },
            );
        }
    }

    /// 旋转指示器（三个圆环）。
    pub fn rotation_gizmo(&mut self, transform: &Transform, radius: f32) {
        self.circle(
            transform.position,
            transform.right(),
            radius,
            color::rgba(0.9, 0.3, 0.3, 0.9),
            32,
        );
        self.circle(
            transform.position,
            transform.up(),
            radius,
            color::rgba(0.3, 0.9, 0.3, 0.9),
            32,
        );
        self.circle(
            transform.position,
            transform.forward(),
            radius,
            color::rgba(0.3, 0.5, 0.95, 0.9),
            32,
        );
    }

    /// 用四元数旋转过的十字。
    pub fn oriented_cross(&mut self, position: Vec3, rotation: Quat, size: f32, color: Vec4) {
        self.line(
            position,
            position + rotation.mul_vec3(Vec3::X) * size,
            color,
        );
        self.line(
            position,
            position + rotation.mul_vec3(Vec3::Y) * size,
            color,
        );
        self.line(
            position,
            position + rotation.mul_vec3(Vec3::Z) * size,
            color,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quench3d_math::vec3;

    #[test]
    fn aabb_draws_twelve_edges() {
        let mut l = DebugLines::new();
        l.aabb(
            &Aabb::new(vec3(-1.0, -1.0, -1.0), vec3(1.0, 1.0, 1.0)),
            Vec4::ONE,
        );
        assert_eq!(l.line_count(), 12);
        assert_eq!(l.vertices.len(), 24);
    }

    #[test]
    fn sphere_ring_vertex_count() {
        let mut l = DebugLines::new();
        l.sphere(Vec3::ZERO, 1.0, Vec4::ONE, 8);
        // 3 个圆环，每个 8 段
        assert_eq!(l.line_count(), 24);
    }

    #[test]
    fn clear_resets() {
        let mut l = DebugLines::new();
        l.axes(Vec3::ZERO, 1.0);
        assert_eq!(l.line_count(), 3);
        l.clear();
        assert!(l.is_empty());
    }

    #[test]
    fn obb_follows_transform() {
        let mut l = DebugLines::new();
        let t = Transform::from_translation(vec3(10.0, 0.0, 0.0));
        l.obb(&t, Vec3::splat(1.0), Vec4::ONE);
        assert_eq!(l.line_count(), 12);
        let xs: Vec<f32> = l.vertices.iter().map(|v| v.position[0]).collect();
        assert!(xs.iter().all(|x| *x >= 9.0), "OBB 应跟随平移: {xs:?}");
    }
}
