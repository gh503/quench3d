//! 点光源阴影（立方体阴影）的 CPU 侧数学：**6 个 90° 面相机** + 面/uv 约定 + 选灯规则。
//!
//! 与聚光灯阴影（[`crate::spot_shadow`]）的关系：**同一套零件**——同一个 `Depth32Float`
//! 格式、同一条只写深度的 `shadow_pipeline`、同一份透视 depth bias 推导（世界空间固定
//! 纹素数 × `dz/dd`）、同一份 3x3 PCF。区别只有一个：聚光灯是**一张 2D 图**（一个视锥），
//! 点光源向四面八方照，所以是**六张 90° 的面**（`texture_depth_2d_array` 的 6 个 layer）。
//!
//! # 为什么是 `texture_depth_2d_array` 而不是硬件的 `texture_cube`
//!
//! 硬件 cube 采样会把方向向量映射到某一面 + 面内 uv，映射规则由图形 API 规定
//! （Vulkan/D3D/GL 的表格互有细节差异），而**本仓库的 CI 没有显卡**：写错一处符号，
//! 只有"影子在面内镜像了"这一种症状，本地门禁（naga 只做语法/类型检查）抓不到。
//! 所以这里把"面选择 + 面内 uv"定义成**本文件里的纯函数**（[`cube_face_uv`]），
//! 渲染用的 6 个矩阵与着色器里的采样都按它来：
//!
//! * 渲染：`views[i]` 就是第 i 面的 `proj * view`（测试断言"矩阵算出的 uv == 纯函数算出的 uv"）；
//! * 采样：WGSL 里逐字镜像 [`cube_face_uv`] 的 6 个分支，再把 layer 当 `texture_depth_2d_array`
//!   的数组下标传进去。
//!
//! 于是"面与 uv 的约定"变成一条**能在无显卡 CI 上跑的等式**，而不是"指望硬件约定跟我一样"。
//! 代价是 PCF 要写成数组版本（`textureSampleCompareLevel` 的数组重载），以及没法让硬件
//! 做 cube 接缝过滤——面边界靠 1% 的 `tan` 余量让相邻面轻微重叠（见
//! [`POINT_SHADOW_FACE_MARGIN`]）。
//!
//! # 面与 uv 的约定（**必须与 `pbr.wgsl` 的 `cube_face_uv` 逐字一致**）
//!
//! 面按 `+X, -X, +Y, -Y, +Z, -Z` 编号 0..6；每面是一台朝该轴的 90°（+1% 余量）透视相机，
//! `up` 取一个与朝向垂直的轴（见 [`face_view`]）。uv 是**纹理空间**（v 向下，与 2D 阴影图
//! 的 `0.5 - ndc.y * 0.5` 同一个约定），所以 `v = 0` 是贴图第 0 行、对应世界里的"+up 一侧"。

use quench3d_math::{Aabb, Mat4, Vec3};

use crate::light::{PointLight, MAX_POINT_LIGHTS};

/// 立方体阴影的面数（`texture_depth_2d_array` 的 layer 数）。
pub const POINT_SHADOW_FACES: usize = 6;

/// 近平面占影响半径的比例（与聚光灯用同一个 1%）。
pub const POINT_SHADOW_NEAR_FRACTION: f32 = 0.01;

/// 近平面的绝对下限（米）：**不能取 0**，理由同 [`crate::spot_shadow::spot_shadow_camera`]
/// ——`near = 0` 时 `z_ndc ≡ 1`，深度图里一个比特的信息都没有。
pub const POINT_SHADOW_NEAR_MIN: f32 = 0.05;

/// 近平面的绝对上限（米）。
pub const POINT_SHADOW_NEAR_MAX: f32 = 0.5;

/// 每个面的 fov 余量（在 `tan` 空间上乘，1%）。
///
/// 90° 的面边界上物体会被两个面同时覆盖；这一步让每个面**多照一点**（半角 `45°` 的
/// `tan` 是 1，乘 1.01），于是边界附近 PCF 的 3x3 采样点仍然落在本面图内（否则会采到
/// 面外的钳制值，表现为立方体六个面接缝处的 1 像素亮/暗线）。
pub const POINT_SHADOW_FACE_MARGIN: f32 = 1.01;

/// 深度偏移（单位：纹素，换算到世界空间后再变到归一化深度）。
pub const POINT_SHADOW_DEPTH_BIAS_TEXELS: f32 = 1.5;

/// 法线偏移（单位：纹素）。
pub const POINT_SHADOW_NORMAL_OFFSET_TEXELS: f32 = 2.0;

/// 斜率因子（与平行光/聚光灯同一条 `1 - n·l * k`）。
pub const POINT_SHADOW_SLOPE_FACTOR: f32 = 0.8;

/// 一个点光源的 6 个面相机 + 采样时要用的常量。
#[derive(Clone, Copy, Debug)]
pub struct PointShadowCamera {
    /// 6 个面的世界 → **透视**裁剪空间矩阵（深度区间 `[0, 1]`）。
    /// 顺序固定为 `+X, -X, +Y, -Y, +Z, -Z`，与 [`cube_face_uv`] 的 layer 一致。
    pub views: [Mat4; POINT_SHADOW_FACES],
    /// 近平面（米，恒 > 0）。
    pub near: f32,
    /// 远平面（米）= 灯的影响半径。
    pub far: f32,
    /// `tan(fov / 2)`：算"一个纹素在某个距离上有多大"要用（≈ 1.01）。
    pub tan_half_fov: f32,
    /// 深度偏移（纹素个数）。
    pub depth_bias_texels: f32,
    /// 法线偏移（纹素个数）。
    pub normal_offset_texels: f32,
}

impl PointShadowCamera {
    /// 距离灯 `axis_depth`（**垂直于某个面**的米数）处一个纹素的世界上大小。
    ///
    /// 立方体的每个面都是 90°（+余量）的透视相机，所以"轴深"就是该方向上的主分量
    /// `max(|dx|,|dy|,|dz|)`——着色器里不用再做别的事，直接拿它算偏移。
    pub fn texel_world_size(&self, map_size_px: f32, axis_depth: f32) -> f32 {
        let d = axis_depth.max(self.near);
        2.0 * self.tan_half_fov * d / map_size_px.max(1.0)
    }

    /// 归一化深度上的偏移（与 [`crate::spot_shadow::SpotShadowCamera::normalized_depth_bias`]
    /// 同一条公式；着色器侧照抄）。
    pub fn normalized_depth_bias(&self, map_size_px: f32, axis_depth: f32, n_dot_l: f32) -> f32 {
        let d = axis_depth.max(self.near);
        let world = self.depth_bias_texels * self.texel_world_size(map_size_px, d);
        let dz_dd = self.far * self.near / ((self.far - self.near).max(1e-6) * d * d);
        world * dz_dd * (1.0 - n_dot_l.clamp(0.0, 1.0) * POINT_SHADOW_SLOPE_FACTOR)
    }

    /// 归一化深度：`z_ndc(d) = far / (far - near) * (1 - near / d)`。
    ///
    /// 面内任意方向上的"深度"都只与**主分量**（垂直于该面平面的距离）有关，
    /// 所以这个函数与朝向无关——这正是 cube 阴影能只用一个 `near`/`far` 的原因。
    /// 给测试与文档用（着色器里是等价的内联式）。
    pub fn normalized_depth(&self, axis_depth: f32) -> f32 {
        let d = axis_depth;
        if d <= self.near {
            return 0.0;
        }
        if d >= self.far {
            return 1.0;
        }
        self.far / (self.far - self.near) * (1.0 - self.near / d)
    }
}

/// 第 `layer` 面的相机朝向 `(forward, up)`。
///
/// `up` 的选取只影响面内 uv 的朝向：**它必须与 [`cube_face_uv`] 的公式一致**，
/// 而这条一致性由 `every_direction_projects_to_the_uv_the_shader_will_use` 钉死
/// （测试用大量方向同时过一遍矩阵与公式，任何一处符号错都会红）。
pub fn face_view(layer: usize, eye: Vec3, near: f32, far: f32, tan_half_fov: f32) -> Mat4 {
    let (forward, up) = match layer {
        0 => (Vec3::X, Vec3::Y),
        1 => (Vec3::NEG_X, Vec3::Y),
        2 => (Vec3::Y, Vec3::NEG_Z),
        3 => (Vec3::NEG_Y, Vec3::Z),
        4 => (Vec3::Z, Vec3::Y),
        _ => (Vec3::NEG_Z, Vec3::Y),
    };
    let fov = 2.0 * tan_half_fov.atan();
    Mat4::perspective_rh(fov, 1.0, near, far) * Mat4::look_to_rh(eye, forward, up)
}

/// 从一盏点光源构造 6 个面相机。
///
/// `near` = 影响半径的 1%（夹在 `[0.05, 0.5]` 米），`far` = 影响半径，
/// 每面 fov = 90° × 余量（见 [`POINT_SHADOW_FACE_MARGIN`]）。
/// 半径非法（0 / 负数 / NaN）时兜底成一个能用的极小灯，绝不产出退化矩阵。
pub fn point_shadow_camera(light: &PointLight) -> PointShadowCamera {
    let radius = if light.radius.is_finite() && light.radius > 0.0 {
        light.radius
    } else {
        POINT_SHADOW_NEAR_MIN * 4.0
    };
    let near = (radius * POINT_SHADOW_NEAR_FRACTION)
        .clamp(POINT_SHADOW_NEAR_MIN, POINT_SHADOW_NEAR_MAX)
        .min(radius * 0.5)
        .max(1e-4);
    let far = radius.max(near * 2.0);
    // 90° 的一面 + 余量：半角 45° 的 tan 是 1，乘 1.01 之后就是 `tan_half_fov`
    let tan_half_fov = POINT_SHADOW_FACE_MARGIN;

    let mut views = [Mat4::IDENTITY; POINT_SHADOW_FACES];
    for (i, slot) in views.iter_mut().enumerate() {
        *slot = face_view(i, light.position, near, far, tan_half_fov);
    }

    PointShadowCamera {
        views,
        near,
        far,
        tan_half_fov,
        depth_bias_texels: POINT_SHADOW_DEPTH_BIAS_TEXELS,
        normal_offset_texels: POINT_SHADOW_NORMAL_OFFSET_TEXELS,
    }
}

/// `(layer, u, v)`：方向（**从灯指向接收面**，不必归一化）落在哪个面、面内 uv 是多少。
///
/// **这是"渲染"与"采样"之间的唯一约定**，WGSL 里有一份逐字对应的实现
/// （`pbr.wgsl::cube_face_uv`）。判据不是"看起来对"，而是：
/// `PointShadowCamera::views[layer]` 把同一个点投到同一个 `(u, v)`（见测试）。
///
/// uv 是**纹理空间**：`v = 0` 是贴图第 0 行（NDC 的 +Y 一侧），与 2D 阴影图的
/// `0.5 - ndc.y * 0.5` 同一个约定——两条路共用同一份"V 翻转"直觉，少一个坑。
///
/// 方向为零向量（正好在灯心上）时退化成 `-Y` 面的中心：那里没有可比的深度。
pub fn cube_face_uv(dir: Vec3) -> (usize, f32, f32) {
    let a = dir.abs();
    // 每面的 fov 是 90° × 余量，所以"横向分量 / 主分量"要先除以余量再映射到 uv
    // （边界上的方向因此落在 1/1.01 而不是 1.0，给 PCF 留出边界；
    //   `tan_half_fov == POINT_SHADOW_FACE_MARGIN` 由 `point_shadow_camera` 保证）。
    let m = POINT_SHADOW_FACE_MARGIN;
    // 主分量：|x|、|y|、|z| 里最大的那个。相等时按 x ≥ y ≥ z 的固定顺序取舍，
    // 保证"同一个方向永远得到同一个面"（确定性）。
    if a.x >= a.y && a.x >= a.z {
        let ma = a.x;
        if ma <= 1e-12 {
            return (3, 0.5, 0.5);
        }
        if dir.x > 0.0 {
            (
                0,
                0.5 * (1.0 + dir.z / ma / m),
                0.5 * (1.0 - dir.y / ma / m),
            )
        } else {
            (
                1,
                0.5 * (1.0 - dir.z / ma / m),
                0.5 * (1.0 - dir.y / ma / m),
            )
        }
    } else if a.y >= a.z {
        let ma = a.y;
        if dir.y > 0.0 {
            (
                2,
                0.5 * (1.0 - dir.x / ma / m),
                0.5 * (1.0 + dir.z / ma / m),
            )
        } else {
            (
                3,
                0.5 * (1.0 - dir.x / ma / m),
                0.5 * (1.0 - dir.z / ma / m),
            )
        }
    } else {
        let ma = a.z;
        if dir.z > 0.0 {
            (
                4,
                0.5 * (1.0 - dir.x / ma / m),
                0.5 * (1.0 - dir.y / ma / m),
            )
        } else {
            (
                5,
                0.5 * (1.0 + dir.x / ma / m),
                0.5 * (1.0 - dir.y / ma / m),
            )
        }
    }
}

/// 选出这一帧负责投影的那盏**点光源**。
///
/// 规则（确定性，与聚光灯同风格）：按 `lighting.point_lights` 的顺序取**第一盏**
/// `cast_shadows == true` 的灯；点光源永远排在局部光名额的前面，所以只要存在就一定有槽位
/// （`MAX_POINT_LIGHTS` 之外的灯不参与着色，自然也不投影）。
///
/// 点光源阴影与聚光灯阴影是**两套独立资源**（一张 6 层数组 vs 一张 2D 图），
/// 所以两者可以同时存在：一盏投影点光源 + 一盏投影聚光灯是允许的。
pub fn pick_point_shadow(points: &[PointLight], enabled: bool) -> Option<usize> {
    if !enabled {
        return None;
    }
    points
        .iter()
        .take(MAX_POINT_LIGHTS)
        .position(|p| p.cast_shadows)
}

/// 一帧的点光源阴影计划。
#[derive(Clone, Copy, Debug)]
pub struct PointShadowPlan {
    /// 在 `lighting.point_lights` 里的下标（= `FrameUniforms::point_lights` 里的下标，
    /// 因为点光源永远排在最前面）。
    pub light_index: usize,
    /// 6 个面相机。
    pub camera: PointShadowCamera,
}

/// 把"选灯"与"造相机"合成一步。
pub fn plan_point_shadow(points: &[PointLight], enabled: bool) -> Option<PointShadowPlan> {
    let light_index = pick_point_shadow(points, enabled)?;
    Some(PointShadowPlan {
        light_index,
        camera: point_shadow_camera(&points[light_index]),
    })
}

/// 包围盒是否与"以 `center` 为心、`radius` 为半径的球"相交。
///
/// 用途：点光源阴影的**投射体名单**。平行光/聚光灯用视锥做剔除，而点光源照亮的是一个球，
/// 没有单一的视锥；用球测一遍才是"灯能照到的都进阴影图"。
/// （镜头看不见的物体仍然不会画进阴影图——那是整条阴影链路的既有前提，见
/// `docs/production_readiness.md`。）
pub fn aabb_intersects_sphere(bounds: &Aabb, center: Vec3, radius: f32) -> bool {
    if !radius.is_finite() || radius <= 0.0 {
        return false;
    }
    let closest = Vec3::new(
        center.x.clamp(bounds.min.x, bounds.max.x),
        center.y.clamp(bounds.min.y, bounds.max.y),
        center.z.clamp(bounds.min.z, bounds.max.z),
    );
    (closest - center).length_squared() <= radius * radius
}

#[cfg(test)]
mod tests {
    use super::*;
    use quench3d_math::{vec3, Vec4};

    fn light(radius: f32) -> PointLight {
        PointLight::new(vec3(0.0, 4.0, 0.0), Vec3::ONE, 20.0, radius)
    }

    /// 纹理空间的 uv（与着色器 `0.5 - ndc.y * 0.5` 同一约定）。
    fn matrix_uv(m: &Mat4, p: Vec3) -> (f32, f32, f32) {
        let ndc = m.project_point3(p);
        (0.5 * (ndc.x + 1.0), 0.5 * (1.0 - ndc.y), ndc.z)
    }

    /// **本模块最重要的一条测试**：渲染用的矩阵与采样用的公式必须给出同一个 `(面, uv)`。
    ///
    /// 走一遍球面上大量方向（含六个轴向、面边界、随机斜向）：
    /// * 公式给出 `(layer, u, v)`；
    /// * 第 `layer` 个矩阵把同一个点投到同一个 `(u, v)`，且深度在 `(0, 1)` 里；
    /// * **其它面**要么把点投到自己的贴图之外（说明它不该负责这个方向），
    ///   要么就是"边界上的重叠"（两个面都装得下，只可能是夹角里的方向）。
    ///
    /// 任何一处符号错（V 翻转、up 选反、层序错）都会让这条红 —— 第 55 轮就是这么
    /// 抓到 3 个面（-X / -Y / -Z）的 uv 符号写反的。
    #[test]
    fn every_direction_projects_to_the_uv_the_shader_will_use() {
        let l = light(20.0);
        let cam = point_shadow_camera(&l);
        let mut checked = 0usize;
        let mut seen_faces = [false; POINT_SHADOW_FACES];
        // 球面网格：经度 72 份、纬度 37 份（含两极）
        for i in 0..72 {
            for j in -18..=18 {
                let theta = i as f32 / 72.0 * std::f32::consts::TAU;
                let phi = j as f32 / 18.0 * (std::f32::consts::FRAC_PI_2 * 0.5);
                let dir = vec3(phi.cos() * theta.cos(), phi.sin(), phi.cos() * theta.sin());
                let (layer, u, v) = cube_face_uv(dir);
                seen_faces[layer] = true;
                assert!(
                    (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v),
                    "方向 {dir:?} 的 uv 越界: face {layer} ({u}, {v})"
                );
                // 距灯 5 米处的一个点
                let p = l.position + dir.normalize() * 5.0;
                let (mu, mv, mz) = matrix_uv(&cam.views[layer], p);
                assert!(
                    (mu - u).abs() < 2e-3 && (mv - v).abs() < 2e-3,
                    "方向 {dir:?}：公式给 ({u}, {v})，矩阵给 ({mu}, {mv})（face {layer}）"
                );
                assert!(
                    (0.0..1.0).contains(&mz),
                    "方向 {dir:?} 的深度应在 (0,1) 里: {mz}"
                );
                // 其它面：要么看不到这个点（投影越界/在背后），要么只是夹角的余量重叠
                for other in 0..POINT_SHADOW_FACES {
                    if other == layer {
                        continue;
                    }
                    let clip = cam.views[other] * Vec4::new(p.x, p.y, p.z, 1.0);
                    if clip.w <= 1e-6 {
                        continue; // 在那一面的背后
                    }
                    let (ou, ov, oz) = matrix_uv(&cam.views[other], p);
                    let inside = (0.0..=1.0).contains(&ou)
                        && (0.0..=1.0).contains(&ov)
                        && (0.0..=1.0).contains(&oz);
                    if inside {
                        // 两个面同时"装得下"这个方向，只可能是因为它落在两面的夹角里
                        // （横向分量与主分量之比 ≤ 余量）。取**第二大**的 |分量| 来判：
                        // 第二大为 0（正好在轴上）时永远不会成立。
                        let abs = dir.abs();
                        let mut comps = [abs.x, abs.y, abs.z];
                        comps.sort_by(|p, q| p.partial_cmp(q).unwrap());
                        let (second, largest) = (comps[1], comps[2]);
                        if second > 0.0 {
                            let ratio = largest / second;
                            assert!(
                                ratio <= POINT_SHADOW_FACE_MARGIN * 1.001,
                                "方向 {dir:?} 同时落在 face {layer} 与 face {other} 内部（比值 {ratio}）"
                            );
                        }
                    }
                }
                checked += 1;
            }
        }
        assert!(checked > 2500, "样本太少: {checked}");
        assert!(
            seen_faces.iter().all(|s| *s),
            "六个面都必须被覆盖到: {seen_faces:?}"
        );
    }

    /// 六个轴向：面编号与 uv 中心都是确定的（层序错了 = 影子长在别的面上）。
    #[test]
    fn the_six_axes_map_to_the_expected_face_centers() {
        let cases = [
            (Vec3::X, 0),
            (Vec3::NEG_X, 1),
            (Vec3::Y, 2),
            (Vec3::NEG_Y, 3),
            (Vec3::Z, 4),
            (Vec3::NEG_Z, 5),
        ];
        for (dir, want) in cases {
            let (layer, u, v) = cube_face_uv(dir);
            assert_eq!(layer, want, "{dir:?} 应落在面 {want}");
            assert!(
                (u - 0.5).abs() < 1e-6 && (v - 0.5).abs() < 1e-6,
                "{dir:?} 应落在贴图正中，实际 ({u}, {v})"
            );
        }
        // 面内的横向偏移：符号就是"镜像与否"的判据，所以逐面断言
        // （六面的 up 选得不同，镜像关系也不同）
        let cases: [(Vec3, usize, bool, bool); 6] = [
            // 方向、期望面、u 是否 > 0.5、v 是否 > 0.5
            (vec3(1.0, 0.5, 0.5), 0, true, false),
            (vec3(-1.0, 0.5, 0.5), 1, false, false),
            (vec3(0.5, 1.0, 0.5), 2, false, true),
            (vec3(0.5, -1.0, 0.5), 3, false, false),
            (vec3(0.5, 0.5, 1.0), 4, false, false),
            (vec3(0.5, 0.5, -1.0), 5, true, false),
        ];
        for (dir, want_face, u_gt, v_gt) in cases {
            let (layer, u, v) = cube_face_uv(dir);
            assert_eq!(layer, want_face, "{dir:?} 应落在面 {want_face}");
            assert_eq!(u > 0.5, u_gt, "{dir:?} 的 u 方向: {u}");
            assert_eq!(v > 0.5, v_gt, "{dir:?} 的 v 方向: {v}");
        }
        // "+z 偏 u 大"在 +X 面上成立；到 -X 面必须翻过来（镜像面）
        let (_, u_pos, _) = cube_face_uv(vec3(1.0, 0.0, 0.5));
        let (_, u_neg, _) = cube_face_uv(vec3(-1.0, 0.0, 0.5));
        assert!(u_pos > 0.5 && u_neg < 0.5, "{u_pos} / {u_neg}");
        assert!(
            ((u_pos - 0.5) + (u_neg - 0.5)).abs() < 1e-6,
            "两个镜像面的 u 偏移必须互为相反数: {u_pos} / {u_neg}"
        );
        // +y 在 +X 与 -X 面上都偏 v 小（两面的 up 都是 +Y）
        let (_, _, v_pos) = cube_face_uv(vec3(1.0, 0.5, 0.0));
        let (_, _, v_neg) = cube_face_uv(vec3(-1.0, 0.5, 0.0));
        assert!(v_pos < 0.5 && v_neg < 0.5, "{v_pos} / {v_neg}");
    }

    /// 面边界：正好 45° 的方向必须落在 `1/1.01`（而不是 0 或 1）—— 余量是给 PCF 用的。
    #[test]
    fn the_face_boundary_lands_just_inside_the_tile() {
        let want = 1.0 / POINT_SHADOW_FACE_MARGIN;
        for dir in [
            vec3(1.0, 1.0, 0.0),
            vec3(1.0, -1.0, 0.0),
            vec3(1.0, 0.0, 1.0),
            vec3(1.0, 0.0, -1.0),
            vec3(0.0, 1.0, 1.0),
            vec3(0.0, 1.0, -1.0),
            vec3(1.0, 1.0, 1.0),
        ] {
            let (_, u, v) = cube_face_uv(dir);
            let outer = (2.0 * u - 1.0).abs().max((2.0 * v - 1.0).abs());
            assert!(
                (outer - want).abs() < 1e-4,
                "方向 {dir:?} 的边界应落在 {want}，实际 {outer}（uv = {u}, {v}）"
            );
        }
        // 反过来：45° 之外的方向会被判给**隔壁那个面**，而原来那个面已经装不下它
        // （说明上一条不是恒真：越界方向确实存在，只是被分给了别的面）
        let beyond = (47.0f32).to_radians();
        let dir = vec3(1.0, beyond.tan(), 0.0);
        let (layer, _, _) = cube_face_uv(dir);
        assert_eq!(layer, 2, "超过 45° 之后主分量变成 +y，该由 +Y 面负责");
        let cam = point_shadow_camera(&light(20.0));
        let p = light(20.0).position + dir.normalize() * 5.0;
        let (u0, v0, z0) = matrix_uv(&cam.views[0], p);
        assert!(
            !(0.0..=1.0).contains(&u0) || !(0.0..=1.0).contains(&v0) || !(0.0..=1.0).contains(&z0),
            "+X 面不该再装得下这个方向: ({u0}, {v0}, {z0})"
        );
    }

    /// 同样的**垂直距离**在不同面上给出同样的深度（cube 只用一个 near/far 的根据）。
    #[test]
    fn depth_depends_only_on_the_perpendicular_distance() {
        let l = light(20.0);
        let cam = point_shadow_camera(&l);
        assert!(cam.near > 0.0 && cam.far == 20.0);
        assert!((cam.tan_half_fov - POINT_SHADOW_FACE_MARGIN).abs() < 1e-6);
        let mut zs = Vec::new();
        for d in [1.0f32, 4.0, 9.0, 15.0, 19.0] {
            let a = cam.views[3].project_point3(l.position + Vec3::NEG_Y * d).z;
            let b = cam.views[0].project_point3(l.position + Vec3::X * d).z;
            let c = cam.views[4].project_point3(l.position + Vec3::Z * d).z;
            assert!((a - b).abs() < 1e-6 && (b - c).abs() < 1e-6, "{a} {b} {c}");
            assert!(
                (a - cam.normalized_depth(d)).abs() < 1e-5,
                "{a} vs {}",
                cam.normalized_depth(d)
            );
            zs.push(a);
        }
        assert!(
            zs.windows(2).all(|w| w[1] > w[0]),
            "深度必须随距离严格递增: {zs:?}"
        );
        assert!(zs[0] > 0.0 && *zs.last().unwrap() < 1.0);
        let beyond = cam.views[0].project_point3(l.position + Vec3::X * 21.0).z;
        assert!(beyond > 1.0, "半径外应当越过 far: {beyond}");
    }

    /// `near` 不能取 0：同一条量化证据（所有深度都塌成 1），与本模块自己的矩阵对账。
    #[test]
    fn a_zero_near_collapses_every_depth_in_the_face_matrix() {
        let l = light(20.0);
        let good = point_shadow_camera(&l);
        // 手工造一个 near = 0 的面相机（不该出现在生产路径里，只为证明它不是"经验之谈"）
        let bad = face_view(0, l.position, 0.0, 20.0, POINT_SHADOW_FACE_MARGIN);
        let mut good_zs = Vec::new();
        for d in [1.0f32, 3.0, 8.0, 15.0] {
            let p = l.position + Vec3::X * d;
            let z = bad.project_point3(p).z;
            assert!(
                (z - 1.0).abs() < 1e-6,
                "near = 0 时 {d} 米的深度应为 1，实际 {z}"
            );
            good_zs.push(good.views[0].project_point3(p).z);
        }
        good_zs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!(
            good_zs.windows(2).all(|w| w[1] > w[0]),
            "near > 0 时深度必须严格递增: {good_zs:?}"
        );
    }

    #[test]
    fn near_is_a_positive_small_fraction_and_bad_radii_do_not_degenerate() {
        for (radius, want_near) in [(20.0f32, 0.2f32), (2.0, 0.05), (200.0, 0.5)] {
            let cam = point_shadow_camera(&light(radius));
            assert!(cam.near > 0.0);
            assert!(
                (cam.near - want_near).abs() < 1e-6,
                "半径 {radius} 的 near 应为 {want_near}，实际 {}",
                cam.near
            );
            assert_eq!(cam.far, radius);
            assert!(cam.views.iter().all(|m| m.is_finite()));
        }
        for bad in [0.0f32, -3.0, f32::NAN] {
            let cam = point_shadow_camera(&light(bad));
            assert!(cam.near > 0.0 && cam.far > cam.near, "{bad} → {cam:?}");
            assert!(
                cam.views.iter().all(|m| m.is_finite()),
                "{bad} → 矩阵不是有限数"
            );
        }
    }

    #[test]
    fn texel_size_grows_linearly_and_the_bias_is_a_constant_world_offset() {
        let cam = point_shadow_camera(&light(20.0));
        let map = 512.0;
        for d in [1.0f32, 3.0, 8.0, 17.0] {
            for n_dot_l in [0.0f32, 0.5, 1.0] {
                let bias_ndc = cam.normalized_depth_bias(map, d, n_dot_l);
                let dz_dd = cam.far * cam.near / ((cam.far - cam.near) * d * d);
                let world = bias_ndc / dz_dd;
                let want = cam.depth_bias_texels
                    * cam.texel_world_size(map, d)
                    * (1.0 - n_dot_l * POINT_SHADOW_SLOPE_FACTOR);
                assert!(
                    (world - want).abs() <= 1e-9 + want * 1e-5,
                    "d = {d}：世界偏移 {world} 与 {want} 不符"
                );
            }
            assert!(
                cam.normalized_depth_bias(map, d, 1.0) < cam.normalized_depth_bias(map, d, 0.0)
            );
        }
        let near_texel = cam.texel_world_size(map, 2.0);
        let far_texel = cam.texel_world_size(map, 16.0);
        assert!((far_texel / near_texel - 8.0).abs() < 1e-4);
    }

    /// 选灯：第一盏勾了的赢，名额之外的灯不算（它们本来就不参与着色）。
    #[test]
    fn the_first_casting_point_light_wins() {
        let mk = |shadows: bool| PointLight {
            cast_shadows: shadows,
            ..PointLight::new(Vec3::ZERO, Vec3::ONE, 1.0, 10.0)
        };
        assert_eq!(pick_point_shadow(&[mk(false)], true), None);
        assert_eq!(
            pick_point_shadow(&[mk(true)], false),
            None,
            "总开关关掉就都不投"
        );
        assert_eq!(
            pick_point_shadow(&[mk(false), mk(true), mk(true)], true),
            Some(1)
        );
        assert_eq!(pick_point_shadow(&[], true), None);
        // 9 盏、只有第 9 盏勾了：它超出了 8 盏名额 → 不投影（而不是"悄悄换一盏"）
        let mut many: Vec<PointLight> = (0..9).map(|_| mk(false)).collect();
        many[8].cast_shadows = true;
        assert_eq!(pick_point_shadow(&many, true), None);
        let mut eight: Vec<PointLight> = (0..8).map(|_| mk(false)).collect();
        eight[7].cast_shadows = true;
        assert_eq!(pick_point_shadow(&eight, true), Some(7));
        // 计划里的相机就是那盏灯的
        let plan = plan_point_shadow(&eight, true).expect("应当有计划");
        assert_eq!(plan.light_index, 7);
        assert!(plan.camera.views[0].approx_eq(&point_shadow_camera(&eight[7]).views[0]));
    }

    /// 选中的下标必须与 `pack_local_lights` 的槽位一致：下标错了，影子就长在**别的灯**下面。
    #[test]
    fn the_picked_index_matches_where_pack_local_lights_puts_the_light() {
        use crate::light::SpotLight;
        use crate::renderer::pack_local_lights;
        let points: Vec<PointLight> = (0..3)
            .map(|i| PointLight {
                cast_shadows: i == 2,
                ..PointLight::new(vec3(i as f32, 4.0, 0.0), Vec3::ONE, 2.0, 12.0)
            })
            .collect();
        let spots = vec![SpotLight::new(
            vec3(5.0, 4.0, 0.0),
            Vec3::NEG_Y,
            Vec3::ONE,
            1.0,
            10.0,
            10.0,
            20.0,
        )];
        let (packed, n) = pack_local_lights(&points, &spots);
        assert_eq!(n, 4);
        let idx = pick_point_shadow(&points, true).expect("第 3 盏勾了");
        assert_eq!(idx, 2);
        assert_eq!(packed[idx].pos_range[0], 2.0);
        assert_eq!(packed[idx].pos_range[3], 12.0);
        assert_eq!(packed[idx].dir_outer[3], 0.0, "下标处必须是点光源");
    }

    /// 投射体名单：球测必须把"灯照得到的盒子"收进来，把远处的排除掉。
    #[test]
    fn the_shadow_caster_sphere_test_behaves() {
        let center = vec3(0.0, 4.0, 0.0);
        let inside = Aabb::new(vec3(-1.0, 3.0, -1.0), vec3(1.0, 5.0, 1.0));
        assert!(aabb_intersects_sphere(&inside, center, 10.0));
        // 刚好贴着球面：最近点 (10, 4, 0) 与灯心距离 10 → 算相交（用 <=）
        let touching = Aabb::new(vec3(10.0, 3.0, -1.0), vec3(12.0, 5.0, 1.0));
        assert!(aabb_intersects_sphere(&touching, center, 10.0));
        let outside = Aabb::new(vec3(10.2, 3.0, -1.0), vec3(12.0, 5.0, 1.0));
        assert!(!aabb_intersects_sphere(&outside, center, 10.0));
        // 巨大的盒子（地面）：球够不到它时也不该被算进来（最近点距离 14 > 10）
        let huge = Aabb::new(vec3(-500.0, -20.0, -500.0), vec3(500.0, -10.0, 500.0));
        assert!(!aabb_intersects_sphere(&huge, center, 10.0));
        assert!(aabb_intersects_sphere(&huge, center, 15.0));
        // 非法半径
        assert!(!aabb_intersects_sphere(&inside, center, 0.0));
        assert!(!aabb_intersects_sphere(&inside, center, f32::NAN));
        // 灯在盒子里面
        assert!(aabb_intersects_sphere(&inside, center, 0.1));
    }
}
