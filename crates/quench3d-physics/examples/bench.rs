//! 物理性能与分配基准（release 下跑）。
//!
//! ```bash
//! cargo run --release -p quench3d-physics --example bench
//! cargo run --release -p quench3d-physics --example bench -- --steps 600 --bodies 100,500,1000
//! ```
//!
//! 输出三样东西，对应「快 / 省 / 稳」：
//! * **吞吐**：每个规模的总耗时、每步耗时、每体每步微秒；
//! * **分配**：实测的分配次数与峰值活动字节（用全局计数分配器统计），
//!   并给出「每步分配次数」——稳态下这个数字应当接近 0；
//! * **稳定性**：长跑若干步后检查 NaN/Inf 与最大速度，并读 `/proc/self/statm`
//!   报告常驻内存（Linux 才有，其它平台自动跳过）。
//!
//! 注意：本文件是**独立 crate**（example），为了用全局分配器统计内存，这里允许
//! `unsafe impl GlobalAlloc`；引擎本体（`quench3d-physics` 库）仍然 `#![forbid(unsafe_code)]`。

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use quench3d_math::Vec3;
use quench3d_physics::{BodyBuilder, PhysicsWorld, Shape};

/// 计数分配器：记录分配/释放次数与当前活动字节。
struct Counting;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCS: AtomicUsize = AtomicUsize::new(0);
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// 是否记录分配点的调用栈（`--who-allocates` 打开）。
static TRACING: AtomicUsize = AtomicUsize::new(0);
/// 抓栈过程中自身产生的分配不能再次触发抓栈。
static IN_CAPTURE: AtomicUsize = AtomicUsize::new(0);
/// 抓到的调用栈（去重后打印）。
static STACKS: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());

fn record_stack() {
    if TRACING.load(Ordering::Relaxed) == 0 || IN_CAPTURE.swap(1, Ordering::Relaxed) == 1 {
        return;
    }
    let text = std::backtrace::Backtrace::force_capture().to_string();
    // 只保留我们关心的几帧，去掉计数器与分配器自身的帧
    let mut kept: Vec<&str> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        // 只去掉「分配计数器自己」的帧，其余原样保留，便于定位到具体代码行
        if !trimmed.contains("record_stack")
            && !trimmed.contains("Counting")
            && !trimmed.contains("Backtrace")
            && !trimmed.contains("backtrace")
        {
            kept.push(trimmed);
        }
    }
    let key = kept.join("\n");
    if let Ok(mut stacks) = STACKS.lock() {
        if let Some(entry) = stacks.iter_mut().find(|(k, _)| *k == key) {
            entry.1 += 1;
        } else {
            stacks.push((key, 1));
        }
    }
    IN_CAPTURE.store(0, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            record_stack();
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        DEALLOCS.fetch_add(1, Ordering::Relaxed);
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            let live = LIVE.fetch_add(new_size, Ordering::Relaxed) + new_size;
            PEAK.fetch_max(live, Ordering::Relaxed);
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        }
        p
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// 计数器快照。
struct AllocSnapshot {
    allocs: usize,
    deallocs: usize,
    live: usize,
    peak: usize,
}

fn snapshot() -> AllocSnapshot {
    AllocSnapshot {
        allocs: ALLOCS.load(Ordering::Relaxed),
        deallocs: DEALLOCS.load(Ordering::Relaxed),
        live: LIVE.load(Ordering::Relaxed),
        peak: PEAK.load(Ordering::Relaxed),
    }
}

/// 常驻内存（MB，Linux 专有）。
fn rss_mb() -> Option<f64> {
    let text = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: f64 = text.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096.0 / (1024.0 * 1024.0))
}

/// 布局方式。
#[derive(Clone, Copy, PartialEq)]
enum SceneLayout {
    /// 单层平铺（接触数与刚体数同阶，接近「开阔场景」）。
    Scatter,
    /// 14x14 逐层堆叠（接触图密集，接近「深堆/塔」）。
    Stack,
    /// 大量互不相干的独立小堆（每个岛 8 个箱子）——
    /// 专门用来量 island 并行的收益：这些堆永远不会合并成一个岛。
    Clusters,
}

/// 造一个有 `n` 个动态刚体的场景。
/// 是否启用 island 并行求解（`--no-parallel` 关掉，用于配对对比）。
static PARALLEL: AtomicBool = AtomicBool::new(true);

fn make_world(n: usize, layout: SceneLayout, sleep: bool) -> PhysicsWorld {
    let mut world = PhysicsWorld::new();
    world.config.parallel = PARALLEL.load(Ordering::Relaxed);
    world.set_gravity(Vec3::new(0.0, -9.81, 0.0));
    if !sleep {
        world.config.sleep_enabled = false;
    }
    world.add_body(
        BodyBuilder::r#static(Shape::Plane {
            normal: Vec3::new(0.0, 1.0, 0.0),
        })
        .build(),
    );
    let side = 14usize;
    let mut made = 0usize;
    let mut layer = 0usize;
    while made < n {
        for i in 0..side {
            for j in 0..side {
                if made >= n {
                    break;
                }
                if layout == SceneLayout::Clusters {
                    // 每 8 个箱子一摞，摞与摞之间隔 5 个单位 → 上百个独立岛
                    let per = 8usize;
                    let pile = made / per;
                    let layer_in_pile = made % per;
                    world.add_body(
                        BodyBuilder::dynamic(Shape::Box {
                            half_extents: Vec3::new(0.5, 0.5, 0.5),
                        })
                        .position(Vec3::new(
                            (pile % 50) as f32 * 5.0,
                            0.5 + layer_in_pile as f32 * 1.02,
                            (pile / 50) as f32 * 5.0,
                        ))
                        .build(),
                    );
                    made += 1;
                    continue;
                }
                let (x, z) = if layout == SceneLayout::Scatter {
                    // 单层、间距 2.5：彼此不接触，只与地面接触
                    let cols = ((n as f32).sqrt().ceil() as usize).max(1);
                    let (r, c) = (made / cols, made % cols);
                    (r as f32 * 2.5, c as f32 * 2.5)
                } else {
                    let _ = (i, j);
                    (
                        (i as f32 - side as f32 * 0.5) * 1.05,
                        (j as f32 - side as f32 * 0.5) * 1.05,
                    )
                };
                let y = 0.5 + layer as f32 * 1.02;
                world.add_body(
                    BodyBuilder::dynamic(Shape::Box {
                        half_extents: Vec3::new(0.5, 0.5, 0.5),
                    })
                    .position(Vec3::new(x, y, z))
                    .build(),
                );
                made += 1;
            }
        }
        layer += 1;
    }
    world
}

/// 有效速度（默认 1/60）与步数。
/// 按求解器阶段分解每步耗时（引擎内真实计时，不是靠改配置差分）。
///
/// 早期版本用「改 `velocity_iterations` 再差分」来估算各阶段成本，结果对深堆场景
/// 完全不可信：**改迭代次数会改变模拟状态本身**。关掉位置修正后箱子互相陷进去，
/// 穿透越深接触流形越大，窄相反而更贵——2000 体深堆在 `position_iterations = 0`
/// 时每步 38 ms，比完整步（10 速度迭代 + 4 位置迭代）的 34 ms 还慢，
/// 差分出来「每个速度迭代」是负的。
///
/// 现在用 `PhysicsWorld::set_phase_timing(true)`：引擎在同一个真实状态上分段取时钟，
/// 默认关闭时连一次 `Instant::now()` 都不调用（物理结果与开关无关）。
fn print_phases(scales: &[usize], steps: u32, warmup: u32, layout: SceneLayout, sleep: bool) {
    println!(
        "\n阶段分解（{}，引擎内计时）",
        match layout {
            SceneLayout::Scatter => "单层平铺",
            SceneLayout::Stack => "逐层堆叠",
            SceneLayout::Clusters => "独立小堆",
        }
    );
    for &n in scales {
        let mut world = make_world(n, layout, sleep);
        for _ in 0..warmup {
            world.step(1.0 / 60.0);
        }
        world.set_phase_timing(true);
        world.reset_phase_timings();
        for _ in 0..steps {
            world.step(1.0 / 60.0);
        }
        let timings = world.phase_timings();
        world.set_phase_timing(false);
        // 最后一项是整步
        let total: f64 = timings.per_step().last().map(|(_, v)| *v).unwrap_or(0.0);
        let pairs = timings.candidate_pairs as f64 / timings.steps.max(1) as f64;
        let manifolds = timings.manifolds as f64 / timings.steps.max(1) as f64;
        let stats = world.island_stats();
        let par = if world.uses_parallel_solver() {
            format!("并行 {} 线程", world.solver_threads())
        } else {
            "串行".to_string()
        };
        println!(
            "\n{n} 体（每步合计 {:.3} ms · 候选对 {pairs:.0} · 流形 {manifolds:.0} · \
             island {} 个 / 最大 {} 体 / 多体岛 {} · {par}）",
            total / 1000.0,
            stats.islands,
            stats.largest,
            stats.multi_body_islands
        );
        for (name, us) in timings.per_step() {
            if name == "整步" {
                continue;
            }
            let ms = us / 1000.0;
            let pct = if total > 0.0 { us / total * 100.0 } else { 0.0 };
            let bar = "#".repeat((pct / 2.5) as usize);
            println!("  {name:<12} {ms:>8.3} ms  {pct:>5.1}%  {bar}");
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut steps = 300u32;
    let mut scales = vec![100usize, 500, 1000, 2000];
    let mut soak_steps = 20_000u32;
    let mut warmup = 30u32;
    let mut layout = SceneLayout::Scatter;
    let mut sleep = true;
    let mut who_allocates = false;
    let mut phases = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--steps" => {
                steps = args[i + 1].parse().expect("--steps 需要整数");
                i += 2;
            }
            "--bodies" => {
                scales = args[i + 1]
                    .split(',')
                    .map(|s| s.trim().parse().expect("--bodies 需要逗号分隔的整数"))
                    .collect();
                i += 2;
            }
            "--soak" => {
                soak_steps = args[i + 1].parse().expect("--soak 需要整数");
                i += 2;
            }
            "--warmup" => {
                warmup = args[i + 1].parse().expect("--warmup 需要整数");
                i += 2;
            }
            "--layout" => {
                layout = match args[i + 1].as_str() {
                    "scatter" => SceneLayout::Scatter,
                    "stack" => SceneLayout::Stack,
                    "clusters" => SceneLayout::Clusters,
                    other => panic!("--layout 只支持 scatter / stack，收到 {other}"),
                };
                i += 2;
            }
            "--no-sleep" => {
                sleep = false;
                i += 1;
            }
            "--who-allocates" => {
                who_allocates = true;
                i += 1;
            }
            "--phases" => {
                phases = true;
                i += 1;
            }
            "--no-parallel" => {
                PARALLEL.store(false, Ordering::Relaxed);
                i += 1;
            }
            other => panic!("未知参数 {other}"),
        }
    }

    println!("Quench3D 物理基准（release）");
    println!(
        "机器: {} / {} 线程{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::thread::available_parallelism()
            .map(|n| format!(" / {n}"))
            .unwrap_or_default()
    );
    if let Some(rss) = rss_mb() {
        println!("启动常驻内存: {rss:.1} MB");
    }
    println!(
        "\n布局 {}{}",
        if layout == SceneLayout::Scatter {
            "scatter（单层平铺）"
        } else {
            "stack（逐层堆叠）"
        },
        if sleep {
            "，启用睡眠"
        } else {
            "，关闭睡眠（全部保持清醒）"
        }
    );
    if phases {
        print_phases(&scales, steps, warmup, layout, sleep);
        return;
    }

    println!(
        "{:<8} {:>10} {:>11} {:>12} {:>13} {:>11} {:>12}",
        "刚体数", "总耗时", "每步", "步/秒", "体·步/秒", "分配次数", "每步分配"
    );

    for &n in &scales {
        let mut world = make_world(n, layout, sleep);
        // 预热：让求解器缓存、睡眠状态、分配器都进入稳态
        for _ in 0..warmup {
            world.step(1.0 / 60.0);
        }
        let before = snapshot();
        let t0 = Instant::now();
        for _ in 0..steps {
            world.step(1.0 / 60.0);
        }
        let elapsed = t0.elapsed();
        let after = snapshot();

        let ms = elapsed.as_secs_f64() * 1000.0;
        let per_step = ms / steps as f64;
        let steps_per_s = steps as f64 / elapsed.as_secs_f64();
        let body_steps = steps_per_s * n as f64;
        let allocs = after.allocs.saturating_sub(before.allocs);
        let per_step_allocs = allocs as f64 / steps as f64;
        println!(
            "{n:<8} {ms:>9.1}ms {per_step:>10.3}ms {steps_per_s:>11.0} {body_steps:>12.0} {allocs:>11} {per_step_allocs:>11.2}"
        );
        let live_delta = after.live as i64 - before.live as i64;
        let frees = after.deallocs.saturating_sub(before.deallocs);
        println!(
            "         峰值活动内存 {:.2} MB · 活动内存变化 {:+.0} KB · 释放 {frees} 次{}",
            after.peak as f64 / (1024.0 * 1024.0),
            live_delta as f64 / 1024.0,
            if live_delta.abs() > 64 * 1024 {
                "  ← 每步可能都在增长，值得查"
            } else {
                ""
            }
        );
    }

    // 分配点溯源：预热后单跑一步，把所有分配点的调用栈抓出来
    if who_allocates {
        println!("\n分配点溯源：预热后单跑一步，抓取所有堆分配");
        let mut world = make_world(scales[0], layout, sleep);
        for _ in 0..warmup.max(60) {
            world.step(1.0 / 60.0);
        }
        STACKS.lock().expect("锁").clear();
        TRACING.store(1, Ordering::Relaxed);
        let before = snapshot().allocs;
        world.step(1.0 / 60.0);
        TRACING.store(0, Ordering::Relaxed);
        let after = snapshot().allocs;
        println!("  这一步共 {} 次分配", after - before);
        let stacks = STACKS.lock().expect("锁");
        if stacks.is_empty() {
            println!("  没有任何分配 → 稳态零分配 ✓");
        }
        for (stack, count) in stacks.iter() {
            println!("  —— {count} 次 ——");
            for line in stack.lines().take(8) {
                println!("      {line}");
            }
        }
        drop(stacks);
    }

    // 整岛休眠的实际收益：同一场景开着睡眠跑，量「入睡需要多少步」和
    // 「全部入睡之后每步还要多少时间」——这才是稳态，不是把结算窗口一起平均。
    println!("\n整岛休眠：500 体堆叠 × {soak_steps} 步（启用睡眠）");
    {
        let mut world = make_world(500, SceneLayout::Stack, true);
        let mut settle_step = 0usize;
        let t_settle = Instant::now();
        for step in 0..soak_steps as usize {
            world.step(1.0 / 60.0);
            // island_stats() 只是读一个结构体，逐步检查也不会有可测开销
            if world.island_stats().islands == 0 {
                settle_step = step + 1;
                break;
            }
        }
        let first_steps_cost = t_settle.elapsed().as_secs_f64();
        if settle_step == 0 {
            settle_step = soak_steps as usize;
        }
        let asleep = world.bodies().filter(|(_, b)| b.is_sleeping()).count();
        // 睡着之后再跑一大段，量纯稳态
        let tail = 10_000usize;
        let t0 = Instant::now();
        for _ in 0..tail {
            world.step(1.0 / 60.0);
        }
        let tail_ms = t0.elapsed().as_secs_f64() * 1e3 / tail as f64;
        println!(
            "  第 {settle_step} 步整场入睡（{:.3} s 模拟时间，耗时 {first_steps_cost:.4} s）· \
             入睡后 {asleep} 个刚体 · 稳态 {tail_ms:.4} ms/步（{:.0} 步/秒）",
            settle_step as f64 / 60.0,
            1.0 / (tail_ms / 1e3),
        );
    }

    // 长跑稳定性：小场景跑很多步，看有没有 NaN、速度爆炸或内存增长
    println!("\n长跑稳定性：500 体 × {soak_steps} 步（关闭睡眠，保持一直在算）");
    let mut world = make_world(500, SceneLayout::Stack, false);
    let before = snapshot();
    let t0 = Instant::now();
    let mut worst_speed = 0.0f32;
    let mut nan_seen = 0usize;
    for _ in 0..soak_steps {
        world.step(1.0 / 60.0);
    }
    let elapsed = t0.elapsed();
    for (_, body) in world.bodies() {
        let p = body.position();
        let v = body.linear_velocity;
        if !p.is_finite() || !v.is_finite() {
            nan_seen += 1;
        }
        worst_speed = worst_speed.max(v.length());
    }
    let after = snapshot();
    println!(
        "  耗时 {:.2}s（{:.0} 步/秒）· NaN {nan_seen} · 最大速度 {worst_speed:.1} m/s · 分配 {} 次（每步 {:.2}）",
        elapsed.as_secs_f64(),
        soak_steps as f64 / elapsed.as_secs_f64(),
        after.allocs.saturating_sub(before.allocs),
        (after.allocs - before.allocs) as f64 / soak_steps as f64
    );
    if let Some(rss) = rss_mb() {
        println!("  常驻内存: {rss:.1} MB");
    }
    assert_eq!(nan_seen, 0, "长跑出现 NaN/Inf");
    assert!(worst_speed < 1000.0, "速度异常：{worst_speed}");
    println!("\n完成。");
}
