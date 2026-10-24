// TAA 运动向量：把当前顶点投到上一帧的屏幕空间。
//
// 只画主场景的实例（天空/网格/线段不写，TAA 在这些像素回退到深度反投影）。
// 每个实例带 `model` 与 `prev_model`；相机矩阵用当前/上一帧的 view-proj。
// 蒙皮/变形只按当前姿态算，变形动画仍可能拖影（真正的每顶点前一帧位置更贵）。

struct Motion {
    current_view_proj: mat4x4<f32>,
    prev_view_proj: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> motion: Motion;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(4) model_0: vec4<f32>,
    @location(5) model_1: vec4<f32>,
    @location(6) model_2: vec4<f32>,
    @location(7) model_3: vec4<f32>,
    @location(13) prev_0: vec4<f32>,
    @location(14) prev_1: vec4<f32>,
    @location(15) prev_2: vec4<f32>,
};

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) prev_clip: vec4<f32>,
};

fn model_matrix(c0: vec4<f32>, c1: vec4<f32>, c2: vec4<f32>, c3: vec4<f32>) -> mat4x4<f32> {
    return mat4x4<f32>(c0, c1, c2, c3);
}

fn prev_matrix(r0: vec4<f32>, r1: vec4<f32>, r2: vec4<f32>) -> mat4x4<f32> {
    return mat4x4<f32>(
        vec4<f32>(r0.x, r1.x, r2.x, 0.0),
        vec4<f32>(r0.y, r1.y, r2.y, 0.0),
        vec4<f32>(r0.z, r1.z, r2.z, 0.0),
        vec4<f32>(r0.w, r1.w, r2.w, 1.0),
    );
}

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    let model = model_matrix(in.model_0, in.model_1, in.model_2, in.model_3);
    let prev = prev_matrix(in.prev_0, in.prev_1, in.prev_2);
    let world = model * vec4<f32>(in.position, 1.0);
    let prev_world = prev * vec4<f32>(in.position, 1.0);
    var out: VertexOut;
    out.position = motion.current_view_proj * world;
    out.prev_clip = motion.prev_view_proj * prev_world;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    // w <= 0：上一帧在相机背后，写 0 让 TAA 回退到当前帧。
    if (in.prev_clip.w <= 1e-5) {
        return vec4<f32>(0.0);
    }
    let ndc = in.prev_clip.xy / in.prev_clip.w;
    let uv = ndc * 0.5 + vec2<f32>(0.5);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(uv, 0.0, 1.0);
}
