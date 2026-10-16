// 深度解析：MSAA 深度 → 单采样深度贴图。
//
// 为什么需要这一趟：主 pass 的深度附件在 `sample_count > 1` 时是**多采样**纹理，
// 而 SSAO 要按像素读深度（`texture_depth_2d`）。多采样深度不能直接当成单采样纹理采样，
// 所以把每个像素的第 0 号采样点搬一份到单采样深度贴图里。
// 取第 0 号采样（而不是平均）是有意的：深度**不能**平均（几何边缘上的平均值是个虚构的
// 深度，反投影出来会悬在空中），SSAO 要的是"这个像素上真实存在的某个深度"。
// 代价只有每像素一次读取 + 一次写入，且只在开了 SSAO 且 MSAA > 1 时才跑。

@group(0) @binding(0) var ms_depth: texture_depth_multisampled_2d;

struct ResolveOut {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> ResolveOut {
    // 全屏三角形：(-1,-1) / (3,-1) / (-1,3)
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: ResolveOut;
    out.position = vec4<f32>(corner * 2.0 - vec2<f32>(1.0), 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: ResolveOut) -> @builtin(frag_depth) f32 {
    // 顶点着色器给的是裁剪空间坐标，片元的 `position.xy` 已经是像素中心（+0.5），
    // `vec2<i32>` 截断即得像素下标。
    return textureLoad(ms_depth, vec2<i32>(in.position.xy), 0);
}
