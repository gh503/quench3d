//! 光照与阴影配置：平行光（含阴影贴图拟合）、点光源、天空、雾。

use quench3d_math::{Aabb, Mat4, Vec3};

/// 平行光（太阳/主光）。
#[derive(Clone, Copy, Debug)]
pub struct DirectionalLight {
    /// 光照方向（从光源射向场景的单位向量）。
    pub direction: Vec3,
    /// 颜色（线性空间）。
    pub color: Vec3,
    /// 强度。
    pub intensity: f32,
    /// 是否投射阴影。
    pub cast_shadows: bool,
    /// 阴影相机沿光线方向覆盖的半径（越大覆盖越广、精度越低）。
    pub shadow_radius: f32,
    /// 阴影贴图分辨率（需在创建渲染器时确定，运行时修改需重建）。
    pub shadow_map_size: u32,
}

impl Default for DirectionalLight {
    fn default() -> Self {
        Self {
            // 默认太阳：从左后上方斜射，阴影朝镜头方向铺开（更容易看清）
            direction: Vec3::new(0.42, -0.78, 0.46).normalize_or_zero(),
            color: Vec3::new(1.0, 0.96, 0.9),
            intensity: 2.9,
            cast_shadows: true,
            shadow_radius: 28.0,
            shadow_map_size: 2048,
        }
    }
}

impl DirectionalLight {
    /// 指向光源的方向（着色器用）。
    #[inline]
    pub fn to_light(&self) -> Vec3 {
        -self.direction.normalize_or_zero()
    }

    /// 根据相机可见范围拟合阴影相机，返回 `(view_projection, 深度偏移)`。
    ///
    /// 采用"以相机目标点为中心、沿光线方向拉出正交投影"的经典做法，
    /// 光源环绕相机移动时阴影贴图不会抖动（对中心做了纹素对齐）。
    pub fn shadow_matrix(&self, center: Vec3, radius: f32, depth_range: f32) -> Mat4 {
        let dir = self.direction.normalize_or_zero();
        let up = if dir.y.abs() > 0.95 { Vec3::Z } else { Vec3::Y };
        let eye = center - dir * (radius + depth_range * 0.6);

        let view = Mat4::look_to_rh(eye, dir, up);
        // 在光源空间中求包围球的正交范围，避免旋转时阴影抖动。
        let w = radius.max(1.0);
        let h = radius.max(1.0);
        let proj = Mat4::orthographic_rh(-w, w, -h, h, 0.05, radius * 2.0 + depth_range);
        let m = proj * view;

        // 纹素对齐：把中心点投影到阴影贴图空间并吸附到纹素格点。
        let size = self.shadow_map_size.max(256) as f32;
        let mut m = m;
        let origin = m.transform_point3(center);
        let texel = 2.0 / size;
        let dx = (origin.x / texel).round() * texel - origin.x;
        let dy = (origin.y / texel).round() * texel - origin.y;
        m.cols[3].x += dx;
        m.cols[3].y += dy;
        m
    }

    /// 根据场景包围盒确定阴影半径。
    pub fn fitted_radius(&self, bounds: &Aabb) -> f32 {
        bounds.half_extents().length().clamp(5.0, 2000.0)
    }
}

/// 点光源（最多 [`MAX_POINT_LIGHTS`] 个参与着色）。
#[derive(Clone, Copy, Debug)]
pub struct PointLight {
    /// 世界坐标。
    pub position: Vec3,
    /// 颜色（线性空间）。
    pub color: Vec3,
    /// 强度。
    pub intensity: f32,
    /// 影响半径（在此距离衰减到 0）。
    pub radius: f32,
}

impl PointLight {
    /// 便捷构造。
    pub fn new(position: Vec3, color: Vec3, intensity: f32, radius: f32) -> Self {
        Self {
            position,
            color,
            intensity,
            radius,
        }
    }
}

/// 着色器支持的最大点光源数量（与 `common.wgsl` 中的数组长度一致）。
pub const MAX_POINT_LIGHTS: usize = 8;

/// 天空（程序化渐变）。
#[derive(Clone, Copy, Debug)]
pub struct SkyParams {
    /// 天顶颜色。
    pub zenith: Vec3,
    /// 地平线颜色。
    pub horizon: Vec3,
    /// 地平线以下颜色。
    pub ground: Vec3,
    /// 是否绘制天空。
    pub enabled: bool,
    /// 地面网格高度（`grid_y`）。
    pub grid_height: f32,
    /// 是否绘制无限地面网格。
    pub show_grid: bool,
}

impl Default for SkyParams {
    fn default() -> Self {
        Self {
            zenith: Vec3::new(0.085, 0.19, 0.44),
            horizon: Vec3::new(0.36, 0.46, 0.63),
            ground: Vec3::new(0.055, 0.06, 0.075),
            enabled: true,
            grid_height: 0.0,
            show_grid: true,
        }
    }
}

/// 指数距离雾。
#[derive(Clone, Copy, Debug)]
pub struct FogParams {
    /// 雾颜色。
    pub color: Vec3,
    /// 密度（0 表示关闭）。
    pub density: f32,
}

impl Default for FogParams {
    fn default() -> Self {
        Self {
            color: Vec3::new(0.36, 0.46, 0.63),
            density: 0.0,
        }
    }
}

impl FogParams {
    /// 开启一定强度的雾。
    pub fn with_density(mut self, d: f32) -> Self {
        self.density = d.max(0.0);
        self
    }
}

/// 一帧的完整光照配置。
#[derive(Clone, Debug)]
pub struct Lighting {
    /// 主平行光（`None` 表示没有主光）。
    pub sun: Option<DirectionalLight>,
    /// 环境光颜色与强度。
    pub ambient_color: Vec3,
    /// 环境光强度。
    pub ambient_intensity: f32,
    /// 点光源列表（超出上限的部分被忽略）。
    pub point_lights: Vec<PointLight>,
    /// 天空参数。
    pub sky: SkyParams,
    /// 雾参数。
    pub fog: FogParams,
    /// 曝光（色调映射前的线性缩放）。
    pub exposure: f32,
    /// 阴影强度（0 = 无阴影，1 = 全黑阴影）。
    pub shadow_strength: f32,
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            sun: Some(DirectionalLight::default()),
            ambient_color: Vec3::new(0.40, 0.47, 0.62),
            ambient_intensity: 0.40,
            point_lights: Vec::new(),
            sky: SkyParams::default(),
            fog: FogParams::default(),
            exposure: 1.0,
            shadow_strength: 0.85,
        }
    }
}

impl Lighting {
    /// 摄影棚风格（三点布光：主光 + 两侧补光）。
    pub fn studio() -> Self {
        Self {
            ambient_intensity: 0.38,
            point_lights: vec![
                PointLight::new(
                    Vec3::new(-4.0, 5.0, 4.0),
                    Vec3::new(1.0, 0.92, 0.85),
                    12.0,
                    18.0,
                ),
                PointLight::new(
                    Vec3::new(5.0, 4.0, -3.0),
                    Vec3::new(0.75, 0.85, 1.0),
                    9.0,
                    20.0,
                ),
            ],
            ..Self::default()
        }
    }

    /// 白天户外：强太阳（略暖）+ 淡蓝天光 + 无补光。
    ///
    /// 与 [`Self::studio`] 的区别是**没有点光源**，照明完全来自太阳与天空，
    /// 适合大场景、地形、需要真实阴影方向的预览。
    pub fn day() -> Self {
        let d = Self::default();
        Self {
            sun: Some(DirectionalLight {
                direction: Vec3::new(0.35, -1.0, 0.42).normalize_or_zero(),
                color: Vec3::new(1.0, 0.97, 0.90),
                intensity: 3.0,
                ..d.sun.unwrap_or_default()
            }),
            ambient_color: Vec3::new(0.45, 0.55, 0.70),
            ambient_intensity: 0.55,
            point_lights: Vec::new(),
            sky: SkyParams {
                zenith: Vec3::new(0.22, 0.42, 0.78),
                horizon: Vec3::new(0.62, 0.74, 0.88),
                ground: Vec3::new(0.24, 0.26, 0.24),
                ..d.sky
            },
            ..d
        }
    }

    /// 夜晚 / 太空风格：弱环境光 + 冷色天空 + 关闭太阳。
    pub fn night() -> Self {
        let d = Self::default();
        Self {
            sun: None,
            ambient_color: Vec3::new(0.10, 0.13, 0.22),
            ambient_intensity: 0.35,
            sky: SkyParams {
                zenith: Vec3::new(0.02, 0.03, 0.08),
                horizon: Vec3::new(0.06, 0.08, 0.16),
                ground: Vec3::new(0.01, 0.01, 0.02),
                ..d.sky
            },
            fog: FogParams {
                color: Vec3::new(0.05, 0.07, 0.13),
                ..d.fog
            },
            ..d
        }
    }

    /// 无光照：没有主光、环境光也为 0（只剩自发光与点光源；物体通常是全黑的）。
    ///
    /// 与 [`Self::night`] 的区别：夜晚还有 0.35 的环境光，这里是彻底不照亮。
    pub fn none() -> Self {
        Self {
            sun: None,
            ambient_intensity: 0.0,
            point_lights: Vec::new(),
            ..Self::night()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quench3d_math::vec3;

    #[test]
    fn shadow_matrix_maps_center_into_unit_cube() {
        let light = DirectionalLight {
            shadow_map_size: 2048,
            shadow_radius: 30.0,
            ..Default::default()
        };
        let center = vec3(3.0, 0.0, -2.0);
        let m = light.shadow_matrix(center, 30.0, 60.0);
        let c = m.transform_point3(center);
        assert!(
            c.x.abs() <= 1.0 && c.y.abs() <= 1.0 && (0.0..=1.0).contains(&c.z),
            "中心点应落在阴影裁剪空间内: {c:?}"
        );
    }

    #[test]
    fn shadow_matrix_covers_scene_radius() {
        let light = DirectionalLight {
            shadow_map_size: 2048,
            shadow_radius: 20.0,
            ..Default::default()
        };
        let center = Vec3::ZERO;
        let m = light.shadow_matrix(center, 20.0, 40.0);
        for p in [
            vec3(19.0, 0.0, 0.0),
            vec3(-19.0, 0.0, 0.0),
            vec3(0.0, 0.0, 19.0),
            vec3(0.0, 0.0, -19.0),
            vec3(0.0, 10.0, 0.0),
        ] {
            let c = m.transform_point3(p);
            assert!(
                c.x.abs() <= 1.01 && c.y.abs() <= 1.01 && (-0.01..=1.01).contains(&c.z),
                "{p:?} 未被阴影相机覆盖: {c:?}"
            );
        }
    }

    #[test]
    fn to_light_is_opposite_of_direction() {
        let l = DirectionalLight::default();
        assert!((l.to_light() + l.direction.normalize_or_zero()).length() < 1e-5);
    }

    #[test]
    fn presets_are_distinct_and_none_really_means_no_light() {
        let studio = Lighting::studio();
        let day = Lighting::day();
        let night = Lighting::night();
        let none = Lighting::none();

        // studio 有太阳 + 两盏补光；day 只有太阳；night 没有太阳
        assert!(studio.sun.is_some() && studio.point_lights.len() == 2);
        assert!(day.sun.is_some() && day.point_lights.is_empty());
        assert!(night.sun.is_none());
        // none 是 night 的强化版：环境光归零、没有点光源
        assert!(none.sun.is_none());
        assert_eq!(none.ambient_intensity, 0.0);
        assert!(none.point_lights.is_empty());
        assert!(
            night.ambient_intensity > 0.0,
            "night 还有微弱环境光，与 none 必须区分开"
        );
    }
}
