//! # Quench3D
//!
//! 一个纯 Rust、零外部物理库依赖的 3D 引擎：数学、刚体物理、PBR 渲染、
//! 场景与资源，一套代码同时跑在**桌面**与**浏览器（WebGPU / WebGL2）**上。
//!
//! ## 四层结构
//!
//! | crate | 作用 | 依赖 |
//! |---|---|---|
//! | [`quench3d_math`] | Vec/Mat/Quat/AABB/Ray/Transform | 无 |
//! | [`quench3d_physics`] | 刚体、碰撞、关节、角色控制器 | 仅 math |
//! | [`quench3d_render`] | wgpu 前向 PBR、阴影、天空、网格、调试线 | math + wgpu |
//! | **quench3d**（本 crate） | 窗口/主循环/输入/场景/资源加载 | 以上全部 |
//!
//! ## 30 秒上手
//!
//! ```no_run
//! use quench3d::prelude::*;
//!
//! fn main() {
//!     quench3d::run(EngineConfig::default(), |engine: &mut Engine, dt: f32| {
//!         engine.world.step(dt);
//!     })
//!     .unwrap();
//! }
//! ```
//!
//! 带物理的场景（完整示例见仓库 `examples/`）：
//!
//! ```no_run
//! use quench3d::prelude::*;
//!
//! struct Demo;
//!
//! impl Game for Demo {
//!     /// 引擎启动时调用一次：此时 GPU、资源缓存、场景、物理世界都已就绪。
//!     fn init(&mut self, engine: &mut Engine) {
//!         // 1. 物理世界：一块无限大的静态地面
//!         engine
//!             .world
//!             .add_builder(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }));
//!
//!         // 2. 一个会掉落的动态盒子
//!         let body = engine.world.add_builder(
//!             BodyBuilder::dynamic(Shape::Box {
//!                 half_extents: Vec3::splat(0.5),
//!             })
//!             .position(Vec3::new(0.0, 4.0, 0.0))
//!             .friction(0.6)
//!             .restitution(0.2),
//!         );
//!
//!         // 3. 视觉：把刚体绑到场景里的立方体网格上（自动跟随物理变换）
//!         let material = engine.assets.solid_material(
//!             &mut engine.renderer,
//!             &engine.gpu,
//!             Vec4::new(0.90, 0.35, 0.25, 1.0),
//!             0.10,
//!             0.40,
//!         );
//!         let index = engine.scene.add_with_body(
//!             engine.assets.unit_cube,
//!             material,
//!             body,
//!             &engine.world,
//!             "箱子",
//!         );
//!         engine.scene.objects[index].transform.scale = Vec3::splat(1.0);
//!         engine.focus_scene();
//!     }
//!
//!     /// 固定步长逻辑：物理必须写在这里，才能保证不同帧率下行为一致。
//!     fn fixed_update(&mut self, engine: &mut Engine, dt: f32) {
//!         engine.world.step(dt);
//!     }
//! }
//!
//! fn main() {
//!     quench3d::run(EngineConfig::default(), Demo).unwrap();
//! }
//! ```
//!
//! 也可以只用闭包（`App` 适配器）：
//!
//! ```no_run
//! # use quench3d::prelude::*;
//! quench3d::run(
//!     EngineConfig::default(),
//!     App::new(
//!         |engine: &mut Engine| {
//!             engine.world.add_builder(
//!                 BodyBuilder::dynamic(Shape::Sphere { radius: 1.0 })
//!                     .position(Vec3::new(0.0, 5.0, 0.0)),
//!             );
//!         },
//!         |engine: &mut Engine, dt: f32| {
//!             engine.world.step(dt);
//!         },
//!     ),
//! )
//! .unwrap();
//! ```
//!
//! ## 操作方式（内置）
//!
//! | 操作 | 功能 |
//! |---|---|
//! | 中键拖动 / Alt+左键 | 轨道旋转 |
//! | 右键拖动 | 自由视角 |
//! | WASD / 方向键 | 平移或飞行 |
//! | Q / E | 升降 |
//! | 滚轮 | 缩放 |
//! | Shift | 加速 |
//! | Tab | 切换轨道 / 飞行相机 |
//! | R | 相机对准整个场景 |
//! | F3 / F4 | 网格 / 阴影开关 |
//! | F5 / F6 | 接触点 / 刚体包围盒 |
//! | P | 暂停物理与时间 |
//! | Esc | 退出 |

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod app;
pub mod assets;
pub mod clock;
pub mod input;
pub mod scene;
pub mod web_console;
/// Web 端自检探针（仅 wasm 目标，供 CI 的 `tools/check_web.mjs` 调用）。
#[cfg(target_arch = "wasm32")]
pub mod web_probe;

/// 数学库。
pub use quench3d_math as math;
/// 物理引擎。
pub use quench3d_physics as physics;
/// 渲染器。
pub use quench3d_render as render;
/// 窗口/输入事件库（转发 winit，方便上层直接用它的类型）。
pub use winit;

pub use app::{render_offscreen, run, App, CameraMode, DebugOptions, Engine, EngineConfig, Game};
pub use input::{Input, MouseButton};
pub use scene::{AssetCache, Scene, SceneObject};

pub use app::{
    render_offscreen_stats, render_sequence, render_sequence_with_game, write_png, SequenceFrame,
    SequenceParams,
};

/// Web 端入口（仅在 `wasm32` 下可用）。
#[cfg(target_arch = "wasm32")]
pub use app::run_web;

/// 常用类型一次性导入。
pub mod prelude {
    pub use crate::app::{
        render_offscreen, render_offscreen_stats, render_sequence, render_sequence_with_game,
        write_png, App, CameraMode, DebugOptions, Engine, EngineConfig, Game, SequenceFrame,
        SequenceParams,
    };
    pub use crate::assets::{load_gltf, load_model, load_obj, AssetError, LoadedModel};
    pub use crate::clock::now_seconds;
    pub use crate::input::{Input, MouseButton};
    pub use crate::scene::{AssetCache, Scene, SceneObject};
    pub use quench3d_math::{
        approx_eq, deg, lerp, rad, vec2, vec3, vec4, Aabb, Deg, Mat3, Mat4, Plane, Quat, Rad, Ray,
        Transform, Vec2, Vec3, Vec4,
    };
    pub use quench3d_physics::{
        BallConeLimit, BodyBuilder, BodyHandle, BodyType, CharacterController, CombineRule,
        ContactPair, ContactPoint, HingeMotor, Joint, JointHandle, MassProperties, PhysicsWorld,
        RaycastHit, RigidBody, Shape,
    };
    pub use quench3d_render::{
        color, Camera, CameraInput, DebugLines, DirectionalLight, FlyCamera, FogParams, GpuContext,
        GpuOptions, ImageData, InstanceDesc, Lighting, MaterialDesc, MaterialId, MeshData, MeshId,
        OrbitCamera, PointLight, RenderStats, Renderer, RendererOptions, SkyParams, TextureId,
    };
    pub use winit::event::WindowEvent;
    pub use winit::keyboard::KeyCode;
}
