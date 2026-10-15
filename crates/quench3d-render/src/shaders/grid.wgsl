// 无限地面网格：屏幕空间射线与地面求交 + 导数抗锯齿，写入真实深度。
//@include common

@group(0) @binding(0) var<uniform> frame: Frame;

struct GridOut {
    @builtin(position) position: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> GridOut {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    let ndc = corner * 2.0 - vec2<f32>(1.0);
    var out: GridOut;
    out.position = vec4<f32>(ndc, 1.0, 1.0);
    out.ndc = ndc;
    return out;
}

struct GridFrag {
    @builtin(frag_depth) depth: f32,
    @location(0) color: vec4<f32>,
};

fn grid_line_coord(p: vec2<f32>, scale: f32, width: vec2<f32>) -> f32 {
    let c = p / scale;
    let grid = abs(fract(c - vec2<f32>(0.5)) - vec2<f32>(0.5)) * scale;
    let d = max(fwidth(c) * scale * 1.2, vec2<f32>(1e-5));
    let line = min(grid.x / d.x, grid.y / d.y);
    return 1.0 - min(line, 1.0);
}

@fragment
fn fs_main(in: GridOut) -> GridFrag {
    let far_point = frame.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let world = far_point.xyz / max(far_point.w, 1e-6);
    let origin = frame.camera_pos.xyz;
    let dir = world - origin;

    let ground_y = frame.params2.w;
    if (abs(dir.y) < 1e-6) {
        discard;
    }
    let t = (ground_y - origin.y) / dir.y;
    if (t <= 0.0 || t > 6000.0) {
        discard;
    }
    let p = origin + dir * t;
    let dist = length(p - origin);

    let minor = grid_line_coord(p.xz, 1.0, vec2<f32>(1.0));
    let major = grid_line_coord(p.xz, 10.0, vec2<f32>(1.0));

    // 距离淡出
    let fade = 1.0 - smoothstep(60.0, 260.0, dist);

    // 主轴高亮
    let dw = max(fwidth(p.xz) * 1.5, vec2<f32>(1e-4));
    let axis_z = 1.0 - min(abs(p.z) / dw.y, 1.0); // x 轴（z = 0）
    let axis_x = 1.0 - min(abs(p.x) / dw.x, 1.0); // z 轴（x = 0）

    var color = vec3<f32>(0.32) * minor * 0.45 + vec3<f32>(0.62) * major;
    color = mix(color, vec3<f32>(0.85, 0.28, 0.30), axis_z * 0.85);
    color = mix(color, vec3<f32>(0.28, 0.48, 0.95), axis_x * 0.85);

    // 雾/淡出混合到天空地平线色，避免远处突兀
    let horizon = frame.sky_horizon.rgb;
    let fog_amount = 1.0 - exp(-frame.fog.w * dist);
    color = mix(color, horizon, clamp(fog_amount, 0.0, 1.0));

    var alpha = max(max(minor * 0.35, major), max(axis_z, axis_x)) * fade;
    alpha = alpha * (1.0 - clamp(fog_amount * 1.2, 0.0, 1.0));
    if (alpha < 0.003) {
        discard;
    }

    let clip = frame.view_proj * vec4<f32>(p, 1.0);
    var out: GridFrag;
    out.depth = clamp(clip.z / max(clip.w, 1e-6), 0.0, 1.0);
    // 线性 HDR + alpha：网格线的混合因此发生在线性空间（见 post.wgsl）。
    out.color = vec4<f32>(color * frame.params2.z, alpha);
    return out;
}
