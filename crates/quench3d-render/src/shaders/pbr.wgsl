// 主前向渲染：PBR（Cook–Torrance）+ 平行光阴影（级联/PCF）+ 点光源（含单盏立方体阴影）
// + 聚光灯（含单盏投影）+ 指数雾。
//@include common

@group(0) @binding(0) var<uniform> frame: Frame;
@group(1) @binding(0) var shadow_map: texture_depth_2d;
@group(1) @binding(1) var shadow_sampler: sampler_comparison;
// 聚光灯阴影图（第二张 2D 深度图，用同一个比较采样器）
@group(1) @binding(2) var spot_shadow_map: texture_depth_2d;
// 点光源阴影图：**6 层深度数组**（`+X, -X, +Y, -Y, +Z, -Z`），不是 `texture_depth_cube`
// ——面/uv 的约定由 `point_shadow.rs::cube_face_uv` 定义，这里逐字镜像它（见下面的注释）。
@group(1) @binding(3) var point_shadow_map: texture_depth_2d_array;
@group(2) @binding(0) var base_color_tex: texture_2d<f32>;
@group(2) @binding(1) var base_color_sampler: sampler;
@group(2) @binding(2) var normal_tex: texture_2d<f32>;
@group(2) @binding(3) var mr_tex: texture_2d<f32>;
@group(2) @binding(4) var<uniform> material: Material;
@group(2) @binding(5) var emissive_tex: texture_2d<f32>;
@group(2) @binding(6) var linear_sampler: sampler;

// GPU 蒙皮调色板：场景把本帧所有蒙皮物体的关节矩阵拼进同一块 uniform，
// 每个实例只带 (起始下标, 关节数)。128 个关节 = 8 KB；只满足 WebGL2
// 最低 4 KB uniform 块的设备需退回 CPU 蒙皮，超出的物体也自动退回。
const MAX_SKIN_JOINTS: u32 = 128u;
struct SkinPalette {
    joints: array<mat4x4<f32>, 128>,
};
@group(3) @binding(0) var<uniform> skin_palette: SkinPalette;
// IBL：预滤波 cubemap 挂在 group 3（WebGL2 的 bind group 上限是 4 个）。
// 没有贴图时 y=0，仍走解析式天空（旧场景像素不变）。
@group(3) @binding(1) var env_map: texture_cube<f32>;
@group(3) @binding(2) var env_sampler: sampler;
@group(3) @binding(3) var<uniform> env_params: vec4<f32>;

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

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) tangent: vec4<f32>,
    // 每实例数据
    @location(4) m0: vec4<f32>,
    @location(5) m1: vec4<f32>,
    @location(6) m2: vec4<f32>,
    @location(7) m3: vec4<f32>,
    @location(8) albedo: vec4<f32>,
    // x = 金属度覆盖, y = 粗糙度覆盖, z = 自发光强度, w = 标志位(bit0: 覆盖材质)
    @location(9) params: vec4<f32>,
    // 顶点关节下标 / 权重（非蒙皮网格全 0）
    @location(10) joints: vec4<u32>,
    @location(11) weights: vec4<f32>,
    // 每实例的蒙皮范围：x = 调色板起始下标，y = 关节数（0 = 不做蒙皮）
    @location(12) skin: vec4<f32>,
};

struct VertexOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) world_position: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) world_tangent: vec3<f32>,
    @location(4) tangent_sign: f32,
    @location(5) albedo: vec4<f32>,
    @location(6) params: vec4<f32>,
    // 1 = 接收阴影，0 = 这层物体不采样任何阴影图（环境光不受影响）。
    @location(7) receive_shadows: f32,
};

// 3x3 逆转置（法线矩阵），避免在 WGSL 里依赖 inverse()。
fn normal_matrix(m: mat3x3<f32>) -> mat3x3<f32> {
    let a = m[0];
    let b = m[1];
    let c = m[2];
    let cof0 = cross(b, c);
    let cof1 = cross(c, a);
    let cof2 = cross(a, b);
    let det = dot(a, cof0);
    let s = select(1.0, -1.0, abs(det) < 1e-8);
    let inv_det = s / select(det, 1.0, abs(det) < 1e-8);
    // adjugate 的转置 = cofactor 矩阵
    return mat3x3<f32>(
        vec3<f32>(cof0.x, cof1.x, cof2.x) * inv_det,
        vec3<f32>(cof0.y, cof1.y, cof2.y) * inv_det,
        vec3<f32>(cof0.z, cof1.z, cof2.z) * inv_det,
    );
}

@vertex
fn vs_main(in: VertexIn) -> VertexOut {
    let model = mat4x4<f32>(in.m0, in.m1, in.m2, in.m3);
    var skin = mat4x4<f32>(vec4<f32>(1.0, 0.0, 0.0, 0.0), vec4<f32>(0.0, 1.0, 0.0, 0.0), vec4<f32>(0.0, 0.0, 1.0, 0.0), vec4<f32>(0.0, 0.0, 0.0, 1.0));
    if in.skin.y > 0.5 {
        skin = skin_matrix(u32(in.skin.x), in.joints, in.weights);
    }
    let world = model * skin * vec4<f32>(in.position, 1.0);
    let linear = mat3x3<f32>(model[0].xyz, model[1].xyz, model[2].xyz)
        * mat3x3<f32>(skin[0].xyz, skin[1].xyz, skin[2].xyz);
    let nm = normal_matrix(linear);

    var out: VertexOut;
    out.clip_position = frame.view_proj * world;
    out.world_position = world.xyz;
    out.world_normal = normalize(nm * in.normal);
    out.uv = in.uv;
    out.world_tangent = normalize(nm * in.tangent.xyz);
    out.tangent_sign = in.tangent.w;
    out.albedo = in.albedo;
    out.params = in.params;
    out.receive_shadows = in.skin.z;
    return out;
}

// ---- BRDF ----
fn distribution_ggx(n_dot_h: f32, roughness: f32) -> f32 {
    let a = roughness * roughness;
    let a2 = a * a;
    let d = n_dot_h * n_dot_h * (a2 - 1.0) + 1.0;
    return a2 / max(PI * d * d, 1e-6);
}

fn geometry_schlick_ggx(n_dot_x: f32, roughness: f32) -> f32 {
    let r = roughness + 1.0;
    let k = (r * r) / 8.0;
    return n_dot_x / (n_dot_x * (1.0 - k) + k);
}

fn geometry_smith(n_dot_v: f32, n_dot_l: f32, roughness: f32) -> f32 {
    return geometry_schlick_ggx(n_dot_v, roughness) * geometry_schlick_ggx(n_dot_l, roughness);
}

fn fresnel_schlick(cos_theta: f32, f0: vec3<f32>) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
}

fn fresnel_schlick_roughness(cos_theta: f32, f0: vec3<f32>, roughness: f32) -> vec3<f32> {
    let f90 = max(vec3<f32>(1.0 - roughness), f0);
    return f0 + (f90 - f0) * pow(clamp(1.0 - cos_theta, 0.0, 1.0), 5.0);
}

// 3x3 比较采样：平行光阴影与聚光灯阴影**共用这一份**（同一张深度格式、同一个比较采样器）。
// `uv` 是贴图上的采样中心，`depth` 是接收面的归一化深度，`texel` 是纹素大小。
//
// 贴图作为参数传进来（而不是写死 `shadow_map`）：聚光灯用的是**另一张**深度图，
// 写死的话聚光那条路会拿自己的 uv 去采平行光的图 —— 采到的是"别的深度"，
// 表现为整片区域半明半暗（本机探针第一版就是这么发现的：无遮挡物时 79% 的受光像素被压暗）。
//
// **必须用 `textureSampleCompareLevel`（= HLSL 的 `SampleCmpLevelZero`），不要改回
// `textureSampleCompare`**：后者要算隐式梯度，而这里是在循环里、又被聚光灯那段的
// `if (i == 投影灯)` 包着。Windows/D3D 走 FXC 编译，FXC 对"非一致控制流里的隐式梯度采样"
// 是**硬报错**（X3570 → D3DCompile 0x80004005），整个 fragment shader 编译失败、
// 所有要吃 GPU 的 CLI 测试在 Windows 腿全红；Linux/macOS 走 SPIR-V/MSL 都不报错，
// 所以本地门禁抓不到。深度图没有 mip（`mip_level_count: 1`），LOD 0 与隐式 LOD 结果完全相同。
fn pcf_3x3(tex: texture_depth_2d, uv: vec2<f32>, depth: f32, texel: f32) -> f32 {
    var sum = 0.0;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel;
            sum = sum + textureSampleCompareLevel(tex, shadow_sampler, uv + offset, depth);
        }
    }
    return sum / 9.0;
}

// 阴影：3x3 PCF 比较采样（多级时先按视空间深度选级，再映射到图集的 2x2 分块）
fn sample_shadow(world: vec3<f32>, n_dot_l: f32) -> f32 {
    let bias = frame.params.y * (1.0 - n_dot_l * 0.8);
    let cascades = u32(frame.cascade_info.x + 0.5);
    var m = frame.shadow_matrix;
    var tile = vec2<f32>(0.0, 0.0);
    var scale = 1.0;
    if (cascades > 1u) {
        // 视空间深度：取矩阵第 3 行即可（等价于 -(view * world).z）
        let depth = -(frame.view * vec4<f32>(world, 1.0)).z;
        var i = 0u;
        loop {
            if (i + 1u >= cascades) { break; }
            if (depth < frame.cascade_splits[i]) { break; }
            i = i + 1u;
        }
        m = frame.shadow_matrices[i];
        scale = frame.cascade_info.y;
        tile = vec2<f32>(f32(i % 2u), f32(i / 2u)) * scale;
    }
    let clip = m * vec4<f32>(world, 1.0);
    // WebGPU 的帧缓冲 Y 轴向下：NDC 的 +Y 对应纹理第 0 行，
    // 因此采样 uv 的 V 必须翻转，否则会采到镜像位置的深度（表现为自阴影）。
    let uv_local = vec2<f32>(clip.x * 0.5 + 0.5, 0.5 - clip.y * 0.5);
    // 越界判据用**分块内**的 uv：否则 PCF 会越过边界采到隔壁那一级的深度（接缝）
    if (uv_local.x < 0.0 || uv_local.x > 1.0 || uv_local.y < 0.0 || uv_local.y > 1.0 || clip.z < 0.0 || clip.z > 1.0) {
        return 1.0;
    }
    let uv = tile + uv_local * scale;
    return pcf_3x3(shadow_map, uv, clip.z - bias, frame.params.z);
}

// 3x3 比较采样（**数组**版本）：与 `pcf_3x3` 形状一致，只多一个 layer 参数。
// 同样**必须**用 `textureSampleCompareLevel`（= HLSL 的 `SampleCmpLevelZero`）：这一份
// 被点光源那段 `if (i == 投影灯)` 包着，属于非一致控制流，D3D/FXC 对隐式梯度采样硬报错
// （第 54 轮 windows 腿就是这么红的）。深度数组没有 mip，LOD 0 与隐式 LOD 等价。
fn pcf_3x3_array(
    tex: texture_depth_2d_array,
    uv: vec2<f32>,
    layer: i32,
    depth: f32,
    texel: f32,
) -> f32 {
    var sum = 0.0;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel;
            sum = sum + textureSampleCompareLevel(tex, shadow_sampler, uv + offset, layer, depth);
        }
    }
    return sum / 9.0;
}

// 方向 → `(面, u, v)`。**必须与 `crates/quench3d-render/src/point_shadow.rs::cube_face_uv`
// 逐字一致**：那边有一条 CPU 测试断言"这六个分支算出的 uv == 六个面矩阵投出来的 uv"，
// 所以两边任何一处符号/层序错都会在**没有显卡的 CI 上**变红（这正是选数组而不是
// 硬件 cube 采样的理由：硬件约定只能靠跑图试）。
//
// 返回值：x = 面号（0..5，喂给 `texture_depth_2d_array` 的 layer），y = u，z = v。
// uv 是纹理空间（v 向下，与 2D 阴影图的 `0.5 - ndc.y * 0.5` 同一约定）。
fn cube_face_uv(dir: vec3<f32>) -> vec3<f32> {
    if (dot(dir, dir) < 1e-12) {
        // 正好在灯心上：没有可比的深度，退化成 -Y 面的中心
        return vec3<f32>(3.0, 0.5, 0.5);
    }
    let a = abs(dir);
    // 每面的 fov 是 90° × 余量：横向分量要先除以余量（= tan(半面角)）再映射到 uv，
    // 边界方向于是落在 1/余量 而不是 1.0，给 3x3 PCF 留出边界
    let m = frame.point_shadow_texel.y;
    if (a.x >= a.y && a.x >= a.z) {
        let ma = a.x;
        if (dir.x > 0.0) {
            return vec3<f32>(
                0.0,
                0.5 * (1.0 + dir.z / ma / m),
                0.5 * (1.0 - dir.y / ma / m),
            );
        }
        return vec3<f32>(
            1.0,
            0.5 * (1.0 - dir.z / ma / m),
            0.5 * (1.0 - dir.y / ma / m),
        );
    }
    if (a.y >= a.z) {
        let ma = a.y;
        if (dir.y > 0.0) {
            return vec3<f32>(
                2.0,
                0.5 * (1.0 - dir.x / ma / m),
                0.5 * (1.0 + dir.z / ma / m),
            );
        }
        return vec3<f32>(
            3.0,
            0.5 * (1.0 - dir.x / ma / m),
            0.5 * (1.0 - dir.z / ma / m),
        );
    }
    let ma = a.z;
    if (dir.z > 0.0) {
        return vec3<f32>(
            4.0,
            0.5 * (1.0 - dir.x / ma / m),
            0.5 * (1.0 - dir.y / ma / m),
        );
    }
    return vec3<f32>(
        5.0,
        0.5 * (1.0 + dir.x / ma / m),
        0.5 * (1.0 - dir.y / ma / m),
    );
}

// 点光源阴影（立方体阴影）：6 层数组 + 上面那份数组版 PCF。
//
// 与聚光那条路的差别：
//   1. "深度"是**主分量**（垂直于被选中那个面的距离），所以只用一个 near/far；
//   2. uv 不是矩阵投影出来的，而是 `cube_face_uv` 算出来的（渲染那 6 张图用的是
//      同一套约定，见 `point_shadow.rs`）；
//   3. 没有视锥边界，取而代之的是"面内 uv 越界 = 这一面不负责这个方向"。
fn sample_point_shadow(
    light_index: i32,
    world: vec3<f32>,
    n: vec3<f32>,
    n_dot_l: f32,
) -> f32 {
    let idx = frame.point_shadow_params.x;
    // idx < 0 = 这一帧没有投影点光源（缺省路径：整个函数体不会被走到）
    if (idx < 0.0) { return 1.0; }
    let lp = frame.point_lights[light_index].pos_range;
    if (dot(world - lp.xyz, world - lp.xyz) <= 1e-12) { return 1.0; }
    let near = frame.point_shadow_params.y;
    let far = frame.point_shadow_params.z;
    let texel = frame.point_shadow_texel.x;
    let slope = 1.0 - clamp(n_dot_l, 0.0, 1.0) * frame.point_shadow_texel.w;

    // 法线偏移：沿接收面法线把采样点推开（斜面上最有效）。一个纹素的**世界**大小
    // 在透视下与"轴深"（主分量）成正比，所以偏移的单位必须是世界长度而不是深度值。
    let axis0 = max(max(abs(world.x - lp.x), abs(world.y - lp.y)), abs(world.z - lp.z));
    let texel_world0 = 2.0 * frame.point_shadow_texel.y * max(axis0, near) * texel;
    let shifted = world + n * (texel_world0 * frame.point_shadow_texel.z);
    let d = shifted - lp.xyz;
    let axis_depth = max(max(abs(d.x), abs(d.y)), abs(d.z));
    if (axis_depth <= near || axis_depth >= far) { return 1.0; }

    let texel_world = 2.0 * frame.point_shadow_texel.y * axis_depth * texel;
    // z_ndc(d) = far / (far - near) * (1 - near / d) → dz/dd = far * near / ((far - near) d²)
    let dz_dd = far * near / (max(far - near, 1e-6) * axis_depth * axis_depth);
    let depth_bias = texel_world * frame.point_shadow_params.w * dz_dd * slope;

    let face = cube_face_uv(d);
    let uv = vec2<f32>(face.y, face.z);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        // 余量之外：这个方向由别的面负责（或正好在面外），这一面没有可比的深度
        return 1.0;
    }
    let reference = far / (far - near) * (1.0 - near / axis_depth);
    return pcf_3x3_array(point_shadow_map, uv, i32(face.x + 0.5), reference - depth_bias, texel);
}

// 聚光灯阴影：一张 2D 深度图 + 上面那份 PCF。
//
// 与平行光那条路的差别只有三点：
//   1. **透视**投影：uv 与深度都要先除以 w（平行光是正交，w 恒为 1）；
//   2. 偏移在**世界空间**算（透视下纹素足迹随距离线性增长），再按 dz/dd 换算成
//      归一化深度——镜像的是 `spot_shadow.rs` 的 `normalized_depth_bias`；
//      常量不写死在这里，全部由 uniform 传进来（`spot_shadow_params` / `spot_shadow_texel`）；
//   3. 没有级联选择（只有一盏灯、一张图）。
fn sample_spot_shadow(world: vec3<f32>, n: vec3<f32>, n_dot_l: f32) -> f32 {
    let idx = frame.spot_shadow_params.x;
    // idx < 0 = 这一帧没有投影聚光灯（缺省路径：整个函数体不会被走到）
    if (idx < 0.0) { return 1.0; }
    let clip = frame.spot_shadow_matrix * vec4<f32>(world, 1.0);
    // 透视投影的 w = 沿光轴的深度；w <= 0 表示在灯后面/灯上，没有可比深度
    if (clip.w <= 1e-6) { return 1.0; }
    let axis_depth = clip.w;
    let texel = frame.spot_shadow_texel.x;
    // 一个纹素在该距离上的世界大小（透视：与距离成正比）
    let texel_world = 2.0 * frame.spot_shadow_texel.y * axis_depth * texel;
    let near = frame.spot_shadow_params.y;
    let far = frame.spot_shadow_params.z;
    // z_ndc(d) = far / (far - near) * (1 - near / d) → dz/dd = far * near / ((far - near) d²)
    let dz_dd = far * near / (max(far - near, 1e-6) * axis_depth * axis_depth);
    // 深度偏移：世界空间固定纹素数 × 斜率因子（正对光源的面几乎不用偏移）
    let slope = 1.0 - clamp(n_dot_l, 0.0, 1.0) * frame.spot_shadow_texel.w;
    let depth_bias = texel_world * frame.spot_shadow_params.w * dz_dd * slope;
    // 法线偏移：沿接收面法线把采样点推开，斜面上最有效
    let shifted = world + n * (texel_world * frame.spot_shadow_texel.z);
    let clip2 = frame.spot_shadow_matrix * vec4<f32>(shifted, 1.0);
    if (clip2.w <= 1e-6) { return 1.0; }
    let ndc = clip2.xyz / clip2.w;
    // 与 sample_shadow 同一套 V 翻转与越界判据（锥外/远平面外一律算"亮着"：
    // 那些地方本来就没有这盏灯的光，返回 1 不会画出多余阴影）
    let uv = vec2<f32>(ndc.x * 0.5 + 0.5, 0.5 - ndc.y * 0.5);
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 || ndc.z < 0.0 || ndc.z > 1.0) {
        return 1.0;
    }
    return pcf_3x3(spot_shadow_map, uv, ndc.z - depth_bias, texel);
}

// 解析式环境：与 `sky.wgsl` 同一条渐变 + 太阳光晕。
// 只在没有环境贴图（`env_params.y < 0.5`）时使用，保证旧场景像素不变。
fn analytic_environment(dir: vec3<f32>) -> vec3<f32> {
    let t = clamp(dir.y, -1.0, 1.0);
    var color = mix(frame.sky_horizon.rgb, frame.sky_zenith.rgb, smoothstep(0.0, 0.7, t));
    color = mix(color, frame.sky_ground.rgb, smoothstep(0.0, -0.35, t));
    let l = normalize(frame.light_dir.xyz);
    let cos_angle = max(dot(dir, l), 0.0);
    let glow = pow(cos_angle, 12.0) * 0.25 + pow(cos_angle, 3.0) * 0.05;
    let disc = smoothstep(0.9993, 0.9998, cos_angle) * 6.0;
    color = color + frame.light_color.rgb * frame.light_dir.w * (glow + disc);
    return color;
}

// 环境辐照/反射：有 IBL 贴图时按粗糙度取对应 mip；否则退回解析式天空。
// `roughness` 只影响贴图路径，解析路径保持原来的方向取值。
fn environment_radiance(dir: vec3<f32>, roughness: f32) -> vec3<f32> {
    if (env_params.y < 0.5) {
        return analytic_environment(dir);
    }
    let c = cos(env_params.w);
    let s = sin(env_params.w);
    let rotated = vec3<f32>(c * dir.x - s * dir.z, dir.y, s * dir.x + c * dir.z);
    let mip = clamp(roughness, 0.0, 1.0) * env_params.z;
    return textureSampleLevel(env_map, env_sampler, rotated, mip).rgb * env_params.x;
}

// Karis 的 split-sum 环境 BRDF 近似（不用 LUT）。
fn environment_brdf(f0: vec3<f32>, roughness: f32, n_dot_v: f32) -> vec3<f32> {
    let c0 = vec4<f32>(-1.0, -0.0275, -0.572, 0.022);
    let c1 = vec4<f32>(1.0, 0.0425, 1.04, -0.04);
    let r = roughness * c0 + c1;
    let a004 = min(r.x * r.x, exp2(-9.28 * n_dot_v)) * r.x + r.y;
    let ab = vec2<f32>(-1.04, 1.04) * a004 + r.zw;
    return f0 * ab.x + vec3<f32>(ab.y);
}

struct FragOut {
    @location(0) color: vec4<f32>,
    // 水面 mask（主 pass 第二颜色附件）：r = 1 表示水面，g = 水面世界高度；
    // 其余材质写全 0。g 用来在后处理里做屏幕射线-平面重建。
    @location(1) mask: vec4<f32>,
}

@fragment
fn fs_main(in: VertexOut, @builtin(front_facing) front_facing: bool) -> FragOut {
    let flags = u32(material.params.w + 0.5);
    let scale = max(material.params.z, 1e-4);
    // 先绕 (0.5, 0.5) 旋转，再平移，最后按 uv_scale 平铺。
    let centered = in.uv - vec2<f32>(0.5);
    let c = material.uv_transform.z;
    let s = material.uv_transform.w;
    let rotated = vec2<f32>(
        centered.x * c - centered.y * s,
        centered.x * s + centered.y * c,
    );
    let uv = (rotated + vec2<f32>(0.5) + material.uv_transform.xy) * scale;

    // 无条件采样，随后用标志位选择，保证控制流均匀。
    let base_sample = textureSample(base_color_tex, base_color_sampler, uv);
    let mr_sample = textureSample(mr_tex, linear_sampler, uv);
    let em_sample = textureSample(emissive_tex, linear_sampler, uv);
    let normal_sample = textureSample(normal_tex, linear_sampler, uv);

    let has_base = (flags & FLAG_BASE_COLOR_TEX) != 0u;
    let has_mr = (flags & FLAG_MR_TEX) != 0u;
    let has_em = (flags & FLAG_EMISSIVE_TEX) != 0u;
    let has_normal = (flags & FLAG_NORMAL_TEX) != 0u;

    var albedo = material.base_color_factor * in.albedo;
    if (has_base) {
        albedo = albedo * base_sample;
    }
    // 双面材质：背面翻转法线
    var n = normalize(in.world_normal);
    if (!front_facing) {
        n = -n;
    }
    if (has_normal) {
        let t = normalize(in.world_tangent - n * dot(n, in.world_tangent));
        let b = cross(n, t) * in.tangent_sign;
        let sampled = normal_sample.xyz * 2.0 - vec3<f32>(1.0);
        let scaled = vec3<f32>(sampled.xy * material.params2.y, sampled.z);
        n = normalize(mat3x3<f32>(t, b, n) * scaled);
    }

    let alpha = albedo.a;
    if (alpha < material.params2.x) {
        discard;
    }

    var metallic = material.params.x;
    var roughness = material.params.y;
    if ((in.params.w >= 0.5)) {
        metallic = in.params.x;
        roughness = in.params.y;
    }
    if (has_mr) {
        roughness = roughness * mr_sample.g;
        metallic = metallic * mr_sample.b;
    }
    roughness = clamp(roughness, 0.045, 1.0);
    metallic = clamp(metallic, 0.0, 1.0);

    let view_dir = normalize(frame.camera_pos.xyz - in.world_position);
    let n_dot_v = max(dot(n, view_dir), 1e-4);
    let f0 = mix(vec3<f32>(0.04), albedo.rgb, metallic);

    var lo = vec3<f32>(0.0);

    if ((flags & FLAG_UNLIT) == 0u) {
        // ---- 平行光 ----
        let l = normalize(frame.light_dir.xyz);
        let n_dot_l = dot(n, l);
        if (n_dot_l > 0.0 && frame.light_dir.w > 0.0) {
            var shadow = 1.0;
            if (frame.light_color.w > 0.5 && in.receive_shadows > 0.5) {
                shadow = sample_shadow(in.world_position, n_dot_l);
                shadow = mix(1.0, shadow, frame.params.w);
            }
            let h = normalize(view_dir + l);
            let n_dot_h = max(dot(n, h), 0.0);
            let ndf = distribution_ggx(n_dot_h, roughness);
            let g = geometry_smith(n_dot_v, n_dot_l, roughness);
            let f = fresnel_schlick(max(dot(h, view_dir), 0.0), f0);
            let specular = (ndf * g * f) / max(4.0 * n_dot_v * n_dot_l, 1e-4);
            let kd = (vec3<f32>(1.0) - f) * (1.0 - metallic);
            let radiance = frame.light_color.rgb * frame.light_dir.w * shadow;
            lo = lo + (kd * albedo.rgb / PI + specular) * radiance * n_dot_l;
        }

        // ---- 点光源 / 聚光灯（同一条数组：dir_outer.w > 0 表示聚光灯）----
        let count = i32(frame.params2.x + 0.5);
        for (var i = 0; i < count; i = i + 1) {
            let lp = frame.point_lights[i].pos_range;
            let to_light = lp.xyz - in.world_position;
            let dist2 = max(dot(to_light, to_light), 1e-6);
            let radius = max(lp.w, 1e-3);
            let dist = sqrt(dist2);
            if (dist < radius) {
                let l2 = to_light / dist;
                // 聚光灯：外锥外直接丢弃，内外锥之间平滑过渡
                let outer_cos = frame.point_lights[i].dir_outer.w;
                var cone = 1.0;
                if (outer_cos > 0.0) {
                    let spot_dir = normalize(frame.point_lights[i].dir_outer.xyz);
                    // l2 是从表面指向灯的方向，所以与"照射方向"反向比较
                    let cos_angle = dot(-l2, spot_dir);
                    if (cos_angle < outer_cos) {
                        cone = 0.0;
                    } else {
                        let inner_cos = frame.point_lights[i].cone_inner.x;
                        cone = clamp(
                            (cos_angle - outer_cos) / max(inner_cos - outer_cos, 1e-4),
                            0.0,
                            1.0,
                        );
                    }
                }
                let n_dot_l2 = dot(n, l2);
                if (n_dot_l2 > 0.0 && cone > 0.0) {
                    // 平滑衰减：在半径处归零，避免硬边
                    let t = clamp(1.0 - pow(dist / radius, 4.0), 0.0, 1.0);
                    let atten = (t * t) / dist2;
                    // 只有"这一盏就是投影灯"才采样阴影图（点光/聚光各自整帧最多一盏，
                    // 两套资源互不相干，所以两盏灯可以同时投影）
                    var shadow2 = 1.0;
                    if (i == i32(frame.spot_shadow_params.x + 0.5) && in.receive_shadows > 0.5) {
                        shadow2 = sample_spot_shadow(in.world_position, n, n_dot_l2);
                    }
                    if (i == i32(frame.point_shadow_params.x + 0.5) && in.receive_shadows > 0.5) {
                        shadow2 = sample_point_shadow(i, in.world_position, n, n_dot_l2);
                    }
                    // 与平行光共用同一个"阴影强度"（0 = 不压暗；shadow2 == 1 时是恒等变换）
                    shadow2 = mix(1.0, shadow2, frame.params.w);
                    let h2 = normalize(view_dir + l2);
                    let ndf = distribution_ggx(max(dot(n, h2), 0.0), roughness);
                    let g = geometry_smith(n_dot_v, n_dot_l2, roughness);
                    let f = fresnel_schlick(max(dot(h2, view_dir), 0.0), f0);
                    let specular = (ndf * g * f) / max(4.0 * n_dot_v * n_dot_l2, 1e-4);
                    let kd = (vec3<f32>(1.0) - f) * (1.0 - metallic);
                    let radiance2 = frame.point_lights[i].color_intensity.rgb
                        * frame.point_lights[i].color_intensity.w
                        * atten
                        * cone
                        * shadow2;
                    lo = lo + (kd * albedo.rgb / PI + specular) * radiance2 * n_dot_l2;
                }
            }
        }

        // ---- 环境光（半球近似 + 粗糙度相关的菲涅尔）----
        let upness = n.y * 0.5 + 0.5;
        let sky = mix(frame.sky_horizon.rgb, frame.sky_zenith.rgb, clamp(upness, 0.0, 1.0));
        let ambient_irradiance = mix(frame.ambient.rgb * frame.ambient.w, sky, 0.65);
        let f_env = fresnel_schlick_roughness(n_dot_v, f0, roughness);
        let kd_env = (vec3<f32>(1.0) - f_env) * (1.0 - metallic);
        if (frame.cascade_info.w > 0.5) {
            // 反射方向取环境色；粗糙度用"反射色 → 环境光"的混合代替预滤波。
            // 对金属这是主要能量来源，对电介质只补一点高光。
            let reflected = reflect(-view_dir, n);
            let env_diffuse = mix(ambient_irradiance, environment_radiance(n, 1.0), 0.65);
            let env_reflection = mix(
                environment_radiance(reflected, roughness),
                ambient_irradiance,
                roughness * roughness,
            );
            let env_spec = env_reflection * environment_brdf(f0, roughness, n_dot_v);
            lo = lo + kd_env * albedo.rgb * env_diffuse;
            lo = lo + env_spec;
        } else {
            lo = lo + kd_env * albedo.rgb * ambient_irradiance;
            lo = lo + f_env * ambient_irradiance * (1.0 - roughness) * (0.35 + 0.65 * metallic);
        }
    } else {
        lo = albedo.rgb;
    }

    // ---- 自发光 ----
    var emissive = material.emissive_factor.rgb * material.emissive_factor.a * in.params.z;
    if (has_em) {
        emissive = emissive * (em_sample.rgb * em_sample.a);
    }
    lo = lo + emissive;

    // ---- 指数距离雾 ----
    if (frame.fog.w > 0.0) {
        let dist = length(frame.camera_pos.xyz - in.world_position);
        let fog_amount = 1.0 - exp(-frame.fog.w * dist);
        // 用天空色做雾色，地平线方向更浓
        let dir = normalize(in.world_position - frame.camera_pos.xyz);
        let haze = mix(frame.fog.rgb, frame.sky_horizon.rgb, clamp(1.0 - abs(dir.y), 0.0, 1.0) * 0.5);
        lo = mix(lo, haze, clamp(fog_amount, 0.0, 1.0));
    }

    lo = lo * frame.params2.z; // 曝光
    // 输出**线性 HDR**：tonemap 由后处理统一做（见 post.wgsl）。
    // 这样半透明/网格线的混合发生在线性空间，而不是"tonemap 之后再混"。
    var out: FragOut;
    out.color = vec4<f32>(lo, alpha);
    // alpha = 覆盖（0 或 1）：alpha 管线的 mask 走 ALPHA_BLENDING（为了避开
    // WebGL2 没有 INDEPENDENT_BLEND），非水面 alpha=0 保持目标值，水面 alpha=1 覆盖。
    out.mask = vec4<f32>(
        material.params2.w,
        in.world_position.y,
        0.0,
        material.params2.w,
    );
    return out;
}
