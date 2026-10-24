// 时域抗锯齿（TAA）：把当前 HDR 与上一帧历史按重投影混合。
//
// 优先读运动向量 pass 的上一帧 UV；没有写的像素（天空/网格/线段）回退到
// "当前深度 + 逆 view-proj → 世界 → 上一帧 view-proj"的反投影。
// 运动向量只按当前姿态算，蒙皮/变形动画仍会靠邻域 clamp 抑制拖影。
//
// 相机每帧有亚像素抖动（见 `Renderer::taa_jitter`），历史样本因此能覆盖
// 不同的子像素位置，达到抗锯齿；抖动偏移在 CPU 侧乘进 view-proj。

struct TaaParams {
    // 当前帧（含抖动）的裁剪空间 → 世界。
    inv_view_proj: mat4x4<f32>,
    // 上一帧（含抖动）的世界 → 裁剪空间。
    prev_view_proj: mat4x4<f32>,
    // x = 历史是否有效（0/1），y = 历史权重（0~0.98），z = 是否用运动向量，w 预留。
    params: vec4<f32>,
    // xy = 1/目标尺寸，zw = 目标尺寸。
    texel: vec4<f32>,
};

@group(0) @binding(0) var current_tex: texture_2d<f32>;
@group(0) @binding(1) var linear_sampler: sampler;
@group(0) @binding(2) var<uniform> taa: TaaParams;
@group(0) @binding(3) var history_tex: texture_2d<f32>;
@group(0) @binding(4) var depth_texture: texture_depth_2d;
// 复用 post 布局的槽：水面 mask（TAA 不读）与运动向量。
@group(0) @binding(5) var water_mask_texture: texture_2d<f32>;
@group(0) @binding(6) var motion_texture: texture_2d<f32>;

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

fn load(coord: vec2<i32>) -> vec3<f32> {
    let size = vec2<i32>(taa.texel.zw);
    let c = clamp(coord, vec2<i32>(0), size - vec2<i32>(1));
    return textureLoad(current_tex, c, 0).rgb;
}

@fragment
fn fs_main(in: Out) -> @location(0) vec4<f32> {
    let coord = vec2<i32>(in.position.xy);
    let current = textureLoad(current_tex, coord, 0).rgb;

    // 第一帧/刚 resize：没有可信历史，直接输出当前帧。
    if (taa.params.x < 0.5) {
        return vec4<f32>(current, 1.0);
    }

    let depth = textureLoad(depth_texture, coord, 0);
    // 天空没有可反投影的几何；直接当前帧，避免天空被拖出黑边。
    if (depth >= 1.0) {
        return vec4<f32>(current, 1.0);
    }

    let motion = textureLoad(motion_texture, coord, 0);
    let uv = (in.position.xy) * taa.texel.xy;
    var prev_uv: vec2<f32>;
    if (taa.params.z > 0.5 && (motion.x != 0.0 || motion.y != 0.0)) {
        prev_uv = motion.xy;
    } else {
        let clip = vec4<f32>(uv * 2.0 - vec2<f32>(1.0), depth, 1.0);
        let world = taa.inv_view_proj * clip;
        if (abs(world.w) < 1e-6) {
            return vec4<f32>(current, 1.0);
        }
        let wp = world.xyz / world.w;
        let prev_clip = taa.prev_view_proj * vec4<f32>(wp, 1.0);
        if (prev_clip.w <= 1e-5) {
            return vec4<f32>(current, 1.0);
        }
        prev_uv = prev_clip.xy / prev_clip.w * 0.5 + vec2<f32>(0.5);
    }
    if (prev_uv.x < 0.0 || prev_uv.x > 1.0 || prev_uv.y < 0.0 || prev_uv.y > 1.0) {
        return vec4<f32>(current, 1.0);
    }

    // 当前像素 3x3 邻域 AABB：历史颜色被夹进去，抑制遮挡/运动边缘的拖影。
    var lo = current;
    var hi = current;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let c = load(coord + vec2<i32>(x, y));
            lo = min(lo, c);
            hi = max(hi, c);
        }
    }
    let history = clamp(
        textureSampleLevel(history_tex, linear_sampler, prev_uv, 0.0).rgb,
        lo,
        hi,
    );
    let weight = clamp(taa.params.y, 0.0, 0.98);
    return vec4<f32>(mix(current, history, weight), 1.0);
}
