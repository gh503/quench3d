// 主前向渲染：PBR（Cook–Torrance）+ 平行光阴影（级联/PCF）+ 点光源 + 聚光灯（含单盏投影）+ 指数雾。
//@include common

@group(0) @binding(0) var<uniform> frame: Frame;
@group(1) @binding(0) var shadow_map: texture_depth_2d;
@group(1) @binding(1) var shadow_sampler: sampler_comparison;
// 聚光灯阴影图（第二张 2D 深度图，用同一个比较采样器）
@group(1) @binding(2) var spot_shadow_map: texture_depth_2d;
@group(2) @binding(0) var base_color_tex: texture_2d<f32>;
@group(2) @binding(1) var base_color_sampler: sampler;
@group(2) @binding(2) var normal_tex: texture_2d<f32>;
@group(2) @binding(3) var mr_tex: texture_2d<f32>;
@group(2) @binding(4) var<uniform> material: Material;
@group(2) @binding(5) var emissive_tex: texture_2d<f32>;
@group(2) @binding(6) var linear_sampler: sampler;

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
    let world = model * vec4<f32>(in.position, 1.0);
    let nm = normal_matrix(mat3x3<f32>(model[0].xyz, model[1].xyz, model[2].xyz));

    var out: VertexOut;
    out.clip_position = frame.view_proj * world;
    out.world_position = world.xyz;
    out.world_normal = normalize(nm * in.normal);
    out.uv = in.uv;
    out.world_tangent = normalize(nm * in.tangent.xyz);
    out.tangent_sign = in.tangent.w;
    out.albedo = in.albedo;
    out.params = in.params;
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

@fragment
fn fs_main(in: VertexOut, @builtin(front_facing) front_facing: bool) -> @location(0) vec4<f32> {
    let flags = u32(material.params.w + 0.5);
    let scale = max(material.params.z, 1e-4);
    let uv = in.uv * scale;

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
            if (frame.light_color.w > 0.5) {
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
                    // 只有"这一盏就是投影灯"才采样阴影图（整帧最多一盏，见 spot_shadow.rs）
                    var shadow2 = 1.0;
                    if (i == i32(frame.spot_shadow_params.x + 0.5)) {
                        shadow2 = sample_spot_shadow(in.world_position, n, n_dot_l2);
                        // 与平行光共用同一个"阴影强度"（0 = 不压暗）
                        shadow2 = mix(1.0, shadow2, frame.params.w);
                    }
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
        lo = lo + kd_env * albedo.rgb * ambient_irradiance;
        lo = lo + f_env * ambient_irradiance * (1.0 - roughness) * (0.35 + 0.65 * metallic);
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
    return vec4<f32>(lo, alpha);
}
