//! Quench3D 渲染器：前向 PBR + 平行光阴影 + 点光源 + 程序化天空 + 无限网格 + 调试线段。
//!
//! 使用方式是"立即模式"：每帧 `begin_frame()` → 多次 `draw()` → `render()`。
//! 渲染器内部会做视锥剔除、按网格/材质合批、以及实例缓冲的按需扩容。

use std::borrow::Cow;
use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use quench3d_math::{Aabb, Mat4, Transform, Vec3, Vec4};
use wgpu::util::DeviceExt;

use crate::camera::{Camera, Frustum};
use crate::context::{vertex_layout, FrameError, GpuContext};
use crate::light::{Lighting, PointLight, SpotLight, MAX_POINT_LIGHTS};
use crate::lines::{DebugLines, LineVertex};
use crate::material::{MaterialDesc, MaterialId, MeshId, TextureId};
use crate::mesh::MeshData;
use crate::point_shadow::{plan_point_shadow, PointShadowPlan, POINT_SHADOW_FACES};
use crate::spot_shadow::{plan_spot_shadow, SpotShadowPlan};
use crate::texture::{default_texture, GpuTexture, ImageData};

/// 主 pass 的两个颜色目标：HDR + 水面 mask。
///
/// 两个目标的 blend 必须**相同**：WebGL2 的 downlevel 设备没有
/// `DownlevelFlags::INDEPENDENT_BLEND`，只要两个目标一个 `Some`、一个 `None`，
/// `create_render_pipeline` 就会报 "INDEPENDENT_BLEND are required"。
/// 所以 alpha 管线的 mask 也走 ALPHA_BLENDING；着色器把 mask 的 alpha 写成
/// 覆盖（非水面 0、水面 1），在 alpha 混合下等价于"非水面不动、水面覆盖"。
fn main_pass_color_targets(
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> Vec<Option<wgpu::ColorTargetState>> {
    vec![
        Some(wgpu::ColorTargetState {
            format,
            blend,
            write_mask: wgpu::ColorWrites::ALL,
        }),
        Some(wgpu::ColorTargetState {
            format: WATER_MASK_FORMAT,
            blend,
            write_mask: wgpu::ColorWrites::ALL,
        }),
    ]
}

/// 点光源阴影纹理的层数：6 个面 + 1 个填充层。
///
/// 7 不是 6 的倍数，wgpu-hal 的 GL 后端就不会把这张正方形纹理猜成 Cube；
/// 着色器绑定的是 `texture_depth_2d_array`，视图也明确只取前 6 层。
const POINT_SHADOW_TEXTURE_LAYERS: u32 = POINT_SHADOW_FACES as u32 + 1;

const COMMON_WGSL: &str = include_str!("shaders/common.wgsl");
const PBR_WGSL: &str = include_str!("shaders/pbr.wgsl");
const SHADOW_WGSL: &str = include_str!("shaders/shadow.wgsl");
const SKY_WGSL: &str = include_str!("shaders/sky.wgsl");
const GRID_WGSL: &str = include_str!("shaders/grid.wgsl");
const LINE_WGSL: &str = include_str!("shaders/line.wgsl");
const POST_WGSL: &str = include_str!("shaders/post.wgsl");
const BLOOM_WGSL: &str = include_str!("shaders/bloom.wgsl");
const FXAA_WGSL: &str = include_str!("shaders/fxaa.wgsl");
const DEPTH_RESOLVE_WGSL: &str = include_str!("shaders/depth_resolve.wgsl");

/// `post.wgsl` 里"依赖读深度纹理"的区段标记（见 [`post_wgsl_for`]）。
const GL_DEPTH_BEGIN: &str = "//@gl-depth-begin";
/// 区段结束标记。
const GL_DEPTH_END: &str = "//@gl-depth-end";

/// 把 `//@include common` 展开为公共 uniform 定义。
fn expand(source: &str) -> String {
    source.replace("//@include common", COMMON_WGSL)
}

/// `post.wgsl` 的**变体选择**：这个后端能不能读深度纹理。
///
/// `//@gl-depth-begin` 与 `//@gl-depth-end` 之间的区段（深度纹理绑定、`view_pos`/
/// `interleaved_gradient`/`ssao_factor` 三个函数、以及 `fs_main` 里的调用点）在
/// "不能读深度纹理"的后端上会被**整段删掉**（哪些后端不行见
/// [`crate::context::supports_depth_texture_read`]）。
///
/// 为什么是删源码，而不是"把 `ssao` 开关置 0 让它不执行"：GLSL 的翻译发生在**建管线**
/// 的时候，naga 会把整个模块都翻一遍——只要 WGSL 里还留着
/// `textureLoad(depth_texture, ..)`，`quench3d-post` 这条管线就建不出来。wasm 上 wgpu 把
/// 管线校验错误当致命错误，结果是**编辑器启动即 panic**（用户实测：Web 编辑器在 GL/WebGL2 上启动即 panic）。
/// 换句话说："运行时判断开关"在这里救不了场，必须在"交给驱动之前"就把代码去掉。
///
/// 删掉之后 SSAO 在 GL 上完全不做（不是"关掉强度"，是这一趟不存在），
/// tonemap / bloom / FXAA 一行都不动 —— 见下面的单元测试。
fn post_wgsl_for(ssao: bool) -> Cow<'static, str> {
    if ssao {
        return Cow::Borrowed(POST_WGSL);
    }
    let mut out = String::with_capacity(POST_WGSL.len());
    let mut dropping = false;
    let mut regions = 0usize;
    for line in POST_WGSL.lines() {
        if line.contains(GL_DEPTH_BEGIN) {
            dropping = true;
            regions += 1;
            continue;
        }
        if line.contains(GL_DEPTH_END) {
            dropping = false;
            continue;
        }
        if !dropping {
            out.push_str(line);
            out.push('\n');
        }
    }
    debug_assert!(!dropping, "post.wgsl 的 {GL_DEPTH_BEGIN} 区段没有闭合");
    debug_assert_eq!(
        regions, 3,
        "post.wgsl 的深度区段应当是 3 段（绑定/函数/调用点）"
    );
    out.shrink_to_fit();
    Cow::Owned(out)
}

/// 场景深度格式。
///
/// 用 `Depth32Float` 而不是 `Depth24Plus`：后者**不可采样**（wgpu 的
/// `TextureSampleType::Depth` 不接受它），而 SSAO 必须把深度当纹理读。
/// 32 位浮点的精度对"从深度反投影出视空间位置"也更友好；代价是每像素多 1 字节。
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// HDR 中间目标格式（线性、可混合、可过滤；WebGPU 核心里 `rgba16float` 三样都支持）。
///
/// 为什么不是 `rgba32float`：它在 WebGPU 核心里**不可混合**（要 `float32-blendable` 特性），
/// 而半透明与网格线都要混合。f16 的精度对"tonemap 之前"的值完全够用
/// （相对误差 ~1e-3，落在 8 位输出的 1 个色阶以内，见 `post_pass` 的像素测试）。
pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// 水面 mask（主 pass 第二颜色附件）：r = 1 表示水面平面覆盖，g = 水面世界高度。
///
/// 用 `Rgba16Float` 而不是 8 位：g 要存世界 Y 坐标（浮点），8 位归一化会把水面
/// 高度量化成几十厘米，射线-平面重建直接失真；同时它是可渲染/可 MSAA resolve 的格式。
pub const WATER_MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// 阴影级联的最大级数（图集是 2x2 分块，所以最多 4 级）。
pub const MAX_SHADOW_CASCADES: usize = 4;

/// 阴影贴图格式（可比较采样）。
pub const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

/// GPU 帧常量（必须与 `common.wgsl` 的 `Frame` 布局逐字节一致）。
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct FrameUniforms {
    /// 视图投影矩阵。
    pub view_proj: [[f32; 4]; 4],
    /// 视图投影逆矩阵。
    pub inv_view_proj: [[f32; 4]; 4],
    /// xyz = 相机位置。
    pub camera_pos: [f32; 4],
    /// xyz = 指向光源方向，w = 平行光强度。
    pub light_dir: [f32; 4],
    /// rgb = 平行光颜色，w = 是否启用阴影。
    pub light_color: [f32; 4],
    /// rgb = 环境光颜色，w = 强度。
    pub ambient: [f32; 4],
    /// 阴影视图投影矩阵（级联时是**第一级**的矩阵，阴影 pass 每一级还会各自覆盖它）。
    pub shadow_matrix: [[f32; 4]; 4],
    /// 相机视图矩阵（级联按视空间深度选级：`depth = -(view * world).z`）。
    pub view: [[f32; 4]; 4],
    /// 每一级的光源空间矩阵（只有前 `cascade_info.x` 个有效）。
    pub shadow_matrices: [[[f32; 4]; 4]; MAX_SHADOW_CASCADES],
    /// 每一级的远平面距离（视空间米数，严格递增，最后一个 = 相机远平面）。
    pub cascade_splits: [f32; 4],
    /// x = 级联数量（0 = 这一帧不投影阴影），y = 分块缩放（1 级 = 1，多级 = 0.5），z/w 备用。
    pub cascade_info: [f32; 4],
    /// 天顶色。
    pub sky_zenith: [f32; 4],
    /// 地平线色。
    pub sky_horizon: [f32; 4],
    /// 地面色。
    pub sky_ground: [f32; 4],
    /// rgb = 雾色，w = 雾密度。
    pub fog: [f32; 4],
    /// x = 时间，y = 阴影偏移，z = 阴影纹素，w = 阴影强度。
    pub params: [f32; 4],
    /// 点光源（结构体数组，与 WGSL 的 `array<PointLight, 8>` 同布局）。
    pub point_lights: [PointLightUniform; MAX_POINT_LIGHTS],
    /// x = 点光源数量，y = 绘制天空，z = 曝光，w = 网格高度。
    pub params2: [f32; 4],
    /// 聚光灯阴影：世界 → 光的**透视**裁剪矩阵（第二张深度图，与 `shadow_matrix` 无关）。
    ///
    /// 没有投影灯时是单位矩阵（着色器靠 `spot_shadow_params.x < 0` 判断，不会用它）。
    pub spot_shadow_matrix: [[f32; 4]; 4],
    /// x = 投影灯在 [`Self::point_lights`] 里的下标（**< 0 = 本帧没有聚光阴影**）,
    /// y = near（米）, z = far（米 = 灯的影响半径）, w = 深度偏移（纹素个数）。
    pub spot_shadow_params: [f32; 4],
    /// x = 1 / 贴图边长, y = `tan(fov / 2)`, z = 法线偏移（纹素个数）,
    /// w = 斜率因子的系数（`1 - n·l * w`，与平行光同形状）。
    pub spot_shadow_texel: [f32; 4],
    /// 点光源阴影（6 层数组，整帧最多一盏投影点光源，见 `point_shadow.rs`）：
    /// x = 投影灯在 [`Self::point_lights`] 里的下标（**< 0 = 本帧没有点光阴影**）,
    /// y = near（米）, z = far（米 = 灯的影响半径）, w = 深度偏移（纹素个数）。
    pub point_shadow_params: [f32; 4],
    /// x = 1 / 单面边长, y = `tan(半面角)`（≈ 1.01 的那个余量）,
    /// z = 法线偏移（纹素个数）, w = 斜率因子的系数。
    pub point_shadow_texel: [f32; 4],
}

/// 单盏点光源的 uniform 数据。
///
/// **必须与 `shaders/common.wgsl` 的 `PointLight` 逐字段对应**：那边是
/// `array<PointLight, 8>`，也就是**结构体数组（pos0, color0, pos1, color1 …）**。
/// 曾经这里写成两个并列数组（8 个位置一段 + 8 个颜色一段），布局对不上，于是着色器
/// 把"第二盏灯的位置"当成"第一盏灯的颜色和强度"在读——看起来有光，其实颜色和强度
/// 全是错的，只有一盏灯时更是完全无效。布局回归测试见
/// `frame_uniform_layout_matches_wgsl`。
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct PointLightUniform {
    /// xyz = 世界坐标，w = 影响半径。
    pub pos_range: [f32; 4],
    /// rgb = 颜色，w = 强度。
    pub color_intensity: [f32; 4],
    /// xyz = 照射方向（聚光灯），w = 外锥角余弦；**w = 0 表示这是一盏点光源**。
    pub dir_outer: [f32; 4],
    /// x = 内锥角余弦，y = 1 表示聚光灯（0 表示点光源），zw 备用。
    pub cone_inner: [f32; 4],
}

/// 把点光源与聚光灯打包进同一条 `array<PointLight, 8>`。
///
/// 顺序是**确定**的：点光源先占名额，聚光灯补后面，超出的忽略（场景校验会给警告）。
/// 抽成纯函数是为了能直接测——"聚光灯有没有被塞进正确的位置、锥角余弦对不对"
/// 这种事一旦错了，画面只是"有一点不对"，靠眼睛很难发现。
pub fn pack_local_lights(
    points: &[PointLight],
    spots: &[SpotLight],
) -> ([PointLightUniform; MAX_POINT_LIGHTS], usize) {
    let mut out = [PointLightUniform::zeroed(); MAX_POINT_LIGHTS];
    let mut n = 0usize;
    for l in points.iter().take(MAX_POINT_LIGHTS) {
        out[n] = PointLightUniform {
            pos_range: [l.position.x, l.position.y, l.position.z, l.radius.max(0.01)],
            color_intensity: [l.color.x, l.color.y, l.color.z, l.intensity],
            // w = 0 → 点光源（着色器靠这个区分）
            dir_outer: [0.0, 0.0, 0.0, 0.0],
            cone_inner: [1.0, 0.0, 0.0, 0.0],
        };
        n += 1;
    }
    for s in spots.iter().take(MAX_POINT_LIGHTS - n) {
        let d = s.dir();
        out[n] = PointLightUniform {
            pos_range: [s.position.x, s.position.y, s.position.z, s.radius.max(0.01)],
            color_intensity: [s.color.x, s.color.y, s.color.z, s.intensity],
            dir_outer: [d.x, d.y, d.z, s.outer_cos],
            cone_inner: [s.inner_cos, 1.0, 0.0, 0.0],
        };
        n += 1;
    }
    (out, n)
}

/// 每实例的顶点数据。
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct InstanceRaw {
    /// 模型矩阵。
    pub model: [[f32; 4]; 4],
    /// 颜色倍乘（线性 RGBA）。
    pub color: [f32; 4],
    /// x = 金属度，y = 粗糙度，z = 自发光强度，w = 是否覆盖材质参数。
    pub params: [f32; 4],
}

/// 一次绘制请求（面向使用者的实例描述）。
#[derive(Clone, Copy, Debug)]
pub struct InstanceDesc {
    /// 世界变换。
    pub transform: Transform,
    /// 颜色倍乘（线性空间）。
    pub color: Vec4,
    /// 覆盖材质的金属度（`None` 用材质自身的值）。
    pub metallic: Option<f32>,
    /// 覆盖材质的粗糙度。
    pub roughness: Option<f32>,
    /// 自发光强度倍乘。
    pub emissive_strength: f32,
}

impl Default for InstanceDesc {
    fn default() -> Self {
        Self {
            transform: Transform::IDENTITY,
            color: Vec4::new(1.0, 1.0, 1.0, 1.0),
            metallic: None,
            roughness: None,
            emissive_strength: 1.0,
        }
    }
}

impl InstanceDesc {
    /// 由变换构造。
    pub fn new(transform: Transform) -> Self {
        Self {
            transform,
            ..Default::default()
        }
    }

    /// 位置 + 等比缩放。
    pub fn from_position(position: Vec3, scale: f32) -> Self {
        Self::new(Transform::new(
            position,
            quench3d_math::Quat::IDENTITY,
            Vec3::splat(scale),
        ))
    }

    /// 颜色。
    pub fn with_color(mut self, color: Vec4) -> Self {
        self.color = color;
        self
    }

    /// 覆盖金属度/粗糙度。
    pub fn with_pbr(mut self, metallic: f32, roughness: f32) -> Self {
        self.metallic = Some(metallic);
        self.roughness = Some(roughness);
        self
    }

    /// 自发光强度。
    pub fn with_emissive(mut self, strength: f32) -> Self {
        self.emissive_strength = strength;
        self
    }

    fn to_raw(self) -> InstanceRaw {
        let m = self.transform.matrix();
        let override_material = self.metallic.is_some() || self.roughness.is_some();
        InstanceRaw {
            model: m.to_cols_arrays(),
            color: self.color.to_array(),
            params: [
                self.metallic.unwrap_or(0.0),
                self.roughness.unwrap_or(0.5),
                self.emissive_strength,
                if override_material { 1.0 } else { 0.0 },
            ],
        }
    }
}

/// 一个批次的绘制范围。
#[derive(Clone, Copy, Debug)]
struct Batch {
    mesh: MeshId,
    material: MaterialId,
    first: u32,
    count: u32,
    bounds: Aabb,
}

/// GPU 网格。
struct GpuMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    bounds: Aabb,
    name: String,
}

/// GPU 材质。
struct GpuMaterial {
    desc: MaterialDesc,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    name: String,
}

/// 把点光源阴影计划打包成 `FrameUniforms` 里的两个 vec4。
///
/// **纯函数**，所以"没有投影灯时写负下标"这件事能在没有显卡的 CI 上直接测
/// （着色器靠 `point_shadow_params.x < 0` 整段跳过采样，这是"缺省逐像素不变"的开关）。
fn point_shadow_uniforms(plan: Option<PointShadowPlan>, map_size: u32) -> ([f32; 4], [f32; 4]) {
    match plan {
        Some(p) => (
            [
                p.light_index as f32,
                p.camera.near,
                p.camera.far,
                p.camera.depth_bias_texels,
            ],
            [
                1.0 / map_size.max(1) as f32,
                // `tan(半面角)`：`cube_face_uv` 用它把方向折进面内 uv（≈ 1.01）
                p.camera.tan_half_fov,
                p.camera.normal_offset_texels,
                crate::point_shadow::POINT_SHADOW_SLOPE_FACTOR,
            ],
        ),
        None => ([-1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0]),
    }
}

/// 每帧渲染统计。
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderStats {
    /// 不透明绘制调用数。
    pub draw_calls: u32,
    /// 提交的实例数。
    pub instances: u32,
    /// 提交的三角形数。
    pub triangles: u64,
    /// 被视锥剔除的实例数。
    pub culled: u32,
    /// 阴影绘制调用数（平行光；多级级联时每级各算一遍）。
    pub shadow_draw_calls: u32,
    /// 阴影实例数。
    pub shadow_instances: u32,
    /// 聚光灯阴影的绘制调用数（0 = 这一帧没有投影聚光灯）。
    ///
    /// 与 [`Self::shadow_draw_calls`] 分开：一个是"太阳投不投影"、一个是"聚光投不投影"，
    /// 合成一个数字就没法判断"影子没出来是哪一边的问题"。
    pub spot_shadow_draw_calls: u32,
    /// 点光源阴影的绘制调用数（0 = 这一帧没有投影点光源）。
    ///
    /// 六个面各算一遍，所以"一盏灯投一次影"在这里是"最多 6 × 投射体批次数"。
    pub point_shadow_draw_calls: u32,
    /// 调试线段数。
    pub lines: u32,
    /// 后处理趟数（正常是 1；bloom 打开时是 1 + 3）。
    pub post_passes: u32,
    /// 帧序号。
    pub frame_index: u64,
}

/// 后处理参数（与 `post.wgsl` / `bloom.wgsl` / `fxaa.wgsl` 的 `PostParams` 逐字节一致）。
///
/// `align(16)`：末尾两个矩阵按 WGSL 的 `mat4x4<f32>` 规则要 16 字节对齐，
/// 前面的字段正好都是 `vec4`，所以偏移天然对齐——这个标注是把这件事写死。
#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct PostUniforms {
    /// x = bloom 开关，y = 强度，z = 阈值，w = 预留。
    pub params: [f32; 4],
    /// xy = bloom 贴图 texel 大小（1/尺寸），zw = bloom 贴图尺寸。
    pub bloom_texel: [f32; 4],
    /// xy = 目标 texel 大小（1/尺寸），zw = 目标尺寸。
    ///
    /// 后处理 pass 的输出尺寸**不一定等于** bloom 贴图尺寸（bloom 是半分辨率），
    /// 采样 bloom 时不能用 bloom 的 texel：那是 2× 缩放，只会读到一角。
    pub target_texel: [f32; 4],
    /// x = FXAA 开关（0/1），y = 相对边缘阈值，z = 绝对边缘阈值下限，w = 预留。
    pub fxaa: [f32; 4],
    /// SSAO：x = 开关（0/1），y = 半径（视空间，米），z = 深度偏移 bias，w = 强度。
    pub ssao: [f32; 4],
    /// SSAO：x = 采样数，yzw = 预留。
    pub ssao2: [f32; 4],
    /// 投影矩阵（视空间 → 裁剪空间）：把半球采样点投回屏幕要用。
    pub proj: [[f32; 4]; 4],
    /// 逆投影矩阵（裁剪空间 → 视空间）：从深度反投影出视空间位置要用。
    pub inv_proj: [[f32; 4]; 4],
    /// 屏幕空间水：x = 开关（0/1），y = 时间，z = 强度，w = 预留。
    pub water: [f32; 4],
    /// 水色（线性 RGB），a = 预留。
    pub water_color: [f32; 4],
    /// 视空间 → 世界：由屏幕坐标算视线方向（菲涅耳）。
    pub inv_view: [[f32; 4]; 4],
    /// x = 泡沫强度，y = 折射强度，z = 太阳高光强度，w = 预留。
    pub water_extra: [f32; 4],
    /// xyz = 太阳方向（从光源射向场景），w = 强度。
    pub water_sun: [f32; 4],
}

/// 屏幕空间水面选项。
#[derive(Clone, Copy, Debug)]
pub struct WaterOptions {
    /// 是否启用（缺省关闭：关闭时后处理完全不走水面分支，像素与以前一致）。
    pub enabled: bool,
    /// 水色（线性 RGB）。
    pub color: [f32; 3],
    /// 波浪强度（0 = 平静水面）；驱动法线扰动幅度。
    pub strength: f32,
    /// 清澈度（0 = 浑浊/强吸收，1 = 清澈）；映射到吸收混合比例。
    pub clarity: f32,
    /// 泡沫强度（0 = 无泡沫）。
    pub foam: f32,
    /// 折射强度（0 = 不扰动背景）。
    pub refraction: f32,
}

impl Default for WaterOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            color: [0.06, 0.28, 0.48],
            strength: 1.0,
            clarity: 1.0,
            foam: 1.0,
            refraction: 1.0,
        }
    }
}

/// Bloom 配置。
#[derive(Clone, Copy, Debug)]
pub struct BloomOptions {
    /// 是否启用。
    pub enabled: bool,
    /// 强度（合成时乘的系数）。
    pub intensity: f32,
    /// 亮度阈值（Rec.709 亮度，线性空间；超过才开始外溢）。
    pub threshold: f32,
}

impl Default for BloomOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            intensity: 0.6,
            threshold: 1.0,
        }
    }
}

/// FXAA（后处理抗锯齿）配置。
///
/// 与 MSAA 的关系：MSAA 在**几何边缘**上以更高采样率着色（贵、且对材质内部的高频无效），
/// FXAA 在**已经 tonemap 过的画面**上找感知边缘做定向模糊（便宜、能顺带压掉着色噪声，
/// 但会把细节一起糊一点）。两者可以叠加：MSAA 管几何、FXAA 管剩下的。
#[derive(Clone, Copy, Debug)]
pub struct FxaaOptions {
    /// 是否启用（缺省关闭：多一趟全屏 pass + 一张 LDR 中间目标）。
    pub enabled: bool,
    /// 相对边缘阈值（跟随亮度）：对比度低于它就不当成边缘。越小越敏感（也越糊）。
    pub edge_threshold: f32,
    /// 绝对边缘阈值下限：压掉平坦区域里的单像素噪点。
    pub edge_threshold_min: f32,
}

impl Default for FxaaOptions {
    /// 缺省值取 FXAA 原版（NVIDIA 3.11）的"质量档"预设。
    fn default() -> Self {
        Self {
            enabled: false,
            edge_threshold: 0.166,
            edge_threshold_min: 0.0833,
        }
    }
}

/// SSAO（屏幕空间环境光遮蔽）配置。
///
/// 实现是**深度-only** 的：没有法线缓冲，法线由视空间位置的屏幕导数重建；
/// 半球采样点在切空间里按黄金角螺旋分布，每像素旋转一个随机角度；
/// 遮挡判定是"该屏幕位置上真实几何比采样点更靠近相机"。
/// 结果乘在**间接光**意义上的整幅 HDR 颜色上（不区分直接光/间接光）——这是
/// 后处理式 SSAO 的固有简化，也是它便宜的原因。
#[derive(Clone, Copy, Debug)]
pub struct SsaoOptions {
    /// 是否启用（缺省关闭：多一趟深度解析 + 每像素 N 次采样）。
    pub enabled: bool,
    /// 采样半径（视空间米数）：物体尺度 1~2 米时 0.5~1.0 比较合适。
    pub radius: f32,
    /// 深度偏移（米）：压掉"同一个面上自己遮自己"的自遮蔽噪声。
    pub bias: f32,
    /// 强度（0 = 不暗，1 = 全黑）：乘在遮蔽比例上。
    pub strength: f32,
    /// 每像素采样数（1~64）：越多越平滑也越贵。
    ///
    /// 缺省 16：12 时接触处的颗粒感还看得出来（每像素旋转的螺旋图案），
    /// 16 是这个 pass 的画质/开销平衡点（再多收益很小，画面已经不是瓶颈）。
    pub samples: u32,
}

impl Default for SsaoOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            radius: 0.75,
            bias: 0.025,
            strength: 1.0,
            samples: 16,
        }
    }
}

/// 渲染器创建选项。
#[derive(Clone, Copy, Debug)]
pub struct RendererOptions {
    /// 阴影贴图分辨率。
    pub shadow_map_size: u32,
    /// 是否启用阴影。
    pub shadows: bool,
    /// 阴影级联数（1 = 单级，与老行为一致；2~4 = 把视锥切段，每段一个分块）。
    ///
    /// 单级用一个固定的、围绕相机前方的正交盒，远处细节靠 `shadow_radius` 粗放覆盖；
    /// 多级按 PSSM 切分视锥，近处细远处粗——同样的贴图分辨率下，近处阴影质量明显更好，
    /// 代价是每一级各画一遍阴影 pass，且每一级只有 `贴图边长 / 2` 个纹素（图集是 2x2）。
    pub shadow_cascades: u32,
    /// 级联切分的对数/均匀混合系数（0 = 均匀，1 = 全对数），只在 `shadow_cascades > 1` 时有意义。
    pub shadow_split_lambda: f32,
    /// 是否启用无限网格。
    pub grid: bool,
    /// 网格高度。
    pub grid_height: f32,
    /// 是否绘制天空。
    pub sky: bool,
    /// Bloom（缺省关闭；打开后画面与缺省不同，见 `docs/gap_analysis.md` P1 #25）。
    pub bloom: BloomOptions,
    /// FXAA（缺省关闭）。
    pub fxaa: FxaaOptions,
    /// SSAO（缺省关闭）。
    pub ssao: SsaoOptions,
    /// 屏幕空间水面（缺省关闭；水面 mask 仍会写，只是后处理不读）。
    pub water: WaterOptions,
    /// 解析式环境高光（缺省开启）：金属/光滑材质按反射方向取程序化天空，
    /// 而不是只用半球环境光。没有 HDRI/cubemap，属于便宜近似。
    pub environment: bool,
}

impl Default for RendererOptions {
    fn default() -> Self {
        Self {
            shadow_map_size: 2048,
            shadows: true,
            shadow_cascades: 1,
            shadow_split_lambda: 0.5,
            grid: true,
            grid_height: 0.0,
            sky: true,
            bloom: BloomOptions::default(),
            fxaa: FxaaOptions::default(),
            ssao: SsaoOptions::default(),
            water: WaterOptions::default(),
            environment: true,
        }
    }
}

/// Quench3D 渲染器。
pub struct Renderer {
    // 管线与布局
    frame_layout: wgpu::BindGroupLayout,
    shadow_bind_layout: wgpu::BindGroupLayout,
    material_layout: wgpu::BindGroupLayout,
    opaque_pipeline: wgpu::RenderPipeline,
    alpha_pipeline: wgpu::RenderPipeline,
    /// 不剔除背面（材质的 `double_sided` 为真时使用）。
    ///
    /// 半透明不需要这条：`alpha_pipeline` 本来就是 `cull_mode: None`。
    opaque_double_pipeline: wgpu::RenderPipeline,
    shadow_pipeline: wgpu::RenderPipeline,
    sky_pipeline: wgpu::RenderPipeline,
    grid_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    /// 后处理：HDR → tonemap（+ bloom）→ 目标。
    post_pipeline: wgpu::RenderPipeline,
    /// 后处理写 **LDR 中间目标**时用的同一条管线（颜色目标格式不同，管线不能复用）。
    post_ldr_pipeline: wgpu::RenderPipeline,
    /// Bloom：亮度提取 + 横向/纵向模糊。
    bloom_bright_pipeline: wgpu::RenderPipeline,
    bloom_blur_h_pipeline: wgpu::RenderPipeline,
    bloom_blur_v_pipeline: wgpu::RenderPipeline,
    /// FXAA：tonemap 之后的画面 → 抗锯齿 → 目标（见 `fxaa.wgsl`）。
    fxaa_pipeline: wgpu::RenderPipeline,
    /// SSAO 需要单采样深度；`sample_count > 1` 时靠这一趟把 MSAA 深度解析出来。
    ///
    /// `None` = 这个后端读不了深度纹理（GL/WebGL2），这一趟根本不建 —— 它的 WGSL 里
    /// 同样有 `textureLoad(ms_depth, ..)`，建了就会 panic（见 [`post_wgsl_for`]）。
    depth_resolve_pipeline: Option<wgpu::RenderPipeline>,
    post_layout: wgpu::BindGroupLayout,
    bloom_layout: wgpu::BindGroupLayout,
    /// FXAA 的绑定组布局（比后处理少一张 bloom 贴图）。
    fxaa_layout: wgpu::BindGroupLayout,
    /// 深度解析的绑定组布局（只读多采样深度）。
    depth_resolve_layout: wgpu::BindGroupLayout,
    post_uniform: wgpu::Buffer,
    post_sampler: wgpu::Sampler,
    /// 依赖尺寸，随 HDR 目标一起重建。
    post_bind_group: Option<wgpu::BindGroup>,
    bloom_bright_bind_group: Option<wgpu::BindGroup>,
    bloom_blur_h_bind_group: Option<wgpu::BindGroup>,
    bloom_blur_v_bind_group: Option<wgpu::BindGroup>,
    fxaa_bind_group: Option<wgpu::BindGroup>,
    depth_resolve_bind_group: Option<wgpu::BindGroup>,

    // 帧资源
    frame_uniform: wgpu::Buffer,
    frame_bind_group: wgpu::BindGroup,
    shadow_texture: wgpu::Texture,
    shadow_view: wgpu::TextureView,
    shadow_sampler: wgpu::Sampler,
    shadow_bind_group: wgpu::BindGroup,
    /// 聚光灯阴影图（**第二张** 2D 深度图，与平行光那张互不干扰）。
    ///
    /// 分辨率取 `shadow_map_size / 2`（与级联的一个分块同尺寸）：外锥通常比平行光的
    /// 覆盖范围小得多，一倍分辨率的收益有限，而每帧多出来的显存是实打实的
    /// （`2048²` 的 `Depth32Float` 是 16 MB）。想要更清晰就调 `shadow_map_size`。
    spot_shadow_texture: wgpu::Texture,
    spot_shadow_view: wgpu::TextureView,
    /// 聚光阴影图的边长（像素）。
    spot_shadow_map_size: u32,
    /// 投影灯那一趟阴影 pass 自己的帧常量（顶点着色器只读 `shadow_matrix`）。
    ///
    /// 必须与级联的 4 个 buffer 分开：同一帧里两个 pass 读不同的矩阵，
    /// 共用一个 uniform buffer 会让后写的那个覆盖先写的那个。
    spot_shadow_frame_buffer: wgpu::Buffer,
    spot_shadow_frame_bind_group: wgpu::BindGroup,
    /// 点光源阴影图：**6 个 90° 面**的深度数组（`+X, -X, +Y, -Y, +Z, -Z`，见 `point_shadow.rs`）。
    ///
    /// 为什么不是硬件 `texture_cube`：面/uv 的映射规则由图形 API 规定，而 CI 没有显卡，
    /// 写错一处符号只有"影子在面内镜像"这一种症状。这里把约定放在 CPU 纯函数里、
    /// 着色器逐字镜像，于是"渲染用的矩阵"与"采样用的 uv"之间有一条能在 CI 上跑的等式。
    /// 代价：PCF 要用数组重载（`textureSampleCompareLevel`），面边界靠 1% 的 tan 余量重叠。
    ///
    /// 边长取聚光那张的**一半**：6 个面加起来是 1.5 倍的 2D 图面积
    /// （默认 `shadow_map_size = 2048` → 聚光 1024、点光每面 512 → 6 MB）。
    point_shadow_texture: wgpu::Texture,
    /// 采样用的**整数组**视图（`texture_depth_2d_array`）。
    point_shadow_array_view: wgpu::TextureView,
    /// 渲染用的**每层** 2D 视图（一个面一个 pass）。两者不能互换。
    point_shadow_face_views: [wgpu::TextureView; POINT_SHADOW_FACES],
    /// 单面边长（像素）。
    point_shadow_map_size: u32,
    /// 6 个面各自的帧常量（顶点着色器只读 `shadow_matrix`，与级联同一个理由）。
    point_shadow_frame_buffers: [wgpu::Buffer; POINT_SHADOW_FACES],
    point_shadow_frame_bind_groups: [wgpu::BindGroup; POINT_SHADOW_FACES],
    /// 每一级各自的帧常量（阴影 pass 的顶点着色器只读 `shadow_matrix`，
    /// 一级一个 buffer 才能在同一帧里画完 4 级——同一 buffer 的多次写入只会在提交时生效）。
    shadow_frame_buffers: [wgpu::Buffer; MAX_SHADOW_CASCADES],
    shadow_frame_bind_groups: [wgpu::BindGroup; MAX_SHADOW_CASCADES],
    /// 图集里一个分块的边长（像素）；整图边长 = 它的 2 倍。
    shadow_tile_size: u32,

    // 资源表
    meshes: Vec<GpuMesh>,
    materials: Vec<GpuMaterial>,
    textures: Vec<GpuTexture>,
    white_texture: GpuTexture,
    flat_normal_texture: GpuTexture,
    black_texture: GpuTexture,

    // 每帧状态
    staging: Vec<(InstanceRaw, Aabb)>,
    visible: Vec<InstanceRaw>,
    /// 每个批次在可见实例缓冲中的 `(起始, 数量)`。
    visible_ranges: Vec<(u32, u32)>,
    batches: Vec<Batch>,
    batch_index: HashMap<u64, usize>,
    /// 不透明物体的绘制顺序：`(0, double_sided, 材质, 网格, 批次下标)`。
    ///
    /// 半透明**不进这里**：透明物体按相机距离全局由远到近逐实例画，
    /// 顺序在 `render` 内用局部 `alpha_draws` 排；合批会丢掉批次之间的前后关系。
    draw_order: Vec<(u8, u8, MaterialId, MeshId, usize)>,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,

    /// 调试线段收集器。
    pub lines: DebugLines,
    line_buffer: wgpu::Buffer,
    line_capacity: usize,

    // 目标尺寸相关
    depth_view: Option<wgpu::TextureView>,
    /// MSAA 颜色视图（HDR 格式）。
    msaa_view: Option<wgpu::TextureView>,
    /// HDR 中间目标（场景渲染到这里，再由后处理读）。
    hdr_view: Option<wgpu::TextureView>,
    /// 水面 mask（单采样）：主 pass 第二颜色附件解析后的结果，后处理按像素读。
    water_mask_view: Option<wgpu::TextureView>,
    /// 水面 mask 的 MSAA 附件（`sample_count == 1` 时为 `None`，直接画到 `water_mask_view`）。
    water_mask_msaa_view: Option<wgpu::TextureView>,
    /// FXAA 的输入：tonemap 之后的画面（线性显示值，格式与 HDR 目标相同）。
    ///
    /// 只在开了 FXAA 时才会被写/读；关着的时候它只是占内存，不影响任何像素。
    ldr_view: Option<wgpu::TextureView>,
    /// 单采样深度（SSAO 的输入）。`sample_count == 1` 时就是主 pass 那一张，
    /// 多采样时是 `depth_resolve.wgsl` 写出来的那一张。
    ///
    /// 只在开了 SSAO 时才建/才读。
    resolved_depth_view: Option<wgpu::TextureView>,
    /// 1×1 的占位深度视图：SSAO 关闭时占住后处理绑定组的 binding 4。
    ///
    /// 为什么需要它：布局要求单采样深度，而 MSAA > 1 时主 pass 的深度视图是**多采样**的，
    /// 绑上去会被 wgpu 当场拒绝。占位资源只占几十字节，比"为了绑定而多建一张全屏深度"
    /// 划算，也比"按开关改布局"（那样管线也得跟着重建）简单。
    dummy_depth_view: wgpu::TextureView,
    /// Bloom ping-pong（半分辨率）。
    bloom_a_view: Option<wgpu::TextureView>,
    bloom_b_view: Option<wgpu::TextureView>,
    bloom_size: (u32, u32),
    target_size: (u32, u32),
    pub sample_count: u32,
    /// 这个后端能不能把深度纹理当纹理读（GL/WebGL2 不能）。
    ///
    /// 它决定三件事：`post.wgsl` 用哪个变体、`depth_resolve` 这条管线建不建、
    /// 以及 [`Renderer::supports_ssao`] 的返回值。**只在 `new()` 里定一次**——
    /// 它由适配器后端决定，运行期不会变。
    depth_texture_read: bool,
    /// "GL 不支持 SSAO，已忽略"这条警告只打一次（每帧打会把日志刷爆）。
    ssao_warned: bool,

    /// 渲染选项。
    pub options: RendererOptions,
    /// 一帧的时间（秒），供着色器动画使用。
    pub time: f32,
    frame_index: u64,
    stats: RenderStats,
}

/// 创建管线时用到的参数集合。
struct PipelineSpec<'a> {
    label: &'a str,
    layout: &'a wgpu::PipelineLayout,
    module: &'a wgpu::ShaderModule,
    vs_entry: &'a str,
    fs_entry: Option<&'a str>,
    buffers: Vec<Option<wgpu::VertexBufferLayout<'a>>>,
    targets: Vec<Option<wgpu::ColorTargetState>>,
    depth: Option<wgpu::DepthStencilState>,
    sample_count: u32,
    primitive: wgpu::PrimitiveState,
}

fn build_pipeline(device: &wgpu::Device, spec: PipelineSpec<'_>) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(spec.label),
        layout: Some(spec.layout),
        vertex: wgpu::VertexState {
            module: spec.module,
            entry_point: Some(spec.vs_entry),
            compilation_options: Default::default(),
            buffers: &spec.buffers,
        },
        primitive: spec.primitive,
        depth_stencil: spec.depth,
        multisample: wgpu::MultisampleState {
            count: spec.sample_count,
            mask: !0,
            alpha_to_coverage_enabled: false,
        },
        fragment: spec.fs_entry.map(|entry| wgpu::FragmentState {
            module: spec.module,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            targets: &spec.targets,
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// 全屏一趟的管线（后处理与 bloom 都是这类：无顶点缓冲、无深度、无 MSAA）。
fn build_fullscreen_pipeline(
    device: &wgpu::Device,
    label: &str,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    entry: &str,
    format: wgpu::TextureFormat,
) -> wgpu::RenderPipeline {
    build_pipeline(
        device,
        PipelineSpec {
            label,
            layout,
            module,
            vs_entry: "vs_main",
            fs_entry: Some(entry),
            buffers: vec![],
            targets: vec![Some(wgpu::ColorTargetState {
                format,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            depth: None,
            sample_count: 1,
            primitive: wgpu::PrimitiveState::default(),
        },
    )
}

fn texture_binding(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn sampler_binding(binding: u32, filterable: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Sampler(if filterable {
            wgpu::SamplerBindingType::Filtering
        } else {
            wgpu::SamplerBindingType::NonFiltering
        }),
        count: None,
    }
}

/// 材质上的四个贴图槽（用于运行时换贴图）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialTextureSlot {
    /// 基础色。
    BaseColor,
    /// 切线空间法线。
    Normal,
    /// 金属度-粗糙度。
    MetallicRoughness,
    /// 自发光。
    Emissive,
}

impl Renderer {
    /// 创建渲染器（分配管线与默认资源）。
    pub fn new(ctx: &GpuContext, options: RendererOptions) -> Self {
        let device = &ctx.device;
        let sample_count = ctx.msaa_samples;

        // ---------- 布局 ----------
        let frame_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quench3d-frame-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let shadow_bind_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("quench3d-shadow-layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        // 阴影用比较采样器（textureSampleCompare）
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        // 聚光灯阴影图（第二张深度图，同一套 PCF、同一个比较采样器）。
                        // 为什么放在同一个 group：着色器里两条路径在同一个 `fs_main`，
                        // 拆成两个 group 只会让管线布局多一层、绑定多一次。
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        // 点光源阴影图：6 层深度数组（不是 `Cube`，面/uv 由我们的纯函数定）。
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Depth,
                            view_dimension: wgpu::TextureViewDimension::D2Array,
                            multisampled: false,
                        },
                        count: None,
                    },
                ],
            });

        let material_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quench3d-material-layout"),
            entries: &[
                texture_binding(0),
                sampler_binding(1, true),
                texture_binding(2),
                texture_binding(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_binding(5),
                sampler_binding(6, true),
            ],
        });

        let pbr_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quench3d-pbr-layout"),
            bind_group_layouts: &[
                Some(&frame_layout),
                Some(&shadow_bind_layout),
                Some(&material_layout),
            ],
            immediate_size: 0,
        });
        let frame_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("quench3d-frame-only-layout"),
                bind_group_layouts: &[Some(&frame_layout)],
                immediate_size: 0,
            });

        // ---------- 后处理布局 ----------
        // 0 = HDR 颜色，1 = 采样器，2 = 后处理参数，3 = bloom 颜色
        let post_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quench3d-post-layout"),
            entries: &[
                texture_binding(0),
                sampler_binding(1, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture_binding(3),
                // 深度（单采样）：SSAO 用。只用 `textureLoad`，所以不需要采样器
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                // 水面 mask（r = 1）：屏幕空间水的可见性；与深度无关，GL/WebGL2 也能用。
                texture_binding(5),
            ],
        });
        let bloom_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quench3d-bloom-layout"),
            entries: &[
                texture_binding(0),
                sampler_binding(1, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let post_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quench3d-post-pipeline-layout"),
            bind_group_layouts: &[Some(&post_layout)],
            immediate_size: 0,
        });
        let bloom_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("quench3d-bloom-pipeline-layout"),
                bind_group_layouts: &[Some(&bloom_layout)],
                immediate_size: 0,
            });
        // FXAA 读的是后处理 pass 的输出（LDR 中间目标），不需要 bloom 贴图；
        // uniform 复用后处理那一份（texel 尺寸与阈值都在里面）。
        let fxaa_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("quench3d-fxaa-layout"),
            entries: &[
                texture_binding(0),
                sampler_binding(1, true),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let fxaa_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quench3d-fxaa-pipeline-layout"),
            bind_group_layouts: &[Some(&fxaa_layout)],
            immediate_size: 0,
        });
        // 深度解析：只读一张多采样深度纹理
        let depth_resolve_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("quench3d-depth-resolve-layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Depth,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: true,
                    },
                    count: None,
                }],
            });
        let depth_resolve_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("quench3d-depth-resolve-pipeline-layout"),
                bind_group_layouts: &[Some(&depth_resolve_layout)],
                immediate_size: 0,
            });

        // ---------- 着色器 ----------
        // GL/GLES（含浏览器 WebGL2）读不了深度纹理：一切依赖它的东西在这里就必须分叉，
        // 否则建管线时 naga 翻译失败 → wasm 上 panic（见 `post_wgsl_for`）。
        let depth_texture_read = ctx.supports_depth_texture_read();
        let post_module = ctx.create_shader("quench3d-post", &post_wgsl_for(depth_texture_read));
        let bloom_module = ctx.create_shader("quench3d-bloom", BLOOM_WGSL);
        let fxaa_module = ctx.create_shader("quench3d-fxaa", FXAA_WGSL);
        // 深度解析那一趟同样有 `textureLoad(ms_depth, ..)`：不支持就不建（连模块都不建）。
        let depth_resolve_module = depth_texture_read
            .then(|| ctx.create_shader("quench3d-depth-resolve", DEPTH_RESOLVE_WGSL));
        let pbr_module = ctx.create_shader("quench3d-pbr", &expand(PBR_WGSL));
        let shadow_module = ctx.create_shader("quench3d-shadow", &expand(SHADOW_WGSL));
        let sky_module = ctx.create_shader("quench3d-sky", &expand(SKY_WGSL));
        let grid_module = ctx.create_shader("quench3d-grid", &expand(GRID_WGSL));
        let line_module = ctx.create_shader("quench3d-line", &expand(LINE_WGSL));

        // ---------- 帧资源 ----------
        let frame_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quench3d-frame-uniform"),
            size: std::mem::size_of::<FrameUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let frame_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("quench3d-frame-bind-group"),
            layout: &frame_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: frame_uniform.as_entire_binding(),
            }],
        });

        let shadow_size = (options.shadow_map_size.clamp(512, 4096) / 2) * 2;
        let shadow_tile_size = shadow_size / 2;
        let shadow_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("quench3d-shadow-map"),
            size: wgpu::Extent3d {
                width: shadow_size,
                height: shadow_size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SHADOW_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let shadow_view = shadow_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("quench3d-shadow-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let mut shadow_frame_buffers = Vec::with_capacity(MAX_SHADOW_CASCADES);
        let mut shadow_frame_bind_groups = Vec::with_capacity(MAX_SHADOW_CASCADES);
        for i in 0..MAX_SHADOW_CASCADES {
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("quench3d-shadow-frame-uniform"),
                size: std::mem::size_of::<FrameUniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("quench3d-shadow-frame-bind-group"),
                layout: &frame_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                }],
            });
            let _ = i;
            shadow_frame_buffers.push(buf);
            shadow_frame_bind_groups.push(bg);
        }
        let shadow_frame_buffers: [wgpu::Buffer; MAX_SHADOW_CASCADES] =
            shadow_frame_buffers.try_into().expect("正好 4 个");
        let shadow_frame_bind_groups: [wgpu::BindGroup; MAX_SHADOW_CASCADES] =
            shadow_frame_bind_groups.try_into().expect("正好 4 个");

        // 聚光灯阴影图：第二张同样格式的深度图，边长是平行光那张的一半
        let spot_shadow_map_size = shadow_tile_size.max(256);
        let spot_shadow_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("quench3d-spot-shadow-map"),
            size: wgpu::Extent3d {
                width: spot_shadow_map_size,
                height: spot_shadow_map_size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SHADOW_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                // COPY_SRC：调试时要能回读（`read_spot_shadow_map`），与平行光那张一致
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let spot_shadow_view =
            spot_shadow_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let spot_shadow_frame_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quench3d-spot-shadow-frame-uniform"),
            size: std::mem::size_of::<FrameUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let spot_shadow_frame_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("quench3d-spot-shadow-frame-bind-group"),
            layout: &frame_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: spot_shadow_frame_buffer.as_entire_binding(),
            }],
        });

        // 点光源阴影图：6 个层，面序 `+X, -X, +Y, -Y, +Z, -Z`（`point_shadow.rs` 的约定）
        let point_shadow_map_size = (spot_shadow_map_size / 2).max(128);
        let point_shadow_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("quench3d-point-shadow-map"),
            size: wgpu::Extent3d {
                width: point_shadow_map_size,
                height: point_shadow_map_size,
                // 7 层而不是 6 层：wgpu-hal 的 GL 后端会把"正方形 + 6 层"的 D2
                // 纹理猜成 Cube，而这里要绑定成 D2Array（着色器是
                // texture_depth_2d_array）。多留一层让启发式走 D2Array，代价是
                // 一张面大小的深度图（默认 512×512×4 ≈ 1 MB）。
                depth_or_array_layers: POINT_SHADOW_TEXTURE_LAYERS,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SHADOW_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                // COPY_SRC：调试时要能逐面回读（`read_point_shadow_map`）
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let point_shadow_array_view =
            point_shadow_texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("quench3d-point-shadow-array-view"),
                dimension: Some(wgpu::TextureViewDimension::D2Array),
                // 明确只绑定 6 个面，7 层里多出的一层只是躲开 GL 的 Cube 启发式。
                array_layer_count: Some(POINT_SHADOW_FACES as u32),
                ..Default::default()
            });
        let mut point_shadow_face_views = Vec::with_capacity(POINT_SHADOW_FACES);
        let mut point_shadow_frame_buffers = Vec::with_capacity(POINT_SHADOW_FACES);
        let mut point_shadow_frame_bind_groups = Vec::with_capacity(POINT_SHADOW_FACES);
        for face in 0..POINT_SHADOW_FACES {
            point_shadow_face_views.push(point_shadow_texture.create_view(
                &wgpu::TextureViewDescriptor {
                    label: Some("quench3d-point-shadow-face-view"),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: face as u32,
                    array_layer_count: Some(1),
                    ..Default::default()
                },
            ));
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("quench3d-point-shadow-frame-uniform"),
                size: std::mem::size_of::<FrameUniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("quench3d-point-shadow-frame-bind-group"),
                layout: &frame_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buf.as_entire_binding(),
                }],
            });
            let _ = face;
            point_shadow_frame_buffers.push(buf);
            point_shadow_frame_bind_groups.push(bg);
        }
        let point_shadow_face_views: [wgpu::TextureView; POINT_SHADOW_FACES] =
            point_shadow_face_views.try_into().expect("正好 6 个");
        let point_shadow_frame_buffers: [wgpu::Buffer; POINT_SHADOW_FACES] =
            point_shadow_frame_buffers.try_into().expect("正好 6 个");
        let point_shadow_frame_bind_groups: [wgpu::BindGroup; POINT_SHADOW_FACES] =
            point_shadow_frame_bind_groups
                .try_into()
                .expect("正好 6 个");

        let shadow_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("quench3d-shadow-bind-group"),
            layout: &shadow_bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&shadow_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&shadow_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&spot_shadow_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&point_shadow_array_view),
                },
            ],
        });

        // ---------- 管线 ----------
        let mesh_buffer = Some(vertex_layout());
        let instance_buffer_layout = Self::instance_layout();

        let color_targets = main_pass_color_targets;
        let alpha_blend = Some(wgpu::BlendState::ALPHA_BLENDING);
        let depth_state = |write: bool, compare: wgpu::CompareFunction, bias: i32| {
            Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(write),
                depth_compare: Some(compare),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState {
                    constant: bias,
                    slope_scale: 0.0,
                    clamp: 0.0,
                },
            })
        };

        let opaque_pipeline = build_pipeline(
            device,
            PipelineSpec {
                label: "quench3d-opaque",
                layout: &pbr_pipeline_layout,
                module: &pbr_module,
                vs_entry: "vs_main",
                fs_entry: Some("fs_main"),
                buffers: vec![mesh_buffer.clone(), Some(instance_buffer_layout.clone())],
                targets: color_targets(HDR_FORMAT, None),
                depth: depth_state(true, wgpu::CompareFunction::Less, 0),
                sample_count,
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: Some(wgpu::Face::Back),
                    ..Default::default()
                },
            },
        );

        let alpha_pipeline = build_pipeline(
            device,
            PipelineSpec {
                label: "quench3d-alpha",
                layout: &pbr_pipeline_layout,
                module: &pbr_module,
                vs_entry: "vs_main",
                fs_entry: Some("fs_main"),
                buffers: vec![mesh_buffer.clone(), Some(instance_buffer_layout.clone())],
                targets: color_targets(HDR_FORMAT, alpha_blend),
                depth: depth_state(false, wgpu::CompareFunction::Less, 0),
                sample_count,
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
            },
        );

        // 双面材质：不剔除背面（着色器按 front_facing 翻转法线，光照仍然正确）
        let opaque_double_pipeline = build_pipeline(
            device,
            PipelineSpec {
                label: "quench3d-opaque-double-sided",
                layout: &pbr_pipeline_layout,
                module: &pbr_module,
                vs_entry: "vs_main",
                fs_entry: Some("fs_main"),
                buffers: vec![mesh_buffer.clone(), Some(instance_buffer_layout.clone())],
                targets: color_targets(HDR_FORMAT, None),
                depth: depth_state(true, wgpu::CompareFunction::Less, 0),
                sample_count,
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
            },
        );

        let shadow_pipeline = build_pipeline(
            device,
            PipelineSpec {
                label: "quench3d-shadow",
                layout: &frame_pipeline_layout,
                module: &shadow_module,
                vs_entry: "vs_main",
                fs_entry: None,
                buffers: vec![mesh_buffer.clone(), Some(instance_buffer_layout.clone())],
                targets: vec![],
                depth: Some(wgpu::DepthStencilState {
                    format: SHADOW_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: wgpu::StencilState::default(),
                    // 正面剔除 + 深度偏移：显著减少阴影痤疮
                    bias: wgpu::DepthBiasState {
                        constant: 2,
                        slope_scale: 2.0,
                        clamp: 0.0,
                    },
                }),
                sample_count: 1,
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: Some(wgpu::Face::Front),
                    ..Default::default()
                },
            },
        );

        let sky_pipeline = build_pipeline(
            device,
            PipelineSpec {
                label: "quench3d-sky",
                layout: &frame_pipeline_layout,
                module: &sky_module,
                vs_entry: "vs_main",
                fs_entry: Some("fs_main"),
                buffers: vec![],
                targets: color_targets(HDR_FORMAT, None),
                depth: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                sample_count,
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
            },
        );

        let grid_pipeline = build_pipeline(
            device,
            PipelineSpec {
                label: "quench3d-grid",
                layout: &frame_pipeline_layout,
                module: &grid_module,
                vs_entry: "vs_main",
                fs_entry: Some("fs_main"),
                buffers: vec![],
                targets: color_targets(HDR_FORMAT, alpha_blend),
                depth: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    // `grid.wgsl` 写了 `frag_depth`；这里必须真的允许写，否则调试线段
                    // 会穿过地面画出来（网格看起来"不挡东西"）。网格在透明物体之后、
                    // 线段之前绘制，写深度后它只遮挡**它后面**的调试几何。
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                sample_count,
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    cull_mode: None,
                    ..Default::default()
                },
            },
        );

        let line_pipeline = build_pipeline(
            device,
            PipelineSpec {
                label: "quench3d-lines",
                layout: &frame_pipeline_layout,
                module: &line_module,
                vs_entry: "vs_main",
                fs_entry: Some("fs_main"),
                buffers: vec![Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<LineVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
                })],
                targets: color_targets(HDR_FORMAT, alpha_blend),
                depth: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    // 线段拓扑不支持深度偏移，这里靠 LessEqual + 略微前移的顶点着色器偏移
                    bias: wgpu::DepthBiasState::default(),
                }),
                sample_count,
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::LineList,
                    cull_mode: None,
                    ..Default::default()
                },
            },
        );

        // ---------- 后处理管线 ----------
        // 后处理输出到**真正的目标**（`ctx.format`，通常是 sRGB）：着色器给线性值，
        // sRGB 编码由目标格式的硬件转换完成——和改造前物体着色器直接写目标时的假设一致。
        let post_pipeline = build_fullscreen_pipeline(
            device,
            "quench3d-post",
            &post_pipeline_layout,
            &post_module,
            "fs_main",
            ctx.format,
        );
        // 开了 FXAA 时后处理写 LDR 中间目标（`rgba16float`）而不是最终目标：
        // **颜色附件格式不同 = 必须是一条独立管线**（wgpu 会在绘制时报格式不兼容）。
        let post_ldr_pipeline = build_fullscreen_pipeline(
            device,
            "quench3d-post-to-ldr",
            &post_pipeline_layout,
            &post_module,
            "fs_main",
            HDR_FORMAT,
        );
        // bloom 三步都写 HDR 格式的半分辨率贴图，不需要深度与 MSAA。
        let bloom_bright_pipeline = build_fullscreen_pipeline(
            device,
            "quench3d-bloom-bright",
            &bloom_pipeline_layout,
            &bloom_module,
            "fs_bright",
            HDR_FORMAT,
        );
        let bloom_blur_h_pipeline = build_fullscreen_pipeline(
            device,
            "quench3d-bloom-blur-h",
            &bloom_pipeline_layout,
            &bloom_module,
            "fs_blur_h",
            HDR_FORMAT,
        );
        let bloom_blur_v_pipeline = build_fullscreen_pipeline(
            device,
            "quench3d-bloom-blur-v",
            &bloom_pipeline_layout,
            &bloom_module,
            "fs_blur_v",
            HDR_FORMAT,
        );
        // 深度解析：没有颜色附件，只有深度附件；`frag_depth` 写回同一格式的深度目标。
        // 比较函数是 `Always`（我们不是在做深度测试，只是在搬运数值）。
        //
        // 只在"能读深度纹理"的后端上建。它读的是 `texture_depth_multisampled_2d`，
        // 同样是 naga GLSL 后端不支持的形式 —— 在 GL 上建它就等于开机 panic，
        // 而 GL 上 SSAO 本来就整体关掉了，这一趟没有任何用处。
        let depth_resolve_pipeline = depth_resolve_module.as_ref().map(|module| {
            build_pipeline(
                device,
                PipelineSpec {
                    label: "quench3d-depth-resolve",
                    layout: &depth_resolve_pipeline_layout,
                    module,
                    vs_entry: "vs_main",
                    fs_entry: Some("fs_main"),
                    buffers: vec![],
                    targets: vec![],
                    depth: Some(wgpu::DepthStencilState {
                        format: DEPTH_FORMAT,
                        depth_write_enabled: Some(true),
                        depth_compare: Some(wgpu::CompareFunction::Always),
                        stencil: wgpu::StencilState::default(),
                        bias: wgpu::DepthBiasState::default(),
                    }),
                    sample_count: 1,
                    primitive: wgpu::PrimitiveState::default(),
                },
            )
        });
        // FXAA 输出到**真正的目标**（`ctx.format`，sRGB）：着色器给的是线性值，
        // 编码由目标格式的硬件转换完成（和 post 那一趟一样）。
        let fxaa_pipeline = build_fullscreen_pipeline(
            device,
            "quench3d-fxaa",
            &fxaa_pipeline_layout,
            &fxaa_module,
            "fs_main",
            ctx.format,
        );

        let dummy_depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("quench3d-dummy-depth"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let dummy_depth_view = dummy_depth.create_view(&wgpu::TextureViewDescriptor::default());
        let post_uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quench3d-post-uniform"),
            size: std::mem::size_of::<PostUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let post_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("quench3d-post-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // ---------- 默认纹理 ----------
        let white_texture = default_texture(device, &ctx.queue, &ImageData::white(), "white");
        let flat_normal_texture =
            default_texture(device, &ctx.queue, &ImageData::flat_normal(), "flat-normal");
        let black_texture = default_texture(
            device,
            &ctx.queue,
            &ImageData::solid([0, 0, 0, 255]),
            "black",
        );

        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quench3d-instances"),
            size: (std::mem::size_of::<InstanceRaw>() * 256) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let line_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quench3d-lines"),
            size: (std::mem::size_of::<LineVertex>() * 1024) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            frame_layout,
            shadow_bind_layout,
            material_layout,
            opaque_pipeline,
            alpha_pipeline,
            opaque_double_pipeline,
            shadow_pipeline,
            sky_pipeline,
            grid_pipeline,
            line_pipeline,
            frame_uniform,
            frame_bind_group,
            shadow_texture,
            shadow_view,
            shadow_sampler,
            shadow_bind_group,
            spot_shadow_texture,
            spot_shadow_view,
            spot_shadow_map_size,
            spot_shadow_frame_buffer,
            spot_shadow_frame_bind_group,
            point_shadow_texture,
            point_shadow_array_view,
            point_shadow_face_views,
            point_shadow_map_size,
            point_shadow_frame_buffers,
            point_shadow_frame_bind_groups,
            shadow_frame_buffers,
            shadow_frame_bind_groups,
            shadow_tile_size,
            meshes: Vec::new(),
            materials: Vec::new(),
            textures: Vec::new(),
            white_texture,
            flat_normal_texture,
            black_texture,
            staging: Vec::new(),
            visible: Vec::new(),
            visible_ranges: Vec::new(),
            batches: Vec::new(),
            batch_index: HashMap::new(),
            draw_order: Vec::new(),
            instance_buffer,
            instance_capacity: 256,
            lines: DebugLines::new(),
            line_buffer,
            line_capacity: 1024,
            post_pipeline,
            post_ldr_pipeline,
            bloom_bright_pipeline,
            bloom_blur_h_pipeline,
            bloom_blur_v_pipeline,
            fxaa_pipeline,
            depth_resolve_pipeline,
            post_layout,
            bloom_layout,
            fxaa_layout,
            depth_resolve_layout,
            post_uniform,
            post_sampler,
            post_bind_group: None,
            bloom_bright_bind_group: None,
            bloom_blur_h_bind_group: None,
            bloom_blur_v_bind_group: None,
            fxaa_bind_group: None,
            depth_resolve_bind_group: None,
            depth_view: None,
            msaa_view: None,
            hdr_view: None,
            water_mask_view: None,
            water_mask_msaa_view: None,
            ldr_view: None,
            resolved_depth_view: None,
            dummy_depth_view,
            bloom_a_view: None,
            bloom_b_view: None,
            bloom_size: (0, 0),
            target_size: (0, 0),
            sample_count,
            depth_texture_read,
            ssao_warned: false,
            options,
            time: 0.0,
            frame_index: 0,
            stats: RenderStats::default(),
        }
    }

    /// 这个渲染器**能不能**做 SSAO。
    ///
    /// GL/GLES（含浏览器 WebGL2）上是 `false`：那个后端的 GLSL 翻译不支持读深度纹理，
    /// 详见 [`crate::context::supports_depth_texture_read`]。调用方（编辑器的 UI、
    /// e2e 测试）应当用它决定"要不要真的断言 AO 生效"，而不是直接看
    /// [`RendererOptions::ssao`] 里的开关。
    pub fn supports_ssao(&self) -> bool {
        self.depth_texture_read
    }

    /// 这一帧 SSAO 是否**真的**生效：配置里开着 **且** 后端支持。
    ///
    /// 后端不支持时会打一条警告（只打一次：这个方法每帧都会被调用）。
    fn ssao_active(&mut self) -> bool {
        if self.options.ssao.enabled && !self.depth_texture_read {
            if !self.ssao_warned {
                self.ssao_warned = true;
                log::warn!(
                    "当前后端读不了深度纹理（GL/GLES 的 naga GLSL 限制），SSAO 已忽略；\
                     画面与其它的后处理不受影响"
                );
            }
            return false;
        }
        self.options.ssao.enabled
    }

    fn instance_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
            4 => Float32x4,
            5 => Float32x4,
            6 => Float32x4,
            7 => Float32x4,
            8 => Float32x4,
            9 => Float32x4
        ];
        wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<InstanceRaw>() as u64,
            step_mode: wgpu::VertexStepMode::Instance,
            attributes: &ATTRS,
        }
    }

    // -----------------------------------------------------------------
    // 资源上传
    // -----------------------------------------------------------------

    /// 上传网格，返回句柄。
    pub fn add_mesh(&mut self, ctx: &GpuContext, mesh: &MeshData) -> MeshId {
        self.add_mesh_named(ctx, mesh, "mesh")
    }

    /// 上传网格并命名（调试用）。
    pub fn add_mesh_named(&mut self, ctx: &GpuContext, mesh: &MeshData, name: &str) -> MeshId {
        let device = &ctx.device;
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quench3d-vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quench3d-indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        let id = self.meshes.len() as MeshId;
        self.meshes.push(GpuMesh {
            vertex_buffer,
            index_buffer,
            index_count: mesh.indices.len() as u32,
            bounds: mesh.aabb(),
            name: name.to_string(),
        });
        id
    }

    /// 上传纹理，返回句柄。
    pub fn add_texture(&mut self, ctx: &GpuContext, image: &ImageData, srgb: bool) -> TextureId {
        let options = if srgb {
            crate::texture::TextureOptions::color()
        } else {
            crate::texture::TextureOptions::data()
        };
        let texture = GpuTexture::upload(&ctx.device, &ctx.queue, image, options);
        let id = self.textures.len() as TextureId;
        self.textures.push(texture);
        id
    }

    /// 创建材质，返回句柄。
    pub fn add_material(&mut self, ctx: &GpuContext, desc: MaterialDesc) -> MaterialId {
        self.add_material_named(ctx, desc, "material")
    }

    /// 构造材质的绑定组（四张贴图 view + 两个采样器 + uniform 都烘在这里）。
    ///
    /// 单独抽出来是为了**换贴图**：贴图句柄是烘进绑定组的，换一张贴图必须重建它。
    fn build_material_bind_group(
        &self,
        ctx: &GpuContext,
        desc: &MaterialDesc,
        uniform_buffer: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let device = &ctx.device;
        let view_for = |id: Option<TextureId>, fallback: &GpuTexture| -> wgpu::TextureView {
            match id.and_then(|i| self.textures.get(i as usize)) {
                Some(t) => t.view.clone(),
                None => fallback.view.clone(),
            }
        };
        let base_view = view_for(desc.base_color_texture, &self.white_texture);
        let normal_view = view_for(desc.normal_texture, &self.flat_normal_texture);
        let mr_view = view_for(desc.metallic_roughness_texture, &self.white_texture);
        let emissive_view = view_for(desc.emissive_texture, &self.black_texture);
        // 采样器统一使用基础色纹理的采样器（贴图参数差异对最终效果影响很小）
        let sampler = self
            .textures
            .get(desc.base_color_texture.unwrap_or(u32::MAX) as usize)
            .map(|t| t.sampler.clone())
            .unwrap_or_else(|| self.white_texture.sampler.clone());
        let data_sampler = self
            .textures
            .get(desc.normal_texture.unwrap_or(u32::MAX) as usize)
            .map(|t| t.sampler.clone())
            .unwrap_or_else(|| self.flat_normal_texture.sampler.clone());

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("quench3d-material-bind-group"),
            layout: &self.material_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&base_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&normal_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&mr_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&emissive_view),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::Sampler(&data_sampler),
                },
            ],
        });

        bind_group
    }

    /// 创建材质并命名。
    pub fn add_material_named(
        &mut self,
        ctx: &GpuContext,
        desc: MaterialDesc,
        name: &str,
    ) -> MaterialId {
        let device = &ctx.device;
        let uniform = desc.to_uniform();
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quench3d-material-uniform"),
            contents: bytemuck::bytes_of(&uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let bind_group = self.build_material_bind_group(ctx, &desc, &uniform_buffer);

        let id = self.materials.len() as MaterialId;
        self.materials.push(GpuMaterial {
            desc,
            bind_group,
            uniform_buffer,
            name: name.to_string(),
        });
        id
    }

    /// 覆盖已有材质的 GPU 常量（用于运行时调整颜色/粗糙度）。
    pub fn update_material(&mut self, ctx: &GpuContext, id: MaterialId, desc: MaterialDesc) {
        if let Some(m) = self.materials.get_mut(id as usize) {
            let uniform = desc.to_uniform();
            ctx.queue
                .write_buffer(&m.uniform_buffer, 0, bytemuck::bytes_of(&uniform));
            m.desc = desc;
        }
    }

    /// 换掉材质上的某张贴图（`None` = 还原成内置的默认贴图）。
    ///
    /// 贴图 view 是烘在绑定组里的，所以这里会**重建绑定组**；
    /// 材质不存在、或传入的贴图 id 无效时返回 `false`，调用方据此报错而不是静默失败。
    pub fn set_material_texture(
        &mut self,
        ctx: &GpuContext,
        id: MaterialId,
        slot: MaterialTextureSlot,
        texture: Option<TextureId>,
    ) -> bool {
        if texture.is_some_and(|t| self.textures.get(t as usize).is_none()) {
            return false;
        }
        let Some(existing) = self.materials.get(id as usize) else {
            return false;
        };
        let mut desc = existing.desc.clone();
        match slot {
            MaterialTextureSlot::BaseColor => desc.base_color_texture = texture,
            MaterialTextureSlot::Normal => desc.normal_texture = texture,
            MaterialTextureSlot::MetallicRoughness => desc.metallic_roughness_texture = texture,
            MaterialTextureSlot::Emissive => desc.emissive_texture = texture,
        }
        let uniform_buffer = self.materials[id as usize].uniform_buffer.clone();
        let bind_group = self.build_material_bind_group(ctx, &desc, &uniform_buffer);
        let Some(m) = self.materials.get_mut(id as usize) else {
            return false;
        };
        m.desc = desc;
        m.bind_group = bind_group;
        true
    }

    /// 读取材质描述。
    pub fn material(&self, id: MaterialId) -> Option<&MaterialDesc> {
        self.materials.get(id as usize).map(|m| &m.desc)
    }

    /// 材质名称（调试用）。
    pub fn material_name(&self, id: MaterialId) -> Option<&str> {
        self.materials.get(id as usize).map(|m| m.name.as_str())
    }

    /// 原地更新一个网格的顶点（CPU 蒙皮/形变用）；索引与拓扑保持不变。
    ///
    /// 返回 `false` 表示网格下标越界。顶点数不变时只写缓冲区（不重建、不换句柄）；
    /// 顶点数变化时才重建顶点缓冲，并更新包围盒。索引缓冲按**旧拓扑**继续使用，
    /// 所以调用方必须保证索引仍然有效（蒙皮正是这样：只动顶点，不动面）。
    pub fn update_mesh_vertices(
        &mut self,
        ctx: &GpuContext,
        id: MeshId,
        vertices: &[crate::Vertex],
    ) -> bool {
        let Some(mesh) = self.meshes.get_mut(id as usize) else {
            return false;
        };
        let bytes = bytemuck::cast_slice(vertices);
        let needed = bytes.len() as wgpu::BufferAddress;
        if needed <= mesh.vertex_buffer.size() {
            ctx.queue.write_buffer(&mesh.vertex_buffer, 0, bytes);
        } else {
            mesh.vertex_buffer = ctx
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("quench3d-vertices"),
                    contents: bytes,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                });
        }
        if !vertices.is_empty() {
            let mut min = Vec3::splat(f32::INFINITY);
            let mut max = Vec3::splat(f32::NEG_INFINITY);
            for v in vertices {
                let p = Vec3::from_array(v.position);
                min = min.min(p);
                max = max.max(p);
            }
            mesh.bounds = Aabb::new(min, max);
        }
        true
    }

    /// 网格信息 `(名称, 索引数, 包围盒)`。
    pub fn mesh_info(&self, id: MeshId) -> Option<(&str, u32, Aabb)> {
        self.meshes
            .get(id as usize)
            .map(|m| (m.name.as_str(), m.index_count, m.bounds))
    }

    /// 已上传资源数量 `(网格, 材质, 纹理)`。
    pub fn resource_counts(&self) -> (usize, usize, usize) {
        (self.meshes.len(), self.materials.len(), self.textures.len())
    }

    // -----------------------------------------------------------------
    // 每帧绘制
    // -----------------------------------------------------------------

    /// 开始一帧（清空待绘制列表）。
    pub fn begin_frame(&mut self) {
        self.staging.clear();
        self.batches.clear();
        self.batch_index.clear();
        self.lines.clear();
    }

    /// 提交一次绘制。
    pub fn draw(&mut self, mesh: MeshId, material: MaterialId, instance: InstanceDesc) {
        let bounds = match self.meshes.get(mesh as usize) {
            Some(m) => m.bounds.transformed(&instance.transform.matrix()),
            None => return,
        };
        let key = ((mesh as u64) << 32) | material as u64;
        let raw = instance.to_raw();
        let index = match self.batch_index.get(&key) {
            Some(i) => *i,
            None => {
                let i = self.batches.len();
                self.batches.push(Batch {
                    mesh,
                    material,
                    first: self.staging.len() as u32,
                    count: 0,
                    bounds: Aabb::EMPTY,
                });
                self.batch_index.insert(key, i);
                i
            }
        };
        let batch = &mut self.batches[index];
        batch.count += 1;
        batch.bounds = batch.bounds.union(&bounds);
        self.staging.push((raw, bounds));
    }

    /// 批量提交同一网格/材质的多个实例。
    pub fn draw_batch(
        &mut self,
        mesh: MeshId,
        material: MaterialId,
        instances: impl IntoIterator<Item = InstanceDesc>,
    ) {
        for i in instances {
            self.draw(mesh, material, i);
        }
    }

    fn ensure_instance_capacity(&mut self, device: &wgpu::Device, needed: usize) {
        if needed <= self.instance_capacity {
            return;
        }
        let cap = needed.next_power_of_two().max(256);
        self.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quench3d-instances"),
            size: (std::mem::size_of::<InstanceRaw>() * cap) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.instance_capacity = cap;
    }

    fn ensure_line_capacity(&mut self, device: &wgpu::Device, needed: usize) {
        if needed <= self.line_capacity {
            return;
        }
        let cap = needed.next_power_of_two().max(1024);
        self.line_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quench3d-lines"),
            size: (std::mem::size_of::<LineVertex>() * cap) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.line_capacity = cap;
    }

    fn ensure_targets(&mut self, ctx: &GpuContext, size: (u32, u32)) {
        // 开了 SSAO 就得有单采样深度可用（多采样时是解析出来的那一张）。
        // 这一条必须进提前返回的判断：编辑器里勾选 SSAO 是**运行时**改 options 的。
        //
        // `ssao_active()` 已经把"后端读不了深度纹理"这一条算进去了：GL 上它恒为 false，
        // 于是 `resolved_depth_view` 不会建、后处理绑定组绑占位深度视图、
        // `post.wgsl` 用的也是删掉 SSAO 的那个变体 —— 三者必须**同时**成立，
        // 否则要么管线建不出来，要么着色器读了没绑的资源。
        let ssao_on = self.ssao_active();
        let ssao_depth_ready = ssao_on == self.resolved_depth_view.is_some();
        if self.target_size == size && self.depth_view.is_some() && ssao_depth_ready {
            return;
        }
        let device = &ctx.device;
        let (w, h) = (size.0.max(1), size.1.max(1));
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("quench3d-depth"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: self.sample_count,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            // TEXTURE_BINDING：SSAO 要把深度当纹理读（`sample_count == 1` 时直接读这一张）
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        self.depth_view = Some(depth_view.clone());

        // HDR 中间目标：场景渲染到这里（线性、rgba16float），后处理再读它做 tonemap。
        let hdr = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("quench3d-hdr"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HDR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let hdr_view = hdr.create_view(&wgpu::TextureViewDescriptor::default());
        self.hdr_view = Some(hdr_view.clone());

        // 水面 mask：单采样目标（后处理读）+（MSAA 时）多采样附件。
        let mask = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("quench3d-water-mask"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: WATER_MASK_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let mask_view = mask.create_view(&wgpu::TextureViewDescriptor::default());
        self.water_mask_view = Some(mask_view);
        self.water_mask_msaa_view = (self.sample_count > 1).then(|| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("quench3d-water-mask-msaa"),
                    size: wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: self.sample_count,
                    dimension: wgpu::TextureDimension::D2,
                    format: WATER_MASK_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        });

        // LDR 中间目标：FXAA 的输入（tonemap 之后的画面）。格式与 HDR 目标相同
        // （`rgba16float`）：着色器给线性显示值，最后写 sRGB 目标时由硬件编码一次；
        // 若改用 8 位 sRGB 贴图，"先编码再解码"会在混合处多一次量化。
        let ldr = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("quench3d-ldr"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: HDR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let ldr_view = ldr.create_view(&wgpu::TextureViewDescriptor::default());
        self.ldr_view = Some(ldr_view.clone());

        // SSAO 的深度输入：多采样时先解析成单采样；单采样时就是主 pass 那一张。
        // 只在（真的能生效的）SSAO 开着时建（关掉就什么都不多占用、像素也与以前逐位相同）。
        if ssao_on && self.sample_count > 1 {
            let resolved = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("quench3d-depth-resolved"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: DEPTH_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let resolved_view = resolved.create_view(&wgpu::TextureViewDescriptor::default());
            self.depth_resolve_bind_group =
                Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("quench3d-depth-resolve-bind-group"),
                    layout: &self.depth_resolve_layout,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&depth_view),
                    }],
                }));
            self.resolved_depth_view = Some(resolved_view);
        } else {
            self.depth_resolve_bind_group = None;
            self.resolved_depth_view = if ssao_on {
                Some(depth_view.clone())
            } else {
                None
            };
        }

        if self.sample_count > 1 {
            let msaa = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("quench3d-msaa"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: self.sample_count,
                dimension: wgpu::TextureDimension::D2,
                // 注意是 HDR 格式：MSAA 颜色缓冲必须与主 pass 的颜色目标格式一致
                format: HDR_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            self.msaa_view = Some(msaa.create_view(&wgpu::TextureViewDescriptor::default()));
        } else {
            self.msaa_view = None;
        }

        // Bloom ping-pong（半分辨率：外溢半径按屏幕比例算，半分辨率足够且省一半带宽）
        let (bw, bh) = ((w.div_ceil(2)).max(1), (h.div_ceil(2)).max(1));
        self.bloom_size = (bw, bh);
        let make_bloom = |label: &str| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: bw,
                        height: bh,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: HDR_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&wgpu::TextureViewDescriptor::default())
        };
        let bloom_a = make_bloom("quench3d-bloom-a");
        let bloom_b = make_bloom("quench3d-bloom-b");
        self.bloom_a_view = Some(bloom_a.clone());
        self.bloom_b_view = Some(bloom_b.clone());

        // 绑定组随尺寸重建
        self.post_bind_group = Some(
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("quench3d-post-bind-group"),
                layout: &self.post_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&hdr_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.post_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.post_uniform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(&bloom_a),
                    },
                    // 深度：SSAO 关闭时绑 1×1 占位（布局固定；着色器在 `ssao.x <= 0.5`
                    // 时根本不读它）。不能绑主 pass 那一张：MSAA > 1 时它是多采样的。
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: wgpu::BindingResource::TextureView(
                            self.resolved_depth_view
                                .as_ref()
                                .unwrap_or(&self.dummy_depth_view),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 5,
                        resource: wgpu::BindingResource::TextureView(
                            self.water_mask_view.as_ref().expect("水面 mask 目标缺失"),
                        ),
                    },
                ],
            }),
        );
        let bloom_bind = |label: &str, src: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout: &self.bloom_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(src),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.post_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.post_uniform.as_entire_binding(),
                    },
                ],
            })
        };
        self.bloom_bright_bind_group =
            Some(bloom_bind("quench3d-bloom-bright-bind-group", &hdr_view));
        self.bloom_blur_h_bind_group =
            Some(bloom_bind("quench3d-bloom-blur-h-bind-group", &bloom_a));
        self.bloom_blur_v_bind_group =
            Some(bloom_bind("quench3d-bloom-blur-v-bind-group", &bloom_b));
        self.fxaa_bind_group = Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("quench3d-fxaa-bind-group"),
            layout: &self.fxaa_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&ldr_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.post_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.post_uniform.as_entire_binding(),
                },
            ],
        }));
        self.target_size = (w, h);
    }

    /// 计算本帧的阴影视图投影矩阵。
    /// 算这一帧的阴影级联：每一级的光源空间矩阵 + 每一级的远平面距离。
    ///
    /// 返回的矩阵个数同时就是级联数量；**0 个**表示这一帧不投影阴影（没有平行光、
    /// 或 `cast_shadows` / `shadows` 关掉了）。
    ///
    /// 单级（`shadow_cascades = 1`）保持老行为：围绕相机前方 `shadow_radius` 的一个正交盒，
    /// 用 `shadow_radius` 而不是相机视锥来决定范围——老场景的阴影完全不变（逐像素一致）。
    fn shadow_cascades_for(&self, camera: &Camera, lighting: &Lighting) -> (Vec<Mat4>, Vec<f32>) {
        let Some(s) = lighting
            .sun
            .filter(|s| s.cast_shadows && self.options.shadows)
        else {
            return (Vec::new(), vec![camera.far]);
        };
        let radius = s.shadow_radius.clamp(2.0, 4000.0);
        let n = self
            .options
            .shadow_cascades
            .clamp(1, MAX_SHADOW_CASCADES as u32);
        if n <= 1 {
            let focus = camera.position + camera.forward() * (radius * 0.45);
            // 纹素吸附必须用**实际纹理尺寸**，不是 `DirectionalLight.shadow_map_size`：
            // 两者不一致时（应用层可以分别设置）阴影会随相机轻微抖动，PCF 也会按错
            // 的纹素尺寸偏移。`self.options.shadow_map_size` 才是真正建出来的那张图。
            let map_size = self.options.shadow_map_size.clamp(512, 4096) as f32;
            return (
                vec![s.shadow_matrix_with_map_size(focus, radius, radius * 2.0, map_size)],
                vec![camera.far.max(camera.near + 1.0)],
            );
        }
        // 多级：按相机视锥切段。近平面夹一下，避免 near 太小导致第一级只有几厘米。
        let near = camera.near.max(0.05);
        let far = camera.far.max(near + 1.0);
        let splits = crate::cascade::splits(near, far, n, self.options.shadow_split_lambda);
        let inv_view_proj = camera.view_projection().inverse();
        let tile = self.shadow_tile_size.max(256) as f32;
        let matrices = crate::cascade::matrices(&s, &inv_view_proj, near, far, &splits, tile);
        (matrices, splits)
    }

    fn build_frame_uniforms(
        &self,
        camera: &Camera,
        lighting: &Lighting,
        cascades: &[Mat4],
        splits: &[f32],
        spot_shadow: Option<SpotShadowPlan>,
        point_shadow: Option<PointShadowPlan>,
    ) -> FrameUniforms {
        let shadow_matrix = cascades.first().copied().unwrap_or(Mat4::IDENTITY);
        let mut shadow_matrices = [[[0.0f32; 4]; 4]; MAX_SHADOW_CASCADES];
        for (i, m) in cascades.iter().take(MAX_SHADOW_CASCADES).enumerate() {
            shadow_matrices[i] = m.to_cols_arrays();
        }
        let mut cascade_splits = [camera.far.max(camera.near + 1.0); 4];
        for (i, d) in splits.iter().take(4).enumerate() {
            cascade_splits[i] = *d;
        }
        let cascade_count = cascades.len().min(MAX_SHADOW_CASCADES);
        // 分块之后的纹素尺寸：PCF 的偏移与深度偏移都要按**分块**算，否则多级阴影会有条纹
        let (tile_scale, texel_map) = if cascade_count > 1 {
            (0.5f32, self.shadow_tile_size.max(256) as f32)
        } else {
            (1.0f32, self.options.shadow_map_size.clamp(512, 4096) as f32)
        };
        let view_proj = camera.view_projection();
        let inv_view_proj = view_proj.inverse();
        let sun = lighting.sun;

        let (point_lights, count) =
            pack_local_lights(&lighting.point_lights, &lighting.spot_lights);

        // 聚光阴影：没有投影灯时写 `x = -1`（着色器据此整段跳过），矩阵写单位阵
        let (spot_shadow_matrix, spot_shadow_params, spot_shadow_texel) = match spot_shadow {
            Some(p) => (
                p.camera.view_proj.to_cols_arrays(),
                [
                    p.packed_index as f32,
                    p.camera.near,
                    p.camera.far,
                    p.camera.depth_bias_texels,
                ],
                [
                    1.0 / self.spot_shadow_map_size.max(1) as f32,
                    p.camera.tan_half_fov,
                    p.camera.normal_offset_texels,
                    // 斜率因子不写死在 WGSL 里：它属于"参数"，与 bias 一起从 Rust 传
                    crate::spot_shadow::SPOT_SHADOW_SLOPE_FACTOR,
                ],
            ),
            None => (
                Mat4::IDENTITY.to_cols_arrays(),
                [-1.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, 0.0],
            ),
        };

        let (point_shadow_params, point_shadow_texel) =
            point_shadow_uniforms(point_shadow, self.point_shadow_map_size);

        let (light_dir, light_color) = match sun {
            Some(s) => {
                let d = s.to_light();
                (
                    [d.x, d.y, d.z, s.intensity],
                    [
                        s.color.x,
                        s.color.y,
                        s.color.z,
                        if s.cast_shadows && self.options.shadows {
                            1.0
                        } else {
                            0.0
                        },
                    ],
                )
            }
            None => ([0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 0.0]),
        };

        FrameUniforms {
            view_proj: view_proj.to_cols_arrays(),
            inv_view_proj: inv_view_proj.to_cols_arrays(),
            camera_pos: [camera.position.x, camera.position.y, camera.position.z, 1.0],
            light_dir,
            light_color,
            ambient: [
                lighting.ambient_color.x,
                lighting.ambient_color.y,
                lighting.ambient_color.z,
                lighting.ambient_intensity,
            ],
            shadow_matrix: shadow_matrix.to_cols_arrays(),
            view: camera.view().to_cols_arrays(),
            shadow_matrices,
            cascade_splits,
            cascade_info: [
                cascade_count as f32,
                tile_scale,
                0.0,
                if self.options.environment { 1.0 } else { 0.0 },
            ],
            sky_zenith: [
                lighting.sky.zenith.x,
                lighting.sky.zenith.y,
                lighting.sky.zenith.z,
                1.0,
            ],
            sky_horizon: [
                lighting.sky.horizon.x,
                lighting.sky.horizon.y,
                lighting.sky.horizon.z,
                1.0,
            ],
            sky_ground: [
                lighting.sky.ground.x,
                lighting.sky.ground.y,
                lighting.sky.ground.z,
                1.0,
            ],
            fog: [
                lighting.fog.color.x,
                lighting.fog.color.y,
                lighting.fog.color.z,
                lighting.fog.density,
            ],
            params: [
                self.time,
                // 深度偏移：约 1.5 个世界空间纹素。这是**归一化深度**里的量，
                // 只有各级共用同一个深度范围时才等于同一个世界偏移——`cascade::matrices`
                // 保证这一点（各级都按最大那一段的 2*radius 建矩阵）。
                // 每级用自己的范围会让近处的世界 bias 小一个数量级：实测近景受影响像素
                // 5.4% → 15.7%（满屏麻点），所以这条不变量有单测钉着。
                0.75 / texel_map,
                1.0 / texel_map,
                lighting.shadow_strength.clamp(0.0, 1.0),
            ],
            point_lights,
            params2: [
                count as f32,
                if lighting.sky.enabled && self.options.sky {
                    1.0
                } else {
                    0.0
                },
                lighting.exposure.max(0.001),
                lighting.sky.grid_height,
            ],
            spot_shadow_matrix,
            spot_shadow_params,
            spot_shadow_texel,
            point_shadow_params,
            point_shadow_texel,
        }
    }

    /// 渲染到指定视图。
    pub fn render(
        &mut self,
        ctx: &GpuContext,
        target: &wgpu::TextureView,
        size: (u32, u32),
        camera: &Camera,
        lighting: &Lighting,
    ) -> RenderStats {
        self.frame_index += 1;
        let mut stats = RenderStats {
            frame_index: self.frame_index,
            ..Default::default()
        };
        self.ensure_targets(ctx, size);
        self.ensure_instance_capacity(&ctx.device, self.staging.len().max(1));
        self.ensure_line_capacity(&ctx.device, self.lines.vertices.len().max(1));

        let (cascades, splits) = self.shadow_cascades_for(camera, lighting);
        let shadow_count = cascades.len().min(MAX_SHADOW_CASCADES);
        // 聚光阴影：整帧最多一盏（规则见 `spot_shadow::pick_spot_shadow`）。
        // `self.options.shadows` 是总开关：关掉它，太阳与聚光都不投影。
        let spot_shadow = plan_spot_shadow(
            &lighting.point_lights,
            &lighting.spot_lights,
            self.options.shadows,
        );
        // 点光源阴影：另一套资源（6 层数组），所以与聚光阴影**可以同时存在**
        let point_shadow = plan_point_shadow(&lighting.point_lights, self.options.shadows);
        let uniforms = self.build_frame_uniforms(
            camera,
            lighting,
            &cascades,
            &splits,
            spot_shadow,
            point_shadow,
        );
        ctx.queue
            .write_buffer(&self.frame_uniform, 0, bytemuck::bytes_of(&uniforms));

        // 阴影 pass 每一级各自一个帧常量：顶点着色器只读 `shadow_matrix`，
        // 一级一个 buffer 才能在同一帧里画完多级。
        for i in 0..MAX_SHADOW_CASCADES {
            let mut one = uniforms;
            one.shadow_matrix = cascades
                .get(i)
                .copied()
                .unwrap_or(Mat4::IDENTITY)
                .to_cols_arrays();
            ctx.queue
                .write_buffer(&self.shadow_frame_buffers[i], 0, bytemuck::bytes_of(&one));
        }
        // 投影聚光灯自己那一趟：复用 `shadow_matrix` 这个槽位，写进独立的 buffer
        // （否则会和级联的最后一写互相覆盖）
        if let Some(p) = spot_shadow {
            let mut one = uniforms;
            one.shadow_matrix = p.camera.view_proj.to_cols_arrays();
            ctx.queue
                .write_buffer(&self.spot_shadow_frame_buffer, 0, bytemuck::bytes_of(&one));
        }
        // 点光源的 6 个面：一面一个 buffer（同一 buffer 的多次写入只会在提交时生效）
        if let Some(p) = point_shadow {
            for face in 0..POINT_SHADOW_FACES {
                let mut one = uniforms;
                one.shadow_matrix = p.camera.views[face].to_cols_arrays();
                ctx.queue.write_buffer(
                    &self.point_shadow_frame_buffers[face],
                    0,
                    bytemuck::bytes_of(&one),
                );
            }
        }

        // 后处理参数（bloom 开关/强度/阈值 + bloom 贴图尺寸 + FXAA 阈值）
        let bloom = self.options.bloom;
        let bloom_on = bloom.enabled;
        let fxaa = self.options.fxaa;
        let fxaa_on = fxaa.enabled;
        let ssao = self.options.ssao;
        let ssao_on = self.ssao_active();
        // 半球采样点要投回屏幕（proj）、深度要反投影成视空间位置（inv_proj）
        let proj = camera.projection_matrix();
        let inv_proj = proj.inverse();
        let post = PostUniforms {
            params: [
                if bloom_on { 1.0 } else { 0.0 },
                bloom.intensity.max(0.0),
                bloom.threshold.max(0.0),
                0.0,
            ],
            bloom_texel: [
                1.0 / self.bloom_size.0.max(1) as f32,
                1.0 / self.bloom_size.1.max(1) as f32,
                self.bloom_size.0.max(1) as f32,
                self.bloom_size.1.max(1) as f32,
            ],
            target_texel: [
                1.0 / self.target_size.0.max(1) as f32,
                1.0 / self.target_size.1.max(1) as f32,
                self.target_size.0.max(1) as f32,
                self.target_size.1.max(1) as f32,
            ],
            fxaa: [
                if fxaa_on { 1.0 } else { 0.0 },
                fxaa.edge_threshold.max(0.0),
                fxaa.edge_threshold_min.max(0.0),
                0.0,
            ],
            ssao: [
                if ssao_on { 1.0 } else { 0.0 },
                ssao.radius.max(1e-3),
                ssao.bias.max(0.0),
                ssao.strength.clamp(0.0, 1.0),
            ],
            ssao2: [ssao.samples.clamp(1, 64) as f32, 0.0, 0.0, 0.0],
            proj: [
                proj.cols[0].to_array(),
                proj.cols[1].to_array(),
                proj.cols[2].to_array(),
                proj.cols[3].to_array(),
            ],
            inv_proj: [
                inv_proj.cols[0].to_array(),
                inv_proj.cols[1].to_array(),
                inv_proj.cols[2].to_array(),
                inv_proj.cols[3].to_array(),
            ],
            water: [
                if self.options.water.enabled { 1.0 } else { 0.0 },
                self.time,
                self.options.water.strength.max(0.0),
                self.options.water.foam.max(0.0),
            ],
            water_color: [
                self.options.water.color[0],
                self.options.water.color[1],
                self.options.water.color[2],
                self.options.water.clarity.clamp(0.0, 1.0),
            ],
            inv_view: {
                let v = camera.view().inverse();
                [
                    v.cols[0].to_array(),
                    v.cols[1].to_array(),
                    v.cols[2].to_array(),
                    v.cols[3].to_array(),
                ]
            },
            water_extra: [
                self.options.water.foam.max(0.0),
                self.options.water.refraction.max(0.0),
                1.0,
                0.0,
            ],
            water_sun: {
                let (d, i) = lighting
                    .sun
                    .map(|s| (s.direction.normalize_or_zero(), s.intensity))
                    .unwrap_or((Vec3::new(0.42, -0.78, 0.46), 0.0));
                [d.x, d.y, d.z, i]
            },
        };
        ctx.queue
            .write_buffer(&self.post_uniform, 0, bytemuck::bytes_of(&post));

        // ---------- 可见性与剔除 ----------
        let frustum = camera.frustum();
        self.visible.clear();
        self.visible_ranges.clear();
        self.visible_ranges.resize(self.batches.len(), (0, 0));
        self.draw_order.clear();
        let want_shadow = uniforms.light_color[3] > 0.5;
        // 每一级一个视锥：物体只要落在**任意一级**里就得画进阴影图，
        // 否则"远处的物体只出现在第 3 级里"这种情形会缺影子。
        // 聚光灯的锥也是同一份名单里的一员：它的遮挡物同样要进 `shadow_batches`。
        let mut shadow_frustums: Vec<Frustum> = cascades
            .iter()
            .take(shadow_count.max(1))
            .map(Frustum::from_view_projection)
            .collect();
        if let Some(p) = spot_shadow {
            shadow_frustums.push(Frustum::from_view_projection(&p.camera.view_proj));
        }
        // 点光源照亮的是一个**球**，没有单一视锥可做剔除：用"球与包围盒相交"测一遍，
        // 否则灯侧面/背后的遮挡物会漏进不了 `shadow_batches`（影子缺一块）。
        let point_shadow_sphere = point_shadow.map(|p| {
            let l = lighting.point_lights[p.light_index];
            (l.position, l.radius.max(1e-3))
        });

        let mut write = 0u32;
        let mut shadow_batches: Vec<(MeshId, u32, u32)> = Vec::new();
        // 透明物体不能按 (材质, 网格) 合批后随便画：混合结果依赖**全局**远→近顺序。
        // 这里为每个可见透明实例记一条 draw（引用打包后的 instance_index），
        // 排序后逐实例画；不透明仍走原来的合批路径。
        let mut alpha_draws: Vec<(f32, MaterialId, MeshId, u32)> = Vec::new();
        let view = camera.view();
        for (i, batch) in self.batches.iter().enumerate() {
            let start = batch.first as usize;
            let end = start + batch.count as usize;
            let batch_first = write;
            let mut batch_count = 0u32;
            let mut shadow_any = false;
            let desc = self.materials.get(batch.material as usize).map(|m| &m.desc);
            let alpha = desc.map(|d| d.needs_alpha_blend()).unwrap_or(false);
            let double = desc.map(|d| d.double_sided).unwrap_or(false);
            let unlit = desc.map(|d| d.unlit).unwrap_or(false);
            for (raw, bounds) in &self.staging[start..end] {
                if !frustum.intersects_aabb(bounds) {
                    stats.culled += 1;
                    continue;
                }
                let instance_index = write;
                self.visible.push(*raw);
                write += 1;
                batch_count += 1;
                if alpha {
                    // 视空间 z 取负 = 到相机的正距离；由远到近排序。
                    let eye = view.transform_point3(bounds.center());
                    alpha_draws.push((-eye.z, batch.material, batch.mesh, instance_index));
                }
                if shadow_frustums.iter().any(|f| f.intersects_aabb(bounds))
                    || point_shadow_sphere.is_some_and(|(c, r)| {
                        crate::point_shadow::aabb_intersects_sphere(bounds, c, r)
                    })
                {
                    shadow_any = true;
                }
            }
            if batch_count == 0 {
                continue;
            }
            self.visible_ranges[i] = (batch_first, batch_count);
            if !alpha {
                self.draw_order
                    .push((0, u8::from(double), batch.material, batch.mesh, i));
            }
            if shadow_any && !alpha && !unlit {
                shadow_batches.push((batch.mesh, batch_first, batch_count));
                stats.shadow_instances += batch_count;
            }
        }
        stats.instances = write;
        self.draw_order.sort_unstable();
        // 距离相等时按材质/网格/实例下标稳定收尾，避免同一帧两次运行顺序漂移。
        alpha_draws.sort_by(|a, b| {
            b.0.total_cmp(&a.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.cmp(&b.2))
                .then_with(|| a.3.cmp(&b.3))
        });

        if !self.visible.is_empty() {
            ctx.queue.write_buffer(
                &self.instance_buffer,
                0,
                bytemuck::cast_slice(&self.visible),
            );
        }
        if !self.lines.vertices.is_empty() {
            ctx.queue.write_buffer(
                &self.line_buffer,
                0,
                bytemuck::cast_slice(&self.lines.vertices),
            );
        }

        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("quench3d-encoder"),
            });

        // ---------- 阴影 pass（级联：每一级画进图集 2x2 的一个分块） ----------
        // 级联画在**同一个编码器**里：一级一个 pass、一级一个分块视口。
        // 第一级清整图，后面几级用 Load——否则后一级的 Clear 会把前一级擦掉。
        if want_shadow && !shadow_batches.is_empty() {
            let count = shadow_count.max(1);
            let full = (self.shadow_tile_size * 2) as f32;
            for ci in 0..count {
                let (vx, vy, vs) = if count <= 1 {
                    (0.0, 0.0, full)
                } else {
                    let col = (ci % 2) as f32;
                    let row = (ci / 2) as f32;
                    let t = self.shadow_tile_size as f32;
                    (col * t, row * t, t)
                };
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("quench3d-shadow-pass"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &self.shadow_view,
                        depth_ops: Some(wgpu::Operations {
                            load: if ci == 0 {
                                wgpu::LoadOp::Clear(1.0)
                            } else {
                                wgpu::LoadOp::Load
                            },
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.shadow_pipeline);
                pass.set_bind_group(0, &self.shadow_frame_bind_groups[ci], &[]);
                pass.set_viewport(vx, vy, vs, vs, 0.0, 1.0);
                pass.set_scissor_rect(vx as u32, vy as u32, vs as u32, vs as u32);
                let mut current_mesh = u32::MAX;
                for (mesh, first, cnt) in &shadow_batches {
                    if *mesh != current_mesh {
                        let m = &self.meshes[*mesh as usize];
                        pass.set_vertex_buffer(0, m.vertex_buffer.slice(..));
                        pass.set_index_buffer(m.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                        current_mesh = *mesh;
                    }
                    pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
                    let m = &self.meshes[*mesh as usize];
                    pass.draw_indexed(0..m.index_count, 0, *first..(*first + *cnt));
                    stats.shadow_draw_calls += 1;
                }
                drop(pass);
            }
        }

        // ---------- 聚光灯阴影 pass（第二张 2D 深度图，同一套管线/同一份 PCF） ----------
        // 与级联共用 `shadow_pipeline`（只写深度、正面剔除、深度偏移），
        // 只是把 `frame.shadow_matrix` 换成聚光灯那台的透视矩阵、把目标换成另一张图。
        // 名单用的是同一份 `shadow_batches`（太阳视锥 ∪ 聚光锥的并集）：
        // 多画几个在锥外的物体不会改变这张图的内容，换来的是一次剔除循环。
        if spot_shadow.is_some() && !shadow_batches.is_empty() {
            let size = self.spot_shadow_map_size as f32;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("quench3d-spot-shadow-pass"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.spot_shadow_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.shadow_pipeline);
            pass.set_bind_group(0, &self.spot_shadow_frame_bind_group, &[]);
            pass.set_viewport(0.0, 0.0, size, size, 0.0, 1.0);
            pass.set_scissor_rect(0, 0, self.spot_shadow_map_size, self.spot_shadow_map_size);
            let mut current_mesh = u32::MAX;
            for (mesh, first, cnt) in &shadow_batches {
                if *mesh != current_mesh {
                    let m = &self.meshes[*mesh as usize];
                    pass.set_vertex_buffer(0, m.vertex_buffer.slice(..));
                    pass.set_index_buffer(m.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                    current_mesh = *mesh;
                }
                pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
                let m = &self.meshes[*mesh as usize];
                pass.draw_indexed(0..m.index_count, 0, *first..(*first + *cnt));
                stats.spot_shadow_draw_calls += 1;
            }
        }

        // ---------- 点光源阴影 pass（6 个 90° 面，同一套管线） ----------
        // 每面一个层视图、一个帧常量；面序与 `point_shadow.rs::cube_face_uv` 一致。
        // 名单还是同一份 `shadow_batches`（多画几个在球外的物体不会改变图的内容）。
        if point_shadow.is_some() && !shadow_batches.is_empty() {
            let size = self.point_shadow_map_size as f32;
            for face in 0..POINT_SHADOW_FACES {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("quench3d-point-shadow-pass"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &self.point_shadow_face_views[face],
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.shadow_pipeline);
                pass.set_bind_group(0, &self.point_shadow_frame_bind_groups[face], &[]);
                pass.set_viewport(0.0, 0.0, size, size, 0.0, 1.0);
                pass.set_scissor_rect(0, 0, self.point_shadow_map_size, self.point_shadow_map_size);
                let mut current_mesh = u32::MAX;
                for (mesh, first, cnt) in &shadow_batches {
                    if *mesh != current_mesh {
                        let m = &self.meshes[*mesh as usize];
                        pass.set_vertex_buffer(0, m.vertex_buffer.slice(..));
                        pass.set_index_buffer(m.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                        current_mesh = *mesh;
                    }
                    pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
                    let m = &self.meshes[*mesh as usize];
                    pass.draw_indexed(0..m.index_count, 0, *first..(*first + *cnt));
                    stats.point_shadow_draw_calls += 1;
                }
            }
        }

        // ---------- 主 pass（渲染到线性 HDR 中间目标） ----------
        let hdr_view = self.hdr_view.clone().expect("HDR 目标缺失");
        let mask_view = self.water_mask_view.clone().expect("水面 mask 目标缺失");
        {
            let (color_view, resolve_target) = match &self.msaa_view {
                // MSAA 解析到 HDR 目标（不是最终目标：tonemap 还没做）
                Some(msaa) => (msaa, Some(&hdr_view)),
                None => (&hdr_view, None),
            };
            let (mask_color, mask_resolve) = match &self.water_mask_msaa_view {
                Some(msaa) => (msaa, Some(&mask_view)),
                None => (&mask_view, None),
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("quench3d-main-pass"),
                color_attachments: &[
                    Some(wgpu::RenderPassColorAttachment {
                        view: color_view,
                        depth_slice: None,
                        resolve_target,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.02,
                                g: 0.03,
                                b: 0.05,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                    Some(wgpu::RenderPassColorAttachment {
                        view: mask_color,
                        depth_slice: None,
                        resolve_target: mask_resolve,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    }),
                ],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: self.depth_view.as_ref().expect("深度视图缺失"),
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });

            // 天空
            if self.options.sky && lighting.sky.enabled {
                pass.set_pipeline(&self.sky_pipeline);
                pass.set_bind_group(0, &self.frame_bind_group, &[]);
                pass.draw(0..3, 0..1);
            }

            // 不透明与透明物体（透明永远排在不透明之后）
            for &(alpha_flag, double_flag, material, mesh, batch_index) in &self.draw_order {
                let (first, count) = self.visible_ranges[batch_index];
                if count == 0 {
                    continue;
                }
                let (Some(gm), Some(gmat)) = (
                    self.meshes.get(mesh as usize),
                    self.materials.get(material as usize),
                ) else {
                    continue;
                };
                // 半透明不在这里画：它们按全局远→近顺序逐实例画在下面。
                debug_assert_eq!(alpha_flag, 0, "draw_order 里不应再有透明批次");
                // 双面材质不能剔除背面；着色器已按 front_facing 翻转法线，
                // 所以这里只需要换一条不剔除的管线。
                let pipeline = if double_flag == 1 {
                    &self.opaque_double_pipeline
                } else {
                    &self.opaque_pipeline
                };
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, &self.frame_bind_group, &[]);
                pass.set_bind_group(1, &self.shadow_bind_group, &[]);
                pass.set_bind_group(2, &gmat.bind_group, &[]);
                pass.set_vertex_buffer(0, gm.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
                pass.set_index_buffer(gm.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..gm.index_count, 0, first..(first + count));
                stats.draw_calls += 1;
                stats.triangles += (gm.index_count as u64 / 3) * count as u64;
            }

            // 透明物体：全局由远到近，逐实例画。不能合批——一旦把两个透明批次
            // 分别 instancing，批次之间的前后关系就丢了（玻璃叠玻璃会闪）。
            for &(_, material, mesh, instance_index) in &alpha_draws {
                let (Some(gm), Some(gmat)) = (
                    self.meshes.get(mesh as usize),
                    self.materials.get(material as usize),
                ) else {
                    continue;
                };
                pass.set_pipeline(&self.alpha_pipeline);
                pass.set_bind_group(0, &self.frame_bind_group, &[]);
                pass.set_bind_group(1, &self.shadow_bind_group, &[]);
                pass.set_bind_group(2, &gmat.bind_group, &[]);
                pass.set_vertex_buffer(0, gm.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
                pass.set_index_buffer(gm.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..gm.index_count, 0, instance_index..(instance_index + 1));
                stats.draw_calls += 1;
                stats.triangles += gm.index_count as u64 / 3;
            }

            // 无限网格
            if self.options.grid && lighting.sky.show_grid {
                pass.set_pipeline(&self.grid_pipeline);
                pass.set_bind_group(0, &self.frame_bind_group, &[]);
                pass.draw(0..3, 0..1);
            }

            // 调试线段
            if self.lines.enabled && !self.lines.vertices.is_empty() {
                pass.set_pipeline(&self.line_pipeline);
                pass.set_bind_group(0, &self.frame_bind_group, &[]);
                pass.set_vertex_buffer(0, self.line_buffer.slice(..));
                pass.draw(0..self.lines.vertices.len() as u32, 0..1);
                stats.lines = self.lines.line_count() as u32;
            }
        }

        // ---------- 深度解析（可选：开了 SSAO 且 MSAA > 1） ----------
        // 把多采样深度的第 0 号采样搬到单采样深度贴图，供后处理按像素读取。
        // 三个都要有：绑定组/目标视图（跟着 `ssao_on` 建）与管线（GL 上是 `None`）。
        if ssao_on {
            if let (Some(pipeline), Some(bind_group), Some(resolved)) = (
                &self.depth_resolve_pipeline,
                &self.depth_resolve_bind_group,
                &self.resolved_depth_view,
            ) {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("quench3d-depth-resolve-pass"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: resolved,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw(0..3, 0..1);
                stats.post_passes += 1;
            }
        }

        // ---------- Bloom（可选，三步都写半分辨率 HDR 贴图） ----------
        if bloom_on {
            let steps: [(
                &str,
                &wgpu::RenderPipeline,
                &wgpu::BindGroup,
                &wgpu::TextureView,
            ); 3] = [
                (
                    "quench3d-bloom-bright-pass",
                    &self.bloom_bright_pipeline,
                    self.bloom_bright_bind_group
                        .as_ref()
                        .expect("bloom 绑定组缺失"),
                    self.bloom_a_view.as_ref().expect("bloom 贴图缺失"),
                ),
                (
                    "quench3d-bloom-blur-h-pass",
                    &self.bloom_blur_h_pipeline,
                    self.bloom_blur_h_bind_group
                        .as_ref()
                        .expect("bloom 绑定组缺失"),
                    self.bloom_b_view.as_ref().expect("bloom 贴图缺失"),
                ),
                (
                    "quench3d-bloom-blur-v-pass",
                    &self.bloom_blur_v_pipeline,
                    self.bloom_blur_v_bind_group
                        .as_ref()
                        .expect("bloom 绑定组缺失"),
                    self.bloom_a_view.as_ref().expect("bloom 贴图缺失"),
                ),
            ];
            for (label, pipeline, bind_group, dst) in steps {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some(label),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: dst,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw(0..3, 0..1);
                stats.post_passes += 1;
            }
        }

        // ---------- 后处理 pass：tonemap（+ bloom 合成）→ 最终目标 ----------
        // 开了 FXAA 时先写 LDR 中间目标，再由下面那一趟做抗锯齿；否则直接写目标
        // （**关掉 FXAA 的像素与加这个功能之前逐位相同**：那一趟根本不跑）。
        let post_target = if fxaa_on {
            self.ldr_view.clone().expect("LDR 中间目标缺失")
        } else {
            target.clone()
        };
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("quench3d-post-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &post_target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(if fxaa_on {
                &self.post_ldr_pipeline
            } else {
                &self.post_pipeline
            });
            pass.set_bind_group(
                0,
                self.post_bind_group.as_ref().expect("后处理绑定组缺失"),
                &[],
            );
            pass.draw(0..3, 0..1);
            stats.post_passes += 1;
        }

        // ---------- FXAA：LDR 中间目标 → 目标（可选） ----------
        if fxaa_on {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("quench3d-fxaa-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.fxaa_pipeline);
            pass.set_bind_group(
                0,
                self.fxaa_bind_group.as_ref().expect("FXAA 绑定组缺失"),
                &[],
            );
            pass.draw(0..3, 0..1);
            stats.post_passes += 1;
        }

        ctx.queue.submit(Some(encoder.finish()));
        self.stats = stats;
        stats
    }

    /// 最近一帧的统计。
    pub fn stats(&self) -> RenderStats {
        self.stats
    }

    /// 获取交换链当前帧并渲染。
    pub fn draw_frame(
        &mut self,
        ctx: &mut GpuContext,
        camera: &Camera,
        lighting: &Lighting,
    ) -> Result<RenderStats, FrameError> {
        let frame = ctx.acquire_frame()?;
        if ctx.take_reconfigure_flag() {
            // 上一帧报告配置次优：趁这一帧已经拿到纹理，重新配置供下一帧使用
            ctx.reconfigure();
        }
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let size = (frame.texture.width(), frame.texture.height());
        let stats = self.render(ctx, &view, size, camera, lighting);
        ctx.queue.present(frame);
        Ok(stats)
    }

    /// 离屏渲染并回读为图像（无头截图 / 自动化测试）。
    pub fn capture(
        &mut self,
        ctx: &mut GpuContext,
        width: u32,
        height: u32,
        camera: &Camera,
        lighting: &Lighting,
    ) -> Result<ImageData, String> {
        // 交换链格式若为 BGRA，直接复用会产生通道错位，这里统一用 RGBA 目标。
        let target = ctx.create_offscreen(width, height);
        let mut camera = *camera;
        camera.set_aspect(width as f32 / height.max(1) as f32);
        self.render(ctx, &target.view, (width, height), &camera, lighting);

        let bytes_per_pixel = 4u32;
        let unpadded = width * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = unpadded.div_ceil(align) * align;

        let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quench3d-readback"),
            size: (padded * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("quench3d-readback-encoder"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        ctx.queue.submit(Some(encoder.finish()));

        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        ctx.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("等待 GPU 失败: {e:?}"))?;
        rx.recv()
            .map_err(|e| format!("回读通道关闭: {e}"))?
            .map_err(|e| format!("映射缓冲失败: {e:?}"))?;

        let data = slice
            .get_mapped_range()
            .map_err(|e| format!("读取映射缓冲失败: {e:?}"))?;
        let mut rgba = Vec::with_capacity((unpadded * height) as usize);
        for row in 0..height {
            let start = (row * padded) as usize;
            rgba.extend_from_slice(&data[start..start + unpadded as usize]);
        }
        drop(data);
        buffer.unmap();

        // 若 GPU 格式为 BGRA，需要交换 R/B 通道。
        if ctx.format == wgpu::TextureFormat::Bgra8UnormSrgb
            || ctx.format == wgpu::TextureFormat::Bgra8Unorm
        {
            for px in rgba.as_chunks_mut::<4>().0 {
                px.swap(0, 2);
            }
        }

        ImageData::from_rgba8(width, height, rgba).map_err(|e| e.to_string())
    }

    /// 回读平行光阴影贴图的深度值（调试用）：返回 `(宽, 高, 深度)`，1.0 表示没有遮挡物。
    pub fn read_shadow_map(&self, ctx: &GpuContext) -> Result<(u32, u32, Vec<f32>), String> {
        self.read_depth_texture(ctx, &self.shadow_texture)
    }

    /// 回读**聚光灯**阴影贴图的深度值（调试用，语义同 [`Self::read_shadow_map`]）。
    ///
    /// 两张图是分开的：聚光那张是透视投影，直接在平行光那张上做"有没有影子"的判断
    /// 会把两张图的坐标混起来。
    pub fn read_spot_shadow_map(&self, ctx: &GpuContext) -> Result<(u32, u32, Vec<f32>), String> {
        self.read_depth_texture(ctx, &self.spot_shadow_texture)
    }

    /// 回读**点光源**阴影图某一面的深度值（调试用，语义同 [`Self::read_shadow_map`]）。
    ///
    /// `face` 是 `+X, -X, +Y, -Y, +Z, -Z` 里的一个（越界返回 `Err`，不 panic）。
    pub fn read_point_shadow_map(
        &self,
        ctx: &GpuContext,
        face: usize,
    ) -> Result<(u32, u32, Vec<f32>), String> {
        if face >= POINT_SHADOW_FACES {
            return Err(format!(
                "面下标 {face} 越界（只有 {POINT_SHADOW_FACES} 个面）"
            ));
        }
        self.read_depth_texture_layer(ctx, &self.point_shadow_texture, face)
    }

    /// 回读一张 `Depth32Float` 纹理（调试用）。
    fn read_depth_texture(
        &self,
        ctx: &GpuContext,
        texture: &wgpu::Texture,
    ) -> Result<(u32, u32, Vec<f32>), String> {
        self.read_depth_texture_layer(ctx, texture, 0)
    }

    /// 与 [`Self::read_depth_texture`] 相同，但只回读数组纹理的**第 `layer` 层**。
    ///
    /// 点光源阴影是 6 层数组，逐面回读才能回答"哪一面的内容不对"
    /// （把 6 面混在一起读只会得到"有一面是错的"）。
    fn read_depth_texture_layer(
        &self,
        ctx: &GpuContext,
        texture: &wgpu::Texture,
        layer: usize,
    ) -> Result<(u32, u32, Vec<f32>), String> {
        let width = texture.width();
        let height = texture.height();
        let unpadded = width * 4;
        let padded = unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quench3d-shadow-readback"),
            size: (padded * height) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("quench3d-shadow-readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer as u32,
                },
                aspect: wgpu::TextureAspect::DepthOnly,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        ctx.queue.submit(Some(encoder.finish()));
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        ctx.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("等待 GPU 失败: {e:?}"))?;
        rx.recv()
            .map_err(|e| format!("回读通道关闭: {e}"))?
            .map_err(|e| format!("映射缓冲失败: {e:?}"))?;
        let data = slice
            .get_mapped_range()
            .map_err(|e| format!("读取映射缓冲失败: {e:?}"))?;
        let mut depths = Vec::with_capacity((width * height) as usize);
        for row in 0..height {
            let start = (row * padded) as usize;
            for i in 0..width as usize {
                let b = &data[start + i * 4..start + i * 4 + 4];
                depths.push(f32::from_le_bytes([b[0], b[1], b[2], b[3]]));
            }
        }
        drop(data);
        buffer.unmap();
        Ok((width, height, depths))
    }

    /// 每帧时间（供着色器动画）。
    pub fn set_time(&mut self, seconds: f32) {
        self.time = seconds;
    }

    /// 视锥剔除测试（供上层做 LOD/可见性查询）。
    pub fn is_visible(&self, camera: &Camera, bounds: &Aabb) -> bool {
        camera.frustum().intersects_aabb(bounds)
    }

    /// 暴露给高级用途：帧绑定组。
    pub fn frame_bind_group(&self) -> &wgpu::BindGroup {
        &self.frame_bind_group
    }

    /// 暴露给高级用途：帧 uniform 布局。
    pub fn frame_layout(&self) -> &wgpu::BindGroupLayout {
        &self.frame_layout
    }

    /// 暴露给高级用途：阴影贴图绑定组布局。
    pub fn shadow_layout(&self) -> &wgpu::BindGroupLayout {
        &self.shadow_bind_layout
    }

    /// 暴露给高级用途：材质绑定组布局。
    pub fn material_layout(&self) -> &wgpu::BindGroupLayout {
        &self.material_layout
    }

    /// 阴影贴图视图（可视化调试用）。
    pub fn shadow_view(&self) -> &wgpu::TextureView {
        &self.shadow_view
    }

    /// 阴影贴图纹理。
    pub fn shadow_texture(&self) -> &wgpu::Texture {
        &self.shadow_texture
    }

    /// 阴影采样器。
    pub fn shadow_sampler(&self) -> &wgpu::Sampler {
        &self.shadow_sampler
    }

    /// 聚光灯阴影贴图视图（可视化调试用；与平行光那张是两张不同的图）。
    pub fn spot_shadow_view(&self) -> &wgpu::TextureView {
        &self.spot_shadow_view
    }

    /// 聚光灯阴影贴图纹理（回读/调试用）。
    pub fn spot_shadow_texture(&self) -> &wgpu::Texture {
        &self.spot_shadow_texture
    }

    /// 聚光灯阴影贴图的边长（像素）。
    pub fn spot_shadow_map_size(&self) -> u32 {
        self.spot_shadow_map_size
    }

    /// 点光源阴影图**一个面**的边长（像素）。
    pub fn point_shadow_map_size(&self) -> u32 {
        self.point_shadow_map_size
    }

    /// 点光源阴影纹理（回读/调试用；6 层数组）。
    pub fn point_shadow_texture(&self) -> &wgpu::Texture {
        &self.point_shadow_texture
    }

    /// 点光源阴影的**整数组**视图（调试/可视化用；着色器绑定的就是它）。
    pub fn point_shadow_array_view(&self) -> &wgpu::TextureView {
        &self.point_shadow_array_view
    }

    /// 点光源阴影第 `face` 面的渲染视图（可视化调试用；面序见 `point_shadow.rs`）。
    pub fn point_shadow_face_view(&self, face: usize) -> Option<&wgpu::TextureView> {
        self.point_shadow_face_views.get(face)
    }

    /// 默认白色纹理视图（自定义管线可复用）。
    pub fn white_view(&self) -> &wgpu::TextureView {
        &self.white_texture.view
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_raw_layout_is_tight() {
        assert_eq!(std::mem::size_of::<InstanceRaw>(), 96);
        assert_eq!(std::mem::size_of::<crate::mesh::Vertex>() % 4, 0);
    }

    /// `PostParams` 的 Rust/WGSL 布局必须一致：6 个 vec4 + 2 个 mat4 +
    /// water/water_color 2 个 vec4 + inv_view 1 个 mat4 + water_extra/water_sun
    /// 2 个 vec4 = 352 字节。
    #[test]
    fn post_uniforms_size_matches_wgsl_layout() {
        assert_eq!(std::mem::size_of::<PostUniforms>(), 352);
        assert_eq!(std::mem::size_of::<PostUniforms>() % 16, 0);
    }

    /// WGSL 源码级守卫：阴影 PCF 里不许用**带隐式梯度**的比较采样。
    ///
    /// 为什么是"扫文本"而不是编译一遍：D3D 的 FXC 只在 Windows 上跑，本地（Linux/Vulkan）
    /// 与 wasm 腿都不会报这个错。第 54 轮就是 `pcf_3x3` 里用了 `textureSampleCompare`，
    /// 本地门禁全绿、Windows 腿 12 个 e2e 测试全红（`X3570` → `D3DCompile 0x80004005`：
    /// 非一致控制流里不许有隐式梯度采样）。深度图只有 1 级 mip，
    /// `textureSampleCompareLevel` 的 LOD 0 与隐式 LOD 完全等价，所以这条禁令没有代价。
    /// 细节见 `docs/overnight_report.md` §47。
    #[test]
    fn wgsl_shadow_pcf_avoids_implicit_gradient_sampling() {
        // 匹配的是**带左括号**的 `textureSampleCompare(`：`textureSampleCompareLevel(`
        // 中间夹着 "Level"，不含这个子串，所以不会误伤。
        for (name, src) in [("pbr.wgsl", PBR_WGSL), ("shadow.wgsl", SHADOW_WGSL)] {
            assert!(
                !src.contains("textureSampleCompare("),
                "{name} 用了带隐式梯度的 textureSampleCompare（请换成 textureSampleCompareLevel：见 pcf_3x3 注释）"
            );
        }
        // 正向确认：Level 变体确实还在用。少了这条，"整个函数被删掉"也会让上面的断言通过。
        assert!(
            PBR_WGSL.contains("textureSampleCompareLevel("),
            "PCF 的 Level 比较采样不见了：要么阴影被删了，要么改成了别的写法"
        );
    }

    /// 用 naga 的 **GLSL 后端**把一段 WGSL 的每个入口点都翻一遍。
    ///
    /// 这就是 GL/WebGL2 后端在建管线时真正跑的那段代码（`wgpu-hal` 的 gles 后端把 WGSL
    /// 翻成 GLSL 再交给驱动编译），所以它**不需要显卡、不需要 GL 上下文**，却能在
    /// Linux/Vulkan 机器上（以及 CI）抓住"这条管线在 GL 上根本建不出来"。
    ///
    /// `Err` 里带上入口点名 —— 事故现场那句 `In Device::create_render_pipeline,
    /// label = 'quench3d-post'` 的等价物。
    // 平台门槛（四条守卫共用）：这些守卫要 `naga` 的 **GLSL 后端**（`naga/glsl-out`），
    // 而那个特性只跟着 wgpu 的 gles 后端一起被编进来。`wgpu-core/Cargo.toml` 里那份
    // target 表写得很清楚：`cfg(any(windows, target_os = "linux", target_os = "android",
    // target_os = "freebsd"))` → `wgpu-core-deps-windows-linux-android`（+ emscripten 那份），
    // 也就是**只有这些平台**有 GL 后端、才有 `naga/glsl-out`。macOS 上 wgpu 走 Metal、
    // 连 GL 后端都不编，这几条守卫在那里既编不出来也没有对象
    // （那里还有一条不依赖 naga 的文本级守卫兜底：`post_gl_variant_drops_every_depth_texture`）；
    // wasm32 没有 test harness。**别因为"换个平台就编不过"而删掉它们** ——
    // 删掉之后 GL/WebGL2 这一类事故就没有任何本地守卫了（第 57 轮的教训）。
    #[cfg(all(
        not(target_arch = "wasm32"),
        any(
            target_os = "linux",
            target_os = "windows",
            target_os = "android",
            target_os = "freebsd",
            target_os = "emscripten"
        )
    ))]
    fn translate_all_entries_to_glsl(
        src: &str,
        version: wgpu::naga::back::glsl::Version,
    ) -> Result<usize, String> {
        use wgpu::naga::{back::glsl, front::wgsl, proc::BoundsCheckPolicies, valid, ShaderStage};
        let module = wgsl::parse_str(src)
            .map_err(|e| format!("WGSL 解析失败：{}", e.emit_to_string(src)))?;
        let info =
            valid::Validator::new(valid::ValidationFlags::all(), valid::Capabilities::empty())
                .validate(&module)
                .map_err(|e| format!("WGSL 校验失败：{e:?}"))?;
        let entries: Vec<(ShaderStage, String)> = module
            .entry_points
            .iter()
            .map(|ep| (ep.stage, ep.name.clone()))
            .collect();
        // GLSL 的 `layout(binding = N)` 靠这张表：不给就会在写全局变量时报错。
        let mut bindings = glsl::BindingMap::default();
        let mut next = 0u8;
        for (_, global) in module.global_variables.iter() {
            if let Some(binding) = global.binding {
                bindings.insert(binding, next);
                next = next.saturating_add(1);
            }
        }
        let options = glsl::Options {
            version,
            writer_flags: glsl::WriterFlags::empty(),
            binding_map: bindings,
            zero_initialize_workgroup_memory: true,
        };
        for (stage, name) in &entries {
            let mut glsl = String::new();
            let pipeline = glsl::PipelineOptions {
                shader_stage: *stage,
                entry_point: name.clone(),
                multiview: None,
            };
            let mut writer = glsl::Writer::new(
                &mut glsl,
                &module,
                &info,
                &options,
                &pipeline,
                BoundsCheckPolicies::default(),
            )
            .map_err(|e| format!("入口点 {name}（{stage:?}）翻译失败：{e:?}"))?;
            writer
                .write()
                .map_err(|e| format!("入口点 {name}（{stage:?}）翻译失败：{e:?}"))?;
        }
        Ok(entries.len())
    }

    #[test]
    fn main_pass_targets_share_blend_for_webgl2() {
        // WebGL2 没有 INDEPENDENT_BLEND：两个颜色目标的 blend 必须同为 Some/None。
        let opaque = main_pass_color_targets(HDR_FORMAT, None);
        assert_eq!(opaque.len(), 2);
        assert!(opaque[0].as_ref().unwrap().blend.is_none());
        assert!(opaque[1].as_ref().unwrap().blend.is_none());
        let alpha = main_pass_color_targets(HDR_FORMAT, Some(wgpu::BlendState::ALPHA_BLENDING));
        assert!(alpha[0].as_ref().unwrap().blend.is_some());
        assert!(alpha[1].as_ref().unwrap().blend.is_some());
        assert_eq!(
            alpha[0].as_ref().unwrap().blend,
            alpha[1].as_ref().unwrap().blend
        );
    }

    #[test]
    fn point_shadow_texture_avoids_the_gl_cube_heuristic() {
        assert_eq!(POINT_SHADOW_TEXTURE_LAYERS, 7);
        assert_ne!(POINT_SHADOW_TEXTURE_LAYERS % 6, 0);
    }

    /// **在 GL/WebGL2 上建的每一条管线，其 WGSL 都必须能被 GLSL 后端翻译出来。**
    ///
    /// 这是用户实测到的线上事故（`quench3d-post` 的 fragment 对深度纹理用 `textureLoad` →
    /// naga 拒绝 → wasm 上 wgpu 把管线校验错误当致命 → **编辑器启动即 panic**）的直接守卫。
    /// 之前为什么没人抓到：本地跑的是 Vulkan，naga 的**只有 GLSL 后端**有这条限制；
    /// Windows 腿也不跑 GL。所以这个"编译一遍"的守卫是唯一能在本地/CI 上挡住它的东西
    /// —— 它比 grep 硬：跑的就是建管线时的那段代码。
    // 平台门槛同 `translate_all_entries_to_glsl`（要 `naga/glsl-out`）。
    #[cfg(all(
        not(target_arch = "wasm32"),
        any(
            target_os = "linux",
            target_os = "windows",
            target_os = "android",
            target_os = "freebsd",
            target_os = "emscripten"
        )
    ))]
    #[test]
    fn gl_pipelines_all_translate_through_the_glsl_backend() {
        use wgpu::naga::back::glsl::Version;
        // 浏览器 WebGL2（GLSL ES 3.00）与桌面 GL（3.30 core）都过一遍。
        let versions = [
            (
                "webgl2",
                Version::Embedded {
                    version: 300,
                    is_webgl: true,
                },
            ),
            ("gl330", Version::Desktop(330)),
        ];
        // `post` 用的是**给 GL 的那个变体**（删掉深度区段），不是 `POST_WGSL` 原文。
        let post_gl = post_wgsl_for(false);
        let pbr = expand(PBR_WGSL);
        let shadow = expand(SHADOW_WGSL);
        let sky = expand(SKY_WGSL);
        let grid = expand(GRID_WGSL);
        let line = expand(LINE_WGSL);
        let sources: [(&str, &str); 8] = [
            ("post.wgsl(gl 变体)", post_gl.as_ref()),
            ("pbr.wgsl", pbr.as_str()),
            ("shadow.wgsl", shadow.as_str()),
            ("sky.wgsl", sky.as_str()),
            ("grid.wgsl", grid.as_str()),
            ("line.wgsl", line.as_str()),
            ("bloom.wgsl", BLOOM_WGSL),
            ("fxaa.wgsl", FXAA_WGSL),
        ];
        for (version_name, version) in versions {
            for (name, src) in sources {
                let entries = translate_all_entries_to_glsl(src, version)
                    .unwrap_or_else(|e| panic!("{name} 在 {version_name} 上建不出管线：{e}"));
                assert!(entries > 0, "{name} 里一个入口点都没有？");
            }
        }
    }

    /// 反向确认：**原文的 `post.wgsl`（带深度 `textureLoad`）在 GLSL 后端上必然被拒**。
    ///
    /// 这条是"上面那个变体为什么必须存在"的证据，也是"这个 bug 真的存在"的复现：
    /// 事故原文就是这句 `WGSL \`textureLoad\` from depth textures is not supported in GLSL`
    /// （naga `back/glsl/writer.rs`）。哪天 naga 支持了，这里会红 —— 那时可以删掉整个变体机制。
    // 平台门槛同 `translate_all_entries_to_glsl`（要 `naga/glsl-out`）。
    #[cfg(all(
        not(target_arch = "wasm32"),
        any(
            target_os = "linux",
            target_os = "windows",
            target_os = "android",
            target_os = "freebsd",
            target_os = "emscripten"
        )
    ))]
    #[test]
    fn full_post_wgsl_is_rejected_by_the_glsl_backend() {
        let err = translate_all_entries_to_glsl(
            POST_WGSL,
            wgpu::naga::back::glsl::Version::Embedded {
                version: 300,
                is_webgl: true,
            },
        )
        .expect_err("原文的 post.wgsl 居然能翻成 GLSL？那这个变体机制可以删了");
        assert!(
            err.contains("textureLoad") && err.contains("depth"),
            "报错不是预期的那条（深度纹理 textureLoad）：{err}"
        );
    }

    /// `depth_resolve.wgsl`（MSAA 深度 → 单采样深度）同样翻不出来 —— 所以 GL 上不建这条管线。
    ///
    /// 它读的是 `texture_depth_multisampled_2d`，**连"改成采样"这条退路都没有**
    /// （多采样纹理不能采样）。GL 上它的用途（SSAO）已经整体关掉，所以直接不建。
    // 平台门槛同 `translate_all_entries_to_glsl`（要 `naga/glsl-out`）。
    #[cfg(all(
        not(target_arch = "wasm32"),
        any(
            target_os = "linux",
            target_os = "windows",
            target_os = "android",
            target_os = "freebsd",
            target_os = "emscripten"
        )
    ))]
    #[test]
    fn depth_resolve_wgsl_is_rejected_by_the_glsl_backend() {
        assert!(
            translate_all_entries_to_glsl(
                DEPTH_RESOLVE_WGSL,
                wgpu::naga::back::glsl::Version::Embedded {
                    version: 300,
                    is_webgl: true,
                },
            )
            .is_err(),
            "depth_resolve.wgsl 能翻成 GLSL 了：请重新评估 GL 上能否恢复 SSAO"
        );
    }

    /// 水面 mask 不依赖深度，GL 变体里也必须保留（否则 WebGL2 上水会消失）。
    #[test]
    fn post_keeps_water_mask_in_both_variants() {
        for src in [post_wgsl_for(false), post_wgsl_for(true)] {
            assert!(
                src.contains("@binding(5) var water_mask_texture"),
                "post 变体里水面 mask 绑定不见了"
            );
            assert!(
                src.contains("textureLoad(water_mask_texture"),
                "post 变体里水面 mask 没有被读取"
            );
            assert!(
                src.contains("textureLoad(water_mask_texture, coord, 0).g"),
                "post 变体没有用 mask.g 的水面高度做射线-平面重建"
            );
            assert!(src.contains("post.water_sun"), "post 变体没有太阳高光参数");
        }
    }

    #[test]
    fn water_options_are_disabled_by_default() {
        let w = WaterOptions::default();
        assert!(!w.enabled);
        assert_eq!(w.color, [0.06, 0.28, 0.48]);
        assert_eq!(w.strength, 1.0);
        assert_eq!(w.clarity, 1.0);
        assert_eq!(w.foam, 1.0);
        assert_eq!(w.refraction, 1.0);
    }

    /// GL 变体里不许再出现任何深度纹理（正向 + 反向都断言）。
    #[test]
    fn post_gl_variant_drops_every_depth_texture() {
        let gl = post_wgsl_for(false);
        for bad in [
            "texture_depth_2d",
            "texture_depth_multisampled_2d",
            "depth_texture",
        ] {
            assert!(
                !gl.contains(bad),
                "GL 变体里还有 `{bad}`：GLSL 后端会拒绝建管线"
            );
        }
        for keep in [
            "aces_tonemap(",
            "hdr_texture",
            "textureSampleLevel(bloom_texture",
            "water_mask_texture",
        ] {
            assert!(
                gl.contains(keep),
                "GL 变体里 `{keep}` 不见了：不该动的东西被删了"
            );
        }
        // 反向：原文里这些东西必须还在（否则上面的"删干净了"可能只是因为全空了）。
        assert!(POST_WGSL.contains("texture_depth_2d"));
        assert!(POST_WGSL.contains("ssao_factor("));
        // 开关打开时返回的必须是原文，一个字节都不改。
        assert_eq!(post_wgsl_for(true), POST_WGSL);
    }

    /// GL 上不改任何开关也建得出管线：`supports_depth_texture_read` 是唯一的判据。
    #[test]
    fn gl_backend_is_declared_unable_to_read_depth_textures() {
        assert!(!crate::context::supports_depth_texture_read(
            wgpu::Backend::Gl
        ));
        for ok in [
            wgpu::Backend::Vulkan,
            wgpu::Backend::Metal,
            wgpu::Backend::Dx12,
            wgpu::Backend::BrowserWebGpu,
        ] {
            assert!(
                crate::context::supports_depth_texture_read(ok),
                "{ok:?} 上深度纹理是按像素读的：关掉 SSAO 是功能回退，不是修复"
            );
        }
    }

    #[test]
    fn gl_backend_is_declared_unable_to_use_msaa() {
        // 依据是两处实测（不是猜）：① 真实 WebGL2（headless Chrome + SwiftShader）上
        // `glow` 的 `tex_storage_2d_multisample` 是 `panic!`；② 原生 GL（llvmpipe）上
        // MSAA > 1 出纯黑帧。两条路都指向"GL 上 sample_count 只能是 1"。
        assert!(!crate::context::supports_msaa(wgpu::Backend::Gl));
        assert_eq!(
            crate::context::effective_msaa_samples(wgpu::Backend::Gl, 4),
            1,
            "缺省 msaa = 4：GL 上必须夹到 1，否则编辑器在 WebGL2 上启动即 panic"
        );
        for bad in [0, 1, 2, 4, 8, 64] {
            assert_eq!(
                crate::context::effective_msaa_samples(wgpu::Backend::Gl, bad),
                1,
                "GL 上任何请求值都必须落在 1"
            );
        }
        for ok in [
            wgpu::Backend::Vulkan,
            wgpu::Backend::Metal,
            wgpu::Backend::Dx12,
            wgpu::Backend::BrowserWebGpu,
        ] {
            assert!(crate::context::supports_msaa(ok), "{ok:?} 上 MSAA 是好的");
            assert_eq!(
                crate::context::effective_msaa_samples(ok, 4),
                4,
                "{ok:?} 上不许动用户的 MSAA 设置"
            );
        }
        // 其它后端仍按老规矩夹在 1..=8
        assert_eq!(
            crate::context::effective_msaa_samples(wgpu::Backend::Vulkan, 0),
            1
        );
        assert_eq!(
            crate::context::effective_msaa_samples(wgpu::Backend::Vulkan, 64),
            8
        );
    }

    #[test]
    fn frame_uniform_layout_matches_wgsl() {
        use std::mem::{offset_of, size_of};
        // 偏移量按 `shaders/common.wgsl` 的 `Frame` 手算：mat4x4 = 64、vec4 = 16，
        // 全部 16 字节对齐。局部光是 `array<PointLight, 8>`，单盏 **64** 字节
        // （位置/半径、颜色/强度、方向/外锥余弦、内锥余弦）、交错排列。
        assert_eq!(
            size_of::<PointLightUniform>(),
            64,
            "PointLight 必须是四个 vec4（点光源与聚光灯共用同一条数组）"
        );
        assert_eq!(
            offset_of!(PointLightUniform, dir_outer),
            32,
            "方向与外锥余弦必须在第三段（WGSL 同顺序）"
        );
        assert_eq!(offset_of!(PointLightUniform, cone_inner), 48);
        assert_eq!(offset_of!(FrameUniforms, shadow_matrix), 192);
        // 级联字段紧跟在 shadow_matrix 之后（WGSL 里也是这个顺序）
        assert_eq!(offset_of!(FrameUniforms, view), 256);
        assert_eq!(offset_of!(FrameUniforms, shadow_matrices), 320);
        assert_eq!(offset_of!(FrameUniforms, cascade_splits), 576);
        assert_eq!(offset_of!(FrameUniforms, cascade_info), 592);
        assert_eq!(offset_of!(FrameUniforms, point_lights), 688);
        assert_eq!(offset_of!(FrameUniforms, params2), 1200);
        // 聚光阴影段紧跟在 params2 之后（WGSL 里也是这个顺序）：
        // 矩阵 64 字节 + 两个 vec4，全部 16 字节对齐
        assert_eq!(offset_of!(FrameUniforms, spot_shadow_matrix), 1216);
        assert_eq!(offset_of!(FrameUniforms, spot_shadow_params), 1280);
        assert_eq!(offset_of!(FrameUniforms, spot_shadow_texel), 1296);
        // 点光源阴影段紧跟在聚光阴影之后（WGSL 同顺序）：两个 vec4
        assert_eq!(offset_of!(FrameUniforms, point_shadow_params), 1312);
        assert_eq!(offset_of!(FrameUniforms, point_shadow_texel), 1328);
        assert_eq!(size_of::<FrameUniforms>(), 1344);
    }

    #[test]
    fn frame_uniforms_size_is_multiple_of_16() {
        let size = std::mem::size_of::<FrameUniforms>();
        assert_eq!(size % 16, 0, "uniform 结构大小必须是 16 的倍数: {size}");
        assert_eq!(size, 1344, "FrameUniforms 布局与 WGSL 不一致");
    }

    /// 新增的聚光阴影字段必须在 `mat4x4<f32>` 的 16 字节边界上，
    /// 否则 WGSL 那边的偏移会和 Rust 错开一个 vec4（画面只是"影子偏一点"，很难查）。
    #[test]
    fn frame_uniform_matrices_are_16_byte_aligned() {
        use std::mem::offset_of;
        for off in [
            offset_of!(FrameUniforms, view_proj),
            offset_of!(FrameUniforms, inv_view_proj),
            offset_of!(FrameUniforms, shadow_matrix),
            offset_of!(FrameUniforms, view),
            offset_of!(FrameUniforms, shadow_matrices),
            offset_of!(FrameUniforms, spot_shadow_matrix),
        ] {
            assert_eq!(off % 16, 0, "矩阵字段偏移 {off} 不是 16 的倍数");
        }
    }

    /// 打包：正下标 / 负下标哨兵 / 两个 vec4 的每个分量都对得上。
    #[test]
    fn point_shadow_uniforms_carry_the_plan_and_a_negative_sentinel() {
        let (p, t) = point_shadow_uniforms(None, 512);
        assert_eq!(p, [-1.0, 0.0, 0.0, 0.0], "没有投影灯必须写负下标");
        assert_eq!(t, [0.0, 0.0, 0.0, 0.0]);

        let light = crate::light::PointLight::new(Vec3::new(1.0, 4.0, -2.0), Vec3::ONE, 3.0, 20.0)
            .with_cast_shadows(true);
        let plan = plan_point_shadow(&[light], true).expect("应当有计划");
        let (p, t) = point_shadow_uniforms(Some(plan), 512);
        assert_eq!(p[0], 0.0, "下标就是 point_lights 里的位置");
        assert!(p[1] > 0.0 && p[1] < p[2], "near ∈ (0, far)：{p:?}");
        assert_eq!(p[2], 20.0, "far 就是影响半径");
        assert_eq!(p[3], crate::point_shadow::POINT_SHADOW_DEPTH_BIAS_TEXELS);
        assert_eq!(t[0], 1.0 / 512.0);
        assert!(
            (t[1] - crate::point_shadow::POINT_SHADOW_FACE_MARGIN).abs() < 1e-6,
            "tan(半面角) 必须就是那个 1% 余量：{}",
            t[1]
        );
        assert_eq!(t[2], crate::point_shadow::POINT_SHADOW_NORMAL_OFFSET_TEXELS);
        assert_eq!(t[3], crate::point_shadow::POINT_SHADOW_SLOPE_FACTOR);
        // map_size = 0 不许产生 inf（除零）
        let (_, t0) = point_shadow_uniforms(Some(plan), 0);
        assert!(t0[0].is_finite() && t0[0] > 0.0, "{t0:?}");
    }

    /// **漂移探测器**：`pbr.wgsl::cube_face_uv` 必须与 `point_shadow.rs::cube_face_uv`
    /// 是同一份公式（同六个分支、同顺序、同符号）。
    ///
    /// 为什么不能只靠"看着一样"：WGSL 只在**运行时**才被 naga 解析，而面内符号写反
    /// 是合法 WGSL——画面表现为"影子在某一面里镜像"，本地门禁抓不到（CI 也没有显卡）。
    /// 这里把六个分支的表达式逐个抠出来比对：任何一个符号/轴/面序改动都会红。
    #[test]
    fn wgsl_cube_face_uv_mirrors_the_cpu_convention() {
        let start = PBR_WGSL
            .find("fn cube_face_uv(")
            .expect("pbr.wgsl 里必须有 cube_face_uv");
        let body = &PBR_WGSL[start..];
        let end = body.find("\n}").expect("函数必须有结尾");
        // 去掉空白与换行：比对的是"公式"，不是排版
        let flat: String = body[..end].chars().filter(|c| !c.is_whitespace()).collect();
        // 六面（+X, -X, +Y, -Y, +Z, -Z）的 uv 公式，与 Rust 侧逐字对应
        let expected = [
            "0.5*(1.0+dir.z/ma/m),0.5*(1.0-dir.y/ma/m)",
            "0.5*(1.0-dir.z/ma/m),0.5*(1.0-dir.y/ma/m)",
            "0.5*(1.0-dir.x/ma/m),0.5*(1.0+dir.z/ma/m)",
            "0.5*(1.0-dir.x/ma/m),0.5*(1.0-dir.z/ma/m)",
            "0.5*(1.0-dir.x/ma/m),0.5*(1.0-dir.y/ma/m)",
            "0.5*(1.0+dir.x/ma/m),0.5*(1.0-dir.y/ma/m)",
        ];
        let mut last = 0usize;
        for (face, want) in expected.iter().enumerate() {
            let at = flat.find(want).unwrap_or_else(|| {
                panic!("{face} 面的公式与 point_shadow.rs 不一致（找不到 {want}）")
            });
            assert!(
                at > last || face == 0,
                "{face} 面的公式顺序不对：面序必须是 +X, -X, +Y, -Y, +Z, -Z"
            );
            last = at;
            // 层号必须也在同一段里（层序错了 = 影子长在别的面上）
            assert!(
                flat.contains(&format!("returnvec3<f32>({face}.0,")),
                "{face} 面的层号没写对"
            );
        }
        // 余量必须来自 uniform（`frame.point_shadow_texel.y`），不许硬编码 1.01：
        // 硬编码的话，改 Rust 常量就不再影响着色器，"两边一致"会悄悄失效。
        assert!(
            flat.contains("letm=frame.point_shadow_texel.y;"),
            "面余量必须取 uniform 里的 tan(半面角)"
        );
        assert!(!PBR_WGSL.contains("1.01"), "不许在 WGSL 里硬编码面余量");
        assert!(
            flat.contains("if(dot(dir,dir)<1e-12)"),
            "零方向要兜底（否则 ma = 0 会除出 NaN）"
        );
    }

    /// 点光源那一档同样：没有投影点光源时 `point_shadow_params.x` 必须是负数。
    #[test]
    fn frame_uniforms_use_a_negative_index_for_no_point_shadow() {
        let light = crate::light::PointLight::new(Vec3::ZERO, Vec3::ONE, 1.0, 20.0);
        assert!(
            plan_point_shadow(&[light], true).is_none(),
            "没勾就不是投影灯"
        );
        assert!(
            plan_point_shadow(&[light.with_cast_shadows(true)], false).is_none(),
            "总开关关掉时没有投影灯"
        );
        let on = plan_point_shadow(&[light.with_cast_shadows(true)], true).expect("应当有");
        assert_eq!(on.light_index, 0);
        // 与聚光灯的槽位互不影响：一盏投影点光源 + 一盏投影聚光灯可以同时存在
        let spot =
            crate::light::SpotLight::new(Vec3::ZERO, Vec3::NEG_Y, Vec3::ONE, 1.0, 10.0, 10.0, 20.0)
                .with_cast_shadows(true);
        let p = plan_point_shadow(&[light.with_cast_shadows(true)], true).unwrap();
        let s = plan_spot_shadow(&[light.with_cast_shadows(true)], &[spot], true).unwrap();
        assert_eq!(p.light_index, 0);
        assert_eq!(s.packed_index, 1, "点光排在前面，聚光在下标 1");
    }

    /// 没写投影灯时 `spot_shadow_params.x` 必须是负数：着色器就是靠它跳过整个采样的。
    #[test]
    fn frame_uniforms_use_a_negative_index_for_no_spot_shadow() {
        let light =
            crate::light::SpotLight::new(Vec3::ZERO, Vec3::NEG_Y, Vec3::ONE, 1.0, 10.0, 10.0, 20.0);
        assert!(plan_spot_shadow(&[], &[light], true).is_none());
        assert!(
            plan_spot_shadow(&[], &[light], false).is_none(),
            "总开关关掉时没有投影灯"
        );
        let on = plan_spot_shadow(&[], &[light.with_cast_shadows(true)], true).expect("应当有");
        assert_eq!(on.packed_index, 0);
    }

    #[test]
    fn pack_local_lights_puts_spots_after_points_with_the_cone_intact() {
        // 2 盏点光 + 3 盏聚光 → 5 个名额，聚光灯从下标 2 开始
        let points = vec![
            crate::light::PointLight::new(Vec3::new(1.0, 0.0, 0.0), Vec3::ONE, 2.0, 10.0),
            crate::light::PointLight::new(Vec3::new(2.0, 0.0, 0.0), Vec3::ONE, 3.0, 11.0),
        ];
        let spots = vec![
            crate::light::SpotLight::new(
                Vec3::new(0.0, 5.0, 0.0),
                Vec3::new(0.0, -1.0, 0.0),
                Vec3::ONE,
                4.0,
                20.0,
                10.0,
                25.0,
            ),
            crate::light::SpotLight::new(
                Vec3::new(0.0, 5.0, 1.0),
                Vec3::new(0.0, -1.0, 0.0),
                Vec3::ONE,
                5.0,
                21.0,
                20.0,
                40.0,
            ),
            crate::light::SpotLight::new(
                Vec3::new(0.0, 5.0, 2.0),
                Vec3::new(0.0, -1.0, 0.0),
                Vec3::ONE,
                6.0,
                22.0,
                30.0,
                60.0,
            ),
        ];
        let (packed, n) = pack_local_lights(&points, &spots);
        assert_eq!(n, 5);
        // 点光源两盏：w = 0 是"这是点光源"的标志
        assert_eq!(packed[0].dir_outer[3], 0.0);
        assert_eq!(packed[1].dir_outer[3], 0.0);
        assert_eq!(packed[1].pos_range[3], 11.0);
        // 聚光灯：位置/半径、方向（单位向量）、外锥余弦、内锥余弦
        assert_eq!(packed[2].pos_range[3], 20.0);
        assert_eq!(packed[2].cone_inner[1], 1.0, "y=1 表示聚光灯");
        let outer = 25.0f32.to_radians().cos();
        let inner = 10.0f32.to_radians().cos();
        assert!(
            (packed[2].dir_outer[3] - outer).abs() < 1e-6,
            "外锥余弦要按角度算：{} vs {outer}",
            packed[2].dir_outer[3]
        );
        assert!((packed[2].cone_inner[0] - inner).abs() < 1e-6);
        assert!(
            packed[2].dir_outer[3] < packed[2].cone_inner[0],
            "外锥余弦必须小于内锥余弦（外锥更大 → 余弦更小）"
        );
        // 方向归一化 + 零方向兜底
        assert!((packed[2].dir_outer[1] + 1.0).abs() < 1e-6);
        let zero =
            crate::light::SpotLight::new(Vec3::ZERO, Vec3::ZERO, Vec3::ONE, 1.0, 5.0, 10.0, 20.0);
        let (packed_zero, _) = pack_local_lights(&[], &[zero]);
        assert!(
            (packed_zero[0].dir_outer[1] + 1.0).abs() < 1e-6,
            "零方向要退化成向下照而不是 NaN"
        );
        // 名额有限：点光源先占满时聚光灯进不来（顺序确定，不是随机）
        let many: Vec<_> = (0..MAX_POINT_LIGHTS)
            .map(|i| {
                crate::light::PointLight::new(Vec3::new(i as f32, 0.0, 0.0), Vec3::ONE, 1.0, 5.0)
            })
            .collect();
        let (packed_full, n_full) = pack_local_lights(&many, &spots);
        assert_eq!(n_full, MAX_POINT_LIGHTS);
        assert!(packed_full.iter().all(|l| l.dir_outer[3] == 0.0));
    }

    #[test]
    fn instance_desc_packs_params() {
        let d = InstanceDesc::default()
            .with_pbr(1.0, 0.2)
            .with_emissive(3.0);
        let raw = d.to_raw();
        assert_eq!(raw.params[0], 1.0);
        assert_eq!(raw.params[1], 0.2);
        assert_eq!(raw.params[2], 3.0);
        assert_eq!(raw.params[3], 1.0, "应标记为覆盖材质");
    }
}
