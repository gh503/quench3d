//! `quench3d bench`：把「快不快、省不省」变成可复现的数字。
//!
//! 一次运行会报告：
//! * **启动开销**：进程启动到开始测量之间的耗时（含参数解析、CLI 初始化）；
//! * **物理扩展性**：多个刚体规模下的每步耗时、步/秒、体·步/秒；
//! * **轨迹采样开销**：同一个场景「不开轨迹 / 开轨迹」的耗时对比；
//! * **渲染**：单帧耗时与绘制统计（无 GPU 时记为 `null`，不会让命令失败）；
//! * **内存**：当前与峰值常驻内存（Linux 读 `/proc/self/status`，其它平台为 `null`）。
//!
//! 设计原则：**不需要 GPU、不需要网络、不改动任何文件**，因此在 CI 和容器里都能跑；
//! 数字来自当前这台机器，用于横向对比与回归，而不是绝对基准。

use std::time::Instant;

use quench3d::math::Vec3;
use quench3d::physics::{BodyBuilder, PhysicsWorld, Shape};
use serde::Serialize;

use crate::runner;
use crate::scene_doc::SceneDoc;

/// 场景布局。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// 单层平铺：接触数与刚体数同阶，反映「开阔场景」的扩展性。
    Scatter,
    /// 逐层堆叠：接触图密集，反映「深堆 / 塔」的最坏情况。
    Stack,
}

impl Layout {
    /// 从字符串解析。
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "scatter" => Some(Self::Scatter),
            "stack" => Some(Self::Stack),
            _ => None,
        }
    }

    /// 中文名。
    pub fn name(&self) -> &'static str {
        match self {
            Self::Scatter => "单层平铺",
            Self::Stack => "逐层堆叠",
        }
    }
}

/// 基准参数。
#[derive(Clone, Debug)]
pub struct BenchOptions {
    /// 要测的刚体规模。
    pub bodies: Vec<usize>,
    /// 每个规模测多少步。
    pub steps: u32,
    /// 预热步数（让缓冲扩容到位、进入稳态）。
    pub warmup: u32,
    /// 布局。
    pub layout: Layout,
    /// 是否启用睡眠（关掉更接近「一直在算」的最坏情况）。
    pub sleep: bool,
    /// 是否附带渲染一帧。
    pub render: bool,
    /// 启动到现在的耗时（毫秒，保留小数），由调用方传入。
    pub startup_ms: f64,
}

impl Default for BenchOptions {
    fn default() -> Self {
        Self {
            bodies: vec![100, 500, 1000],
            steps: 300,
            warmup: 30,
            layout: Layout::Scatter,
            sleep: true,
            render: true,
            startup_ms: 0.0,
        }
    }
}

/// 单个规模的测量结果。
#[derive(Clone, Debug, Serialize)]
pub struct ScaleResult {
    /// 刚体数（不含地面）。
    pub bodies: usize,
    /// 步数。
    pub steps: u32,
    /// 总耗时（毫秒）。
    pub elapsed_ms: f64,
    /// 每步耗时（毫秒）。
    pub ms_per_step: f64,
    /// 每秒步数。
    pub steps_per_sec: f64,
    /// 每秒「刚体·步」数（跨规模可比的吞吐指标）。
    pub body_steps_per_sec: f64,
    /// 跑完后处于睡眠的刚体数。
    pub sleeping: usize,
    /// 最终接触数。
    pub contacts: usize,
}

/// 轨迹采样的开销对比。
#[derive(Clone, Debug, Serialize)]
pub struct TraceCost {
    /// 用来说明测的是哪个场景。
    pub scene: String,
    /// 不开轨迹的耗时（毫秒）。
    pub without_trace_ms: f64,
    /// 开轨迹（`--trace-all`）的耗时（毫秒）。
    pub with_trace_ms: f64,
    /// 相对开销（百分比）。
    pub overhead_percent: f64,
    /// 采集到的采样点总数。
    pub samples: usize,
}

/// 渲染测量。
#[derive(Clone, Debug, Serialize)]
pub struct RenderCost {
    /// 输出文件。
    pub output: String,
    /// 分辨率。
    pub width: u32,
    /// 分辨率。
    pub height: u32,
    /// 单帧耗时（含 GPU 初始化）。
    pub elapsed_ms: u64,
    /// 绘制调用数。
    pub draw_calls: u32,
    /// 实例数。
    pub instances: u32,
    /// 三角形数。
    pub triangles: u64,
}

/// 完整报告。
#[derive(Clone, Debug, Serialize)]
pub struct BenchReport {
    /// 平台。
    pub platform: String,
    /// CPU 架构。
    pub arch: String,
    /// 可用并行度。
    pub threads: usize,
    /// 进程启动到开始测量（毫秒）。
    pub startup_ms: f64,
    /// 当前常驻内存（MB，Linux 才有）。
    pub rss_mb: Option<f64>,
    /// 峰值常驻内存（MB，Linux 才有）。
    pub peak_rss_mb: Option<f64>,
    /// 布局。
    pub layout: Layout,
    /// 是否启用睡眠。
    pub sleep: bool,
    /// 各规模结果。
    pub scales: Vec<ScaleResult>,
    /// 轨迹开销。
    pub trace: Option<TraceCost>,
    /// 渲染结果（无 GPU 时为 `None`）。
    pub render: Option<RenderCost>,
    /// 渲染失败原因（无 GPU 时说明）。
    pub render_note: Option<String>,
    /// 备注：怎么读这些数字。
    pub notes: Vec<String>,
}

/// 内存快照 `(当前, 峰值)`，单位 MB。
fn memory_mb() -> Option<(f64, f64)> {
    let text = std::fs::read_to_string("/proc/self/status").ok()?;
    let mut current = None;
    let mut peak = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            current = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok())
                .map(|kb| kb / 1024.0);
        } else if let Some(rest) = line.strip_prefix("VmHWM:") {
            peak = rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok())
                .map(|kb| kb / 1024.0);
        }
    }
    current.zip(peak)
}

/// 造一个测试场景：地面 + `n` 个刚体。
fn make_world(n: usize, layout: Layout, sleep: bool) -> PhysicsWorld {
    let mut world = PhysicsWorld::new();
    world.set_gravity(Vec3::new(0.0, -9.81, 0.0));
    world.config.sleep_enabled = sleep;
    world.add_body(
        BodyBuilder::r#static(Shape::Plane {
            normal: Vec3::new(0.0, 1.0, 0.0),
        })
        .build(),
    );
    let cols = ((n as f32).sqrt().ceil() as usize).max(1);
    let side = 10usize;
    for i in 0..n {
        let (x, y, z) = match layout {
            Layout::Scatter => {
                let (row, col) = (i / cols, i % cols);
                (row as f32 * 2.5, 0.4, col as f32 * 2.5)
            }
            Layout::Stack => {
                let per_layer = side * side;
                let layer = (i / per_layer) as f32;
                let k = i % per_layer;
                (
                    (k % side) as f32 * 1.02 - side as f32 * 0.5,
                    0.4 + layer * 0.85,
                    (k / side) as f32 * 1.02 - side as f32 * 0.5,
                )
            }
        };
        world.add_body(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::splat(0.4),
            })
            .position(Vec3::new(x, y, z))
            .build(),
        );
    }
    world
}

/// 跑一个规模的测量。
fn measure(bodies: usize, options: &BenchOptions) -> ScaleResult {
    let mut world = make_world(bodies, options.layout, options.sleep);
    for _ in 0..options.warmup {
        world.step(1.0 / 60.0);
    }
    let started = Instant::now();
    for _ in 0..options.steps {
        world.step(1.0 / 60.0);
    }
    let elapsed = started.elapsed().as_secs_f64();
    let steps = options.steps.max(1);
    let mut sleeping = 0usize;
    for (_, body) in world.bodies() {
        if body.is_sleeping() {
            sleeping += 1;
        }
    }
    ScaleResult {
        bodies,
        steps: options.steps,
        elapsed_ms: elapsed * 1000.0,
        ms_per_step: elapsed * 1000.0 / steps as f64,
        steps_per_sec: steps as f64 / elapsed,
        body_steps_per_sec: steps as f64 * bodies as f64 / elapsed,
        sleeping,
        contacts: world.contacts().len(),
    }
}

/// 轨迹采样开销：同一个场景分别不开 / 开 `--trace-all` 跑一遍。
fn measure_trace(steps: u32) -> Option<TraceCost> {
    let mut doc = SceneDoc::from_json(crate::schema::EXAMPLE_SCENE).ok()?;
    doc.simulate.steps = steps.max(1);

    let started = Instant::now();
    runner::run_simulate(&doc).ok()?;
    let without = started.elapsed().as_secs_f64() * 1000.0;

    // 跟踪所有物体（含地面），因此每条轨迹都会按采样上限被自动抽稀
    // 展开模板/实例后的名字（副本也是独立刚体）
    let expanded = doc.expanded();
    let names: Vec<String> = (0..expanded.objects.len())
        .map(|i| runner::object_name(&expanded, i))
        .collect();
    let started = Instant::now();
    let (_, trajectories) = runner::run_simulate_traced(&doc, &names, 1).ok()?;
    let with = started.elapsed().as_secs_f64() * 1000.0;
    let samples: usize = trajectories.iter().map(|t| t.samples.len()).sum();
    Some(TraceCost {
        scene: format!("内置示例（{} 个刚体）", expanded.objects.len()),
        without_trace_ms: without,
        with_trace_ms: with,
        overhead_percent: if without > 0.0 {
            (with - without) / without * 100.0
        } else {
            0.0
        },
        samples,
    })
}

/// 渲染一帧（没有 GPU 就返回原因，不算失败）。
fn measure_render() -> (Option<RenderCost>, Option<String>) {
    let doc = match SceneDoc::from_json(crate::schema::EXAMPLE_SCENE) {
        Ok(doc) => doc,
        Err(e) => return (None, Some(format!("示例场景不可用：{e}"))),
    };
    match runner::run_render(&doc, None, runner::OutputPolicy::default()) {
        Ok(report) => {
            let cleanup = std::fs::remove_file(&report.output);
            let _ = cleanup;
            (
                Some(RenderCost {
                    output: report.output,
                    width: report.width,
                    height: report.height,
                    elapsed_ms: report.elapsed_ms,
                    draw_calls: report.draw_calls,
                    instances: report.instances,
                    triangles: report.triangles,
                }),
                None,
            )
        }
        Err(e) => (None, Some(e.message)),
    }
}

/// 跑完整基准。
pub fn run(options: &BenchOptions) -> BenchReport {
    let mut scales = Vec::with_capacity(options.bodies.len());
    for &n in &options.bodies {
        scales.push(measure(n, options));
    }
    let trace = measure_trace(600);
    let (render, render_note) = if options.render {
        measure_render()
    } else {
        (None, Some("未启用（--no-render）".to_string()))
    };
    let (rss, peak) = match memory_mb() {
        Some((a, b)) => (Some(a), Some(b)),
        None => (None, None),
    };

    let mut notes = vec![
        "数字来自当前机器，用于横向对比与回归，不是绝对基准。".to_string(),
        "关掉睡眠（--no-sleep）才是「一直在算」的最坏情况；默认开启睡眠更接近真实游戏。".to_string(),
        "物理稳态每步堆分配为 0（回归测试 quench3d-cli/tests/steady_state_allocations.rs 盯着这一点）。"
            .to_string(),
    ];
    if !options.render {
        notes.push("渲染未测（--no-render）。".to_string());
    }

    BenchReport {
        platform: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        threads: std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
        startup_ms: options.startup_ms,
        rss_mb: rss,
        peak_rss_mb: peak,
        layout: options.layout,
        sleep: options.sleep,
        scales,
        trace,
        render,
        render_note,
        notes,
    }
}

/// 人可读输出。
pub fn human(report: &BenchReport) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "平台      : {} / {} · {} 线程\n",
        report.platform, report.arch, report.threads
    ));
    out.push_str(&format!(
        "启动开销  : {:.2} ms（进程启动到开始测量）\n",
        report.startup_ms
    ));
    match (report.rss_mb, report.peak_rss_mb) {
        (Some(cur), Some(peak)) => {
            out.push_str(&format!(
                "内存      : 常驻 {cur:.1} MB · 峰值 {peak:.1} MB\n"
            ));
        }
        _ => out.push_str("内存      : 本平台未提供（仅 Linux 读 /proc）\n"),
    }
    out.push_str(&format!(
        "布局      : {} · 睡眠 {}\n",
        report.layout.name(),
        if report.sleep { "开启" } else { "关闭" }
    ));
    out.push_str("\n刚体数        每步        步/秒        体·步/秒     睡眠  接触\n");
    let mut slept_through = false;
    for s in &report.scales {
        out.push_str(&format!(
            "{:<8} {:>9.3} ms {:>11.0} {:>14.0} {:>6} {:>5}\n",
            s.bodies, s.ms_per_step, s.steps_per_sec, s.body_steps_per_sec, s.sleeping, s.contacts
        ));
        if report.sleep && s.sleeping >= s.bodies {
            slept_through = true;
        }
    }
    if slept_through {
        out.push_str(
            "注意      : 有规模跑完后全部入睡，此时每步几乎不花时间；\n            想看「一直在算」的数字请加 --no-sleep。\n",
        );
    }
    if let Some(t) = &report.trace {
        out.push_str(&format!(
            "\n轨迹采样  : {} · 不开 {:.1} ms / 开 {:.1} ms（{:+.1}%）· {} 个采样点\n",
            t.scene, t.without_trace_ms, t.with_trace_ms, t.overhead_percent, t.samples
        ));
    }
    match (&report.render, &report.render_note) {
        (Some(r), _) => out.push_str(&format!(
            "渲染一帧  : {} ms（{}×{}，绘制调用 {}，实例 {}，三角形 {}）\n",
            r.elapsed_ms, r.width, r.height, r.draw_calls, r.instances, r.triangles
        )),
        (None, Some(note)) => out.push_str(&format!("渲染一帧  : 未测量（{note}）\n")),
        (None, None) => {}
    }
    out
}
