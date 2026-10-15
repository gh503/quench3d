// 调试线段：坐标轴、包围盒、接触点、射线等。
//@include common

@group(0) @binding(0) var<uniform> frame: Frame;

struct LineIn {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct LineOut {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(in: LineIn) -> LineOut {
    var out: LineOut;
    out.position = frame.view_proj * vec4<f32>(in.position, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: LineOut) -> @location(0) vec4<f32> {
    return in.color;
}
