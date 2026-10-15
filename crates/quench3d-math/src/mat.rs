//! 矩阵类型：`Mat3`（旋转/法线变换）与 `Mat4`（列主序，wgpu 可直接上传）。

use core::ops::{Mul, MulAssign};

use crate::{approx_eq, Quat, Vec3, Vec4};

/// 3x3 矩阵，列主序。
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "bytemuck", derive(bytemuck::Pod, bytemuck::Zeroable))]
#[repr(C)]
pub struct Mat3 {
    /// 三列。
    pub cols: [Vec3; 3],
}

impl Default for Mat3 {
    #[inline]
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Mat3 {
    /// 单位矩阵。
    pub const IDENTITY: Self = Self {
        cols: [
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        ],
    };
    /// 零矩阵。
    pub const ZERO: Self = Self {
        cols: [Vec3::ZERO, Vec3::ZERO, Vec3::ZERO],
    };

    /// 按列构造。
    #[inline]
    pub const fn from_cols(x: Vec3, y: Vec3, z: Vec3) -> Self {
        Self { cols: [x, y, z] }
    }

    /// 按列主序数组构造：`[c0.x, c0.y, c0.z, c1.x, ...]`。
    #[inline]
    pub const fn from_cols_array(a: &[f32; 9]) -> Self {
        Self::from_cols(
            Vec3::new(a[0], a[1], a[2]),
            Vec3::new(a[3], a[4], a[5]),
            Vec3::new(a[6], a[7], a[8]),
        )
    }

    /// 展开为列主序数组。
    #[inline]
    pub const fn to_cols_array(&self) -> [f32; 9] {
        [
            self.cols[0].x,
            self.cols[0].y,
            self.cols[0].z,
            self.cols[1].x,
            self.cols[1].y,
            self.cols[1].z,
            self.cols[2].x,
            self.cols[2].y,
            self.cols[2].z,
        ]
    }

    /// 对角矩阵。
    #[inline]
    pub const fn from_diagonal(d: Vec3) -> Self {
        Self::from_cols(
            Vec3::new(d.x, 0.0, 0.0),
            Vec3::new(0.0, d.y, 0.0),
            Vec3::new(0.0, 0.0, d.z),
        )
    }

    /// 缩放矩阵。
    #[inline]
    pub const fn from_scale(s: Vec3) -> Self {
        Self::from_diagonal(s)
    }

    /// 绕任意轴旋转。
    #[inline]
    pub fn from_axis_angle(axis: Vec3, angle: f32) -> Self {
        Self::from_quat(Quat::from_axis_angle(axis, angle))
    }

    /// 由四元数构造旋转矩阵。
    pub fn from_quat(q: Quat) -> Self {
        let q = q.normalize_or_identity();
        let (x, y, z, w) = (q.x, q.y, q.z, q.w);
        let (x2, y2, z2) = (x + x, y + y, z + z);
        let (xx, xy, xz) = (x * x2, x * y2, x * z2);
        let (yy, yz, zz) = (y * y2, y * z2, z * z2);
        let (wx, wy, wz) = (w * x2, w * y2, w * z2);
        Self::from_cols(
            Vec3::new(1.0 - (yy + zz), xy + wz, xz - wy),
            Vec3::new(xy - wz, 1.0 - (xx + zz), yz + wx),
            Vec3::new(xz + wy, yz - wx, 1.0 - (xx + yy)),
        )
    }

    /// 取第 `i` 列。
    #[inline]
    pub fn col(&self, i: usize) -> Vec3 {
        self.cols[i]
    }

    /// 转置。
    #[inline]
    pub fn transpose(&self) -> Self {
        let c = &self.cols;
        Self::from_cols(
            Vec3::new(c[0].x, c[1].x, c[2].x),
            Vec3::new(c[0].y, c[1].y, c[2].y),
            Vec3::new(c[0].z, c[1].z, c[2].z),
        )
    }

    /// 行列式。
    #[inline]
    pub fn determinant(&self) -> f32 {
        let c = &self.cols;
        c[0].x * (c[1].y * c[2].z - c[2].y * c[1].z) - c[1].x * (c[0].y * c[2].z - c[2].y * c[0].z)
            + c[2].x * (c[0].y * c[1].z - c[1].y * c[0].z)
    }

    /// 逆矩阵；奇异时返回 `None`。
    pub fn try_inverse(&self) -> Option<Self> {
        let c = &self.cols;
        let (a, b, cc) = (c[0].x, c[1].x, c[2].x);
        let (d, e, f) = (c[0].y, c[1].y, c[2].y);
        let (g, h, i) = (c[0].z, c[1].z, c[2].z);

        let det = a * (e * i - f * h) - b * (d * i - f * g) + cc * (d * h - e * g);
        if det.abs() < 1e-12 {
            return None;
        }
        let inv = 1.0 / det;
        // 伴随矩阵的转置，按列写入。
        Some(Self::from_cols(
            Vec3::new(
                (e * i - f * h) * inv,
                (f * g - d * i) * inv,
                (d * h - e * g) * inv,
            ),
            Vec3::new(
                (cc * h - b * i) * inv,
                (a * i - cc * g) * inv,
                (b * g - a * h) * inv,
            ),
            Vec3::new(
                (b * f - cc * e) * inv,
                (cc * d - a * f) * inv,
                (a * e - b * d) * inv,
            ),
        ))
    }

    /// 逆矩阵；奇异时返回零矩阵。
    #[inline]
    pub fn inverse(&self) -> Self {
        self.try_inverse().unwrap_or(Self::ZERO)
    }

    /// 变换向量（`M * v`）。
    #[inline]
    pub fn mul_vec3(&self, v: Vec3) -> Vec3 {
        self.cols[0] * v.x + self.cols[1] * v.y + self.cols[2] * v.z
    }

    /// 是否有限。
    #[inline]
    pub fn is_finite(&self) -> bool {
        self.cols.iter().all(|c| c.is_finite())
    }

    /// 正交化（Gram–Schmidt），用于抑制累积误差。
    pub fn orthonormalize(&self) -> Self {
        let x = self.cols[0].normalize_or_zero();
        let y = (self.cols[1] - x * self.cols[1].dot(x)).normalize_or_zero();
        let z = x.cross(y);
        Self::from_cols(x, y, z)
    }

    /// 扩展为 `Mat4`。
    #[inline]
    pub fn to_mat4(&self) -> Mat4 {
        Mat4::from_cols(
            self.cols[0].extend(0.0),
            self.cols[1].extend(0.0),
            self.cols[2].extend(0.0),
            Vec4::new(0.0, 0.0, 0.0, 1.0),
        )
    }
}

impl Mul for Mat3 {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: Self) -> Self {
        Self::from_cols(
            self.mul_vec3(rhs.cols[0]),
            self.mul_vec3(rhs.cols[1]),
            self.mul_vec3(rhs.cols[2]),
        )
    }
}

impl Mul<Vec3> for Mat3 {
    type Output = Vec3;
    #[inline]
    fn mul(self, v: Vec3) -> Vec3 {
        self.mul_vec3(v)
    }
}

impl Mul<f32> for Mat3 {
    type Output = Self;
    #[inline]
    fn mul(self, s: f32) -> Self {
        Self::from_cols(self.cols[0] * s, self.cols[1] * s, self.cols[2] * s)
    }
}

impl MulAssign for Mat3 {
    #[inline]
    fn mul_assign(&mut self, rhs: Self) {
        *self = *self * rhs;
    }
}

/// 4x4 矩阵，**列主序**存储（与 WGSL `mat4x4<f32>` 内存布局一致）。
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "bytemuck", derive(bytemuck::Pod, bytemuck::Zeroable))]
#[repr(C)]
pub struct Mat4 {
    /// 四列；`cols[i][j]` 是第 `i` 列第 `j` 行。
    pub cols: [Vec4; 4],
}

impl Default for Mat4 {
    #[inline]
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Mat4 {
    /// 单位矩阵。
    pub const IDENTITY: Self = Self {
        cols: [
            Vec4::new(1.0, 0.0, 0.0, 0.0),
            Vec4::new(0.0, 1.0, 0.0, 0.0),
            Vec4::new(0.0, 0.0, 1.0, 0.0),
            Vec4::new(0.0, 0.0, 0.0, 1.0),
        ],
    };
    /// 零矩阵。
    pub const ZERO: Self = Self {
        cols: [Vec4::ZERO, Vec4::ZERO, Vec4::ZERO, Vec4::ZERO],
    };

    /// 按列构造。
    #[inline]
    pub const fn from_cols(x: Vec4, y: Vec4, z: Vec4, w: Vec4) -> Self {
        Self { cols: [x, y, z, w] }
    }

    /// 按列主序数组构造（`[c0.x, c0.y, c0.z, c0.w, c1.x, ...]`）。
    #[inline]
    pub const fn from_cols_array(a: &[f32; 16]) -> Self {
        Self::from_cols(
            Vec4::new(a[0], a[1], a[2], a[3]),
            Vec4::new(a[4], a[5], a[6], a[7]),
            Vec4::new(a[8], a[9], a[10], a[11]),
            Vec4::new(a[12], a[13], a[14], a[15]),
        )
    }

    /// 按行主序数组构造（从其它库/文件读入时很方便）。
    #[inline]
    pub const fn from_rows_array(a: &[f32; 16]) -> Self {
        Self::from_cols(
            Vec4::new(a[0], a[4], a[8], a[12]),
            Vec4::new(a[1], a[5], a[9], a[13]),
            Vec4::new(a[2], a[6], a[10], a[14]),
            Vec4::new(a[3], a[7], a[11], a[15]),
        )
    }

    /// 展开为列主序数组。
    #[inline]
    pub const fn to_cols_array(&self) -> [f32; 16] {
        [
            self.cols[0].x,
            self.cols[0].y,
            self.cols[0].z,
            self.cols[0].w,
            self.cols[1].x,
            self.cols[1].y,
            self.cols[1].z,
            self.cols[1].w,
            self.cols[2].x,
            self.cols[2].y,
            self.cols[2].z,
            self.cols[2].w,
            self.cols[3].x,
            self.cols[3].y,
            self.cols[3].z,
            self.cols[3].w,
        ]
    }

    /// 展开为 `[[f32; 4]; 4]`（列优先）。
    #[inline]
    pub const fn to_cols_arrays(&self) -> [[f32; 4]; 4] {
        [
            self.cols[0].to_array(),
            self.cols[1].to_array(),
            self.cols[2].to_array(),
            self.cols[3].to_array(),
        ]
    }

    /// 平移矩阵。
    #[inline]
    pub const fn from_translation(t: Vec3) -> Self {
        Self::from_cols(
            Vec4::new(1.0, 0.0, 0.0, 0.0),
            Vec4::new(0.0, 1.0, 0.0, 0.0),
            Vec4::new(0.0, 0.0, 1.0, 0.0),
            Vec4::new(t.x, t.y, t.z, 1.0),
        )
    }

    /// 缩放矩阵。
    #[inline]
    pub const fn from_scale(s: Vec3) -> Self {
        Self::from_cols(
            Vec4::new(s.x, 0.0, 0.0, 0.0),
            Vec4::new(0.0, s.y, 0.0, 0.0),
            Vec4::new(0.0, 0.0, s.z, 0.0),
            Vec4::new(0.0, 0.0, 0.0, 1.0),
        )
    }

    /// 绕 X 轴旋转。
    #[inline]
    pub fn from_rotation_x(angle: f32) -> Self {
        let (s, c) = angle.sin_cos();
        Self::from_cols(
            Vec4::new(1.0, 0.0, 0.0, 0.0),
            Vec4::new(0.0, c, s, 0.0),
            Vec4::new(0.0, -s, c, 0.0),
            Vec4::new(0.0, 0.0, 0.0, 1.0),
        )
    }

    /// 绕 Y 轴旋转。
    #[inline]
    pub fn from_rotation_y(angle: f32) -> Self {
        let (s, c) = angle.sin_cos();
        Self::from_cols(
            Vec4::new(c, 0.0, -s, 0.0),
            Vec4::new(0.0, 1.0, 0.0, 0.0),
            Vec4::new(s, 0.0, c, 0.0),
            Vec4::new(0.0, 0.0, 0.0, 1.0),
        )
    }

    /// 绕 Z 轴旋转。
    #[inline]
    pub fn from_rotation_z(angle: f32) -> Self {
        let (s, c) = angle.sin_cos();
        Self::from_cols(
            Vec4::new(c, s, 0.0, 0.0),
            Vec4::new(-s, c, 0.0, 0.0),
            Vec4::new(0.0, 0.0, 1.0, 0.0),
            Vec4::new(0.0, 0.0, 0.0, 1.0),
        )
    }

    /// 由四元数构造旋转矩阵。
    #[inline]
    pub fn from_quat(q: Quat) -> Self {
        Mat3::from_quat(q).to_mat4()
    }

    /// 绕任意轴旋转。
    #[inline]
    pub fn from_axis_angle(axis: Vec3, angle: f32) -> Self {
        Self::from_quat(Quat::from_axis_angle(axis, angle))
    }

    /// 依次应用缩放、旋转、平移得到的 `T * R * S`。
    #[inline]
    pub fn from_rotation_translation_scale(t: Vec3, r: Quat, s: Vec3) -> Self {
        let m = Mat4::from_quat(r);
        Self::from_cols(
            m.cols[0] * s.x,
            m.cols[1] * s.y,
            m.cols[2] * s.z,
            Vec4::new(t.x, t.y, t.z, 1.0),
        )
    }

    /// 右手系观察矩阵（相机看向 `center`）。
    pub fn look_at_rh(eye: Vec3, center: Vec3, up: Vec3) -> Self {
        Self::look_to_rh(eye, (center - eye).normalize_or_zero(), up)
    }

    /// 右手系观察矩阵（给定朝向）。
    pub fn look_to_rh(eye: Vec3, dir: Vec3, up: Vec3) -> Self {
        let f = dir.normalize_or_zero();
        // 处理 dir 与 up 平行导致的退化。
        let mut up_n = up.normalize_or_zero();
        if f.cross(up_n).length_squared() < 1e-8 {
            let alt = if f.y.abs() > 0.9 { Vec3::Z } else { Vec3::Y };
            up_n = alt;
        }
        let s = f.cross(up_n).normalize_or_zero();
        let u = s.cross(f);
        Self::from_cols(
            Vec4::new(s.x, u.x, -f.x, 0.0),
            Vec4::new(s.y, u.y, -f.y, 0.0),
            Vec4::new(s.z, u.z, -f.z, 0.0),
            Vec4::new(-s.dot(eye), -u.dot(eye), f.dot(eye), 1.0),
        )
    }

    /// 右手系透视投影，深度区间 `[0, 1]`（wgpu / D3D 约定）。
    pub fn perspective_rh(fov_y_radians: f32, aspect: f32, near: f32, far: f32) -> Self {
        let f = 1.0 / (fov_y_radians * 0.5).tan();
        let d = near - far;
        Self::from_cols(
            Vec4::new(f / aspect.max(1e-6), 0.0, 0.0, 0.0),
            Vec4::new(0.0, f, 0.0, 0.0),
            Vec4::new(0.0, 0.0, far / d, -1.0),
            Vec4::new(0.0, 0.0, near * far / d, 0.0),
        )
    }

    /// 右手系透视投影，深度区间 `[-1, 1]`（OpenGL 约定）。
    pub fn perspective_rh_gl(fov_y_radians: f32, aspect: f32, near: f32, far: f32) -> Self {
        let f = 1.0 / (fov_y_radians * 0.5).tan();
        let d = near - far;
        Self::from_cols(
            Vec4::new(f / aspect.max(1e-6), 0.0, 0.0, 0.0),
            Vec4::new(0.0, f, 0.0, 0.0),
            Vec4::new(0.0, 0.0, (far + near) / d, -1.0),
            Vec4::new(0.0, 0.0, 2.0 * far * near / d, 0.0),
        )
    }

    /// 右手系正交投影，深度区间 `[0, 1]`。
    pub fn orthographic_rh(
        left: f32,
        right: f32,
        bottom: f32,
        top: f32,
        near: f32,
        far: f32,
    ) -> Self {
        let rl = right - left;
        let tb = top - bottom;
        let nf = near - far;
        Self::from_cols(
            Vec4::new(2.0 / rl, 0.0, 0.0, 0.0),
            Vec4::new(0.0, 2.0 / tb, 0.0, 0.0),
            Vec4::new(0.0, 0.0, 1.0 / nf, 0.0),
            Vec4::new(-(right + left) / rl, -(top + bottom) / tb, near / nf, 1.0),
        )
    }

    /// 转置。
    #[inline]
    pub fn transpose(&self) -> Self {
        let c = &self.cols;
        Self::from_cols(
            Vec4::new(c[0].x, c[1].x, c[2].x, c[3].x),
            Vec4::new(c[0].y, c[1].y, c[2].y, c[3].y),
            Vec4::new(c[0].z, c[1].z, c[2].z, c[3].z),
            Vec4::new(c[0].w, c[1].w, c[2].w, c[3].w),
        )
    }

    /// 行列式。
    pub fn determinant(&self) -> f32 {
        let m = self.to_cols_array();
        let (m00, m01, m02, m03) = (m[0], m[4], m[8], m[12]);
        let (m10, m11, m12, m13) = (m[1], m[5], m[9], m[13]);
        let (m20, m21, m22, m23) = (m[2], m[6], m[10], m[14]);
        let (m30, m31, m32, m33) = (m[3], m[7], m[11], m[15]);

        m00 * (m11 * (m22 * m33 - m32 * m23) - m21 * (m12 * m33 - m32 * m13)
            + m31 * (m12 * m23 - m22 * m13))
            - m10
                * (m01 * (m22 * m33 - m32 * m23) - m21 * (m02 * m33 - m32 * m03)
                    + m31 * (m02 * m23 - m22 * m03))
            + m20
                * (m01 * (m12 * m33 - m32 * m13) - m11 * (m02 * m33 - m32 * m03)
                    + m31 * (m02 * m13 - m12 * m03))
            - m30
                * (m01 * (m12 * m23 - m22 * m13) - m11 * (m02 * m23 - m22 * m03)
                    + m21 * (m02 * m13 - m12 * m03))
    }

    /// 逆矩阵；奇异时返回 `None`。
    pub fn try_inverse(&self) -> Option<Self> {
        // 行主序展开，套用经典伴随矩阵公式（MESA gluInvertMatrix 的推导）。
        let c = &self.cols;
        let m: [f32; 16] = [
            c[0].x, c[1].x, c[2].x, c[3].x, // 第 0 行
            c[0].y, c[1].y, c[2].y, c[3].y, // 第 1 行
            c[0].z, c[1].z, c[2].z, c[3].z, // 第 2 行
            c[0].w, c[1].w, c[2].w, c[3].w, // 第 3 行
        ];

        let mut inv = [0.0f32; 16];
        inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15]
            + m[9] * m[7] * m[14]
            + m[13] * m[6] * m[11]
            - m[13] * m[7] * m[10];
        inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15]
            - m[8] * m[7] * m[14]
            - m[12] * m[6] * m[11]
            + m[12] * m[7] * m[10];
        inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15]
            + m[8] * m[7] * m[13]
            + m[12] * m[5] * m[11]
            - m[12] * m[7] * m[9];
        inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14]
            - m[8] * m[6] * m[13]
            - m[12] * m[5] * m[10]
            + m[12] * m[6] * m[9];
        inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15]
            - m[9] * m[3] * m[14]
            - m[13] * m[2] * m[11]
            + m[13] * m[3] * m[10];
        inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15]
            + m[8] * m[3] * m[14]
            + m[12] * m[2] * m[11]
            - m[12] * m[3] * m[10];
        inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15]
            - m[8] * m[3] * m[13]
            - m[12] * m[1] * m[11]
            + m[12] * m[3] * m[9];
        inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14]
            + m[8] * m[2] * m[13]
            + m[12] * m[1] * m[10]
            - m[12] * m[2] * m[9];
        inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15]
            + m[5] * m[3] * m[14]
            + m[13] * m[2] * m[7]
            - m[13] * m[3] * m[6];
        inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15]
            - m[4] * m[3] * m[14]
            - m[12] * m[2] * m[7]
            + m[12] * m[3] * m[6];
        inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15]
            + m[4] * m[3] * m[13]
            + m[12] * m[1] * m[7]
            - m[12] * m[3] * m[5];
        inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14]
            - m[4] * m[2] * m[13]
            - m[12] * m[1] * m[6]
            + m[12] * m[2] * m[5];
        inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11]
            - m[5] * m[3] * m[10]
            - m[9] * m[2] * m[7]
            + m[9] * m[3] * m[6];
        inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11]
            + m[4] * m[3] * m[10]
            + m[8] * m[2] * m[7]
            - m[8] * m[3] * m[6];
        inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11]
            - m[4] * m[3] * m[9]
            - m[8] * m[1] * m[7]
            + m[8] * m[3] * m[5];
        inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10]
            + m[4] * m[2] * m[9]
            + m[8] * m[1] * m[6]
            - m[8] * m[2] * m[5];

        let det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12];
        if det.abs() < 1e-20 || !det.is_finite() {
            return None;
        }
        let d = 1.0 / det;
        // `inv` 是行主序，写入时需要转成列主序。
        Some(Self::from_cols(
            Vec4::new(inv[0] * d, inv[4] * d, inv[8] * d, inv[12] * d),
            Vec4::new(inv[1] * d, inv[5] * d, inv[9] * d, inv[13] * d),
            Vec4::new(inv[2] * d, inv[6] * d, inv[10] * d, inv[14] * d),
            Vec4::new(inv[3] * d, inv[7] * d, inv[11] * d, inv[15] * d),
        ))
    }

    /// 逆矩阵；奇异时返回单位矩阵。
    #[inline]
    pub fn inverse(&self) -> Self {
        self.try_inverse().unwrap_or(Self::IDENTITY)
    }

    /// 变换齐次向量。
    #[inline]
    pub fn mul_vec4(&self, v: Vec4) -> Vec4 {
        self.cols[0] * v.x + self.cols[1] * v.y + self.cols[2] * v.z + self.cols[3] * v.w
    }

    /// 变换点（`w = 1`，不做透视除法）。
    #[inline]
    pub fn transform_point3(&self, p: Vec3) -> Vec3 {
        let r = self.mul_vec4(p.extend(1.0));
        Vec3::new(r.x, r.y, r.z)
    }

    /// 变换方向（`w = 0`，忽略平移）。
    #[inline]
    pub fn transform_vector3(&self, v: Vec3) -> Vec3 {
        let r = self.mul_vec4(v.extend(0.0));
        Vec3::new(r.x, r.y, r.z)
    }

    /// 变换点并做透视除法。
    #[inline]
    pub fn project_point3(&self, p: Vec3) -> Vec3 {
        let r = self.mul_vec4(p.extend(1.0));
        if r.w.abs() > 1e-12 {
            Vec3::new(r.x / r.w, r.y / r.w, r.z / r.w)
        } else {
            Vec3::new(r.x, r.y, r.z)
        }
    }

    /// 取平移分量。
    #[inline]
    pub fn translation(&self) -> Vec3 {
        Vec3::new(self.cols[3].x, self.cols[3].y, self.cols[3].z)
    }

    /// 取左上 3x3。
    #[inline]
    pub fn to_mat3(&self) -> Mat3 {
        Mat3::from_cols(
            self.cols[0].truncate(),
            self.cols[1].truncate(),
            self.cols[2].truncate(),
        )
    }

    /// 由 `Mat3` 提升（平移为 0）。
    #[inline]
    pub fn from_mat3(m: Mat3) -> Self {
        m.to_mat4()
    }

    /// 分解为 `(缩放, 旋转, 平移)`。
    pub fn to_scale_rotation_translation(&self) -> (Vec3, Quat, Vec3) {
        let scale = Vec3::new(
            self.cols[0].truncate().length(),
            self.cols[1].truncate().length(),
            self.cols[2].truncate().length(),
        );
        // 用行列式符号修正镜像导致的负缩放。
        let det = self.determinant();
        let scale = if det < 0.0 {
            Vec3::new(-scale.x, scale.y, scale.z)
        } else {
            scale
        };
        let mut rot = Mat3::from_cols(
            self.cols[0].truncate() / scale.x.max(1e-12),
            self.cols[1].truncate() / scale.y.max(1e-12),
            self.cols[2].truncate() / scale.z.max(1e-12),
        );
        rot = rot.orthonormalize();
        (scale, Quat::from_mat3(rot), self.translation())
    }

    /// 是否有限。
    #[inline]
    pub fn is_finite(&self) -> bool {
        self.cols
            .iter()
            .all(|c| c.x.is_finite() && c.y.is_finite() && c.z.is_finite() && c.w.is_finite())
    }

    /// 近似相等（逐元素）。
    pub fn approx_eq(&self, o: &Self) -> bool {
        self.to_cols_array()
            .iter()
            .zip(o.to_cols_array().iter())
            .all(|(a, b)| approx_eq(*a, *b))
    }
}

impl Mul for Mat4 {
    type Output = Self;
    #[inline]
    fn mul(self, rhs: Self) -> Self {
        Self::from_cols(
            self.mul_vec4(rhs.cols[0]),
            self.mul_vec4(rhs.cols[1]),
            self.mul_vec4(rhs.cols[2]),
            self.mul_vec4(rhs.cols[3]),
        )
    }
}

impl Mul<Vec4> for Mat4 {
    type Output = Vec4;
    #[inline]
    fn mul(self, v: Vec4) -> Vec4 {
        self.mul_vec4(v)
    }
}

impl Mul<f32> for Mat4 {
    type Output = Self;
    #[inline]
    fn mul(self, s: f32) -> Self {
        Self::from_cols(
            self.cols[0] * s,
            self.cols[1] * s,
            self.cols[2] * s,
            self.cols[3] * s,
        )
    }
}

impl MulAssign for Mat4 {
    #[inline]
    fn mul_assign(&mut self, rhs: Self) {
        *self = *self * rhs;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vec3;

    fn sample() -> Mat4 {
        Mat4::from_rotation_translation_scale(
            vec3(1.5, -2.0, 3.25),
            Quat::from_axis_angle(Vec3::new(0.3, 1.0, 0.2).normalize_or_zero(), 0.9),
            vec3(2.0, 0.5, 1.25),
        )
    }

    #[test]
    fn inverse_identity() {
        let m = sample();
        let prod = m * m.inverse();
        assert!(prod.approx_eq(&Mat4::IDENTITY), "M * M^-1 = {prod:?}");
    }

    #[test]
    fn inverse_singular_is_none() {
        assert!(Mat4::ZERO.try_inverse().is_none());
    }

    #[test]
    fn roundtrip_array() {
        let m = sample();
        let a = m.to_cols_array();
        assert!(Mat4::from_cols_array(&a).approx_eq(&m));
    }

    #[test]
    fn decompose() {
        let m = sample();
        let (s, r, t) = m.to_scale_rotation_translation();
        assert!(approx_eq(s.x, 2.0) && approx_eq(s.y, 0.5) && approx_eq(s.z, 1.25));
        assert!((t - vec3(1.5, -2.0, 3.25)).length() < 1e-5);
        let rebuilt = Mat4::from_rotation_translation_scale(t, r, s);
        assert!(rebuilt.approx_eq(&m), "重建失败:\n{rebuilt:?}\n{m:?}");
    }

    #[test]
    fn perspective_depth_range_0_1() {
        let p = Mat4::perspective_rh(1.0, 1.6, 0.1, 100.0);
        let near = p.project_point3(vec3(0.0, 0.0, -0.1));
        let far = p.project_point3(vec3(0.0, 0.0, -100.0));
        assert!(near.z.abs() < 1e-4, "近平面 z 应为 0，实际 {}", near.z);
        assert!(
            (far.z - 1.0).abs() < 1e-4,
            "远平面 z 应为 1，实际 {}",
            far.z
        );
    }

    #[test]
    fn look_at_puts_center_in_front() {
        let v = Mat4::look_at_rh(vec3(0.0, 0.0, 5.0), Vec3::ZERO, Vec3::Y);
        let p = v.transform_point3(Vec3::ZERO);
        assert!(p.z < 0.0, "观察空间里目标应在 -Z 方向，实际 {p:?}");
        assert!(p.x.abs() < 1e-5 && p.y.abs() < 1e-5);
    }

    #[test]
    fn mat3_inverse() {
        let m = Mat3::from_quat(Quat::from_axis_angle(Vec3::Y, 0.7))
            * Mat3::from_scale(vec3(2.0, 3.0, 4.0));
        let id = m * m.inverse();
        assert!((id.cols[0] - Vec3::X).length() < 1e-5);
        assert!((id.cols[1] - Vec3::Y).length() < 1e-5);
        assert!((id.cols[2] - Vec3::Z).length() < 1e-5);
    }
}
