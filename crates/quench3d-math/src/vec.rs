//! 向量类型：`Vec2` / `Vec3` / `Vec4`。
//!
//! 全部为 `#[repr(C)]` 的 `f32` 紧凑布局，可直接上传到 GPU。

use core::fmt;
use core::ops::{
    Add, AddAssign, Div, DivAssign, Index, IndexMut, Mul, MulAssign, Neg, Sub, SubAssign,
};

use crate::{sign_or_zero, EPSILON};

/// 为向量生成逐分量运算符实现。
macro_rules! impl_vec_ops {
    ($name:ident, $($f:ident),+) => {
        impl Add for $name {
            type Output = Self;
            #[inline]
            fn add(self, o: Self) -> Self { Self { $($f: self.$f + o.$f),+ } }
        }
        impl Add<f32> for $name {
            type Output = Self;
            #[inline]
            fn add(self, s: f32) -> Self { Self { $($f: self.$f + s),+ } }
        }
        impl AddAssign for $name {
            #[inline]
            fn add_assign(&mut self, o: Self) { $(self.$f += o.$f;)+ }
        }
        impl AddAssign<f32> for $name {
            #[inline]
            fn add_assign(&mut self, s: f32) { $(self.$f += s;)+ }
        }
        impl Sub for $name {
            type Output = Self;
            #[inline]
            fn sub(self, o: Self) -> Self { Self { $($f: self.$f - o.$f),+ } }
        }
        impl Sub<f32> for $name {
            type Output = Self;
            #[inline]
            fn sub(self, s: f32) -> Self { Self { $($f: self.$f - s),+ } }
        }
        impl SubAssign for $name {
            #[inline]
            fn sub_assign(&mut self, o: Self) { $(self.$f -= o.$f;)+ }
        }
        impl Mul<f32> for $name {
            type Output = Self;
            #[inline]
            fn mul(self, s: f32) -> Self { Self { $($f: self.$f * s),+ } }
        }
        impl Mul<$name> for f32 {
            type Output = $name;
            #[inline]
            fn mul(self, v: $name) -> $name { v * self }
        }
        impl Mul for $name {
            type Output = Self;
            #[inline]
            fn mul(self, o: Self) -> Self { Self { $($f: self.$f * o.$f),+ } }
        }
        impl MulAssign<f32> for $name {
            #[inline]
            fn mul_assign(&mut self, s: f32) { $(self.$f *= s;)+ }
        }
        impl MulAssign for $name {
            #[inline]
            fn mul_assign(&mut self, o: Self) { $(self.$f *= o.$f;)+ }
        }
        impl Div<f32> for $name {
            type Output = Self;
            #[inline]
            fn div(self, s: f32) -> Self { Self { $($f: self.$f / s),+ } }
        }
        impl Div for $name {
            type Output = Self;
            #[inline]
            fn div(self, o: Self) -> Self { Self { $($f: self.$f / o.$f),+ } }
        }
        impl DivAssign<f32> for $name {
            #[inline]
            fn div_assign(&mut self, s: f32) { $(self.$f /= s;)+ }
        }
        impl Neg for $name {
            type Output = Self;
            #[inline]
            fn neg(self) -> Self { Self { $($f: -self.$f),+ } }
        }
    };
}

// ---------------------------------------------------------------------------
// Vec2
// ---------------------------------------------------------------------------

/// 二维向量。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "bytemuck", derive(bytemuck::Pod, bytemuck::Zeroable))]
#[repr(C)]
pub struct Vec2 {
    /// X 分量。
    pub x: f32,
    /// Y 分量。
    pub y: f32,
}

impl_vec_ops!(Vec2, x, y);

impl Vec2 {
    /// 零向量。
    pub const ZERO: Self = Self::new(0.0, 0.0);
    /// 全 1 向量。
    pub const ONE: Self = Self::new(1.0, 1.0);
    /// X 单位向量。
    pub const X: Self = Self::new(1.0, 0.0);
    /// Y 单位向量。
    pub const Y: Self = Self::new(0.0, 1.0);

    /// 构造。
    #[inline]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    /// 所有分量取同一值。
    #[inline]
    pub const fn splat(v: f32) -> Self {
        Self::new(v, v)
    }
    /// 点积。
    #[inline]
    pub fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y
    }
    /// 二维叉积（返回 z 分量）。
    #[inline]
    pub fn cross(self, o: Self) -> f32 {
        self.x * o.y - self.y * o.x
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
    /// 归一化；零向量返回零向量。
    #[inline]
    pub fn normalize_or_zero(self) -> Self {
        let l = self.length();
        if l > EPSILON {
            self / l
        } else {
            Self::ZERO
        }
    }
    /// 归一化，失败返回 `None`。
    #[inline]
    pub fn try_normalize(self) -> Option<Self> {
        let l = self.length();
        if l > EPSILON {
            Some(self / l)
        } else {
            None
        }
    }
    /// 到另一点的距离。
    #[inline]
    pub fn distance(self, o: Self) -> f32 {
        (self - o).length()
    }
    /// 线性插值。
    #[inline]
    pub fn lerp(self, o: Self, t: f32) -> Self {
        self + (o - self) * t
    }
    /// 转为数组。
    #[inline]
    pub const fn to_array(self) -> [f32; 2] {
        [self.x, self.y]
    }
    /// 从数组构造。
    #[inline]
    pub const fn from_array(a: [f32; 2]) -> Self {
        Self::new(a[0], a[1])
    }
    /// 是否有限。
    #[inline]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
    /// 分量旋转指定弧度。
    #[inline]
    pub fn rotate(self, angle: f32) -> Self {
        let (s, c) = angle.sin_cos();
        Self::new(self.x * c - self.y * s, self.x * s + self.y * c)
    }
    /// 垂直向量（逆时针 90°）。
    #[inline]
    pub fn perp(self) -> Self {
        Self::new(-self.y, self.x)
    }
}

impl Index<usize> for Vec2 {
    type Output = f32;
    #[inline]
    fn index(&self, i: usize) -> &f32 {
        match i {
            0 => &self.x,
            1 => &self.y,
            _ => panic!("Vec2 索引越界: {i}"),
        }
    }
}

impl IndexMut<usize> for Vec2 {
    #[inline]
    fn index_mut(&mut self, i: usize) -> &mut f32 {
        match i {
            0 => &mut self.x,
            1 => &mut self.y,
            _ => panic!("Vec2 索引越界: {i}"),
        }
    }
}

impl From<[f32; 2]> for Vec2 {
    #[inline]
    fn from(a: [f32; 2]) -> Self {
        Self::from_array(a)
    }
}

impl From<Vec2> for [f32; 2] {
    #[inline]
    fn from(v: Vec2) -> Self {
        v.to_array()
    }
}

impl fmt::Display for Vec2 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

// ---------------------------------------------------------------------------
// Vec3
// ---------------------------------------------------------------------------

/// 三维向量（12 字节，`#[repr(C)]`，无填充）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "bytemuck", derive(bytemuck::Pod, bytemuck::Zeroable))]
#[repr(C)]
pub struct Vec3 {
    /// X 分量。
    pub x: f32,
    /// Y 分量。
    pub y: f32,
    /// Z 分量。
    pub z: f32,
}

impl_vec_ops!(Vec3, x, y, z);

impl Vec3 {
    /// 零向量。
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0);
    /// 全 1 向量。
    pub const ONE: Self = Self::new(1.0, 1.0, 1.0);
    /// X 单位向量。
    pub const X: Self = Self::new(1.0, 0.0, 0.0);
    /// Y 单位向量。
    pub const Y: Self = Self::new(0.0, 1.0, 0.0);
    /// Z 单位向量。
    pub const Z: Self = Self::new(0.0, 0.0, 1.0);
    /// -X 单位向量。
    pub const NEG_X: Self = Self::new(-1.0, 0.0, 0.0);
    /// -Y 单位向量。
    pub const NEG_Y: Self = Self::new(0.0, -1.0, 0.0);
    /// -Z 单位向量。
    pub const NEG_Z: Self = Self::new(0.0, 0.0, -1.0);
    /// 无穷大。
    pub const INFINITY: Self = Self::splat(f32::INFINITY);
    /// 负无穷大。
    pub const NEG_INFINITY: Self = Self::splat(f32::NEG_INFINITY);

    /// 构造。
    #[inline]
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }
    /// 所有分量取同一值。
    #[inline]
    pub const fn splat(v: f32) -> Self {
        Self::new(v, v, v)
    }
    /// 点积。
    #[inline]
    pub fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    /// 叉积。
    #[inline]
    pub fn cross(self, o: Self) -> Self {
        Self::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
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
    /// 距离平方。
    #[inline]
    pub fn distance_squared(self, o: Self) -> f32 {
        (self - o).length_squared()
    }
    /// 距离。
    #[inline]
    pub fn distance(self, o: Self) -> f32 {
        (self - o).length()
    }
    /// 归一化；零向量返回零向量（安全版本）。
    #[inline]
    pub fn normalize_or_zero(self) -> Self {
        let l = self.length();
        if l > EPSILON {
            self / l
        } else {
            Self::ZERO
        }
    }
    /// 归一化，长度为 0 时返回 `None`。
    #[inline]
    pub fn try_normalize(self) -> Option<Self> {
        let l = self.length();
        if l > EPSILON {
            Some(self / l)
        } else {
            None
        }
    }
    /// 归一化；调用者需保证非零（调试期会断言）。
    #[inline]
    pub fn normalize(self) -> Self {
        debug_assert!(self.length_squared() > 0.0, "Vec3::normalize 遇到零向量");
        self / self.length()
    }
    /// 是否已归一化。
    #[inline]
    pub fn is_normalized(self) -> bool {
        (self.length_squared() - 1.0).abs() < 1e-3
    }
    /// 逐分量最小值。
    #[inline]
    pub fn min(self, o: Self) -> Self {
        Self::new(self.x.min(o.x), self.y.min(o.y), self.z.min(o.z))
    }
    /// 逐分量最大值。
    #[inline]
    pub fn max(self, o: Self) -> Self {
        Self::new(self.x.max(o.x), self.y.max(o.y), self.z.max(o.z))
    }
    /// 逐分量绝对值。
    #[inline]
    pub fn abs(self) -> Self {
        Self::new(self.x.abs(), self.y.abs(), self.z.abs())
    }
    /// 逐分量符号（0 保持 0）。
    #[inline]
    pub fn signum(self) -> Self {
        Self::new(
            sign_or_zero(self.x),
            sign_or_zero(self.y),
            sign_or_zero(self.z),
        )
    }
    /// 逐分量夹紧。
    #[inline]
    pub fn clamp(self, lo: Self, hi: Self) -> Self {
        self.min(hi).max(lo)
    }
    /// 限制长度不超过 `max`。
    #[inline]
    pub fn clamp_length(self, max: f32) -> Self {
        let l = self.length();
        if l > max && l > EPSILON {
            self * (max / l)
        } else {
            self
        }
    }
    /// 线性插值。
    #[inline]
    pub fn lerp(self, o: Self, t: f32) -> Self {
        self + (o - self) * t
    }
    /// `self * a + b`（逐分量乘加）。
    #[inline]
    pub fn mul_add(self, a: Self, b: Self) -> Self {
        Self::new(
            self.x.mul_add(a.x, b.x),
            self.y.mul_add(a.y, b.y),
            self.z.mul_add(a.z, b.z),
        )
    }
    /// 两向量夹角（弧度）。
    #[inline]
    pub fn angle_between(self, o: Self) -> f32 {
        let d = self.dot(o);
        let l = (self.length_squared() * o.length_squared()).sqrt();
        if l <= EPSILON {
            0.0
        } else {
            (d / l).clamp(-1.0, 1.0).acos()
        }
    }
    /// 投影到 `onto` 上。
    #[inline]
    pub fn project_onto(self, onto: Self) -> Self {
        let d = onto.dot(onto);
        if d > EPSILON {
            onto * (self.dot(onto) / d)
        } else {
            Self::ZERO
        }
    }
    /// 去掉 `onto` 方向后的正交分量。
    #[inline]
    pub fn reject_from(self, onto: Self) -> Self {
        self - self.project_onto(onto)
    }
    /// 相对法线的反射。
    #[inline]
    pub fn reflect(self, normal: Self) -> Self {
        self - normal * (2.0 * self.dot(normal))
    }
    /// 球面线性插值（用于方向向量）。
    #[inline]
    pub fn slerp(self, o: Self, t: f32) -> Self {
        let a = self.try_normalize().unwrap_or(Self::ZERO);
        let b = o.try_normalize().unwrap_or(Self::ZERO);
        let dot = a.dot(b).clamp(-1.0, 1.0);
        let theta = dot.acos();
        if theta.abs() < EPSILON {
            return a.lerp(b, t).normalize_or_zero();
        }
        let s = theta.sin();
        (a * ((1.0 - t) * theta).sin() / s) + (b * (t * theta).sin() / s)
    }
    /// 给定任意单位向量，返回与它正交的一对单位向量。
    ///
    /// 物理接触流形、法线基、TBN 构建都要用（Duff 等人的分支无关方法）。
    #[inline]
    pub fn any_orthonormal_pair(self) -> (Self, Self) {
        let sign = (1.0f32).copysign(self.z);
        let a = -1.0 / (sign + self.z);
        let b = self.x * self.y * a;
        (
            Self::new(1.0 + sign * self.x * self.x * a, sign * b, -sign * self.x),
            Self::new(b, sign + self.y * self.y * a, -self.y),
        )
    }
    /// 忽略 `y` 的水平长度（常用于地面移动）。
    #[inline]
    pub fn horizontal_length(self) -> f32 {
        (self.x * self.x + self.z * self.z).sqrt()
    }
    /// 扩展为 `Vec4`。
    #[inline]
    pub const fn extend(self, w: f32) -> Vec4 {
        Vec4::new(self.x, self.y, self.z, w)
    }
    /// 转为数组。
    #[inline]
    pub const fn to_array(self) -> [f32; 3] {
        [self.x, self.y, self.z]
    }
    /// 从数组构造。
    #[inline]
    pub const fn from_array(a: [f32; 3]) -> Self {
        Self::new(a[0], a[1], a[2])
    }
    /// 是否有限。
    #[inline]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
    /// 是否存在 NaN。
    #[inline]
    pub fn is_nan(self) -> bool {
        self.x.is_nan() || self.y.is_nan() || self.z.is_nan()
    }
    /// 最小分量。
    #[inline]
    pub fn min_element(self) -> f32 {
        self.x.min(self.y).min(self.z)
    }
    /// 最大分量。
    #[inline]
    pub fn max_element(self) -> f32 {
        self.x.max(self.y).max(self.z)
    }
    /// 分量和。
    #[inline]
    pub fn sum(self) -> f32 {
        self.x + self.y + self.z
    }
}

impl Index<usize> for Vec3 {
    type Output = f32;
    #[inline]
    fn index(&self, i: usize) -> &f32 {
        match i {
            0 => &self.x,
            1 => &self.y,
            2 => &self.z,
            _ => panic!("Vec3 索引越界: {i}"),
        }
    }
}

impl IndexMut<usize> for Vec3 {
    #[inline]
    fn index_mut(&mut self, i: usize) -> &mut f32 {
        match i {
            0 => &mut self.x,
            1 => &mut self.y,
            2 => &mut self.z,
            _ => panic!("Vec3 索引越界: {i}"),
        }
    }
}

impl From<[f32; 3]> for Vec3 {
    #[inline]
    fn from(a: [f32; 3]) -> Self {
        Self::from_array(a)
    }
}

impl From<Vec3> for [f32; 3] {
    #[inline]
    fn from(v: Vec3) -> Self {
        v.to_array()
    }
}

impl From<(f32, f32, f32)> for Vec3 {
    #[inline]
    fn from(t: (f32, f32, f32)) -> Self {
        Self::new(t.0, t.1, t.2)
    }
}

impl fmt::Display for Vec3 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {}, {})", self.x, self.y, self.z)
    }
}

// ---------------------------------------------------------------------------
// Vec4
// ---------------------------------------------------------------------------

/// 四维向量（齐次坐标 / RGBA / 平面方程都用它）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "bytemuck", derive(bytemuck::Pod, bytemuck::Zeroable))]
#[repr(C)]
pub struct Vec4 {
    /// X 分量。
    pub x: f32,
    /// Y 分量。
    pub y: f32,
    /// Z 分量。
    pub z: f32,
    /// W 分量。
    pub w: f32,
}

impl_vec_ops!(Vec4, x, y, z, w);

impl Vec4 {
    /// 零向量。
    pub const ZERO: Self = Self::new(0.0, 0.0, 0.0, 0.0);
    /// 全 1 向量。
    pub const ONE: Self = Self::new(1.0, 1.0, 1.0, 1.0);

    /// 构造。
    #[inline]
    pub const fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self { x, y, z, w }
    }
    /// 所有分量取同一值。
    #[inline]
    pub const fn splat(v: f32) -> Self {
        Self::new(v, v, v, v)
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
    /// 归一化；零向量返回零向量。
    #[inline]
    pub fn normalize_or_zero(self) -> Self {
        let l = self.length();
        if l > EPSILON {
            self / l
        } else {
            Self::ZERO
        }
    }
    /// 取前三个分量。
    #[inline]
    pub const fn truncate(self) -> Vec3 {
        Vec3::new(self.x, self.y, self.z)
    }
    /// 线性插值。
    #[inline]
    pub fn lerp(self, o: Self, t: f32) -> Self {
        self + (o - self) * t
    }
    /// 转为数组。
    #[inline]
    pub const fn to_array(self) -> [f32; 4] {
        [self.x, self.y, self.z, self.w]
    }
    /// 从数组构造。
    #[inline]
    pub const fn from_array(a: [f32; 4]) -> Self {
        Self::new(a[0], a[1], a[2], a[3])
    }
}

impl Index<usize> for Vec4 {
    type Output = f32;
    #[inline]
    fn index(&self, i: usize) -> &f32 {
        match i {
            0 => &self.x,
            1 => &self.y,
            2 => &self.z,
            3 => &self.w,
            _ => panic!("Vec4 索引越界: {i}"),
        }
    }
}

impl IndexMut<usize> for Vec4 {
    #[inline]
    fn index_mut(&mut self, i: usize) -> &mut f32 {
        match i {
            0 => &mut self.x,
            1 => &mut self.y,
            2 => &mut self.z,
            3 => &mut self.w,
            _ => panic!("Vec4 索引越界: {i}"),
        }
    }
}

impl From<Vec3> for Vec4 {
    #[inline]
    fn from(v: Vec3) -> Self {
        v.extend(0.0)
    }
}

impl From<[f32; 4]> for Vec4 {
    #[inline]
    fn from(a: [f32; 4]) -> Self {
        Self::from_array(a)
    }
}

impl From<Vec4> for [f32; 4] {
    #[inline]
    fn from(v: Vec4) -> Self {
        v.to_array()
    }
}

impl fmt::Display for Vec4 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {}, {}, {})", self.x, self.y, self.z, self.w)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_vec3() {
        let a = Vec3::new(1.0, 2.0, 3.0);
        let b = Vec3::new(4.0, 5.0, 6.0);
        assert_eq!(a + b, Vec3::new(5.0, 7.0, 9.0));
        assert_eq!(a.dot(b), 32.0);
        assert_eq!(Vec3::X.cross(Vec3::Y), Vec3::Z);
        assert_eq!(b - a, Vec3::new(3.0, 3.0, 3.0));
        assert!((a.length() - 14.0f32.sqrt()).abs() < 1e-6);
        assert_eq!(a * 2.0, Vec3::new(2.0, 4.0, 6.0));
        assert_eq!(2.0 * a, Vec3::new(2.0, 4.0, 6.0));
    }

    #[test]
    fn normalize_zero_is_safe() {
        assert_eq!(Vec3::ZERO.normalize_or_zero(), Vec3::ZERO);
        assert!(Vec3::new(3.0, 0.0, 4.0).normalize_or_zero().is_normalized());
    }

    #[test]
    fn orthonormal_pair_is_orthogonal() {
        for n in [
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.1, -0.7, 0.3).normalize_or_zero(),
            Vec3::new(-1.0, 0.0, 0.0),
        ] {
            let (t, b) = n.any_orthonormal_pair();
            assert!(t.dot(n).abs() < 1e-4, "t·n 应为 0，实际 {}", t.dot(n));
            assert!(b.dot(n).abs() < 1e-4, "b·n 应为 0，实际 {}", b.dot(n));
            assert!((t.length() - 1.0).abs() < 1e-4);
            assert!((b.length() - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn reflect_and_project() {
        let v = Vec3::new(1.0, -1.0, 0.0);
        assert_eq!(v.reflect(Vec3::Y), Vec3::new(1.0, 1.0, 0.0));
        assert_eq!(
            Vec3::new(3.0, 4.0, 0.0).project_onto(Vec3::X),
            Vec3::new(3.0, 0.0, 0.0)
        );
    }
}
