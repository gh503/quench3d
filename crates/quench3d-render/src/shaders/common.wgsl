// Quench3D 共享的 uniform 结构定义。
// 这个文件不会被单独编译，而是由 Rust 侧在 `//@include frame` 处替换进各个着色器。

struct PointLight {
    // xyz = 世界坐标, w = 影响半径
    pos_range: vec4<f32>,
    // rgb = 颜色, w = 强度
    color_intensity: vec4<f32>,
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
