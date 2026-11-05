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

struct SkyFrag {
    @location(0) color: vec4<f32>,
    @location(1) mask: vec4<f32>,
};

// Hash integer cube-face cells, never screen pixels or camera position.
fn star_hash(cell: vec3<u32>) -> vec3<f32> {
    var h = cell * vec3<u32>(1597334677u, 3812015801u, 2798796415u);
    h = (h ^ h.yzx) * vec3<u32>(2246822519u);
    h = (h ^ (h >> vec3<u32>(16u))) * vec3<u32>(3266489917u);
    return vec3<f32>(h ^ (h >> vec3<u32>(16u))) / 4294967295.0;
}

fn space_stars(dir: vec3<f32>) -> vec3<f32> {
    // Cubical projection has no spherical pole singularity or longitude seam.
    // Jitter keeps stars away from cell/face edges; neighboring cells are sampled
    // so antialiased footprints remain continuous across internal cell borders.
    let a = abs(dir);
    var uv = dir.xy / a.z;
    var face = select(4u, 5u, dir.z < 0.0);
    if a.x >= a.y && a.x >= a.z {
        uv = dir.zy / a.x;
        face = select(0u, 1u, dir.x < 0.0);
    } else if a.y >= a.z {
        uv = dir.xz / a.y;
        face = select(2u, 3u, dir.y < 0.0);
    }
    let grid = (uv * 0.5 + vec2<f32>(0.5)) * 96.0;
    let cell = vec2<i32>(floor(grid));
    let pixel = max(length(fwidth(uv)), 0.000001);
    var stars = vec3<f32>(0.0);
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let neighbor = cell + vec2<i32>(x, y);
            if any(neighbor < vec2<i32>(0)) || any(neighbor >= vec2<i32>(96)) { continue; }
            let h = star_hash(vec3<u32>(vec2<u32>(neighbor), face + 1u));
            if h.x > 0.028 { continue; }
            let jitter = star_hash(vec3<u32>(vec2<u32>(neighbor), face + 17u));
            let point = ((vec2<f32>(neighbor) + 0.2 + jitter.xy * 0.6) / 96.0) * 2.0 - 1.0;
            let radius = mix(0.00035, 0.00115, jitter.z * jitter.z);
            let variance = radius * radius + pixel * pixel * 0.20;
            let delta = uv - point;
            let coverage = exp(-dot(delta, delta) / variance) * radius * radius / variance;
            let tint = mix(vec3<f32>(1.0, 0.84, 0.68), vec3<f32>(0.72, 0.85, 1.0), h.y);
            stars += tint * mix(1.0, 3.0, h.z) * coverage;
        }
    }
    return stars;
}

@fragment
fn fs_main(in: SkyOut) -> SkyFrag {
    let far_point = frame.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let world = far_point.xyz / max(far_point.w, 1e-6);
    let dir = normalize(world - frame.camera_pos.xyz);

    if frame.sky_zenith.w > 0.5 {
        // No horizon, solar disc or atmospheric glow in space. Configurable
        // colors supply only a subdued all-sky tint; stars are HDR points.
        let background = mix(frame.sky_ground.rgb, frame.sky_zenith.rgb, dir.y * 0.5 + 0.5);
        var space_out: SkyFrag;
        space_out.color = vec4<f32>((background + space_stars(dir)) * frame.params2.z, 1.0);
        space_out.mask = vec4<f32>(0.0);
        return space_out;
    }

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
    var out: SkyFrag;
    out.color = vec4<f32>(color, 1.0);
    out.mask = vec4<f32>(0.0);
    return out;
}
