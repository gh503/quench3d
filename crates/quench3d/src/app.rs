//! 引擎主循环：窗口、GPU 初始化、固定步长调度、相机控制与调试绘制。
//!
//! 使用者只需要实现 [`Game`] trait（或直接传一个闭包），引擎负责
//! 事件处理、dt 计算、固定步长物理调度、从物理同步到场景、以及提交渲染。

use std::sync::Arc;

use quench3d_math::{Transform, Vec2, Vec3, Vec4};
use quench3d_physics::{BodyHandle, JointVisual, PhysicsWorld};
use quench3d_render::{
    Camera, FlyCamera, FrameError, GpuContext, GpuOptions, Lighting, OrbitCamera, RenderStats,
    Renderer, RendererOptions,
};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton as WinitMouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowAttributes, WindowId};

use crate::input::{Input, MouseButton};
use crate::scene::{AssetCache, Scene};

/// 相机控制模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CameraMode {
    /// 轨道相机：绕焦点旋转（建模/观察用）。
    Orbit,
    /// 自由飞行：第一人称漫游（游戏用）。
    Fly,
}

/// 引擎配置。
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// 窗口标题。
    pub title: String,
    /// 初始宽度（逻辑像素）。
    pub width: u32,
    /// 初始高度（逻辑像素）。
    pub height: u32,
    /// 垂直同步。
    pub vsync: bool,
    /// MSAA 采样数。
    pub msaa_samples: u32,
    /// 渲染器选项。
    pub renderer: RendererOptions,
    /// 初始相机模式。
    pub camera_mode: CameraMode,
    /// 物理固定步长（秒）。
    pub fixed_dt: f32,
    /// 关闭窗口时是否退出程序。
    pub exit_on_close: bool,
    /// Web 端使用的 canvas 元素 id（桌面端忽略）。
    pub canvas_id: String,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            title: "Quench3D".to_string(),
            width: 1280,
            height: 720,
            vsync: true,
            msaa_samples: 4,
            renderer: RendererOptions::default(),
            camera_mode: CameraMode::Orbit,
            fixed_dt: 1.0 / 60.0,
            exit_on_close: true,
            canvas_id: "quench3d-canvas".to_string(),
        }
    }
}

/// 调试可视化开关。
#[derive(Clone, Copy, Debug)]
pub struct DebugOptions {
    /// 绘制世界坐标轴。
    pub axes: bool,
    /// 绘制物理接触点与法线。
    pub physics_contacts: bool,
    /// 绘制速度矢量。
    pub velocities: bool,
    /// 绘制每个动态刚体的包围盒。
    pub body_bounds: bool,
    /// 从相机中心打一条射线，画出命中的位置与法线（"我看的这块是什么"）。
    pub query_probe: bool,
    /// 画出播放中模型的骨骼线段（父关节 → 子关节）。
    pub bones: bool,
    /// 画出物理关节的两端锚点、连线与铰链/滑块轴。
    pub joints: bool,
    /// 是否启用（总开关）。
    pub enabled: bool,
}

impl Default for DebugOptions {
    fn default() -> Self {
        Self {
            axes: true,
            physics_contacts: false,
            velocities: false,
            body_bounds: false,
            query_probe: false,
            bones: false,
            joints: false,
            enabled: true,
        }
    }
}

/// 查询探针的最远距离（米）。
pub const QUERY_PROBE_MAX: f32 = 500.0;

/// 把关节可视化转成调试线段（纯函数，便于测试）。
fn push_joint_debug_lines(lines: &mut quench3d_render::DebugLines, joints: &[JointVisual]) {
    for joint in joints {
        lines.line(joint.a, joint.b, Vec4::new(0.25, 0.9, 0.95, 1.0));
        lines.contact_point(joint.a, Vec3::Y, 0.06, Vec4::new(1.0, 0.75, 0.15, 1.0));
        lines.contact_point(joint.b, Vec3::Y, 0.06, Vec4::new(1.0, 0.75, 0.15, 1.0));
        if let Some(axis) = joint.axis {
            let mid = (joint.a + joint.b) * 0.5;
            lines.line(
                mid,
                mid + axis.normalize_or_zero() * 0.35,
                Vec4::new(1.0, 0.3, 1.0, 1.0),
            );
        }
    }
}

/// 相机中心射线的最近命中（编辑器 F7 的查询探针；纯函数，便于离线测试）。
pub fn center_probe(
    camera: &Camera,
    world: &PhysicsWorld,
    max_distance: f32,
) -> Option<quench3d_physics::RaycastHit> {
    let ray = camera.ndc_ray(Vec2::ZERO);
    world.raycast(ray.origin, ray.dir, max_distance)
}

/// 引擎：持有 GPU 上下文、渲染器、场景、物理世界与输入状态。
pub struct Engine {
    /// GPU 上下文。
    pub gpu: GpuContext,
    /// 渲染器。
    pub renderer: Renderer,
    /// 预设资源缓存。
    pub assets: AssetCache,
    /// 场景。
    pub scene: Scene,
    /// 物理世界。
    pub world: PhysicsWorld,
    /// 输入状态。
    pub input: Input,
    /// 轨道相机。
    pub orbit: OrbitCamera,
    /// 飞行相机。
    pub fly: FlyCamera,
    /// 当前相机模式。
    pub camera_mode: CameraMode,
    /// 调试可视化开关。
    pub debug: DebugOptions,
    /// 仿真时长（秒）。
    ///
    /// 只在未暂停时增长；驱动着色器动画的 `time` 也取自它。
    pub time: f32,
    /// 帧计数（渲染帧数，暂停时照常累加）。
    pub frame_count: u64,
    /// 上一帧耗时。
    pub last_dt: f32,
    /// 平滑后的 FPS（暂停时照常更新：它描述的是渲染开销）。
    pub fps: f32,
    /// 是否暂停。
    ///
    /// 暂停时**不推进物理，也不增长 [`Engine::time`]**（画面与仿真一起冻结）；
    /// `frame_count` 与 `fps` 仍然更新，因为它们描述的是渲染本身。
    pub paused: bool,
    /// 是否请求退出。
    pub should_exit: bool,
    /// 是否由引擎自动把输入映射到相机（默认 `true`）。
    ///
    /// 编辑器这类应用要自己决定「这一下拖动是转相机还是拖物体」时，把它设成
    /// `false`，然后每帧自己调 [`Engine::update_camera`]（或直接
    /// `engine.orbit.update(&input)`）——两种相机都会按 `camera_mode` 走。
    pub auto_camera: bool,
    /// 最近一帧渲染统计。
    pub stats: RenderStats,
    /// 固定步长累加器。
    accumulator: f32,
    /// 窗口句柄（离屏模式下为 `None`）。
    window: Option<Arc<Window>>,
}

impl Engine {
    /// 由已有的 GPU 上下文构造引擎。
    pub fn new(gpu: GpuContext, config: &EngineConfig) -> Self {
        let mut renderer = Renderer::new(&gpu, config.renderer);
        let assets = AssetCache::new(&mut renderer, &gpu);
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(config.fixed_dt);
        Self {
            gpu,
            renderer,
            assets,
            scene: Scene::new(),
            world,
            input: Input::new(),
            orbit: OrbitCamera::default(),
            fly: FlyCamera::default(),
            camera_mode: config.camera_mode,
            debug: DebugOptions::default(),
            time: 0.0,
            frame_count: 0,
            last_dt: 0.0,
            fps: 0.0,
            paused: false,
            should_exit: false,
            auto_camera: true,
            stats: RenderStats::default(),
            accumulator: 0.0,
            window: None,
        }
    }

    /// 设置窗口句柄（内部使用）。
    pub fn set_window(&mut self, window: Arc<Window>) {
        self.window = Some(window);
    }

    /// 窗口句柄。
    pub fn window(&self) -> Option<&Arc<Window>> {
        self.window.as_ref()
    }

    /// 当前相机。
    pub fn camera(&self) -> Camera {
        let aspect = self.gpu.aspect();
        match self.camera_mode {
            CameraMode::Orbit => self.orbit.camera(aspect),
            CameraMode::Fly => self.fly.camera(aspect),
        }
    }

    /// 窗口尺寸（像素）。
    pub fn size(&self) -> (u32, u32) {
        self.gpu.size()
    }

    /// 视口尺寸（`Vec2`，便于直接传给 `Camera::screen_ray`）。
    pub fn viewport(&self) -> Vec2 {
        let (w, h) = self.size();
        Vec2::new(w as f32, h as f32)
    }

    /// 宽高比。
    pub fn aspect(&self) -> f32 {
        self.gpu.aspect()
    }

    /// 截屏保存为 PNG（需要 `image` 特性；桌面端可用）。
    pub fn screenshot(
        &mut self,
        width: u32,
        height: u32,
        path: impl AsRef<std::path::Path>,
    ) -> Result<(), String> {
        let camera = self.camera();
        let lighting = self.scene.lighting.clone();
        let image = self
            .renderer
            .capture(&mut self.gpu, width, height, &camera, &lighting)?;
        write_png(&image, path)?;
        Ok(())
    }

    /// 把当前相机对准整个场景。
    pub fn focus_scene(&mut self) {
        if let Some(bounds) = self.scene.bounds(&self.renderer) {
            self.orbit.focus_on(&bounds);
            self.fly.position = bounds.center() - self.orbit.direction() * self.orbit.distance;
        }
    }

    /// 加载 3D 模型并按扩展名自动选择解析器（`.gltf` / `.glb` / `.obj`），
    /// 把每个子网格注册成场景物体，返回它们的下标。
    ///
    /// 路径会依次尝试原路径、`assets/`、`quench3d/assets/` 与 crate 目录。
    ///
    /// ```no_run
    /// # use quench3d::prelude::*;
    /// # fn demo(engine: &mut Engine) -> Result<(), Box<dyn std::error::Error>> {
    /// let ids = engine.load_model("chair.glb", Transform::from_translation(Vec3::Y * 1.0))?;
    /// if let Some(b) = engine.scene.bounds_of(&engine.renderer, &ids) {
    ///     engine.orbit.focus_on(&b);
    /// }
    /// # let _ = ids;
    /// # Ok(()) }
    /// ```
    pub fn load_model(
        &mut self,
        path: impl AsRef<std::path::Path>,
        transform: Transform,
    ) -> Result<Vec<usize>, crate::assets::AssetError> {
        let model = crate::assets::load_model(path)?;
        Ok(self.add_model(&model, transform))
    }

    /// 把一个**已经加载好**的模型加进场景（渲染器 + 场景图），返回场景物体下标。
    ///
    /// 与 [`Engine::load_model`] 分开是为了让调用方先走模型缓存：解析（解压 / 解析 glTF /
    /// 解码贴图）是最贵的一步，同一个文件在一次运行里可能被读好几遍。
    pub fn add_model(
        &mut self,
        model: &crate::assets::LoadedModel,
        transform: Transform,
    ) -> Vec<usize> {
        model
            .spawn(&mut self.renderer, &self.gpu, &mut self.scene, transform)
            .objects
    }

    /// 加载模型，同时用**合并后的三角网格**创建一个静态刚体（关卡地形 / 建筑），
    /// 返回 `(场景物体下标, 刚体句柄)`。
    ///
    /// 静态三角网格质量视为无穷大，动态刚体可以落在它上面、被射线检测命中。
    pub fn load_model_as_static(
        &mut self,
        path: impl AsRef<std::path::Path>,
        transform: Transform,
        friction: f32,
    ) -> Result<(Vec<usize>, BodyHandle), crate::assets::AssetError> {
        let model = crate::assets::load_model(path)?;

        // 合并所有子网格，生成三角网格碰撞体
        let mut vertices: Vec<Vec3> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        for m in &model.meshes {
            let base = vertices.len() as u32;
            vertices.extend(m.data.vertices.iter().map(|v| {
                let p = v.pos();
                transform.position
                    + transform.rotation.mul_vec3(Vec3::new(
                        p.x * transform.scale.x,
                        p.y * transform.scale.y,
                        p.z * transform.scale.z,
                    ))
            }));
            indices.extend(m.data.indices.iter().map(|i| i + base));
        }

        let body = if vertices.is_empty() {
            None
        } else {
            Some(
                self.world.add_builder(
                    quench3d_physics::BodyBuilder::r#static(quench3d_physics::Shape::TriMesh {
                        vertices,
                        indices,
                    })
                    .position(transform.position)
                    .friction(friction),
                ),
            )
        };

        let ids = model
            .spawn(&mut self.renderer, &self.gpu, &mut self.scene, transform)
            .objects;
        Ok((ids, body.unwrap_or_else(|| BodyHandle::new(0, 0))))
    }

    /// 设置窗口标题。
    pub fn set_title(&self, title: &str) {
        if let Some(w) = &self.window {
            w.set_title(title);
        }
    }

    /// 请求重绘。
    pub fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    /// 处理输入到相机的映射（引擎自动调用；自己写事件循环时也可直接调）。
    pub fn update_camera(&mut self, dt: f32) {
        let orbit_drag = match self.camera_mode {
            CameraMode::Orbit => {
                self.input.mouse_button_down(MouseButton::Middle)
                    || (self.input.alt && self.input.mouse_button_down(MouseButton::Left))
            }
            CameraMode::Fly => self.input.mouse_button_down(MouseButton::Right),
        };
        let cam_input = self.input.camera_input(dt, orbit_drag);
        match self.camera_mode {
            CameraMode::Orbit => self.orbit.update(&cam_input),
            CameraMode::Fly => self.fly.update(&cam_input),
        }
    }

    /// 内置快捷键（Tab 相机模式、R 聚焦、F3/F4 网格阴影、F5~F8 调试、P 暂停、Esc 退出）。
    ///
    /// 自己写事件循环（编辑器）时直接调它就能白拿这套快捷键；
    /// 传 `None` 表示"只改状态，不主动退出事件循环"（退出交给调用方看
    /// [`Engine::should_exit`]）。
    pub fn handle_engine_keys(&mut self, event_loop: Option<&ActiveEventLoop>) {
        if self.input.key_pressed(KeyCode::Tab) {
            self.camera_mode = match self.camera_mode {
                CameraMode::Orbit => CameraMode::Fly,
                CameraMode::Fly => CameraMode::Orbit,
            };
        }
        if self.input.key_pressed(KeyCode::KeyR) {
            self.focus_scene();
        }
        if self.input.key_pressed(KeyCode::F3) {
            self.renderer.options.grid = !self.renderer.options.grid;
        }
        if self.input.key_pressed(KeyCode::F4) {
            self.renderer.options.shadows = !self.renderer.options.shadows;
        }
        if self.input.key_pressed(KeyCode::F5) {
            self.debug.physics_contacts = !self.debug.physics_contacts;
        }
        if self.input.key_pressed(KeyCode::F6) {
            self.debug.body_bounds = !self.debug.body_bounds;
        }
        if self.input.key_pressed(KeyCode::F7) {
            self.debug.query_probe = !self.debug.query_probe;
        }
        if self.input.key_pressed(KeyCode::F8) {
            self.debug.bones = !self.debug.bones;
        }
        if self.input.key_pressed(KeyCode::F11) {
            self.debug.joints = !self.debug.joints;
        }
        if self.input.key_pressed(KeyCode::KeyT) {
            self.renderer.options.tonemap = self.renderer.options.tonemap.next();
        }
        if self.input.key_pressed(KeyCode::KeyP) {
            self.paused = !self.paused;
        }
        if self.input.key_pressed(KeyCode::Escape) {
            self.should_exit = true;
            if let Some(el) = event_loop {
                el.exit();
            }
        }
    }

    fn draw_debug(&mut self) {
        if !self.debug.enabled {
            return;
        }
        // 查询探针的结果先算好：后面 `lines` 借了 `self.renderer` 的可变引用，
        // 那时就不能再读 `self.camera()` / `self.world` 了。
        let skeleton = if self.debug.bones {
            self.scene.model_skeleton_lines()
        } else {
            Vec::new()
        };
        let joint_visuals = if self.debug.joints {
            self.world.joint_visuals()
        } else {
            Vec::new()
        };
        let probe = if self.debug.query_probe {
            let camera = self.camera();
            let ray = camera.ndc_ray(Vec2::ZERO);
            Some((ray, center_probe(&camera, &self.world, QUERY_PROBE_MAX)))
        } else {
            None
        };
        let lines = &mut self.renderer.lines;
        if self.debug.axes {
            lines.world_axes(3.0, 0.35);
        }
        for (a, b) in skeleton {
            lines.line(a, b, Vec4::new(0.2, 0.95, 0.85, 0.95));
            lines.contact_point(a, Vec3::Y, 0.025, Vec4::new(1.0, 0.85, 0.2, 0.9));
        }
        push_joint_debug_lines(lines, &joint_visuals);
        if self.debug.physics_contacts {
            for c in self.world.contacts() {
                // 传感器（触发区）的接触不是真的"碰上"：用青色区分，免得看着像穿了模
                // （它本来就不产生冲量，物体确实会穿过去）。
                let color = if c.sensor {
                    Vec4::new(0.2, 0.85, 1.0, 1.0)
                } else {
                    Vec4::new(1.0, 0.6, 0.1, 1.0)
                };
                lines.contact_point(c.point, c.normal, 0.08, color);
            }
        }
        if self.debug.velocities {
            for (_, body) in self.world.bodies() {
                if body.is_dynamic() && !body.is_sleeping() {
                    let v = body.linear_velocity();
                    if v.length_squared() > 1e-4 {
                        lines.arrow(
                            body.position(),
                            body.position() + v * 0.12,
                            Vec4::new(0.3, 0.8, 1.0, 0.9),
                        );
                    }
                }
            }
        }
        if self.debug.body_bounds {
            for (_, body) in self.world.bodies() {
                if body.is_dynamic() {
                    lines.aabb(
                        &body.world_aabb().grown(0.002),
                        Vec4::new(0.2, 1.0, 0.4, 0.32),
                    );
                }
            }
        }
        if let Some((ray, hit)) = probe {
            // 相机中心那一条射线打到什么：命中的位置画红箭头、法线画绿箭头。
            // 与 CLI 的 `quench3d query --ray`、MCP 的 quench3d_query 是同一条查询路径。
            match hit {
                Some(hit) => {
                    lines.arrow(ray.origin, hit.point, Vec4::new(1.0, 0.25, 0.3, 0.95));
                    lines.arrow(
                        hit.point,
                        hit.point + hit.normal * 0.4,
                        Vec4::new(0.4, 1.0, 0.5, 0.95),
                    );
                }
                None => {
                    // 没打到：画一条灰线示意"看的方向"，长度=探测距离上限
                    lines.arrow(
                        ray.origin,
                        ray.origin + ray.dir * QUERY_PROBE_MAX,
                        Vec4::new(0.6, 0.6, 0.6, 0.5),
                    );
                }
            }
        }
    }

    /// 推进一帧：输入 → 相机 → `game.update` → 固定步长物理 → 物理回同步 →
    /// 提交绘制指令（`begin_frame` + 场景 + 调试线 + `game.draw`）→ 推进时钟。
    ///
    /// **不碰交换链**。两种用法：
    /// * "一帧到底"（`ForgeApp`、示例）：[`Engine::tick`] 之后调
    ///   `renderer.draw_frame`；
    /// * 自己掌控呈现（编辑器要在 3D 之上叠 UI、离屏录制）：
    ///   [`Engine::tick`] 之后自己取帧 → [`Engine::render_to`] → 叠 UI → 呈现。
    ///
    /// 存在的理由：以前这套顺序只有 `ForgeApp::frame` 一条私有路径，
    /// 想加个 UI 层就得把它抄一遍（抄漏一处就是"物理跑两遍""时间不推进"这类鬼 bug）。
    pub fn tick<G: Game + ?Sized>(&mut self, game: &mut G, dt: f32) {
        self.input.begin_frame();
        // 着色器动画时间：与离屏录制路径同源（当前着色器没读这个字段，
        // 但契约放在这里，免得窗口路径永远是 0）。
        self.renderer.set_time(self.time);
        if self.auto_camera {
            self.update_camera(dt);
        }
        game.update(self, dt);

        // 固定步长逻辑（物理）：最多追赶 8 步，避免长时间卡顿后"炸开"。
        if !self.paused {
            let fixed = self.world.fixed_dt().clamp(1.0 / 1000.0, 1.0 / 10.0);
            self.accumulator += dt;
            let mut steps = 0;
            while self.accumulator >= fixed && steps < 8 {
                game.fixed_update(self, fixed);
                self.accumulator -= fixed;
                steps += 1;
            }
            if steps == 8 {
                self.accumulator = 0.0;
            }
        }

        if self.scene.auto_sync_physics {
            let world = &self.world;
            self.scene.sync_from_physics(world);
        }
        // 模型动画按**帧**时间推进（不是物理子步）：它是视觉量，暂停时也不该继续走。
        if !self.paused {
            self.scene
                .advance_animations_with_renderer(dt, &mut self.renderer, &self.gpu);
        }

        // 手工 LOD：相机移动（即使暂停）也要切层级；相机位置在切之前取。
        let camera_position = self.camera().position;
        self.scene.update_lods(camera_position);
        // 粒子是视觉量：暂停时不再老化/飞行，但仍要面向当前相机。
        let particle_dt = if self.paused { 0.0 } else { dt };
        self.scene.update_particles(camera_position, particle_dt);

        self.renderer.begin_frame();
        self.scene.submit(&mut self.renderer);
        self.draw_debug();
        game.draw(self);

        advance_clock(
            &mut self.time,
            &mut self.frame_count,
            &mut self.last_dt,
            &mut self.fps,
            dt,
            self.paused,
        );
    }

    /// 把当前场景渲染到任意视图（**不呈现**）。`size` 是目标像素尺寸。
    ///
    /// 必须在 [`Engine::tick`] 之后调用（绘制指令是 tick 里提交的）。
    pub fn render_to(
        &mut self,
        view: &quench3d_render::wgpu::TextureView,
        size: (u32, u32),
    ) -> RenderStats {
        let camera = self.camera();
        let lighting: Lighting = self.scene.lighting.clone();
        self.renderer
            .render(&self.gpu, view, size, &camera, &lighting)
    }

    /// 执行一帧（逻辑 + 渲染 + 呈现），窗口循环用。
    fn frame<G: Game>(&mut self, game: &mut G, dt: f32) {
        self.tick(game, dt);

        let camera = self.camera();
        let lighting: Lighting = self.scene.lighting.clone();
        match self.renderer.draw_frame(&mut self.gpu, &camera, &lighting) {
            Ok(stats) => self.stats = stats,
            Err(FrameError::Lost | FrameError::Outdated) => {
                // 交换链失效：重新配置后继续（下一帧会重建尺寸相关的附件）
                let (w, h) = self.gpu.size();
                self.gpu.resize(w, h);
                self.gpu.reconfigure();
            }
            // 超时 / 被遮挡 / 验证错误：跳过这一帧即可
            Err(e) => log::debug!("本帧跳过: {e}"),
        }
    }
}

/// 推进一帧时钟。
///
/// `frame_count` / `last_dt` / `fps` **每帧都更新**：它们描述的是「渲染了多少帧、跑多快」，
/// 与仿真时间无关（暂停时 UI 仍然需要它们保持活跃）。
/// `time` 只在未暂停时增长——「暂停」对仿真时间是真的冻结。
///
/// 拆成自由函数是为了能在没有 GPU 的情况下单测这条规则（`Engine` 必须持有 GPU 上下文）。
fn advance_clock(
    time: &mut f32,
    frame_count: &mut u64,
    last_dt: &mut f32,
    fps: &mut f32,
    dt: f32,
    paused: bool,
) {
    *frame_count += 1;
    *last_dt = dt;
    if !paused {
        *time += dt;
    }
    let inst = if dt > 0.0 { 1.0 / dt } else { 0.0 };
    *fps = if *fps <= 0.0 {
        inst
    } else {
        *fps * 0.9 + inst * 0.1
    };
}

// 固定步长累加器的只读访问（调试/UI 用）。
impl Engine {
    /// 尚未消费的固定步长余量（秒）。
    pub fn pending_steps(&self) -> f32 {
        self.accumulator
    }
}

/// 逐帧 + 固定步长的游戏回调。
/// 游戏/应用逻辑入口。
///
/// 所有方法都有默认实现，按需覆盖即可；最简单的用法是直接传一个
/// `FnMut(&mut Engine, f32)` 闭包（自动实现本 trait），或用 [`App`]
/// 把「初始化 + 每帧」两个闭包组装起来。
pub trait Game {
    /// 引擎就绪后调用一次。
    fn init(&mut self, _engine: &mut Engine) {}
    /// 每帧调用（可变步长）。
    fn update(&mut self, _engine: &mut Engine, _dt: f32) {}
    /// 固定步长调用，适合物理与确定性逻辑。
    fn fixed_update(&mut self, _engine: &mut Engine, _dt: f32) {}
    /// 渲染阶段调用，可继续提交自定义绘制。
    fn draw(&mut self, _engine: &mut Engine) {}
    /// 原始窗口事件。
    fn on_window_event(&mut self, _engine: &mut Engine, _event: &WindowEvent) {}
}

impl<F> Game for F
where
    F: FnMut(&mut Engine, f32),
{
    fn update(&mut self, engine: &mut Engine, dt: f32) {
        (self)(engine, dt);
    }
}

/// 用闭包快速组装 [`Game`] 的适配器。
///
/// ```
/// # use quench3d::prelude::*;
/// # fn demo() {
/// quench3d::run(
///     EngineConfig::default(),
///     quench3d::App::new(
///         |engine: &mut Engine| {
///             let cube = engine.assets.unit_cube;
///             let mat = engine.assets.solid_material(
///                 &mut engine.renderer, &engine.gpu, Vec4::new(0.9, 0.3, 0.2, 1.0), 0.1, 0.4);
///             engine.scene.add_named(cube, mat, Transform::IDENTITY, "cube");
///         },
///         |engine: &mut Engine, dt: f32| {
///             engine.world.step(dt);
///         },
///     ),
/// )
/// .unwrap();
/// # }
/// ```
pub struct App<S, U> {
    /// 初始化回调。
    pub setup: S,
    /// 每帧回调。
    pub update: U,
}

impl<S, U> App<S, U>
where
    S: FnMut(&mut Engine),
    U: FnMut(&mut Engine, f32),
{
    /// 由「初始化 + 每帧」两个闭包构造。
    pub fn new(setup: S, update: U) -> Self {
        Self { setup, update }
    }

    /// 只有每帧回调（没有初始化逻辑）。
    pub fn from_update(update: U) -> App<fn(&mut Engine), U> {
        fn noop(_: &mut Engine) {}
        App {
            setup: noop as fn(&mut Engine),
            update,
        }
    }
}

impl<S, U> Game for App<S, U>
where
    S: FnMut(&mut Engine),
    U: FnMut(&mut Engine, f32),
{
    fn init(&mut self, engine: &mut Engine) {
        (self.setup)(engine);
    }

    fn update(&mut self, engine: &mut Engine, dt: f32) {
        (self.update)(engine, dt);
    }
}

/// Web 端异步 GPU 初始化的结果槽：`(上下文, 窗口)` 或错误字符串。
///
/// 单独起个名字，否则 `Rc<RefCell<Option<Result<..>>>>` 会被 clippy 的
/// `type_complexity` 拦下（wasm 的 CI 任务带 `-D warnings`）。
#[cfg(target_arch = "wasm32")]
type PendingGpu =
    std::rc::Rc<std::cell::RefCell<Option<Result<(GpuContext, Arc<Window>), String>>>>;

/// 包装 `Game` 的 winit 应用。
struct ForgeApp<G: Game> {
    config: EngineConfig,
    game: G,
    engine: Option<Box<Engine>>,
    last_time: f64,
    initialized_game: bool,
    /// Web 端异步 GPU 初始化的结果槽。
    #[cfg(target_arch = "wasm32")]
    pending: PendingGpu,
    /// Web 端：GPU 就绪前保留窗口引用，避免 canvas 被回收。
    #[cfg(target_arch = "wasm32")]
    pending_window: Option<Arc<Window>>,
    /// Web 端：是否已经画出第一帧（用来决定什么时候收起页面遮罩）。
    #[cfg(target_arch = "wasm32")]
    first_frame_done: bool,
}

impl<G: Game> ForgeApp<G> {
    fn new(config: EngineConfig, game: G) -> Self {
        // wasm 上没有 std 时钟，物理的阶段计时需要宿主装一个（不装也不会 panic，
        // 只是计时全是 0）。原生用 Instant，无需安装。
        #[cfg(target_arch = "wasm32")]
        {
            quench3d_physics::set_time_source(crate::clock::now_seconds);
        }
        Self {
            config,
            game,
            engine: None,
            last_time: crate::clock::now_seconds(),
            initialized_game: false,
            #[cfg(target_arch = "wasm32")]
            pending: std::rc::Rc::new(std::cell::RefCell::new(None)),
            #[cfg(target_arch = "wasm32")]
            pending_window: None,
            #[cfg(target_arch = "wasm32")]
            first_frame_done: false,
        }
    }

    fn window_attributes(&self) -> WindowAttributes {
        let attrs = Window::default_attributes()
            .with_title(self.config.title.clone())
            .with_inner_size(winit::dpi::LogicalSize::new(
                self.config.width,
                self.config.height,
            ));
        #[cfg(target_arch = "wasm32")]
        let attrs = {
            use wasm_bindgen::JsCast;
            use winit::platform::web::WindowAttributesExtWebSys;
            let canvas = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.get_element_by_id(&self.config.canvas_id))
                .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok());
            attrs.with_canvas(canvas)
        };
        attrs
    }

    /// 构建引擎并调用 `Game::init`（只调用一次）。
    fn init_engine(&mut self, gpu: GpuContext, window: Option<Arc<Window>>) {
        let mut engine = Box::new(Engine::new(gpu, &self.config));
        if let Some(w) = window {
            engine.set_window(w);
        }
        if !self.initialized_game {
            self.game.init(&mut engine);
            self.initialized_game = true;
        }
        self.engine = Some(engine);
        self.last_time = crate::clock::now_seconds();
    }

    /// 平台无关的窗口事件处理。
    fn handle_window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        let Some(engine) = self.engine.as_mut() else {
            return;
        };
        self.game.on_window_event(engine, &event);
        match event {
            WindowEvent::CloseRequested => {
                if self.config.exit_on_close {
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(size) => {
                engine.gpu.resize(size.width, size.height);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(w) = engine.window() {
                    let size = w.inner_size();
                    engine.gpu.resize(size.width, size.height);
                }
            }
            WindowEvent::Focused(focused) => {
                engine.input.focused = focused;
                if !focused {
                    engine.input.on_focus_lost();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    match event.state {
                        ElementState::Pressed => engine.input.on_key_down(code),
                        ElementState::Released => engine.input.on_key_up(code),
                    }
                }
                if event.state == ElementState::Pressed && !event.repeat {
                    engine.handle_engine_keys(Some(event_loop));
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                let state = modifiers.state();
                engine.input.shift = state.shift_key();
                engine.input.ctrl = state.control_key();
                engine.input.alt = state.alt_key();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let btn = match button {
                    WinitMouseButton::Left => Some(MouseButton::Left),
                    WinitMouseButton::Right => Some(MouseButton::Right),
                    WinitMouseButton::Middle => Some(MouseButton::Middle),
                    _ => None,
                };
                if let Some(b) = btn {
                    match state {
                        ElementState::Pressed => engine.input.on_mouse_down(b),
                        ElementState::Released => engine.input.on_mouse_up(b),
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                engine
                    .input
                    .on_cursor_moved(Vec2::new(position.x as f32, position.y as f32));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let scroll = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 50.0,
                };
                engine.input.on_scroll(scroll);
            }
            WindowEvent::RedrawRequested => {
                let now = crate::clock::now_seconds();
                let dt = ((now - self.last_time) as f32).clamp(0.0, 0.1);
                self.last_time = now;
                let game = &mut self.game;
                if let Some(engine) = self.engine.as_mut() {
                    engine.frame(game, dt);
                    // 第一帧真的画出来了，才把页面上的遮罩收掉：
                    // 否则页面会「假装启动成功」，然后给你一块黑屏。
                    #[cfg(target_arch = "wasm32")]
                    if !self.first_frame_done {
                        self.first_frame_done = true;
                        if let Some(w) = engine.window() {
                            let size = w.inner_size();
                            log::info!(
                                "第一帧已渲染：{}x{}（{}）",
                                size.width,
                                size.height,
                                engine.gpu.adapter_info.name
                            );
                        }
                        crate::web_console::mark_ready();
                    }
                    if engine.should_exit {
                        event_loop.exit();
                    }
                    if let Some(w) = engine.window() {
                        w.request_redraw();
                    }
                }
            }
            _ => {}
        }
    }

    /// 是否有需要持续重绘的窗口。
    fn request_next_frame(&self) {
        if let Some(engine) = self.engine.as_ref() {
            if let Some(w) = engine.window() {
                w.request_redraw();
            }
        }
    }

    /// 处理 Web 端异步初始化结果。
    #[cfg(target_arch = "wasm32")]
    fn poll_pending(&mut self, event_loop: &ActiveEventLoop) {
        let result = self.pending.borrow_mut().take();
        match result {
            Some(Ok((gpu, window))) => {
                self.pending_window = None;
                log::info!(
                    "Quench3D 启动：{} ({:?})",
                    gpu.adapter_info.name,
                    gpu.adapter_info.backend
                );
                self.init_engine(gpu, Some(window));
            }
            Some(Err(e)) => {
                crate::web_console::report_fatal(&format!(
                    "GPU 初始化失败：{e}\n（浏览器需要 WebGPU，或可用的 WebGL2。\
                     强制后端：在网址后面加 ?backend=gl 或 ?backend=webgpu 再刷新）"
                ));
                event_loop.exit();
            }
            None => {}
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl<G: Game> ApplicationHandler for ForgeApp<G> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.engine.is_some() {
            return;
        }
        let window = match event_loop.create_window(self.window_attributes()) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("创建窗口失败: {e}");
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        let options = GpuOptions {
            msaa_samples: self.config.msaa_samples,
            vsync: self.config.vsync,
            ..Default::default()
        };
        let win = window.clone();
        let gpu = match pollster::block_on(GpuContext::from_candidates(w, h, options, |inst| {
            inst.create_surface(win.clone())
                .map(Some)
                .map_err(|e| quench3d_render::GpuInitError::Surface(format!("{e}")))
        })) {
            Ok(g) => g,
            Err(e) => {
                log::error!("GPU 初始化失败: {e}");
                event_loop.exit();
                return;
            }
        };
        log::info!(
            "Quench3D 启动：{} ({:?}) {}x{} MSAA x{}",
            gpu.adapter_info.name,
            gpu.adapter_info.backend,
            w,
            h,
            gpu.msaa_samples
        );
        self.init_engine(gpu, Some(window));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        self.handle_window_event(event_loop, event);
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        self.request_next_frame();
    }
}

/// Web 端：窗口/GPU 初始化必须异步完成，结果通过 `pending` 槽回传。
#[cfg(target_arch = "wasm32")]
impl<G: Game> ApplicationHandler for ForgeApp<G> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.engine.is_some() || self.pending.borrow().is_some() {
            return;
        }
        crate::web_console::set_progress("正在创建 WebGPU/WebGL2 渲染面…");
        let window = match event_loop.create_window(self.window_attributes()) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                crate::web_console::report_fatal(&format!("创建窗口失败：{e}"));
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        crate::web_console::set_progress(&format!("正在初始化渲染后端（画布 {w}x{h}）…"));
        let options = GpuOptions {
            msaa_samples: self.config.msaa_samples,
            vsync: self.config.vsync,
            ..Default::default()
        };
        let slot = self.pending.clone();
        let win = window.clone();
        wasm_bindgen_futures::spawn_local(async move {
            // 后端顺序由页面探好（`globalThis.__quench3d_backend`）：浏览器里
            // "navigator.gpu 存在" ≠ "有 WebGPU 适配器"，而 wgpu 只要看到
            // `BROWSER_WEBGPU` + navigator.gpu 存在就只建 WebGPU 上下文，连 WebGL2
            // 实例都不建；更糟的是**同一张画布要过 webgpu 上下文后，getContext("webgl2")
            // 会返回 null**，回退就再也回不去了。所以先探、再按结果排候选。
            let candidates =
                quench3d_render::GpuContext::candidates_from_hint(web_backend_hint().as_deref());
            log::info!("Web 后端候选：{candidates:?}");
            let w2 = win.clone();
            let result =
                GpuContext::from_candidates_with(&candidates, w, h, options, move |inst| {
                    inst.create_surface(w2.clone())
                        .map(Some)
                        .map_err(|e| quench3d_render::GpuInitError::Surface(format!("{e}")))
                })
                .await
                .map(|gpu| (gpu, win))
                .map_err(|e| e.to_string());
            *slot.borrow_mut() = Some(result);
        });
        self.pending_window = Some(window);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        self.poll_pending(event_loop);
        if self.engine.is_some() {
            self.handle_window_event(event_loop, event);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.poll_pending(event_loop);
        self.request_next_frame();
    }
}

/// 运行引擎（桌面端阻塞直到窗口关闭）。
pub fn run<G: Game + 'static>(
    config: EngineConfig,
    game: G,
) -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    }
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = ForgeApp::new(config, game);
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// Web 端"按页面探好的结果"排出的后端候选（优先级从高到低）。
///
/// 页面在 `start()` 之前把结论写进 `globalThis.__quench3d_backend`
/// （见 `web/index.html`）：
/// - `"webgpu"`：`navigator.gpu.requestAdapter()` 真的给到了适配器 → WebGPU 优先，WebGL2 兜底；
/// - `"gl"`：没有 `navigator.gpu`，或探测返回 null（Linux Chrome 没开 Vulkan、
///   软件光栅环境都长这样）→ **只**试 WebGL2，绝不去碰画布的 `webgpu` 上下文；
/// - `?backend=` 显式指定时按指定的来。
///
/// 没探到结论（老页面、直接嵌 wasm）就退回默认顺序，行为与不带页面时一致。
#[cfg(target_arch = "wasm32")]
fn web_backend_hint() -> Option<String> {
    let key = wasm_bindgen::JsValue::from_str("__quench3d_backend");
    js_sys::Reflect::get(&js_sys::global(), &key)
        .ok()
        .and_then(|v| v.as_string())
}

/// 运行引擎（Web 端：由 `#[wasm_bindgen]` 入口调用）。
#[cfg(target_arch = "wasm32")]
pub fn run_web<G: Game + 'static>(
    config: EngineConfig,
    game: G,
) -> Result<(), Box<dyn std::error::Error>> {
    use winit::platform::web::EventLoopExtWebSys;
    console_error_panic_hook::set_once();
    // 之前 Web 端没装 logger，所有 log::info!/log::error! 都被丢掉，
    // 出问题只剩一块黑屏——这里把日志接到浏览器控制台。
    crate::web_console::install_logger();
    log::info!("Quench3D Web 启动：{}x{}", config.width, config.height);
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    // Web 端不能用 run_app（它会抛出 JS 异常且永不返回），改用 spawn_app
    event_loop.spawn_app(ForgeApp::new(config, game));
    Ok(())
}

/// 把图像写成 PNG 文件（仅桌面端）。
#[cfg(not(target_arch = "wasm32"))]
pub fn write_png(
    image: &quench3d_render::ImageData,
    path: impl AsRef<std::path::Path>,
) -> Result<(), String> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
        }
    }
    let buf: image::RgbaImage =
        image::ImageBuffer::from_raw(image.width, image.height, image.rgba.clone())
            .ok_or_else(|| "图像数据不合法".to_string())?;
    buf.save(path).map_err(|e| format!("保存 PNG 失败: {e}"))
}

/// 把图像写成 PNG：Web 端编码后通过 `<a download>` 触发浏览器下载。
///
/// `path` 只取文件名作为下载文件名（浏览器不允许写入任意路径）。
#[cfg(target_arch = "wasm32")]
pub fn write_png(
    image: &quench3d_render::ImageData,
    path: impl AsRef<std::path::Path>,
) -> Result<(), String> {
    use wasm_bindgen::JsCast;

    let mut bytes: Vec<u8> = Vec::new();
    image
        .write_png(&mut bytes)
        .map_err(|e| format!("PNG 编码失败: {e}"))?;

    let array = js_sys::Uint8Array::from(bytes.as_slice());
    let parts = js_sys::Array::new();
    parts.push(&array.buffer());
    let blob = web_sys::Blob::new_with_u8_array_sequence(&parts)
        .map_err(|e| format!("创建 Blob 失败: {e:?}"))?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)
        .map_err(|e| format!("创建下载链接失败: {e:?}"))?;

    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| "无法访问 document".to_string())?;
    let anchor: web_sys::HtmlAnchorElement = document
        .create_element("a")
        .map_err(|e| format!("创建 <a> 失败: {e:?}"))?
        .dyn_into()
        .map_err(|_| "创建 <a> 失败".to_string())?;
    let name = path
        .as_ref()
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("quench3d.png");
    anchor.set_href(&url);
    anchor.set_download(name);
    anchor.click();
    let _ = web_sys::Url::revoke_object_url(&url);
    Ok(())
}

/// 无头渲染一张截图（不需要窗口；适合 CI 与自动化验证）。
pub fn render_offscreen(
    width: u32,
    height: u32,
    config: &EngineConfig,
    setup: impl FnOnce(&mut Engine),
) -> Result<quench3d_render::ImageData, String> {
    render_offscreen_stats(width, height, config, setup).map(|(image, _)| image)
}

/// 与 [`render_offscreen`] 相同，但额外返回这一帧的渲染统计。
///
/// 适合需要「到底画了多少东西」的工具链（例如 CLI 的结构化报告）。
pub fn render_offscreen_stats(
    width: u32,
    height: u32,
    config: &EngineConfig,
    setup: impl FnOnce(&mut Engine),
) -> Result<(quench3d_render::ImageData, quench3d_render::RenderStats), String> {
    let gpu = pollster::block_on(GpuContext::headless(
        width,
        height,
        GpuOptions {
            msaa_samples: config.msaa_samples,
            ..Default::default()
        },
    ))
    .map_err(|e| e.to_string())?;
    let mut engine = Engine::new(gpu, config);
    setup(&mut engine);
    let camera = engine.camera();
    let lighting = engine.scene.lighting.clone();
    engine.renderer.begin_frame();
    engine.scene.submit(&mut engine.renderer);
    engine.draw_debug();
    let image = engine
        .renderer
        .capture(&mut engine.gpu, width, height, &camera, &lighting)?;
    Ok((image, engine.renderer.stats()))
}

// ---------------------------------------------------------------------------
// 仿真视频 / 无头逐帧推进
// ---------------------------------------------------------------------------

/// 逐帧推进参数（仿真视频与无头回归共用）。
///
/// 视频时长完全由**物理步**决定，与墙钟无关：
/// `时长 = frames × steps_per_frame × dt`，等价帧率 `fps = 1 / (dt × steps_per_frame)`。
/// 想录慢动作就把 fps 调小、想录加速就调大——仿真本身不变。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SequenceParams {
    /// 输出帧数（不含预热帧）。
    pub frames: u32,
    /// 每帧推进的物理步数。
    pub steps_per_frame: u32,
    /// 单个物理步的固定时长（秒）。
    pub dt: f32,
    /// 预热帧数：只推进、不输出（丢弃着色器编译与物理沉降造成的脏帧）。
    pub warmup_frames: u32,
}

impl Default for SequenceParams {
    fn default() -> Self {
        Self {
            frames: 60,
            steps_per_frame: 1,
            dt: 1.0 / 60.0,
            warmup_frames: 0,
        }
    }
}

impl SequenceParams {
    /// 输出视频时长（秒）。
    pub fn duration(&self) -> f32 {
        self.frames as f32 * self.steps_per_frame as f32 * self.dt
    }

    /// 等价帧率（每秒输出帧数）。
    pub fn fps(&self) -> f32 {
        if self.steps_per_frame == 0 || !self.dt.is_finite() || self.dt <= 0.0 {
            0.0
        } else {
            1.0 / (self.dt * self.steps_per_frame as f32)
        }
    }

    /// 总物理步数（含预热帧）。
    pub fn total_steps(&self) -> u64 {
        u64::from(self.warmup_frames.saturating_add(self.frames)) * u64::from(self.steps_per_frame)
    }
}

/// 一帧的输出记录（可直接序列化成 `trace.jsonl` 的一行）。
#[derive(Debug, Clone, Copy)]
pub struct SequenceFrame {
    /// 输出帧序号（从 0 起，不含预热帧）。
    pub frame: u32,
    /// 累计物理步数（含预热帧）。
    pub steps: u64,
    /// 累计仿真时间（秒，含预热帧）。
    pub time: f32,
    /// 这一帧的渲染统计。
    pub stats: quench3d_render::RenderStats,
}

/// 什么都不做的游戏逻辑：让 [`render_sequence`] 只推进物理。
struct NoGame;

impl Game for NoGame {
    fn fixed_update(&mut self, engine: &mut Engine, dt: f32) {
        engine.world.step(dt);
    }
}

/// 无头逐帧推进并渲染（不需要窗口），**全程共用一个 GPU 上下文**。
///
/// 与 [`render_offscreen`] 的区别，也是它存在的理由：
///
/// 1. **只创建一次 GPU 设备**。`render_offscreen` 每次调用都会新建 headless 上下文，
///    逐帧调用会把设备初始化开销乘上帧数（实测约 1.1 秒/次 × 帧数，600 帧就是 11 分钟）。
/// 2. **真的推进物理与引擎时间**：每帧固定推进 `steps_per_frame` 步，不依赖墙钟，
///    所以同一场景两次运行的像素结果一致，适合录仿真视频与做 CI 回归。
/// 3. **预热帧**：先推进 `warmup_frames` 帧再开始输出，避免第一帧脏。
///
/// `on_frame` 每个输出帧回调一次（拿得到这一帧的像素与统计），可用来写 PNG、算哈希、
/// 喂给编码器。返回所有输出帧的记录。
///
/// ```
/// # use quench3d::prelude::*;
/// # fn demo() -> Result<(), String> {
/// let params = quench3d::SequenceParams {
///     frames: 3,
///     ..Default::default()
/// };
/// let frames = quench3d::render_sequence(
///     320,
///     240,
///     &EngineConfig::default(),
///     params,
///     |engine| {
///         let cube = engine.assets.unit_cube;
///         let mat = engine.assets.solid_material(
///             &mut engine.renderer,
///             &engine.gpu,
///             Vec4::new(0.9, 0.3, 0.2, 1.0),
///             0.1,
///             0.4,
///         );
///         engine.scene.add(cube, mat, Transform::IDENTITY);
///     },
///     |_engine, frame, _image| {
///         assert!(frame.time > 0.0);
///         Ok(())
///     },
/// )?;
/// assert_eq!(frames.len(), 3);
/// # Ok(())
/// # }
/// ```
pub fn render_sequence(
    width: u32,
    height: u32,
    config: &EngineConfig,
    params: SequenceParams,
    setup: impl FnOnce(&mut Engine),
    on_frame: impl FnMut(&mut Engine, SequenceFrame, &quench3d_render::ImageData) -> Result<(), String>,
) -> Result<Vec<SequenceFrame>, String> {
    render_sequence_with_game(width, height, config, params, &mut NoGame, setup, on_frame)
}

/// 与 [`render_sequence`] 相同，但驱动一个真实的 [`Game`]。
///
/// 帧体顺序与窗口帧循环保持一致（`update` → `fixed_update`×N → 物理同步
/// → `begin_frame`/`submit`/`draw_debug`/`Game::draw` → 渲染），区别是**步长固定**、
/// 且在无窗口环境下运行——这样游戏逻辑本身也能在 CI 里被真正执行。
///
/// 世界步进遵循与窗口循环相同的约定：由 [`Game::fixed_update`] 负责调用
/// `PhysicsWorld::step`；[`render_sequence`] 用的内部空实现会自动步进。
pub fn render_sequence_with_game<G: Game>(
    width: u32,
    height: u32,
    config: &EngineConfig,
    params: SequenceParams,
    game: &mut G,
    setup: impl FnOnce(&mut Engine),
    mut on_frame: impl FnMut(
        &mut Engine,
        SequenceFrame,
        &quench3d_render::ImageData,
    ) -> Result<(), String>,
) -> Result<Vec<SequenceFrame>, String> {
    let gpu = pollster::block_on(GpuContext::headless(
        width,
        height,
        GpuOptions {
            msaa_samples: config.msaa_samples,
            ..Default::default()
        },
    ))
    .map_err(|e| e.to_string())?;
    let mut engine = Engine::new(gpu, config);
    if params.dt > 0.0 && params.dt.is_finite() {
        engine.world.set_fixed_dt(params.dt);
    }
    setup(&mut engine);
    game.init(&mut engine);

    let frame_dt = params.dt * params.steps_per_frame as f32;
    let total = params.warmup_frames.saturating_add(params.frames);
    let mut out = Vec::with_capacity(params.frames as usize);
    let mut steps: u64 = 0;

    for i in 0..total {
        engine.input.begin_frame();
        engine.update_camera(frame_dt);
        game.update(&mut engine, frame_dt);
        // 与窗口循环同一条规则：暂停时物理与仿真时间都不推进。
        // （不暂停时行为与以前逐位一致，录制结果与 digest 不受影响。）
        if !engine.paused {
            for _ in 0..params.steps_per_frame {
                game.fixed_update(&mut engine, params.dt);
                steps += 1;
            }
        }
        engine.frame_count += 1;
        engine.last_dt = frame_dt;
        if !engine.paused {
            engine.time += frame_dt;
        }
        engine.renderer.set_time(engine.time);

        if engine.scene.auto_sync_physics {
            engine.scene.sync_from_physics(&engine.world);
        }
        // 与窗口循环同一个口径：动画按帧时间推进，暂停时不走。
        if !engine.paused {
            engine.scene.advance_animations_with_renderer(
                frame_dt,
                &mut engine.renderer,
                &engine.gpu,
            );
        }

        let camera = engine.camera();
        let lighting = engine.scene.lighting.clone();
        engine.renderer.begin_frame();
        engine.scene.submit(&mut engine.renderer);
        engine.draw_debug();
        game.draw(&mut engine);

        let image = engine
            .renderer
            .capture(&mut engine.gpu, width, height, &camera, &lighting)?;
        let stats = engine.renderer.stats();
        engine.stats = stats;

        if i >= params.warmup_frames {
            let record = SequenceFrame {
                frame: i - params.warmup_frames,
                steps,
                time: engine.time,
                stats,
            };
            on_frame(&mut engine, record, &image)?;
            out.push(record);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 查询探针：相机中心看什么就查到什么（F7 依赖它）。
    #[test]
    fn center_probe_hits_what_the_camera_looks_at() {
        use quench3d_math::vec3;
        use quench3d_physics::{BodyBuilder, Shape};
        let mut world = PhysicsWorld::new();
        let target = world.add_body(
            BodyBuilder::r#static(Shape::Box {
                half_extents: Vec3::splat(0.5),
            })
            .position(vec3(0.0, 0.0, 0.0))
            .build(),
        );
        let mut camera = Camera::perspective(1.0);
        camera.position = vec3(0.0, 0.0, 8.0);
        camera.look_at(Vec3::ZERO);
        let hit = center_probe(&camera, &world, QUERY_PROBE_MAX).expect("正对着盒子，应该打到");
        assert_eq!(hit.handle, target);
        // 注意：射线起点在近裁剪面上（不是相机位置），所以距离是"起点到 z=0.5 的盒面"
        let start = camera.ndc_ray(Vec2::ZERO).origin;
        assert!(
            (hit.distance - (start.z - 0.5)).abs() < 1e-3,
            "距离 {}（起点 z={}）",
            hit.distance,
            start.z
        );
        assert!((hit.point.z - 0.5).abs() < 1e-3, "命中点 {:?}", hit.point);
        assert!(
            hit.normal.dot(Vec3::Z) > 0.9,
            "法线朝相机：{:?}",
            hit.normal
        );

        // 看旁边（+X）：这条线上什么都没有
        camera.look_at(vec3(50.0, 0.0, 8.0));
        assert!(center_probe(&camera, &world, QUERY_PROBE_MAX).is_none());
        // 距离上限比命中距离短：也应当什么都没有
        camera.look_at(Vec3::ZERO);
        assert!(center_probe(&camera, &world, 3.0).is_none());
        assert!(center_probe(&camera, &world, 100.0).is_some());
    }

    /// 帧数/时长/帧率的口径必须自洽：视频时长 = 帧数 × 每帧步数 × 步长。
    #[test]
    fn sequence_params_are_self_consistent() {
        let p = SequenceParams {
            frames: 300,
            steps_per_frame: 1,
            dt: 1.0 / 60.0,
            warmup_frames: 0,
        };
        assert!((p.duration() - 5.0).abs() < 1e-4, "300 帧 @1/60 = 5 秒");
        assert!((p.fps() - 60.0).abs() < 1e-3);
        assert_eq!(p.total_steps(), 300);
    }

    /// 每帧多步 = 仿真更细/更慢，步数由 frames × steps_per_frame 决定。
    #[test]
    fn steps_per_frame_scales_simulation_not_frame_count() {
        let p = SequenceParams {
            frames: 120,
            steps_per_frame: 2,
            dt: 1.0 / 60.0,
            warmup_frames: 0,
        };
        assert_eq!(p.total_steps(), 240);
        assert!((p.duration() - 4.0).abs() < 1e-4, "4 秒仿真");
        assert!((p.fps() - 30.0).abs() < 1e-3, "2 步/帧 → 等价 30 fps");
    }

    /// 预热帧计入仿真步数，但不计入输出帧与时长。
    #[test]
    fn warmup_frames_are_simulated_but_not_output() {
        let p = SequenceParams {
            frames: 10,
            steps_per_frame: 1,
            dt: 1.0 / 60.0,
            warmup_frames: 5,
        };
        assert_eq!(p.total_steps(), 15);
        assert!((p.duration() - 10.0 / 60.0).abs() < 1e-5, "时长只算输出帧");
    }

    /// 退化参数不能产生 NaN/Inf 帧率（否则 ffmpeg 命令会写出垃圾）。
    #[test]
    fn degenerate_params_do_not_produce_nan() {
        let zero_dt = SequenceParams {
            frames: 1,
            steps_per_frame: 1,
            dt: 0.0,
            warmup_frames: 0,
        };
        assert_eq!(zero_dt.fps(), 0.0);
        assert_eq!(zero_dt.duration(), 0.0);

        let zero_steps = SequenceParams {
            frames: 1,
            steps_per_frame: 0,
            dt: 1.0 / 60.0,
            warmup_frames: 0,
        };
        assert_eq!(zero_steps.fps(), 0.0);
        assert_eq!(zero_steps.total_steps(), 0);

        let huge = SequenceParams {
            frames: u32::MAX,
            steps_per_frame: 2,
            dt: 1.0 / 60.0,
            warmup_frames: u32::MAX,
        };
        assert!(huge.total_steps() > 0, "饱和加法不能回绕成 0");
    }

    /// 暂停的语义：仿真时间冻结，但帧计数与 fps 照常更新。
    #[test]
    fn paused_freezes_simulation_time_but_not_frame_stats() {
        let (mut time, mut frames, mut last_dt, mut fps) = (0.0f32, 0u64, 0.0f32, 0.0f32);
        for _ in 0..10 {
            advance_clock(&mut time, &mut frames, &mut last_dt, &mut fps, 0.1, false);
        }
        assert!((time - 1.0).abs() < 1e-5, "未暂停时 time 应当累加：{time}");
        assert_eq!(frames, 10);
        assert!((fps - 10.0).abs() < 1e-3, "缺省首帧直接取瞬时帧率：{fps}");

        let frozen_time = time;
        for _ in 0..10 {
            advance_clock(&mut time, &mut frames, &mut last_dt, &mut fps, 0.1, true);
        }
        assert_eq!(time, frozen_time, "暂停时 time 必须完全不动");
        assert_eq!(frames, 20, "暂停时仍然渲染，帧计数要继续走");
        assert!((last_dt - 0.1).abs() < 1e-6);
        assert!((fps - 10.0).abs() < 1e-3, "fps 描述渲染开销，暂停时也更新");
    }
    #[test]
    fn joint_debug_lines_draw_anchors_connector_and_axis() {
        let mut lines = quench3d_render::DebugLines::new();
        let a = Vec3::new(1.0, 2.0, 0.0);
        let b = Vec3::new(3.0, 2.0, 0.0);
        push_joint_debug_lines(
            &mut lines,
            &[JointVisual {
                a,
                b,
                axis: Some(Vec3::Z),
            }],
        );
        assert!(lines.line_count() >= 4, "锚点 + 连线 + 轴应当至少 4 条线");
        assert_eq!(lines.vertices[0].position, a.to_array());
        assert_eq!(lines.vertices[1].position, b.to_array());
        let last = &lines.vertices[lines.vertices.len() - 1];
        let mid = (a + b) * 0.5;
        assert_eq!(last.position, (mid + Vec3::Z * 0.35).to_array());
    }
}
