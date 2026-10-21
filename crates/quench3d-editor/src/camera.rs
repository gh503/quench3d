//! 摄像头动捕的**读端**：轮询 `latest.json` + `latest.rgb`，在 egui 里显示
//! 2D 轮廓/骨架，并把 3D 世界点作为调试线段叠进 3D 视口。
//!
//! 采集/推理在 `tools/camera_mocap/camera_mocap.py` 里（MediaPipe + OpenCV），
//! 这里不链接任何 CV 库，也不启动子进程。两边只共享一个很小的文件协议：
//!
//! ```text
//! target/camera/latest.json   元数据 + 33 个 2D/3D 关键点 + 轮廓多边形
//! target/camera/latest.rgb    width*height*3 的 RGB24
//! target/camera/latest.depth  width*height 的 u8 归一化深度（可选）
//! target/camera/latest.points 深度反投影点云 xyz_f32_le（可选）
//! ```
//!
//! 用文件而不是 stdout/socket 是刻意的：Python 可以独立起停、崩溃不影响编辑器，
//! 也方便把一帧目录直接拷走做回归。代价是最多约一帧的文件轮询延迟。
//!
//! 读端对文件是**只读**的；不修改、不删除任何摄像头数据。

#[cfg(not(target_arch = "wasm32"))]
use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[cfg(not(target_arch = "wasm32"))]
use egui::{Color32, ColorImage, Pos2, Rect, Stroke, TextureHandle, TextureOptions};
#[cfg(not(target_arch = "wasm32"))]
use quench3d_math::Vec3;
#[cfg(not(target_arch = "wasm32"))]
use quench3d_render::DebugLines;
#[cfg(not(target_arch = "wasm32"))]
use serde::Deserialize;

/// MediaPipe Pose 的 33 点连接表。
///
/// 与 `tools/camera_mocap/camera_mocap.py` 的 `POSE_CONNECTIONS` 保持一致；
/// 两边的单测都会检查索引范围。
pub const POSE_CONNECTIONS: [(usize, usize); 35] = [
    (0, 1),
    (1, 2),
    (2, 3),
    (3, 7),
    (0, 4),
    (4, 5),
    (5, 6),
    (6, 8),
    (9, 10),
    (11, 12),
    (11, 13),
    (13, 15),
    (15, 17),
    (15, 19),
    (15, 21),
    (17, 19),
    (12, 14),
    (14, 16),
    (16, 18),
    (16, 20),
    (16, 22),
    (18, 20),
    (11, 23),
    (12, 24),
    (23, 24),
    (23, 25),
    (24, 26),
    (25, 27),
    (26, 28),
    (27, 29),
    (28, 30),
    (29, 31),
    (30, 32),
    (27, 31),
    (28, 32),
];

/// 3D 骨架的放置点。世界单位是米；把人放在默认场景相机看得见的位置。
#[cfg(not(target_arch = "wasm32"))]
const MOCAP_ORIGIN: Vec3 = Vec3::new(2.2, 1.0, 0.0);

/// 一个 2D/3D 关键点。
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct Landmark {
    /// 图像归一化 x（0..1）。
    #[serde(default)]
    pub x: f32,
    /// 图像归一化 y（0..1）。
    #[serde(default)]
    pub y: f32,
    /// MediaPipe 的相对深度（不是米；3D 用 `world`）。
    #[serde(default)]
    pub z: f32,
    /// 可见性置信度 0..1。
    #[serde(default)]
    pub visibility: f32,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, Default, Deserialize)]
struct PoseJson {
    #[serde(default)]
    present: bool,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    landmarks: Vec<Landmark>,
    #[serde(default)]
    world: Vec<Landmark>,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, Default, Deserialize)]
struct SilhouetteJson {
    #[serde(default)]
    present: bool,
    #[serde(default)]
    polygon: Vec<[f32; 2]>,
    #[serde(default)]
    area: f32,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, Default, Deserialize)]
struct DepthJson {
    #[serde(default)]
    present: bool,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    raw_min: Option<f32>,
    #[serde(default)]
    raw_max: Option<f32>,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, Default, Deserialize)]
struct PointsJson {
    #[serde(default)]
    present: bool,
    #[serde(default)]
    path: String,
    #[serde(default)]
    count: usize,
    #[serde(default)]
    layout: String,
    #[serde(default)]
    space: String,
    #[serde(default)]
    step: u32,
    #[serde(default)]
    bounds: Option<[f32; 6]>,
}

/// `latest.json` 的反序列化目标。
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, Default, Deserialize)]
pub struct FeedJson {
    /// 协议名，必须是 `quench3d.mocap/1`。
    #[serde(default)]
    pub schema: String,
    /// 协议版本。
    #[serde(default)]
    pub version: u32,
    /// 自增帧号。
    #[serde(default)]
    pub frame: u64,
    /// Unix 毫秒时间戳。
    #[serde(default)]
    pub timestamp_ms: u64,
    /// 输入源描述（摄像头编号/文件/synthetic）。
    #[serde(default)]
    pub source: String,
    /// 像素宽度。
    #[serde(default)]
    pub width: u32,
    /// 像素高度。
    #[serde(default)]
    pub height: u32,
    /// 采集端估计的 FPS。
    #[serde(default)]
    pub fps: f32,
    /// 是否水平镜像。
    #[serde(default)]
    pub mirrored: bool,
    #[serde(default)]
    pose: PoseJson,
    #[serde(default)]
    silhouette: SilhouetteJson,
    #[serde(default)]
    depth: DepthJson,
    #[serde(default)]
    points: PointsJson,
}

/// 一帧已经读入内存的动捕数据。
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct FeedFrame {
    /// 元数据。
    pub json: FeedJson,
    /// RGB24 像素。
    pub rgb: Vec<u8>,
    /// 可选的 u8 深度图。
    pub depth: Option<Vec<u8>>,
    /// 可选的点云（引擎坐标）。
    pub points: Option<Vec<Vec3>>,
    /// 读入时刻，用于判断数据是否已经停更。
    pub loaded_at: Instant,
}

#[cfg(not(target_arch = "wasm32"))]
struct Shared {
    latest: Arc<Mutex<Option<Arc<FeedFrame>>>>,
    error: Arc<Mutex<Option<String>>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Shared {
    fn new() -> Self {
        Self {
            latest: Arc::new(Mutex::new(None)),
            error: Arc::new(Mutex::new(None)),
        }
    }
}

/// 摄像头动捕读端。
///
/// 桌面端会启一个后台线程轮询输出目录；Web 端是空实现（浏览器没有本地文件
/// 轮询这条路，摄像头接入属于另一条 Web 管线）。
#[cfg(not(target_arch = "wasm32"))]
pub struct CameraFeed {
    dir: PathBuf,
    shared: Shared,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    texture: Option<TextureHandle>,
    texture_frame: u64,
    depth_texture: Option<TextureHandle>,
    depth_frame: u64,
    show_depth: bool,
    show_points: bool,
}

#[cfg(not(target_arch = "wasm32"))]
impl CameraFeed {
    /// 新建：读 `QUENCH3D_CAMERA_DIR`，缺省是当前目录下的 `target/camera`。
    pub fn new() -> Self {
        let dir = std::env::var_os("QUENCH3D_CAMERA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/camera"));
        let shared = Shared::new();
        let stop = Arc::new(AtomicBool::new(false));
        let worker = spawn_worker(
            dir.clone(),
            shared.latest.clone(),
            shared.error.clone(),
            stop.clone(),
        );
        Self {
            dir,
            shared,
            stop,
            worker,
            texture: None,
            texture_frame: u64::MAX,
            depth_texture: None,
            depth_frame: u64::MAX,
            show_depth: false,
            show_points: false,
        }
    }

    /// 最近一帧（如果有）。
    pub fn latest(&self) -> Option<Arc<FeedFrame>> {
        self.shared
            .latest
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().cloned())
    }

    fn last_error(&self) -> Option<String> {
        self.shared
            .error
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    /// 把 3D 世界关键点画到渲染器的调试线段里。
    ///
    /// 在 `Engine::tick` 之后、`render_to` 之前调用；`begin_frame` 已经清过线段，
    /// 所以这里每次只加本帧的骨架。数据超过 2 秒没更新就不再画，避免留下僵尸骨架。
    pub fn add_lines(&self, lines: &mut DebugLines) {
        let Some(frame) = self.latest() else {
            return;
        };
        if frame.loaded_at.elapsed() > Duration::from_secs(2) {
            return;
        }
        if self.show_points {
            if let Some(points) = &frame.points {
                add_point_lines(lines, points);
            }
        }
        let pose = &frame.json.pose;
        if !pose.present || pose.world.len() < 33 {
            return;
        }
        add_pose_lines(lines, &pose.world);
    }

    /// 画浮动窗口：摄像头画面 + 2D 骨架/轮廓 + 深度开关。
    pub fn window(&mut self, ctx: &egui::Context) {
        // 不挂关闭按钮：第一阶段先保证"能看见"；等接入顶栏开关后再让它可隐藏。
        egui::Window::new("摄像头动捕（实验）")
            .default_pos(egui::pos2(40.0, 140.0))
            .default_width(380.0)
            .resizable(true)
            .show(ctx, |ui| self.contents(ui));
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
        ui.label(
            egui::RichText::new(
                "采集/姿态/轮廓/深度由 tools/camera_mocap 在本地完成；编辑器只读 latest.* 文件。",
            )
            .small()
            .weak(),
        );
        let Some(frame) = self.latest() else {
            ui.add_space(4.0);
            ui.label("还没有收到帧。先运行：");
            ui.code("python tools/camera_mocap/camera_mocap.py --source 0");
            ui.label(egui_hint());
            if let Some(err) = self.last_error() {
                ui.colored_label(Color32::from_rgb(255, 120, 100), err);
            }
            return;
        };

        let stale = frame.loaded_at.elapsed() > Duration::from_secs(2);
        let status = if stale {
            format!(
                "{}×{} · 帧 {} · 数据已停止更新",
                frame.json.width, frame.json.height, frame.json.frame
            )
        } else {
            format!(
                "{}×{} · 帧 {} · {:.1} fps · 姿态 {:.2}",
                frame.json.width,
                frame.json.height,
                frame.json.frame,
                frame.json.fps,
                frame.json.pose.confidence
            )
        };
        let status = format!(
            "{status} · 协议 v{} · t={} ms",
            frame.json.version, frame.json.timestamp_ms
        );
        ui.label(if stale {
            egui::RichText::new(status).color(Color32::from_rgb(255, 170, 80))
        } else {
            egui::RichText::new(status)
        });

        if frame.json.pose.present {
            ui.label(
                egui::RichText::new("3D 骨骼已叠加到视口（青绿骨骼 / 黄色关节点）")
                    .small()
                    .weak(),
            );
        } else {
            ui.label("画面里没有检测到人体。");
        }
        if frame.json.silhouette.present {
            ui.label(
                egui::RichText::new(format!(
                    "轮廓：{} 个点 · 面积 {:.1}%",
                    frame.json.silhouette.polygon.len(),
                    frame.json.silhouette.area * 100.0
                ))
                .small()
                .weak(),
            );
        }

        self.update_texture(ui.ctx(), &frame);
        let w = ui.available_width().min(480.0);
        let h = (w * frame.json.height.max(1) as f32 / frame.json.width.max(1) as f32).max(1.0);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
        if let Some(tex) = &self.texture {
            ui.painter().image(
                tex.id(),
                rect,
                Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        self.paint_overlay(ui, rect, &frame);

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.show_depth, "显示深度");
            if frame.json.depth.present {
                ui.label(
                    egui::RichText::new(format!(
                        "{} 相对逆深度 {:?}..{:?}",
                        frame.json.depth.kind, frame.json.depth.raw_min, frame.json.depth.raw_max
                    ))
                    .small()
                    .weak(),
                );
            } else {
                ui.label(egui::RichText::new("未启用深度").small().weak());
            }
        });
        if self.show_depth {
            if frame.json.depth.present {
                self.update_depth_texture(ui.ctx(), &frame);
                if let Some(tex) = &self.depth_texture {
                    let (drect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
                    ui.painter().image(
                        tex.id(),
                        drect,
                        Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }
            } else {
                ui.label("采集端没写 latest.depth（加 --no-depth 时正常）。");
            }
        }
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.show_points, "显示点云（深度反投影）");
            if frame.json.points.present {
                let hover = frame
                    .json
                    .points
                    .bounds
                    .map(|b| format!("bounds [{b:?}]"))
                    .unwrap_or_else(|| "bounds 未提供".to_string());
                ui.label(
                    egui::RichText::new(format!(
                        "{} 点 · 步长 {} · {}",
                        frame.json.points.count, frame.json.points.step, frame.json.points.space
                    ))
                    .small()
                    .weak(),
                )
                .on_hover_text(hover);
            } else {
                ui.label(
                    egui::RichText::new("没有点云（--no-points 或未启用深度）")
                        .small()
                        .weak(),
                );
            }
        });
        ui.label(
            egui::RichText::new(format!(
                "源：{} · 镜像：{} · 目录：{}",
                frame.json.source,
                if frame.json.mirrored { "是" } else { "否" },
                self.dir.display()
            ))
            .small()
            .weak(),
        );
        if let Some(err) = self.last_error() {
            ui.colored_label(Color32::from_rgb(255, 120, 100), err);
        }
    }

    fn update_texture(&mut self, ctx: &egui::Context, frame: &FeedFrame) {
        if self.texture_frame == frame.json.frame && self.texture.is_some() {
            return;
        }
        let w = frame.json.width as usize;
        let h = frame.json.height as usize;
        if w == 0 || h == 0 || frame.rgb.len() != w * h * 3 {
            return;
        }
        let image = ColorImage::from_rgb([w, h], &frame.rgb);
        match &mut self.texture {
            Some(tex) => tex.set(image, TextureOptions::LINEAR),
            None => {
                self.texture =
                    Some(ctx.load_texture("quench3d-camera-frame", image, TextureOptions::LINEAR));
            }
        }
        self.texture_frame = frame.json.frame;
    }

    fn update_depth_texture(&mut self, ctx: &egui::Context, frame: &FeedFrame) {
        if self.depth_frame == frame.json.frame && self.depth_texture.is_some() {
            return;
        }
        let Some(depth) = &frame.depth else {
            return;
        };
        let w = frame.json.width as usize;
        let h = frame.json.height as usize;
        if w == 0 || h == 0 || depth.len() != w * h {
            return;
        }
        let mut rgb = Vec::with_capacity(w * h * 3);
        for &v in depth {
            rgb.extend_from_slice(&[v, v, v]);
        }
        let image = ColorImage::from_rgb([w, h], &rgb);
        match &mut self.depth_texture {
            Some(tex) => tex.set(image, TextureOptions::LINEAR),
            None => {
                self.depth_texture =
                    Some(ctx.load_texture("quench3d-camera-depth", image, TextureOptions::LINEAR));
            }
        }
        self.depth_frame = frame.json.frame;
    }

    fn paint_overlay(&self, ui: &egui::Ui, rect: Rect, frame: &FeedFrame) {
        let painter = ui.painter_at(rect);
        let map = |x: f32, y: f32| -> Pos2 {
            Pos2::new(
                rect.left() + x.clamp(0.0, 1.0) * rect.width(),
                rect.top() + y.clamp(0.0, 1.0) * rect.height(),
            )
        };
        if frame.json.silhouette.present && frame.json.silhouette.polygon.len() >= 3 {
            let mut points: Vec<Pos2> = frame
                .json
                .silhouette
                .polygon
                .iter()
                .map(|p| map(p[0], p[1]))
                .collect();
            if let Some(first) = points.first().copied() {
                points.push(first);
            }
            painter.add(egui::Shape::line(
                points,
                Stroke::new(1.5, Color32::from_rgb(80, 190, 255)),
            ));
        }
        if !frame.json.pose.present {
            return;
        }
        let screen = &frame.json.pose.landmarks;
        for &(a, b) in POSE_CONNECTIONS.iter() {
            if a >= screen.len() || b >= screen.len() {
                continue;
            }
            let (la, lb) = (&screen[a], &screen[b]);
            if la.visibility < 0.2 || lb.visibility < 0.2 {
                continue;
            }
            painter.line_segment(
                [map(la.x, la.y), map(lb.x, lb.y)],
                Stroke::new(2.0, Color32::from_rgb(90, 235, 150)),
            );
        }
        for lm in screen {
            if lm.visibility < 0.2 {
                continue;
            }
            painter.circle_filled(map(lm.x, lm.y), 2.6, Color32::from_rgb(255, 220, 80));
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for CameraFeed {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for CameraFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn egui_hint() -> &'static str {
    "没有摄像头时可以用合成源验证：\n\
     python tools/camera_mocap/camera_mocap.py --source synthetic\n\
     或指定输出目录 QUENCH3D_CAMERA_DIR=/path/to/dir 后重启编辑器。"
}

#[cfg(not(target_arch = "wasm32"))]
fn spawn_worker(
    dir: PathBuf,
    latest: Arc<Mutex<Option<Arc<FeedFrame>>>>,
    error: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
) -> Option<JoinHandle<()>> {
    thread::Builder::new()
        .name("quench3d-camera-feed".to_string())
        .spawn(move || {
            let mut last_frame = u64::MAX;
            while !stop.load(Ordering::Relaxed) {
                match read_frame(&dir) {
                    Ok(Some(frame)) => {
                        if frame.json.frame != last_frame {
                            last_frame = frame.json.frame;
                            if let Ok(mut slot) = latest.lock() {
                                *slot = Some(Arc::new(frame));
                            }
                            if let Ok(mut e) = error.lock() {
                                *e = None;
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(message) => {
                        if let Ok(mut e) = error.lock() {
                            *e = Some(message);
                        }
                    }
                }
                thread::sleep(Duration::from_millis(33));
            }
        })
        .ok()
}

#[cfg(not(target_arch = "wasm32"))]
fn read_frame(dir: &Path) -> Result<Option<FeedFrame>, String> {
    let json_path = dir.join("latest.json");
    let json_bytes = match std::fs::read(&json_path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("读 {} 失败：{e}", json_path.display())),
    };
    let json: FeedJson =
        serde_json::from_slice(&json_bytes).map_err(|e| format!("latest.json 解析失败：{e}"))?;
    if json.schema != "quench3d.mocap/1" {
        return Err(format!(
            "不认识的动捕协议：`{}`（期望 quench3d.mocap/1）",
            json.schema
        ));
    }
    if json.version != 1 {
        return Err(format!("不支持的动捕协议版本：{}", json.version));
    }
    let w = json.width as usize;
    let h = json.height as usize;
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        return Err(format!("非法画面尺寸：{}×{}", json.width, json.height));
    }
    let rgb_path = dir.join("latest.rgb");
    let rgb =
        std::fs::read(&rgb_path).map_err(|e| format!("读 {} 失败：{e}", rgb_path.display()))?;
    if rgb.len() != w * h * 3 {
        return Err(format!(
            "latest.rgb 长度 {} 与 {}×{}×3 不符",
            rgb.len(),
            w,
            h
        ));
    }
    let depth = if json.depth.present {
        let path = dir.join("latest.depth");
        match std::fs::read(&path) {
            Ok(bytes) if bytes.len() == w * h => Some(bytes),
            Ok(bytes) => {
                return Err(format!(
                    "latest.depth 长度 {} 与 {}×{} 不符",
                    bytes.len(),
                    w,
                    h
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("读 {} 失败：{e}", path.display())),
        }
    } else {
        None
    };
    let points = if json.points.present {
        if !json.points.layout.is_empty() && json.points.layout != "xyz_f32_le" {
            return Err(format!("不支持的点云布局：{}", json.points.layout));
        }
        if !json.points.space.is_empty() && json.points.space != "engine" {
            return Err(format!("不支持的点云坐标系：{}", json.points.space));
        }
        let expected = json
            .points
            .count
            .checked_mul(12)
            .ok_or_else(|| "点云数量溢出".to_string())?;
        if json.points.count > 1_000_000 {
            return Err(format!("点云数量过大：{}", json.points.count));
        }
        let path = if json.points.path.is_empty() {
            dir.join("latest.points")
        } else {
            dir.join(&json.points.path)
        };
        match std::fs::read(&path) {
            Ok(bytes) if bytes.len() == expected => {
                let mut out = Vec::with_capacity(json.points.count);
                for chunk in bytes.as_chunks::<12>().0 {
                    let x = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    let y = f32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
                    let z = f32::from_le_bytes([chunk[8], chunk[9], chunk[10], chunk[11]]);
                    if !x.is_finite() || !y.is_finite() || !z.is_finite() {
                        return Err("点云含非有限坐标".to_string());
                    }
                    out.push(Vec3::new(x, y, z));
                }
                Some(out)
            }
            Ok(bytes) => {
                return Err(format!(
                    "latest.points 长度 {} 与 {} 点×12 不符",
                    bytes.len(),
                    json.points.count
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("读 {} 失败：{e}", path.display())),
        }
    } else {
        None
    };
    Ok(Some(FeedFrame {
        json,
        rgb,
        depth,
        points,
        loaded_at: Instant::now(),
    }))
}

/// 把点云画成小十字。深度反投影没有拓扑，点本身就是最小可读单元。
#[cfg(not(target_arch = "wasm32"))]
fn add_point_lines(lines: &mut DebugLines, points: &[Vec3]) {
    let color = quench3d_math::Vec4::new(0.3, 0.85, 1.0, 0.85);
    let d = 0.006;
    for p in points {
        lines.line(
            *p - Vec3::new(d, 0.0, 0.0),
            *p + Vec3::new(d, 0.0, 0.0),
            color,
        );
        lines.line(
            *p - Vec3::new(0.0, d, 0.0),
            *p + Vec3::new(0.0, d, 0.0),
            color,
        );
        lines.line(
            *p - Vec3::new(0.0, 0.0, d),
            *p + Vec3::new(0.0, 0.0, d),
            color,
        );
    }
}

/// 把 33 个世界点画成骨骼与关节点；抽出来是为了能脱离轮询线程做单测。
#[cfg(not(target_arch = "wasm32"))]
fn add_pose_lines(lines: &mut DebugLines, world: &[Landmark]) {
    let bone_color = quench3d_math::Vec4::new(0.2, 1.0, 0.75, 0.95);
    let joint_color = quench3d_math::Vec4::new(1.0, 0.85, 0.25, 0.95);
    for &(a, b) in POSE_CONNECTIONS.iter() {
        if a >= world.len() || b >= world.len() {
            continue;
        }
        let pa = &world[a];
        let pb = &world[b];
        if pa.visibility < 0.2 || pb.visibility < 0.2 {
            continue;
        }
        lines.line(world_to_scene(*pa), world_to_scene(*pb), bone_color);
    }
    for lm in world {
        if lm.visibility < 0.2 {
            continue;
        }
        lines.sphere(world_to_scene(*lm), 0.018, joint_color, 6);
    }
}

/// MediaPipe 世界点 → 引擎世界点。
///
/// MediaPipe：x 向右、y **向下**、z 向后为正，原点在髋部。
/// 引擎：y 向上、z 向屏幕外（相机在 +z 看向 -z）。所以 y/z 取反，
/// 再平移到 `MOCAP_ORIGIN`，避免骨架埋在默认场景的原点物体里。
#[cfg(not(target_arch = "wasm32"))]
fn world_to_scene(lm: Landmark) -> Vec3 {
    Vec3::new(
        lm.x + MOCAP_ORIGIN.x,
        -lm.y + MOCAP_ORIGIN.y,
        -lm.z + MOCAP_ORIGIN.z,
    )
}

#[cfg(target_arch = "wasm32")]
/// Web 端的空实现：桌面文件协议不适用于浏览器。
pub struct CameraFeed;

#[cfg(target_arch = "wasm32")]
impl Default for CameraFeed {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_arch = "wasm32")]
impl CameraFeed {
    /// 新建空实现。
    pub fn new() -> Self {
        Self
    }

    /// 无操作。
    pub fn add_lines(&self, _lines: &mut quench3d_render::DebugLines) {}

    /// 无操作。
    pub fn window(&mut self, _ctx: &egui::Context) {}
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn pose_connections_are_in_range() {
        for &(a, b) in POSE_CONNECTIONS.iter() {
            assert!(a < 33 && b < 33);
        }
    }

    #[test]
    fn world_to_scene_flips_y_and_z() {
        let p = world_to_scene(Landmark {
            x: 0.5,
            y: 0.25,
            z: -0.3,
            visibility: 1.0,
        });
        assert!((p.x - (MOCAP_ORIGIN.x + 0.5)).abs() < 1e-6);
        assert!((p.y - (MOCAP_ORIGIN.y - 0.25)).abs() < 1e-6);
        assert!((p.z - (MOCAP_ORIGIN.z + 0.3)).abs() < 1e-6);
    }

    #[test]
    fn feed_json_parses_the_wire_format() {
        let text = r#"{
          "schema":"quench3d.mocap/1","version":1,"frame":7,
          "timestamp_ms":123,"source":"synthetic","width":2,"height":2,
          "fps":29.5,"mirrored":true,
          "pose":{"present":true,"confidence":0.9,
            "landmarks":[{"x":0.5,"y":0.5,"z":0.0,"visibility":0.99}],
            "world":[{"x":0.0,"y":0.0,"z":0.0,"visibility":0.99}]},
          "silhouette":{"present":true,"polygon":[[0.1,0.2],[0.9,0.2],[0.9,0.8]],"area":0.1},
          "depth":{"present":false,"kind":"relative_inverse_depth","raw_min":null,"raw_max":null}
        }"#;
        let json: FeedJson = serde_json::from_str(text).expect("解析");
        assert_eq!(json.frame, 7);
        assert_eq!(json.width, 2);
        assert!(json.pose.present);
        assert_eq!(json.pose.world.len(), 1);
        assert!(json.silhouette.present);
        assert!(!json.depth.present);
    }

    #[test]
    fn pose_lines_cover_all_bones_and_joints() {
        let world: Vec<Landmark> = (0..33)
            .map(|i| Landmark {
                x: (i as f32 - 16.0) * 0.01,
                y: (i as f32 - 16.0) * 0.01,
                z: 0.0,
                visibility: 1.0,
            })
            .collect();
        let mut lines = DebugLines::new();
        add_pose_lines(&mut lines, &world);
        // 35 根骨 + 33 个关节点 × 3 个正交圆 × 6 段
        assert_eq!(lines.line_count(), POSE_CONNECTIONS.len() + 33 * 18);
    }

    #[test]
    fn invisible_joints_are_not_drawn() {
        let world = vec![
            Landmark {
                x: 0.0,
                y: 0.0,
                z: 0.0,
                visibility: 0.0,
            };
            33
        ];
        let mut lines = DebugLines::new();
        add_pose_lines(&mut lines, &world);
        assert_eq!(lines.line_count(), 0);
    }

    #[test]
    fn read_frame_accepts_a_complete_frame() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "quench3d-camera-ok-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("建目录");
        let json = r#"{"schema":"quench3d.mocap/1","version":1,"frame":1,
            "timestamp_ms":0,"source":"test","width":2,"height":2,"fps":30,
            "pose":{"present":true,"world":[]},"silhouette":{"present":false},
            "depth":{"present":true,"kind":"relative_inverse_depth"}}"#;
        std::fs::write(dir.join("latest.json"), json).expect("写 json");
        std::fs::write(dir.join("latest.rgb"), [7u8; 12]).expect("写 rgb");
        std::fs::write(dir.join("latest.depth"), [3u8; 4]).expect("写 depth");
        let frame = read_frame(&dir).expect("读").expect("有帧");
        assert_eq!(frame.json.frame, 1);
        assert_eq!(frame.rgb.len(), 12);
        assert_eq!(frame.depth.as_deref(), Some([3u8; 4].as_slice()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn points_json_parses() {
        let text = r#"{
          "schema":"quench3d.mocap/1","version":1,"frame":7,
          "timestamp_ms":123,"source":"synthetic","width":2,"height":2,
          "fps":29.5,"mirrored":true,
          "pose":{"present":false},"silhouette":{"present":false},
          "depth":{"present":false},
          "points":{"present":true,"path":"latest.points","count":2,
            "layout":"xyz_f32_le","space":"engine","step":6,
            "bounds":[-1.0,-1.0,-3.0,1.0,1.0,0.0]}
        }"#;
        let json: FeedJson = serde_json::from_str(text).expect("解析");
        assert!(json.points.present);
        assert_eq!(json.points.count, 2);
        assert_eq!(json.points.layout, "xyz_f32_le");
        assert_eq!(json.points.bounds, Some([-1.0, -1.0, -3.0, 1.0, 1.0, 0.0]));
    }

    #[test]
    fn point_cloud_draws_three_cross_segments_per_point() {
        let mut lines = DebugLines::new();
        add_point_lines(&mut lines, &[Vec3::ZERO, Vec3::new(1.0, 2.0, 3.0)]);
        assert_eq!(lines.line_count(), 6);
    }

    #[test]
    fn read_frame_accepts_point_cloud() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "quench3d-camera-points-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("建目录");
        let json = r#"{"schema":"quench3d.mocap/1","version":1,"frame":2,
            "timestamp_ms":0,"source":"test","width":2,"height":2,"fps":30,
            "pose":{"present":false},"silhouette":{"present":false},
            "depth":{"present":false},
            "points":{"present":true,"path":"latest.points","count":2,
              "layout":"xyz_f32_le","space":"engine","step":1}}"#;
        std::fs::write(dir.join("latest.json"), json).expect("写 json");
        std::fs::write(dir.join("latest.rgb"), [0u8; 12]).expect("写 rgb");
        let mut bytes = Vec::new();
        for v in [2.0f32, 1.0, -3.0, -0.5, 2.5, -1.0] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(dir.join("latest.points"), bytes).expect("写 points");
        let frame = read_frame(&dir).expect("读").expect("有帧");
        let points = frame.points.expect("点云应被读入");
        assert_eq!(points.len(), 2);
        assert!((points[0].x - 2.0).abs() < 1e-6);
        assert!((points[1].z + 1.0).abs() < 1e-6);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_frame_rejects_truncated_rgb() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "quench3d-camera-test-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("建目录");
        let json = r#"{"schema":"quench3d.mocap/1","version":1,"frame":1,
            "timestamp_ms":0,"source":"test","width":2,"height":2,"fps":30,
            "pose":{"present":false},"silhouette":{"present":false},
            "depth":{"present":false}}"#;
        std::fs::write(dir.join("latest.json"), json).expect("写 json");
        std::fs::write(dir.join("latest.rgb"), [0u8; 4]).expect("写坏 rgb");
        let err = read_frame(&dir).expect_err("必须拒绝");
        assert!(err.contains("长度"), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
