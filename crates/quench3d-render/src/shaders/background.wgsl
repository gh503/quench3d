// 摄像头/视频背景：全屏三角形，画在天空之前、场景几何之后（深度不参与）。
// UV 由 CPU 按 cover/contain 算好，避免把 4:3 摄像头画面硬拉成 16:9。

struct Bg {
    scale: vec2<f32>,
    offset: vec2<f32>,
};

@group(0) @binding(0) var bg_tex: texture_2d<f32>;
@group(0) @binding(1) var bg_sampler: sampler;
@group(0) @binding(2) var<uniform> bg: Bg;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    // 一个大三角形盖住整个 NDC：(-1,-1), (3,-1), (-1,3)。
    let x = f32((index << 1u) & 2u);
    let y = f32(index & 2u);
    let p = vec2<f32>(x * 2.0 - 1.0, y * 2.0 - 1.0);
    var out: VsOut;
    out.position = vec4<f32>(p, 0.0, 1.0);
    // egui/摄像头纹理原点在左上，NDC 的 y 向上，所以 v 要翻。
    out.uv = vec2<f32>(p.x * 0.5 + 0.5, 0.5 - p.y * 0.5);
    return out;
}

struct FragOut {
    @location(0) color: vec4<f32>,
    // 主 pass 的第二个目标是水面 mask；背景不写 mask。
    @location(1) mask: vec4<f32>,
};

@fragment
fn fs_main(in: VsOut) -> FragOut {
    let uv = (in.uv - vec2<f32>(0.5)) * bg.scale + vec2<f32>(0.5) + bg.offset;
    var out: FragOut;
    out.mask = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        out.color = vec4<f32>(0.0, 0.0, 0.0, 1.0);
    } else {
        out.color = textureSample(bg_tex, bg_sampler, uv);
    }
    return out;
}
