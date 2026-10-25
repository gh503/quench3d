// 盒投影贴花：全屏三角形 + 场景深度重建世界坐标，再投进贴花的局部盒。
//
// 这样贴花会贴在任何**已经写进深度**的表面上（地面、斜坡、别的物体），
// 不是一张浮在空中的透明四边形。贴花盒的局部空间是 [-0.5, 0.5]^3，
// 沿局部 Y 轴投影（local.xz 就是 UV）。

struct DecalParams {
    inv_view_proj: mat4x4<f32>,
    inv_model: mat4x4<f32>,
    color: vec4<f32>,
};

@group(0) @binding(0) var decal_tex: texture_2d<f32>;
@group(0) @binding(1) var decal_sampler: sampler;
@group(0) @binding(2) var<uniform> params: DecalParams;
@group(0) @binding(3) var scene_depth: texture_depth_2d;

struct VsOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
    // 覆盖屏幕的大三角形；uv 的 y 向下，和后处理一致。
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    let p = positions[index];
    var out: VsOut;
    out.position = vec4<f32>(p, 0.0, 1.0);
    out.uv = vec2<f32>((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let coord = vec2<i32>(in.position.xy);
    let depth = textureLoad(scene_depth, coord, 0);
    if (depth >= 1.0) {
        discard;
    }
    let ndc = vec3<f32>(in.uv.x * 2.0 - 1.0, 1.0 - in.uv.y * 2.0, depth);
    let world4 = params.inv_view_proj * vec4<f32>(ndc, 1.0);
    let world = world4.xyz / world4.w;
    let local = params.inv_model * vec4<f32>(world, 1.0);
    if (any(abs(local.xyz) > vec3<f32>(0.5))) {
        discard;
    }
    let sample = textureSample(decal_tex, decal_sampler, local.xz + vec2<f32>(0.5));
    let alpha = sample.a * params.color.a;
    if (alpha <= 0.001) {
        discard;
    }
    return vec4<f32>(sample.rgb * params.color.rgb, alpha);
}
