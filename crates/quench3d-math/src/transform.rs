//! `Transform` —— 位置 / 旋转 / 缩放（TRS）三元组。
//!
//! 引擎中场景节点与物理刚体都用它表达位姿，可以直接生成渲染矩阵，
//! 也可以与父节点级联形成层级变换。

use core::ops::Mul;

use crate::{Mat4, Quat, Vec3};

/// 位置、旋转、缩放。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    /// 位置。
    pub position: Vec3,
    /// 旋转。
    pub rotation: Quat,
    /// 缩放。
    pub scale: Vec3,
}

impl Default for Transform {
    #[inline]
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    /// 单位变换。
    pub const IDENTITY: Self = Self {
        position: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    /// 构造。
    #[inline]
    pub const fn new(position: Vec3, rotation: Quat, scale: Vec3) -> Self {
        Self {
            position,
            rotation,
            scale,
        }
    }

    /// 仅平移。
    #[inline]
    pub const fn from_translation(t: Vec3) -> Self {
        Self::new(t, Quat::IDENTITY, Vec3::ONE)
    }

    /// 仅旋转。
    #[inline]
    pub const fn from_rotation(r: Quat) -> Self {
        Self::new(Vec3::ZERO, r, Vec3::ONE)
    }

    /// 仅缩放。
    #[inline]
    pub const fn from_scale(s: Vec3) -> Self {
        Self::new(Vec3::ZERO, Quat::IDENTITY, s)
    }

    /// 完整构造。
    #[inline]
    pub const fn from_trs(t: Vec3, r: Quat, s: Vec3) -> Self {
        Self::new(t, r, s)
    }

    /// 由矩阵分解构造。
    #[inline]
    pub fn from_matrix(m: &Mat4) -> Self {
        let (scale, rotation, position) = m.to_scale_rotation_translation();
        Self::new(position, rotation, scale)
    }

    /// 生成模型矩阵 `T * R * S`。
    #[inline]
    pub fn matrix(&self) -> Mat4 {
        Mat4::from_rotation_translation_scale(self.position, self.rotation, self.scale)
    }

    /// 生成法线矩阵（逆转置的 3x3 部分，忽略平移）。
    #[inline]
    pub fn normal_matrix(&self) -> Mat4 {
        let s = Vec3::new(
            if self.scale.x.abs() > 1e-8 {
                1.0 / self.scale.x
            } else {
                0.0
            },
            if self.scale.y.abs() > 1e-8 {
                1.0 / self.scale.y
            } else {
                0.0
            },
            if self.scale.z.abs() > 1e-8 {
                1.0 / self.scale.z
            } else {
                0.0
            },
        );
        Mat4::from_quat(self.rotation) * Mat4::from_scale(s)
    }

    /// 逆变换矩阵。
    #[inline]
    pub fn inverse_matrix(&self) -> Mat4 {
        self.matrix().inverse()
    }

    /// 把点从本地空间变换到父空间。
    #[inline]
    pub fn transform_point(&self, p: Vec3) -> Vec3 {
        self.position + self.rotation.mul_vec3(p * self.scale)
    }

    /// 把方向从本地空间变换到父空间（忽略平移）。
    #[inline]
    pub fn transform_vector(&self, v: Vec3) -> Vec3 {
        self.rotation.mul_vec3(v * self.scale)
    }

    /// 把点从父空间变换回本地空间。
    #[inline]
    pub fn inverse_transform_point(&self, p: Vec3) -> Vec3 {
        let d = p - self.position;
        let inv_s = Vec3::new(
            if self.scale.x.abs() > 1e-8 {
                1.0 / self.scale.x
            } else {
                0.0
            },
            if self.scale.y.abs() > 1e-8 {
                1.0 / self.scale.y
            } else {
                0.0
            },
            if self.scale.z.abs() > 1e-8 {
                1.0 / self.scale.z
            } else {
                0.0
            },
        );
        self.rotation.inverse().mul_vec3(d) * inv_s
    }

    /// 前方向（本地 -Z）。
    #[inline]
    pub fn forward(&self) -> Vec3 {
        self.rotation.forward()
    }

    /// 右方向（本地 +X）。
    #[inline]
    pub fn right(&self) -> Vec3 {
        self.rotation.right()
    }

    /// 上方向（本地 +Y）。
    #[inline]
    pub fn up(&self) -> Vec3 {
        self.rotation.up()
    }

    /// 以 `self` 为父、把 `child` 拼接到 `self` 空间。
    #[inline]
    pub fn mul_transform(&self, child: &Transform) -> Transform {
        Transform::new(
            self.transform_point(child.position),
            self.rotation * child.rotation,
            self.scale * child.scale,
        )
    }

    /// 面向目标点的朝向（`-Z` 指向目标）。
    #[inline]
    pub fn looking_at(position: Vec3, target: Vec3, up: Vec3) -> Self {
        Self::new(position, Quat::look_to_rh(target - position, up), Vec3::ONE)
    }

    /// 插值（旋转用 slerp）。
    pub fn lerp(&self, o: &Transform, t: f32) -> Self {
        Self::new(
            self.position.lerp(o.position, t),
            self.rotation.slerp(o.rotation, t),
            self.scale.lerp(o.scale, t),
        )
    }

    /// 平移叠加。
    #[inline]
    pub fn translated(&self, delta: Vec3) -> Self {
        Self::new(self.position + delta, self.rotation, self.scale)
    }

    /// 绕世界轴旋转。
    #[inline]
    pub fn rotated_world(&self, axis: Vec3, angle: f32) -> Self {
        Self::new(
            self.position,
            Quat::from_axis_angle(axis, angle) * self.rotation,
            self.scale,
        )
    }

    /// 绕本地轴旋转。
    #[inline]
    pub fn rotated_local(&self, axis: Vec3, angle: f32) -> Self {
        Self::new(
            self.position,
            self.rotation * Quat::from_axis_angle(axis, angle),
            self.scale,
        )
    }

    /// 均匀性检查：三个轴缩放是否近似相等。
    #[inline]
    pub fn is_uniform_scale(&self) -> bool {
        (self.scale.x - self.scale.y).abs() < 1e-5 && (self.scale.y - self.scale.z).abs() < 1e-5
    }
}

impl Mul for Transform {
    type Output = Transform;
    #[inline]
    fn mul(self, child: Transform) -> Transform {
        self.mul_transform(&child)
    }
}

impl From<Transform> for Mat4 {
    #[inline]
    fn from(t: Transform) -> Self {
        t.matrix()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vec3;
    use core::f32::consts::FRAC_PI_2;

    #[test]
    fn point_transform_matches_matrix() {
        let t = Transform::from_trs(
            vec3(1.0, 2.0, 3.0),
            Quat::from_rotation_y(0.6),
            vec3(2.0, 1.0, 0.5),
        );
        let p = vec3(0.5, -1.0, 2.0);
        let a = t.transform_point(p);
        let b = t.matrix().transform_point3(p);
        assert!((a - b).length() < 1e-4, "{a:?} vs {b:?}");
    }

    #[test]
    fn inverse_roundtrip() {
        let t = Transform::from_trs(
            vec3(-4.0, 0.5, 2.0),
            Quat::from_axis_angle(vec3(1.0, 1.0, 0.0).normalize_or_zero(), 1.2),
            vec3(1.0, 3.0, 0.25),
        );
        let p = vec3(0.3, 0.7, -1.1);
        let back = t.inverse_transform_point(t.transform_point(p));
        assert!((back - p).length() < 1e-4, "{back:?} != {p:?}");
    }

    #[test]
    fn compose_transforms() {
        let parent = Transform::from_trs(Vec3::ZERO, Quat::from_rotation_y(FRAC_PI_2), Vec3::ONE);
        let child = Transform::from_translation(vec3(0.0, 0.0, -1.0));
        let world = parent * child;
        // 父绕 Y 转 90°：本地 -Z 变成世界 -X 方向… 实际 (0,0,-1) 绕 Y 转 90° 得 (-1,0,0)
        assert!(
            (world.position - vec3(-1.0, 0.0, 0.0)).length() < 1e-5,
            "{:?}",
            world.position
        );
        assert!((world.forward() - vec3(-1.0, 0.0, 0.0)).length() < 1e-5);
    }

    #[test]
    fn looking_at_faces_target() {
        let t = Transform::looking_at(vec3(0.0, 0.0, 0.0), vec3(5.0, 0.0, 0.0), Vec3::Y);
        assert!((t.forward() - Vec3::X).length() < 1e-5, "{:?}", t.forward());
    }
}
