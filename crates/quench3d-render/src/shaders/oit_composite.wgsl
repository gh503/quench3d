// 加权混合 OIT（WBOIT）的解析 pass：accum / max(accum.a, eps) 得到平均色，
// 1 - reveal 得到透射率。输出是**预乘感**的平均色 + 覆盖率 alpha，交给
// `src = One, dst = OneMinusSrcAlpha` 混合到已经解析过的 HDR 目标。
//
// 用 textureLoad 而不是采样：目标与屏幕逐像素对齐，不需要过滤，也不该受
// sampler 的边界模式影响（否则最外一圈会出现颜色渗漏）。

@group(0) @binding(0) var accum_tex: texture_2d<f32>;
@group(0) @binding(1) var reveal_tex: texture_2d<f32>;

struct Out {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> Out {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: Out;
    out.position = vec4<f32>(corner * 2.0 - vec2<f32>(1.0), 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let coord = vec2<i32>(position.xy);
    let accum = textureLoad(accum_tex, coord, 0);
    let reveal = textureLoad(reveal_tex, coord, 0);
    let color = accum.rgb / max(accum.a, 1e-4);
    let alpha = clamp(1.0 - reveal.r, 0.0, 1.0);
    return vec4<f32>(color, alpha);
}
