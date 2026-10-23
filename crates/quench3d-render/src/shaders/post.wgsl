// 后处理：把线性 HDR 中间目标变成最终画面（可选 SSAO → tonemap + bloom 合成）。
//
// 为什么要有这一趟：
//   * 物体着色器直接往 sRGB 目标写"已经 tonemap 过的值"，混合发生在 tonemap **之后**——
//     半透明与网格线的颜色是错的（业内叫"能量错"：等于二次 sRGB 编码）；
//   * 曝光/雾/自发光都在线性空间算，但 tonemap 原先散落在三个着色器里（pbr/sky/grid），
//     想加 bloom 就没有地方挂。
// 现在物体只输出**线性 HDR**，场景先渲染到 `rgba16float` 中间目标，再由这一趟统一处理。
// 色调映射算子可切（ACES / Reinhard / 线性），见 `params.w`。
//
// SSAO 也挂在这一趟里（tonemap **之前**，因为它是线性光照上的一个乘子）：
// 深度-only 实现——法线由视空间位置的屏幕导数重建，半球采样按黄金角螺旋分布、
// 每像素旋转一个随机角，遮挡判定是"该屏幕位置上真实几何比采样点更靠近相机"。
// 乘在整幅 HDR 颜色上（不区分直接光/间接光），这是后处理式 SSAO 的固有简化。

@group(0) @binding(0) var hdr_texture: texture_2d<f32>;
@group(0) @binding(1) var hdr_sampler: sampler;
@group(0) @binding(2) var<uniform> post: PostParams;
@group(0) @binding(3) var bloom_texture: texture_2d<f32>;
/// Reinhard：x / (1 + x)。
fn reinhard_tonemap(x: vec3<f32>) -> vec3<f32> {
    return x / (vec3<f32>(1.0) + x);
}

/// 按 `params.w` 选算子：0 = ACES，1 = Reinhard，2 = 线性夹取。
fn tonemap(x: vec3<f32>) -> vec3<f32> {
    let c = max(x, vec3<f32>(0.0));
    let mode = post.params.w;
    if (mode < 0.5) {
        return aces_tonemap(c);
    }
    if (mode < 1.5) {
        return reinhard_tonemap(c);
    }
    return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
}

//@gl-depth-begin
// 深度（单采样；MSAA > 1 时是"解析"出来的那一张，见 `depth_resolve.wgsl`）。
// 只用 `textureLoad` 按像素读，所以不需要采样器——深度也不该被双线性插值。
@group(0) @binding(4) var depth_texture: texture_depth_2d;
//@gl-depth-end
// 水面 mask（r = 1）：主 pass 第二颜色附件解析后的结果；不依赖深度，GL 也能用。
@group(0) @binding(5) var water_mask_texture: texture_2d<f32>;

struct PostParams {
    // x = bloom 开关（0/1），y = bloom 强度，z = bloom 阈值，w = 预留
    params: vec4<f32>,
    // xy = bloom 贴图 texel 大小，zw = bloom 贴图尺寸
    bloom_texel: vec4<f32>,
    // xy = 输出目标 texel 大小，zw = 输出目标尺寸
    // （bloom 是半分辨率，采样它必须用**目标**的 texel，否则是 2× 缩放，只读到一角）
    target_texel: vec4<f32>,
    // x = FXAA 开关（0/1），y = 相对边缘阈值，z = 绝对边缘阈值下限，w = 预留
    // （这一趟不读，留给 `fxaa.wgsl`；三个后处理着色器共用同一个 uniform 缓冲）
    fxaa: vec4<f32>,
    // x = SSAO 开关（0/1），y = 半径（视空间米），z = 深度偏移 bias，w = 强度
    ssao: vec4<f32>,
    // x = 采样数，yzw = 预留
    ssao2: vec4<f32>,
    // 投影（视空间 → 裁剪空间）与逆投影：反投影位置、再把半球采样点投回屏幕
    proj: mat4x4<f32>,
    inv_proj: mat4x4<f32>,
    // x = 开关（0/1），y = 时间（秒），z = 强度，w = 预留
    water: vec4<f32>,
    // rgb = 水色（线性），a = 预留
    water_color: vec4<f32>,
    // 视空间 → 世界：用于由屏幕坐标算视线方向（菲涅耳）
    inv_view: mat4x4<f32>,
    // x = 泡沫强度，y = 折射强度，z = 太阳高光强度，w = 预留
    water_extra: vec4<f32>,
    // xyz = 太阳方向（从光源射向场景），w = 强度
    water_sun: vec4<f32>,
    // 景深：x = 开关（0/1），y = 对焦距离（视空间米），z = 光圈，w = 最大模糊像素
    dof: vec4<f32>,
};

/// Narkowicz 2015 的 ACES 近似。
fn aces_tonemap(x: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let c = 2.43;
    let d = 0.59;
    let e = 0.14;
    return clamp((x * (a * x + b)) / (x * (c * x + d) + e), vec3<f32>(0.0), vec3<f32>(1.0));
}

//@gl-depth-begin
/// 裁剪空间深度 → 视空间位置（相机看向 -Z，所以近处的 z 更大）。
fn view_pos(uv: vec2<f32>, depth: f32) -> vec3<f32> {
    let clip = vec4<f32>(uv * 2.0 - 1.0, depth, 1.0);
    let v = post.inv_proj * clip;
    return v.xyz / v.w;
}

/// 交错梯度噪声（Jorge Jimenez 的 interleaved gradient noise）：给每个像素一个固定的旋转角。
///
/// 这里刻意**不用**白噪声（hash）：同样是"每像素随机旋转"，白噪声在低采样数下表现为
/// 明显的颗粒感，而交错梯度噪声把误差摊成一个有规律的高频图案，眼睛更容易读成"纹理"
/// 而不是"噪点"。代价一样是 0：两次乘加 + 两次 fract。
/// 数值是原论文给的魔数，不要随手改（改了图案就散）。
fn interleaved_gradient(x: f32, y: f32) -> f32 {
    return fract(52.9829189 * fract(0.06711056 * x + 0.00583715 * y));
}

/// SSAO：返回 0~1 的遮蔽系数（1 = 完全没被遮）。
fn ssao_factor(coord: vec2<i32>, uv: vec2<f32>) -> f32 {
    let depth = textureLoad(depth_texture, coord, 0);
    // 远平面（没有几何）不参与：否则天空会被周围物体的深度"遮"出黑边
    if (depth >= 1.0) {
        return 1.0;
    }
    let p = view_pos(uv, depth);
    // 法线由深度重建：屏幕导数给出切平面上的两个向量，叉积即法线；朝相机的那一侧为准
    var n = normalize(cross(dpdy(p), dpdx(p)));
    if (n.z < 0.0) {
        n = -n;
    }

    let radius = post.ssao.y;
    let bias = post.ssao.z;
    let count = i32(post.ssao2.x);
    let rot = interleaved_gradient(f32(coord.x), f32(coord.y)) * 6.2831853;

    // 切空间基（随机旋转）：让螺旋图案每像素不同，避免规则条纹
    let t = vec3<f32>(cos(rot), sin(rot), 0.0);
    let b = cross(n, t);
    let screen_size = post.target_texel.zw;
    let texel = post.target_texel.xy;

    var occlusion = 0.0;
    for (var i = 0; i < count; i += 1) {
        // 黄金角螺旋 + 余弦加权半球（z 分量保证采样点在表面上方的半球内）
        let a = f32(i) * 2.3999632 + rot;
        let r = sqrt((f32(i) + 0.5) / f32(count));
        let k = vec3<f32>(cos(a) * r, sin(a) * r, sqrt(max(0.0, 1.0 - r * r)));
        let sp = p + (t * k.x + b * k.y + n * k.z) * radius;

        let clip = post.proj * vec4<f32>(sp, 1.0);
        let suv = (clip.xy / clip.w) * 0.5 + 0.5;
        // 采样点跑到屏幕外就跳过（不然会读到别的屏幕边缘的深度）
        if (suv.x < 0.0 || suv.x > 1.0 || suv.y < 0.0 || suv.y > 1.0) {
            continue;
        }
        // 夹到有效像素范围：`suv` 正好等于 1.0 时下标会越界（越界读出的值没有定义）
        let sc = clamp(
            vec2<i32>(suv * screen_size),
            vec2<i32>(0),
            vec2<i32>(screen_size) - vec2<i32>(1),
        );
        let sdepth = textureLoad(depth_texture, sc, 0);
        // 该屏幕位置上**真实**几何的视空间 z（比 p.z 大 = 比中心点更靠近相机）
        let dz = p.z - view_pos(suv, sdepth).z;
        // 遮挡判定："更靠近相机" + 距离落在 (`bias`, `radius`) 之间。
        // * 上界不能省：只比"更近"的话，掠射角下**同一个平面**会自己遮自己——切平面上的
        //   采样点滑到旁边，投回屏幕落在比中心点更近的地面上，整片远处地面被压暗
        //   （实测：远处平坦地面平均暗 3.5/255，加上上界后 2.2/255）。
        // * 权重用 `radius * 0.5` 收口（而不是直接到 `radius`）：正好在半径边缘的"遮挡物"
        //   多半就是那个平面自己，让它们的贡献归零，接缝处的遮挡（dz 小得多）几乎不受影响。
        // * 不用"把偏移投影到法线上"这种更漂亮的判据：它依赖重建出来的法线，而掠射角下
        //   法线误差最大 —— 实测反而把远处地面压暗到 11/255（越"正确"的公式越吃误差）。
        if (dz > bias && dz < radius) {
            occlusion += 1.0 - smoothstep(bias, radius * 0.5, dz);
        }
    }
    let ratio = occlusion / f32(count);
    return clamp(1.0 - ratio * post.ssao.w, 0.0, 1.0);
}
//@gl-depth-end

struct PostOut {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> PostOut {
    // 全屏三角形：(-1,-1) / (3,-1) / (-1,3)
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: PostOut;
    out.position = vec4<f32>(corner * 2.0 - vec2<f32>(1.0), 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: PostOut) -> @location(0) vec4<f32> {
    // 1:1 逐像素读取（`textureLoad`，不走采样器插值）：中间目标与输出同尺寸，
    // 于是"只多了一趟 tonemap"在数值上可复现，不会引入任何滤波误差。
    let coord = vec2<i32>(in.position.xy);
    let texel = post.target_texel.xy;
    // `in.position.xy` 已经是像素中心（+0.5），不用再加
    let uv = in.position.xy * texel;
    var hdr = textureLoad(hdr_texture, coord, 0).rgb;

    //@gl-depth-begin
    // SSAO 先乘上去（在 bloom 之前：自发光/亮区的外溢不该被环境光遮蔽压暗）
    if (post.ssao.x > 0.5) {
        hdr = hdr * ssao_factor(coord, uv);
    }
    //@gl-depth-end

    // bloom 在 tonemap **之前**加回线性 HDR：亮区外溢会被同一套 tonemap 压回来，
    // 不会像"tonemap 后加"那样过曝成白块。
    if (post.params.x > 0.5) {
        let bloom = textureSampleLevel(bloom_texture, hdr_sampler, uv, 0.0).rgb;
        hdr = hdr + bloom * post.params.y;
    }

    // ---------- 屏幕空间水面 ----------
    // mask 由水面平面写进主 pass 的第二颜色附件：r = 覆盖，g = 世界高度。
    // 这里用相机射线与水面的交点重建世界坐标，据此生成程序化波面法线，
    // 再做折射/吸收/菲涅耳/太阳高光/泡沫。仍然不读深度纹理（GL/WebGL2 可用）。
    if (post.water.x > 0.5) {
        let m = textureLoad(water_mask_texture, coord, 0).r;
        let visible = smoothstep(0.15, 0.85, m);
        if (visible > 0.001) {
            let tt = post.water.y;
            let wave = max(post.water.z, 0.0);
            let foam_strength = max(post.water.w, 0.0);
            let refraction = max(post.water_extra.y, 0.0);

            // 屏幕射线（世界空间）与水面高度（mask.g）。
            let ndc = vec2<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
            let ray = normalize((post.inv_view * vec4<f32>(ndc, -1.0, 0.0)).xyz);
            let cam = post.inv_view[3].xyz;
            let level = textureLoad(water_mask_texture, coord, 0).g;
            var wp = cam + ray * 10.0;
            if (abs(ray.y) > 1e-4) {
                let hit = (level - cam.y) / ray.y;
                if (hit > 0.0) {
                    wp = cam + ray * hit;
                }
            }

            // 两个八度的程序化波面斜率；波峰由法线倾角间接给出。
            let p = wp.xz;
            let sx = 0.10 * wave * cos(p.x * 6.0 + p.y * 2.0 + tt * 1.2)
                + 0.04 * wave * cos(p.x * 17.0 - p.y * 3.0 - tt * 2.1);
            let sz = 0.10 * wave * cos(p.y * 7.0 - p.x * 1.5 + tt * 1.7)
                + 0.04 * wave * cos(p.y * 19.0 + p.x * 4.0 + tt * 1.3);
            let n = normalize(vec3<f32>(-sx, 1.0, -sz));

            // 折射：沿法线在屏幕空间偏移背景；再叠一点随时间的细波纹。
            let ripple = vec2<f32>(
                sin(uv.y * 37.0 + tt * 2.0),
                cos(uv.x * 29.0 + tt * 1.7),
            ) * (2.5 * wave);
            let bend = vec2<f32>(n.x, -n.z) * (10.0 * refraction);
            let refracted = textureSampleLevel(
                hdr_texture, hdr_sampler, uv + (ripple + bend) * texel, 0.0,
            ).rgb;

            // 菲涅耳：F0 = 0.02（水），正视更透、掠射更亮。
            let view_dir = -ray;
            let cos_v = clamp(dot(n, view_dir), 0.0, 1.0);
            let fresnel = 0.02 + 0.98 * pow(1.0 - cos_v, 5.0);

            // 吸收：clarity 越高越清澈（吸收越弱）；固定厚度近似。
            let clarity = clamp(post.water_color.a, 0.0, 1.0);
            let absorb = mix(0.82, 0.34, clarity);
            let body = mix(refracted, refracted * post.water_color.rgb, absorb);
            let reflect_col = post.water_color.rgb * 1.15 + vec3<f32>(0.02);
            var water_col = mix(body, reflect_col, 0.06 + 0.58 * fresnel);

            // 太阳高光：用半程向量，只在太阳方向有效时计算。
            let sun_len = length(post.water_sun.xyz);
            if (sun_len > 1e-4) {
                // water_sun 是"从光源射向场景"的方向；半程向量要的是"指向光源"。
                let sun_dir = -post.water_sun.xyz / sun_len;
                let half_dir = normalize(sun_dir + view_dir);
                let spec = pow(max(dot(n, half_dir), 0.0), 120.0)
                    * max(post.water_sun.w, 0.0) * post.water_extra.z * 0.35;
                water_col = water_col + vec3<f32>(1.0, 0.96, 0.88) * spec;
            }

            // 边缘泡沫：mask 2px 邻域里只要有一个不是水，就认为是水面边缘；
            // 再叠一点"波峰白水"（法线越斜越容易起泡）。
            let size = vec2<i32>(post.target_texel.zw) - vec2<i32>(1);
            let ml = textureLoad(water_mask_texture, clamp(coord - vec2<i32>(2, 0), vec2<i32>(0), size), 0).r;
            let mr = textureLoad(water_mask_texture, clamp(coord + vec2<i32>(2, 0), vec2<i32>(0), size), 0).r;
            let mu = textureLoad(water_mask_texture, clamp(coord + vec2<i32>(0, 2), vec2<i32>(0), size), 0).r;
            let md = textureLoad(water_mask_texture, clamp(coord - vec2<i32>(0, 2), vec2<i32>(0), size), 0).r;
            let edge = 1.0 - min(min(ml, mr), min(mu, md));
            let foam_noise = 0.55 + 0.45 * sin(uv.x * 90.0 + uv.y * 70.0 + tt * 3.0);
            let crest = smoothstep(0.08, 0.36, 1.0 - n.y);
            let foam = (smoothstep(0.45, 0.95, edge) * foam_noise + crest * 0.25)
                * foam_strength;
            let foam_col = vec3<f32>(0.82, 0.90, 0.92);

            hdr = mix(hdr, water_col + foam_col * foam * 0.35, visible * (0.58 + 0.24 * fresnel));
        }
    }

    // ---------- 景深（DoF） ----------
    // 后处理式：用单采样深度算每个像素到焦平面的距离，再在 HDR 上做固定 16 tap
    // 的黄金角圆盘模糊。天空（depth = 1）不参与；GL/WebGL2 读不了深度，整段被
    // `@gl-depth` 剥离，打开开关也不会改变画面。
    //@gl-depth-begin
    if (post.dof.x > 0.5) {
        let depth = textureLoad(depth_texture, coord, 0);
        if (depth < 1.0) {
            let p = view_pos(uv, depth);
            let dist = -p.z;
            let focus = max(post.dof.y, 0.01);
            let coc = clamp(abs(dist - focus) / focus * max(post.dof.z, 0.0), 0.0, 1.0);
            let radius = max(post.dof.w, 0.0) * coc;
            if (radius > 0.25) {
                let rot = interleaved_gradient(f32(coord.x), f32(coord.y)) * 6.2831853;
                var sum = vec3<f32>(0.0);
                for (var i = 0; i < 16; i += 1) {
                    let a = f32(i) * 2.3999632 + rot;
                    let r = sqrt((f32(i) + 0.5) / 16.0);
                    let off = vec2<f32>(cos(a), sin(a)) * r * radius * texel;
                    sum = sum + textureSampleLevel(hdr_texture, hdr_sampler, uv + off, 0.0).rgb;
                }
                hdr = sum / 16.0;
            }
        }
    }
    //@gl-depth-end

    return vec4<f32>(tonemap(hdr), 1.0);
}
