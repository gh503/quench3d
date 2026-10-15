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

use crate::doc::Doc;
use crate::gizmo::{Gizmo, Mode as GizmoMode, Space as GizmoSpace};
use crate::ui::{self, Viewport};
use egui_wgpu::ScreenDescriptor;
use quench3d::prelude::*;
use quench3d::render::wgpu;
use quench3d::winit::application::ApplicationHandler;
use quench3d::winit::dpi::LogicalSize;
use quench3d::winit::event::{
    ElementState, MouseButton as WinitMouseButton, MouseScrollDelta, WindowEvent,
};
use quench3d::winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use quench3d::winit::keyboard::PhysicalKey;
use quench3d::winit::window::{Window, WindowId};
use std::sync::Arc;

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
        if self.keys_enabled {
            engine.handle_engine_keys(None);
        }
    }

    fn fixed_update(&mut self, engine: &mut Engine, dt: f32) {
        engine.world.step(dt);
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
    last_time: f64,
    first_frame_done: bool,
}

impl Inner {
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
        // 启动即载入：只有桌面端有真实文件系统，Web 端靠页面上的「打开」按钮上传。
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = &config.startup_scene {
            match crate::scene_io::load_file_into_doc(&mut engine, &mut doc, path) {
                Ok((objects, _)) => {
                    doc.file_path = path.display().to_string();
                    doc.selected = if objects > 0 { Some(0) } else { None };
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
        doc.reset_history(&engine);

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
            last_time: quench3d::clock::now_seconds(),
            first_frame_done: false,
        }
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
            );
            if requests.screenshot {
                self.screenshot();
            }
            if requests.save {
                self.save_scene();
            }
            if requests.load {
                self.load_scene();
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

        // 拾取：用这一帧的相机与视口矩形。
        if let Some(local) = self.viewport.click {
            let viewport = Vec2::new(
                self.viewport.rect.width().max(1.0),
                self.viewport.rect.height().max(1.0),
            );
            let ray = self.engine.camera().screen_ray(local, viewport);
            self.doc.selected = self.doc.pick(&self.engine, &ray);
        }

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
                }
                Err(e) => self.doc.status = format!("下载失败：{e}"),
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
                    self.doc.selected = if objects > 0 { Some(0) } else { None };
                    self.doc.capture_baseline(&self.engine);
                    self.doc.reset_history(&self.engine);
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

    /// 把浏览器上传的场景 JSON 装进引擎（Web 专用）。
    #[cfg(target_arch = "wasm32")]
    pub fn apply_scene_json(&mut self, text: &str) {
        match crate::scene_io::load_text_into_doc(&mut self.engine, &mut self.doc, text) {
            Ok((objects, path)) => {
                self.doc.selected = if objects > 0 { Some(0) } else { None };
                self.doc.file_path = path;
                self.doc.capture_baseline(&self.engine);
                self.doc.reset_history(&self.engine);
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
    #[cfg(target_arch = "wasm32")]
    pending: crate::PendingSlot,
}

impl EditorApp {
    /// 新建（此时还没窗口，等 `resumed`）。
    pub fn new(config: EditorConfig) -> Self {
        Self {
            config,
            inner: None,
            #[cfg(target_arch = "wasm32")]
            pending: crate::new_pending_slot(),
        }
    }

    fn window_attributes(&self) -> winit::window::WindowAttributes {
        let attrs = Window::default_attributes()
            .with_title(self.config.title.clone())
            .with_inner_size(LogicalSize::new(self.config.width, self.config.height));
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
                event_loop.exit();
                return;
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
                                _ => {}
                            }
                        }
                        if code == KeyCode::KeyF {
                            inner.doc.focus_selected(engine);
                            return;
                        }
                        // 正在拖手柄时 Esc = 取消这次拖拽，不能让引擎把它当成"退出"。
                        if code == KeyCode::Escape && inner.gizmo.is_dragging() {
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
                // 引擎内置快捷键（Tab/R/F3–F6/P/Esc）只在不打字时生效。
                if keys_enabled {
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
        if let Some(inner) = self.inner.as_ref() {
            inner.window.request_redraw();
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
