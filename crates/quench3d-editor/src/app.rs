//! 编辑器应用：窗口、输入、GPU、egui 与引擎的接线。
//!
//! 一帧的顺序（和「谁先谁后」有关，写在最前面）：
//!
//! 1. 先跑 egui 的界面（它可能改场景、改物理参数、请求存盘/截图）；
//! 2. 再推引擎一帧（[`Engine::tick`]：物理固定步长 + 提交绘制指令）；
//! 3. 取交换链帧 → 3D 渲染进这张图 → 同一张图上叠 egui → 呈现。
//!
//! 第 3 步是这个 crate 存在的理由：引擎自带的 `ForgeApp` 只能「3D 渲染 + 呈现」，
//! 中间插不进一层 UI。这里自己拿 `acquire_frame`，所以 UI 能浮在 3D 上面。

use crate::camera::CameraFeed;
use crate::doc::Doc;
use crate::gizmo::{Gizmo, Mode as GizmoMode, Pivot as GizmoPivot, Space as GizmoSpace};
use crate::ui::{self, Viewport};
use egui_wgpu::ScreenDescriptor;
use quench3d::prelude::*;
use quench3d::render::wgpu;
#[cfg(not(target_arch = "wasm32"))]
use quench3d::render::Projection;
use quench3d::winit::application::ApplicationHandler;
#[cfg(not(target_arch = "wasm32"))]
use quench3d::winit::dpi::LogicalPosition;
use quench3d::winit::dpi::LogicalSize;
use quench3d::winit::event::{
    ElementState, MouseButton as WinitMouseButton, MouseScrollDelta, WindowEvent,
};
use quench3d::winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use quench3d::winit::keyboard::PhysicalKey;
use quench3d::winit::window::{Window, WindowId};
use quench3d_cli::scene_doc::{ProbeDoc, ProbeShapeDoc};
use std::sync::Arc;

/// 自动退出钩子的环境变量名（**测试/调试专用**）。
///
/// 值是一个正整数毫秒数 `N`：窗口画起来之后再等 `N` 毫秒，编辑器就自动**投一个
/// 真的 `WindowEvent::CloseRequested`** —— 与用户点关闭按钮时合成器发来的是**同一个
/// 事件、同一个 match 分支**，所以它走的是同一条退出路径，而不是另写一条。
///
/// 存在的理由：退出路径**只有真跑 GUI 才会经过**（CI 上没有显示器、没有真 GPU），
/// 而 `gdb -batch` 里没人能点关闭按钮 —— 这个钩子让「关窗口 → 退出」可以被
/// 无人值守地复现（见 `docs/overnight_report.md` 的复现配方）。
///
/// 与仓库既有的 `QUENCH3D_GPU_TESTS` 同源：**环境变量显式 opt-in**。
/// 默认关闭（不设这个变量 = 永远不自动退出），而且它**只影响退出时机**，
/// 不影响退出路径本身、不影响渲染、不影响任何其它行为。
#[cfg(not(target_arch = "wasm32"))]
pub const EXIT_AFTER_MS_ENV: &str = "QUENCH3D_EDITOR_EXIT_AFTER_MS";

/// 解析 [`EXIT_AFTER_MS_ENV`] 的值；`None`/空白/非数字/`0` → `None`（不自动退出）。
///
/// 为什么抽成纯函数：**不需要显卡就能测**。这段解析一旦错，无人值守的复现就会
/// 失去意义 —— 要么永远不退出（跑满 `timeout`，看起来像「没崩」），要么起表过早
/// （第一帧还没画完就退，根本没走到真正的退出析构）。
#[cfg(not(target_arch = "wasm32"))]
pub fn parse_exit_after_ms(raw: Option<&str>) -> Option<u64> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    match raw.parse::<u64>() {
        Ok(ms) if ms > 0 => Some(ms),
        // `0` 是「显式要求不自动退出」，不是错误，所以不告警。
        Ok(_) => None,
        Err(_) => {
            log::warn!("{EXIT_AFTER_MS_ENV}={raw:?} 不是非负整数，忽略（不自动退出）");
            None
        }
    }
}

/// 编辑器配置。
#[derive(Clone, Debug)]
pub struct EditorConfig {
    /// 窗口标题。
    pub title: String,
    /// 初始宽度（逻辑像素）。
    pub width: u32,
    /// 初始高度（逻辑像素）。
    pub height: u32,
    /// MSAA 采样数。
    pub msaa_samples: u32,
    /// 垂直同步。
    pub vsync: bool,
    /// Web 端画布元素 id（桌面端忽略）。
    pub canvas_id: String,
    /// 渲染器选项。
    pub renderer: quench3d::render::RendererOptions,
    /// 启动时是否摆好默认场景。
    pub demo_scene: bool,
    /// 启动时要打开的场景文件（桌面端；给了它就不再摆默认场景）。
    pub startup_scene: Option<std::path::PathBuf>,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            title: "Quench3D 编辑器".to_string(),
            width: 1280,
            height: 800,
            msaa_samples: 4,
            vsync: true,
            canvas_id: "quench3d-canvas".to_string(),
            renderer: quench3d::render::RendererOptions::default(),
            demo_scene: true,
            startup_scene: None,
        }
    }
}

/// 引擎回调：物理推进 + 相机。
///
/// 相机由这里接管（`Engine::auto_camera = false`），因为编辑器要自己决定
/// 「这一下左键拖动是转视角、还是以后要变成拖物体」。
#[derive(Default)]
struct EditorGame {
    /// 左键拖动是否当作转视角（在视口里按下才算）。
    orbit_with_left: bool,
    /// egui 是否想要键盘（输入框聚焦时，快捷键要让路）。
    keys_enabled: bool,
}

impl Game for EditorGame {
    fn update(&mut self, engine: &mut Engine, dt: f32) {
        let orbit_drag = self.orbit_with_left
            || engine.input.mouse_button_down(MouseButton::Middle)
            || (engine.input.alt && engine.input.mouse_button_down(MouseButton::Left));
        let camera_input = engine.input.camera_input(dt, orbit_drag);
        match engine.camera_mode {
            CameraMode::Orbit => engine.orbit.update(&camera_input),
            CameraMode::Fly => engine.fly.update(&camera_input),
        }
    }

    fn fixed_update(&mut self, engine: &mut Engine, dt: f32) {
        engine.world.step(dt);
    }
}

/// 把场景探针画成线框：球 = 三个正交圆，盒 = 12 条边。
///
/// 纯函数（只碰 `DebugLines` 与文档数据），不依赖 GPU，方便单测线段数量。
fn draw_probes(lines: &mut quench3d::render::DebugLines, probes: &[ProbeDoc]) {
    let color = quench3d::prelude::Vec4::new(0.2, 0.9, 0.9, 1.0);
    const SEGMENTS: usize = 24;
    for probe in probes {
        let center =
            quench3d::prelude::Vec3::new(probe.position[0], probe.position[1], probe.position[2]);
        match probe.shape {
            ProbeShapeDoc::Sphere => {
                let r = probe.radius.max(0.01);
                for axis in 0..3 {
                    let mut points = Vec::with_capacity(SEGMENTS);
                    for i in 0..SEGMENTS {
                        let t = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
                        let (s, c) = (t.sin(), t.cos());
                        points.push(
                            center
                                + match axis {
                                    0 => quench3d::prelude::Vec3::new(0.0, c * r, s * r),
                                    1 => quench3d::prelude::Vec3::new(c * r, 0.0, s * r),
                                    _ => quench3d::prelude::Vec3::new(c * r, s * r, 0.0),
                                },
                        );
                    }
                    lines.closed_polyline(&points, color);
                }
            }
            ProbeShapeDoc::Box => {
                let size = probe.size.unwrap_or([1.0, 1.0, 1.0]);
                let half = quench3d::prelude::Vec3::new(size[0], size[1], size[2]) * 0.5;
                lines.aabb(
                    &quench3d::prelude::Aabb::from_center_half_extents(center, half),
                    color,
                );
            }
        }
    }
}

/// 选中物体的世界包围盒（只读场景/网格信息，不碰物理也不改材质）。
fn selection_bounds(engine: &Engine, selected: &[usize]) -> Vec<quench3d::prelude::Aabb> {
    let mut out = Vec::with_capacity(selected.len());
    for &index in selected {
        let Some(object) = engine.scene.objects.get(index) else {
            continue;
        };
        if !object.visible {
            continue;
        }
        let Some((_, _, local)) = engine.renderer.mesh_info(object.mesh) else {
            continue;
        };
        let world = engine.scene.world_transform(index);
        out.push(local.transformed(&world.matrix()));
    }
    out
}

/// 选中高亮：给每个选中物体画橙色世界包围盒。
fn draw_selection_boxes(
    lines: &mut quench3d::render::DebugLines,
    bounds: &[quench3d::prelude::Aabb],
    color: quench3d::prelude::Vec4,
) {
    for bounds in bounds {
        lines.aabb(bounds, color);
    }
}

/// 初始化完成后的全部状态。
struct Inner {
    window: Arc<Window>,
    engine: Engine,
    game: EditorGame,
    doc: Doc,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    viewport: Viewport,
    /// 视口里的变换手柄（模式 / 坐标系 / 正在拖拽的状态）。
    gizmo: Gizmo,
    /// 摄像头动捕读端（桌面端轮询 latest.*；Web 端空实现）。
    camera: CameraFeed,
    /// 崩溃恢复自动保存（桌面写文件，Web 写 localStorage）。
    autosave: crate::autosave::AutoSave,
    last_time: f64,
    first_frame_done: bool,
    /// 窗口移动过、但还没写进 layout.json。
    #[cfg(not(target_arch = "wasm32"))]
    window_pos_dirty: bool,
    /// 上次写布局文件的时间（移动窗口时节流，避免每个像素都写盘）。
    #[cfg(not(target_arch = "wasm32"))]
    last_layout_save: std::time::Instant,
    /// 测试/演示钩子：跑到第 N 帧自动点一次「截图」（`QUENCH3D_EDITOR_SCREENSHOT_AFTER_FRAMES`）。
    #[cfg(not(target_arch = "wasm32"))]
    screenshot_at_frame: Option<u64>,
}

#[cfg(not(target_arch = "wasm32"))]
fn preview_camera(current: Camera, camera_doc: &quench3d_cli::scene_doc::CameraDoc) -> Camera {
    let mut camera = current;
    camera.set_aspect(16.0 / 9.0);
    if let Some(fov) = camera_doc.fov_deg {
        camera.fov_y = fov.to_radians();
    }
    if let Some(near) = camera_doc.near {
        camera.near = near;
    }
    if let Some(far) = camera_doc.far {
        camera.far = far;
    }
    if let Some(position) = camera_doc.position {
        camera.position = Vec3::new(position[0], position[1], position[2]);
    }
    if let Some(target) = camera_doc.look_at {
        camera.look_at(Vec3::new(target[0], target[1], target[2]));
    }
    if let Some(height) = camera_doc.orthographic_height {
        camera.projection = Projection::Orthographic { height };
    }
    camera
}

#[cfg(not(target_arch = "wasm32"))]
fn capture_preview(
    engine: &mut Engine,
    camera: &Camera,
    width: u32,
    height: u32,
) -> Result<quench3d::render::ImageData, String> {
    let lighting = engine.scene.lighting.clone();
    let Engine { renderer, gpu, .. } = engine;
    renderer.capture_preserving_stats(gpu, width, height, camera, &lighting)
}

#[cfg(not(target_arch = "wasm32"))]
fn refresh_pip(engine: &mut Engine, doc: &mut Doc, ctx: &egui::Context) -> bool {
    let Some(name) = doc.pip_camera.clone() else {
        return false;
    };
    if engine.frame_count < doc.pip_last_frame.saturating_add(doc.pip_interval.max(1)) {
        return false;
    }
    let Some(camera_doc) = doc
        .base_doc()
        .and_then(|base| base.cameras.get(&name).cloned())
    else {
        return false;
    };
    doc.pip_last_frame = engine.frame_count;
    let camera = preview_camera(engine.camera(), &camera_doc);
    let (width, height) = doc.pip_size();
    let image = match capture_preview(engine, &camera, width, height) {
        Ok(image) => image,
        Err(e) => {
            log::warn!("画中画捕获失败：{e}");
            return false;
        }
    };
    let color = egui::ColorImage::from_rgba_unmultiplied(
        [image.width as usize, image.height as usize],
        &image.rgba,
    );
    if let Some(texture) = doc.pip_texture.as_mut() {
        texture.set(color, egui::TextureOptions::LINEAR);
    } else {
        doc.pip_texture =
            Some(ctx.load_texture("quench3d-pip", color, egui::TextureOptions::LINEAR));
    }
    true
}

impl Inner {
    /// 画中画：用第二台命名相机在 320×180 下重画一遍并回读成 egui 纹理。
    #[cfg(not(target_arch = "wasm32"))]
    fn capture_pip(&mut self) {
        let _ = refresh_pip(&mut self.engine, &mut self.doc, &self.egui_ctx);
    }

    fn new(window: Arc<Window>, gpu: GpuContext, config: &EditorConfig) -> Self {
        let engine_config = EngineConfig {
            title: config.title.clone(),
            width: config.width,
            height: config.height,
            renderer: config.renderer,
            msaa_samples: config.msaa_samples,
            vsync: config.vsync,
            canvas_id: config.canvas_id.clone(),
            ..Default::default()
        };
        let mut engine = Engine::new(gpu, &engine_config);
        engine.set_window(window.clone());
        // 相机由 EditorGame 接管；开箱先暂停，让人先看清场景再决定要不要跑。
        engine.auto_camera = false;
        engine.paused = true;

        let mut doc = Doc::new();
        doc.layout = crate::layout::Layout::load();
        // 启动即载入：只有桌面端有真实文件系统，Web 端靠页面上的「打开」按钮上传。
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = &config.startup_scene {
            match crate::scene_io::load_file_into_doc(&mut engine, &mut doc, path) {
                Ok((objects, _)) => {
                    doc.file_path = path.display().to_string();
                    doc.select_only(if objects > 0 { Some(0) } else { None });
                    doc.status = format!("已载入 {}（{objects} 个物体）", path.display());
                }
                Err(e) => doc.status = format!("载入 {} 失败：{e}", path.display()),
            }
            doc.capture_baseline(&engine);
        } else if config.demo_scene {
            doc.build_default_scene(&mut engine);
        }
        #[cfg(target_arch = "wasm32")]
        if config.demo_scene {
            doc.build_default_scene(&mut engine);
        }
        // 无头验收/演示用：启动时直接选一台命名相机做画中画。普通用户仍从下拉框选。
        #[cfg(not(target_arch = "wasm32"))]
        if let Ok(name) = std::env::var("QUENCH3D_EDITOR_PIP_CAMERA") {
            if !name.trim().is_empty() {
                doc.pip_camera = Some(name);
            }
        }
        doc.reset_history(&engine);
        // 上次崩溃留下的自动保存：先挂出来，等用户点「恢复 / 丢弃」。
        // 桌面读文件，Web 读 localStorage（同一个版本化的键）。
        let autosave = crate::autosave::AutoSave::new(
            crate::autosave::AutoSave::default_path(),
            crate::autosave::DEFAULT_INTERVAL,
        );
        if let Some(text) = crate::autosave::AutoSave::read_recovery() {
            doc.recovery = Some(text);
            doc.status = "发现上次崩溃前的自动保存，可点「恢复」或「丢弃」".to_string();
        }

        let egui_ctx = egui::Context::default();
        install_fonts(&egui_ctx);
        egui_ctx.set_visuals(egui::Visuals::dark());
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let egui_renderer = egui_wgpu::Renderer::new(
            &engine.gpu.device,
            engine.gpu.format,
            egui_wgpu::RendererOptions {
                msaa_samples: 1,
                depth_stencil_format: None,
                dithering: false,
                ..Default::default()
            },
        );

        Self {
            window,
            engine,
            game: EditorGame {
                orbit_with_left: false,
                keys_enabled: true,
            },
            doc,
            egui_ctx,
            egui_state,
            egui_renderer,
            viewport: Viewport::default(),
            gizmo: Gizmo::default(),
            camera: CameraFeed::new(),
            autosave,
            last_time: quench3d::clock::now_seconds(),
            first_frame_done: false,
            #[cfg(not(target_arch = "wasm32"))]
            window_pos_dirty: false,
            #[cfg(not(target_arch = "wasm32"))]
            last_layout_save: std::time::Instant::now(),
            #[cfg(not(target_arch = "wasm32"))]
            screenshot_at_frame: std::env::var("QUENCH3D_EDITOR_SCREENSHOT_AFTER_FRAMES")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|n| *n > 0),
        }
    }

    /// 把当前窗口位置写进布局文件；失败只记日志（布局是偏好，不该阻止编辑）。
    #[cfg(not(target_arch = "wasm32"))]
    fn persist_layout(&mut self) {
        if let Err(e) = self.doc.layout.save() {
            log::warn!("写布局失败（窗口位置不会记住）：{e}");
        }
        self.last_layout_save = std::time::Instant::now();
        self.window_pos_dirty = false;
    }

    /// 渲染一帧（含 UI）。
    fn draw(&mut self) -> Result<(), quench3d::render::FrameError> {
        #[cfg(target_arch = "wasm32")]
        if let Some((name, text)) = crate::scene_io::take_pending_upload() {
            self.apply_scene_json(&text);
            self.doc.file_path = name;
        }
        let now = quench3d::clock::now_seconds();
        let dt = ((now - self.last_time) as f32).clamp(0.0, 0.1);
        self.last_time = now;

        // 恢复点存在时，F9/F10 等价于点「恢复 / 丢弃」。放在 tick 之前：`tick` 会
        // `begin_frame` 清掉本帧按键，这里读到的才是刚按下的。
        if self.doc.recovery.is_some() && !self.egui_ctx.egui_wants_keyboard_input() {
            if self
                .engine
                .input
                .key_pressed(quench3d::winit::keyboard::KeyCode::F9)
            {
                self.restore_recovery();
            } else if self
                .engine
                .input
                .key_pressed(quench3d::winit::keyboard::KeyCode::F10)
            {
                self.discard_recovery();
            }
        }

        // ---- 1. 界面：先跑 UI，让它有机会改场景 ----
        let raw_input = self.egui_state.take_egui_input(&self.window);
        let ctx = self.egui_ctx.clone();
        let mut full_output = ctx.run_ui(raw_input, |ui| {
            let requests = ui::panels(
                ui,
                &mut self.engine,
                &mut self.doc,
                &mut self.viewport,
                &mut self.gizmo,
                &mut self.camera,
            );
            if requests.screenshot {
                self.screenshot();
            }
            if requests.save {
                self.save_scene();
            }
            if requests.export_gltf {
                self.export_gltf();
            }
            if requests.export_glb {
                self.export_glb();
            }
            if requests.load {
                self.load_scene();
            }
            if requests.restore_recovery {
                self.restore_recovery();
            }
            if requests.discard_recovery {
                self.discard_recovery();
            }
        });
        let keys_enabled = !self.egui_ctx.egui_wants_keyboard_input();
        self.egui_state
            .handle_platform_output(&self.window, full_output.platform_output.clone());
        let paint_jobs = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        for (id, deltas) in &full_output.textures_delta.set {
            for delta in deltas {
                self.egui_renderer.update_texture(
                    &self.engine.gpu.device,
                    &self.engine.gpu.queue,
                    *id,
                    delta,
                );
            }
        }

        // ---- 2. 引擎一帧：物理 + 提交绘制指令 ----
        let mut game = std::mem::take(&mut self.game);
        game.keys_enabled = keys_enabled;
        self.engine.tick(&mut game, dt);
        self.game = game;
        // 摄像头骨架是**每帧重建**的调试线段：begin_frame 已清空，tick 之后、
        // render_to 之前加入。停更 2 秒会自动消失，不会留下僵尸骨架。
        self.camera.add_lines(&mut self.engine.renderer.lines);
        if self.doc.show_probes {
            if let Some(base) = self.doc.base_doc() {
                draw_probes(&mut self.engine.renderer.lines, &base.probes);
            }
        }
        if self.doc.show_selection {
            let selected = self.doc.selection();
            let bounds = selection_bounds(&self.engine, &selected);
            let hover = self
                .doc
                .hovered
                .and_then(|index| selection_bounds(&self.engine, &[index]).pop());
            let lines = &mut self.engine.renderer.lines;
            draw_selection_boxes(lines, &bounds, Vec4::new(1.0, 0.55, 0.1, 1.0));
            if let Some(hover) = hover {
                draw_selection_boxes(lines, &[hover], Vec4::new(0.25, 0.85, 1.0, 1.0));
            }
        }
        if self.gizmo.pivot == GizmoPivot::Cursor {
            let c = self.gizmo.cursor;
            let color = quench3d::prelude::Vec4::new(1.0, 0.75, 0.2, 1.0);
            self.engine.renderer.lines.line(
                c - quench3d::prelude::Vec3::X * 0.6,
                c + quench3d::prelude::Vec3::X * 0.6,
                color,
            );
            self.engine.renderer.lines.line(
                c - quench3d::prelude::Vec3::Y * 0.6,
                c + quench3d::prelude::Vec3::Y * 0.6,
                color,
            );
            self.engine.renderer.lines.line(
                c - quench3d::prelude::Vec3::Z * 0.6,
                c + quench3d::prelude::Vec3::Z * 0.6,
                color,
            );
        }
        self.autosave_tick();
        #[cfg(target_arch = "wasm32")]
        publish_ui_debug_state(UiDebugState {
            keys_enabled: self.game.keys_enabled,
            paused: self.engine.paused,
            grid: self.engine.renderer.options.grid,
            shadows: self.engine.renderer.options.shadows,
            lod: self.engine.renderer.options.auto_lod,
            shadow_instances: self.engine.renderer.stats().shadow_instances,
            shadow_strength: self.engine.scene.lighting.shadow_strength,
            sun_cast: self
                .engine
                .scene
                .lighting
                .sun
                .map(|s| s.cast_shadows)
                .unwrap_or(false),
            camera_mode: self.engine.camera_mode,
            gizmo_mode: self.gizmo.mode,
            gizmo_pivot: self.gizmo.pivot.label(),
            cursor: self.gizmo.cursor.to_array(),
            contacts: self.engine.debug.physics_contacts,
            bounds: self.engine.debug.body_bounds,
            query: self.engine.debug.query_probe,
            bones: self.engine.debug.bones,
            joints: self.engine.debug.joints,
            probes: self.doc.show_probes,
            selection: self.doc.show_selection,
            hovered: self.doc.hovered.unwrap_or(usize::MAX),
            tonemap: self.engine.renderer.options.tonemap.label(),
            recovery: self.doc.recovery.is_some(),
            objects: self.engine.scene.len(),
            selected: self.doc.selection().len(),
            parents: self
                .engine
                .scene
                .objects
                .iter()
                .filter(|o| o.parent.is_some())
                .count(),
            camera_pose: self.camera.has_pose(),
            camera_world: self.camera.has_world_pose(),
        });

        // ---- 3. 取帧 → 3D → egui 叠上去 → 呈现 ----
        let frame = self.engine.gpu.acquire_frame()?;
        if self.engine.gpu.take_reconfigure_flag() {
            self.engine.gpu.reconfigure();
        }
        let texture_view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let size = (frame.texture.width(), frame.texture.height());
        self.engine.render_to(&texture_view, size);
        #[cfg(not(target_arch = "wasm32"))]
        self.capture_pip();

        // 拾取：用这一帧的相机与视口矩形。
        if let Some(local) = self.viewport.click {
            let viewport = Vec2::new(
                self.viewport.rect.width().max(1.0),
                self.viewport.rect.height().max(1.0),
            );
            let ray = self.engine.camera().screen_ray(local, viewport);
            let picked = self.doc.pick(&self.engine, &ray);
            if self.viewport.additive {
                if let Some(index) = picked {
                    self.doc.toggle_selected(index);
                }
            } else {
                self.doc.select_only(picked);
            }
        }
        // 悬停拾取：只做高亮，不改选择；指针在 gizmo/框选/转视角时不抢。
        self.doc.hovered = if self.viewport.gizmo_active
            || self.viewport.dragging
            || self.viewport.box_select_start.is_some()
        {
            None
        } else {
            self.viewport.hover.and_then(|local| {
                let viewport = Vec2::new(
                    self.viewport.rect.width().max(1.0),
                    self.viewport.rect.height().max(1.0),
                );
                let ray = self.engine.camera().screen_ray(local, viewport);
                self.doc.pick(&self.engine, &ray)
            })
        };

        let screen = ScreenDescriptor {
            size_in_pixels: [size.0, size.1],
            pixels_per_point: full_output.pixels_per_point,
        };
        let mut encoder =
            self.engine
                .gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("quench3d-editor-ui"),
                });
        let user_buffers = self.egui_renderer.update_buffers(
            &self.engine.gpu.device,
            &self.engine.gpu.queue,
            &mut encoder,
            &paint_jobs,
            &screen,
        );
        {
            let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("quench3d-editor-ui-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &texture_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        // 3D 已经画在这张图上了：Load，不要 Clear。
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let mut pass = pass.forget_lifetime();
            self.egui_renderer.render(&mut pass, &paint_jobs, &screen);
        }
        self.engine.gpu.queue.submit(
            user_buffers
                .into_iter()
                .chain(std::iter::once(encoder.finish())),
        );
        self.engine.gpu.queue.present(frame);
        for id in &full_output.textures_delta.free {
            self.egui_renderer.free_texture(id);
        }
        // egui 的 texture delta 是「必须确认收到」的：Drop 时若还留着未处理项就会
        // panic（epaint 的 debug_assert）。上面已经逐条交给渲染器了，这里按约定
        // 把它标记为已处理 —— 少了这一行，编辑器能画出一帧然后立刻崩。
        full_output.textures_delta.clear();

        #[cfg(not(target_arch = "wasm32"))]
        if let Some(frame) = self.screenshot_at_frame {
            if self.engine.frame_count >= frame {
                self.screenshot_at_frame = None;
                self.screenshot();
            }
        }

        if !self.first_frame_done {
            self.first_frame_done = true;
            // 字体自检放在这里：egui 的字体要等第一遍 `run_ui` 之后才可用
            // （早于它调 fonts_mut 会 panic：No fonts available until first call to
            // Context::run()）。egui 自带字体一个中文字形都没有，界面全靠挂在字体栈
            // 末尾的那份中文子集，所以把它变成可观测事实：换字体、升级 egui 之后
            // 看日志就知道中文还能不能显示，不用盯着屏幕猜。
            let cjk_ok = self.egui_ctx.fonts_mut(|f| {
                f.has_glyphs(
                    &egui::FontId::proportional(14.0),
                    "场景物体物理材质密度摩擦弹性暂停播放单步重置添加删除清空网格阴影坐标轴",
                )
            });
            log::info!("编辑器字体：中文子集可用 = {cjk_ok}");
            log::info!(
                "编辑器第一帧已渲染：{}x{}（{}）",
                size.0,
                size.1,
                self.engine.gpu.adapter_info.name
            );
            quench3d::web_console::mark_ready();
        }
        Ok(())
    }

    /// 截图：桌面写文件，Web 走浏览器下载。
    fn screenshot(&mut self) {
        let (width, height) = self.engine.gpu.size();
        let (w, h) = (width.max(1), height.max(1));
        #[cfg(not(target_arch = "wasm32"))]
        {
            let path = std::path::PathBuf::from("quench3d-editor-screenshot.png");
            match self.engine.screenshot(w, h, &path) {
                Ok(()) => {
                    self.doc.status = format!("截图已保存：{}", path.display());
                }
                Err(e) => self.doc.status = format!("截图失败：{e}"),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (w, h);
            self.doc.status = "Web 端截图请用浏览器自带的截图功能".to_string();
        }
    }

    /// 保存场景（同一个 `quench3d.scene` 格式，CLI 能直接渲染这份文件）。
    fn save_scene(&mut self) {
        let saved = match crate::scene_io::scene_to_json(&self.engine, &self.doc) {
            Ok(saved) => saved,
            Err(e) => {
                self.doc.status = format!("保存失败：{e}");
                return;
            }
        };
        let json = saved.json;
        let mut note = String::new();
        if !saved.skipped.is_empty() {
            note = format!(
                "（跳过 {} 个无法用场景格式表达的物体：{}）",
                saved.skipped.len(),
                saved.skipped.join("、")
            );
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let path = std::path::PathBuf::from(self.doc.file_path.clone());
            match std::fs::write(&path, json.as_bytes()) {
                Ok(()) => {
                    self.doc.status =
                        format!("已保存 {}（{} 字节）{note}", path.display(), json.len());
                    self.autosave.clear();
                    self.doc.needs_autosave = false;
                }
                Err(e) => self.doc.status = format!("保存失败：{e}"),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            match crate::scene_io::download_text(&self.doc.file_path, &json) {
                Ok(()) => {
                    self.doc.status =
                        format!("已下载 {}（{} 字节）{note}", self.doc.file_path, json.len());
                    // 已经拿到一份用户主动下载的场景，恢复点没必要再出现。
                    self.autosave.clear();
                    self.doc.needs_autosave = false;
                }
                Err(e) => self.doc.status = format!("下载失败：{e}"),
            }
        }
    }

    /// 导出单文件 glTF 2.0（当前场景旁放一个同名 `.gltf`；Web 走下载）。
    fn export_gltf(&mut self) {
        let saved = match crate::scene_io::scene_to_gltf(&self.engine, &self.doc) {
            Ok(saved) => saved,
            Err(e) => {
                self.doc.status = format!("导出 glTF 失败：{e}");
                return;
            }
        };
        let mut note = String::new();
        if !saved.skipped.is_empty() {
            note = format!("（跳过：{}）", saved.skipped.join("、"));
        }
        let path = export_path_for(&self.doc.file_path, &mut self.doc.export_path, "gltf");
        #[cfg(not(target_arch = "wasm32"))]
        {
            match std::fs::write(&path, saved.json.as_bytes()) {
                Ok(()) => {
                    self.doc.status = format!(
                        "已导出 {}（{} 个物体 / {} 三角形 / {} 条动画 / {} 字节）{note}",
                        path.display(),
                        saved.objects,
                        saved.triangles,
                        saved.animations,
                        saved.json.len()
                    );
                }
                Err(e) => self.doc.status = format!("导出 glTF 失败：{e}"),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("scene.gltf");
            match crate::scene_io::download_text(name, &saved.json) {
                Ok(()) => {
                    self.doc.status = format!(
                        "已下载 {name}（{} 个物体 / {} 三角形 / {} 条动画 / {} 字节）{note}",
                        saved.objects,
                        saved.triangles,
                        saved.animations,
                        saved.json.len()
                    );
                }
                Err(e) => self.doc.status = format!("下载 glTF 失败：{e}"),
            }
        }
    }

    /// 导出二进制 GLB（当前场景旁放一个同名 `.glb`；Web 走下载）。
    fn export_glb(&mut self) {
        let saved = match crate::scene_io::scene_to_glb(&self.engine, &self.doc) {
            Ok(saved) => saved,
            Err(e) => {
                self.doc.status = format!("导出 GLB 失败：{e}");
                return;
            }
        };
        let mut note = String::new();
        if !saved.skipped.is_empty() {
            note = format!("（跳过：{}）", saved.skipped.join("、"));
        }
        let path = export_path_for(&self.doc.file_path, &mut self.doc.export_path, "glb");
        #[cfg(not(target_arch = "wasm32"))]
        {
            match std::fs::write(&path, &saved.bytes) {
                Ok(()) => {
                    self.doc.status = format!(
                        "已导出 {}（{} 个物体 / {} 三角形 / {} 条动画 / {} 字节）{note}",
                        path.display(),
                        saved.objects,
                        saved.triangles,
                        saved.animations,
                        saved.bytes.len()
                    );
                }
                Err(e) => self.doc.status = format!("导出 GLB 失败：{e}"),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("scene.glb");
            match crate::scene_io::download_bytes(name, &saved.bytes) {
                Ok(()) => {
                    self.doc.status = format!(
                        "已下载 {name}（{} 个物体 / {} 三角形 / {} 条动画 / {} 字节）{note}",
                        saved.objects,
                        saved.triangles,
                        saved.animations,
                        saved.bytes.len()
                    );
                }
                Err(e) => self.doc.status = format!("下载 GLB 失败：{e}"),
            }
        }
    }

    /// 打开场景。
    fn load_scene(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let path = std::path::PathBuf::from(self.doc.file_path.clone());
            match crate::scene_io::load_file_into_doc(&mut self.engine, &mut self.doc, &path) {
                Ok((objects, _)) => {
                    self.doc
                        .select_only(if objects > 0 { Some(0) } else { None });
                    self.doc.capture_baseline(&self.engine);
                    self.doc.reset_history(&self.engine);
                    // 用户主动打开了别的场景：旧恢复点不再代表当前编辑，提示也收起。
                    self.doc.recovery = None;
                    self.doc.needs_autosave = false;
                    self.autosave.clear();
                    self.doc.status = format!("已载入 {}（{objects} 个物体）", path.display());
                }
                Err(e) => self.doc.status = format!("载入失败：{e}"),
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.doc.status = "选择文件…".to_string();
            crate::scene_io::pick_file_and_load(self.doc.file_path.clone());
        }
    }

    /// 把自动保存的内容装进引擎（不覆盖原场景文件/不自动下载）。
    fn restore_recovery(&mut self) {
        let Some(text) = self.doc.recovery.clone() else {
            return;
        };
        match crate::scene_io::load_text_into_doc(&mut self.engine, &mut self.doc, &text) {
            Ok((objects, _)) => {
                self.doc.recovery = None;
                self.doc
                    .select_only(if objects > 0 { Some(0) } else { None });
                self.doc.capture_baseline(&self.engine);
                self.doc.reset_history(&self.engine);
                // 恢复出来的场景与启动场景不同：允许下一帧写一个新恢复点。
                self.doc.needs_autosave = true;
                self.autosave.clear();
                self.doc.status = format!("已恢复自动保存（{objects} 个物体；尚未写回磁盘）");
            }
            Err(e) => {
                // 恢复失败时保留内容，用户还能再试或改完手动处理。
                self.doc.status = format!("恢复自动保存失败：{e}");
            }
        }
    }

    /// 丢弃自动保存。
    fn discard_recovery(&mut self) {
        self.doc.recovery = None;
        self.autosave.clear();
        self.doc.status = "已丢弃自动保存".to_string();
    }

    /// 每帧调一次；内部先判时间，只有到点才序列化整个场景。
    fn autosave_tick(&mut self) {
        // 刚打开、没有任何编辑的默认场景不写恢复点：否则 Web 每次刷新都会
        // 弹一次"发现自动保存"。有撤销栈或刚恢复过才值得写。
        if !self.doc.history.can_undo() && !self.doc.needs_autosave {
            return;
        }
        if !self.autosave.due() {
            return;
        }
        let Ok(saved) = crate::scene_io::scene_to_json(&self.engine, &self.doc) else {
            self.autosave.defer();
            return;
        };
        // 有无法用场景格式表达的物体时宁可不写：恢复一份"悄悄丢了模型"的场景，
        // 比没有自动保存更危险。`defer` 保证不会因此每帧都重试。
        if !saved.skipped.is_empty() {
            self.autosave.defer();
            return;
        }
        match self.autosave.tick(&saved.json) {
            Ok(true) => self.doc.needs_autosave = false,
            Ok(false) => {}
            Err(e) => {
                self.autosave.defer();
                log::warn!("自动保存失败：{e}");
            }
        }
    }

    /// 把浏览器上传的场景 JSON 装进引擎（Web 专用）。
    #[cfg(target_arch = "wasm32")]
    pub fn apply_scene_json(&mut self, text: &str) {
        match crate::scene_io::load_text_into_doc(&mut self.engine, &mut self.doc, text) {
            Ok((objects, path)) => {
                self.doc
                    .select_only(if objects > 0 { Some(0) } else { None });
                self.doc.file_path = path;
                self.doc.capture_baseline(&self.engine);
                self.doc.reset_history(&self.engine);
                self.doc.recovery = None;
                self.doc.needs_autosave = false;
                self.autosave.clear();
                self.doc.status = format!("已载入场景（{objects} 个物体）");
            }
            Err(e) => self.doc.status = format!("载入失败：{e}"),
        }
    }
}

/// 把中文字体装进 egui。
///
/// egui 自带的字体（Ubuntu-Light + 两套图标字体）**一个汉字都没有**，
/// 不装这个的话界面全是豆腐块。字体来自 Droid Sans Fallback（Apache-2.0），
/// 用 fontTools 取了「源码里出现过的字 + GB2312 一级汉字」的子集，
/// 体积约 560 KB；放在回退链最后，拉丁字母仍然用 egui 自带字体。
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "quench3d-cjk".to_string(),
        std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
            "../assets/ui-font.ttf"
        ))),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("quench3d-cjk".to_string());
    }
    ctx.set_fonts(fonts);
}

/// 编辑器应用（桌面与 Web 共用；只有入口不同）。
pub struct EditorApp {
    config: EditorConfig,
    inner: Option<Inner>,
    /// 自动退出钩子的等待毫秒数（`None` = 钩子关闭）。见 [`EXIT_AFTER_MS_ENV`]。
    #[cfg(not(target_arch = "wasm32"))]
    exit_after_ms: Option<u64>,
    /// 钩子的截止时刻。第一帧画起来（`inner` 就绪）时才起表 —— 窗口创建与 GPU
    /// 初始化在慢机器/软件 Vulkan 上可能要好几秒，从进程启动起表会算不准。
    #[cfg(not(target_arch = "wasm32"))]
    exit_deadline: Option<std::time::Instant>,
    #[cfg(target_arch = "wasm32")]
    pending: crate::PendingSlot,
}

/// 仅当 URL 带 `ui_debug=1` 时，把编辑器关键状态挂到 `<body data-quench3d-ui>` 上。
///
/// 这不是产品功能，而是**自动化探针的观测孔**：Web 端控件全画在 canvas 里，
/// 没有 DOM 可查；没有这个属性，测试只能靠截图猜"按键到底有没有生效"。
/// 默认不开（不污染页面），只有测试 URL 显式要求时才写。
#[cfg(target_arch = "wasm32")]
struct UiDebugState {
    keys_enabled: bool,
    paused: bool,
    grid: bool,
    shadows: bool,
    /// 自动 LOD 开关（只影响之后上传的网格；用于 UI 状态对账）。
    lod: bool,
    shadow_instances: u32,
    shadow_strength: f32,
    sun_cast: bool,
    camera_mode: quench3d::prelude::CameraMode,
    gizmo_mode: GizmoMode,
    gizmo_pivot: &'static str,
    cursor: [f32; 3],
    contacts: bool,
    bounds: bool,
    query: bool,
    bones: bool,
    joints: bool,
    /// 场景探针线框开关。
    probes: bool,
    /// 选中包围盒开关。
    selection: bool,
    /// 悬停命中的物体下标（`usize::MAX` = 没命中）。
    hovered: usize,
    /// 当前色调映射算子的标签。
    tonemap: &'static str,
    /// 是否挂着自动保存恢复点（Web 端 localStorage / 桌面文件）。
    recovery: bool,
    /// 当前场景里的物体数（含粒子/水面代理；用于恢复测试对账）。
    objects: usize,
    /// 当前选中数（含主选与附加）。
    selected: usize,
    /// 有父物体的场景物体数（场景树拖放改父的观测孔）。
    parents: usize,
    /// Web 摄像头是否已有外部 JS 注入的 2D 骨架。
    camera_pose: bool,
    /// Web 摄像头是否已有外部 JS 注入的 3D 世界点。
    camera_world: bool,
}

#[cfg(target_arch = "wasm32")]
fn publish_ui_debug_state(state: UiDebugState) {
    let UiDebugState {
        keys_enabled,
        paused,
        grid,
        shadows,
        lod,
        shadow_instances,
        shadow_strength,
        sun_cast,
        camera_mode,
        gizmo_mode,
        gizmo_pivot,
        cursor,
        contacts,
        bounds,
        query,
        bones,
        joints,
        probes,
        selection,
        hovered,
        tonemap,
        recovery,
        objects,
        selected,
        parents,
        camera_pose,
        camera_world,
    } = state;
    let Some(window) = web_sys::window() else {
        return;
    };
    let search = window.location().search().unwrap_or_default();
    if !search.contains("ui_debug=1") {
        return;
    }
    let Some(body) = window.document().and_then(|d| d.body()) else {
        return;
    };
    let state = format!(
        "keys_enabled={keys_enabled};paused={paused};grid={grid};shadows={shadows};\
lod={lod};shadow_instances={shadow_instances};shadow_strength={shadow_strength};sun_cast={sun_cast};camera={camera_mode:?};gizmo={gizmo_mode:?};pivot={gizmo_pivot};cursor={cursor:?};contacts={contacts};bounds={bounds};query={query};bones={bones};joints={joints};probes={probes};selection={selection};hovered={hovered};tonemap={tonemap};recovery={recovery};objects={objects};selected={selected};parents={parents};camera_pose={camera_pose};camera_world={camera_world}"
    );
    let _ = body.set_attribute("data-quench3d-ui", &state);
}

/// 调试探针：记录最近一条 winit 键盘事件（仅 `?ui_debug=1`）。
#[cfg(target_arch = "wasm32")]
fn publish_last_key(event: &winit::event::KeyEvent) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let search = window.location().search().unwrap_or_default();
    if !search.contains("ui_debug=1") {
        return;
    }
    let Some(body) = window.document().and_then(|d| d.body()) else {
        return;
    };
    let state = format!(
        "physical={:?};logical={:?};state={:?};repeat={}",
        event.physical_key, event.logical_key, event.state, event.repeat
    );
    let _ = body.set_attribute("data-quench3d-last-key", &state);
}

/// 导出目标：优先用选择器给的一次性路径；否则场景同名换扩展名。
fn export_path_for(
    file_path: &str,
    once: &mut Option<std::path::PathBuf>,
    extension: &str,
) -> std::path::PathBuf {
    once.take()
        .unwrap_or_else(|| std::path::Path::new(file_path).with_extension(extension))
}

impl EditorApp {
    /// 新建（此时还没窗口，等 `resumed`）。
    pub fn new(config: EditorConfig) -> Self {
        Self {
            config,
            inner: None,
            #[cfg(not(target_arch = "wasm32"))]
            exit_after_ms: parse_exit_after_ms(std::env::var(EXIT_AFTER_MS_ENV).ok().as_deref()),
            #[cfg(not(target_arch = "wasm32"))]
            exit_deadline: None,
            #[cfg(target_arch = "wasm32")]
            pending: crate::new_pending_slot(),
        }
    }

    fn window_attributes(&self) -> winit::window::WindowAttributes {
        let attrs = Window::default_attributes()
            .with_title(self.config.title.clone())
            .with_inner_size(LogicalSize::new(self.config.width, self.config.height));
        #[cfg(not(target_arch = "wasm32"))]
        let attrs = apply_window_position(attrs, &crate::layout::Layout::load());
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

    fn gpu_options(&self) -> GpuOptions {
        GpuOptions {
            msaa_samples: self.config.msaa_samples,
            vsync: self.config.vsync,
            ..Default::default()
        }
    }

    /// 平台无关的窗口事件处理。
    fn handle_window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        // egui 先看一遍：它消费掉的事件（点在面板上、在输入框里打字）就不再喂给引擎，
        // 否则会出现「点一下面板，相机也转了一下」这种事。
        let response = inner.egui_state.on_window_event(&inner.window, &event);
        let consumed = response.consumed;
        let pointer_in_viewport = inner.viewport.contains_pointer();
        let forward = !consumed || pointer_in_viewport;

        match &event {
            WindowEvent::CloseRequested => {
                #[cfg(not(target_arch = "wasm32"))]
                if inner.window_pos_dirty {
                    inner.persist_layout();
                }
                event_loop.exit();
                return;
            }
            WindowEvent::Moved(position) => {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    inner.doc.layout.set_window_position(position.x, position.y);
                    inner.window_pos_dirty = true;
                }
                #[cfg(target_arch = "wasm32")]
                let _ = position;
            }
            WindowEvent::Resized(size) => {
                inner.engine.gpu.resize(size.width, size.height);
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                let size = inner.window.inner_size();
                inner.engine.gpu.resize(size.width, size.height);
            }
            WindowEvent::RedrawRequested => {
                match inner.draw() {
                    Ok(()) => {}
                    Err(e) => {
                        // 交换链失效：重配一下，下一帧继续。
                        log::debug!("本帧未画：{e}");
                        let (w, h) = inner.engine.gpu.size();
                        inner.engine.gpu.resize(w, h);
                        inner.engine.gpu.reconfigure();
                    }
                }
                return;
            }
            _ => {}
        }

        if !forward {
            return;
        }
        let keys_enabled = inner.game.keys_enabled;
        let engine = &mut inner.engine;
        match &event {
            WindowEvent::Focused(focused) => {
                engine.input.focused = *focused;
                if !focused {
                    engine.input.on_focus_lost();
                    inner.game.orbit_with_left = false;
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                #[cfg(target_arch = "wasm32")]
                publish_last_key(event);
                if let PhysicalKey::Code(code) = event.physical_key {
                    // 编辑器快捷键先于引擎：Ctrl+Z/Ctrl+Shift+Z/Ctrl+Y 是编辑器的，
                    // F 聚焦选中。引擎拿到的仍然是它认识的那些键（不会重复处理）。
                    if event.state == ElementState::Pressed {
                        if engine.input.ctrl {
                            match code {
                                KeyCode::KeyZ => {
                                    if engine.input.shift {
                                        inner.doc.redo(engine);
                                    } else {
                                        inner.doc.undo(engine);
                                    }
                                    return;
                                }
                                KeyCode::KeyY => {
                                    inner.doc.redo(engine);
                                    return;
                                }
                                KeyCode::KeyC => {
                                    inner.doc.copy_selected(engine);
                                    return;
                                }
                                KeyCode::KeyV => {
                                    inner.doc.paste_clipboard(engine);
                                    return;
                                }
                                _ => {}
                            }
                        }
                        if code == KeyCode::KeyF {
                            inner.doc.focus_selected(engine);
                            return;
                        }
                        // H：选中包围盒（编辑器自己的显示开关）。
                        if code == KeyCode::KeyH && !engine.input.ctrl {
                            inner.doc.show_selection = !inner.doc.show_selection;
                            inner.doc.status = if inner.doc.show_selection {
                                "选中包围盒：显示".to_string()
                            } else {
                                "选中包围盒：隐藏".to_string()
                            };
                            return;
                        }
                        // B：场景探针线框（编辑器自己的显示开关，不写回场景）。
                        if code == KeyCode::KeyB && !engine.input.ctrl {
                            inner.doc.show_probes = !inner.doc.show_probes;
                            inner.doc.status = if inner.doc.show_probes {
                                "场景探针：显示".to_string()
                            } else {
                                "场景探针：隐藏".to_string()
                            };
                            return;
                        }
                        // 正在拖手柄时 Esc = 取消这次拖拽，不能让引擎把它当成"退出"。
                        if code == KeyCode::Escape && inner.gizmo.is_dragging() {
                            return;
                        }
                        if code == KeyCode::KeyK {
                            if let Some(center) = inner.doc.selection_center(engine) {
                                inner.gizmo.cursor = center;
                            }
                            inner.gizmo.pivot = GizmoPivot::Cursor;
                            inner.doc.status = format!(
                                "3D 光标：({:.2}, {:.2}, {:.2})",
                                inner.gizmo.cursor.x, inner.gizmo.cursor.y, inner.gizmo.cursor.z
                            );
                            return;
                        }
                        // W/E/R 切手柄模式、L 切坐标系：只在轨道相机下生效。
                        // 飞行相机要用 WASD/QE，那时这些键归相机；R 那时也归引擎的"聚焦场景"。
                        if engine.camera_mode == CameraMode::Orbit && !engine.input.ctrl {
                            match code {
                                KeyCode::KeyW => {
                                    inner.gizmo.mode = GizmoMode::Translate;
                                    return;
                                }
                                KeyCode::KeyE => {
                                    inner.gizmo.mode = GizmoMode::Rotate;
                                    return;
                                }
                                KeyCode::KeyR => {
                                    inner.gizmo.mode = GizmoMode::Scale;
                                    return;
                                }
                                KeyCode::KeyL => {
                                    inner.gizmo.space = match inner.gizmo.space {
                                        GizmoSpace::World => GizmoSpace::Local,
                                        GizmoSpace::Local => GizmoSpace::World,
                                    };
                                    let label = inner.gizmo.space.label();
                                    inner.doc.status = format!("坐标系：{label}");
                                    return;
                                }
                                _ => {}
                            }
                        }
                    }
                    match event.state {
                        ElementState::Pressed => engine.input.on_key_down(code),
                        ElementState::Released => engine.input.on_key_up(code),
                    }
                }
                // 引擎内置快捷键在**按下事件**里处理：`tick` 开头会 `begin_frame()`
                // 清掉 `keys_pressed`，等 `Game::update` 再读就已经晚了。只处理
                // Pressed（且忽略 auto-repeat）保证一次按下只切换一次；抬起不再
                // 调 `handle_engine_keys`，否则同帧内的 down/up 会互相抵消。
                if event.state == ElementState::Pressed && !event.repeat && keys_enabled {
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
                        ElementState::Pressed => {
                            engine.input.on_mouse_down(b);
                            // 在视口里按下左键 = 开始「转视角」；点一下（没拖动）
                            // 会被 egui 认成 click，那时光标没动，转视角也看不出。
                            if b == MouseButton::Left
                                && pointer_in_viewport
                                && !inner.viewport.gizmo_active
                                && !engine.input.shift
                                && !engine.input.ctrl
                            {
                                inner.game.orbit_with_left = true;
                            }
                        }
                        ElementState::Released => {
                            engine.input.on_mouse_up(b);
                            if b == MouseButton::Left {
                                inner.game.orbit_with_left = false;
                            }
                        }
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
                    MouseScrollDelta::LineDelta(_, y) => *y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 50.0,
                };
                engine.input.on_scroll(scroll);
            }
            _ => {}
        }
        if engine.should_exit {
            event_loop.exit();
        }
    }
}

/// 把 `layout.json` 里的窗口坐标装进 winit 的窗口属性（桌面）。
///
/// 单独抽出来是为了在不创建窗口、不碰显示服务器的前提下测试「文件 → 窗口属性」
/// 这一段；真正的窗口放置仍由窗口系统决定。
#[cfg(not(target_arch = "wasm32"))]
fn apply_window_position(
    attrs: quench3d::winit::window::WindowAttributes,
    layout: &crate::layout::Layout,
) -> quench3d::winit::window::WindowAttributes {
    match (layout.window_x, layout.window_y) {
        (Some(x), Some(y)) => attrs.with_position(LogicalPosition::new(f64::from(x), f64::from(y))),
        _ => attrs,
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl ApplicationHandler for EditorApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.inner.is_some() {
            return;
        }
        let window = match event_loop.create_window(self.window_attributes()) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("创建窗口失败：{e}");
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        let options = self.gpu_options();
        let win = window.clone();
        let gpu = match pollster::block_on(GpuContext::from_candidates(w, h, options, |inst| {
            inst.create_surface(win.clone())
                .map(Some)
                .map_err(|e| quench3d::render::GpuInitError::Surface(format!("{e}")))
        })) {
            Ok(gpu) => gpu,
            Err(e) => {
                log::error!("GPU 初始化失败：{e}");
                event_loop.exit();
                return;
            }
        };
        log::info!(
            "编辑器启动：{} ({:?}) {}x{} MSAA x{}",
            gpu.adapter_info.name,
            gpu.adapter_info.backend,
            w,
            h,
            gpu.msaa_samples
        );
        self.inner = Some(Inner::new(window, gpu, &self.config));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        self.handle_window_event(event_loop, event);
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(inner) = self.inner.as_mut() {
            inner.window.request_redraw();
            // 拖动窗口时 `Moved` 可能每帧都来；节流到最多 1 秒写一次布局。
            if inner.window_pos_dirty
                && inner.last_layout_save.elapsed() >= std::time::Duration::from_secs(1)
            {
                inner.persist_layout();
            }
        }
        self.poll_exit_hook(_event_loop);
    }

    /// 事件循环即将退出 —— 在这里**按正确顺序**释放窗口 / GPU / 剪贴板。
    ///
    /// 这段看着多余，但删掉它编辑器关窗口必崩 SIGSEGV（退出码 139）。动之前请读完：
    ///
    /// **根因**：`winit::event_loop::EventLoop::run_app(self, app)` 是按值**吃掉**
    /// EventLoop 的（`winit-0.30.13/src/event_loop.rs:264`）。于是 winit 自己的 Wayland
    /// 连接在 `run_app` **内部**就析构了；而 `run()` 里的 `EditorApp` 是
    /// `event_loop.run_app(&mut EditorApp::new(config))?` 里的**临时值**，要到整条语句
    /// 结束才析构 —— 也就是**晚于** winit 的连接。`egui-winit` 的剪贴板
    /// （`smithay-clipboard` 0.7.3，由 `egui-winit` 的 `clipboard` 特性拉进来）包的是
    /// winit 那个 `wl_display*`（`Backend::from_foreign_display`，**不持有所有权**）。
    /// 它的 worker 线程析构 `PrimarySelectionManagerState` 时会
    /// `zwp_primary_selection_device_manager_v1.destroy()` → `wl_proxy_destroy(proxy)`，
    /// 而 `proxy->display` 指向的 `wl_display` 已经被 winit `disconnect`/释放掉了 →
    /// 释放后使用 → SIGSEGV。崩溃点固定是 worker.rs:101 的 `State` 析构。
    ///
    /// **为什么是这里**：`exiting` 是 winit 唯一保证「事件循环即将退出、但连接仍然有效」
    /// 时回调的地方 —— Wayland 后端在 `pump_events` 里派发 `Event::LoopExiting`
    /// （`platform_impl/linux/wayland/event_loop/mod.rs:231`），此时 `self.connection`
    /// 还在作用域内；而且它之后紧接着一次 `roundtrip()`，winit 自己的注释就写着
    /// 「如果应用刚刚丢掉窗口，得确保最后的请求发给了合成器」——
    /// 也就是说「在 `exiting` 里丢窗口」正是它设计的用法。
    ///
    /// **为什么不用别的写法**：
    /// * 升级依赖 —— 本仓库永远 `--offline`，而 cargo 缓存里 `smithay-clipboard` **只有
    ///   `0.7.3`**（就是 `Cargo.lock` 里这个），本地 registry 索引里的**最大**版本也是
    ///   `0.7.3` —— 离线根本没有可升的版本。见 `docs/production_readiness.md` §15.4。
    /// * `std::process::exit(0)` 跳过析构 —— 是**绕过**而不是**修好**这个顺序问题：
    ///   GPU/Vulkan 一起失去正常拆解路径；而且 `std::process::exit` 在 wasm32 上不存在，
    ///   还得多一层 `#[cfg]` 门控。既然 winit 给了一个正确的钩子，就用这个。
    /// * 捕获 / 忽略 SIGSEGV —— 明令禁止的掩盖手段：真出了别的越界就再也看不见了。
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        // 正常关闭 = 用户知情地离开；删掉恢复文件，下次启动才不会再问。
        // 崩溃/强杀不会走到这里，恢复文件因此只会在真正需要时出现。
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(inner) = self.inner.as_mut() {
            inner.autosave.clear();
        }
        if !self.needs_early_release() {
            return;
        }
        if self.inner.take().is_some() {
            log::info!(
                "事件循环退出：已按序释放窗口/GPU/剪贴板（理由见 EditorApp::exiting 的注释）"
            );
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl EditorApp {
    /// 这次退出要不要在 `exiting` 里提前释放 `Inner`？
    ///
    /// **只有 Wayland 要。** 判据就是 `egui-winit` 自己造 `smithay-clipboard` 的判据：
    /// 它只在拿到 [`RawDisplayHandle::Wayland`] 时才建那个对象
    /// （`egui-winit-0.37.0/src/clipboard.rs` 的 `init_smithay_clipboard`），
    /// 也才会去包 winit 的裸 `wl_display*`。X11 下根本没有这个对象，
    /// 也就没有这个析构顺序问题。
    ///
    /// 反过来，**在 X11 上提前丢窗口是有代价的**：winit 0.30.13 的 X11 退出路径本身
    /// 就带着一个与本次修复无关的 `BadWindow` panic（见 `docs/production_readiness.md`），
    /// 提前在 `exiting` 里丢窗口会把它从"多数情况 panic 退出（101）"变成
    /// "几乎必然二次 panic → abort（134）"。既然 X11 上修不到任何东西，
    /// 就不要去动它的既有行为。
    ///
    /// 用**显示句柄**而不是环境变量来判断：`WAYLAND_DISPLAY`/`WAYLAND_SOCKET` 只反映
    /// winit 的选路，嵌套合成器等场景下未必与实际句柄一致；显示句柄才是权威，
    /// 而且和 `egui-winit` 用的是同一个值。
    fn needs_early_release(&self) -> bool {
        use quench3d::winit::raw_window_handle::{HasDisplayHandle as _, RawDisplayHandle};
        let Some(inner) = self.inner.as_ref() else {
            return false;
        };
        let window: &quench3d::winit::window::Window = inner.window.as_ref();
        matches!(
            window.display_handle().map(|handle| handle.as_raw()),
            Ok(RawDisplayHandle::Wayland(_))
        )
    }

    /// 自动退出钩子（见 [`EXIT_AFTER_MS_ENV`]）：窗口就绪后等够毫秒数，就往事件
    /// 处理里**投一个真的 [`WindowEvent::CloseRequested`]**。
    ///
    /// 为什么不是直接 `event_loop.exit()`：投 `CloseRequested` 走的是**用户点窗口
    /// 关闭按钮时合成器发来的那条一模一样的事件**、一模一样的 match 分支，
    /// 所以这个钩子验证的不是"另一条路"，而是同一条路的无人值守版本。
    ///
    /// 默认关闭：没设环境变量时第一行就返回。
    fn poll_exit_hook(&mut self, event_loop: &ActiveEventLoop) {
        let Some(ms) = self.exit_after_ms else {
            return;
        };
        // 窗口还没起来（创建窗口 / GPU 初始化中）就不起表，否则慢机器上
        // 「3 秒后退出」可能在第一帧之前就到点，等于没测到退出路径。
        if self.inner.is_none() {
            return;
        }
        let deadline = *self.exit_deadline.get_or_insert_with(|| {
            std::time::Instant::now() + std::time::Duration::from_millis(ms)
        });
        if std::time::Instant::now() >= deadline {
            // 只投一次：投完就把钩子关掉（否则每次 `about_to_wait` 都会再投一次）。
            self.exit_after_ms = None;
            log::info!("{EXIT_AFTER_MS_ENV}={ms}：自动投递 CloseRequested（= 点关闭按钮那条路）");
            self.handle_window_event(event_loop, WindowEvent::CloseRequested);
        }
    }
}

/// Web 端：窗口与 GPU 初始化必须异步完成。
#[cfg(target_arch = "wasm32")]
impl ApplicationHandler for EditorApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.inner.is_some() || self.pending.borrow().is_some() {
            return;
        }
        quench3d::web_console::set_progress("正在创建渲染面…");
        let window = match event_loop.create_window(self.window_attributes()) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                quench3d::web_console::report_fatal(&format!("创建窗口失败：{e}"));
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        let options = self.gpu_options();
        let slot = self.pending.clone();
        let win = window.clone();
        wasm_bindgen_futures::spawn_local(async move {
            // 后端顺序由页面探好（见 web/index.html）：浏览器里 navigator.gpu 存在
            // 不等于有可用适配器，而画布一旦走过 webgpu 上下文就再也拿不到 webgl2。
            let candidates = quench3d::render::GpuContext::candidates_from_hint(
                crate::web_backend_hint().as_deref(),
            );
            let result =
                GpuContext::from_candidates_with(&candidates, w, h, options, move |inst| {
                    inst.create_surface(win.clone())
                        .map(Some)
                        .map_err(|e| quench3d::render::GpuInitError::Surface(format!("{e}")))
                })
                .await;
            *slot.borrow_mut() = Some(result.map(|gpu| (gpu, window)));
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        self.poll_pending(event_loop);
        self.handle_window_event(event_loop, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.poll_pending(event_loop);
        if let Some(inner) = self.inner.as_ref() {
            inner.window.request_redraw();
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl EditorApp {
    /// 收 Web 端异步初始化的结果。
    fn poll_pending(&mut self, event_loop: &ActiveEventLoop) {
        let result = self.pending.borrow_mut().take();
        match result {
            Some(Ok((gpu, window))) => {
                log::info!(
                    "编辑器启动：{} ({:?})",
                    gpu.adapter_info.name,
                    gpu.adapter_info.backend
                );
                self.inner = Some(Inner::new(window, gpu, &self.config));
            }
            Some(Err(e)) => {
                quench3d::web_console::report_fatal(&format!(
                    "GPU 初始化失败：{e}\n（浏览器需要 WebGPU，或可用的 WebGL2。\
                     强制后端：网址后加 ?backend=gl 或 ?backend=webgpu 再刷新）"
                ));
                event_loop.exit();
            }
            None => {}
        }
    }
}

/// 桌面端入口。
#[cfg(not(target_arch = "wasm32"))]
pub fn run(config: EditorConfig) -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.run_app(&mut EditorApp::new(config))?;
    Ok(())
}

/// Web 端入口。
#[cfg(target_arch = "wasm32")]
pub fn run(config: EditorConfig) -> Result<(), Box<dyn std::error::Error>> {
    use quench3d::winit::platform::web::EventLoopExtWebSys;
    console_error_panic_hook::set_once();
    quench3d::web_console::install_logger();
    log::info!(
        "Quench3D 编辑器 Web 启动：{}x{}",
        config.width,
        config.height
    );
    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.spawn_app(EditorApp::new(config));
    Ok(())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn layout_position_reaches_window_attributes() {
        let layout = crate::layout::Layout {
            window_x: Some(120),
            window_y: Some(-40),
            ..crate::layout::Layout::default()
        };
        let attrs = apply_window_position(Window::default_attributes(), &layout);
        assert_eq!(
            attrs.position,
            Some(quench3d::winit::dpi::Position::Logical(
                LogicalPosition::new(120.0, -40.0)
            ))
        );
        let none = apply_window_position(
            Window::default_attributes(),
            &crate::layout::Layout::default(),
        );
        assert_eq!(none.position, None, "没有记录位置时不要硬塞 (0,0)");
    }
}

#[cfg(test)]
mod probe_draw_tests {
    use super::*;

    #[test]
    fn selection_boxes_draw_twelve_edges_each() {
        let bounds = [
            quench3d::prelude::Aabb::new(
                quench3d::prelude::Vec3::ZERO,
                quench3d::prelude::Vec3::ONE,
            ),
            quench3d::prelude::Aabb::new(
                quench3d::prelude::Vec3::splat(-2.0),
                quench3d::prelude::Vec3::splat(-1.0),
            ),
        ];
        let mut lines = quench3d::render::DebugLines::new();
        draw_selection_boxes(
            &mut lines,
            &bounds,
            quench3d::prelude::Vec4::new(1.0, 0.55, 0.1, 1.0),
        );
        assert_eq!(lines.line_count(), 24);
    }

    #[test]
    fn probes_draw_three_circles_and_one_box() {
        let probes = vec![
            ProbeDoc {
                name: "门".to_string(),
                shape: ProbeShapeDoc::Sphere,
                position: [0.0, 1.0, 0.0],
                radius: 2.0,
                size: None,
                watch: Vec::new(),
            },
            ProbeDoc {
                name: "房间".to_string(),
                shape: ProbeShapeDoc::Box,
                position: [5.0, 0.0, 0.0],
                radius: 1.0,
                size: Some([2.0, 3.0, 4.0]),
                watch: Vec::new(),
            },
        ];
        let mut lines = quench3d::render::DebugLines::new();
        draw_probes(&mut lines, &probes);
        assert_eq!(lines.line_count(), 3 * 24 + 12);
    }
}

#[cfg(test)]
mod export_path_tests {
    use super::*;

    #[test]
    fn export_path_uses_picker_once_then_falls_back() {
        let mut once = None;
        assert_eq!(
            export_path_for("scene.json", &mut once, "glb"),
            std::path::PathBuf::from("scene.glb")
        );
        let mut once = Some(std::path::PathBuf::from("/tmp/custom.glb"));
        assert_eq!(
            export_path_for("scene.json", &mut once, "glb"),
            std::path::PathBuf::from("/tmp/custom.glb")
        );
        assert!(once.is_none(), "一次性路径必须被消费，不能影响下一次导出");
        assert_eq!(
            export_path_for("scene.json", &mut once, "gltf"),
            std::path::PathBuf::from("scene.gltf")
        );
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod preview_camera_tests {
    use super::*;

    /// 画中画完整链路：选相机 → 捕获 → 上传成 320×180 的 egui 纹理。
    #[test]
    fn pip_refresh_uploads_a_texture() {
        if std::env::var_os("QUENCH3D_GPU_TESTS").is_none() {
            return;
        }
        let gpu = match pollster::block_on(GpuContext::headless(
            64,
            64,
            quench3d::render::GpuOptions::default(),
        )) {
            Ok(gpu) => gpu,
            Err(e) => {
                eprintln!("跳过：没有可用的 wgpu 适配器（{e}）");
                return;
            }
        };
        let config = EngineConfig {
            width: 64,
            height: 64,
            msaa_samples: 1,
            ..Default::default()
        };
        let mut engine = Engine::new(gpu, &config);
        let mut doc = Doc::new();
        let text = r#"{
            "cameras": {
                "侧视": { "position": [6, 1, 0], "look_at": [0, 0, 0] }
            },
            "objects": [
                { "name": "箱", "shape": { "type": "box", "size": [1, 1, 1] } }
            ]
        }"#;
        crate::scene_io::load_text_into_doc(&mut engine, &mut doc, text).expect("场景应能装载");
        doc.pip_camera = Some("侧视".to_string());
        doc.pip_last_frame = 0;
        engine.frame_count = 100;
        engine.renderer.begin_frame();
        engine.scene.submit(&mut engine.renderer);
        let ctx = egui::Context::default();
        assert!(refresh_pip(&mut engine, &mut doc, &ctx), "画中画应捕获成功");
        assert!(
            !refresh_pip(&mut engine, &mut doc, &ctx),
            "同一帧不应重复回读（5 Hz 节流）"
        );
        engine.frame_count += 12;
        assert!(
            refresh_pip(&mut engine, &mut doc, &ctx),
            "过 12 帧后应允许下一次回读"
        );
        let texture = doc.pip_texture.as_ref().expect("应生成画中画纹理");
        assert_eq!(texture.size(), [320, 180], "默认画中画纹理应为 320×180");
        // 低分辨率：160×90；间隔改成 1 帧，下一帧就应重拍。
        doc.pip_resolution = 0;
        doc.pip_interval = 24;
        engine.frame_count += 12;
        assert!(
            !refresh_pip(&mut engine, &mut doc, &ctx),
            "24 帧间隔下只过 12 帧不应重拍"
        );
        engine.frame_count += 12;
        assert!(
            refresh_pip(&mut engine, &mut doc, &ctx),
            "满 24 帧应允许重拍"
        );
        assert_eq!(
            doc.pip_texture.as_ref().expect("低分辨率纹理").size(),
            [160, 90],
            "低分辨率画中画应为 160×90"
        );
        doc.pip_interval = 1;
        engine.frame_count += 1;
        assert!(
            refresh_pip(&mut engine, &mut doc, &ctx),
            "1 帧间隔下一帧重拍"
        );
        let texture_id = doc.pip_texture.as_ref().expect("应生成画中画纹理").id();
        let mut view = crate::ui::Viewport::default();
        let mut gizmo = crate::gizmo::Gizmo::default();
        let mut camera = crate::camera::CameraFeed::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1280.0, 900.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            let _ = crate::ui::panels(
                ui,
                &mut engine,
                &mut doc,
                &mut view,
                &mut gizmo,
                &mut camera,
            );
        });
        assert!(
            has_texture(&output.shapes, texture_id),
            "画中画纹理必须真的进入视口绘制列表"
        );
        output.textures_delta.clear();
    }

    fn has_texture(shapes: &[egui::epaint::ClippedShape], id: egui::TextureId) -> bool {
        shapes.iter().any(|clipped| match &clipped.shape {
            egui::Shape::Mesh(mesh) => mesh.texture_id == id,
            egui::Shape::Vec(inner) => inner.iter().any(|shape| match shape {
                egui::Shape::Mesh(mesh) => mesh.texture_id == id,
                _ => false,
            }),
            _ => false,
        })
    }

    /// 画中画回读不能改写主视图的状态栏统计。
    #[test]
    fn pip_capture_preserves_main_stats() {
        if std::env::var_os("QUENCH3D_GPU_TESTS").is_none() {
            return;
        }
        let gpu = match pollster::block_on(GpuContext::headless(
            64,
            64,
            quench3d::render::GpuOptions::default(),
        )) {
            Ok(gpu) => gpu,
            Err(e) => {
                eprintln!("跳过：没有可用的 wgpu 适配器（{e}）");
                return;
            }
        };
        let mut engine = Engine::new(
            gpu,
            &EngineConfig {
                width: 64,
                height: 64,
                msaa_samples: 1,
                ..Default::default()
            },
        );
        let mut doc = Doc::new();
        let text = r#"{
            "objects": [
                { "name": "箱", "shape": { "type": "box", "size": [1, 1, 1] } }
            ]
        }"#;
        crate::scene_io::load_text_into_doc(&mut engine, &mut doc, text).expect("场景应能装载");
        engine.renderer.begin_frame();
        engine.scene.submit(&mut engine.renderer);
        let camera = engine.camera();
        let lighting = engine.scene.lighting.clone();
        let Engine { renderer, gpu, .. } = &mut engine;
        renderer
            .capture(gpu, 64, 64, &camera, &lighting)
            .expect("主视图回读应成功");
        let before = format!("{:?}", engine.renderer.stats());
        let preview = capture_preview(&mut engine, &camera, 320, 180).expect("画中画回读应成功");
        assert_eq!(preview.width, 320);
        assert_eq!(
            before,
            format!("{:?}", engine.renderer.stats()),
            "画中画回读后主视图 RenderStats 必须原样保留"
        );
    }

    /// 第二相机不是摆设：同一帧里正面与侧面必须拍到不同画面，且不是空背景。
    #[test]
    fn second_camera_capture_differs_from_main() {
        if std::env::var_os("QUENCH3D_GPU_TESTS").is_none() {
            return;
        }
        let gpu = match pollster::block_on(GpuContext::headless(
            64,
            64,
            quench3d::render::GpuOptions::default(),
        )) {
            Ok(gpu) => gpu,
            Err(e) => {
                eprintln!("跳过：没有可用的 wgpu 适配器（{e}）");
                return;
            }
        };
        let config = EngineConfig {
            width: 64,
            height: 64,
            msaa_samples: 1,
            ..Default::default()
        };
        let mut engine = Engine::new(gpu, &config);
        let mesh = engine.assets.mesh(
            &mut engine.renderer,
            &engine.gpu,
            "$pip_box",
            &quench3d::render::mesh::MeshData::box_sized(Vec3::new(3.0, 0.5, 0.4)),
        );
        let material = engine.assets.material(
            &mut engine.renderer,
            &engine.gpu,
            "$pip_red",
            quench3d::render::material::MaterialDesc {
                base_color: Vec4::new(1.0, 0.0, 0.0, 1.0),
                unlit: true,
                ..Default::default()
            },
        );
        engine.scene.add(mesh, material, Transform::default());
        engine.renderer.begin_frame();
        engine.scene.submit(&mut engine.renderer);

        let current = engine.camera();
        let front = preview_camera(
            current,
            &quench3d_cli::scene_doc::CameraDoc {
                position: Some([0.0, 0.0, 6.0]),
                look_at: Some([0.0, 0.0, 0.0]),
                ..Default::default()
            },
        );
        let side = preview_camera(
            current,
            &quench3d_cli::scene_doc::CameraDoc {
                position: Some([6.0, 0.0, 0.0]),
                look_at: Some([0.0, 0.0, 0.0]),
                ..Default::default()
            },
        );
        let a = capture_preview(&mut engine, &front, 320, 180).expect("正面相机应能回读");
        let b = capture_preview(&mut engine, &side, 320, 180).expect("侧面相机应能回读");
        assert_eq!((a.width, a.height), (320, 180));
        let px = |img: &quench3d::render::ImageData, x: usize, y: usize| {
            let i = (y * img.width as usize + x) * 4;
            [
                img.rgba[i],
                img.rgba[i + 1],
                img.rgba[i + 2],
                img.rgba[i + 3],
            ]
        };
        let corner = px(&a, 0, 0);
        let a_diff = (0..a.rgba.len() / 4).any(|i| {
            let p = &a.rgba[i * 4..i * 4 + 4];
            p != corner
        });
        assert!(a_diff, "正面相机不该是纯背景");
        assert!(a.rgba != b.rgba, "正面与侧面必须是不同的画面");
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod texture_edit_tests {
    use super::*;

    /// 贴花/粒子贴图应用与清除都真的换材质、写回底稿；不是只改 UI 文案。
    #[test]
    fn decal_and_particle_texture_apply_and_clear() {
        if std::env::var_os("QUENCH3D_GPU_TESTS").is_none() {
            return;
        }
        let gpu = match pollster::block_on(GpuContext::headless(
            64,
            64,
            quench3d::render::GpuOptions::default(),
        )) {
            Ok(gpu) => gpu,
            Err(e) => {
                eprintln!("跳过：没有可用的 wgpu 适配器（{e}）");
                return;
            }
        };
        let config = EngineConfig {
            width: 64,
            height: 64,
            msaa_samples: 1,
            ..Default::default()
        };
        let mut engine = Engine::new(gpu, &config);
        let mut doc = Doc::new();
        let text = r#"{
            "objects": [
                { "name": "地面", "shape": { "type": "plane", "size": [4, 4, 1] } }
            ],
            "decals": [
                { "position": [0, 0.01, 0], "normal": [0, 1, 0], "size": [1, 1] }
            ],
            "particles": [
                { "count": 2, "position": [0, 1, 0], "lifetime": 1.0, "size": 0.1 }
            ]
        }"#;
        crate::scene_io::load_text_into_doc(&mut engine, &mut doc, text).expect("场景应能装载");

        let dir = std::env::temp_dir().join(format!(
            "quench3d-texture-edit-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).expect("应能建临时目录");
        let path = dir.join("tex.png");
        doc.file_path = dir
            .join("scene.quench3d.json")
            .to_string_lossy()
            .to_string();
        let mut png = Vec::new();
        quench3d::render::ImageData::solid([0, 255, 0, 255])
            .write_png(&mut png)
            .expect("应能编码 PNG");
        std::fs::write(&path, &png).expect("应能写临时 PNG");
        let path_text = "tex.png".to_string();

        let assets_dir = dir.join("assets");
        std::fs::create_dir_all(&assets_dir).expect("资源目录应可建");
        std::fs::copy(dir.join("tex.png"), assets_dir.join("tex2.png"))
            .expect("资源目录里的贴图应可复制");
        doc.base_doc_mut().expect("应有底稿").assets = Some(quench3d_cli::scene_doc::AssetsDoc {
            search_paths: vec![std::path::PathBuf::from("assets")],
        });
        assert_eq!(
            doc.resolve_texture_path("tex2.png"),
            assets_dir.join("tex2.png"),
            "assets.search_paths 里的相对路径应以场景目录为基准解析"
        );

        let ground_material = engine.scene.objects[0].material;
        let ground_texture = crate::ui::apply_object_texture(
            &mut engine,
            &mut doc,
            "地面",
            quench3d::render::MaterialTextureSlot::BaseColor,
            true,
            &path_text,
        )
        .expect("物体基础色贴图应应用成功");
        assert!(ground_texture.is_some());
        assert_eq!(
            engine
                .renderer
                .material(ground_material)
                .and_then(|m| m.base_color_texture),
            ground_texture
        );
        assert_eq!(
            doc.textures.get("地面").and_then(|t| t.base_color.clone()),
            Some(path_text.clone())
        );
        crate::ui::apply_object_texture(
            &mut engine,
            &mut doc,
            "地面",
            quench3d::render::MaterialTextureSlot::BaseColor,
            true,
            "",
        )
        .expect("物体基础色贴图应清除");
        assert!(engine
            .renderer
            .material(ground_material)
            .and_then(|m| m.base_color_texture)
            .is_none());
        assert_eq!(
            doc.textures.get("地面").and_then(|t| t.base_color.clone()),
            None
        );

        let decal_object = engine
            .scene
            .objects
            .iter()
            .position(|o| o.name == "贴花0")
            .expect("场景应生成贴花物体");
        let old_decal_material = engine.scene.objects[decal_object].material;
        crate::ui::replace_decal_texture(&mut engine, &mut doc, 0, &path_text)
            .expect("贴花贴图应应用成功");
        let new_decal_material = engine.scene.objects[decal_object].material;
        assert_ne!(
            old_decal_material, new_decal_material,
            "应用贴图必须换成新材质，不能改共享材质"
        );
        assert!(
            engine
                .renderer
                .material(new_decal_material)
                .and_then(|m| m.base_color_texture)
                .is_some(),
            "新材质必须真的带 BaseColor 贴图"
        );
        assert_eq!(
            doc.base_doc().and_then(|d| d.decals[0].texture.clone()),
            Some(path_text.clone())
        );

        crate::ui::replace_decal_texture(&mut engine, &mut doc, 0, "").expect("贴花贴图应清除");
        assert!(
            engine
                .renderer
                .material(engine.scene.objects[decal_object].material)
                .and_then(|m| m.base_color_texture)
                .is_none(),
            "清除后材质必须没有 BaseColor 贴图"
        );
        assert_eq!(
            doc.base_doc().and_then(|d| d.decals[0].texture.clone()),
            None
        );

        let old_particle_material = engine.scene.particles[0].material;
        crate::ui::replace_particle_texture(&mut engine, &mut doc, 0, &path_text)
            .expect("粒子贴图应应用成功");
        let new_particle_material = engine.scene.particles[0].material;
        assert_ne!(old_particle_material, new_particle_material);
        assert!(engine
            .renderer
            .material(new_particle_material)
            .and_then(|m| m.base_color_texture)
            .is_some());
        for object in &engine.scene.particles[0].objects {
            assert_eq!(
                engine.scene.objects[*object].material, new_particle_material,
                "所有粒子代理都必须换到新材质"
            );
        }
        assert_eq!(
            doc.base_doc().and_then(|d| d.particles[0].texture.clone()),
            Some(path_text)
        );

        crate::ui::replace_particle_texture(&mut engine, &mut doc, 0, "").expect("粒子贴图应清除");
        assert!(engine
            .renderer
            .material(engine.scene.particles[0].material)
            .and_then(|m| m.base_color_texture)
            .is_none());
        assert_eq!(
            doc.base_doc().and_then(|d| d.particles[0].texture.clone()),
            None
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
