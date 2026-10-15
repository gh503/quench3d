//! Quench3D 数学库 —— 零依赖的 `f32` 线性代数。
//!
//! 设计目标：
//! * **独立自持**：不依赖 `glam` / `nalgebra`，引擎的数学基础完全自研。
//! * **贴合 wgpu**：投影矩阵默认使用 `[0, 1]` 深度区间（wgpu / D3D 约定）。
//! * **列主序**：`Mat4` 就是 GPU 常量缓冲可直接使用的 16 个连续 `f32`。
//!
//! ```
//! use quench3d_math::*;
//! let t = Transform::from_translation(vec3(1.0, 2.0, 3.0));
//! let m = t.matrix();
//! assert_eq!(m.transform_point3(Vec3::ZERO), vec3(1.0, 2.0, 3.0));
//! ```

#![forbid(unsafe_code)]

mod aabb;
mod geom;
mod mat;
mod quat;
mod transform;
mod vec;

pub use aabb::Aabb;
pub use geom::{Plane, Ray};
pub use mat::{Mat3, Mat4};
pub use quat::Quat;
pub use transform::Transform;
pub use vec::{Vec2, Vec3, Vec4};

/// 浮点比较与几何判定使用的容差。
pub const EPSILON: f32 = 1e-6;
/// 更宽松的容差，用于退化情形判定。
pub const EPSILON_LOOSE: f32 = 1e-4;

/// 角度（弧度）。显式类型可避免角度/弧度混用。
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct Rad(pub f32);

/// 角度（度）。
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct Deg(pub f32);

impl Rad {
    /// 转换为度。
    #[inline]
    pub fn to_degrees(self) -> Deg {
        Deg(self.0.to_degrees())
    }
}

impl Deg {
    /// 转换为弧度。
    #[inline]
    pub fn to_radians(self) -> Rad {
        Rad(self.0.to_radians())
    }
}

impl From<Deg> for Rad {
    #[inline]
    fn from(d: Deg) -> Self {
        d.to_radians()
    }
}

impl From<Rad> for Deg {
    #[inline]
    fn from(r: Rad) -> Self {
        r.to_degrees()
    }
}

/// 便捷构造：弧度。
#[inline]
pub const fn rad(v: f32) -> Rad {
    Rad(v)
}

/// 便捷构造：角度。
#[inline]
pub const fn deg(v: f32) -> Deg {
    Deg(v)
}

/// 构造 `Vec2` 的简写。
#[inline]
pub const fn vec2(x: f32, y: f32) -> Vec2 {
    Vec2::new(x, y)
}

/// 构造 `Vec3` 的简写。
#[inline]
pub const fn vec3(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3::new(x, y, z)
}

/// 构造 `Vec4` 的简写。
#[inline]
pub const fn vec4(x: f32, y: f32, z: f32, w: f32) -> Vec4 {
    Vec4::new(x, y, z, w)
}

/// 近似相等（绝对容差）。
#[inline]
pub fn approx_eq(a: f32, b: f32) -> bool {
    (a - b).abs() <= EPSILON
}

/// 近似相等（相对 + 绝对容差），适合比较量级差别很大的值。
#[inline]
pub fn approx_eq_rel(a: f32, b: f32) -> bool {
    (a - b).abs() <= EPSILON_LOOSE * a.abs().max(b.abs()).max(1.0)
}

/// 把 `v` 限制在 `[min, max]`。
#[inline]
pub fn clamp(v: f32, min: f32, max: f32) -> f32 {
    if v < min {
        min
    } else if v > max {
        max
    } else {
        v
    }
}

/// 线性插值（不做夹紧）。
#[inline]
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// 平滑阶跃，常用于插值与淡入淡出。
#[inline]
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = clamp((x - edge0) / (edge1 - edge0), 0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// 返回符号，`0.0` 保持为 `0.0`。
#[inline]
pub fn sign_or_zero(v: f32) -> f32 {
    if v > 0.0 {
        1.0
    } else if v < 0.0 {
        -1.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angle_roundtrip() {
        assert!(approx_eq(deg(180.0).to_radians().0, core::f32::consts::PI));
        assert!(approx_eq(rad(core::f32::consts::PI).to_degrees().0, 180.0));
    }
}
