// FXAA：在 tonemap **之后**的画面上找边缘、沿着边缘方向做一次定向模糊。
//
// 为什么放在 tonemap 之后：FXAA 的阈值是按**显示空间**定的（0..1 的感知亮度）。
// 在线性 HDR 上做，太阳/自发光动辄是 8、20，任何阈值判断都会失效。
// 为什么混合在**线性**空间做：中间的 LDR 目标也是 `rgba16float`，最后写进 sRGB 目标时
// 由硬件做一次编码。如果在着色器里先编码再混合，最后会被编码两次（8 位往返还会带条带）。
// 亮度判断仍然在 sRGB 空间做（`luma` 里先 `pow(1/2.2)`），与 FXAA 原版一致——
// 边缘是**感知**现象，不是线性光强的现象。
//
// 算法是 FXAA 3.11 的 console/低质量档（NVIDIA，Timothy Lottes，公共领域）：
//   1. 取中心 + 4 个对角共 5 个亮度，求 (min, max)；
//   2. 对比度低于阈值（相对阈值 `fxaa.y` 与绝对下限 `fxaa.z` 取大）→ 直接返回中心像素
//      （**平坦区域逐位不变**，这也是 FXAA 便宜的原因）；
//   3. 用对角亮度差估边缘方向 `dir`，按 |dir| 归一化并夹到 ±8 texel；
//   4. 沿 `dir` 取两对采样做混合，若混合结果的亮度跑出 [min, max] 就退回更保守的那次。
// 没做的是 FXAA 高质量档的"子像素锯齿消除"（subpixel aliasing removal）与 3×3 全邻域
// 方向估计——那需要更多采样，性价比不如直接开 MSAA。

@group(0) @binding(0) var src_texture: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var<uniform> post: PostParams;

// 与 `post.wgsl` / `bloom.wgsl` 的 `PostParams` 逐字节一致（同一个 uniform 缓冲）。
struct PostParams {
    // x = bloom 开关（0/1），y = bloom 强度，z = bloom 阈值，w = 预留
    params: vec4<f32>,
    // xy = bloom 贴图 texel 大小，zw = bloom 贴图尺寸
    bloom_texel: vec4<f32>,
    // xy = 输出目标 texel 大小，zw = 输出目标尺寸
    target_texel: vec4<f32>,
    // x = FXAA 开关（0/1），y = 相对边缘阈值，z = 绝对边缘阈值下限，w = 预留
    fxaa: vec4<f32>,
};

struct FxaaOut {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> FxaaOut {
    // 全屏三角形：(-1,-1) / (3,-1) / (-1,3)
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: FxaaOut;
    out.position = vec4<f32>(corner * 2.0 - vec2<f32>(1.0), 0.0, 1.0);
    return out;
}

/// 感知亮度：与 FXAA 原版一样在 sRGB 编码后的值上算。
fn luma(c: vec3<f32>) -> f32 {
    let encoded = pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.2));
    return dot(encoded, vec3<f32>(0.2126, 0.7152, 0.0722));
}

/// 读一个像素（显式 LOD 0：后处理是全屏 pass，不需要也不该用隐式导数）。
fn fetch(uv: vec2<f32>) -> vec3<f32> {
    return textureSampleLevel(src_texture, src_sampler, uv, 0.0).rgb;
}

@fragment
fn fs_main(in: FxaaOut) -> @location(0) vec4<f32> {
    let texel = post.target_texel.xy;
    let uv = in.position.xy * texel;

    let rgb_m = fetch(uv);
    let luma_m = luma(rgb_m);
    // 四个对角（FXAA 用对角差估方向；正十字邻域留给高质量档）
    let luma_nw = luma(fetch(uv + vec2(-1.0, -1.0) * texel));
    let luma_ne = luma(fetch(uv + vec2(1.0, -1.0) * texel));
    let luma_sw = luma(fetch(uv + vec2(-1.0, 1.0) * texel));
    let luma_se = luma(fetch(uv + vec2(1.0, 1.0) * texel));

    let luma_min = min(luma_m, min(min(luma_nw, luma_ne), min(luma_sw, luma_se)));
    let luma_max = max(luma_m, max(max(luma_nw, luma_ne), max(luma_sw, luma_se)));

    // 提前退出：对比度不够就不是边缘。相对阈值跟着亮度走（暗部更敏感），
    // 绝对下限压掉平坦区域里的单像素噪点。
    let threshold = max(post.fxaa.z, luma_max * post.fxaa.y);
    if (luma_max - luma_min < threshold) {
        return vec4<f32>(rgb_m, 1.0);
    }

    // 边缘方向：两个对角和的差。加 `dir_reduce` 是为了让"近乎水平/垂直"的边缘
    // 不要因为分母趋零而炸成很长的采样步长。
    var dir = vec2<f32>(
        -((luma_nw + luma_ne) - (luma_sw + luma_se)),
        (luma_nw + luma_sw) - (luma_ne + luma_se),
    );
    let dir_reduce = max(
        (luma_nw + luma_ne + luma_sw + luma_se) * (0.25 * 0.125),
        1.0 / 128.0,
    );
    let rcp_dir_min = 1.0 / (min(abs(dir.x), abs(dir.y)) + dir_reduce);
    // 步长最多 8 texel：再多就会把远处的细节也糊掉
    dir = clamp(dir * rcp_dir_min, vec2<f32>(-8.0), vec2<f32>(8.0)) * texel;

    // 两次混合：`rgb_a` 靠近中心（±1/6 步长，保守），`rgb_b` 再往外带一半（更平滑）
    let rgb_a = 0.5
        * (fetch(uv + dir * (1.0 / 3.0 - 0.5)) + fetch(uv + dir * (2.0 / 3.0 - 0.5)));
    let rgb_b = rgb_a * 0.5 + 0.25 * (fetch(uv - dir * 0.5) + fetch(uv + dir * 0.5));

    // 平滑结果的亮度必须落在原来的 [min, max] 之间，否则说明我们跨过了真正的边缘
    // （比如细线/文字），此时退回保守的 `rgb_a`。
    let luma_b = luma(rgb_b);
    if (luma_b < luma_min || luma_b > luma_max) {
        return vec4<f32>(rgb_a, 1.0);
    }
    return vec4<f32>(rgb_b, 1.0);
}
