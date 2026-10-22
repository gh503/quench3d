// 阴影投射：只用每实例的模型矩阵，深度写入阴影贴图。
//@include common

@group(0) @binding(0) var<uniform> frame: Frame;

// GPU 蒙皮调色板（与 pbr.wgsl 同一份 buffer，只是绑定组号不同）。
const MAX_SKIN_JOINTS: u32 = 128u;
struct SkinPalette {
    joints: array<mat4x4<f32>, 128>,
};
@group(1) @binding(0) var<uniform> skin_palette: SkinPalette;

fn skin_matrix(base: u32, joint_ids: vec4<u32>, weights: vec4<f32>) -> mat4x4<f32> {
    let total = weights.x + weights.y + weights.z + weights.w;
    if total <= 1e-8 {
        return mat4x4<f32>(vec4<f32>(1.0, 0.0, 0.0, 0.0), vec4<f32>(0.0, 1.0, 0.0, 0.0), vec4<f32>(0.0, 0.0, 1.0, 0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0));
    }
    var result = mat4x4<f32>(vec4<f32>(0.0), vec4<f32>(0.0), vec4<f32>(0.0), vec4<f32>(0.0));
    for (var k = 0u; k < 4u; k = k + 1u) {
        let w = weights[k];
        if w <= 0.0 {
            continue;
        }
        let index = base + joint_ids[k];
        if index >= MAX_SKIN_JOINTS {
            continue;
        }
        result = result + skin_palette.joints[index] * w;
    }
    return result * (1.0 / total);
}

struct ShadowIn {
    @location(0) position: vec3<f32>,
    @location(4) m0: vec4<f32>,
    @location(5) m1: vec4<f32>,
    @location(6) m2: vec4<f32>,
    @location(7) m3: vec4<f32>,
    @location(10) joints: vec4<u32>,
    @location(11) weights: vec4<f32>,
    @location(12) skin: vec4<f32>,
};

@vertex
fn vs_main(in: ShadowIn) -> @builtin(position) vec4<f32> {
    let model = mat4x4<f32>(in.m0, in.m1, in.m2, in.m3);
    var skin = mat4x4<f32>(vec4<f32>(1.0, 0.0, 0.0, 0.0), vec4<f32>(0.0, 1.0, 0.0, 0.0), vec4<f32>(0.0, 0.0, 1.0, 0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0));
    if in.skin.y > 0.5 {
        skin = skin_matrix(u32(in.skin.x), in.joints, in.weights);
    }
    return frame.shadow_matrix * model * skin * vec4<f32>(in.position, 1.0);
}
