//! 级联阴影（CSM）的 CPU 侧数学：视锥切分 + 每一级的光源空间矩阵。
//!
//! 这里只做纯计算：给定相机与平行光，算出"每一级覆盖多远"和"每一级的光源矩阵"。
//! 渲染侧把 N 级（N ≤ 4）画进同一张深度图的 2x2 分块，主 pass 按视空间深度选级。
//!
//! 为什么把这部分单独拎出来：**它完全不需要显卡就能验收**。判据不是"代码看起来对"，而是
//! "把某一级的 8 个角点用那一级的矩阵变过去，必须全部落在单位立方体内"——这正是
//! "这一级真的罩住了那块视锥"的定义。见本文件的 `each_cascade_box_contains_its_own_slice`。

use crate::light::DirectionalLight;
use quench3d_math::{Mat4, Vec3};

/// 视锥切分（PSSM）：返回每一级的**远平面距离**（最后一个必然是 `far`）。
///
/// `lambda` 在"对数切分"（近处细、远处粗，阴影质量分布最均匀）与"均匀切分"之间插值：
/// `lambda = 1` 全对数，`0` 全均匀。`count = 1` 时就是不分级（返回 `[far]`）。
///
/// 边界都做了收敛：`near` 取正、`far` 不大于 `near` 时退化成单级，`lambda` 夹到 `[0, 1]`，
/// 这样调用方传什么进来都不会得到"非单调的切分点"（那会让选级逻辑选错块）。
pub fn splits(near: f32, far: f32, count: u32, lambda: f32) -> Vec<f32> {
    let count = count.clamp(1, 4);
    let near = if near.is_finite() && near > 0.0 {
        near
    } else {
        0.1
    };
    let far = if far.is_finite() && far > near {
        far
    } else {
        near + 1.0
    };
    let lambda = if lambda.is_finite() {
        lambda.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let mut out = Vec::with_capacity(count as usize);
    for i in 1..=count {
        let p = i as f32 / count as f32;
        let log = near * (far / near).powf(p);
        let uniform = near + (far - near) * p;
        let d = lambda * log + (1.0 - lambda) * uniform;
        out.push(d.clamp(near, far));
    }
    if let Some(last) = out.last_mut() {
        *last = far;
    }
    out
}

/// 相机视锥的世界空间角点：`[near 四点, far 四点]`（每面顺序：`(-1,-1) (1,-1) (1,1) (-1,1)`）。
///
/// 用 `inv_view_proj` 反投影 NDC 角点。WebGPU 的深度区间是 `[0, 1]`，所以近平面取 `z_ndc = 0`、
/// 远平面取 `z_ndc = 1`（这也是 `pbr.wgsl` 里判 `clip.z` 用 0~1 的原因）。
pub fn frustum_corners(inv_view_proj: &Mat4) -> [Vec3; 8] {
    let xs = [-1.0f32, 1.0, 1.0, -1.0];
    let ys = [-1.0f32, -1.0, 1.0, 1.0];
    let mut out = [Vec3::ZERO; 8];
    for i in 0..4 {
        for (plane, z) in [(0usize, 0.0f32), (1usize, 1.0f32)] {
            let clip = *inv_view_proj * quench3d_math::Vec4::new(xs[i], ys[i], z, 1.0);
            out[plane * 4 + i] = Vec3::new(clip.x, clip.y, clip.z) / clip.w;
        }
    }
    out
}

/// 从整锥角点里取出相机空间 `[d0, d1]`（距离）那一段的 8 个世界空间角点。
///
/// 沿"近角点 → 远角点"连线做线性插值是**正确**的：视空间深度是世界位置的线性函数，
/// 而连线本身在世界空间里是线性的，所以 `t = (d - near) / (far - near)` 一一对应。
pub fn slice_corners(corners: &[Vec3; 8], near: f32, far: f32, d0: f32, d1: f32) -> [Vec3; 8] {
    let span = (far - near).max(1e-6);
    let t0 = ((d0 - near) / span).clamp(0.0, 1.0);
    let t1 = ((d1 - near) / span).clamp(0.0, 1.0);
    let mut out = [Vec3::ZERO; 8];
    for i in 0..4 {
        out[i] = corners[i].lerp(corners[4 + i], t0);
        out[4 + i] = corners[i].lerp(corners[4 + i], t1);
    }
    out
}

/// 某一级角点的**包围球**（球心 + 半径）。
///
/// 用包围球而不是紧致的 OBB：球心随相机旋转平滑变化，不会像逐帧重算的紧致盒子那样抖动；
/// 代价是稍微多覆盖一点（阴影贴图利用率低一些），换来的是**旋转相机时阴影不跳**。
pub fn bounding_sphere(corners: &[Vec3; 8]) -> (Vec3, f32) {
    let mut center = Vec3::ZERO;
    for c in corners {
        center += *c;
    }
    center /= 8.0;
    let mut radius: f32 = 0.0;
    for c in corners {
        radius = radius.max((*c - center).length());
    }
    (center, radius.max(0.01))
}

/// 每一级的光源空间矩阵。
///
/// `split_far` 是 [`splits`] 的输出（每一级的远平面距离）；`tile_texels` 是**该级分块**的
/// 边长（像素），用于纹素对齐——级联画在图集的 2x2 分块里，所以它是整图边长的一半。
///
/// 深度范围取 `半径 * 2`（与单级路径一致），保证该级视锥内的物体不会被裁掉。
pub fn matrices(
    sun: &DirectionalLight,
    inv_view_proj: &Mat4,
    near: f32,
    far: f32,
    split_far: &[f32],
    tile_texels: f32,
) -> Vec<Mat4> {
    // 每一段**用自己的 2*radius 深度范围**（`shadow_matrix_with_map_size` 里
    // 正交远平面 = 2*radius + depth_range = 4*radius）。这有意为之：
    //
    // 归一化深度的尺度 ≈ 1/(4r)，而这一级的纹素在世界里的大小 ≈ 2r/tile，
    // 两者相除 r 就消掉了 —— 所以**一个固定的归一化 bias（`frame.params.y`）
    // 在每一级都恰好等于固定的纹素数**（1/(2*tile) × bias）。这正是单级与多级
    // 能共用同一个 bias 常量的原因，也是它不会"近处自阴影"的原因。
    // `one_normalized_bias_is_the_same_number_of_texels_in_every_cascade` 钉着这条不变式；
    // 谁要是把这里改成"各级共用同一个深度范围"，那条测试会立刻红。
    let corners = frustum_corners(inv_view_proj);
    let mut out = Vec::with_capacity(split_far.len());
    let mut prev = near;
    for d in split_far {
        let slice = slice_corners(&corners, near, far, prev, *d);
        let (center, radius) = bounding_sphere(&slice);
        out.push(sun.shadow_matrix_with_map_size(center, radius, radius * 2.0, tile_texels));
        prev = *d;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;
    use quench3d_math::{vec3, Vec4};

    /// 原点、看 -Z 的相机：`Camera::perspective` 的缺省位置是 `(0, 1, 5)`，
    /// 测试里显式钉住原点，否则"近平面在 z = -near"这种断言会莫名其妙地失败（这也确实绊了我一次）。
    fn camera() -> Camera {
        let mut cam = Camera::perspective(16.0 / 9.0);
        cam.position = Vec3::ZERO;
        cam.rotation = quench3d_math::Quat::IDENTITY;
        cam.near = 0.1;
        cam.far = 100.0;
        cam
    }

    #[test]
    fn splits_are_monotonic_and_end_at_far() {
        let s = splits(0.1, 100.0, 4, 0.5);
        assert_eq!(s.len(), 4);
        assert_eq!(*s.last().unwrap(), 100.0, "最后一级必须正好到 far");
        for w in s.windows(2) {
            assert!(w[1] > w[0], "切分点必须严格递增: {s:?}");
        }
        assert!(s[0] > 0.1 && s[0] < 100.0);

        // count = 1 = 不分级
        assert_eq!(splits(0.1, 100.0, 1, 0.5), vec![100.0]);
        // lambda = 0 → 均匀切分：第一级正好是 1/4 处
        let uniform = splits(0.0 + 0.1, 100.0, 4, 0.0);
        assert!(
            (uniform[0] - (0.1 + (100.0 - 0.1) * 0.25)).abs() < 1e-3,
            "{uniform:?}"
        );
        // lambda = 1 → 对数切分：第一级更近
        let log = splits(0.1, 100.0, 4, 1.0);
        assert!(
            log[0] < uniform[0],
            "对数切分近处更细: {log:?} vs {uniform:?}"
        );
        // 荒谬输入不许产出非单调结果
        let bad = splits(-1.0, f32::NAN, 4, f32::INFINITY);
        for w in bad.windows(2) {
            assert!(w[1] > w[0], "{bad:?}");
        }
    }

    #[test]
    fn frustum_corners_match_the_projection() {
        let cam = camera();
        let inv = cam.view_projection().inverse();
        let c = frustum_corners(&inv);
        // 默认相机在原点、看向 -Z：近平面角点 z ≈ -near，远平面 z ≈ -far
        for i in 0..4 {
            assert!((c[i].z + cam.near).abs() < 1e-3, "近平面: {:?}", c[i]);
            // 远平面在大坐标处（x 已经到 ±100 量级），f32 精度只到 1e-2 左右
            assert!(
                (c[4 + i].z + cam.far).abs() < 0.05,
                "远平面: {:?}",
                c[4 + i]
            );
        }
        // 近平面更窄
        assert!(c[0].x.abs() < c[4].x.abs());
        // 8 个点互不相同
        for i in 0..8 {
            for j in (i + 1)..8 {
                assert!((c[i] - c[j]).length() > 1e-4);
            }
        }
    }

    /// 验收判据：每一级的包围盒必须罩住**它自己那一段**视锥。
    #[test]
    fn one_normalized_bias_is_the_same_number_of_texels_in_every_cascade() {
        use crate::light::DirectionalLight;
        let cam = camera();
        let sun = DirectionalLight::default();
        let inv = cam.view_projection().inverse();
        let splits = splits(cam.near, cam.far, 4, 0.5);
        let corners = frustum_corners(&inv);
        let tile = 1024.0f32;
        let ms = matrices(&sun, &inv, cam.near, cam.far, &splits, tile);

        // 每一级：正交远平面 = 2*radius + depth_range = 4*radius（`shadow_matrix_with_map_size`），
        // 近平面 0.05。于是"1 单位归一化深度"在世界里 = far - near，
        // 而这一级的纹素在世界里 = 2*radius/tile。两者之比_r 会被消掉。
        let mut per_texel = Vec::new();
        let mut prev = cam.near;
        for (i, d) in splits.iter().enumerate() {
            let slice = slice_corners(&corners, cam.near, cam.far, prev, *d);
            let (c, radius) = bounding_sphere(&slice);
            let depth_extent = 4.0 * radius - 0.05;
            let texel_world = 2.0 * radius / tile;
            per_texel.push(texel_world / depth_extent);
            // 顺带用矩阵自身核一遍深度尺度：沿光线挪 1 米，归一化深度变化的倒数就是深度范围
            let dir = sun.direction.normalize_or_zero();
            let a = ms[i] * Vec4::new(c.x, c.y, c.z, 1.0);
            let b = ms[i] * Vec4::new(c.x - dir.x, c.y - dir.y, c.z - dir.z, 1.0);
            let measured_extent = 1.0 / (a.z - b.z).abs();
            assert!(
                (measured_extent - depth_extent).abs() / depth_extent < 1e-3,
                "第 {i} 级的深度范围与推导不符：矩阵 {measured_extent:.3}，推导 {depth_extent:.3}"
            );
            prev = *d;
        }
        // 各级之间的相等是**近似**的：正交近平面 0.05 米让比值变成
        // 1/(2*tile) * 1/(1 - 0.05/(4r))，r 越小的那级偏大一点（最近一级约百分之几）。
        // 不为了"精确"去改这个公式——重要的是各级**几乎相同**，所以一个 bias 常量
        // 在各级都约等于同一个纹素数。容差 3% 就是照着这个偏差定的。
        for (i, v) in per_texel.iter().enumerate() {
            let rel = (v - per_texel[0]).abs() / per_texel[0];
            assert!(
                rel < 0.03,
                "第 {i} 级每个纹素对应的归一化深度与第 0 级差 {:.1}%（{per_texel:?}）：\
                 同一个 bias 常量在各段会变成不同的纹素数",
                rel * 100.0
            );
        }
        // 极限值就是 1/(2*tile)：与切分、相机、半径都无关（r 被消掉了）
        let expect = 1.0 / (2.0 * tile);
        let rel = (per_texel[0] - expect).abs() / expect;
        assert!(
            rel < 0.03,
            "期望约 {expect}，实际 {}（差 {:.1}%）",
            per_texel[0],
            rel * 100.0
        );
    }

    #[test]
    fn each_cascade_box_contains_its_own_slice() {
        let mut cam = camera();
        // 让相机斜着看：这一步很关键，正对时会掩盖旋转相关的错误
        cam.rotation = quench3d_math::Quat::from_euler_xyz(-0.6, 0.4, 0.0);
        let inv = cam.view_projection().inverse();
        let corners = frustum_corners(&inv);
        let s = splits(cam.near, cam.far, 4, 0.5);
        let sun = DirectionalLight {
            direction: vec3(-0.4, -1.0, -0.3).normalize_or_zero(),
            ..Default::default()
        };
        let ms = matrices(&sun, &inv, cam.near, cam.far, &s, 1024.0);
        assert_eq!(ms.len(), 4);

        let mut prev = cam.near;
        for (i, d) in s.iter().enumerate() {
            let slice = slice_corners(&corners, cam.near, cam.far, prev, *d);
            for corner in slice {
                let clip = ms[i].transform_point3(corner);
                // 光源空间是 [-1,1]（x/y）与 [0,1]（z，WebGPU 深度）
                assert!(
                    clip.x.abs() <= 1.0 + 1e-3 && clip.y.abs() <= 1.0 + 1e-3,
                    "第 {i} 级没罩住角点 {corner:?} → {clip:?}"
                );
                assert!(
                    (0.0..=1.0).contains(&clip.z),
                    "第 {i} 级深度范围不对: {clip:?}"
                );
            }
            prev = *d;
        }
    }

    #[test]
    fn slice_corners_is_linear_in_view_depth() {
        let cam = camera();
        let inv = cam.view_projection().inverse();
        let corners = frustum_corners(&inv);
        let mid = slice_corners(
            &corners,
            cam.near,
            cam.far,
            cam.near,
            (cam.near + cam.far) * 0.5,
        );
        // 这一段的**近**边界就是整锥的近平面，**远**边界才是 z = -(near+far)/2
        assert!(
            (mid[0].z + cam.near).abs() < 1e-3,
            "这段的近边界应当就是整锥近平面: {:?}",
            mid[0]
        );
        let expected = -(cam.near + cam.far) * 0.5;
        assert!((mid[4].z - expected).abs() < 1e-2, "{:?}", mid[4]);
        let _ = Vec4::ZERO;
    }
}
