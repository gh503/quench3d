//! 摄像头动捕的**读端**：轮询 `latest.json` + `latest.rgb`，在 egui 里显示
//! 2D 轮廓/骨架，并把 3D 世界点作为调试线段叠进 3D 视口。
//!
//! 采集/推理在 `tools/camera_mocap/camera_mocap.py` 里（MediaPipe + OpenCV），
//! 这里不链接任何 CV 库；用户点击后由原生控制器启动本地采集子进程。两边共享文件协议：
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
//!
//! **Web 端**另走一条预览：顶部「摄像头」→ `getUserMedia` → `<video>` →
//! 2D canvas 采样 → egui 纹理。浏览器里不跑推理；外部 JS（例如 MediaPipe Tasks
//! Vision 的页面脚本）可以调用 `set_camera_pose(json)` 注入 33 点骨架，读端在
//! 预览图上画线。**内置推理仍不在浏览器里**。

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
#[path = "camera_capture.rs"]
mod capture;

use crate::reconstruct::PointCloudAccumulator;
#[cfg(not(target_arch = "wasm32"))]
use egui::{Color32, ColorImage, Pos2, Rect, RichText, Stroke, TextureHandle, TextureOptions};
#[cfg(not(target_arch = "wasm32"))]
use quench3d::Engine;
use quench3d_math::Vec3;
#[cfg(not(target_arch = "wasm32"))]
use quench3d_math::Vec4;
use quench3d_render::DebugLines;
#[cfg(target_arch = "wasm32")]
use quench3d_render::{GpuContext, Renderer};
#[cfg(not(target_arch = "wasm32"))]
use quench3d_render::{
    ImageData, MaterialDesc, MaterialId, MaterialTextureSlot, MeshData, MeshId, TextureId,
};
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
const MOCAP_ORIGIN: Vec3 = Vec3::new(2.2, 1.0, 0.0);

/// 一个 2D/3D 关键点。
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
    /// 反投影采样步长（像素）；0 视为 6（采集端缺省）。
    #[serde(default)]
    point_step: u32,
    /// u8=255 对应的相机空间深度（任意尺度，缺省 0.3）。
    #[serde(default)]
    z_near: Option<f32>,
    /// u8=0 对应的相机空间深度（缺省 3.0）。
    #[serde(default)]
    z_far: Option<f32>,
    /// 反投影后加上的引擎坐标原点（缺省 [2.2, 1.0, 0.0]）。
    #[serde(default)]
    origin: Option<[f32; 3]>,
}

/// 采集端估计的针孔内参；缺省 0 表示没有内参、不做深度反投影。
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, Default, Deserialize)]
struct IntrinsicsJson {
    #[serde(default)]
    fx: f32,
    #[serde(default)]
    fy: f32,
    #[serde(default)]
    cx: f32,
    #[serde(default)]
    cy: f32,
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
    #[serde(default)]
    intrinsics: IntrinsicsJson,
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
/// 桌面端启后台线程轮询输出目录；Web 端用 `getUserMedia` 预览（见下方 wasm 实现）。
#[cfg(not(target_arch = "wasm32"))]
pub struct CameraFeed {
    dir: PathBuf,
    capture: capture::CaptureProcess,
    show: bool,
    source: u32,
    pose_capture: bool,
    shared: Shared,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    texture: Option<TextureHandle>,
    texture_frame: u64,
    depth_texture: Option<TextureHandle>,
    depth_frame: u64,
    show_depth: bool,
    show_points: bool,
    /// 把最新摄像头帧作为 3D 视口背景（AR 模式）；桌面端专用。
    background_3d: bool,
    /// 已经上传到渲染器的帧号（避免同一帧每帧重传）。
    background_frame: u64,
    /// 复用的 RGBA 缓冲（摄像头 640×480 一帧约 1.2 MB）。
    background_rgba: Vec<u8>,
    /// UI 请求"从当前点云生成网格"，下一帧由 3D 循环消费。
    recon_requested: bool,
    /// 是否跟随最新帧自动重建（每帧一次，开销明显）。
    recon_auto: bool,
    /// 已重建的摄像头帧号。
    recon_frame: u64,
    /// 上次重建用的体素尺寸（改尺寸后同一帧也要重建）。
    recon_voxel_used: f32,
    /// 用平滑 Surface Nets（否则用体素块）。
    recon_smooth: bool,
    /// 上次重建是否用了平滑模式。
    recon_smooth_used: bool,
    /// 生成的网格/物体句柄（会话内复用，避免每次点击都新增物体）。
    recon_mesh: Option<MeshId>,
    recon_object: Option<usize>,
    /// 重建材质（用于把摄像头原图作为基础色贴图）。
    recon_material: Option<MaterialId>,
    /// 当前挂在重建材质上的摄像头贴图；换帧时回收旧的。
    recon_texture: Option<TextureId>,
    /// 体素边长（米）。
    recon_voxel: f32,
    /// 是否把最近若干帧点云按体素累积后再重建（缺省关，旧路径不变）。
    recon_accumulate: bool,
    /// 累积器（`recon_accumulate` 打开时才建）。
    recon_accum: Option<PointCloudAccumulator>,
    /// 累积器已经吸收到的摄像头帧号。
    recon_accum_frame: u64,
    /// 累积器建立时用的体素尺寸（改尺寸要重建累积器）。
    recon_accum_voxel: f32,
    /// 是否用相邻帧 ICP 把移动相机拼到同一参考系（缺省关）。
    recon_track: bool,
    /// 上一帧原始点云（相机坐标系），ICP 的参考帧。
    track_previous: Vec<Vec3>,
    /// 累积的“当前相机 → 参考帧”旋转。
    track_rotation: quench3d_math::Quat,
    /// 累积的“当前相机 → 参考帧”平移。
    track_translation: Vec3,
    /// 成功配准的帧数（诊断/UI）。
    track_frames: u64,
    /// 最近一次配准 rms（米）。
    track_rms: f32,
    /// 最近一次生成的结果/错误，显示在窗口里。
    recon_status: String,
}

/// 把一帧 `raw` 用 ICP 增量叠进已经累积的位姿，并返回搬到参考系的点。
///
/// `rotation`/`translation` 是“上一帧相机 → 参考帧”；配准成功后就地更新成
/// “当前帧相机 → 参考帧”。单独抽出来是为了能不用窗口/线程直接测组合顺序。
fn compose_tracked_points(
    previous: &[Vec3],
    raw: &[Vec3],
    rotation: &mut quench3d_math::Quat,
    translation: &mut Vec3,
) -> Option<(Vec<Vec3>, f32)> {
    let reg = crate::reconstruct::register_point_clouds(previous, raw)?;
    *translation = rotation.mul_vec3(reg.translation) + *translation;
    *rotation = (*rotation * reg.rotation).normalize_or_identity();
    Some((
        raw.iter()
            .map(|p| rotation.mul_vec3(*p) + *translation)
            .collect(),
        reg.rms,
    ))
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
            capture: capture::CaptureProcess::default(),
            show: false,
            source: 0,
            pose_capture: false,
            shared,
            stop,
            worker,
            texture: None,
            texture_frame: u64::MAX,
            depth_texture: None,
            depth_frame: u64::MAX,
            show_depth: false,
            show_points: false,
            background_3d: false,
            background_frame: u64::MAX,
            background_rgba: Vec::new(),
            recon_requested: false,
            recon_auto: false,
            recon_frame: u64::MAX,
            recon_voxel_used: 0.0,
            recon_smooth: true,
            recon_smooth_used: false,
            recon_mesh: None,
            recon_object: None,
            recon_material: None,
            recon_texture: None,
            recon_voxel: 0.03,
            recon_accumulate: false,
            recon_accum: None,
            recon_accum_frame: u64::MAX,
            recon_accum_voxel: 0.0,
            recon_track: false,
            track_previous: Vec::new(),
            track_rotation: quench3d_math::Quat::IDENTITY,
            track_translation: Vec3::ZERO,
            track_frames: 0,
            track_rms: f32::INFINITY,
            recon_status: String::new(),
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

    /// 测试用：直接注入一帧，避免依赖真实摄像头目录。
    #[cfg(test)]
    fn set_latest_for_test(&self, frame: Arc<FeedFrame>) {
        if let Ok(mut slot) = self.shared.latest.lock() {
            *slot = Some(frame);
        }
    }

    /// 最新帧号（不克隆帧数据）。
    fn latest_frame_id(&self) -> Option<u64> {
        self.shared
            .latest
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|f| f.json.frame))
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
                let colors = Self::frame_projection(frame.as_ref())
                    .and_then(|projection| sample_point_colors(points, &frame.rgb, &projection));
                add_point_lines(lines, points, colors.as_deref());
            }
        }
        let pose = &frame.json.pose;
        if !pose.present || pose.world.len() < 33 {
            return;
        }
        add_pose_lines(lines, &pose.world);
    }

    /// UI 是否请求生成重建网格（手动或实时跟随）。
    pub fn recon_requested(&self) -> bool {
        self.recon_requested || self.recon_auto
    }

    /// 当前帧的针孔投影；缺内参/尺寸时返回 `None`，此时保持旧的纯色材质。
    fn frame_projection(frame: &FeedFrame) -> Option<crate::reconstruct::CameraProjection> {
        let intr = &frame.json.intrinsics;
        let origin =
            frame
                .json
                .depth
                .origin
                .unwrap_or([MOCAP_ORIGIN.x, MOCAP_ORIGIN.y, MOCAP_ORIGIN.z]);
        let projection = crate::reconstruct::CameraProjection {
            width: frame.json.width,
            height: frame.json.height,
            fx: intr.fx,
            fy: intr.fy,
            cx: intr.cx,
            cy: intr.cy,
            origin: Vec3::from_array(origin),
            mirrored: frame.json.mirrored,
        };
        projection.is_valid().then_some(projection)
    }

    /// 把摄像头原图挂到重建材质上；没有颜色时清掉并回到纯色。
    fn sync_recon_texture(&mut self, engine: &mut Engine, frame: &FeedFrame, colored: usize) {
        if self.recon_material.is_none() {
            self.recon_material = self
                .recon_object
                .and_then(|i| engine.scene.objects.get(i))
                .map(|o| o.material);
        }
        let Some(material) = self.recon_material else {
            return;
        };
        let expected = frame.json.width as usize * frame.json.height as usize * 3;
        if colored == 0 || frame.rgb.len() != expected || expected == 0 {
            if let Some(old) = self.recon_texture.take() {
                engine.renderer.set_material_texture(
                    &engine.gpu,
                    material,
                    MaterialTextureSlot::BaseColor,
                    None,
                );
                engine.renderer.update_material(
                    &engine.gpu,
                    material,
                    MaterialDesc {
                        base_color: Vec4::new(0.52, 0.68, 0.82, 1.0),
                        metallic: 0.0,
                        roughness: 0.7,
                        ..MaterialDesc::default()
                    },
                );
                engine.renderer.remove_texture(old);
            }
            return;
        }
        let mut rgba = Vec::with_capacity(expected / 3 * 4);
        for px in frame.rgb.as_chunks::<3>().0 {
            rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
        let image = ImageData {
            width: frame.json.width,
            height: frame.json.height,
            rgba,
        };
        let texture = engine.renderer.add_texture(&engine.gpu, &image, true);
        // 贴图会乘基础色；有原图时基础色必须是白色，否则会被旧的蓝色染一遍。
        engine.renderer.update_material(
            &engine.gpu,
            material,
            MaterialDesc {
                base_color: Vec4::new(1.0, 1.0, 1.0, 1.0),
                metallic: 0.0,
                roughness: 0.7,
                base_color_texture: Some(texture),
                unlit: true,
                ..MaterialDesc::default()
            },
        );
        if !engine.renderer.set_material_texture(
            &engine.gpu,
            material,
            MaterialTextureSlot::BaseColor,
            Some(texture),
        ) {
            engine.renderer.remove_texture(texture);
            return;
        }
        if let Some(old) = self.recon_texture.replace(texture) {
            engine.renderer.remove_texture(old);
        }
    }

    /// 清空 ICP 状态；开关切换或累积器重建时调用。
    fn reset_tracking(&mut self) {
        self.track_previous.clear();
        self.track_rotation = quench3d_math::Quat::IDENTITY;
        self.track_translation = Vec3::ZERO;
        self.track_frames = 0;
        self.track_rms = f32::INFINITY;
    }

    /// 用相邻帧点云 ICP 把当前帧映射到参考帧；配准失败就清空累积器、
    /// 以当前帧重新开一个参考系，避免把两个坐标系混在同一个体素网格里。
    fn track_points(&mut self, raw: &[Vec3]) -> Vec<Vec3> {
        if self.track_previous.is_empty() {
            self.track_previous = raw.to_vec();
            return raw.to_vec();
        }
        let Some((points, rms)) = compose_tracked_points(
            &self.track_previous,
            raw,
            &mut self.track_rotation,
            &mut self.track_translation,
        ) else {
            if let Some(acc) = self.recon_accum.as_mut() {
                acc.clear();
            }
            self.track_previous = raw.to_vec();
            self.track_rotation = quench3d_math::Quat::IDENTITY;
            self.track_translation = Vec3::ZERO;
            self.track_frames = 0;
            self.track_rms = f32::INFINITY;
            self.recon_accum_frame = u64::MAX;
            return raw.to_vec();
        };
        self.track_previous = raw.to_vec();
        self.track_frames = self.track_frames.saturating_add(1);
        self.track_rms = rms;
        points
    }

    /// 从当前深度点云生成/更新一个体素表面物体。
    ///
    /// 物体标记 `generated`，场景 JSON 不会尝试保存它（没有源网格会丢）。
    /// 同一会话重复点击只更新同一个网格，不会每点一次多一个物体。
    pub fn build_reconstruction(&mut self, engine: &mut Engine) {
        self.recon_requested = false;
        let Some(frame) = self.latest() else {
            self.recon_status = "没有摄像头帧".to_string();
            return;
        };
        let Some(raw_points) = frame.points.as_deref() else {
            self.recon_status =
                "这一帧没有点云（需要深度 + 内参，或采集端写 --points）".to_string();
            return;
        };
        // 时间累积：同一体素取多帧平均，弥补单帧遮挡/噪声；缺省关时完全走旧路径。
        let accumulated;
        let points: &[Vec3] = if self.recon_accumulate {
            if self.recon_accum.is_none() || self.recon_accum_voxel != self.recon_voxel {
                self.recon_accum = Some(PointCloudAccumulator::new(
                    self.recon_voxel * 0.5,
                    250_000,
                    90,
                ));
                self.recon_accum_voxel = self.recon_voxel;
                self.recon_accum_frame = u64::MAX;
                self.reset_tracking();
            }
            if self.recon_accum_frame == frame.json.frame {
                if let Some(acc) = self.recon_accum.as_ref() {
                    accumulated = acc.points();
                } else {
                    accumulated = Vec::new();
                }
            } else {
                // ICP 与累积都在这里消费“原始相机坐标点”；多帧累加前先搬到
                // 同一参考系，否则移动相机时体素网格会被拖成双层。
                let tracked;
                let input: &[Vec3] = if self.recon_track {
                    tracked = self.track_points(raw_points);
                    &tracked
                } else {
                    self.reset_tracking();
                    raw_points
                };
                if let Some(acc) = self.recon_accum.as_mut() {
                    acc.merge(input, frame.json.frame);
                    self.recon_accum_frame = frame.json.frame;
                    accumulated = acc.points();
                } else {
                    accumulated = Vec::new();
                }
            }
            if accumulated.is_empty() {
                self.recon_status = "累积点云为空".to_string();
                return;
            }
            accumulated.as_slice()
        } else {
            raw_points
        };
        let object_ok = self
            .recon_object
            .and_then(|i| engine.scene.objects.get(i))
            .zip(self.recon_mesh)
            .is_some_and(|(o, id)| o.generated && o.mesh == id);
        if object_ok
            && self.recon_frame == frame.json.frame
            && self.recon_voxel_used == self.recon_voxel
            && self.recon_smooth_used == self.recon_smooth
        {
            self.recon_status = format!("已是最新（帧 {}）", frame.json.frame);
            return;
        }
        let mut mesh = if self.recon_smooth {
            crate::reconstruct::smooth_surface(points, self.recon_voxel, 0.45)
        } else {
            crate::reconstruct::voxel_surface_robust(points, self.recon_voxel)
        };
        let projection = Self::frame_projection(&frame);
        let colored = projection
            .as_ref()
            .map(|p| crate::reconstruct::colorize_mesh_uvs(&mut mesh, &frame.rgb, p))
            .unwrap_or(0);
        if mesh.indices.is_empty() {
            self.recon_status = "点云为空或体素太大，没有生成面".to_string();
            return;
        }
        let triangles = mesh.indices.len() / 3;
        if !object_ok {
            let placeholder = engine.assets.mesh(
                &mut engine.renderer,
                &engine.gpu,
                "$camera_recon",
                &MeshData::cube(0.001),
            );
            let material = engine.assets.material(
                &mut engine.renderer,
                &engine.gpu,
                "$camera_recon",
                MaterialDesc {
                    base_color: if colored > 0 {
                        Vec4::new(1.0, 1.0, 1.0, 1.0)
                    } else {
                        Vec4::new(0.52, 0.68, 0.82, 1.0)
                    },
                    metallic: 0.0,
                    roughness: 0.7,
                    // 有摄像头原图时直接显示原色；场景光照/阴影只适用于没有
                    // 颜色时的蓝灰占位表面。
                    unlit: colored > 0,
                    ..MaterialDesc::default()
                },
            );
            let index = engine.scene.add_named(
                placeholder,
                material,
                quench3d_math::Transform::IDENTITY,
                "摄像头重建",
            );
            engine.scene.objects[index].generated = true;
            self.recon_mesh = Some(placeholder);
            self.recon_object = Some(index);
            self.recon_material = Some(material);
        }
        let Some(id) = self.recon_mesh else {
            self.recon_status = "网格句柄丢失".to_string();
            return;
        };
        if engine.renderer.update_mesh_data(&engine.gpu, id, &mesh) {
            self.sync_recon_texture(engine, &frame, colored);
            self.recon_frame = frame.json.frame;
            self.recon_voxel_used = self.recon_voxel;
            self.recon_smooth_used = self.recon_smooth;
            let tracking = if self.recon_track && self.track_frames > 0 {
                format!(
                    "，ICP {} 帧（rms {:.1} mm）",
                    self.track_frames,
                    self.track_rms * 1000.0
                )
            } else {
                String::new()
            };
            self.recon_status = format!(
                "已生成 {triangles} 个三角形（帧 {}，{} {:.1} cm）{}{}",
                frame.json.frame,
                if self.recon_smooth {
                    "平滑"
                } else {
                    "体素"
                },
                self.recon_voxel * 100.0,
                if colored > 0 { "，彩色" } else { "" },
                tracking
            );
        } else {
            self.recon_status = "网格已不存在（场景可能被清空），请再点一次".to_string();
            self.recon_mesh = None;
            self.recon_object = None;
            self.recon_material = None;
            if let Some(old) = self.recon_texture.take() {
                engine.renderer.remove_texture(old);
            }
        }
    }

    /// 把最新一帧上传为 3D 背景；关闭开关时清掉背景。
    ///
    /// 返回是否成功设置。同一帧只上传一次；尺寸/帧号变化才重传。
    /// 不放宽文件协议：仍然只读 `latest.rgb`，没有摄像头时什么也不做。
    pub fn sync_background(
        &mut self,
        renderer: &mut quench3d_render::Renderer,
        gpu: &quench3d_render::GpuContext,
    ) -> bool {
        if !self.background_3d {
            if renderer.has_background() {
                renderer.clear_background();
            }
            self.background_frame = u64::MAX;
            return false;
        }
        if let Some(id) = self.latest_frame_id() {
            if id == self.background_frame && renderer.has_background() {
                return true;
            }
        }
        let Some(frame) = self.latest() else {
            return false;
        };
        let (w, h) = (frame.json.width, frame.json.height);
        let Some(expected) = (w as usize)
            .checked_mul(h as usize)
            .and_then(|n| n.checked_mul(3))
        else {
            return false;
        };
        if w == 0 || h == 0 || frame.rgb.len() != expected {
            return false;
        }
        if self.background_frame == frame.json.frame && renderer.has_background() {
            return true;
        }
        self.background_rgba.clear();
        self.background_rgba.reserve(w as usize * h as usize * 4);
        let (pixels, _) = frame.rgb.as_chunks::<3>();
        for px in pixels {
            self.background_rgba
                .extend_from_slice(&[px[0], px[1], px[2], 255]);
        }
        let ok = renderer.set_background_rgba(gpu, w, h, &self.background_rgba);
        if ok {
            self.background_frame = frame.json.frame;
        }
        ok
    }

    /// 由“视图 → 摄像头预览”打开窗口；不主动访问设备。
    pub fn toolbar(&mut self, _ui: &mut egui::Ui) {
        self.show = true;
    }

    fn stop_reader(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    fn clear_preview(&mut self) {
        if let Ok(mut error) = self.shared.error.lock() {
            *error = None;
        }
        if let Ok(mut latest) = self.shared.latest.lock() {
            *latest = None;
        }
        self.texture = None;
        self.depth_texture = None;
        self.background_3d = false;
        self.background_frame = u64::MAX;
        self.texture_frame = u64::MAX;
        self.depth_frame = u64::MAX;
        self.recon_auto = false;
    }

    fn start_capture(&mut self) {
        match self.capture.start(self.source, self.pose_capture) {
            Ok(dir) => {
                self.stop_reader();
                self.clear_preview();
                self.dir = dir;
                self.stop = Arc::new(AtomicBool::new(false));
                self.worker = spawn_worker(
                    self.dir.clone(),
                    self.shared.latest.clone(),
                    self.shared.error.clone(),
                    self.stop.clone(),
                );
            }
            Err(error) => self.capture.status = error,
        }
    }

    fn stop_capture(&mut self) {
        self.capture.stop();
        self.stop_reader();
        self.clear_preview();
    }

    /// 画浮动窗口；关闭窗口只隐藏，停止按钮才结束采集。
    pub fn window(&mut self, ctx: &egui::Context) {
        let was_running = self.capture.running();
        self.capture.poll();
        if was_running && !self.capture.running() {
            self.stop_reader();
            self.clear_preview();
        }
        if !self.show {
            return;
        }
        let mut open = self.show;
        egui::Window::new("摄像头预览")
            .open(&mut open)
            .default_pos(egui::pos2(40.0, 140.0))
            .default_width(380.0)
            .resizable(true)
            .show(ctx, |ui| self.contents(ui));
        self.show = open;
    }

    fn contents(&mut self, ui: &mut egui::Ui) {
        ui.label("普通摄像头预览在本机运行；关闭窗口只隐藏，停止按钮会释放摄像头。");
        ui.horizontal(|ui| {
            ui.label("设备编号");
            ui.add_enabled(
                !self.capture.running(),
                egui::DragValue::new(&mut self.source).range(0..=99),
            );
            if !self.capture.running() {
                if ui.button("开启摄像头").clicked() {
                    self.start_capture();
                }
            } else if ui.button("停止摄像头").clicked() {
                self.stop_capture();
            }
        });
        ui.add_enabled_ui(!self.capture.running(), |ui| {
            ui.checkbox(&mut self.pose_capture, "人体识别（姿态与轮廓）");
        });
        if !self.capture.status.is_empty() {
            ui.label(&self.capture.status);
        }
        let Some(frame) = self.latest() else {
            ui.add_space(4.0);
            ui.label("点击开启摄像头后，画面会显示在这里。");
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

        ui.checkbox(
            &mut self.background_3d,
            "作为 3D 视口背景（AR，模型叠在画面上）",
        )
        .on_hover_text("摄像头帧会铺在 3D 场景后面；天空被替换，模型仍正常受光/投影。");
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
            } else if let Some(points) = &frame.points {
                ui.label(
                    egui::RichText::new(format!(
                        "深度内参反投影 {} 点 · 采集端未写 latest.points",
                        points.len()
                    ))
                    .small()
                    .weak(),
                )
                .on_hover_text("用 latest.depth + intrinsics 现算，尺度仍是相对逆深度");
            } else {
                ui.label(
                    egui::RichText::new("没有点云（需要深度 + 内参，或 --points）")
                        .small()
                        .weak(),
                );
            }
        });
        ui.horizontal(|ui| {
            ui.label("重建体素");
            ui.add(
                egui::DragValue::new(&mut self.recon_voxel)
                    .range(0.01..=0.10)
                    .speed(0.005)
                    .suffix(" m"),
            );
            let enabled = frame.points.is_some();
            ui.checkbox(&mut self.recon_smooth, "平滑表面")
                .on_hover_text("Surface Nets 连续表面；关掉则是一个个扫描块");
            if ui
                .checkbox(&mut self.recon_accumulate, "时间累积")
                .on_hover_text("把最近约 90 帧点云按体素平均；慢速移动时更完整，快速移动会有拖影")
                .changed()
            {
                self.recon_accum = None;
                self.recon_frame = u64::MAX;
                self.reset_tracking();
            }
            if ui
                .checkbox(&mut self.recon_track, "跟踪位姿（ICP）")
                .on_hover_text(
                    "相邻帧点云刚体配准，把移动相机拼到同一参考系；只适合相邻帧小运动，失败会重置累积器",
                )
                .changed()
            {
                self.reset_tracking();
                self.recon_accum = None;
                self.recon_frame = u64::MAX;
                if self.recon_track {
                    self.recon_accumulate = true;
                }
            }
            if self.recon_track && !self.recon_accumulate {
                ui.label(RichText::new("跟踪位姿需要同时开「时间累积」").small().weak());
            }
            if ui
                .add_enabled(enabled, egui::Button::new("生成/更新网格"))
                .on_hover_text("把当前深度点云固化成体素表面；会作为一个可选中物体加到场景里")
                .clicked()
            {
                self.recon_requested = true;
            }
            ui.checkbox(&mut self.recon_auto, "实时跟随")
                .on_hover_text("每个新摄像头帧都重建一次；体素小/点数多时明显吃 CPU");
        });
        if !self.recon_status.is_empty() {
            ui.label(egui::RichText::new(&self.recon_status).small().weak());
        }
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
        self.capture.stop();
        self.stop_reader();
    }
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
    // 采集端可以只写深度（`--no-points`）：读端用内参 + z 映射补出同一点云。
    // 没有内参（fx/fy <= 0）时不猜，保持旧行为 `points = None`。
    let points = if json.points.present {
        points
    } else {
        points.or_else(|| {
            let depth = depth.as_deref()?;
            depth_to_points(&json, depth).ok()
        })
    };
    Ok(Some(FeedFrame {
        json,
        rgb,
        depth,
        points,
        loaded_at: Instant::now(),
    }))
}

/// 用 u8 相对深度 + 针孔内参反投影成引擎坐标点云。
///
/// 与采集端 `camera_mocap.py::depth_to_points` 同一套约定：u8=255 → `z_near`，
/// u8=0 → `z_far`；像素 `(x, y)` 按 `(x-cx)/fx`、`-(y-cy)/fy` 反投影，再转成
/// 引擎 x 右 / y 上 / z 向屏幕外。桌面读文件和 Web 端外部注入深度共用它。
#[allow(clippy::too_many_arguments)]
pub(crate) fn depth_to_points_grid(
    depth: &[u8],
    width: usize,
    height: usize,
    fx: f32,
    fy: f32,
    cx: f32,
    cy: f32,
    z_near: f32,
    z_far: f32,
    origin: [f32; 3],
    point_step: u32,
) -> Result<Vec<Vec3>, String> {
    if !(fx.is_finite() && fy.is_finite() && fx > 0.0 && fy > 0.0) {
        return Err("缺少有效内参（fx/fy）".to_string());
    }
    if width == 0 || height == 0 {
        return Err("深度图尺寸不能为 0".to_string());
    }
    if depth.len() != width * height {
        return Err("深度图长度与画面尺寸不符".to_string());
    }
    let step = usize::try_from(point_step)
        .ok()
        .filter(|s| *s > 0)
        .unwrap_or(6);
    if !(z_near.is_finite() && z_far.is_finite() && z_far > z_near) {
        return Err("深度 z_near/z_far 非法".to_string());
    }
    if !(cx.is_finite() && cy.is_finite()) {
        return Err("深度内参 cx/cy 非法".to_string());
    }
    if !origin.iter().all(|v| v.is_finite()) {
        return Err("深度原点含非有限值".to_string());
    }
    let xs: Vec<usize> = (0..width).step_by(step).collect();
    let ys: Vec<usize> = (0..height).step_by(step).collect();
    let count = xs
        .len()
        .checked_mul(ys.len())
        .ok_or_else(|| "深度重建点数溢出".to_string())?;
    if count > 1_000_000 {
        return Err(format!("深度重建点数过多：{count}"));
    }
    let mut out = Vec::with_capacity(count);
    for &y in &ys {
        for &x in &xs {
            let d = f32::from(depth[y * width + x]) / 255.0;
            let z = z_near + (1.0 - d) * (z_far - z_near);
            let gx = (x as f32 - cx) / fx * z;
            let gy = -(y as f32 - cy) / fy * z;
            out.push(Vec3::new(gx + origin[0], gy + origin[1], -z + origin[2]));
        }
    }
    Ok(out)
}

/// 用 `latest.json` 的内参与深度映射把 u8 相对深度反投影成引擎坐标点云。
#[cfg(not(target_arch = "wasm32"))]
fn depth_to_points(json: &FeedJson, depth: &[u8]) -> Result<Vec<Vec3>, String> {
    let intrinsics = &json.intrinsics;
    depth_to_points_grid(
        depth,
        json.width as usize,
        json.height as usize,
        intrinsics.fx,
        intrinsics.fy,
        intrinsics.cx,
        intrinsics.cy,
        json.depth.z_near.unwrap_or(0.3),
        json.depth.z_far.unwrap_or(3.0),
        json.depth.origin.unwrap_or([2.2, 1.0, 0.0]),
        json.depth.point_step,
    )
}

/// 按摄像头画面给每个点云顶点取色；无内参/无 RGB/长度不符时返回 `None`，
/// 调用方退回原来的青色十字。
#[cfg(not(target_arch = "wasm32"))]
fn sample_point_colors(
    points: &[Vec3],
    rgb: &[u8],
    projection: &crate::reconstruct::CameraProjection,
) -> Option<Vec<quench3d_math::Vec4>> {
    if !projection.is_valid()
        || rgb.len() != projection.width as usize * projection.height as usize * 3
    {
        return None;
    }
    let fallback = quench3d_math::Vec4::new(0.3, 0.85, 1.0, 0.85);
    let mut out = Vec::with_capacity(points.len());
    for p in points {
        let Some(uv) = projection.uv(*p) else {
            out.push(fallback);
            continue;
        };
        // `CameraProjection::uv` 返回的是纹素中心（已带 +0.5）；减 0.5 再取整
        // 才能落回原像素，而不是偏到右/下一格。
        let x = (uv.x * projection.width as f32 - 0.5)
            .floor()
            .clamp(0.0, projection.width.saturating_sub(1) as f32) as usize;
        let y = (uv.y * projection.height as f32 - 0.5)
            .floor()
            .clamp(0.0, projection.height.saturating_sub(1) as f32) as usize;
        let i = (y * projection.width as usize + x) * 3;
        let r = quench3d::render::color::srgb_to_linear(rgb[i] as f32 / 255.0);
        let g = quench3d::render::color::srgb_to_linear(rgb[i + 1] as f32 / 255.0);
        let b = quench3d::render::color::srgb_to_linear(rgb[i + 2] as f32 / 255.0);
        out.push(quench3d_math::Vec4::new(r, g, b, 0.95));
    }
    Some(out)
}

/// 把点云画成小十字。深度反投影没有拓扑，点本身就是最小可读单元。
fn add_point_lines(
    lines: &mut DebugLines,
    points: &[Vec3],
    colors: Option<&[quench3d_math::Vec4]>,
) {
    let fallback = quench3d_math::Vec4::new(0.3, 0.85, 1.0, 0.85);
    let d = 0.006;
    for (i, p) in points.iter().enumerate() {
        let color = colors.and_then(|c| c.get(i).copied()).unwrap_or(fallback);
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
fn world_to_scene(lm: Landmark) -> Vec3 {
    Vec3::new(
        lm.x + MOCAP_ORIGIN.x,
        -lm.y + MOCAP_ORIGIN.y,
        -lm.z + MOCAP_ORIGIN.z,
    )
}

#[cfg(target_arch = "wasm32")]
use std::{cell::RefCell, rc::Rc, time::Duration};

#[cfg(target_arch = "wasm32")]
use egui::{ColorImage, TextureHandle, TextureOptions};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::{JsCast, JsValue};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::{spawn_local, JsFuture};
#[cfg(target_arch = "wasm32")]
use web_sys::{
    CanvasRenderingContext2d, HtmlCanvasElement, HtmlMediaElement, HtmlVideoElement, MediaStream,
    MediaStreamConstraints,
};

/// 把归一化姿态点映射到预览图矩形（越界夹到图像内）。
///
/// 抽成平台无关函数：桌面/原生测试也能验证这条映射，不必起浏览器。
pub fn pose_screen_pos(rect: egui::Rect, x: f32, y: f32) -> egui::Pos2 {
    let x = if x.is_finite() { x } else { 0.5 };
    let y = if y.is_finite() { y } else { 0.5 };
    egui::pos2(
        rect.left() + rect.width() * x.clamp(0.0, 1.0),
        rect.top() + rect.height() * y.clamp(0.0, 1.0),
    )
}

/// 外部注入的一帧 2D 姿态（归一化图像坐标）。
#[cfg(target_arch = "wasm32")]
#[derive(Clone, Debug, Default, serde::Deserialize)]
struct WebPoseJson {
    #[serde(default)]
    present: bool,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    landmarks: Vec<WebLandmarkJson>,
    /// 可选的世界坐标 33 点；给了就在 3D 视口里画骨架（与桌面管线同一套连接表）。
    #[serde(default)]
    world: Vec<Landmark>,
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Copy, Debug, serde::Deserialize)]
struct WebLandmarkJson {
    #[serde(default)]
    x: f32,
    #[serde(default)]
    y: f32,
    #[serde(default = "default_pose_visibility")]
    visibility: f32,
}

#[cfg(target_arch = "wasm32")]
fn default_pose_visibility() -> f32 {
    1.0
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    static WEB_POSE: RefCell<Option<WebPoseJson>> = const { RefCell::new(None) };
}

/// 接收外部 JS 注入的姿态 JSON（MediaPipe 33 点的子集即可）。
///
/// 缺字段/坏 JSON 只记日志并清空，不会让页面崩；`present:false` 或点数不足 2
/// 视为"没有骨架"。调用方（页面）负责把 `window.quench3dCameraPose` 接到推理结果。
#[cfg(target_arch = "wasm32")]
pub fn set_web_pose(json: &str) {
    let parsed: Option<WebPoseJson> =
        serde_json::from_str(json)
            .ok()
            .and_then(|pose: WebPoseJson| {
                (pose.present && (pose.landmarks.len() >= 2 || pose.world.len() >= 2))
                    .then_some(pose)
            });
    if parsed.is_none() {
        web_sys::console::warn_1(
            &"quench3d camera: 忽略无效姿态 JSON（需要 present + >=2 landmarks）".into(),
        );
    }
    WEB_POSE.with(|slot| *slot.borrow_mut() = parsed);
}

/// 清掉外部姿态（页面切换/推理停止时调用）。
#[cfg(target_arch = "wasm32")]
pub fn clear_web_pose() {
    WEB_POSE.with(|slot| *slot.borrow_mut() = None);
}

/// 当前是否有可用姿态。
#[cfg(target_arch = "wasm32")]
pub fn has_web_pose() -> bool {
    WEB_POSE.with(|slot| slot.borrow().is_some())
}

#[cfg(target_arch = "wasm32")]
fn web_pose() -> Option<WebPoseJson> {
    WEB_POSE.with(|slot| slot.borrow().clone())
}

/// 把重建表面画成线框（Web 端没有场景网格时也能看到表面）。
#[cfg(target_arch = "wasm32")]
fn add_surface_lines(lines: &mut DebugLines, triangles: &[[Vec3; 3]]) {
    let color = quench3d_math::Vec4::new(0.95, 0.75, 0.25, 0.9);
    for t in triangles.iter().take(20_000) {
        lines.line(t[0], t[1], color);
        lines.line(t[1], t[2], color);
        lines.line(t[2], t[0], color);
    }
}

#[cfg(target_arch = "wasm32")]
fn draw_web_pose(ui: &egui::Ui, rect: egui::Rect, pose: &WebPoseJson) {
    let painter = ui.painter_at(rect);
    let map = |lm: &WebLandmarkJson| pose_screen_pos(rect, lm.x, lm.y);
    for &(a, b) in POSE_CONNECTIONS.iter() {
        let (Some(a), Some(b)) = (pose.landmarks.get(a), pose.landmarks.get(b)) else {
            continue;
        };
        if a.visibility < 0.05 || b.visibility < 0.05 {
            continue;
        }
        painter.line_segment(
            [map(a), map(b)],
            egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 40, 255)),
        );
    }
    for lm in &pose.landmarks {
        if lm.visibility < 0.05 {
            continue;
        }
        painter.circle_filled(map(lm), 2.5, egui::Color32::from_rgb(255, 230, 80));
    }
    painter.text(
        rect.left_top() + egui::vec2(6.0, 6.0),
        egui::Align2::LEFT_TOP,
        format!("外部骨架 {:.2}", pose.confidence),
        egui::FontId::monospace(11.0),
        egui::Color32::from_rgb(255, 230, 80),
    );
}

/// Web 端外部注入的深度帧与重建结果。
#[cfg(target_arch = "wasm32")]
struct WebDepthFrame {
    frame: u64,
    /// 最近一帧的原始点云（关闭时间累积时回到它）。
    raw: Vec<Vec3>,
    /// 当前用于显示/重建的点云（开启累积时是累积结果）。
    points: Vec<Vec3>,
    triangles: Vec<[Vec3; 3]>,
}

#[cfg(target_arch = "wasm32")]
thread_local! {
    static WEB_DEPTH: RefCell<Option<WebDepthFrame>> = const { RefCell::new(None) };
    static WEB_DEPTH_ACCUM: RefCell<Option<PointCloudAccumulator>> = const { RefCell::new(None) };
    static WEB_DEPTH_ACCUM_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static WEB_DEPTH_TRACK: RefCell<Option<WebTracking>> = const { RefCell::new(None) };
    static WEB_DEPTH_TRACK_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Web 端相邻帧 ICP 的参考帧与累计位姿。
#[cfg(target_arch = "wasm32")]
struct WebTracking {
    /// 上一帧原始点云（相机坐标，未搬到参考系）。
    previous: Vec<Vec3>,
    /// 当前相机 → 参考帧的累计旋转。
    rotation: quench3d_math::Quat,
    /// 当前相机 → 参考帧的累计平移。
    translation: Vec3,
    /// 成功配准的帧数（第一帧算 1）。
    frames: u64,
    /// 最近一次 rms（米）；只有一帧时是 +∞。
    rms: f32,
}

#[cfg(target_arch = "wasm32")]
impl WebTracking {
    fn new(points: &[Vec3]) -> Self {
        Self {
            previous: points.to_vec(),
            rotation: quench3d_math::Quat::IDENTITY,
            translation: Vec3::ZERO,
            frames: 1,
            rms: f32::INFINITY,
        }
    }
}

/// 开关 Web 深度时间累积之上的 ICP 位姿跟踪。
///
/// 打开时会同时打开时间累积（没有累积器时跟踪结果无处可放），并清空旧参考帧；
/// 关闭只关跟踪，不动累积。缺省关，关时行为与旧版完全一致。
#[cfg(target_arch = "wasm32")]
pub fn set_web_tracking(enabled: bool) {
    WEB_DEPTH_TRACK_ON.with(|c| c.set(enabled));
    WEB_DEPTH_TRACK.with(|slot| *slot.borrow_mut() = None);
    if enabled && !web_accumulation_enabled() {
        set_web_accumulation(true);
    }
}

/// 当前 Web ICP 是否开启。
#[cfg(target_arch = "wasm32")]
pub fn web_tracking_enabled() -> bool {
    WEB_DEPTH_TRACK_ON.with(|c| c.get())
}

/// Web ICP 诊断：`(是否开启, 已配准帧数, 最近 rms 米)`。
#[cfg(target_arch = "wasm32")]
pub fn web_tracking_status() -> (bool, u64, f32) {
    let enabled = web_tracking_enabled();
    WEB_DEPTH_TRACK.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or((enabled, 0, f32::INFINITY), |t| (enabled, t.frames, t.rms))
    })
}

/// 开关 Web 深度时间累积（最近约 90 帧、2 cm 体素平均）。
///
/// 与桌面「时间累积」同一套 `PointCloudAccumulator`；缺省关，关时每帧只显示
/// 当前深度，旧行为不变。开启/关闭都会清掉重建表面，避免拿旧点云画新表面。
#[cfg(target_arch = "wasm32")]
pub fn set_web_accumulation(enabled: bool) {
    WEB_DEPTH_ACCUM_ON.with(|c| c.set(enabled));
    if enabled {
        let seed = WEB_DEPTH.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|d| (d.frame, d.raw.clone()))
                .filter(|(_, p)| !p.is_empty())
        });
        WEB_DEPTH_ACCUM.with(|slot| {
            let mut accum = PointCloudAccumulator::new(0.02, 200_000, 90);
            if let Some((frame, points)) = seed {
                accum.merge(&points, frame);
            }
            *slot.borrow_mut() = Some(accum);
        });
    } else {
        WEB_DEPTH_ACCUM.with(|slot| *slot.borrow_mut() = None);
        // 累积关了，跟踪结果没有容器；同时关掉跟踪，避免 UI 显示“已跟踪”
        // 但实际每帧都回到单帧。
        WEB_DEPTH_TRACK_ON.with(|c| c.set(false));
        WEB_DEPTH_TRACK.with(|slot| *slot.borrow_mut() = None);
        WEB_DEPTH.with(|slot| {
            if let Some(d) = slot.borrow_mut().as_mut() {
                if !d.raw.is_empty() {
                    d.points = d.raw.clone();
                }
                d.triangles.clear();
            }
        });
    }
}

/// 当前是否开启 Web 深度时间累积。
#[cfg(target_arch = "wasm32")]
pub fn web_accumulation_enabled() -> bool {
    WEB_DEPTH_ACCUM_ON.with(|c| c.get())
}

/// 注入一帧 u8 深度图，按针孔内参反投影成引擎坐标点云。
///
/// 参数约定与桌面 `latest.depth` 相同：u8=255 → `z_near`，u8=0 → `z_far`。
/// 浏览器普通 RGB 摄像头没有深度；这里只接外部提供深度的源（如 WebXR Depth
/// Sensing / 服务端推流），不会假装平面视频能猜出可靠深度。
#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
pub fn set_web_depth(
    width: u32,
    height: u32,
    depth: &[u8],
    fx: f32,
    fy: f32,
    cx: f32,
    cy: f32,
    z_near: f32,
    z_far: f32,
    step: u32,
    origin: [f32; 3],
) -> Result<(), String> {
    let points = depth_to_points_grid(
        depth,
        width as usize,
        height as usize,
        fx,
        fy,
        cx,
        cy,
        z_near,
        z_far,
        origin,
        step,
    )?;
    let frame = WEB_DEPTH.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or(1, |d| d.frame.wrapping_add(1))
    });
    let raw = points.clone();
    let points = if web_accumulation_enabled() {
        // ICP 与桌面同一个组合函数：把当前帧搬到参考系后再进体素累积器。
        // 跟踪失败会返回 `reset = true`，这里必须连累积器一起重建，不能把两个
        // 坐标系混在同一个网格里。
        let (tracked, reset) = if web_tracking_enabled() {
            let mut reset = false;
            let tracked = WEB_DEPTH_TRACK.with(|slot| {
                let mut guard = slot.borrow_mut();
                let Some(tracking) = guard.as_mut() else {
                    *guard = Some(WebTracking::new(&raw));
                    return raw.clone();
                };
                match compose_tracked_points(
                    &tracking.previous,
                    &raw,
                    &mut tracking.rotation,
                    &mut tracking.translation,
                ) {
                    Some((points, rms)) => {
                        tracking.previous = raw.clone();
                        tracking.frames = tracking.frames.saturating_add(1);
                        tracking.rms = rms;
                        points
                    }
                    None => {
                        // 无重叠/运动太大：以当前帧重开参考系，而不是丢掉这帧。
                        reset = true;
                        *guard = Some(WebTracking::new(&raw));
                        raw.clone()
                    }
                }
            });
            (tracked, reset)
        } else {
            (raw.clone(), false)
        };
        if reset {
            WEB_DEPTH_ACCUM.with(|slot| {
                *slot.borrow_mut() = Some(PointCloudAccumulator::new(0.02, 200_000, 90));
            });
        }
        WEB_DEPTH_ACCUM.with(|slot| {
            if slot.borrow().is_none() {
                *slot.borrow_mut() = Some(PointCloudAccumulator::new(0.02, 200_000, 90));
            }
            let mut guard = slot.borrow_mut();
            if let Some(accum) = guard.as_mut() {
                accum.merge(&tracked, frame);
                accum.points()
            } else {
                tracked
            }
        })
    } else {
        points
    };
    WEB_DEPTH.with(|slot| {
        *slot.borrow_mut() = Some(WebDepthFrame {
            frame,
            raw,
            points,
            triangles: Vec::new(),
        });
    });
    Ok(())
}

/// 清空外部深度与重建表面。
#[cfg(target_arch = "wasm32")]
pub fn clear_web_depth() {
    WEB_DEPTH.with(|slot| *slot.borrow_mut() = None);
    WEB_DEPTH_ACCUM.with(|slot| *slot.borrow_mut() = None);
    WEB_DEPTH_TRACK.with(|slot| *slot.borrow_mut() = None);
}

/// 当前注入点云/表面计数，给页面与探针显示。
#[cfg(target_arch = "wasm32")]
pub fn web_depth_counts() -> (usize, usize) {
    WEB_DEPTH.with(|slot| {
        slot.borrow()
            .as_ref()
            .map_or((0, 0), |d| (d.points.len(), d.triangles.len()))
    })
}

#[cfg(target_arch = "wasm32")]
fn web_depth_points() -> Option<Vec<Vec3>> {
    WEB_DEPTH.with(|slot| slot.borrow().as_ref().map(|d| d.points.clone()))
}

#[cfg(target_arch = "wasm32")]
fn web_depth_surface() -> Option<Vec<[Vec3; 3]>> {
    WEB_DEPTH.with(|slot| slot.borrow().as_ref().map(|d| d.triangles.clone()))
}

/// 从当前注入点云重建体素表面（Web 端没有场景网格，用线框显示）。
#[cfg(target_arch = "wasm32")]
pub fn rebuild_web_depth_surface(voxel: f32) -> Result<usize, String> {
    let points = WEB_DEPTH.with(|slot| {
        slot.borrow()
            .as_ref()
            .map(|d| d.points.clone())
            .ok_or_else(|| "还没有注入深度".to_string())
    })?;
    if points.len() < 8 {
        return Err("点太少，无法重建表面".to_string());
    }
    let mesh = crate::reconstruct::voxel_surface_robust(&points, voxel.clamp(0.002, 0.5));
    if mesh.indices.is_empty() {
        return Err("没有生成面（体素可能太大）".to_string());
    }
    let mut triangles = Vec::with_capacity(mesh.indices.len() / 3);
    for tri in mesh.indices.as_chunks::<3>().0 {
        let Some(a) = mesh.vertices.get(tri[0] as usize) else {
            continue;
        };
        let Some(b) = mesh.vertices.get(tri[1] as usize) else {
            continue;
        };
        let Some(c) = mesh.vertices.get(tri[2] as usize) else {
            continue;
        };
        triangles.push([
            Vec3::from_array(a.position),
            Vec3::from_array(b.position),
            Vec3::from_array(c.position),
        ]);
    }
    let count = triangles.len();
    WEB_DEPTH.with(|slot| {
        if let Some(d) = slot.borrow_mut().as_mut() {
            d.triangles = triangles;
        }
    });
    Ok(count)
}

#[cfg(target_arch = "wasm32")]
#[derive(Default)]
struct WebCameraState {
    video: Option<HtmlVideoElement>,
    canvas: Option<HtmlCanvasElement>,
    context: Option<CanvasRenderingContext2d>,
    error: Option<String>,
    starting: bool,
    /// 最近一帧的 RGBA（给 3D 背景用，和 egui 预览纹理分开存）。
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    /// 帧序号：同一帧不重复上传 GPU 纹理。
    frame: u64,
}

/// Web 端摄像头**预览**：`getUserMedia` → `<video>` → 2D canvas 采样 → egui 纹理。
///
/// 浏览器里不读桌面文件协议，也不跑 MediaPipe；骨架/轮廓/深度仍由桌面 Python
/// 管线负责。权限被拒、非安全上下文（非 https/localhost）或缺摄像头时把原因
/// 显示在窗口里，不 panic。
#[cfg(target_arch = "wasm32")]
pub struct CameraFeed {
    state: Rc<RefCell<WebCameraState>>,
    texture: Option<TextureHandle>,
    show: bool,
    /// `?camera=1` 调试入口：加载后自动开窗并申请权限（配合无头 Chrome 假摄像头）。
    autostart: bool,
    /// 是否把摄像头画面作为 3D 视口背景。
    background_3d: bool,
    /// 已上传背景的帧序号。
    background_frame: u64,
    /// 是否画外部注入深度的点云。
    show_points: bool,
    /// 是否画外部深度重建的线框表面。
    show_surface: bool,
    /// 最近一次重建结果/错误。
    depth_status: String,
}

#[cfg(target_arch = "wasm32")]
impl Default for CameraFeed {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_arch = "wasm32")]
impl CameraFeed {
    /// 新建空预览。
    pub fn new() -> Self {
        let autostart = web_sys::window()
            .and_then(|w| w.location().search().ok())
            .is_some_and(|search| search.split('&').any(|part| part == "camera=1"));
        Self {
            state: Rc::new(RefCell::new(WebCameraState::default())),
            texture: None,
            show: autostart,
            autostart,
            background_3d: false,
            background_frame: u64::MAX,
            show_points: true,
            show_surface: false,
            depth_status: String::new(),
        }
    }

    /// 顶部工具栏入口：打开预览窗口（真正申请权限在窗口里的按钮）。
    pub fn toolbar(&mut self, ui: &mut egui::Ui) {
        if ui
            .button("摄像头")
            .on_hover_text("浏览器摄像头预览（getUserMedia；外部 JS 可注入 2D 骨架）")
            .clicked()
        {
            self.show = true;
        }
    }

    /// 无 3D 骨架可叠；Web 端只画预览图上的外部注入 2D 骨架。
    pub fn add_lines(&self, lines: &mut DebugLines) {
        if self.show_points {
            if let Some(points) = web_depth_points() {
                add_point_lines(lines, &points, None);
            }
        }
        if self.show_surface {
            if let Some(triangles) = web_depth_surface() {
                add_surface_lines(lines, &triangles);
            }
        }
        if let Some(pose) = web_pose() {
            if pose.world.len() >= 2 {
                add_pose_lines(lines, &pose.world);
            }
        }
    }

    /// 当前姿态是否带 3D 世界点（只注入 2D 时为 false）。
    pub fn has_world_pose(&self) -> bool {
        web_pose().is_some_and(|pose| pose.world.len() >= 2)
    }

    /// 当前是否有外部 JS 注入的姿态。
    pub fn has_pose(&self) -> bool {
        has_web_pose()
    }

    fn start(&mut self, ctx: &egui::Context) {
        if self.state.borrow().starting {
            return;
        }
        self.state.borrow_mut().starting = true;
        self.state.borrow_mut().error = None;
        let state = Rc::clone(&self.state);
        let ctx = ctx.clone();
        spawn_local(async move {
            let result = open_camera().await;
            let mut state = state.borrow_mut();
            state.starting = false;
            match result {
                Ok(video) => state.video = Some(video),
                Err(e) => {
                    web_sys::console::error_1(&format!("quench3d camera: {e}").into());
                    state.error = Some(e);
                }
            }
            ctx.request_repaint();
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let (video, width, height) = {
            let state = self.state.borrow();
            let Some(video) = state.video.clone() else {
                return;
            };
            let width = video.video_width();
            let height = video.video_height();
            (video, width, height)
        };
        if width == 0 || height == 0 {
            ctx.request_repaint_after(Duration::from_millis(50));
            return;
        }

        let (canvas, context) = {
            let mut state = self.state.borrow_mut();
            if state.canvas.is_none() {
                let Some(document) = web_sys::window().and_then(|w| w.document()) else {
                    state.error = Some("没有 document".to_string());
                    return;
                };
                let Ok(canvas) = document.create_element("canvas") else {
                    state.error = Some("创建 canvas 失败".to_string());
                    return;
                };
                let Ok(canvas) = canvas.dyn_into::<HtmlCanvasElement>() else {
                    state.error = Some("canvas 类型不对".to_string());
                    return;
                };
                let Some(context) = canvas
                    .get_context("2d")
                    .ok()
                    .flatten()
                    .and_then(|c| c.dyn_into::<CanvasRenderingContext2d>().ok())
                else {
                    state.error = Some("浏览器不支持 2D canvas".to_string());
                    return;
                };
                state.canvas = Some(canvas);
                state.context = Some(context);
            }
            let Some(canvas) = state.canvas.clone() else {
                return;
            };
            let Some(context) = state.context.clone() else {
                return;
            };
            (canvas, context)
        };

        canvas.set_width(width);
        canvas.set_height(height);
        let draw = context.draw_image_with_html_video_element(&video, 0.0, 0.0);
        let image =
            draw.and_then(|()| context.get_image_data(0.0, 0.0, width as f64, height as f64));
        let Ok(image_data) = image else {
            self.state.borrow_mut().error = Some("读取摄像头画面失败".to_string());
            return;
        };
        let pixels = image_data.data();
        {
            let mut state = self.state.borrow_mut();
            state.rgba.clear();
            state.rgba.extend_from_slice(&pixels[..]);
            state.width = width;
            state.height = height;
            state.frame = state.frame.wrapping_add(1);
        }
        let color =
            ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &pixels[..]);
        match self.texture.as_mut() {
            Some(texture) => texture.set(color, TextureOptions::LINEAR),
            None => {
                self.texture = Some(ctx.load_texture("web-camera", color, TextureOptions::LINEAR));
            }
        }
        ctx.request_repaint_after(Duration::from_millis(33));
    }

    /// 把最新一帧上传为 3D 视口背景（Web 端）。
    ///
    /// 与桌面端同义：只有勾选「作为 3D 背景」且已经有摄像头帧时才上传；
    /// 同一帧不重复写纹理，关掉开关会清空背景。
    pub fn sync_background(&mut self, renderer: &mut Renderer, gpu: &GpuContext) -> bool {
        if !self.background_3d {
            if renderer.has_background() {
                renderer.clear_background();
            }
            self.background_frame = u64::MAX;
            return false;
        }
        let state = self.state.borrow();
        if state.width == 0 || state.height == 0 || state.rgba.is_empty() {
            return false;
        }
        if state.frame == self.background_frame && renderer.has_background() {
            return true;
        }
        let ok = renderer.set_background_rgba(gpu, state.width, state.height, &state.rgba);
        if ok {
            self.background_frame = state.frame;
        }
        ok
    }

    /// 预览窗口；默认关闭，从顶部「摄像头」按钮打开。
    pub fn window(&mut self, ctx: &egui::Context) {
        if self.autostart {
            self.autostart = false;
            self.start(ctx);
        }
        if !self.show {
            return;
        }
        let mut open = self.show;
        egui::Window::new("摄像头（Web 预览）")
            .open(&mut open)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.checkbox(&mut self.background_3d, "作为 3D 背景")
                    .on_hover_text("把摄像头画面放到 3D 视口后面；需要先开启摄像头");
                let error = self.state.borrow().error.clone();
                let starting = self.state.borrow().starting;
                let has_video = self.state.borrow().video.is_some();
                if let Some(error) = error {
                    ui.colored_label(egui::Color32::LIGHT_RED, error);
                }
                if !has_video && !starting {
                    if ui.button("开启摄像头").clicked() {
                        self.start(ctx);
                    }
                    ui.label(
                        egui::RichText::new(
                            "需要 https 或 localhost；内置推理不在浏览器里，外部 JS 可用 window.quench3dCameraPose(json) 注入 2D/3D 骨架。",
                        )
                        .small()
                        .weak(),
                    );
                }
                if starting {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("等待浏览器授权…");
                    });
                }
                if has_video {
                    self.poll(ctx);
                    if let Some(texture) = &self.texture {
                        let size = texture.size_vec2();
                        let scale = (400.0 / size.x).min(1.0);
                        let image = ui.image((texture.id(), size * scale));
                        if let Some(pose) = web_pose() {
                            draw_web_pose(ui, image.rect, &pose);
                        }
                    }
                    ui.separator();
                    let mut accum = web_accumulation_enabled();
                    if ui
                        .checkbox(&mut accum, "深度时间累积（最近约 90 帧）")
                        .changed()
                    {
                        set_web_accumulation(accum);
                        self.depth_status.clear();
                    }
                    let mut track = web_tracking_enabled();
                    if ui
                        .checkbox(&mut track, "跟踪位姿（ICP，移动相机）")
                        .changed()
                    {
                        set_web_tracking(track);
                        self.depth_status.clear();
                    }
                    let (_, track_frames, track_rms) = web_tracking_status();
                    if track_frames > 0 && track_rms.is_finite() {
                        ui.label(
                            egui::RichText::new(format!(
                                "ICP {track_frames} 帧 · rms {:.1} mm",
                                track_rms * 1000.0
                            ))
                            .small()
                            .weak(),
                        );
                    }
                    ui.checkbox(&mut self.show_points, "显示注入深度的点云");
                    ui.horizontal(|ui| {
                        if ui.button("从注入深度重建表面").clicked() {
                            self.depth_status = match rebuild_web_depth_surface(0.02) {
                                Ok(n) => format!("已重建 {n} 个三角形"),
                                Err(e) => e,
                            };
                        }
                        ui.checkbox(&mut self.show_surface, "显示表面线框");
                    });
                    let (points, tris) = web_depth_counts();
                    ui.label(
                        egui::RichText::new(format!(
                            "深度点 {points} · 表面三角形 {tris} · {}",
                            self.depth_status
                        ))
                        .small()
                        .weak(),
                    );
                    ui.label(
                        egui::RichText::new("预览 + 外部 JS 注入的 2D 骨架 / 3D 世界点；浏览器普通摄像头没有深度，点云/表面需外部注入深度（set_camera_depth）")
                            .small()
                            .weak(),
                    );
                }
            });
        self.show = open;
    }
}

#[cfg(target_arch = "wasm32")]
async fn open_camera() -> Result<HtmlVideoElement, String> {
    let window = web_sys::window().ok_or("没有 window")?;
    let devices = window
        .navigator()
        .media_devices()
        .map_err(|e| format!("浏览器没有 getUserMedia（非安全上下文？）：{e:?}"))?;
    let constraints = MediaStreamConstraints::new();
    constraints.set_video(&JsValue::TRUE);
    let promise = devices
        .get_user_media_with_constraints(&constraints)
        .map_err(|e| format!("调用 getUserMedia 失败：{e:?}"))?;
    let stream = JsFuture::from(promise)
        .await
        .map_err(|e| format!("摄像头权限被拒或没有设备：{e:?}"))?;
    let stream = stream
        .dyn_into::<MediaStream>()
        .map_err(|_| "getUserMedia 没有返回 MediaStream".to_string())?;
    let document = window.document().ok_or("没有 document")?;
    let video = document
        .create_element("video")
        .map_err(|e| format!("创建 video 失败：{e:?}"))?
        .dyn_into::<HtmlVideoElement>()
        .map_err(|_| "video 类型不对".to_string())?;
    let media: &HtmlMediaElement = video.unchecked_ref();
    media.set_autoplay(true);
    media.set_muted(true);
    media.set_src_object(Some(&stream));
    let play = media
        .play()
        .map_err(|e| format!("启动 video 失败：{e:?}"))?;
    JsFuture::from(play)
        .await
        .map_err(|e| format!("video.play 被拒：{e:?}"))?;
    Ok(video)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn native_preview_buttons_start_and_stop_fake_worker() {
        fn text_rect(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<egui::Rect> {
            fn locate(shape: &egui::epaint::Shape, needle: &str) -> Option<egui::Rect> {
                match shape {
                    egui::epaint::Shape::Text(text) if text.galley.text() == needle => {
                        Some(text.galley.rect.translate(text.pos.to_vec2()))
                    }
                    egui::epaint::Shape::Vec(shapes) => {
                        shapes.iter().find_map(|s| locate(s, needle))
                    }
                    _ => None,
                }
            }
            shapes.iter().find_map(|s| locate(&s.shape, needle))
        }
        let mut feed = CameraFeed::new();
        let directory = std::env::temp_dir().join(format!(
            "camera-ui-fake-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut command =
            std::process::Command::new(if cfg!(windows) { "python" } else { "python3" });
        command.args(["-c", "import time; time.sleep(60)"]);
        feed.capture.test_command = Some((command, directory));
        feed.show = true;
        let ctx = egui::Context::default();
        let run = |feed: &mut CameraFeed, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 900.0),
                    )),
                    events,
                    focused: true,
                    ..Default::default()
                },
                |ui| {
                    feed.window(ui.ctx());
                },
            );
            output.textures_delta.clear();
            output.shapes
        };
        let click = |pos| {
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: Default::default(),
                },
            ]
        };
        run(&mut feed, vec![]);
        let shapes = run(&mut feed, vec![]);
        let start = text_rect(&shapes, "开启摄像头")
            .expect("start button visible")
            .center();
        run(&mut feed, click(start));
        assert!(feed.capture.running());
        feed.show = false;
        run(&mut feed, vec![]);
        assert!(
            feed.capture.running(),
            "hiding preview must not stop capture"
        );
        feed.show = true;
        let shapes = run(&mut feed, vec![]);
        let stop = text_rect(&shapes, "停止摄像头")
            .expect("stop button visible")
            .center();
        run(&mut feed, click(stop));
        assert!(!feed.capture.running());
        assert!(feed.worker.is_none());
        assert!(feed.latest().is_none());
        assert!(!feed.background_3d);
    }

    #[test]
    fn depth_grid_backprojects_in_engine_axes() {
        // u8=255 → z_near、u8=0 → z_far；x 向右、y 向上、z 向屏幕外（-z）。
        let pts = depth_to_points_grid(
            &[255, 0, 255, 0],
            2,
            2,
            1.0,
            1.0,
            0.0,
            0.0,
            0.5,
            2.0,
            [10.0, 20.0, 30.0],
            1,
        )
        .expect("反投影");
        assert_eq!(pts.len(), 4);
        assert_eq!(pts[0], Vec3::new(10.0, 20.0, 29.5));
        assert_eq!(pts[1], Vec3::new(12.0, 20.0, 28.0));
        assert_eq!(pts[2], Vec3::new(10.0, 19.5, 29.5));
        assert_eq!(pts[3], Vec3::new(12.0, 18.0, 28.0));
    }

    #[test]
    fn pose_screen_pos_maps_and_clamps() {
        let rect = egui::Rect::from_min_size(egui::Pos2::new(10.0, 20.0), egui::vec2(200.0, 100.0));
        assert_eq!(pose_screen_pos(rect, 0.0, 0.0), egui::pos2(10.0, 20.0));
        assert_eq!(pose_screen_pos(rect, 1.0, 1.0), egui::pos2(210.0, 120.0));
        assert_eq!(pose_screen_pos(rect, 0.5, 0.5), egui::pos2(110.0, 70.0));
        assert_eq!(pose_screen_pos(rect, -1.0, 2.0), egui::pos2(10.0, 120.0));
        assert_eq!(
            pose_screen_pos(rect, f32::NAN, 0.5),
            egui::pos2(110.0, 70.0)
        );
    }

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
        assert!(frame.points.is_none(), "没有内参时不能猜点云");
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

    fn depth_only_json(step: u32) -> FeedJson {
        let text = format!(
            r#"{{"schema":"quench3d.mocap/1","version":1,"frame":1,
            "timestamp_ms":0,"source":"test","width":2,"height":2,"fps":30,
            "pose":{{"present":false}},"silhouette":{{"present":false}},
            "intrinsics":{{"fx":2.0,"fy":2.0,"cx":0.5,"cy":0.5}},
            "depth":{{"present":true,"kind":"relative_inverse_depth",
              "point_step":{step},"z_near":0.3,"z_far":3.0,"origin":[0.0,0.0,0.0]}}}}"#
        );
        serde_json::from_str(&text).expect("解析")
    }

    #[test]
    fn depth_to_points_matches_the_producer_convention() {
        let json = depth_only_json(1);
        let points = depth_to_points(&json, &[255, 0, 255, 0]).expect("反投影");
        assert_eq!(points.len(), 4);
        let close = |a: f32, b: f32| (a - b).abs() < 1e-5;
        // 像素 (0,0)：d=255 → z=0.3，gx=-0.075，gy=+0.075，输出 z=-0.3
        assert!(close(points[0].x, -0.075), "{:?}", points[0]);
        assert!(close(points[0].y, 0.075), "{:?}", points[0]);
        assert!(close(points[0].z, -0.3), "{:?}", points[0]);
        // 像素 (1,0)：d=0 → z=3.0，gx=gy=0.75
        assert!(close(points[1].x, 0.75), "{:?}", points[1]);
        assert!(close(points[1].y, 0.75), "{:?}", points[1]);
        assert!(close(points[1].z, -3.0), "{:?}", points[1]);
        // 像素 (1,1)：gy 取负
        assert!(close(points[3].y, -0.75), "{:?}", points[3]);
    }

    #[test]
    fn depth_to_points_honors_step_and_rejects_missing_intrinsics() {
        let json = depth_only_json(2);
        let points = depth_to_points(&json, &[255, 0, 255, 0]).expect("反投影");
        assert_eq!(points.len(), 1, "2×2 图步长 2 只采 (0,0)");
        let mut missing = depth_only_json(1);
        missing.intrinsics.fx = 0.0;
        let err = depth_to_points(&missing, &[255, 0, 255, 0]).expect_err("缺内参必须拒绝");
        assert!(err.contains("内参"), "{err}");
    }

    #[test]
    fn read_frame_reconstructs_points_from_depth_without_a_points_file() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "quench3d-camera-depth-only-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("建目录");
        let json = r#"{"schema":"quench3d.mocap/1","version":1,"frame":3,
            "timestamp_ms":0,"source":"test","width":2,"height":2,"fps":30,
            "pose":{"present":false},"silhouette":{"present":false},
            "intrinsics":{"fx":2.0,"fy":2.0,"cx":0.5,"cy":0.5},
            "depth":{"present":true,"kind":"relative_inverse_depth",
              "point_step":1,"z_near":0.3,"z_far":3.0,"origin":[0.0,0.0,0.0]},
            "points":{"present":false}}"#;
        std::fs::write(dir.join("latest.json"), json).expect("写 json");
        std::fs::write(dir.join("latest.rgb"), [0u8; 12]).expect("写 rgb");
        std::fs::write(dir.join("latest.depth"), [255u8, 0, 255, 0]).expect("写 depth");
        let frame = read_frame(&dir).expect("读").expect("有帧");
        assert!(!frame.json.points.present);
        let points = frame.points.expect("深度应有内参反投影点云");
        assert_eq!(points.len(), 4);
        assert!((points[0].z + 0.3).abs() < 1e-5, "{:?}", points[0]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tracking_composes_increments_into_the_reference_frame() {
        fn patch(shift: Vertex) -> Vec<Vec3> {
            let mut out = Vec::new();
            for i in 0..18 {
                for j in 0..18 {
                    let x = (i as f32 - 8.5) * 0.05;
                    let y = (j as f32 - 8.5) * 0.05;
                    let z = 0.06 * (x * 6.0).sin() + 0.03 * (y * 5.0).cos();
                    out.push(Vec3::new(x + shift.0, y + shift.1, z + shift.2));
                }
            }
            out
        }
        type Vertex = (f32, f32, f32);
        let reference = patch((0.0, 0.0, 0.0));
        let first = patch((0.004, 0.0, 0.0));
        let second = patch((0.008, 0.0, 0.0));
        let mut rotation = quench3d_math::Quat::IDENTITY;
        let mut translation = Vec3::ZERO;
        let (mapped_first, rms_first) =
            compose_tracked_points(&reference, &first, &mut rotation, &mut translation)
                .expect("第一段应配准");
        assert!(rms_first < 0.002, "第一段 rms 太大：{rms_first}");
        let (mapped_second, rms_second) =
            compose_tracked_points(&first, &second, &mut rotation, &mut translation)
                .expect("第二段应配准");
        assert!(rms_second < 0.002, "第二段 rms 太大：{rms_second}");
        let mut worst = 0.0f32;
        for (p, q) in reference.iter().zip(&mapped_second) {
            worst = worst.max((*p - *q).length());
        }
        assert!(worst < 0.006, "两段增量叠到参考系后误差应 < 6 mm：{worst}");
        // 第一段映射也必须仍然落在参考系，而不是只把第二段硬贴回去。
        for (p, q) in reference.iter().zip(&mapped_first) {
            assert!((*p - *q).length() < 0.006);
        }
    }

    #[test]
    fn point_cloud_draws_three_cross_segments_per_point() {
        let mut lines = DebugLines::new();
        add_point_lines(&mut lines, &[Vec3::ZERO, Vec3::new(1.0, 2.0, 3.0)], None);
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
    fn missing_promised_points_file_is_not_silently_rebuilt_from_depth() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "quench3d-camera-missing-points-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("建目录");
        let json = r#"{"schema":"quench3d.mocap/1","version":1,"frame":4,
            "timestamp_ms":0,"source":"test","width":2,"height":2,"fps":30,
            "pose":{"present":false},"silhouette":{"present":false},
            "intrinsics":{"fx":2.0,"fy":2.0,"cx":0.5,"cy":0.5},
            "depth":{"present":true,"point_step":1,"z_near":0.3,"z_far":3.0,
              "origin":[0.0,0.0,0.0]},
            "points":{"present":true,"path":"latest.points","count":4}}"#;
        std::fs::write(dir.join("latest.json"), json).expect("写 json");
        std::fs::write(dir.join("latest.rgb"), [0u8; 12]).expect("写 rgb");
        std::fs::write(dir.join("latest.depth"), [255u8; 4]).expect("写 depth");
        let frame = read_frame(&dir).expect("读").expect("有帧");
        assert!(
            frame.points.is_none(),
            "采集端声明了 latest.points 但文件缺失时，不能悄悄改用深度"
        );
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
    #[test]
    fn reconstruction_uses_camera_rgb_as_a_base_color_texture() {
        if std::env::var_os("QUENCH3D_GPU_TESTS").is_none() {
            return;
        }
        let gpu = match pollster::block_on(quench3d::render::GpuContext::headless(
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
        let config = quench3d::EngineConfig {
            width: 64,
            height: 64,
            msaa_samples: 1,
            ..Default::default()
        };
        let mut engine = Engine::new(gpu, &config);
        let mut feed = CameraFeed::new();
        let json = FeedJson {
            frame: 1,
            width: 64,
            height: 64,
            depth: DepthJson {
                // 点云是相机坐标 + origin；这里用原点，投影更容易手算。
                origin: Some([0.0, 0.0, 0.0]),
                ..DepthJson::default()
            },
            intrinsics: IntrinsicsJson {
                fx: 50.0,
                fy: 50.0,
                cx: 32.0,
                cy: 32.0,
            },
            ..FeedJson::default()
        };
        let mut points = Vec::new();
        for z in -10..=0 {
            for y in -4..=4 {
                for x in -4..=4 {
                    points.push(Vec3::new(x as f32 * 0.04, y as f32 * 0.04, z as f32 * 0.04));
                }
            }
        }
        let mut rgb = vec![0u8; 64 * 64 * 3];
        for (i, px) in rgb.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            // 上半屏红、下半屏绿：只要有一半被采样到，就说明不是纯色路径。
            let y = i / 64;
            *px = if y < 32 { [230, 20, 20] } else { [20, 230, 20] };
        }
        let frame = FeedFrame {
            json,
            rgb,
            depth: None,
            points: Some(points),
            loaded_at: std::time::Instant::now(),
        };
        feed.set_latest_for_test(Arc::new(frame));
        feed.recon_smooth = false;
        feed.recon_voxel = 0.04;
        feed.build_reconstruction(&mut engine);
        assert_eq!(engine.scene.objects.len(), 1, "应生成一个重建物体");
        let object = &engine.scene.objects[0];
        assert!(object.generated);
        let material = engine
            .renderer
            .material(object.material)
            .expect("重建材质应存在");
        assert!(
            material.base_color_texture.is_some(),
            "有 RGB + 内参时应把摄像头原图挂成基础色贴图"
        );
        assert_eq!(material.base_color, Vec4::new(1.0, 1.0, 1.0, 1.0));
        assert!(material.unlit, "摄像头原图应无光照直出");
        assert!(feed.recon_texture.is_some());
        assert!(feed.recon_status.contains("彩色"), "{}", feed.recon_status);
    }
    #[test]
    fn point_cloud_colors_sample_the_camera_image() {
        let projection = crate::reconstruct::CameraProjection {
            width: 8,
            height: 8,
            fx: 1.0,
            fy: 1.0,
            cx: 4.0,
            cy: 4.0,
            origin: Vec3::ZERO,
            mirrored: false,
        };
        let mut rgb = vec![0u8; 8 * 8 * 3];
        let red = (4 * 8 + 4) * 3;
        rgb[red] = 255;
        // 8x8、fx=fy=1、原点在相机：p=(0.5,-0.5,-1) 正对像素 (4,4)。
        let points = [Vec3::new(0.5, -0.5, -1.0), Vec3::new(100.0, 0.0, -1.0)];
        let colors = sample_point_colors(&points, &rgb, &projection).expect("应能取色");
        assert_eq!(colors.len(), 2);
        assert!(colors[0].x > 0.95 && colors[0].y < 0.01 && colors[0].z < 0.01);
        assert!(
            colors[1].x > 0.2 && colors[1].y > 0.7,
            "画面外的点应退回青色"
        );
        assert!(sample_point_colors(&points, &[0; 3], &projection).is_none());
    }
}
