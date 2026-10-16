//! 执行层：把场景文档落到引擎上，产出结构化报告。
//!
//! 分成两条路径：
//!
//! * **纯物理路径**（[`run_simulate`]）：不需要 GPU，直接把场景里的刚体建到
//!   `PhysicsWorld` 并步进，输出数值报告。agent 做快速物理验证走这条路，通常几十毫秒。
//! * **渲染路径**（[`run_render`]）：创建无窗口 GPU 上下文，建完整场景（网格/材质/模型），
//!   步进物理后渲染成 PNG。
//!
//! 两条路径共用 [`build_world`]：物理世界先独立建好（可以在没有 GPU 的机器上跑），
//! 渲染时再把它交给引擎，可视物体按名字与刚体关联。

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use quench3d::physics::{BodyType, PhysicsWorld};
use quench3d::prelude::*;
use quench3d::render::RenderStats;
use serde::{Deserialize, Serialize};

use crate::scene_doc::{
    arr, v3, ActionKind, BodyKind, JointKind, MaterialDoc, ProbeDoc, ProbeShapeDoc, SceneDoc,
    ShapeDoc, ShapeKind,
};

/// 运行期错误。
#[derive(Debug)]
pub struct RunError {
    /// 类别：`scene` / `environment` / `io`。
    pub kind: &'static str,
    /// 说明。
    pub message: String,
    /// 相关 JSON 路径（可选）。
    pub path: Option<String>,
}

impl RunError {
    /// 场景错误。
    pub fn scene(message: impl Into<String>) -> Self {
        Self {
            kind: "scene",
            message: message.into(),
            path: None,
        }
    }

    /// 环境错误（GPU / 平台）。
    pub fn env(message: impl Into<String>) -> Self {
        Self {
            kind: "environment",
            message: message.into(),
            path: None,
        }
    }

    /// 输出已存在，而调用方要求不覆盖（`--no-clobber`）。
    pub fn output_exists(path: &std::path::Path) -> Self {
        Self {
            kind: "output_exists",
            message: format!("输出已存在，拒绝覆盖：{}", path.display()),
            path: Some(path.display().to_string()),
        }
    }

    /// IO 错误。
    pub fn io(message: impl Into<String>) -> Self {
        Self {
            kind: "io",
            message: message.into(),
            path: None,
        }
    }
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for RunError {}

/// 单个刚体的状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BodyState {
    /// 名称（`$ground` / `$model[i]` 表示引擎自动创建的地面与模型碰撞体）。
    pub name: String,
    /// 刚体类型：static / dynamic / kinematic。
    pub kind: String,
    /// 位置。
    pub position: [f32; 3],
    /// 朝向（四元数 **xyzw**，单位四元数）。
    ///
    /// 只看位置的话，"原地转起来的物体"和"完全静止的物体"长得一模一样；
    /// 有了朝向才能验证 `angular_velocity`、翻滚、关节约束这些转动行为。
    pub rotation: [f32; 4],
    /// 线速度大小（m/s）。
    pub speed: f32,
    /// 相对出生点的位移（米）。
    pub displacement: f32,
    /// 是否已休眠。
    pub asleep: bool,
    /// 是否与别的刚体接触。
    pub touching: bool,
}

/// 物理仿真报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimReport {
    /// 步数。
    pub steps: u32,
    /// 步长（秒）。
    pub dt: f32,
    /// 重力。
    pub gravity: [f32; 3],
    /// 刚体总数。
    pub bodies: usize,
    /// 动态刚体数。
    pub dynamic: usize,
    /// 已休眠数。
    pub sleeping: usize,
    /// 接触流形数。
    pub contacts: usize,
    /// 关节数。
    pub joints: usize,
    /// 含 NaN/Inf 的刚体数（正常应为 0）。
    pub nan_bodies: usize,
    /// 最低点 y。
    pub lowest_y: f32,
    /// 最大速度。
    pub max_speed: f32,
    /// 逐刚体状态（按名字排序，输出稳定）。
    pub states: Vec<BodyState>,
    /// 区域探针的观测结果（场景里没写 `probes` 时这个字段不出现）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub probes: Vec<ProbeReport>,
    /// 非致命提示。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// 轨迹采样点。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceSample {
    /// 第几步（0 是初始状态）。
    pub step: u32,
    /// 时间（秒）。
    pub time: f32,
    /// 位置。
    pub position: [f32; 3],
    /// 速度大小（m/s）。
    pub speed: f32,
    /// 相对初始位置的位移（米）。
    pub displacement: f32,
    /// 姿态倾斜角（度）：0 表示站得笔直，90 表示躺平。
    pub tilt_deg: f32,
    /// 是否已休眠。
    pub asleep: bool,
}

/// 一个刚体在一段时间里的轨迹与派生结论。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trajectory {
    /// 刚体名。
    pub name: String,
    /// 采样点。
    pub samples: Vec<TraceSample>,
    /// 起点。
    pub start: [f32; 3],
    /// 终点。
    pub end: [f32; 3],
    /// 包围盒最小角。
    pub min: [f32; 3],
    /// 包围盒最大角。
    pub max: [f32; 3],
    /// 最大下坠高度（起点 y 减最低 y）。
    pub fall: f32,
    /// 总爬升高度（最高 y 减起点 y）。
    pub rise: f32,
    /// 首末位移（米）。
    pub travel: f32,
    /// 累计路程（米，带上下起伏时大于位移）。
    pub path_length: f32,
    /// 最大速度。
    pub max_speed: f32,
    /// 最大倾斜角（度）。球体等各向同性形状会随意自转，此时该值没有物理意义。
    pub max_tilt_deg: f32,
    /// 形状种类（box / sphere / capsule / cylinder / plane / convex）。
    pub shape: String,
    /// 倾斜角是否能说明问题（球体 / 平面为 false）。
    pub tilt_meaningful: bool,
    /// 是否翻倒（形状有意义且最大倾斜角超过 60 度）。
    pub tipped_over: bool,
    /// 最后一次明显运动（速度 > 0.05 m/s）的步数；一直没动则为 None。
    pub last_motion_step: Option<u32>,
    /// 采样是否被上限截断。
    pub truncated: bool,
}

/// 默认每条轨迹最多采样多少个点。
pub const MAX_TRACE_SAMPLES: usize = 4000;

/// 渲染报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderReport {
    /// 输出文件。
    pub output: String,
    /// 宽度。
    pub width: u32,
    /// 高度。
    pub height: u32,
    /// 文件字节数。
    pub bytes: u64,
    /// 绘制调用数。
    pub draw_calls: u32,
    /// 实例数。
    pub instances: u32,
    /// 三角形数。
    pub triangles: u64,
    /// 被剔除的实例数。
    pub culled: u32,
    /// 阴影实例数。
    pub shadow_instances: u32,
    /// 调试线段数。
    pub lines: u32,
    /// 场景物体数。
    pub objects: usize,
    /// 相机位置。
    pub camera_position: [f32; 3],
    /// 区域探针的观测结果（场景里没写 `probes` 时这个字段不出现）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub probes: Vec<ProbeReport>,
    /// 仿真报告（配置了步进时才有）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simulate: Option<SimReport>,
    /// 是否为 `--dry-run`（真：没有渲染、没有写盘，渲染相关统计全为 0）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub dry_run: bool,
    /// `--dry-run` 时"本来会写这些文件"。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub would_write: Vec<String>,
    /// 总耗时（毫秒）。
    pub elapsed_ms: u64,
}

/// serde 辅助：`false` 不序列化（保证缺省行为下 JSON 逐字节不变）。
fn is_false(b: &bool) -> bool {
    !*b
}

/// 单帧记录（写进 `trace.jsonl`，一行一帧）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovieFrame {
    /// 输出帧序号（从 0 起）。
    pub frame: u32,
    /// 帧文件名（相对 `frames_dir`）。
    pub file: String,
    /// 累计物理步数（含预热帧）。
    pub steps: u64,
    /// 累计仿真时间（秒，含预热帧）。
    pub time: f32,
    /// 绘制调用数。
    pub draw_calls: u32,
    /// 实例数。
    pub instances: u32,
    /// 三角形数。
    pub triangles: u64,
    /// 被剔除实例数。
    pub culled: u32,
    /// 阴影实例数。
    pub shadow_instances: u32,
}

/// 输出策略：给"多 agent 并行"用的两个开关。
///
/// * `no_clobber`：目标已存在就**报错**而不是覆盖。同一台机器上多个 agent
///   同时渲染时，缺省路径（`render.output`，常见就是 `output.png`）必然互踩。
/// * `dry_run`：只校验 + 跑物理 + 采探针，**不初始化 GPU、不写任何文件**。
///   agent 可以先用它确认"这份场景跑得起来、会写到哪里"，再花 GPU 时间。
#[derive(Debug, Clone, Copy, Default)]
pub struct OutputPolicy {
    /// 已存在的输出一律当错误。
    pub no_clobber: bool,
    /// 只校验与跑物理，不渲染不写盘。
    pub dry_run: bool,
}

impl OutputPolicy {
    /// 目标已存在且要求不覆盖 → 报错。
    pub fn guard(&self, path: &std::path::Path) -> Result<(), RunError> {
        if self.no_clobber && path.exists() {
            return Err(RunError::output_exists(path));
        }
        Ok(())
    }
}

/// 目录里是否有内容（`--no-clobber` 判断帧序列目录用）。
fn dir_has_entries(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

/// 录制参数。
#[derive(Debug, Clone)]
pub struct MovieOptions {
    /// 输出帧数。
    pub frames: u32,
    /// 视频帧率（只影响播放速度与 ffmpeg 命令，不影响仿真）。
    pub fps: f32,
    /// 帧序列目录。
    pub frames_dir: std::path::PathBuf,
    /// 每帧推进的物理步数。
    pub steps_per_frame: u32,
    /// 预热帧数（丢弃）。
    pub warmup_frames: u32,
    /// 宽度。
    pub width: u32,
    /// 高度。
    pub height: u32,
    /// 封装格式。
    pub encoder: MovieEncoder,
    /// 单文件输出路径（`encoder = png` 时为 `None`）。
    pub output: Option<std::path::PathBuf>,
    /// 已存在的输出（含帧目录）一律当错误。
    pub no_clobber: bool,
    /// 只校验 + 跑物理，不渲染不写盘。
    pub dry_run: bool,
}

/// 录制报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovieReport {
    /// 帧序列目录。
    pub frames_dir: String,
    /// 封装格式（`png` / `gif` / `apng`）。
    pub encoder: String,
    /// 单文件输出路径（PNG 序列为空字符串）。
    pub output: String,
    /// trace 文件路径。
    pub trace: String,
    /// 实际输出的帧数。
    pub frames: u32,
    /// 因已存在而跳过写盘的帧数（断点续录）。
    pub skipped: u32,
    /// 视频帧率。
    pub fps: f32,
    /// 视频时长（秒）。
    pub duration: f32,
    /// 每帧物理步数。
    pub steps_per_frame: u32,
    /// 物理步长（秒）。
    pub dt: f32,
    /// 总物理步数（含预热）。
    pub total_steps: u64,
    /// 预热帧数。
    pub warmup_frames: u32,
    /// 宽度。
    pub width: u32,
    /// 高度。
    pub height: u32,
    /// 写出的总字节数（跳过的帧不计）。
    pub bytes: u64,
    /// 全部输出帧像素的 FNV-1a 摘要（同场景两次录制必须相同）。
    pub digest: String,
    /// 场景物体数。
    pub objects: usize,
    /// 相机位置。
    pub camera_position: [f32; 3],
    /// 区域探针的观测结果（场景里没写 `probes` 时这个字段不出现）。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub probes: Vec<ProbeReport>,
    /// 首帧记录。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_frame: Option<MovieFrame>,
    /// 末帧记录。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_frame: Option<MovieFrame>,
    /// 建议的 ffmpeg 命令（帧序列 → MP4）；内置编码器为空串。
    pub ffmpeg: String,
    /// 需要提醒用户的口径/格式限制（例如 GIF 的 10 ms 延迟粒度）。
    #[serde(default)]
    pub notes: Vec<String>,
    /// 是否为 `--dry-run`（真：没有渲染、没有写盘，`digest` 为空串）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub dry_run: bool,
    /// `--dry-run` 时"本来会写这些文件/目录"。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub would_write: Vec<String>,
    /// 总耗时（毫秒）。
    pub elapsed_ms: u64,
}

/// 模型信息报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// 路径。
    pub path: String,
    /// 解析器：gltf / obj。
    pub format: String,
    /// 子网格数。
    pub meshes: usize,
    /// 材质数。
    pub materials: usize,
    /// 贴图数。
    pub images: usize,
    /// 顶点数。
    pub vertices: usize,
    /// 三角形数。
    pub triangles: usize,
    /// 包围盒最小点。
    pub bounds_min: [f32; 3],
    /// 包围盒最大点。
    pub bounds_max: [f32; 3],
    /// 包围盒尺寸。
    pub bounds_size: [f32; 3],
    /// 各子网格三角形数。
    pub mesh_triangles: Vec<usize>,
}

/// 环境自检报告。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    /// 操作系统（`std::env::consts::OS`）。
    pub os: String,
    /// CPU 架构。
    pub arch: String,
    /// 逻辑核心数。
    pub cpu_threads: usize,
    /// 是否成功创建 GPU 上下文。
    pub gpu_available: bool,
    /// 适配器名称。
    pub adapter: String,
    /// 图形后端。
    pub backend: String,
    /// 设备类型。
    pub device_type: String,
    /// 是否软件光栅化。
    pub software_rasterizer: bool,
    /// 说明与建议。
    pub notes: Vec<String>,
}

// ---------------------------------------------------------------------------
// 物理世界构建
// ---------------------------------------------------------------------------

/// 构建结果：世界 + 名字表 + 出生位置。
pub struct WorldBuild {
    /// 物理世界。
    pub world: PhysicsWorld,
    /// 名字 → 刚体句柄（含 `$ground` 与 `$model[i]`）。
    pub handles: BTreeMap<String, BodyHandle>,
    /// 名字 → 出生位置。
    pub born: BTreeMap<String, Vec3>,
}

impl WorldBuild {
    /// 步进若干步。
    pub fn step(&mut self, steps: u32, dt: f32) {
        for _ in 0..steps {
            self.world.step(dt);
        }
    }
}

/// 一个被展开的模型引用：`SceneDoc::models[model]` 在引擎场景里对应哪些物体。
///
/// 为什么需要它：`setup_visuals` 把模型的**每个子网格**推成一个普通场景物体，
/// 之后编辑器只看得见物体、看不见"它们是同一个模型"。没有这份映射，
/// 「拖模型」就只能改文件里的 `models[i]`（重开后生效），视口里当场不动。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInstance {
    /// `SceneDoc::models` 里的下标。
    pub model: usize,
    /// 场景文件里写的模型路径（界面上显示用；不影响加载，加载走 `SceneDoc`）。
    pub path: std::path::PathBuf,
    /// 展开出来的场景物体下标（按添加顺序；`LoadedModel::spawn` 是连续追加的）。
    pub objects: Vec<usize>,
}

impl ModelInstance {
    /// 这个物体属于本模型实例吗。
    pub fn contains(&self, object: usize) -> bool {
        self.objects.contains(&object)
    }
}

/// 一个探针观测到的某个刚体的进入/离开记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeHit {
    /// 刚体名。
    pub name: String,
    /// 首次进入的步号（0 = 仿真开始时就在区域内）。
    pub enter_step: u32,
    /// 最近一次离开的步号（缺省 = 结束时仍在区域内）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_step: Option<u32>,
    /// 累计在区域内的步数。
    pub steps_inside: u32,
    /// 进入次数（出去又进来算两次）。
    pub entries: u32,
    /// 在区域内出现过的最高速度（m/s）。
    pub max_speed: f32,
}

/// 一个区域探针的观测结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeReport {
    /// 探针名。
    pub name: String,
    /// 区域形状（`sphere` / `box`）。
    pub shape: String,
    /// 区域中心。
    pub position: [f32; 3],
    /// 结束时区域内是否还有刚体。
    pub occupied: bool,
    /// 结束时刻仍在区域内的刚体名（按名字排序）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inside: Vec<String>,
    /// 曾经进入过区域的刚体（按名字排序）。
    pub bodies: Vec<ProbeHit>,
}

/// 单个探针里单个刚体的运行时状态。
#[derive(Debug, Clone, Default)]
struct ProbeState {
    enter_step: u32,
    exit_step: Option<u32>,
    steps_inside: u32,
    entries: u32,
    max_speed: f32,
    inside: bool,
}

/// 一个探针的运行时数据（观测名单已解析成句柄）。
#[derive(Debug, Clone)]
struct ProbeRuntime {
    doc: ProbeDoc,
    /// 观测名单：`(名字, 句柄)`，按名字排序保证输出稳定。
    targets: Vec<(String, BodyHandle)>,
    /// 名字 → 状态。
    states: BTreeMap<String, ProbeState>,
}

/// 编译好的定时动作（步号已折算、句柄已解析、向量已转好）。
#[derive(Debug, Clone)]
struct PreparedAction {
    kind: ActionKind,
    handle: BodyHandle,
    /// 第几步开始前施加。
    step: u32,
    /// 持续步数（只有 force 用）。
    steps: u32,
    value: Vec3,
    point: Vec3,
    position: Vec3,
    /// 目标朝向（只有 teleport 用）。
    rotation: Option<Quat>,
    /// 原始下标：同一时刻的动作按作者写的顺序施加，保证确定性。
    index: usize,
}

/// 场景里的"行为与探针"：定时动作 + 只读区域探针。
///
/// 两条硬性约束：
///
/// 1. **不写就不施加**：`actions` 为空时不碰任何刚体状态，[`Script::step`] 与直接调用
///    `PhysicsWorld::step` 逐位一致。
/// 2. **探针只读**：判定只用 `world_aabb()` 与线速度，不写回世界、不唤醒睡眠刚体，
///    所以"同一个场景多挂一个探针"物理结果逐位不变。
#[derive(Debug, Clone, Default)]
pub struct Script {
    actions: Vec<PreparedAction>,
    probes: Vec<ProbeRuntime>,
    step: u32,
}

impl Script {
    /// 从场景编译出动作与探针（把目标名解析成句柄，`world` 用来判定刚体类型）。
    pub fn new(
        doc: &SceneDoc,
        world: &PhysicsWorld,
        handles: &BTreeMap<String, BodyHandle>,
    ) -> Result<Self, RunError> {
        let mut actions = Vec::new();
        for (index, a) in doc.actions.iter().enumerate() {
            let handle = *handles
                .get(&a.target)
                .ok_or_else(|| RunError::scene(format!("动作目标 \"{}\" 不存在", a.target)))?;
            let step = match (a.at_step, a.at_time) {
                (Some(s), _) => s,
                (None, Some(t)) => (t / doc.simulate.dt).round().max(0.0) as u32,
                (None, None) => 0,
            };
            actions.push(PreparedAction {
                kind: a.kind,
                handle,
                step,
                steps: if a.kind.uses_steps() {
                    a.steps.max(1)
                } else {
                    1
                },
                value: v3(a.value.unwrap_or([0.0; 3])),
                point: v3(a.point.unwrap_or([0.0; 3])),
                position: v3(a.position.unwrap_or([0.0; 3])),
                rotation: a.rotation_deg.map(|r| euler_quat(Some(r))),
                index,
            });
        }
        // 同一时刻的动作按作者顺序执行，跨时刻按步号先后
        actions.sort_by_key(|a| (a.step, a.index));

        // 缺省观测名单 = 所有动态 / 运动学刚体（静态物体的包围盒往往极大，比如地面半空间，
        // 默认纳进来只会让"谁进了区域"变成噪声；要观测静态物体就用 watch 点名）。
        let mut auto: Vec<(String, BodyHandle)> = handles
            .iter()
            .filter(|(_, h)| {
                world
                    .body(**h)
                    .is_some_and(|b| !matches!(b.body_type, BodyType::Static))
            })
            .map(|(n, h)| (n.clone(), *h))
            .collect();
        auto.sort_by(|a, b| a.0.cmp(&b.0));

        let mut probes = Vec::new();
        for p in &doc.probes {
            let targets: Vec<(String, BodyHandle)> = if p.watch.is_empty() {
                auto.clone()
            } else {
                let mut t = Vec::new();
                for n in &p.watch {
                    let h = *handles.get(n).ok_or_else(|| {
                        RunError::scene(format!("探针 \"{}\" 要观测的刚体 \"{n}\" 不存在", p.name))
                    })?;
                    t.push((n.clone(), h));
                }
                t.sort_by(|a, b| a.0.cmp(&b.0));
                t.dedup_by(|a, b| a.0 == b.0);
                t
            };
            let states = targets
                .iter()
                .map(|(n, _)| (n.clone(), ProbeState::default()))
                .collect();
            probes.push(ProbeRuntime {
                doc: p.clone(),
                targets,
                states,
            });
        }

        Ok(Self {
            actions,
            probes,
            step: 0,
        })
    }

    /// 是否什么都没配（此时 [`Script::step`] 就是纯粹的物理步进）。
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty() && self.probes.is_empty()
    }

    /// 采样第 0 步（仿真开始前）的探针状态。
    pub fn begin(&mut self, world: &PhysicsWorld) {
        self.observe(world);
    }

    /// 推进一个物理步：先施加本步的动作，再步进，最后只读地采集探针。
    pub fn step(&mut self, world: &mut PhysicsWorld, dt: f32) {
        self.apply_actions(world);
        world.step(dt);
        self.observe(world);
        self.step += 1;
    }

    /// 施加属于第 `self.step` 步的动作。
    fn apply_actions(&mut self, world: &mut PhysicsWorld) {
        if self.actions.is_empty() {
            return;
        }
        for a in &self.actions {
            // 持续力 `steps` 步：作用区间是 [step, step + steps)
            if self.step < a.step || self.step >= a.step.saturating_add(a.steps) {
                continue;
            }
            let Some(body) = world.body_mut(a.handle) else {
                continue;
            };
            match a.kind {
                ActionKind::Impulse => body.apply_impulse(a.value),
                ActionKind::ImpulseAtPoint => body.apply_impulse_at_point(a.value, a.point),
                ActionKind::TorqueImpulse => body.apply_torque_impulse(a.value),
                ActionKind::Velocity => body.set_linear_velocity(a.value),
                ActionKind::AngularVelocity => body.set_angular_velocity(a.value),
                ActionKind::Force => body.apply_force(a.value),
                ActionKind::Wake => body.wake_up(),
                ActionKind::Teleport => {
                    let rot = a.rotation.unwrap_or_else(|| body.rotation());
                    body.set_transform(Transform::new(a.position, rot, Vec3::ONE));
                }
            }
        }
    }

    /// 只读地采集探针状态。
    fn observe(&mut self, world: &PhysicsWorld) {
        if self.probes.is_empty() {
            return;
        }
        let step = self.step;
        for probe in self.probes.iter_mut() {
            for (name, handle) in &probe.targets {
                let Some(body) = world.body(*handle) else {
                    continue;
                };
                let hit = probe_overlaps(&probe.doc, body);
                let speed = body.linear_velocity().length();
                let Some(state) = probe.states.get_mut(name) else {
                    continue;
                };
                match (state.inside, hit) {
                    (false, true) => {
                        // 第一次进入才记 enter_step；出去又进来只加计数
                        if state.entries == 0 {
                            state.enter_step = step;
                        }
                        state.entries += 1;
                        state.exit_step = None;
                        state.inside = true;
                        state.max_speed = state.max_speed.max(speed);
                    }
                    (true, true) => state.max_speed = state.max_speed.max(speed),
                    (true, false) => {
                        state.exit_step = Some(step);
                        state.inside = false;
                    }
                    (false, false) => {}
                }
                if state.inside {
                    state.steps_inside += 1;
                }
            }
        }
    }

    /// 汇总探针报告（探针按定义顺序，刚体按名字排序，输出稳定）。
    pub fn report(&self) -> Vec<ProbeReport> {
        self.probes
            .iter()
            .map(|probe| {
                let inside: Vec<String> = probe
                    .states
                    .iter()
                    .filter(|(_, s)| s.inside)
                    .map(|(n, _)| n.clone())
                    .collect();
                let bodies = probe
                    .states
                    .iter()
                    .filter(|(_, s)| s.entries > 0)
                    .map(|(n, s)| ProbeHit {
                        name: n.clone(),
                        enter_step: s.enter_step,
                        exit_step: s.exit_step,
                        steps_inside: s.steps_inside,
                        entries: s.entries,
                        max_speed: s.max_speed,
                    })
                    .collect();
                ProbeReport {
                    name: probe.doc.name.clone(),
                    shape: match probe.doc.shape {
                        ProbeShapeDoc::Sphere => "sphere".into(),
                        ProbeShapeDoc::Box => "box".into(),
                    },
                    position: probe.doc.position,
                    occupied: !inside.is_empty(),
                    inside,
                    bodies,
                }
            })
            .collect()
    }
}

/// 把 [`Script`] 接进无头逐帧循环的 [`Game`] 适配器。
///
/// `fixed_update` 里做的顺序与 `simulate` 完全一致：**先施加本步动作，再步进物理，
/// 最后只读采集探针**；三条路径（simulate / render / movie）由此得到同一套结果。
struct ScriptedGame {
    script: Script,
}

impl Game for ScriptedGame {
    fn fixed_update(&mut self, engine: &mut Engine, dt: f32) {
        self.script.step(&mut engine.world, dt);
    }
}

/// 探针是否与刚体的世界包围盒相交。
///
/// 球形：包围盒上离球心最近的点在半径内；盒形：两个 AABB 相交（"碰到"就算进入）。
/// 半空间（地面平面）没有有限包围盒，判定为**不相交**——它不是一个可以"进入"的区域。
fn probe_overlaps(probe: &ProbeDoc, body: &quench3d::physics::RigidBody) -> bool {
    let bb = body.world_aabb();
    if !bb.is_valid() {
        return false;
    }
    let center = v3(probe.position);
    match probe.shape {
        ProbeShapeDoc::Sphere => {
            let closest = Vec3::new(
                center.x.clamp(bb.min.x, bb.max.x),
                center.y.clamp(bb.min.y, bb.max.y),
                center.z.clamp(bb.min.z, bb.max.z),
            );
            (closest - center).length() <= probe.radius
        }
        ProbeShapeDoc::Box => {
            let half = v3(probe.size.unwrap_or([1.0, 1.0, 1.0])) * 0.5;
            let lo = center - half;
            let hi = center + half;
            lo.x <= bb.max.x
                && lo.y <= bb.max.y
                && lo.z <= bb.max.z
                && bb.min.x <= hi.x
                && bb.min.y <= hi.y
                && bb.min.z <= hi.z
        }
    }
}

/// 物理形状。
pub fn physics_shape(shape: &ShapeDoc) -> Shape {
    match shape.kind {
        ShapeKind::Box => Shape::Box {
            half_extents: v3(shape.size.unwrap_or([1.0, 1.0, 1.0])) * 0.5,
        },
        ShapeKind::Sphere => Shape::Sphere {
            radius: shape.radius.unwrap_or(0.5),
        },
        ShapeKind::Capsule => Shape::Capsule {
            half_height: shape.half_height.unwrap_or(0.5),
            radius: shape.radius.unwrap_or(0.5),
        },
        ShapeKind::Cylinder => Shape::Cylinder {
            half_height: shape.half_height.unwrap_or(0.5),
            radius: shape.radius.unwrap_or(0.5),
        },
        ShapeKind::Plane => Shape::Plane {
            normal: v3(shape.normal.unwrap_or([0.0, 1.0, 0.0])).normalize_or_zero(),
        },
        ShapeKind::Convex => Shape::ConvexHull {
            points: shape
                .points
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(v3)
                .collect(),
        },
    }
}

/// 渲染缩放：把单位图元拉到目标尺寸。
pub fn render_scale(shape: &ShapeDoc, plane_size: f32) -> Vec3 {
    match shape.kind {
        ShapeKind::Box => v3(shape.size.unwrap_or([1.0, 1.0, 1.0])),
        ShapeKind::Sphere => Vec3::splat(shape.radius.unwrap_or(0.5) * 2.0),
        ShapeKind::Capsule => {
            // 单位胶囊：半径 0.5、总高 2
            let r = shape.radius.unwrap_or(0.5);
            let h = shape.half_height.unwrap_or(0.5);
            Vec3::new(r * 2.0, h + r, r * 2.0)
        }
        ShapeKind::Cylinder => {
            // 单位圆柱：半径 0.5、高 1
            let r = shape.radius.unwrap_or(0.5);
            let h = shape.half_height.unwrap_or(0.5);
            Vec3::new(r * 2.0, h * 2.0, r * 2.0)
        }
        // 凸包不走这里（见 setup_visuals 里的 AABB 近似）
        ShapeKind::Plane => Vec3::splat(plane_size.max(0.01)),
        ShapeKind::Convex => Vec3::ONE,
    }
}

/// 凸包点集的包围盒；点集为空或非法时退回单位盒（渲染代理用）。
pub fn convex_bounds(points: &[[f32; 3]]) -> (Vec3, Vec3) {
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    for p in points {
        let p = v3(*p);
        if !p.is_finite() {
            continue;
        }
        lo = lo.min(p);
        hi = hi.max(p);
    }
    if lo.x > hi.x || !lo.is_finite() || !hi.is_finite() {
        return (Vec3::splat(-0.5), Vec3::splat(0.5));
    }
    (lo, hi)
}

/// 物体名（缺省 `objects[i]`）。
pub fn object_name(doc: &SceneDoc, index: usize) -> String {
    doc.objects[index]
        .name
        .clone()
        .unwrap_or_else(|| format!("objects[{index}]"))
}

fn euler_quat(deg: Option<[f32; 3]>) -> Quat {
    match deg {
        None => Quat::IDENTITY,
        Some(d) => {
            let r = std::f32::consts::PI / 180.0;
            Quat::from_euler_xyz(d[0] * r, d[1] * r, d[2] * r)
        }
    }
}

/// 把文档里的物理内容建成一个独立世界（不需要 GPU）。
pub fn build_world(doc: &SceneDoc) -> Result<WorldBuild, RunError> {
    let mut world = PhysicsWorld::new();
    let mut handles = BTreeMap::new();
    let mut born = BTreeMap::new();

    if let Some(g) = doc.gravity {
        world.set_gravity(v3(g));
    }

    if let Some(ground) = &doc.ground {
        let pos = Vec3::new(0.0, ground.y, 0.0);
        // 摩擦/弹性可配（以前硬编码 0.85，用户没法让地面更滑或更涩）
        let h = world.add_builder(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
                .position(pos)
                .friction(ground.friction)
                .restitution(ground.restitution),
        );
        handles.insert("$ground".into(), h);
        born.insert("$ground".into(), pos);
    }

    for (i, o) in doc.objects.iter().enumerate() {
        if o.body == BodyKind::None {
            continue;
        }
        let mut builder = match o.body {
            BodyKind::Static => BodyBuilder::r#static(physics_shape(&o.shape)),
            BodyKind::Dynamic => BodyBuilder::dynamic(physics_shape(&o.shape)),
            BodyKind::Kinematic => BodyBuilder::kinematic(physics_shape(&o.shape)),
            BodyKind::None => unreachable!(),
        };
        let pos = v3(o.position.unwrap_or([0.0, 0.0, 0.0]));
        builder = builder
            .position(pos)
            .rotation(euler_quat(o.rotation_deg))
            .restitution(o.restitution.unwrap_or(0.2))
            .friction(o.friction.unwrap_or(0.5));
        if let Some(d) = o.density {
            builder = builder.density(d);
        }
        if let Some(v) = o.velocity {
            builder = builder.linear_velocity(v3(v));
        }
        if let Some(w) = o.angular_velocity {
            builder = builder.angular_velocity(v3(w));
        }
        if let Some(d) = o.linear_damping {
            builder = builder.linear_damping(d);
        }
        if let Some(d) = o.angular_damping {
            builder = builder.angular_damping(d);
        }
        if let Some(g) = o.gravity_scale {
            builder = builder.gravity_scale(g);
        }
        if let Some(m) = o.mass {
            builder = builder.mass(m);
        }
        if let Some(allow) = o.allow_sleep {
            builder = builder.can_sleep(allow);
        }
        let handle = world.add_builder(builder);
        let name = object_name(doc, i);
        handles.insert(name.clone(), handle);
        born.insert(name, pos);
    }

    for (i, m) in doc.models.iter().enumerate() {
        if !m.collide {
            continue;
        }
        let model = quench3d::assets::load_model(&m.path)
            .map_err(|e| RunError::scene(format!("加载模型 {} 失败: {e}", m.path.display())))?;
        let s = m.scale.unwrap_or(1.0);
        let rot = euler_quat(m.rotation_deg);
        let origin = v3(m.position.unwrap_or([0.0, 0.0, 0.0]));
        let mut vertices: Vec<Vec3> = Vec::new();
        let mut indices: Vec<u32> = Vec::new();
        for mesh in &model.meshes {
            let base = vertices.len() as u32;
            for vert in &mesh.data.vertices {
                vertices.push(origin + rot.mul_vec3(vert.pos() * s));
            }
            indices.extend(mesh.data.indices.iter().map(|idx| idx + base));
        }
        if indices.is_empty() {
            return Err(RunError::scene(format!(
                "模型 {} 没有三角形，无法作为碰撞体",
                m.path.display()
            )));
        }
        let h = world.add_builder(
            BodyBuilder::r#static(Shape::TriMesh { vertices, indices })
                .position(Vec3::ZERO)
                .friction(m.friction.unwrap_or(0.8)),
        );
        let name = format!("$model[{i}]");
        handles.insert(name.clone(), h);
        born.insert(name, origin);
    }

    for (i, j) in doc.joints.iter().enumerate() {
        let a = *handles
            .get(&j.a)
            .ok_or_else(|| RunError::scene(format!("joints[{i}].a 找不到刚体 {}", j.a)))?;
        let b = *handles
            .get(&j.b)
            .ok_or_else(|| RunError::scene(format!("joints[{i}].b 找不到刚体 {}", j.b)))?;
        let axis = v3(j.axis.unwrap_or([0.0, 1.0, 0.0])).normalize_or_zero();
        let anchor_a = v3(j.anchor_a.unwrap_or([0.0, 0.0, 0.0]));
        let anchor_b = v3(j.anchor_b.unwrap_or([0.0, 0.0, 0.0]));
        let joint = match j.kind {
            JointKind::Fixed => Joint::Fixed {
                a,
                b,
                local_anchor_a: anchor_a,
                local_anchor_b: anchor_b,
                local_frame_a: Quat::IDENTITY,
                local_frame_b: Quat::IDENTITY,
            },
            JointKind::Ball => Joint::Ball {
                a,
                b,
                local_anchor_a: anchor_a,
                local_anchor_b: anchor_b,
            },
            JointKind::Hinge => Joint::Hinge {
                a,
                b,
                local_anchor_a: anchor_a,
                local_anchor_b: anchor_b,
                local_axis_a: axis,
                local_axis_b: axis,
                limits: j.limits.map(|l| (l[0], l[1])),
                motor: j.motor_velocity.map(|v| HingeMotor {
                    target_velocity: v,
                    max_impulse: j.motor_max_impulse.unwrap_or(f32::MAX),
                }),
            },
            JointKind::Distance => Joint::Distance {
                a,
                b,
                local_anchor_a: anchor_a,
                local_anchor_b: anchor_b,
                rest_length: j.rest_length.unwrap_or(1.0),
                stiffness: j.stiffness.unwrap_or(20.0),
                damping: j.damping.unwrap_or(1.0),
            },
            JointKind::Slider => Joint::Slider {
                a,
                b,
                local_anchor_a: anchor_a,
                local_anchor_b: anchor_b,
                local_axis_a: axis,
                local_axis_b: axis,
                limits: j.limits.map(|l| (l[0], l[1])),
            },
        };
        world.add_joint(joint);
    }

    Ok(WorldBuild {
        world,
        handles,
        born,
    })
}

fn body_states(build: &WorldBuild) -> Vec<BodyState> {
    let touching: HashSet<BodyHandle> = build
        .world
        .contacts()
        .iter()
        .flat_map(|c| [c.a, c.b])
        .collect();
    let mut out = Vec::new();
    for (name, handle) in &build.handles {
        let Some(body) = build.world.body(*handle) else {
            continue;
        };
        let p = body.position();
        let birth = build.born.get(name).copied().unwrap_or(p);
        out.push(BodyState {
            name: name.clone(),
            kind: format!("{:?}", body.body_type).to_ascii_lowercase(),
            position: arr(p),
            rotation: {
                let q = body.rotation();
                [q.x, q.y, q.z, q.w]
            },
            speed: body.linear_velocity.length(),
            displacement: (p - birth).length(),
            asleep: body.is_sleeping(),
            touching: touching.contains(handle),
        });
    }
    out
}

/// 汇总物理报告。
pub fn summarize(build: &WorldBuild, steps: u32, dt: f32) -> SimReport {
    let states = body_states(build);
    let mut nan = 0usize;
    let mut max_speed = 0.0f32;
    let mut lowest_y = f32::MAX;
    let mut sleeping = 0usize;
    let mut dynamic = 0usize;

    for body in build.world.bodies().map(|(_, b)| b) {
        let p = body.position();
        if !p.is_finite() || !body.linear_velocity.is_finite() {
            nan += 1;
            continue;
        }
        max_speed = max_speed.max(body.linear_velocity.length());
        lowest_y = lowest_y.min(p.y);
        if body.is_dynamic() {
            dynamic += 1;
        }
        if body.is_sleeping() {
            sleeping += 1;
        }
    }

    let mut warnings = Vec::new();
    if nan > 0 {
        warnings.push(format!("有 {nan} 个刚体出现 NaN/Inf"));
    }
    if let Some(ground) = &build.born.get("$ground") {
        let floor = ground.y;
        let below = states
            .iter()
            .filter(|s| s.kind == "dynamic" && s.position[1] < floor - 0.5)
            .count();
        if below > 0 {
            warnings.push(format!("有 {below} 个动态刚体掉到地面以下"));
        }
    }

    SimReport {
        probes: Vec::new(),
        steps,
        dt,
        gravity: arr(build.world.gravity()),
        bodies: build.world.body_count(),
        dynamic,
        sleeping,
        contacts: build.world.contacts().len(),
        joints: build.world.joint_count(),
        nan_bodies: nan,
        lowest_y: if lowest_y == f32::MAX { 0.0 } else { lowest_y },
        max_speed,
        states,
        warnings,
    }
}

/// 只跑物理（不需要 GPU）。
pub fn run_simulate(doc: &SceneDoc) -> Result<SimReport, RunError> {
    run_simulate_traced(doc, &[], 1).map(|(report, _)| report)
}

/// 跑物理并采集指定刚体的逐帧轨迹。
///
/// `trace` 是要跟踪的刚体名（支持 `$ground`、`$model[0]` 这类自动名字）；
/// `every` 是采样间隔（步）。每条轨迹最多 [`MAX_TRACE_SAMPLES`] 个点，
/// 超出后按 `every` 自动放大间隔，保证长跑也不会把内存撑爆。
pub fn run_simulate_traced(
    doc: &SceneDoc,
    trace: &[String],
    every: u32,
) -> Result<(SimReport, Vec<Trajectory>), RunError> {
    let mut build = build_world(doc)?;
    let steps = doc.simulate.steps;
    let dt = doc.simulate.dt;
    let every = every.max(1);
    // 采样太密会超上限时自动放大间隔
    let every = if steps / every > MAX_TRACE_SAMPLES as u32 {
        (steps / MAX_TRACE_SAMPLES as u32).max(1)
    } else {
        every
    };
    let truncated = steps / every > MAX_TRACE_SAMPLES as u32;

    let mut traced: Vec<Tracing> = Vec::new();
    for name in trace {
        let Some(handle) = build.handles.get(name).copied() else {
            let mut known: Vec<&str> = build.handles.keys().map(|k| k.as_str()).collect();
            known.sort_unstable();
            return Err(RunError::scene(format!(
                "找不到刚体 `{name}`；可用名字：{}",
                known.join("、")
            )));
        };
        let body = build
            .world
            .body(handle)
            .ok_or_else(|| RunError::scene(format!("刚体 `{name}` 不存在")))?;
        let p = body.position();
        traced.push(Tracing {
            name: name.clone(),
            handle,
            shape: shape_kind_of(doc, name),
            start: arr(p),
            samples: Vec::new(),
        });
    }

    // 先采初始状态
    for t in traced.iter_mut() {
        if let Some(body) = build.world.body(t.handle) {
            let s = sample(body, 0, 0.0, t.start);
            t.samples.push(s);
        }
    }

    // 定时动作 + 只读探针：不写 actions/probes 时 `Script::step` 就是纯粹的 `world.step`
    let mut script = Script::new(doc, &build.world, &build.handles)?;
    script.begin(&build.world);

    for step in 1..=steps {
        script.step(&mut build.world, dt);
        if step % every != 0 && step != steps {
            continue;
        }
        for t in traced.iter_mut() {
            if let Some(body) = build.world.body(t.handle) {
                let s = sample(body, step, step as f32 * dt, t.start);
                t.samples.push(s);
            }
        }
    }

    let mut report = summarize(&build, steps, dt);
    report.probes = script.report();
    let trajectories = traced
        .into_iter()
        .map(|t| summarize_trace(t.name, t.shape.to_string(), t.samples, truncated))
        .collect();
    Ok((report, trajectories))
}

/// 采集一个采样点。
fn sample(
    body: &quench3d::physics::RigidBody,
    step: u32,
    time: f32,
    start: [f32; 3],
) -> TraceSample {
    let p = body.position();
    let up = body.rotation() * Vec3::new(0.0, 1.0, 0.0);
    let tilt = up.y.clamp(-1.0, 1.0).acos().to_degrees();
    TraceSample {
        step,
        time,
        position: arr(p),
        speed: body.linear_velocity.length(),
        displacement: (p - v3(start)).length(),
        tilt_deg: tilt,
        asleep: body.is_sleeping(),
    }
}

/// 正在跟踪的一条轨迹（采集过程中的临时状态）。
struct Tracing {
    /// 刚体名。
    name: String,
    /// 句柄。
    handle: BodyHandle,
    /// 形状种类。
    shape: &'static str,
    /// 出生位置。
    start: [f32; 3],
    /// 已采集的采样点。
    samples: Vec<TraceSample>,
}

/// 查一个刚体名对应的形状种类（自动创建的刚体返回 `unknown`）。
fn shape_kind_of(doc: &SceneDoc, name: &str) -> &'static str {
    for (i, o) in doc.objects.iter().enumerate() {
        if object_name(doc, i) == name {
            return match o.shape.kind {
                ShapeKind::Box => "box",
                ShapeKind::Sphere => "sphere",
                ShapeKind::Capsule => "capsule",
                ShapeKind::Cylinder => "cylinder",
                ShapeKind::Plane => "plane",
                ShapeKind::Convex => "convex",
            };
        }
    }
    "unknown"
}

/// 把采样点汇总成轨迹结论。
fn summarize_trace(
    name: String,
    shape: String,
    samples: Vec<TraceSample>,
    truncated: bool,
) -> Trajectory {
    let start_p = samples.first().map(|s| s.position).unwrap_or([0.0; 3]);
    let end_p = samples.last().map(|s| s.position).unwrap_or(start_p);
    let mut min = start_p;
    let mut max = start_p;
    let mut path_length = 0.0f32;
    let mut max_speed = 0.0f32;
    let mut max_tilt = 0.0f32;
    let mut last_motion = None;
    let mut previous = start_p;
    for s in &samples {
        min = [
            min[0].min(s.position[0]),
            min[1].min(s.position[1]),
            min[2].min(s.position[2]),
        ];
        max = [
            max[0].max(s.position[0]),
            max[1].max(s.position[1]),
            max[2].max(s.position[2]),
        ];
        path_length += (v3(s.position) - v3(previous)).length();
        previous = s.position;
        max_speed = max_speed.max(s.speed);
        max_tilt = max_tilt.max(s.tilt_deg);
        if s.speed > 0.05 {
            last_motion = Some(s.step);
        }
    }
    // 球与平面没有"朝向"概念，自转不代表翻倒
    let tilt_meaningful = !matches!(shape.as_str(), "sphere" | "plane" | "unknown");
    Trajectory {
        name,
        shape,
        tilt_meaningful,
        fall: (start_p[1] - min[1]).max(0.0),
        rise: (max[1] - start_p[1]).max(0.0),
        travel: (v3(end_p) - v3(start_p)).length(),
        path_length,
        start: start_p,
        end: end_p,
        min,
        max,
        max_speed,
        max_tilt_deg: max_tilt,
        tipped_over: tilt_meaningful && max_tilt > 60.0,
        last_motion_step: last_motion,
        samples,
        truncated,
    }
}

// ---------------------------------------------------------------------------
// 可视化与渲染
// ---------------------------------------------------------------------------

/// 把可视部分写进引擎（物理世界由调用方预先放入 `engine.world`）。
///
/// 返回**模型实例映射**（`models[i]` → 它展开出来的物体下标），见 [`ModelInstance`]。
/// 没有模型的场景返回空表；调用方不需要时忽略即可。
pub fn setup_visuals(
    engine: &mut Engine,
    doc: &SceneDoc,
    handles: &BTreeMap<String, BodyHandle>,
) -> Result<Vec<ModelInstance>, RunError> {
    apply_lighting(engine, doc);

    // 材质（贴图按路径加载并按 (路径, 是否 sRGB) 缓存，多个材质共用一张图只上一次显存）
    let mut materials: BTreeMap<String, MaterialId> = BTreeMap::new();
    let mut texture_cache: TextureCache = BTreeMap::new();
    for (name, m) in &doc.materials {
        let desc = material_desc(engine, doc, name, m, &mut texture_cache)?;
        let id = engine
            .assets
            .material(&mut engine.renderer, &engine.gpu, name, desc);
        materials.insert(name.clone(), id);
    }
    let default_material = engine.assets.solid_material(
        &mut engine.renderer,
        &engine.gpu,
        Vec4::new(0.72, 0.72, 0.75, 1.0),
        0.0,
        0.6,
    );

    // 地面
    if let Some(g) = &doc.ground {
        let mat = g
            .material
            .as_ref()
            .and_then(|n| materials.get(n).copied())
            .unwrap_or(default_material);
        let index = match handles.get("$ground") {
            Some(h) => {
                engine
                    .scene
                    .add_with_body(engine.assets.unit_plane, mat, *h, &engine.world, "地面")
            }
            None => engine.scene.add_named(
                engine.assets.unit_plane,
                mat,
                Transform::new(Vec3::new(0.0, g.y, 0.0), Quat::IDENTITY, Vec3::ONE),
                "地面",
            ),
        };
        engine.scene.objects[index].transform.scale = Vec3::splat(g.size);
    }

    let plane_size = doc.ground.as_ref().map(|g| g.size).unwrap_or(40.0);
    // 文档下标 → 场景物体下标：地面在场景里排在最前面，两者并不相等。
    let mut doc_to_scene: Vec<usize> = Vec::with_capacity(doc.objects.len());

    // 图元
    for (i, o) in doc.objects.iter().enumerate() {
        let mat = o
            .material
            .as_ref()
            .and_then(|n| materials.get(n).copied())
            .unwrap_or(default_material);
        let mesh = match o.shape.kind {
            ShapeKind::Box => engine.assets.unit_cube,
            ShapeKind::Sphere => engine.assets.unit_sphere,
            ShapeKind::Capsule => engine.assets.unit_capsule,
            ShapeKind::Cylinder => engine.assets.unit_cylinder,
            ShapeKind::Plane => engine.assets.unit_plane,
            ShapeKind::Convex => engine.assets.unit_cube,
        };
        // 凸包没有三角化网格（物理只用支撑函数）：用点集的 AABB 盒体近似，
        // 并把盒体中心平移到点集中心——以前是拿单位立方体按 plane_size 缩放，
        // 结果一个 40 米的巨箱顶在那儿（见 gap_analysis §3.3）。
        let hull = match o.shape.kind {
            ShapeKind::Convex => {
                let (lo, hi) = convex_bounds(o.shape.points.as_deref().unwrap_or(&[]));
                Some((lo, hi))
            }
            _ => None,
        };
        let scale = o.scale.map(v3).unwrap_or_else(|| match hull {
            Some((lo, hi)) => (hi - lo).max(Vec3::splat(0.01)),
            None => render_scale(&o.shape, plane_size),
        });
        let rotation = euler_quat(o.rotation_deg);
        let position = v3(o.position.unwrap_or([0.0, 0.0, 0.0]));
        // 盒体以物体原点为中心，所以要把原点移到点集中心（旋转后再平移）
        let origin = match hull {
            Some((lo, hi)) => position + rotation * ((lo + hi) * 0.5),
            None => position,
        };
        let name = object_name(doc, i);
        let index = match (o.body, handles.get(&name)) {
            (BodyKind::None, _) => {
                engine
                    .scene
                    .add_named(mesh, mat, Transform::new(origin, rotation, scale), &name)
            }
            (_, Some(h)) => engine
                .scene
                .add_with_body(mesh, mat, *h, &engine.world, &name),
            (_, None) => {
                engine
                    .scene
                    .add_named(mesh, mat, Transform::new(origin, rotation, scale), &name)
            }
        };
        if let Some(obj) = engine.scene.objects.get_mut(index) {
            obj.visible = o.visible;
            // 刚体的位姿来自物理，缩放只影响渲染
            obj.transform.scale = scale;
        }
        doc_to_scene.push(index);
    }

    // 父子：文件里用**名字**写（父物体可以排在子物体后面，顺序不该成为限制），
    // 引擎里用**下标**。校验器已经查过悬空/自环/成环/带刚体，这里再挡一次是因为
    // 引擎侧的规则可能比校验器更严 —— 真挡下来了就报错，不要静默丢掉父物体。
    let doc_index_of_name: BTreeMap<String, usize> = doc
        .objects
        .iter()
        .enumerate()
        .map(|(i, _)| (object_name(doc, i), i))
        .collect();
    for (i, o) in doc.objects.iter().enumerate() {
        let Some(parent) = o.parent.as_deref() else {
            continue;
        };
        let (Some(&child), Some(&parent_doc)) = (
            doc_to_scene.get(i),
            doc_index_of_name
                .get(parent)
                .and_then(|j| doc_to_scene.get(*j)),
        ) else {
            continue; // 校验器已报过"找不到父物体"，这里不重复报
        };
        if let Err(e) = engine.scene.set_parent(child, Some(parent_doc)) {
            let mut err = RunError::io(format!("应用父子关系失败: {e}"));
            err.path = Some(format!("objects[{i}].parent"));
            return Err(err);
        }
    }

    // 模型（碰撞体已在 build_world 里建好，这里只画）。
    // 顺手把 `models[i]` 展开出来的物体下标记下来 —— 这是编辑器"视口里拖模型"的唯一依据。
    let mut instances: Vec<ModelInstance> = Vec::new();
    for (i, m) in doc.models.iter().enumerate() {
        let transform = Transform::new(
            v3(m.position.unwrap_or([0.0, 0.0, 0.0])),
            euler_quat(m.rotation_deg),
            Vec3::splat(m.scale.unwrap_or(1.0)),
        );
        match engine.load_model(&m.path, transform) {
            Ok(objects) => instances.push(ModelInstance {
                model: i,
                path: m.path.clone(),
                objects,
            }),
            Err(e) => {
                // 以前这里只往 stderr 打一行警告就继续渲染——用户拿到的图跟场景对不上却
                // 以为成功了。宁可报错，也不要出一张"少了几件东西"的图。
                let mut err = RunError::io(format!(
                    "加载模型 {} 失败: {e}（确认文件是完好的 glTF/GLB/OBJ）",
                    m.path.display()
                ));
                err.path = Some(format!("models[{i}]"));
                return Err(err);
            }
        }
    }

    apply_camera(engine, doc);
    Ok(instances)
}

/// 贴图缓存键：`(解析后的路径, 是否按 sRGB 采样)`。
type TextureCache = BTreeMap<(String, bool), TextureId>;

/// 按 `MaterialDoc` 构造渲染材质（含贴图加载）。
fn material_desc(
    engine: &mut Engine,
    doc: &SceneDoc,
    name: &str,
    m: &MaterialDoc,
    cache: &mut TextureCache,
) -> Result<MaterialDesc, RunError> {
    let alpha = m.alpha.unwrap_or(1.0);
    let color = Vec4::new(m.color[0], m.color[1], m.color[2], alpha);
    let mut desc =
        MaterialDesc::from_srgb(color.x, color.y, color.z, color.w).pbr(m.metallic, m.roughness);

    // 自发光：颜色缺省沿用 `color`，并且与历史行为一致地**不**再做空间转换
    // （`MaterialDesc.emissive` 直接吃这个数值）；显式给了 emissive_color 时按 sRGB 转线性。
    if m.emissive > 0.0 || m.emissive_color.is_some() {
        desc.emissive = match m.emissive_color {
            Some(c) => Vec3::new(
                quench3d::render::color::srgb_to_linear(c[0]),
                quench3d::render::color::srgb_to_linear(c[1]),
                quench3d::render::color::srgb_to_linear(c[2]),
            ),
            None => Vec3::new(m.color[0], m.color[1], m.color[2]),
        };
        desc.emissive_strength = m.emissive;
    }
    if let Some(u) = m.uv_scale {
        desc.uv_scale = u;
    }
    if let Some(n) = m.normal_scale {
        desc.normal_scale = n;
    }
    if let Some(cut) = m.alpha_cutoff {
        desc.alpha_cutoff = cut;
    }
    if let Some(b) = m.alpha_blend {
        desc.alpha_blend = b;
    }
    if let Some(u) = m.unlit {
        desc.unlit = u;
    }
    if let Some(d) = m.double_sided {
        desc.double_sided = d;
    }

    for (field, path, srgb) in m.textures() {
        let Some(path) = path else { continue };
        let id = texture_from_doc(
            engine,
            doc,
            &format!("materials.{name}.{field}"),
            path,
            srgb,
            cache,
        )?;
        match field {
            "base_color_texture" => desc.base_color_texture = Some(id),
            "normal_texture" => desc.normal_texture = Some(id),
            "metallic_roughness_texture" => desc.metallic_roughness_texture = Some(id),
            "emissive_texture" => desc.emissive_texture = Some(id),
            _ => {}
        }
    }
    Ok(desc)
}

/// 加载/复用一张贴图。
fn texture_from_doc(
    engine: &mut Engine,
    doc: &SceneDoc,
    field_path: &str,
    rel: &str,
    srgb: bool,
    cache: &mut TextureCache,
) -> Result<TextureId, RunError> {
    let path = match doc.resolve_asset(rel) {
        Some(p) => p,
        None => {
            let mut err = RunError::io(format!(
                "找不到贴图文件 \"{rel}\"（贴图路径相对场景文件所在目录解析，也可以写绝对路径）"
            ));
            err.path = Some(field_path.to_string());
            return Err(err);
        }
    };
    let key = (path.display().to_string(), srgb);
    if let Some(id) = cache.get(&key) {
        return Ok(*id);
    }
    let image = ImageData::from_file(&path).map_err(|e| {
        RunError::io(format!(
            "{field_path}: 贴图 {} 解码失败: {e}",
            path.display()
        ))
    })?;
    let id = engine.renderer.add_texture(&engine.gpu, &image, srgb);
    cache.insert(key, id);
    Ok(id)
}

/// 把场景里的 bloom 配置翻成渲染器选项（缺省关闭）。
fn bloom_options(doc: &SceneDoc) -> quench3d::render::BloomOptions {
    let Some(b) = &doc.lighting.bloom else {
        return quench3d::render::BloomOptions::default();
    };
    quench3d::render::BloomOptions {
        enabled: b.enabled.unwrap_or(true),
        intensity: b.intensity.unwrap_or(0.6),
        threshold: b.threshold.unwrap_or(1.0),
    }
}

/// 把场景里的 FXAA 配置翻成渲染器选项（缺省关闭）。
fn fxaa_options(doc: &SceneDoc) -> quench3d::render::FxaaOptions {
    let Some(f) = &doc.lighting.fxaa else {
        return quench3d::render::FxaaOptions::default();
    };
    let defaults = quench3d::render::FxaaOptions::default();
    quench3d::render::FxaaOptions {
        enabled: f.enabled.unwrap_or(true),
        edge_threshold: f.edge_threshold.unwrap_or(defaults.edge_threshold),
        edge_threshold_min: f.edge_threshold_min.unwrap_or(defaults.edge_threshold_min),
    }
}

fn apply_lighting(engine: &mut Engine, doc: &SceneDoc) {
    let l = &doc.lighting;
    // 未知预设由 `SceneDoc::validate` 拦下（`load_scene` 会拒绝带 error 的场景），
    // 所以这里的兜底分支只对"没写 preset"生效。
    engine.scene.lighting = match l.preset.as_deref() {
        Some("night") => Lighting::night(),
        Some("none") => Lighting::none(),
        Some("day") => Lighting::day(),
        _ => Lighting::studio(),
    };
    let lighting = &mut engine.scene.lighting;
    if let Some(dir) = l.sun_direction {
        if let Some(sun) = lighting.sun.as_mut() {
            sun.direction = v3(dir).normalize_or_zero();
        }
    }
    if let Some(intensity) = l.sun_intensity {
        if let Some(sun) = lighting.sun.as_mut() {
            sun.intensity = intensity;
        }
    }
    if let Some(a) = l.ambient {
        lighting.ambient_intensity = a;
    }
    if let Some(e) = l.exposure {
        lighting.exposure = e;
    }
    if let Some(sky) = l.sky {
        lighting.sky.enabled = sky;
    }
    if let Some(grid) = l.grid {
        lighting.sky.show_grid = grid;
    }
    if let Some(shadows) = l.shadows {
        if let Some(sun) = lighting.sun.as_mut() {
            sun.cast_shadows = shadows;
        }
    }
    // 点光源：预设会整体替换 lighting，所以放在最后覆盖。
    // 注意"没写"与"写了空数组"是两种意思：前者保留预设的补光，后者才是显式清空。
    if let Some(lights) = &l.point_lights {
        lighting.point_lights = lights
            .iter()
            .map(|p| PointLight {
                position: v3(p.position),
                color: Vec3::new(p.color[0], p.color[1], p.color[2]),
                intensity: p.intensity,
                radius: p.radius,
            })
            .collect();
    }
}

/// 应用相机配置。
fn apply_camera(engine: &mut Engine, doc: &SceneDoc) {
    let c = &doc.camera;
    engine.camera_mode = CameraMode::Orbit;
    if let Some(fov) = c.fov_deg {
        engine.orbit.fov_y = fov.to_radians();
    }
    if let Some(near) = c.near {
        engine.orbit.near = near;
    }
    if let Some(far) = c.far {
        engine.orbit.far = far;
    }
    // 给了正交高度就切正交投影；不给保持透视（与以往逐位一致）
    engine.orbit.orthographic_height = c.orthographic_height;
    match c.position {
        Some(p) => {
            // 环绕相机的约定：position = focus + distance * (cos(pitch)sin(yaw), sin(pitch), cos(pitch)cos(yaw))
            // 因此由「眼睛 + 注视点」反解时，用**焦点指向眼睛**的向量：
            let eye = v3(p);
            let target = v3(c.look_at.unwrap_or([0.0, 0.0, 0.0]));
            let offset = eye - target;
            let distance = offset.length();
            engine.orbit.focus = target;
            if distance > 1e-4 {
                engine.orbit.distance = distance;
                engine.orbit.yaw = offset.x.atan2(offset.z);
                engine.orbit.pitch = (offset.y / distance).clamp(-1.0, 1.0).asin();
            }
        }
        None => {
            if let Some(d) = c.distance {
                engine.orbit.distance = d;
            }
            if let Some(y) = c.yaw_deg {
                engine.orbit.yaw = y.to_radians();
            }
            if let Some(p) = c.pitch_deg {
                engine.orbit.pitch = p.to_radians();
            }
            if let Some(t) = c.look_at {
                engine.orbit.focus = v3(t);
            }
        }
    }
    if c.frame_scene {
        engine.focus_scene();
    }
}

/// 渲染场景并写 PNG。
pub fn run_render(
    doc: &SceneDoc,
    output_override: Option<&std::path::Path>,
    policy: OutputPolicy,
) -> Result<RenderReport, RunError> {
    let started = Instant::now();
    // 场景说"不要渲染"，就别渲染：宁可报错，也不要无视这个字段（以前它是死字段）
    if !doc.render.enabled {
        return Err(RunError::scene(
            "场景里 render.enabled = false，但本次就是要渲染；要只跑物理请用 simulate",
        ));
    }
    let width = doc.render.width;
    let height = doc.render.height;
    let output = output_override
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| doc.render.output.clone());
    // 覆盖检查放在渲染**之前**：既然要报错，就不该先白花一遍 GPU 时间。
    policy.guard(&output)?;

    // 物理先建好并步进（不需要 GPU），再交给引擎。
    // 走 `Script` 而不是 `WorldBuild::step`：定时动作要能在这里生效，探针要能采到状态。
    let mut build = build_world(doc)?;
    let mut script = Script::new(doc, &build.world, &build.handles)?;
    script.begin(&build.world);
    for _ in 0..doc.simulate.steps {
        script.step(&mut build.world, doc.simulate.dt);
    }
    let probes = script.report();
    let sim = if doc.simulate.steps > 0 {
        let mut rep = summarize(&build, doc.simulate.steps, doc.simulate.dt);
        rep.probes.clone_from(&probes);
        Some(rep)
    } else {
        None
    };
    if policy.dry_run {
        // 只跑物理：不建 GPU 上下文、不写盘。渲染相关的统计保持 0，
        // 由 `dry_run: true` 与 `would_write` 说明"这些数不是真的没有，而是没测"。
        return Ok(RenderReport {
            output: output.display().to_string(),
            width,
            height,
            bytes: 0,
            draw_calls: 0,
            instances: 0,
            triangles: 0,
            culled: 0,
            shadow_instances: 0,
            lines: 0,
            objects: 0,
            camera_position: [0.0; 3],
            probes,
            simulate: sim,
            dry_run: true,
            would_write: vec![output.display().to_string()],
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    }

    let WorldBuild {
        world,
        handles,
        born: _,
    } = build;

    let config = EngineConfig {
        title: doc.name.clone().unwrap_or_else(|| "Quench3D".into()),
        width,
        height,
        msaa_samples: doc.render.msaa,
        renderer: quench3d::render::RendererOptions {
            bloom: bloom_options(doc),
            fxaa: fxaa_options(doc),
            ..Default::default()
        },
        ..Default::default()
    };

    let mut scene_objects = 0usize;
    let mut camera_position = [0.0f32; 3];
    // 闭包签名是 `FnMut(&mut Engine) -> ()`，所以先把错误兜住，渲染结束后再冒泡。
    let mut setup_error: Option<RunError> = None;
    let (image, stats) = quench3d::render_offscreen_stats(width, height, &config, |engine| {
        engine.world = world;
        if let Err(e) = setup_visuals(engine, doc, &handles) {
            setup_error = Some(e);
            return;
        }
        engine.scene.sync_from_physics(&engine.world);
        scene_objects = engine.scene.len();
        camera_position = arr(engine.camera().position);
    })
    .map_err(RunError::env)?;
    if let Some(e) = setup_error {
        return Err(e);
    }

    if let Some(dir) = output.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)
                .map_err(|e| RunError::io(format!("创建输出目录失败: {e}")))?;
        }
    }
    quench3d::write_png(&image, &output)
        .map_err(|e| RunError::io(format!("写入 PNG 失败: {e}")))?;
    let bytes = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);

    let RenderStats {
        draw_calls,
        instances,
        triangles,
        culled,
        shadow_instances,
        lines,
        ..
    } = stats;

    Ok(RenderReport {
        output: output.display().to_string(),
        width,
        height,
        bytes,
        draw_calls,
        instances,
        triangles,
        culled,
        shadow_instances,
        lines,
        objects: scene_objects,
        camera_position,
        probes,
        simulate: sim,
        dry_run: false,
        would_write: Vec::new(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// FNV-1a 64 位的初始值（帧序列可复现性摘要用）。
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a 步进：把 `bytes` 混进 `hash`。
fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// 帧序列的封装格式。
///
/// `Gif` / `Apng` 都是**纯 Rust、免专利**的内置编码器：不装 ffmpeg 也能得到
/// 一个能直接播放/嵌进 README 的文件。代价是 GIF 只有 256 色（逐帧局部调色板）、
/// APNG 无损但体积更大；要长时间线/高压缩率仍然走 `Png` + 外部编码器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovieEncoder {
    /// 每帧一个 PNG（缺省）：可断点续录，编码交给外部工具。
    Png,
    /// 单个 GIF（256 色，适合几十帧的演示）。
    Gif,
    /// 单个 APNG（无损动画 PNG）。
    Apng,
}

impl MovieEncoder {
    /// 命令行/报告里的名字。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Gif => "gif",
            Self::Apng => "apng",
        }
    }

    /// 解析命令行取值；未知取值返回 `None`（调用方负责报错）。
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "gif" => Some(Self::Gif),
            "apng" => Some(Self::Apng),
            _ => None,
        }
    }

    /// 是否输出单个文件（PNG 之外都是）。
    pub fn single_file(self) -> bool {
        !matches!(self, Self::Png)
    }

    /// 单个文件的缺省名字。
    pub fn default_output(self) -> &'static str {
        match self {
            Self::Png => "frames",
            Self::Gif => "movie.gif",
            Self::Apng => "movie.png",
        }
    }
}

/// 解析单文件编码器的最终输出路径；PNG 序列返回 `None`。
fn resolve_output(encoder: MovieEncoder, explicit: Option<&Path>) -> Option<PathBuf> {
    if !encoder.single_file() {
        return None;
    }
    Some(match explicit {
        Some(p) => p.to_path_buf(),
        None => PathBuf::from(encoder.default_output()),
    })
}

/// 帧输出目标：PNG 逐帧落盘，GIF/APNG 流式写进单个文件。
enum FrameSink {
    /// 每帧一个 PNG（`(目录, 已写字节)`）。
    Png(PathBuf),
    /// GIF 编码器 + 帧延迟（单位 10 ms）+ 复用的 RGBA 暂存缓冲。
    Gif {
        encoder: Box<gif::Encoder<std::io::BufWriter<std::fs::File>>>,
        delay_cs: u16,
        scratch: Vec<u8>,
    },
    /// APNG 写入器 + 帧延迟分式（`num/den` 秒）。
    Apng {
        writer: Box<png::Writer<std::io::BufWriter<std::fs::File>>>,
        delay_num: u16,
        delay_den: u16,
    },
}

impl FrameSink {
    /// 建好输出目标（GIF/APNG 会先写文件头，所以总帧数必须已知）。
    fn create(
        encoder: MovieEncoder,
        frames_dir: &Path,
        output: Option<&Path>,
        width: u32,
        height: u32,
        total_frames: u32,
        fps: f32,
    ) -> Result<Self, RunError> {
        match encoder {
            MovieEncoder::Png => Ok(Self::Png(frames_dir.to_path_buf())),
            MovieEncoder::Gif => {
                let path = output
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from(MovieEncoder::Gif.default_output()));
                let file = std::fs::File::create(&path)
                    .map_err(|e| RunError::io(format!("创建 {} 失败: {e}", path.display())))?;
                let writer = std::io::BufWriter::new(file);
                // 空全局调色板 → 每帧自带局部调色板（逐帧自适应量化）
                let encoder = gif::Encoder::new(writer, width as u16, height as u16, &[])
                    .map_err(|e| RunError::io(format!("初始化 GIF 编码器失败: {e}")))?;
                // GIF 延迟单位是 10 ms
                let delay_cs = (100.0 / fps.max(0.01)).round().clamp(1.0, 65535.0) as u16;
                Ok(Self::Gif {
                    encoder: Box::new(encoder),
                    delay_cs,
                    scratch: Vec::new(),
                })
            }
            MovieEncoder::Apng => {
                let path = output
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from(MovieEncoder::Apng.default_output()));
                let file = std::fs::File::create(&path)
                    .map_err(|e| RunError::io(format!("创建 {} 失败: {e}", path.display())))?;
                let writer = std::io::BufWriter::new(file);
                let mut encoder = png::Encoder::new(writer, width, height);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                // num_plays = 0 → 无限循环
                encoder
                    .set_animated(total_frames.max(1), 0)
                    .map_err(|e| RunError::io(format!("初始化 APNG 编码器失败: {e}")))?;
                let writer = encoder
                    .write_header()
                    .map_err(|e| RunError::io(format!("写入 APNG 头失败: {e}")))?;
                // 延迟写成 num/den 秒：用 fps×100 做分母，60fps → 100/6000
                let delay_num = 100u16;
                let delay_den = (fps.max(0.01) * 100.0).round().clamp(1.0, 60000.0) as u16;
                Ok(Self::Apng {
                    writer: Box::new(writer),
                    delay_num,
                    delay_den,
                })
            }
        }
    }

    /// 写一帧；返回本次直接落盘的字节数（GIF/APNG 在收尾时才算总大小）。
    ///
    /// 对 PNG 目标，已存在的帧会被跳过（断点续录），返回值是 `(写了吗, 字节数)`。
    fn write(
        &mut self,
        frame: u32,
        image: &quench3d::render::ImageData,
    ) -> Result<(bool, u64), String> {
        match self {
            Self::Png(dir) => {
                let name = format!("frame_{frame:05}.png");
                let path = dir.join(&name);
                if path.exists() {
                    return Ok((false, 0));
                }
                write_png(image, &path).map_err(|e| format!("写入 {name} 失败: {e}"))?;
                Ok((true, std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0)))
            }
            Self::Gif {
                encoder,
                delay_cs,
                scratch,
            } => {
                scratch.clear();
                scratch.extend_from_slice(&image.rgba);
                // speed 越大越快、质量越低；10 在演示片段上肉眼几乎无差
                let mut gif_frame = gif::Frame::from_rgba_speed(
                    image.width as u16,
                    image.height as u16,
                    scratch,
                    10,
                );
                gif_frame.delay = *delay_cs;
                // 每帧都是整幅画面：用 Background 清掉上一帧，避免残影
                gif_frame.dispose = gif::DisposalMethod::Background;
                encoder
                    .write_frame(&gif_frame)
                    .map_err(|e| format!("写入 GIF 第 {frame} 帧失败: {e}"))?;
                Ok((true, 0))
            }
            Self::Apng {
                writer,
                delay_num,
                delay_den,
            } => {
                writer
                    .set_frame_delay(*delay_num, *delay_den)
                    .map_err(|e| format!("设置 APNG 帧延迟失败: {e}"))?;
                writer
                    .write_image_data(&image.rgba)
                    .map_err(|e| format!("写入 APNG 第 {frame} 帧失败: {e}"))?;
                Ok((true, 0))
            }
        }
    }

    /// 收尾：GIF/APNG 写结束标记（PNG 不需要做任何事）。
    fn finish(self) -> Result<(), RunError> {
        match self {
            Self::Png(_) => Ok(()),
            Self::Gif { encoder, .. } => {
                let writer = encoder
                    .into_inner()
                    .map_err(|e| RunError::io(format!("收尾 GIF 失败: {e}")))?;
                // 显式 flush：不依赖 Drop
                writer
                    .into_inner()
                    .map_err(|e| RunError::io(format!("刷新 GIF 失败: {e}")))?
                    .sync_all()
                    .map_err(|e| RunError::io(format!("同步 GIF 失败: {e}")))?;
                Ok(())
            }
            Self::Apng { writer, .. } => writer
                .finish()
                .map_err(|e| RunError::io(format!("收尾 APNG 失败: {e}"))),
        }
    }
}

/// 录制仿真视频：逐帧推进物理并渲染成 PNG 序列 + `trace.jsonl`。
///
/// 与 [`run_render`] 的关键区别是**只创建一次 GPU 上下文**，然后在循环里逐帧渲染
/// （`render_offscreen` 每次调用都会重建 headless 设备，逐帧调用会被初始化开销淹没：
/// 实测约 1.1 秒/次）。帧数与时长由**物理步**决定而不依赖墙钟，因此同一场景两次录制
/// 逐像素一致（`digest` 可直接用于断言）。
///
/// 编码交给外部工具：返回的 `ffmpeg` 字段给出建议命令。已存在的帧不再写盘，
/// 所以长视频中断后可以重跑续录。
pub fn run_movie(doc: &SceneDoc, opts: &MovieOptions) -> Result<MovieReport, RunError> {
    let started = Instant::now();
    // `render.enabled = false` 与「录视频」直接冲突：宁可报错，也不要渲染出一堆
    // 用户明确说过不要的帧（这个字段在 render 路径上仍是死字段，见 gap_analysis §3.3）。
    if !doc.render.enabled {
        return Err(RunError::scene(
            "场景里 render.enabled = false，但 movie 的目的就是渲染；只跑物理请用 simulate",
        ));
    }
    let WorldBuild {
        mut world,
        handles,
        born: _,
    } = build_world(doc)?;

    let policy = OutputPolicy {
        no_clobber: opts.no_clobber,
        dry_run: opts.dry_run,
    };
    let output_path = resolve_output(opts.encoder, opts.output.as_deref());
    let trace_path = match &output_path {
        Some(path) => path.with_extension("trace.jsonl"),
        None => opts.frames_dir.join("trace.jsonl"),
    };
    if policy.no_clobber {
        match &output_path {
            Some(path) => policy.guard(path)?,
            // PNG 序列没有单文件可查，就查目录：里面有东西说明是上一次录的
            None if dir_has_entries(&opts.frames_dir) => {
                return Err(RunError::output_exists(&opts.frames_dir))
            }
            None => {}
        }
    }

    if policy.dry_run {
        // 只跑物理 + 采探针：不建 GPU、不建目录、不写帧也不写 trace。
        let dt = doc.simulate.dt;
        let params = SequenceParams {
            frames: opts.frames,
            steps_per_frame: opts.steps_per_frame,
            dt,
            warmup_frames: opts.warmup_frames,
        };
        let mut script = Script::new(doc, &world, &handles)?;
        script.begin(&world);
        for _ in 0..params.total_steps() {
            script.step(&mut world, dt);
        }
        let mut would_write = vec![output_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| opts.frames_dir.display().to_string())];
        would_write.push(trace_path.display().to_string());
        return Ok(MovieReport {
            frames_dir: opts.frames_dir.display().to_string(),
            encoder: opts.encoder.as_str().to_string(),
            output: output_path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            trace: trace_path.display().to_string(),
            frames: opts.frames,
            skipped: 0,
            fps: opts.fps,
            duration: params.duration(),
            steps_per_frame: opts.steps_per_frame,
            dt,
            total_steps: params.total_steps(),
            warmup_frames: opts.warmup_frames,
            width: opts.width,
            height: opts.height,
            bytes: 0,
            digest: String::new(),
            objects: 0,
            camera_position: [0.0; 3],
            probes: script.report(),
            first_frame: None,
            last_frame: None,
            ffmpeg: String::new(),
            notes: vec!["--dry-run：只跑了物理与校验，没有渲染、没有写盘（digest 为空）".into()],
            dry_run: true,
            would_write,
            elapsed_ms: started.elapsed().as_millis() as u64,
        });
    }

    // PNG 序列需要一个目录；单文件编码器（gif/apng）不需要，硬建出空目录只会让人困惑。
    if !opts.encoder.single_file() {
        std::fs::create_dir_all(&opts.frames_dir).map_err(|e| {
            RunError::io(format!(
                "创建帧目录 {} 失败: {e}",
                opts.frames_dir.display()
            ))
        })?;
    }

    let config = EngineConfig {
        title: doc.name.clone().unwrap_or_else(|| "Quench3D".into()),
        width: opts.width,
        height: opts.height,
        msaa_samples: doc.render.msaa,
        renderer: quench3d::render::RendererOptions {
            bloom: bloom_options(doc),
            fxaa: fxaa_options(doc),
            ..Default::default()
        },
        ..Default::default()
    };
    let params = SequenceParams {
        frames: opts.frames,
        steps_per_frame: opts.steps_per_frame,
        dt: doc.simulate.dt,
        warmup_frames: opts.warmup_frames,
    };

    let mut rows: Vec<MovieFrame> = Vec::with_capacity(opts.frames as usize);
    let mut skipped = 0u32;
    let mut bytes = 0u64;
    let mut digest = FNV_OFFSET;
    let mut scene_objects = 0usize;
    let mut camera_position = [0.0f32; 3];
    let mut sink = FrameSink::create(
        opts.encoder,
        &opts.frames_dir,
        output_path.as_deref(),
        opts.width,
        opts.height,
        opts.frames,
        opts.fps,
    )?;

    let mut setup_error: Option<RunError> = None;
    // 定时动作与探针由这个 `Game` 驱动：它的 `fixed_update` 复刻 `NoGame` 的行为
    // （不配 actions/probes 时就是一个裸的 `world.step`），所以缺省录制的像素逐位不变。
    let mut script = Script::new(doc, &world, &handles)?;
    script.begin(&world);
    let mut game = ScriptedGame { script };
    render_sequence_with_game(
        opts.width,
        opts.height,
        &config,
        params,
        &mut game,
        |engine| {
            engine.world = world;
            if let Err(e) = setup_visuals(engine, doc, &handles) {
                setup_error = Some(e);
                return;
            }
            engine.scene.sync_from_physics(&engine.world);
            scene_objects = engine.scene.len();
            camera_position = arr(engine.camera().position);
        },
        |_engine, frame, image| {
            // 摘要覆盖「帧号 + 像素」，所以帧顺序或画面任何变化都会反映出来。
            digest = fnv1a(digest, &frame.frame.to_le_bytes());
            digest = fnv1a(digest, &image.rgba);

            let (written, size) = sink.write(frame.frame, image)?;
            if !written {
                // 断点续录：已存在的帧不再写盘（渲染结果本来就可复现）。
                skipped += 1;
            }
            bytes += size;
            let name = match opts.encoder {
                MovieEncoder::Png => format!("frame_{:05}.png", frame.frame),
                other => other.default_output().to_string(),
            };

            rows.push(MovieFrame {
                frame: frame.frame,
                file: name,
                steps: frame.steps,
                time: frame.time,
                draw_calls: frame.stats.draw_calls,
                instances: frame.stats.instances,
                triangles: frame.stats.triangles,
                culled: frame.stats.culled,
                shadow_instances: frame.stats.shadow_instances,
            });
            Ok(())
        },
    )
    .map_err(RunError::env)?;
    // 建场景/加载贴图失败时闭包只记了错误；这里立刻冒泡，不要吐出一份空场景的录像
    if let Some(e) = setup_error {
        return Err(e);
    }

    // GIF/APNG 在收尾时才写结束标记，文件大小也只有那时才知道
    sink.finish()?;
    if let Some(path) = &output_path {
        bytes += std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    }

    // trace 跟着产物走：单文件编码器就放在输出文件旁边（`demo.gif` → `demo.trace.jsonl`）
    let mut body = String::new();
    for row in &rows {
        body.push_str(
            &serde_json::to_string(row)
                .map_err(|e| RunError::io(format!("序列化帧记录失败: {e}")))?,
        );
        body.push('\n');
    }
    std::fs::write(&trace_path, body)
        .map_err(|e| RunError::io(format!("写入 {} 失败: {e}", trace_path.display())))?;

    // GIF 的帧延迟单位是 10 ms：不是 10 的整数倍的帧率会被量化，
    // 时长跟着变（60 fps → 20 ms/帧 → 实际 50 fps）。这是格式限制，
    // 但必须说出来，不能让用户对着 0.48 s 的视频纳闷。
    let mut notes: Vec<String> = Vec::new();
    if opts.encoder == MovieEncoder::Gif {
        let cs = 100.0 / opts.fps;
        let rounded = cs.round().max(1.0);
        if (cs - rounded).abs() > 1e-3 {
            notes.push(format!(
                "GIF 帧延迟只有 10 ms 粒度：{:.2} fps 量化成 {:.2} fps（每帧 {} ms），总时长 {:.2} s → {:.2} s",
                opts.fps,
                100.0 / rounded,
                (rounded * 10.0) as u32,
                params.duration(),
                (rounded * 10.0 / 1000.0) * rows.len() as f32,
            ));
        }
    }

    // ffmpeg 的 -framerate 只接受整数/分式，这里取整（缺省即等价帧率）。
    // 内置编码器（gif/apng）已经产出了可直接播放的文件，不需要外部工具。
    let fps_int = opts.fps.round().max(1.0) as u32;
    let ffmpeg = if opts.encoder == MovieEncoder::Png {
        format!(
            "ffmpeg -framerate {fps_int} -i {}/frame_%05d.png -c:v libx264 -preset slow -crf 18 -pix_fmt yuv420p -movflags +faststart movie.mp4",
            opts.frames_dir.display()
        )
    } else {
        String::new()
    };

    Ok(MovieReport {
        frames_dir: opts.frames_dir.display().to_string(),
        encoder: opts.encoder.as_str().to_string(),
        output: output_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        trace: trace_path.display().to_string(),
        frames: rows.len() as u32,
        skipped,
        fps: opts.fps,
        duration: params.duration(),
        steps_per_frame: opts.steps_per_frame,
        dt: doc.simulate.dt,
        total_steps: params.total_steps(),
        warmup_frames: opts.warmup_frames,
        width: opts.width,
        height: opts.height,
        bytes,
        digest: format!("fnv1a64:{digest:016x}"),
        objects: scene_objects,
        camera_position,
        probes: game.script.report(),
        first_frame: rows.first().cloned(),
        last_frame: rows.last().cloned(),
        ffmpeg,
        notes,
        dry_run: false,
        would_write: Vec::new(),
        elapsed_ms: started.elapsed().as_millis() as u64,
    })
}

/// 读取模型信息（不需要 GPU）。
pub fn run_info(path: &std::path::Path) -> Result<ModelInfo, RunError> {
    let model = quench3d::assets::load_model(path)
        .map_err(|e| RunError::scene(format!("加载模型失败: {e}")))?;
    let bounds = model.bounds();
    let format = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    Ok(ModelInfo {
        path: path.display().to_string(),
        format,
        meshes: model.meshes.len(),
        materials: model.materials.len(),
        images: model.images.len(),
        vertices: model.vertex_count(),
        triangles: model.triangle_count(),
        bounds_min: arr(bounds.min),
        bounds_max: arr(bounds.max),
        bounds_size: arr(bounds.size()),
        mesh_triangles: model
            .meshes
            .iter()
            .map(|m| m.data.triangle_count())
            .collect(),
    })
}

/// 环境自检：创建一次 64×64 的 GPU 上下文，报告适配器与后端。
pub fn run_doctor() -> DoctorReport {
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let mut report = DoctorReport {
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        cpu_threads: threads,
        gpu_available: false,
        adapter: String::new(),
        backend: String::new(),
        device_type: String::new(),
        software_rasterizer: false,
        notes: Vec::new(),
    };
    match pollster::block_on(quench3d::render::GpuContext::headless(
        64,
        64,
        Default::default(),
    )) {
        Ok(ctx) => {
            report.gpu_available = true;
            report.adapter = ctx.adapter_info.name.clone();
            report.backend = format!("{:?}", ctx.adapter_info.backend);
            report.device_type = format!("{:?}", ctx.adapter_info.device_type);
            report.software_rasterizer = ctx.is_software();
            if ctx.is_software() {
                report
                    .notes
                    .push("软件光栅化：渲染可用但较慢，建议分辨率不超过 1280×800".into());
            }
        }
        Err(e) => {
            report.notes.push(format!("无法创建 GPU 上下文: {e}"));
            report
                .notes
                .push("渲染类命令会失败；请安装显卡驱动或 Vulkan / 软件光栅后端".into());
        }
    }
    report
}
