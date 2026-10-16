// Quench3D 共享的 uniform 结构定义。
// 这个文件不会被单独编译，而是由 Rust 侧在 `//@include frame` 处替换进各个着色器。

struct PointLight {
    // xyz = 世界坐标, w = 影响半径
    pos_range: vec4<f32>,
    // rgb = 颜色, w = 强度
    color_intensity: vec4<f32>,
    // xyz = 照射方向（聚光灯），w = 外锥角余弦；w = 0 表示这是点光源
    dir_outer: vec4<f32>,
    // x = 内锥角余弦，y = 1 表示聚光灯（0 表示点光源）
    cone_inner: vec4<f32>,
};

struct Frame {
    view_proj: mat4x4<f32>,
    inv_view_proj: mat4x4<f32>,
    // xyz = 相机世界坐标
    camera_pos: vec4<f32>,
    // xyz = 指向光源的单位向量, w = 平行光强度
    light_dir: vec4<f32>,
    // rgb = 平行光颜色, w > 0.5 表示启用阴影
    light_color: vec4<f32>,
    // rgb = 环境光颜色, w = 环境光强度
    ambient: vec4<f32>,
    shadow_matrix: mat4x4<f32>,
    // 相机视图矩阵（级联按视空间深度选级：depth = -(view * world).z）
    view: mat4x4<f32>,
    // 每一级的光源空间矩阵（只有前 cascade_info.x 个有效）
    shadow_matrices: array<mat4x4<f32>, 4>,
    // 每一级的远平面距离（视空间米数，严格递增）
    cascade_splits: vec4<f32>,
    // x = 级联数量（0 = 不投影阴影）, y = 图集分块缩放（单级 1，多级 0.5）, z/w 备用
    cascade_info: vec4<f32>,
    sky_zenith: vec4<f32>,
    sky_horizon: vec4<f32>,
    sky_ground: vec4<f32>,
    // rgb = 雾颜色, w = 雾密度
    fog: vec4<f32>,
    // x = 时间(秒), y = 阴影偏移, z = 阴影贴图纹素大小, w = 阴影强度
    params: vec4<f32>,
    point_lights: array<PointLight, 8>,
    // x = 点光源数量, y = 是否绘制天空, z = 曝光, w = 备用
    params2: vec4<f32>,
    // ---- 聚光灯阴影（整帧只支持一盏投影聚光灯，见 spot_shadow.rs）----
    // 世界 → 光的**透视**裁剪空间矩阵（与平行光那张图无关，是第二张深度图）
    spot_shadow_matrix: mat4x4<f32>,
    // x = 投影灯在 point_lights 里的下标（< 0 = 本帧没有聚光阴影）,
    // y = near（米）, z = far（米 = 灯的影响半径）, w = 深度偏移（纹素个数）
    spot_shadow_params: vec4<f32>,
    // x = 1 / 贴图边长（纹素）, y = tan(fov / 2), z = 法线偏移（纹素个数）, w = 斜率因子
    spot_shadow_texel: vec4<f32>,

    // ---- 点光源阴影（6 层深度数组，整帧只支持一盏投影点光源，见 point_shadow.rs）----
    // 与聚光灯那套是**两套独立资源**：可以同时有一盏投影点光源 + 一盏投影聚光灯。
    // x = 投影灯在 point_lights 里的下标（**< 0 = 本帧没有点光阴影**）、y = near、
    // z = far（= 影响半径）、w = 深度偏移（纹素个数）。
    point_shadow_params: vec4<f32>,
    // x = 1 / 单面边长、y = tan(半面角)（≈ 1.01 的面余量，cube_face_uv 要用）、
    // z = 法线偏移（纹素个数）、w = 斜率因子。
    point_shadow_texel: vec4<f32>,
};

struct Material {
    base_color_factor: vec4<f32>,
    emissive_factor: vec4<f32>,
    // x = 金属度, y = 粗糙度, z = UV 缩放, w = 标志位
    params: vec4<f32>,
    // x = alpha 裁剪阈值, y = 法线强度, z = 环境光遮蔽强度, w = 备用
    params2: vec4<f32>,
};

const PI: f32 = 3.14159265359;
// 标志位
const FLAG_BASE_COLOR_TEX: u32 = 1u;
const FLAG_NORMAL_TEX: u32 = 2u;
const FLAG_MR_TEX: u32 = 4u;
const FLAG_EMISSIVE_TEX: u32 = 8u;
const FLAG_UNLIT: u32 = 16u;
