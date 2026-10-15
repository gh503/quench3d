//! 颜色空间辅助函数。

use quench3d_math::{Vec3, Vec4};

/// sRGB 分量 → 线性分量。
#[inline]
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// 线性分量 → sRGB 分量。
#[inline]
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// sRGB 颜色 → 线性 `Vec3`。
#[inline]
pub fn color3_srgb_to_linear(c: Vec3) -> Vec3 {
    Vec3::new(
        srgb_to_linear(c.x),
        srgb_to_linear(c.y),
        srgb_to_linear(c.z),
    )
}

/// sRGB 颜色（`0..255`）→ 线性 RGBA。
pub fn rgba8(r: u8, g: u8, b: u8, a: u8) -> Vec4 {
    Vec4::new(
        srgb_to_linear(r as f32 / 255.0),
        srgb_to_linear(g as f32 / 255.0),
        srgb_to_linear(b as f32 / 255.0),
        a as f32 / 255.0,
    )
}

/// sRGB 浮点颜色 → 线性 RGBA。
pub fn rgba(r: f32, g: f32, b: f32, a: f32) -> Vec4 {
    Vec4::new(srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b), a)
}

/// 十六进制颜色（`0xRRGGBB`）→ 线性 RGBA。
pub fn hex(value: u32) -> Vec4 {
    rgba8(
        ((value >> 16) & 0xFF) as u8,
        ((value >> 8) & 0xFF) as u8,
        (value & 0xFF) as u8,
        255,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for c in [0.0, 0.05, 0.25, 0.5, 0.75, 1.0] {
            assert!((linear_to_srgb(srgb_to_linear(c)) - c).abs() < 1e-5);
        }
    }

    #[test]
    fn hex_parsing() {
        let c = hex(0xFF0000);
        assert!((c.x - 1.0).abs() < 1e-5 && c.y.abs() < 1e-6 && c.z.abs() < 1e-6);
    }
}
