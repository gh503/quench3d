// Bloom：亮度提取（半分辨率）→ 分离高斯模糊（横/竖）。
//
// 三步都用这一份模块，只是 `fs_entry` 不同：
//   `fs_bright`  读 HDR 中间目标，半分辨率 4 抽头盒式下采样 + 阈值
//   `fs_blur_h`  横向 9 抽头高斯（用线性插值把 9 抽头压成 5 次采样）
//   `fs_blur_v`  纵向同理
// 在 tonemap 之前回到线性 HDR（见 post.wgsl），所以外溢不会过曝。

@group(0) @binding(0) var src_texture: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var<uniform> bloom: PostParams;

struct PostParams {
    // x = bloom 开关（0/1），y = bloom 强度，z = bloom 阈值，w = 预留
    params: vec4<f32>,
    // xy = bloom 贴图的 texel 大小，zw = bloom 贴图尺寸
    bloom_texel: vec4<f32>,
    // 后处理 / FXAA 用；这里不读（保持与 post.wgsl 逐字节一致的布局）
    target_texel: vec4<f32>,
    // FXAA 阈值；这里不读（同上，三个着色器共用一个 uniform 缓冲）
    fxaa: vec4<f32>,
};

struct BloomOut {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> BloomOut {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: BloomOut;
    out.position = vec4<f32>(corner * 2.0 - vec2<f32>(1.0), 0.0, 1.0);
    return out;
}

/// Rec.709 亮度。
fn luminance(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(0.2126, 0.7152, 0.0722));
}

/// 亮度提取：只留下超过阈值的部分，并做软过渡（避免阈值处出现硬边）。
fn prefilter(c: vec3<f32>, threshold: f32) -> vec3<f32> {
    let l = luminance(c);
    let knee = max(threshold * 0.5, 1e-4);
    let soft = clamp(l - threshold + knee, 0.0, 2.0 * knee);
    let contribution = max(soft * soft / (4.0 * knee), l - threshold) / max(l, 1e-4);
    return c * contribution;
}

@fragment
fn fs_bright(in: BloomOut) -> @location(0) vec4<f32> {
    // 半分辨率：4 抽头盒式下采样（每个输出像素覆盖 2×2 输入像素）
    let dst_size = bloom.bloom_texel.zw;
    let uv = in.position.xy / dst_size;
    let half_texel = bloom.bloom_texel.xy * 0.5;
    var sum = vec3<f32>(0.0);
    sum = sum + textureSampleLevel(src_texture, src_sampler, uv + vec2<f32>(-half_texel.x, -half_texel.y), 0.0).rgb;
    sum = sum + textureSampleLevel(src_texture, src_sampler, uv + vec2<f32>(half_texel.x, -half_texel.y), 0.0).rgb;
    sum = sum + textureSampleLevel(src_texture, src_sampler, uv + vec2<f32>(-half_texel.x, half_texel.y), 0.0).rgb;
    sum = sum + textureSampleLevel(src_texture, src_sampler, uv + vec2<f32>(half_texel.x, half_texel.y), 0.0).rgb;
    return vec4<f32>(prefilter(sum * 0.25, bloom.params.z), 1.0);
}

/// 9 抽头高斯（σ≈2，抽头间距 1 texel）：权重已归一化。
/// 用线性插值采样，两次采样合成一个抽头对上，所以只用 5 次采样。
const GAUSS_W: array<f32, 5> = array<f32, 5>(0.227027, 0.316216, 0.070270, 0.008816, 0.000229);

fn blur(dir: vec2<f32>, uv: vec2<f32>) -> vec3<f32> {
    let t = bloom.bloom_texel.xy * dir;
    var sum = textureSampleLevel(src_texture, src_sampler, uv, 0.0).rgb * GAUSS_W[0];
    // 成对采样：σ 越远越稀，用 1.3846 / 3.2308 的经典偏移
    sum = sum + textureSampleLevel(src_texture, src_sampler, uv + t * 1.3846154, 0.0).rgb * GAUSS_W[1];
    sum = sum + textureSampleLevel(src_texture, src_sampler, uv - t * 1.3846154, 0.0).rgb * GAUSS_W[1];
    sum = sum + textureSampleLevel(src_texture, src_sampler, uv + t * 3.2307692, 0.0).rgb * GAUSS_W[2];
    sum = sum + textureSampleLevel(src_texture, src_sampler, uv - t * 3.2307692, 0.0).rgb * GAUSS_W[2];
    sum = sum + textureSampleLevel(src_texture, src_sampler, uv + t * 5.0, 0.0).rgb * GAUSS_W[3];
    sum = sum + textureSampleLevel(src_texture, src_sampler, uv - t * 5.0, 0.0).rgb * GAUSS_W[3];
    sum = sum + textureSampleLevel(src_texture, src_sampler, uv + t * 6.5, 0.0).rgb * GAUSS_W[4];
    sum = sum + textureSampleLevel(src_texture, src_sampler, uv - t * 6.5, 0.0).rgb * GAUSS_W[4];
    return sum;
}

@fragment
fn fs_blur_h(in: BloomOut) -> @location(0) vec4<f32> {
    let uv = in.position.xy / bloom.bloom_texel.zw;
    return vec4<f32>(blur(vec2<f32>(1.0, 0.0), uv), 1.0);
}

@fragment
fn fs_blur_v(in: BloomOut) -> @location(0) vec4<f32> {
    let uv = in.position.xy / bloom.bloom_texel.zw;
    return vec4<f32>(blur(vec2<f32>(0.0, 1.0), uv), 1.0);
}
