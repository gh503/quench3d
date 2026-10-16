//! **Quench3D 渲染器** —— 基于 `wgpu` 的前向 PBR 渲染，同时支持桌面与 Web。
//!
//! 特性：
//! * Cook–Torrance PBR（金属度/粗糙度工作流）+ 贴图 + 法线贴图
//! * 平行光阴影贴图（3x3 PCF、纹素对齐、斜率偏移）
//! * 最多 8 个带平滑衰减的点光源
//! * 程序化天空（天顶/地平线/地面渐变 + 太阳光晕）
//! * 解析抗锯齿的无限地面网格（写入真实深度）
//! * 指数距离雾、ACES 色调映射、sRGB 输出、MSAA
//! * 逐实例数据（模型矩阵 + 颜色 + PBR 覆盖）+ 视锥剔除 + 按网格/材质合批
//! * 调试线段（坐标轴、包围盒、球、接触点、射线、视锥…）
//! * 离屏渲染与像素回读（无头截图 / 自动化测试）
//!
//! ```
//! use quench3d_render::prelude::*;
//!
//! # pollster::block_on(async {
//! // 没有可用 GPU 适配器时直接跳过（CI 机器就常常没有软件光栅驱动）。
//! // 文档示例不该在没显卡的机器上把 `cargo test` 弄红——这是本仓库对
//! // 「要 GPU 的测试」一贯的处理方式（见各 tests/*.rs 里的 headless_engine）。
//! let Ok(mut ctx) =
//!     GpuContext::headless(320, 200, GpuOptions { msaa_samples: 1, ..Default::default() }).await
//! else {
//!     return;
//! };
//! let mut renderer = Renderer::new(&ctx, RendererOptions::default());
//! let cube = renderer.add_mesh(&ctx, &MeshData::cube(1.0));
//! let mat = renderer.add_material(&ctx, MaterialDesc::from_srgb(0.9, 0.3, 0.2, 1.0).pbr(0.1, 0.4));
//!
//! renderer.begin_frame();
//! renderer.draw(cube, mat, InstanceDesc::from_position(Vec3::ZERO, 1.0));
//!
//! let mut camera = Camera::perspective(1.6);
//! camera.position = vec3(3.0, 2.0, 4.0);
//! camera.look_at(Vec3::ZERO);
//! let image = renderer.capture(&mut ctx, 320, 200, &camera, &Lighting::default()).unwrap();
//! assert_eq!(image.width, 320);
//! # });
//! ```

#![forbid(unsafe_code)]

pub mod camera;
pub mod cascade;
pub mod color;
pub mod context;
pub mod light;
pub mod lines;
pub mod material;
pub mod mesh;
pub mod renderer;
pub mod texture;

pub use camera::{Camera, CameraInput, FlyCamera, Frustum, OrbitCamera, Projection};
/// 重新导出 `wgpu`：上层（门面、编辑器、示例）需要它的类型（`TextureView`、
/// `Device`、`SurfaceConfiguration`…）时一律走这里，避免各处各写一个版本号。
pub use wgpu;

pub use context::{FrameError, GpuContext, GpuInitError, GpuOptions, OffscreenTarget};
pub use light::{DirectionalLight, FogParams, Lighting, PointLight, SkyParams, MAX_POINT_LIGHTS};
pub use lines::{DebugLines, LineVertex};
pub use material::{MaterialDesc, MaterialId, MeshId, TextureId};
pub use mesh::{MeshData, Vertex};
pub use renderer::{
    BloomOptions, FxaaOptions, InstanceDesc, MaterialTextureSlot, RenderStats, Renderer,
    RendererOptions, SsaoOptions, DEPTH_FORMAT, HDR_FORMAT, SHADOW_FORMAT,
};
pub use texture::{Filter, GpuTexture, ImageData, TextureOptions};

/// 常用类型一次性导入。
pub mod prelude {
    pub use crate::camera::{Camera, CameraInput, FlyCamera, OrbitCamera, Projection};
    pub use crate::color;
    pub use crate::context::{FrameError, GpuContext, GpuOptions};
    pub use crate::light::{DirectionalLight, FogParams, Lighting, PointLight, SkyParams};
    pub use crate::lines::DebugLines;
    pub use crate::material::MaterialDesc;
    pub use crate::mesh::MeshData;
    pub use crate::renderer::{
        BloomOptions, FxaaOptions, InstanceDesc, RenderStats, Renderer, RendererOptions,
        SsaoOptions,
    };
    pub use crate::texture::ImageData;
    pub use quench3d_math::{
        approx_eq, deg, lerp, rad, vec2, vec3, vec4, Aabb, Deg, Mat3, Mat4, Plane, Quat, Rad, Ray,
        Transform, Vec2, Vec3, Vec4,
    };
}
