//! 材质系统：PBR 参数 + 可选贴图，编译为单独的 GPU 绑定组。

use bytemuck::{Pod, Zeroable};
use quench3d_math::{Vec3, Vec4};

/// 纹理句柄。
pub type TextureId = u32;
/// 材质句柄。
pub type MaterialId = u32;
/// 网格句柄。
pub type MeshId = u32;

/// 材质标志位（与 `common.wgsl` 中的 `FLAG_*` 保持一致）。
pub mod flags {
    /// 使用基础色贴图。
    pub const BASE_COLOR_TEX: u32 = 1;
    /// 使用法线贴图。
    pub const NORMAL_TEX: u32 = 2;
    /// 使用金属度/粗糙度贴图。
    pub const MR_TEX: u32 = 4;
    /// 使用自发光贴图。
    pub const EMISSIVE_TEX: u32 = 8;
    /// 无光照（纯色/自发光卡通外观）。
    pub const UNLIT: u32 = 16;
    /// 使用环境光遮蔽（AO）贴图。
    pub const AO_TEX: u32 = 32;
}

/// GPU 材质常量（与 `common.wgsl` 的 `Material` 布局一致）。
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct MaterialUniform {
    /// 基础色 * alpha。
    pub base_color_factor: [f32; 4],
    /// rgb = 自发光颜色，a = 强度。
    pub emissive_factor: [f32; 4],
    /// x = 金属度，y = 粗糙度，z = UV 缩放，w = 标志位。
    pub params: [f32; 4],
    /// x = alpha 裁剪阈值，y = 法线强度，z = 环境光遮蔽强度，w = 备用。
    pub params2: [f32; 4],
    /// xy = UV 偏移，z = 旋转 cos，w = 旋转 sin。
    pub uv_transform: [f32; 4],
}

/// 材质描述（创建后不可变，修改请重新创建）。
#[derive(Clone, Debug, PartialEq)]
pub struct MaterialDesc {
    /// 基础色（线性空间，alpha 为透明度）。
    pub base_color: Vec4,
    /// 金属度 `[0, 1]`。
    pub metallic: f32,
    /// 粗糙度 `[0, 1]`。
    pub roughness: f32,
    /// 自发光颜色（线性空间）。
    pub emissive: Vec3,
    /// 自发光强度。
    pub emissive_strength: f32,
    /// UV 缩放（平铺次数）。
    pub uv_scale: f32,
    /// UV 偏移（贴图坐标，0.5 = 半个贴图）。
    pub uv_offset: [f32; 2],
    /// UV 旋转（弧度，绕 (0.5, 0.5)）。
    pub uv_rotation: f32,
    /// 基础色贴图。
    pub base_color_texture: Option<TextureId>,
    /// 法线贴图。
    pub normal_texture: Option<TextureId>,
    /// 金属度-粗糙度贴图（G = 粗糙度，B = 金属度）。
    pub metallic_roughness_texture: Option<TextureId>,
    /// 自发光贴图。
    pub emissive_texture: Option<TextureId>,
    /// 环境光遮蔽（AO）贴图，只采样 R 通道。
    pub ao_texture: Option<TextureId>,
    /// AO 强度（0 = 不压暗，1 = 全额；默认 1）。
    pub ao_strength: f32,
    /// 法线强度。
    pub normal_scale: f32,
    /// alpha 裁剪阈值（> 0 时启用裁剪，0 表示不裁剪）。
    pub alpha_cutoff: f32,
    /// 强制走 alpha 混合管线。
    ///
    /// 贴图自带 alpha（glTF `alphaMode: BLEND`）时必须显式置位：只看 `base_color.w`
    /// 会把这类材质误判成不透明——贴图 alpha 要采样后才出现，而 `base_color` 因子通常是 1.0。
    pub alpha_blend: bool,
    /// 无光照。
    pub unlit: bool,
    /// 双面渲染。
    pub double_sided: bool,
    /// 写入水面 mask（主 pass 的第二颜色附件，供屏幕空间水后处理使用）。
    ///
    /// 只有水面平面该置位；它不影响颜色，只在 mask 附件里写 1。
    pub water_mask: bool,
}

impl Default for MaterialDesc {
    fn default() -> Self {
        Self {
            base_color: Vec4::new(0.8, 0.8, 0.82, 1.0),
            metallic: 0.0,
            roughness: 0.5,
            emissive: Vec3::ZERO,
            emissive_strength: 1.0,
            uv_scale: 1.0,
            uv_offset: [0.0, 0.0],
            uv_rotation: 0.0,
            base_color_texture: None,
            normal_texture: None,
            metallic_roughness_texture: None,
            emissive_texture: None,
            ao_texture: None,
            ao_strength: 1.0,
            normal_scale: 1.0,
            alpha_cutoff: 0.0,
            alpha_blend: false,
            unlit: false,
            double_sided: false,
            water_mask: false,
        }
    }
}

impl MaterialDesc {
    /// 由线性空间颜色构造标准 PBR 材质。
    pub fn new(base_color: Vec4) -> Self {
        Self {
            base_color,
            ..Default::default()
        }
    }

    /// 由 sRGB 颜色构造（写代码时更直观）。
    pub fn from_srgb(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self::new(Vec4::new(
            crate::color::srgb_to_linear(r),
            crate::color::srgb_to_linear(g),
            crate::color::srgb_to_linear(b),
            a,
        ))
    }

    /// 金属/粗糙度。
    pub fn pbr(mut self, metallic: f32, roughness: f32) -> Self {
        self.metallic = metallic;
        self.roughness = roughness;
        self
    }

    /// 自发光。
    pub fn emissive(mut self, color: Vec3, strength: f32) -> Self {
        self.emissive = color;
        self.emissive_strength = strength;
        self
    }

    /// 基础色贴图。
    pub fn with_base_color_texture(mut self, id: TextureId) -> Self {
        self.base_color_texture = Some(id);
        self
    }

    /// 法线贴图。
    pub fn with_normal_texture(mut self, id: TextureId) -> Self {
        self.normal_texture = Some(id);
        self
    }

    /// 金属度-粗糙度贴图。
    pub fn with_metallic_roughness_texture(mut self, id: TextureId) -> Self {
        self.metallic_roughness_texture = Some(id);
        self
    }

    /// UV 平铺。
    pub fn with_uv_scale(mut self, scale: f32) -> Self {
        self.uv_scale = scale;
        self
    }

    /// AO 贴图（只采样 R 通道，乘到环境光/IBL 上）。
    pub fn with_ao_texture(mut self, id: TextureId) -> Self {
        self.ao_texture = Some(id);
        self
    }

    /// AO 强度（0..=1，默认 1）。
    pub fn with_ao_strength(mut self, strength: f32) -> Self {
        self.ao_strength = strength;
        self
    }

    /// 无光照。
    pub fn unlit(mut self) -> Self {
        self.unlit = true;
        self
    }

    /// 双面。
    pub fn double_sided(mut self) -> Self {
        self.double_sided = true;
        self
    }

    /// alpha 裁剪。
    pub fn alpha_cutout(mut self, cutoff: f32) -> Self {
        self.alpha_cutoff = cutoff;
        self
    }

    /// alpha 混合（贴图自带 alpha，或颜色 alpha < 1）。
    pub fn alpha_blend(mut self) -> Self {
        self.alpha_blend = true;
        self
    }

    /// 常用预设：金属。
    pub fn metal(color: Vec3, roughness: f32) -> Self {
        Self::new(color.extend(1.0)).pbr(1.0, roughness)
    }

    /// 常用预设：塑料/涂漆。
    pub fn plastic(color: Vec3, roughness: f32) -> Self {
        Self::new(color.extend(1.0)).pbr(0.0, roughness)
    }

    /// 计算标志位。
    pub fn flag_bits(&self) -> u32 {
        let mut f = 0;
        if self.base_color_texture.is_some() {
            f |= flags::BASE_COLOR_TEX;
        }
        if self.normal_texture.is_some() {
            f |= flags::NORMAL_TEX;
        }
        if self.metallic_roughness_texture.is_some() {
            f |= flags::MR_TEX;
        }
        if self.emissive_texture.is_some() {
            f |= flags::EMISSIVE_TEX;
        }
        if self.ao_texture.is_some() {
            f |= flags::AO_TEX;
        }
        if self.unlit {
            f |= flags::UNLIT;
        }
        f
    }

    /// 转换为 GPU 常量。
    pub fn to_uniform(&self) -> MaterialUniform {
        MaterialUniform {
            base_color_factor: self.base_color.to_array(),
            emissive_factor: [
                self.emissive.x,
                self.emissive.y,
                self.emissive.z,
                self.emissive_strength,
            ],
            params: [
                self.metallic,
                self.roughness,
                self.uv_scale.max(1e-4),
                self.flag_bits() as f32,
            ],
            params2: [
                self.alpha_cutoff,
                self.normal_scale,
                self.ao_strength.clamp(0.0, 1.0),
                if self.water_mask { 1.0 } else { 0.0 },
            ],
            uv_transform: [
                self.uv_offset[0],
                self.uv_offset[1],
                self.uv_rotation.cos(),
                self.uv_rotation.sin(),
            ],
        }
    }

    /// 是否需要 alpha 混合管线。
    ///
    /// 两个来源：显式置位（贴图 alpha）或颜色 alpha < 1（手动构造的材质）。
    /// 裁剪（cutout）优先于混合：有裁剪阈值时走不透明管线 + 着色器 discard。
    pub fn needs_alpha_blend(&self) -> bool {
        (self.alpha_blend || self.base_color.w < 0.999) && self.alpha_cutoff <= 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_conversion_is_reasonable() {
        assert!((crate::color::srgb_to_linear(0.0)).abs() < 1e-6);
        assert!((crate::color::srgb_to_linear(1.0) - 1.0).abs() < 1e-6);
        // 中灰 sRGB 0.5 → 线性约 0.214
        assert!((crate::color::srgb_to_linear(0.5) - 0.2140).abs() < 0.01);
    }

    #[test]
    fn flags_reflect_textures() {
        let m = MaterialDesc::default();
        assert_eq!(m.flag_bits(), 0);
        let m = m.with_base_color_texture(0).unlit().double_sided();
        assert_eq!(m.flag_bits(), flags::BASE_COLOR_TEX | flags::UNLIT);
        assert!(m.double_sided);
    }

    #[test]
    fn uniform_layout_is_16_byte_aligned() {
        assert_eq!(std::mem::size_of::<MaterialUniform>() % 16, 0);
        assert_eq!(std::mem::size_of::<MaterialUniform>(), 80);
    }

    #[test]
    fn uv_transform_is_carried_as_offset_and_rotation() {
        let m = MaterialDesc {
            uv_offset: [0.25, -0.5],
            uv_rotation: std::f32::consts::FRAC_PI_2,
            ..MaterialDesc::default()
        };
        let u = m.to_uniform().uv_transform;
        assert_eq!(u[0], 0.25);
        assert_eq!(u[1], -0.5);
        assert!(u[2].abs() < 1e-6, "cos(90°) 应约为 0：{}", u[2]);
        assert!((u[3] - 1.0).abs() < 1e-6, "sin(90°) 应约为 1：{}", u[3]);
    }

    #[test]
    fn ao_texture_sets_flag_and_strength_reaches_uniform() {
        let m = MaterialDesc {
            ao_texture: Some(3),
            ao_strength: 0.35,
            ..MaterialDesc::default()
        };
        let u = m.to_uniform();
        assert_eq!(
            m.flag_bits() & flags::AO_TEX,
            flags::AO_TEX,
            "有 AO 贴图时必须置 AO 标志"
        );
        assert!((u.params2[2] - 0.35).abs() < 1e-6, "AO 强度没进 uniform");
        let plain = MaterialDesc::default();
        assert_eq!(plain.flag_bits() & flags::AO_TEX, 0);
        assert!((plain.to_uniform().params2[2] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn water_mask_is_carried_in_params2_w() {
        assert_eq!(MaterialDesc::default().to_uniform().params2[3], 0.0);
        let m = MaterialDesc {
            water_mask: true,
            ..MaterialDesc::default()
        };
        assert_eq!(m.to_uniform().params2[3], 1.0);
    }

    #[test]
    fn alpha_blend_is_decided_by_flag_or_color_alpha() {
        // 贴图自带 alpha：颜色因子是 1.0，但必须走混合管线（否则整块不透明）
        let textured = MaterialDesc::default().alpha_blend();
        assert_eq!(textured.base_color.w, 1.0);
        assert!(textured.needs_alpha_blend(), "贴图 alpha 必须触发混合");

        // 手动构造的半透明颜色：沿用旧行为
        let mut manual = MaterialDesc::default();
        manual.base_color.w = 0.4;
        assert!(manual.needs_alpha_blend());

        // 裁剪优先于混合
        assert!(!MaterialDesc::default()
            .alpha_blend()
            .alpha_cutout(0.5)
            .needs_alpha_blend());

        // 完全不透明：不走混合
        assert!(!MaterialDesc::default().needs_alpha_blend());
    }

    #[test]
    fn alpha_detection() {
        let opaque = MaterialDesc::from_srgb(1.0, 0.0, 0.0, 1.0);
        assert!(!opaque.needs_alpha_blend());
        let transparent = MaterialDesc::from_srgb(1.0, 0.0, 0.0, 0.5);
        assert!(transparent.needs_alpha_blend());
        let cutout = transparent.clone().alpha_cutout(0.5);
        assert!(!cutout.needs_alpha_blend());
    }
}
