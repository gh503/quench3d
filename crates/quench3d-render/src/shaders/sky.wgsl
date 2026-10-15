// 程序化天空：地平线渐变 + 大气感 + 太阳光晕。
//@include common

@group(0) @binding(0) var<uniform> frame: Frame;

struct SkyOut {
    @builtin(position) position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> SkyOut {
    // 全屏三角形：(-1,-1) / (3,-1) / (-1,3)
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    let ndc = corner * 2.0 - vec2<f32>(1.0);
    var out: SkyOut;
    out.position = vec4<f32>(ndc, 1.0, 1.0);
    out.ndc = ndc;
    return out;
}

@fragment
fn fs_main(in: SkyOut) -> @location(0) vec4<f32> {
    let far_point = frame.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let world = far_point.xyz / max(far_point.w, 1e-6);
    let dir = normalize(world - frame.camera_pos.xyz);

    // 天顶 → 地平线 → 地面
    let t = clamp(dir.y, -1.0, 1.0);
    var color = mix(frame.sky_horizon.rgb, frame.sky_zenith.rgb, smoothstep(0.0, 0.7, t));
    color = mix(color, frame.sky_ground.rgb, smoothstep(0.0, -0.35, t));

    // 太阳圆盘 + 光晕
    let l = normalize(frame.light_dir.xyz);
    let cos_angle = max(dot(dir, l), 0.0);
    let glow = pow(cos_angle, 12.0) * 0.25 + pow(cos_angle, 3.0) * 0.05;
    let disc = smoothstep(0.9993, 0.9998, cos_angle) * 6.0;
    color = color + frame.light_color.rgb * frame.light_dir.w * (glow + disc);

    color = color * frame.params2.z;
    // 线性 HDR：太阳圆盘可以远大于 1，tonemap 由后处理统一做（也才能被 bloom 拾取）。
    return vec4<f32>(color, 1.0);
}
