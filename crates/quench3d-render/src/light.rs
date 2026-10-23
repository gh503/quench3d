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
    /// 阴影深度偏移（单位：阴影贴图纹素，缺省 0.75）。
    ///
    /// 太小会出现自阴影麻点（shadow acne），太大会让接触处的阴影脱离物体
    /// （peter-panning）。只在 `cast_shadows` 为真时有意义。
    pub shadow_bias: f32,
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
            shadow_bias: 0.75,
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
        self.shadow_matrix_with_map_size(
            center,
            radius,
            depth_range,
            self.shadow_map_size.max(256) as f32,
        )
    }

    /// 与 [`DirectionalLight::shadow_matrix`] 相同，但**纹素对齐用的贴图尺寸可指定**。
    ///
    /// 级联阴影把 N 级画进同一张深度图的 2x2 分块（见 [`crate::cascade`]），每一级实际只有
    /// `贴图尺寸 / 2` 个纹素；纹素对齐必须按**分块**算，否则每一级的阴影都会随相机抖动。
    pub fn shadow_matrix_with_map_size(
        &self,
        center: Vec3,
        radius: f32,
        depth_range: f32,
        map_size_px: f32,
    ) -> Mat4 {
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
        let size = map_size_px.max(256.0);
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
///
/// **投影**：`cast_shadows` 缺省 `false`（老场景逐像素不变）。整帧**只支持一盏**投影
/// 点光源——选择规则见 [`crate::point_shadow::pick_point_shadow`]："第一盏
/// `cast_shadows == true` 的点光源"（点光源排在局部光名额最前面，所以只有排在第
/// [`MAX_POINT_LIGHTS`] 盏及以后的才会被名额挤掉，这时它既不照明也不投影）。
/// 用的是 6 个 90° 面的深度数组（立方体阴影），与聚光灯那张 2D 图是**两套独立资源**，
/// 所以"一盏投影点光源 + 一盏投影聚光灯"可以同时存在。
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
    /// 是否投射阴影（缺省 false = 与加这个功能之前逐像素相同）。
    pub cast_shadows: bool,
}

/// 聚光灯：位置 + 指向 + 一个圆锥（内锥全强度、外锥外无光）。
///
/// 与点光源**共享**每帧的局部光名额（[`MAX_POINT_LIGHTS`] 个，先放点光源、
/// 再放聚光灯，放不下的忽略）——所以聚光灯多的时候点光源会被挤掉，这是有意的：
/// 预算固定、行为可预测，比"有时亮有时不亮"更好查。
///
/// **投影**：`cast_shadows` 缺省 `false`（老场景逐像素不变）。整帧**只支持一盏**投影
/// 聚光灯——选择规则见 [`crate::spot_shadow::pick_spot_shadow`]：
/// "第一盏 `cast_shadows == true` 且没有被局部光名额挤掉的聚光灯"。
/// 其余聚光灯照常照明，只是不投影；点光源的投影是**另一套资源**（6 面深度数组，
/// 见 [`crate::point_shadow`]），所以两者可以各有且只有一盏。
#[derive(Clone, Copy, Debug)]
pub struct SpotLight {
    /// 世界坐标。
    pub position: Vec3,
    /// 照射方向（不需要自己归一化）。
    pub direction: Vec3,
    /// 颜色（线性空间）。
    pub color: Vec3,
    /// 强度。
    pub intensity: f32,
    /// 影响半径（米，在此距离衰减到 0）。
    pub radius: f32,
    /// 内锥角余弦（此角内为全强度）。
    pub inner_cos: f32,
    /// 外锥角余弦（此角外完全无光）。
    pub outer_cos: f32,
    /// 是否投射阴影（缺省 false = 与加这个功能之前逐像素相同）。
    pub cast_shadows: bool,
}

impl SpotLight {
    /// 从**角度**（度）构造：引擎内部只存余弦，避免每帧算三角函数。
    pub fn new(
        position: Vec3,
        direction: Vec3,
        color: Vec3,
        intensity: f32,
        radius: f32,
        inner_deg: f32,
        outer_deg: f32,
    ) -> Self {
        // 角度在场景校验里已经查过，这里只做兜底：夹到 (0, 90) 并保证 inner >= outer
        let clamp = |d: f32| d.clamp(0.01, 89.99);
        let inner = clamp(inner_deg).to_radians();
        let outer = clamp(outer_deg).to_radians().max(inner);
        Self {
            position,
            direction,
            color,
            intensity,
            radius,
            inner_cos: inner.cos(),
            outer_cos: outer.cos(),
            // 缺省不投影：加了 `cast_shadows` 这个字段之后，老场景的像素必须逐位不变
            cast_shadows: false,
        }
    }

    /// 打开/关闭投影（链式，给 `new` 之后用）。
    ///
    /// 为什么要有它：锥角编辑会整体重建一盏灯（引擎只存余弦，改角度只能重算），
    /// 直接 `*s = SpotLight::new(..)` 会把投影开关悄悄清掉。
    pub fn with_cast_shadows(mut self, cast_shadows: bool) -> Self {
        self.cast_shadows = cast_shadows;
        self
    }

    /// 单位方向（零向量时退化为向下照，避免 NaN）。
    pub fn dir(&self) -> Vec3 {
        if self.direction.length_squared() > 1e-12 {
            self.direction.normalize()
        } else {
            Vec3::new(0.0, -1.0, 0.0)
        }
    }
}

impl PointLight {
    /// 便捷构造。
    pub fn new(position: Vec3, color: Vec3, intensity: f32, radius: f32) -> Self {
        Self {
            position,
            color,
            intensity,
            radius,
            // 缺省不投影：加了 `cast_shadows` 之后，老场景的像素必须逐位不变
            cast_shadows: false,
        }
    }

    /// 打开/关闭投影（链式）。
    pub fn with_cast_shadows(mut self, cast_shadows: bool) -> Self {
        self.cast_shadows = cast_shadows;
        self
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
    /// 点光源列表（超出上限的部分被忽略；与聚光灯**共享**同一份预算）。
    pub point_lights: Vec<PointLight>,
    /// 聚光灯列表（排在点光源之后，放不下的忽略）。
    pub spot_lights: Vec<SpotLight>,
    /// 天空参数。
    pub sky: SkyParams,
    /// 雾参数。
    pub fog: FogParams,
    /// 曝光（色调映射前的线性缩放）。
    pub exposure: f32,
    /// 阴影强度（0 = 无阴影，1 = 全黑阴影）。
    pub shadow_strength: f32,
    /// IBL 环境贴图（CPU 副本；渲染器另有 GPU cubemap）。
    pub environment: Option<std::sync::Arc<crate::environment::EnvironmentMap>>,
}

impl Default for Lighting {
    fn default() -> Self {
        Self {
            sun: Some(DirectionalLight::default()),
            ambient_color: Vec3::new(0.40, 0.47, 0.62),
            ambient_intensity: 0.40,
            point_lights: Vec::new(),
            spot_lights: Vec::new(),
            sky: SkyParams::default(),
            fog: FogParams::default(),
            exposure: 1.0,
            shadow_strength: 0.85,
            environment: None,
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

    /// 与聚光灯同一条底线：**缺省不投影**必须是构造器的行为。
    ///
    /// 点光源在这一轮之前没有 `cast_shadows` 字段，老场景的灯全是"只照亮"，
    /// 所以构造器缺省必须是 `false`——否则所有老场景的像素都会变（多出阴影）。
    #[test]
    fn point_lights_do_not_cast_shadows_unless_asked() {
        let p = PointLight::new(Vec3::ZERO, Vec3::ONE, 1.0, 10.0);
        assert!(!p.cast_shadows);
        assert!(p.with_cast_shadows(true).cast_shadows);
        assert!(
            !p.with_cast_shadows(true)
                .with_cast_shadows(false)
                .cast_shadows
        );
        // 链式调用不许动别的字段
        let q = p.with_cast_shadows(true);
        assert_eq!(q.position, p.position);
        assert_eq!(q.color, p.color);
        assert_eq!(q.intensity, p.intensity);
        assert_eq!(q.radius, p.radius);
    }

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

    /// 加了 `cast_shadows` 之后，"缺省不投影"必须是构造器的行为，
    /// 而不是靠每个调用点记得关——否则老场景会悄悄多出阴影。
    #[test]
    fn spot_lights_do_not_cast_shadows_unless_asked() {
        let s = SpotLight::new(Vec3::ZERO, Vec3::NEG_Y, Vec3::ONE, 1.0, 10.0, 10.0, 20.0);
        assert!(!s.cast_shadows);
        assert!(s.with_cast_shadows(true).cast_shadows);
        assert!(
            !s.with_cast_shadows(true)
                .with_cast_shadows(false)
                .cast_shadows
        );
        // 链式调用不能改动别的字段（锥角编辑会重建整盏灯，靠这条保证开关不丢）
        let t = s.with_cast_shadows(true);
        assert_eq!(t.position, s.position);
        assert_eq!(t.inner_cos, s.inner_cos);
        assert_eq!(t.outer_cos, s.outer_cos);
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
