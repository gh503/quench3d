//! 四元数：旋转的紧凑表示，用于刚体姿态、相机朝向与动画插值。

use core::ops::{Mul, MulAssign};

use crate::{approx_eq, Mat3, Mat4, Vec3, EPSILON};

/// 单位四元数 `(x, y, z, w)`，其中 `w` 为实部。
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "bytemuck", derive(bytemuck::Pod, bytemuck::Zeroable))]
#[repr(C)]
pub struct Quat {
    /// 虚部 X。
    pub x: f32,
    /// 虚部 Y。
    pub y: f32,
    /// 虚部 Z。
    pub z: f32,
    /// 实部 W。
    pub w: f32,
}

impl Default for Quat {
    #[inline]
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Quat {
    /// 单位四元数（无旋转）。
    pub const IDENTITY: Self = Self {
        x: 0.0,
        y: 0.0,
        z: 0.0,
        w: 1.0,
    };

    /// 构造。
    #[inline]
    pub const fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self { x, y, z, w }
    }

    /// 由实部与虚部构造。
    #[inline]
    pub const fn from_xyzw(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self::new(x, y, z, w)
    }

    /// 绕单位轴 `axis` 旋转 `angle` 弧度。
    #[inline]
    pub fn from_axis_angle(axis: Vec3, angle: f32) -> Self {
        let axis = axis.normalize_or_zero();
        let (s, c) = (angle * 0.5).sin_cos();
        Self::new(axis.x * s, axis.y * s, axis.z * s, c)
    }

    /// 绕 X 轴旋转。
    #[inline]
    pub fn from_rotation_x(angle: f32) -> Self {
        let (s, c) = (angle * 0.5).sin_cos();
        Self::new(s, 0.0, 0.0, c)
    }

    /// 绕 Y 轴旋转。
    #[inline]
    pub fn from_rotation_y(angle: f32) -> Self {
        let (s, c) = (angle * 0.5).sin_cos();
        Self::new(0.0, s, 0.0, c)
    }

    /// 绕 Z 轴旋转。
    #[inline]
    pub fn from_rotation_z(angle: f32) -> Self {
        let (s, c) = (angle * 0.5).sin_cos();
        Self::new(0.0, 0.0, s, c)
    }

    /// 欧拉角（弧度），按 `Y * X * Z` 顺序组合（游戏中最常用的 yaw-pitch-roll）。
    #[inline]
    pub fn from_euler_yxz(yaw: f32, pitch: f32, roll: f32) -> Self {
        Self::from_rotation_y(yaw) * Self::from_rotation_x(pitch) * Self::from_rotation_z(roll)
    }

    /// 欧拉角（弧度），按 `X * Y * Z` 顺序组合。
    #[inline]
    pub fn from_euler_xyz(x: f32, y: f32, z: f32) -> Self {
        Self::from_rotation_x(x) * Self::from_rotation_y(y) * Self::from_rotation_z(z)
    }

    /// 从旋转矩阵构造（Shepperd 分支法，数值稳定）。
    pub fn from_mat3(m: Mat3) -> Self {
        let (m00, m01, m02) = (m.cols[0].x, m.cols[1].x, m.cols[2].x);
        let (m10, m11, m12) = (m.cols[0].y, m.cols[1].y, m.cols[2].y);
        let (m20, m21, m22) = (m.cols[0].z, m.cols[1].z, m.cols[2].z);

        let trace = m00 + m11 + m22;
        if trace > 0.0 {
            let s = (trace + 1.0).sqrt() * 2.0;
            Self::new((m21 - m12) / s, (m02 - m20) / s, (m10 - m01) / s, 0.25 * s)
        } else if m00 > m11 && m00 > m22 {
            let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
            Self::new(0.25 * s, (m01 + m10) / s, (m02 + m20) / s, (m21 - m12) / s)
        } else if m11 > m22 {
            let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
            Self::new((m01 + m10) / s, 0.25 * s, (m12 + m21) / s, (m02 - m20) / s)
        } else {
            let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
            Self::new((m02 + m20) / s, (m12 + m21) / s, 0.25 * s, (m10 - m01) / s)
        }
        .normalize_or_identity()
    }

    /// 从 4x4 矩阵的旋转部分构造。
    #[inline]
    pub fn from_mat4(m: Mat4) -> Self {
        Self::from_mat3(m.to_mat3())
    }

    /// 把向量 `from` 旋转到向量 `to` 的最短旋转。
    pub fn from_rotation_arc(from: Vec3, to: Vec3) -> Self {
        let f = from.normalize_or_zero();
        let t = to.normalize_or_zero();
        let d = f.dot(t).clamp(-1.0, 1.0);
        if d > 1.0 - EPSILON {
            return Self::IDENTITY;
        }
        if d < -1.0 + EPSILON {
            // 反向：找任意正交轴旋转 180°。
            let (a, _) = f.any_orthonormal_pair();
            return Self::from_axis_angle(a, core::f32::consts::PI);
        }
        let c = f.cross(t);
        // 标准最短弧：q = normalize([cross, 1 + dot])。
        // 不能把 w 写成 sqrt(1 + dot)：那只在 90° 时碰巧正确，任意夹角都会转过头。
        Self::new(c.x, c.y, c.z, 1.0 + d).normalize_or_identity()
    }

    /// 从观察方向与上方向构造朝向。
    pub fn look_to_rh(dir: Vec3, up: Vec3) -> Self {
        let f = dir.normalize_or_zero();
        let mut up_n = up.normalize_or_zero();
        if f.cross(up_n).length_squared() < 1e-8 {
            up_n = if f.y.abs() > 0.9 { Vec3::Z } else { Vec3::Y };
        }
        let s = f.cross(up_n).normalize_or_zero();
        let u = s.cross(f);
        // 标准右手系基：X = right, Y = up, Z = -forward（相机看向 -Z）
        Self::from_mat3(Mat3::from_cols(s, u, -f))
    }

    /// 点积。
    #[inline]
    pub fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z + self.w * o.w
    }

    /// 长度平方。
    #[inline]
    pub fn length_squared(self) -> f32 {
        self.dot(self)
    }

    /// 长度。
    #[inline]
    pub fn length(self) -> f32 {
        self.length_squared().sqrt()
    }

    /// 归一化；退化时返回单位四元数。
    #[inline]
    pub fn normalize_or_identity(self) -> Self {
        let l = self.length();
        if l > EPSILON && l.is_finite() {
            Self::new(self.x / l, self.y / l, self.z / l, self.w / l)
        } else {
            Self::IDENTITY
        }
    }

    /// 归一化（原地）。
    #[inline]
    pub fn normalize(&mut self) {
        *self = self.normalize_or_identity();
    }

    /// 共轭（单位四元数下等价于逆）。
    #[inline]
    pub fn conjugate(self) -> Self {
        Self::new(-self.x, -self.y, -self.z, self.w)
    }

    /// 逆。
    #[inline]
    pub fn inverse(self) -> Self {
        let l2 = self.length_squared();
        if l2 > EPSILON {
            let c = self.conjugate();
            Self::new(c.x / l2, c.y / l2, c.z / l2, c.w / l2)
        } else {
            Self::IDENTITY
        }
    }

    /// 旋转一个向量（`q * v * q⁻¹` 的优化形式）。
    #[inline]
    pub fn mul_vec3(self, v: Vec3) -> Vec3 {
        let q = Vec3::new(self.x, self.y, self.z);
        let t = q.cross(v) * 2.0;
        v + t * self.w + q.cross(t)
    }

    /// 变换为 3x3 旋转矩阵。
    #[inline]
    pub fn to_mat3(self) -> Mat3 {
        Mat3::from_quat(self)
    }

    /// 变换为 4x4 旋转矩阵。
    #[inline]
    pub fn to_mat4(self) -> Mat4 {
        Mat4::from_quat(self)
    }

    /// 球面线性插值。
    pub fn slerp(self, mut o: Self, t: f32) -> Self {
        let mut cos = self.dot(o);
        // 取最短路径。
        if cos < 0.0 {
            o = Self::new(-o.x, -o.y, -o.z, -o.w);
            cos = -cos;
        }
        if cos > 1.0 - 1e-5 {
            // 夹角极小，退化为线性插值以避免除零。
            return Self::new(
                self.x + (o.x - self.x) * t,
                self.y + (o.y - self.y) * t,
                self.z + (o.z - self.z) * t,
                self.w + (o.w - self.w) * t,
            )
            .normalize_or_identity();
        }
        let theta = cos.clamp(-1.0, 1.0).acos();
        let sin_theta = theta.sin();
        let a = ((1.0 - t) * theta).sin() / sin_theta;
        let b = (t * theta).sin() / sin_theta;
        Self::new(
            self.x * a + o.x * b,
            self.y * a + o.y * b,
            self.z * a + o.z * b,
            self.w * a + o.w * b,
        )
        .normalize_or_identity()
    }

    /// 归一化线性插值（更快，长序列插值时误差更小）。
    #[inline]
    pub fn nlerp(self, o: Self, t: f32) -> Self {
        Self::new(
            self.x + (o.x - self.x) * t,
            self.y + (o.y - self.y) * t,
            self.z + (o.z - self.z) * t,
            self.w + (o.w - self.w) * t,
        )
        .normalize_or_identity()
    }

    /// 两旋转之间的夹角（弧度）。
    #[inline]
    pub fn angle_between(self, o: Self) -> f32 {
        2.0 * self.dot(o).abs().clamp(-1.0, 1.0).acos()
    }

    /// 转为 `(轴, 角度)`。
    pub fn to_axis_angle(self) -> (Vec3, f32) {
        let q = self.normalize_or_identity();
        let angle = 2.0 * q.w.clamp(-1.0, 1.0).acos();
        let s = (1.0 - q.w * q.w).max(0.0).sqrt();
        if s < EPSILON {
            (Vec3::Y, 0.0)
        } else {
            (Vec3::new(q.x / s, q.y / s, q.z / s), angle)
        }
    }

    /// 直观的欧拉角（弧度）：`(yaw 绕 Y, pitch 绕 X, roll 绕 Z)`。
    pub fn to_euler_yxz(self) -> (f32, f32, f32) {
        let q = self.normalize_or_identity();
        let (x, y, z, w) = (q.x, q.y, q.z, q.w);
        // pitch (X)：R[1][2] = 2(yz - wx)
        let sin_pitch = 2.0 * (w * x - y * z);
        let pitch = if sin_pitch.abs() >= 1.0 {
            sin_pitch.signum() * core::f32::consts::FRAC_PI_2
        } else {
            sin_pitch.asin()
        };
        // yaw (Y)：R[0][2] = 2(xz + wy)，R[2][2] = 1 - 2(x² + y²)
        let sin_yaw = 2.0 * (x * z + w * y);
        let cos_yaw = 1.0 - 2.0 * (x * x + y * y);
        let yaw = sin_yaw.atan2(cos_yaw);
        // roll (Z)：R[1][0] = 2(xy + wz)，R[1][1] = 1 - 2(x² + z²)
        let sin_roll = 2.0 * (w * z + x * y);
        let cos_roll = 1.0 - 2.0 * (x * x + z * z);
        let roll = sin_roll.atan2(cos_roll);
        (yaw, pitch, roll)
    }

    /// 前方向量（-Z）。
    #[inline]
    pub fn forward(self) -> Vec3 {
        self.mul_vec3(Vec3::NEG_Z)
    }

    /// 右方向量（+X）。
    #[inline]
    pub fn right(self) -> Vec3 {
        self.mul_vec3(Vec3::X)
    }

    /// 上方向量（+Y）。
    #[inline]
    pub fn up(self) -> Vec3 {
        self.mul_vec3(Vec3::Y)
    }

    /// 绕本地轴旋转。
    #[inline]
    pub fn rotate_local(self, axis: Vec3, angle: f32) -> Self {
        self * Self::from_axis_angle(axis, angle)
    }

    /// 是否有限。
    #[inline]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite() && self.w.is_finite()
    }

    /// 近似相等（考虑 `q` 与 `-q` 表示同一旋转）。
    pub fn approx_eq_rotation(self, o: Self) -> bool {
        let a = self.normalize_or_identity();
        let b = o.normalize_or_identity();
        (approx_eq(a.x, b.x) && approx_eq(a.y, b.y) && approx_eq(a.z, b.z) && approx_eq(a.w, b.w))
            || (approx_eq(a.x, -b.x)
                && approx_eq(a.y, -b.y)
                && approx_eq(a.z, -b.z)
                && approx_eq(a.w, -b.w))
    }

    /// 转为数组。
    #[inline]
    pub const fn to_array(self) -> [f32; 4] {
        [self.x, self.y, self.z, self.w]
    }
}

impl Mul for Quat {
    type Output = Self;
    /// Hamilton 乘积：`self * rhs` 表示先应用 `rhs` 再应用 `self`。
    #[inline]
    fn mul(self, r: Self) -> Self {
        Self::new(
            self.w * r.x + self.x * r.w + self.y * r.z - self.z * r.y,
            self.w * r.y - self.x * r.z + self.y * r.w + self.z * r.x,
            self.w * r.z + self.x * r.y - self.y * r.x + self.z * r.w,
            self.w * r.w - self.x * r.x - self.y * r.y - self.z * r.z,
        )
    }
}

impl Mul<Vec3> for Quat {
    type Output = Vec3;
    #[inline]
    fn mul(self, v: Vec3) -> Vec3 {
        self.mul_vec3(v)
    }
}

impl Mul<f32> for Quat {
    type Output = Self;
    #[inline]
    fn mul(self, s: f32) -> Self {
        Self::new(self.x * s, self.y * s, self.z * s, self.w * s)
    }
}

impl MulAssign for Quat {
    #[inline]
    fn mul_assign(&mut self, r: Self) {
        *self = *self * r;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::f32::consts::{FRAC_PI_2, PI};

    #[test]
    fn identity_rotation() {
        assert_eq!(Quat::IDENTITY.mul_vec3(Vec3::X), Vec3::X);
    }

    #[test]
    fn rotate_90_about_y_maps_x_to_minus_z() {
        let q = Quat::from_rotation_y(FRAC_PI_2);
        let v = q.mul_vec3(Vec3::X);
        assert!((v - Vec3::NEG_Z).length() < 1e-5, "实际 {v:?}");
    }

    #[test]
    fn mat3_roundtrip() {
        for q in [
            Quat::from_rotation_y(0.7),
            Quat::from_axis_angle(Vec3::new(1.0, 2.0, 3.0).normalize_or_zero(), 2.1),
            Quat::from_rotation_z(-1.4),
        ] {
            let back = Quat::from_mat3(q.to_mat3());
            assert!(back.approx_eq_rotation(q), "{q:?} -> {back:?}");
        }
    }

    #[test]
    fn slerp_halfway() {
        let a = Quat::IDENTITY;
        let b = Quat::from_rotation_y(FRAC_PI_2);
        let h = a.slerp(b, 0.5);
        let expected = Quat::from_rotation_y(FRAC_PI_2 * 0.5);
        assert!(h.approx_eq_rotation(expected), "{h:?} != {expected:?}");
    }

    #[test]
    fn euler_roundtrip() {
        let q = Quat::from_euler_yxz(0.6, -0.3, 0.2);
        let (y, p, r) = q.to_euler_yxz();
        let q2 = Quat::from_euler_yxz(y, p, r);
        assert!(q2.approx_eq_rotation(q));
    }

    #[test]
    fn rotation_arc() {
        let q = Quat::from_rotation_arc(Vec3::X, Vec3::Y);
        assert!((q.mul_vec3(Vec3::X) - Vec3::Y).length() < 1e-5);
        let q2 = Quat::from_rotation_arc(Vec3::X, Vec3::NEG_X);
        assert!((q2.mul_vec3(Vec3::X) - Vec3::NEG_X).length() < 1e-4);
        // 任意夹角（不是 90°）：旧实现把 w 写成 sqrt(1+d)，会转过头。
        let to = Vec3::new(0.0, 0.6, 0.8);
        let q3 = Quat::from_rotation_arc(Vec3::Y, to);
        assert!(
            (q3.mul_vec3(Vec3::Y) - to).length() < 1e-5,
            "Y 应精确转到 {to:?}，实际 {:?}",
            q3.mul_vec3(Vec3::Y)
        );
    }

    #[test]
    fn look_to_forward_is_minus_z() {
        let q = Quat::look_to_rh(Vec3::NEG_Z, Vec3::Y);
        assert!(q.approx_eq_rotation(Quat::IDENTITY));
        let q2 = Quat::look_to_rh(Vec3::X, Vec3::Y);
        assert!((q2.forward() - Vec3::X).length() < 1e-5);
    }

    #[test]
    fn angle_between_pi() {
        let a = Quat::IDENTITY;
        let b = Quat::from_rotation_y(PI);
        assert!((a.angle_between(b) - PI).abs() < 1e-4);
    }
}
