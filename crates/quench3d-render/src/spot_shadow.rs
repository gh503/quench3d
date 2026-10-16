//! 聚光灯阴影（spot shadow）的 CPU 侧数学：一盏灯的**透视**阴影相机 + 选灯规则。
//!
//! 与平行光阴影（[`crate::cascade`]）的关系：**复用同一套机制**——同一张
//! `Depth32Float` 阴影贴图格式、同一条只写深度的顶点着色器（[`crate::renderer`]
//! 里的 `shadow_pipeline`）、同一份 3x3 PCF 采样/比较。区别只有两点：
//!
//! 1. 投影从**正交**变成**透视**（聚光灯是个点，锥体在远处张得更开）；
//! 2. 范围不是"相机周围一个盒子"，而是灯自己的圆锥（fov = 2×外锥角，far = 影响半径）。
//!
//! 这个文件里全是纯计算，**不需要显卡就能验收**。判据不是"代码看起来对"，而是几条
//! 能算出来的不变量（见文件末尾的测试）：
//! * 正下方（沿轴）的点投影到贴图正中；
//! * 外锥角边界上的点落在贴图边缘附近（既没被裁掉、也没浪费半张图）；
//! * `radius` 之外的点在远平面之外；
//! * `near` 取 0 会让**所有**深度都变成 1（深度图彻底失去信息）——所以它有一个正的下限；
//! * 深度偏移在**世界空间**里恒等于固定个数的纹素（与距离无关），
//!   这正是"归一化深度上的常量 bias 在透视投影里不等于常量世界偏移"的修法。
//!
//! **只支持一盏**投影聚光灯（见 [`pick_spot_shadow`]）：一张 2D 阴影图只能对应一个
//! 视锥。多盏、级联式聚光阴影都没做——边界写在 `docs/production_readiness.md`
//! 与 `CHANGELOG.md` 里。点光源的立方体阴影在第 55 轮补上了，是**另一套资源**
//! （6 层深度数组，见 [`crate::point_shadow`]），所以这里那张 2D 图不受影响。

use quench3d_math::{Mat4, Vec3};

use crate::light::{PointLight, SpotLight, MAX_POINT_LIGHTS};

/// 近平面占影响半径的比例（1%）。
///
/// 为什么按半径缩放而不是写死：影响半径从 2 米到 200 米都有，一个固定的小 near 在
/// 大半径下白白浪费深度精度（深度是 `1/d` 分布的，near 越小、远处越挤）。
pub const SPOT_SHADOW_NEAR_FRACTION: f32 = 0.01;

/// 近平面的绝对下限（米）：**不能取 0**，见 [`spot_shadow_camera`] 的说明。
pub const SPOT_SHADOW_NEAR_MIN: f32 = 0.05;

/// 近平面的绝对上限（米）：别让 200 米半径的灯把 near 拉到 2 米，
/// 那样灯附近 2 米内的遮挡物会整块消失。
pub const SPOT_SHADOW_NEAR_MAX: f32 = 0.5;

/// fov 在 **`tan` 空间**上的余量（1%）：给 3x3 PCF 留一点边界。
///
/// 为什么放宽 tan 而不是角度：角度上乘 1.01 在宽锥下会失控
/// （外锥 80 度时 `tan(80.8°) / tan(80°) ≈ 0.92`，贴图边缘白白浪费掉 8%），
/// 而 `tan` 上乘 1.01 对任何角度都恰好是"边界落在 NDC 的 1/1.01 ≈ 0.990 处"。
pub const SPOT_SHADOW_CONE_MARGIN: f32 = 1.01;

/// 深度偏移（单位：纹素，**世界空间**里换算之后再变到归一化深度）。
pub const SPOT_SHADOW_DEPTH_BIAS_TEXELS: f32 = 1.5;

/// 法线偏移（单位：纹素）：沿接收面法线把采样点推开，斜面上最有效。
pub const SPOT_SHADOW_NORMAL_OFFSET_TEXELS: f32 = 2.0;

/// 斜率因子：与平行光阴影用同一个 `bias * (1 - n·l * k)` 形状（[`crate::renderer`] 的
/// `sample_shadow` 里是 0.8），好让两条路径的观感一致。
pub const SPOT_SHADOW_SLOPE_FACTOR: f32 = 0.8;

/// 一盏聚光灯的阴影相机 + 采样时需要的那几个常量。
#[derive(Clone, Copy, Debug)]
pub struct SpotShadowCamera {
    /// 世界 → 光的**透视**裁剪空间矩阵（深度区间 `[0, 1]`，与 wgpu 一致）。
    pub view_proj: Mat4,
    /// 近平面（米，恒 > 0）。
    pub near: f32,
    /// 远平面（米）= 灯的影响半径。
    pub far: f32,
    /// `tan(fov / 2)`：算"一个纹素在某个距离上有多大"要用。
    pub tan_half_fov: f32,
    /// 深度偏移（纹素个数）。
    pub depth_bias_texels: f32,
    /// 法线偏移（纹素个数）。
    pub normal_offset_texels: f32,
}

impl SpotShadowCamera {
    /// 距离灯 `axis_depth`（沿光轴，米）处**一个纹素**的世界上大小（米）。
    ///
    /// 透视投影下纹素足迹随距离线性增长——这正是"世界空间固定偏移"和
    /// "归一化深度固定偏移"在透视里不等价的原因。
    pub fn texel_world_size(&self, map_size_px: f32, axis_depth: f32) -> f32 {
        let d = axis_depth.max(self.near);
        2.0 * self.tan_half_fov * d / map_size_px.max(1.0)
    }

    /// 归一化深度上的偏移：与 [`Self::texel_world_size`] 同一条公式的镜像（WGSL 侧照抄）。
    ///
    /// 推导：`z_ndc(d) = f / (f - n) * (1 - n / d)`，所以 `dz/dd = f * n / ((f - n) d²)`。
    /// 世界偏移 `b = 纹素数 * 纹素大小` 换算成归一化深度就是 `b * dz/dd`，
    /// 再乘上斜率因子 `(1 - n·l * k)`（正对光源的面几乎不需要偏移）。
    pub fn normalized_depth_bias(&self, map_size_px: f32, axis_depth: f32, n_dot_l: f32) -> f32 {
        let d = axis_depth.max(self.near);
        let world = self.depth_bias_texels * self.texel_world_size(map_size_px, d);
        let dz_dd = self.far * self.near / ((self.far - self.near).max(1e-6) * d * d);
        world * dz_dd * (1.0 - n_dot_l.clamp(0.0, 1.0) * SPOT_SHADOW_SLOPE_FACTOR)
    }
}

/// 从一盏聚光灯构造阴影相机。
///
/// * `fov = 2 × 外锥角 × 1.01`（余量只为 PCF 边界，见 [`SPOT_SHADOW_CONE_MARGIN`]）；
/// * `near` = 影响半径的 1%，夹在 `[0.05, 0.5]` 米；
/// * `far` = 影响半径（灯在半径处已经衰减到 0，再远的东西照不到也不该投影）；
/// * 宽高比 1:1（阴影图是正方形，外锥在内切圆里）。
///
/// # 为什么 `near` 不能取 0
///
/// 两个理由，第一个是数学上的、第二个是精度上的：
///
/// 1. `Mat4::perspective_rh` 的分母是 `near - far`、平移项是 `near * far / (near - far)`。
///    `near = 0` 时它们退化成 `-far` 与 `0`，投影矩阵**不再可逆**（求逆会得到垃圾），
///    而"从世界坐标反推光空间"在调试/拟合时是要用的。
/// 2. 更要命的是深度会**彻底失去信息**：`near = 0` 时 `clip.z = -z_view`、`clip.w = -z_view`，
///    于是 `z_ndc ≡ 1`——不管多远都写进深度图的同一层。阴影比较（`textureSampleCompare`）
///    在"遮挡物深度 == 接收面深度"的情况下没有任何可比的东西，阴影不是不准，是**不存在**。
///    这条不是推理，是 `near_zero_destroys_every_depth` 那条测试量出来的。
///
/// 取一个正的小值还带来一个好处：`z_ndc` 在小距离上有充足的有效位（f32 深度 + 1% 半径的
/// near，20 米半径的灯在 1 米处仍有约 1e-4 米量级的可分辨度）。
pub fn spot_shadow_camera(light: &SpotLight) -> SpotShadowCamera {
    let dir = light.dir();
    // 半径非法时兜底成一个"能用的极小灯"，而不是让 far <= near（那会让矩阵退化）
    let radius = if light.radius.is_finite() && light.radius > 0.0 {
        light.radius
    } else {
        SPOT_SHADOW_NEAR_MIN * 4.0
    };
    let near = (radius * SPOT_SHADOW_NEAR_FRACTION)
        .clamp(SPOT_SHADOW_NEAR_MIN, SPOT_SHADOW_NEAR_MAX)
        .min(radius * 0.5)
        .max(1e-4);
    let far = radius.max(near * 2.0);

    // 外锥角从余弦反解（引擎只存余弦），`tan` 上再加一点 PCF 余量
    let outer_angle = light.outer_cos.clamp(-1.0, 1.0).acos();
    let tan_half_fov = (outer_angle.tan() * SPOT_SHADOW_CONE_MARGIN)
        .clamp(0.5f32.to_radians().tan(), 89.9f32.to_radians().tan());
    let half_fov = tan_half_fov.atan();
    let tan_half_fov = half_fov.tan();

    // up 与光轴平行时退化：换一个轴（与平行光阴影的 `shadow_matrix` 同一处理）
    let up = if dir.y.abs() > 0.99 { Vec3::Z } else { Vec3::Y };
    let view = Mat4::look_to_rh(light.position, dir, up);
    let proj = Mat4::perspective_rh(half_fov * 2.0, 1.0, near, far);

    SpotShadowCamera {
        view_proj: proj * view,
        near,
        far,
        tan_half_fov,
        depth_bias_texels: SPOT_SHADOW_DEPTH_BIAS_TEXELS,
        normal_offset_texels: SPOT_SHADOW_NORMAL_OFFSET_TEXELS,
    }
}

/// 选出这一帧负责投影的那盏聚光灯。
///
/// 规则（**确定性**，见 `docs/production_readiness.md` §12.4）：
/// 按 `lighting.spot_lights` 的顺序，取**第一盏** `cast_shadows == true`
/// **且没有被局部光名额挤掉**的灯。返回 `(聚光灯下标, 打包进 uniform 的下标)`。
///
/// 为什么是"第一盏"而不是"最亮的一盏"：可预测比"猜你想要哪盏"重要——
/// 用户勾了哪盏就是哪盏，勾第二盏而第一盏也勾着时行为由文档写死（校验会警告）。
///
/// 名额的算法必须与 `pack_local_lights` **逐字一致**（点光源先占名额、聚光灯排在后面），
/// 否则会出现"阴影图算的是 A 灯、着色器按 B 灯的位置采样"这种**看起来有影子但位置不对**的 bug。
/// 这也是把它做成纯函数、并且和 `pack_local_lights` 一起测的原因。
pub fn pick_spot_shadow(
    points: &[PointLight],
    spots: &[SpotLight],
    enabled: bool,
) -> Option<(usize, usize)> {
    if !enabled {
        return None;
    }
    let used_by_points = points.len().min(MAX_POINT_LIGHTS);
    for (i, s) in spots.iter().enumerate() {
        if !s.cast_shadows {
            continue;
        }
        let packed = used_by_points + i;
        if packed >= MAX_POINT_LIGHTS {
            // 这盏灯自己就被名额挤掉了（不参与着色），自然也不该投影
            return None;
        }
        return Some((i, packed));
    }
    None
}

/// 一帧的聚光阴影计划：灯的下标 + 阴影相机。
#[derive(Clone, Copy, Debug)]
pub struct SpotShadowPlan {
    /// 在 `lighting.spot_lights` 里的下标。
    pub spot_index: usize,
    /// 在 `FrameUniforms::point_lights`（着色器里的 `array<PointLight, 8>`）里的下标。
    pub packed_index: usize,
    /// 阴影相机。
    pub camera: SpotShadowCamera,
}

/// 把"选灯"与"造相机"合成一步，给渲染器直接用。
pub fn plan_spot_shadow(
    points: &[PointLight],
    spots: &[SpotLight],
    enabled: bool,
) -> Option<SpotShadowPlan> {
    let (spot_index, packed_index) = pick_spot_shadow(points, spots, enabled)?;
    Some(SpotShadowPlan {
        spot_index,
        packed_index,
        camera: spot_shadow_camera(&spots[spot_index]),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use quench3d_math::{vec3, Vec4};

    fn light(outer_deg: f32, radius: f32) -> SpotLight {
        SpotLight::new(
            vec3(0.0, 10.0, 0.0),
            vec3(0.0, -1.0, 0.0),
            Vec3::ONE,
            20.0,
            radius,
            10.0,
            outer_deg,
        )
    }

    /// 沿光轴距离灯 `d` 米的点（正下方）。
    fn on_axis(l: &SpotLight, d: f32) -> Vec3 {
        l.position + l.dir() * d
    }

    /// 圆锥边界上的点：沿光轴 `d` 米，再横向偏 `d * tan(外锥角)`。
    fn on_cone_edge(l: &SpotLight, d: f32, lateral: Vec3) -> Vec3 {
        let angle = l.outer_cos.acos();
        on_axis(l, d) + lateral.normalize() * (d * angle.tan())
    }

    #[test]
    fn the_point_straight_below_the_light_lands_in_the_map_center() {
        let l = light(30.0, 20.0);
        let cam = spot_shadow_camera(&l);
        let ndc = cam.view_proj.project_point3(on_axis(&l, 10.0));
        assert!(
            ndc.x.abs() < 1e-5 && ndc.y.abs() < 1e-5,
            "轴心应落在正中: {ndc:?}"
        );
        assert!((0.0..1.0).contains(&ndc.z), "轴心点要在深度范围内: {ndc:?}");
        // 方向朝下时 up 退化，换成 Z 轴；换 up 不该把轴心挪走
        assert!(cam.view_proj.is_finite());
    }

    #[test]
    fn the_cone_boundary_lands_just_inside_the_map() {
        // 余量是在 `tan` 空间乘的，所以"边界落在 NDC 的 1/1.01 处"与锥角无关——
        // 窄锥、宽锥都一样，这条断言把那个不变量钉死。
        let want = 1.0 / SPOT_SHADOW_CONE_MARGIN;
        for outer_deg in [10.0f32, 30.0, 60.0, 80.0] {
            let l = light(outer_deg, 30.0);
            let cam = spot_shadow_camera(&l);
            let d = 12.0;
            for lateral in [vec3(1.0, 0.0, 0.0), vec3(0.0, 0.0, 1.0)] {
                let ndc = cam.view_proj.project_point3(on_cone_edge(&l, d, lateral));
                let r = ndc.x.abs().max(ndc.y.abs());
                assert!(
                    (r - want).abs() < 2e-3,
                    "外锥 {outer_deg} 度（{lateral:?}）的边界应落在 {want}，实际 {r}（{ndc:?}）"
                );
                assert!((0.0..1.0).contains(&ndc.z), "{ndc:?}");
            }
            // 边界之外的点确实在贴图之外（这一刀说明上面那条不是恒真）
            let beyond = (outer_deg + 10.0f32).to_radians();
            let outside = on_axis(&l, d) + vec3(1.0, 0.0, 0.0) * (d * beyond.tan());
            let ndc = cam.view_proj.project_point3(outside);
            assert!(
                ndc.x.abs() > 1.0,
                "外锥 {outer_deg} 度之外（{beyond} 弧度对应的方向）应当在图外: {ndc:?}"
            );
        }
    }

    #[test]
    fn near_is_a_positive_small_fraction_and_far_is_the_radius() {
        for (radius, want_near) in [(20.0f32, 0.2f32), (2.0, 0.05), (200.0, 0.5)] {
            let cam = spot_shadow_camera(&light(30.0, radius));
            assert!(cam.near > 0.0, "near 必须为正");
            assert!(
                (cam.near - want_near).abs() < 1e-6,
                "半径 {radius} 的 near 应为 {want_near}，实际 {}",
                cam.near
            );
            assert_eq!(cam.far, radius, "far 就是影响半径");
            assert!(cam.far > cam.near);
        }
        // 荒谬输入（0 / 负数 / NaN 半径）不许产出退化矩阵
        for bad in [0.0f32, -3.0, f32::NAN] {
            let cam = spot_shadow_camera(&light(30.0, bad));
            assert!(cam.near > 0.0 && cam.far > cam.near, "{bad} → {cam:?}");
            assert!(cam.view_proj.is_finite(), "{bad} → 矩阵不是有限数");
        }
    }

    #[test]
    fn points_beyond_the_radius_are_past_the_far_plane() {
        let l = light(25.0, 10.0);
        let cam = spot_shadow_camera(&l);
        let inside = cam.view_proj.project_point3(on_axis(&l, 9.0));
        assert!(
            (0.0..1.0).contains(&inside.z),
            "半径内应当在 far 之内: {inside:?}"
        );
        let outside = cam.view_proj.project_point3(on_axis(&l, 11.0));
        assert!(outside.z > 1.0, "半径外应当越过 far: {outside:?}");
        // 远平面上/内侧都不该被裁剪成 NaN（w = 0 情形）
        assert!(outside.is_finite() && inside.is_finite());
    }

    /// `near = 0` 的投影会把**所有**深度映到 1：这就是"near 不能取 0"的量化证据。
    #[test]
    fn near_zero_destroys_every_depth() {
        let fov = 60f32.to_radians();
        let good = Mat4::perspective_rh(fov, 1.0, 0.2, 20.0);
        let bad = Mat4::perspective_rh(fov, 1.0, 0.0, 20.0);
        let mut good_zs = Vec::new();
        for d in [0.5f32, 1.0, 5.0, 12.0, 19.0] {
            let p = vec3(0.0, 0.0, -d);
            good_zs.push(good.project_point3(p).z);
            let z = bad.project_point3(p).z;
            assert!(
                (z - 1.0).abs() < 1e-6,
                "near = 0 时 {d} 米的深度应为 1（无信息），实际 {z}"
            );
        }
        good_zs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!(
            good_zs.windows(2).all(|w| w[1] > w[0]),
            "near > 0 时不同距离必须给出**严格递增**的归一化深度: {good_zs:?}"
        );
        assert!(
            good_zs.last().unwrap() - good_zs.first().unwrap() > 0.3,
            "深度必须真的铺开（不是挤在 1 附近）: {good_zs:?}"
        );
        // 0 米处 w = 0：`project_point3` 会退化成"不除"，所以调用方必须自己判 w
        let w = good * Vec4::new(0.0, 0.0, 0.0, 1.0);
        assert!(w.w.abs() < 1e-6);
    }

    #[test]
    fn depth_bias_is_a_constant_number_of_world_texels_at_any_distance() {
        let cam = spot_shadow_camera(&light(30.0, 20.0));
        let map = 1024.0;
        for d in [1.0f32, 3.0, 8.0, 17.0] {
            for n_dot_l in [0.0f32, 0.5, 1.0] {
                let bias_ndc = cam.normalized_depth_bias(map, d, n_dot_l);
                // 反算回世界空间：Δd = Δz / (dz/dd)
                let dz_dd = cam.far * cam.near / ((cam.far - cam.near) * d * d);
                let world = bias_ndc / dz_dd;
                let want = cam.depth_bias_texels
                    * cam.texel_world_size(map, d)
                    * (1.0 - n_dot_l * SPOT_SHADOW_SLOPE_FACTOR);
                assert!(
                    (world - want).abs() <= 1e-9 + want * 1e-5,
                    "d = {d}、n·l = {n_dot_l}：世界偏移 {world} 与 {want} 不符"
                );
            }
            // 正对光源时偏移更小（斜率因子），且都为正
            assert!(
                cam.normalized_depth_bias(map, d, 1.0) < cam.normalized_depth_bias(map, d, 0.0)
            );
            assert!(cam.normalized_depth_bias(map, d, 1.0) > 0.0);
        }
        // 纹素大小随距离线性增长：这是透视投影的必然，也是"世界偏移恒定"的根据
        let near_texel = cam.texel_world_size(map, 2.0);
        let far_texel = cam.texel_world_size(map, 16.0);
        assert!((far_texel / near_texel - 8.0).abs() < 1e-4);
    }

    #[test]
    fn the_first_caster_wins_and_the_budget_is_respected() {
        let mk = |shadows: bool, x: f32| {
            SpotLight::new(
                vec3(x, 5.0, 0.0),
                vec3(0.0, -1.0, 0.0),
                Vec3::ONE,
                1.0,
                10.0,
                10.0,
                20.0,
            )
            .with_cast_shadows(shadows)
        };
        let points: Vec<PointLight> = (0..2)
            .map(|i| PointLight::new(vec3(i as f32, 0.0, 0.0), Vec3::ONE, 1.0, 5.0))
            .collect();

        // 一盏都没勾 → 没有投影灯
        assert_eq!(pick_spot_shadow(&points, &[mk(false, 1.0)], true), None);
        // 关掉整个阴影开关 → 没有投影灯
        assert_eq!(pick_spot_shadow(&[], &[mk(true, 1.0)], false), None);
        // 第一盏勾了的赢（后面的勾不勾都不影响）
        let spots = vec![mk(false, 0.0), mk(true, 1.0), mk(true, 2.0)];
        assert_eq!(pick_spot_shadow(&points, &spots, true), Some((1, 3)));
        // 点光源先占名额：8 盏点光 + 一盏勾了的聚光 → 聚光进不了 uniform，也不投影
        let many: Vec<PointLight> = (0..8)
            .map(|i| PointLight::new(vec3(i as f32, 0.0, 0.0), Vec3::ONE, 1.0, 5.0))
            .collect();
        assert_eq!(pick_spot_shadow(&many, &spots, true), None);
        // 7 盏点光占 0..6，spots[0]（没勾）拿 7，spots[1]（勾了）要 8 → 超预算，
        // 于是**一盏都不投**（而不是"悄悄换成没勾的那盏"）
        assert_eq!(pick_spot_shadow(&many[..7], &spots, true), None);
        // 6 盏点光：spots[0] → 6、spots[1] → 7，勾了的那盏在预算内
        let six = &many[..6];
        assert_eq!(pick_spot_shadow(six, &spots, true), Some((1, 7)));
        // 计划里带的相机就是那盏灯的相机
        let plan = plan_spot_shadow(six, &spots, true).expect("应当有计划");
        assert_eq!(plan.spot_index, 1);
        assert_eq!(plan.packed_index, 7);
        let cam = spot_shadow_camera(&spots[1]);
        assert!(plan.camera.view_proj.approx_eq(&cam.view_proj));
    }

    /// 选灯规则必须与 `pack_local_lights` 的打包顺序一致：这是"影子位置不对"的根源。
    #[test]
    fn the_picked_index_matches_where_pack_local_lights_puts_the_spot() {
        use crate::renderer::pack_local_lights;
        let points: Vec<PointLight> = (0..3)
            .map(|i| PointLight::new(vec3(i as f32, 0.0, 0.0), Vec3::ONE, 1.0, 5.0))
            .collect();
        let spots = vec![
            light(30.0, 10.0),
            SpotLight::new(
                vec3(2.0, 5.0, 0.0),
                vec3(0.0, -1.0, 0.0),
                Vec3::ONE,
                1.0,
                10.0,
                10.0,
                20.0,
            )
            .with_cast_shadows(true),
        ];
        let (packed, n) = pack_local_lights(&points, &spots);
        let (spot_index, packed_index) = pick_spot_shadow(&points, &spots, true).expect("有投影灯");
        assert_eq!(n, 5);
        assert_eq!(spot_index, 1);
        assert_eq!(packed_index, 4);
        // 打包结果里那个位置确实是聚光灯，而且方向/位置就是那盏灯的
        assert_eq!(
            packed[packed_index].cone_inner[1], 1.0,
            "下标处必须是聚光灯"
        );
        assert_eq!(packed[packed_index].pos_range[0], 2.0);
        assert!((packed[packed_index].dir_outer[1] + 1.0).abs() < 1e-6);
    }
}
