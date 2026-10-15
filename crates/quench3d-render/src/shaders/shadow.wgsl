// 阴影投射：只用每实例的模型矩阵，深度写入阴影贴图。
//@include common

@group(0) @binding(0) var<uniform> frame: Frame;

struct ShadowIn {
    @location(0) position: vec3<f32>,
    @location(4) m0: vec4<f32>,
    @location(5) m1: vec4<f32>,
    @location(6) m2: vec4<f32>,
    @location(7) m3: vec4<f32>,
};

@vertex
fn vs_main(in: ShadowIn) -> @builtin(position) vec4<f32> {
    let model = mat4x4<f32>(in.m0, in.m1, in.m2, in.m3);
    return frame.shadow_matrix * model * vec4<f32>(in.position, 1.0);
}
