// 后处理：把线性 HDR 中间目标变成最终画面（tonemap + bloom 合成）。
//
// 为什么要有这一趟：
//   * 物体着色器直接往 sRGB 目标写"已经 tonemap 过的值"，混合发生在 tonemap **之后**——
//     半透明与网格线的颜色是错的（业内叫"能量错"：等于二次 sRGB 编码）；
//   * 曝光/雾/自发光都在线性空间算，但 tonemap 原先散落在三个着色器里（pbr/sky/grid），
//     想加 bloom 就没有地方挂。
// 现在物体只输出**线性 HDR**，场景先渲染到 `rgba16float` 中间目标，再由这一趟统一处理。
// ACES 近似全项目**只此一份**。

@group(0) @binding(0) var hdr_texture: texture_2d<f32>;
@group(0) @binding(1) var hdr_sampler: sampler;
@group(0) @binding(2) var<uniform> post: PostParams;
@group(0) @binding(3) var bloom_texture: texture_2d<f32>;

struct PostParams {
    // x = bloom 开关（0/1），y = bloom 强度，z = bloom 阈值，w = 预留
    params: vec4<f32>,
    // xy = bloom 贴图 texel 大小，zw = bloom 贴图尺寸
    bloom_texel: vec4<f32>,
    // xy = 输出目标 texel 大小，zw = 输出目标尺寸
    // （bloom 是半分辨率，采样它必须用**目标**的 texel，否则是 2× 缩放，只读到一角）
    target_texel: vec4<f32>,
};

/// Narkowicz 2015 的 ACES 近似（全项目唯一一份）。
fn aces_tonemap(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

struct PostOut {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> PostOut {
    // 全屏三角形：(-1,-1) / (3,-1) / (-1,3)
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: PostOut;
    out.position = vec4<f32>(corner * 2.0 - vec2<f32>(1.0), 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: PostOut) -> @location(0) vec4<f32> {
    // 1:1 逐像素读取（`textureLoad`，不走采样器插值）：中间目标与输出同尺寸，
    // 于是"只多了一趟 tonemap"在数值上可复现，不会引入任何滤波误差。
    let coord = vec2<i32>(in.position.xy);
    var hdr = textureLoad(hdr_texture, coord, 0).rgb;

    // bloom 在 tonemap **之前**加回线性 HDR：亮区外溢会被同一套 tonemap 压回来，
    // 不会像"tonemap 后加"那样过曝成白块。
    if (post.params.x > 0.5) {
        let uv = in.position.xy * post.target_texel.xy;
        let bloom = textureSampleLevel(bloom_texture, hdr_sampler, uv, 0.0).rgb;
        hdr = hdr + bloom * post.params.y;
    }

    return vec4<f32>(aces_tonemap(hdr), 1.0);
}
