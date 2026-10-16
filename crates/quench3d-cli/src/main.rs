//! `quench3d` 命令行入口。
//!
//! 设计原则：**给 agent 用的路径必须是确定性的**——任何命令都支持 `--json`，
//! 输出单行 JSON 信封，退出码固定（见 `report` 模块），诊断带 JSON 路径。

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use quench3d_cli::llm::LlmClient;
use quench3d_cli::report::{self, Envelope, ErrorInfo, EXIT_INVALID, EXIT_OK, EXIT_RUNTIME};
use quench3d_cli::runner;
use quench3d_cli::scene_doc::{SceneDoc, Severity};
use quench3d_cli::schema;
use serde_json::{json, Value};

/// 需要跟一个值的参数。
/// 渲染单边像素上限（挡住 `--width 99999` 这类输入）。
const MAX_RENDER_SIDE: u32 = 8192;
/// 渲染总像素上限（约 4096×4096）。
const MAX_RENDER_PIXELS: u32 = 16_777_216;
/// 单次录制允许的最大帧数（挡住 `--frames 999999999` 这类输入）。
const MAX_MOVIE_FRAMES: u32 = 100_000;
/// 每帧最多推进多少物理步。
const MAX_STEPS_PER_FRAME: u32 = 1_000;
/// 最多允许丢弃多少预热帧。
const MAX_MOVIE_WARMUP: u32 = 10_000;

/// 允许的最大固定时间步（1 秒；再大就不是「仿真」而是跳变了）。
const MAX_FIXED_DT: f32 = 1.0;

const VALUE_FLAGS: &[&str] = &[
    "--camera",
    "--asset-path",
    "--out",
    "--width",
    "--height",
    "--steps",
    "--dt",
    "--render",
    "--base-url",
    "--model",
    "--scene",
    "--device",
    "--max-rounds",
    "--repair-rounds",
    "--trace",
    "--trace-every",
    "--bodies",
    "--warmup",
    "--layout",
    "--frames",
    "--fps",
    "--frames-dir",
    "--steps-per-frame",
    "--encoder",
    "--ray",
    "--overlap",
    "--mask",
    "--max",
];

#[derive(Debug)]
struct Args {
    command: String,
    positional: Vec<String>,
    flags: BTreeMap<String, String>,
    json: bool,
}

impl Args {
    fn parse(raw: &[String]) -> Result<Self, String> {
        let mut positional = Vec::new();
        let mut flags = BTreeMap::new();
        let mut json = false;
        let mut command = String::new();
        let mut i = 0usize;
        while i < raw.len() {
            let token = &raw[i];
            if token == "--json" {
                json = true;
                i += 1;
                continue;
            }
            if let Some(rest) = token.strip_prefix("--") {
                if let Some((key, value)) = rest.split_once('=') {
                    flags.insert(format!("--{key}"), value.to_string());
                    i += 1;
                    continue;
                }
                let key = format!("--{rest}");
                if VALUE_FLAGS.contains(&key.as_str()) {
                    let value = raw
                        .get(i + 1)
                        .ok_or_else(|| format!("{key} 缺少取值"))?
                        .clone();
                    flags.insert(key, value);
                    i += 2;
                } else {
                    flags.insert(key, "true".into());
                    i += 1;
                }
                continue;
            }
            if command.is_empty() && !token.starts_with('-') {
                command = token.clone();
            } else {
                positional.push(token.clone());
            }
            i += 1;
        }
        Ok(Self {
            command,
            positional,
            flags,
            json,
        })
    }

    fn flag_str(&self, key: &str) -> Option<&str> {
        self.flags.get(key).map(|s| s.as_str())
    }

    fn flag_parse<T: std::str::FromStr>(&self, key: &str) -> Result<Option<T>, String> {
        match self.flags.get(key) {
            None => Ok(None),
            Some(v) => v
                .parse::<T>()
                .map(Some)
                .map_err(|_| format!("{key} 的值 \"{v}\" 无法解析")),
        }
    }

    fn has(&self, key: &str) -> bool {
        self.flags.contains_key(key)
    }
}

/// 进程启动时刻（`bench` 用它报告「启动到开始测量」的耗时）。
static PROCESS_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn main() -> ExitCode {
    let _ = PROCESS_START.set(Instant::now());
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let args = match Args::parse(&raw) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("参数错误: {e}");
            return ExitCode::from(EXIT_INVALID as u8);
        }
    };
    // 资源搜索路径要在任何场景装载之前登记（命令内部可能自己再 from_file 一次）
    quench3d_cli::scene_doc::set_cli_asset_paths(asset_paths_from_args(&args));
    let code = dispatch(&args);
    ExitCode::from(code as u8)
}

fn dispatch(args: &Args) -> i32 {
    match args.command.as_str() {
        "" | "help" | "--help" | "-h" => {
            print_help();
            EXIT_OK
        }
        "version" | "--version" | "-V" => {
            println!("quench3d {}", schema::VERSION);
            EXIT_OK
        }
        "schema" => {
            let started = Instant::now();
            let value = if args.has("--json-schema") {
                schema::json_schema()
            } else {
                schema::schema()
            };
            finish(args, "schema", Ok(value), started, |v| {
                println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
            })
        }
        "bench" => {
            let started = Instant::now();
            let bodies: Vec<usize> = match args.flag_str("--bodies") {
                Some(text) => {
                    let mut parsed = Vec::new();
                    for part in text.split(',') {
                        match part.trim().parse::<usize>() {
                            Ok(n) if n > 0 => parsed.push(n),
                            _ => {
                                return fail(
                                    args,
                                    "bench",
                                    ErrorInfo::new(
                                        "invalid",
                                        format!("--bodies 里的 `{part}` 不是正整数"),
                                    )
                                    .hint("示例：--bodies 100,500,1000,2000"),
                                    started,
                                );
                            }
                        }
                    }
                    parsed
                }
                None => vec![100, 500, 1000],
            };
            let mut options = quench3d_cli::bench::BenchOptions {
                bodies,
                steps: args
                    .flag_parse::<u32>("--steps")
                    .ok()
                    .flatten()
                    .unwrap_or(300),
                warmup: args
                    .flag_parse::<u32>("--warmup")
                    .ok()
                    .flatten()
                    .unwrap_or(30),
                layout: match args.flag_str("--layout") {
                    Some(text) => match quench3d_cli::bench::Layout::parse(text) {
                        Some(layout) => layout,
                        None => {
                            return fail(
                                args,
                                "bench",
                                ErrorInfo::new(
                                    "invalid",
                                    format!("--layout 只支持 scatter / stack，收到 `{text}`"),
                                ),
                                started,
                            );
                        }
                    },
                    None => quench3d_cli::bench::Layout::Scatter,
                },
                sleep: !args.has("--no-sleep"),
                render: !args.has("--no-render"),
                startup_ms: PROCESS_START
                    .get()
                    .map(|t| t.elapsed().as_secs_f64() * 1000.0)
                    .unwrap_or(0.0),
            };
            options.bodies.sort_unstable();
            options.bodies.dedup();
            let report = quench3d_cli::bench::run(&options);
            let human = quench3d_cli::bench::human(&report)
                + &report
                    .notes
                    .iter()
                    .map(|n| format!("备注      : {n}\n"))
                    .collect::<String>();
            let value = serde_json::to_value(&report).unwrap_or(Value::Null);
            finish(args, "bench", Ok(value), started, |_| println!("{human}"))
        }
        "doctor" => {
            let started = Instant::now();
            let report = runner::run_doctor();
            let value = serde_json::to_value(&report).unwrap_or(Value::Null);
            let human = format!(
                "平台      : {} / {}（逻辑核心 {}）\nGPU 可用  : {}\n适配器    : {}\n后端      : {}（{}）\n软件光栅  : {}\n{}",
                report.os,
                report.arch,
                report.cpu_threads,
                if report.gpu_available { "是" } else { "否" },
                if report.adapter.is_empty() { "-" } else { &report.adapter },
                report.backend,
                report.device_type,
                if report.software_rasterizer { "是（渲染较慢）" } else { "否" },
                report
                    .notes
                    .iter()
                    .map(|n| format!("提示      : {n}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            finish(args, "doctor", Ok(value), started, |_| println!("{human}"))
        }
        "info" => {
            let started = Instant::now();
            let path = match args.positional.first() {
                Some(p) => PathBuf::from(p),
                None => {
                    return missing(args, "info", "需要模型文件路径", "quench3d info chair.glb")
                }
            };
            match runner::run_info(&path) {
                Ok(info) => {
                    let value = serde_json::to_value(&info).unwrap_or(Value::Null);
                    let human = format!(
                        "文件      : {}\n格式      : {}\n子网格    : {}（三角形 {:?}）\n材质/贴图 : {} / {}\n顶点      : {}\n三角形    : {}\n包围盒    : min {:?}  max {:?}\n尺寸      : {:?}",
                        info.path,
                        info.format,
                        info.meshes,
                        info.mesh_triangles,
                        info.materials,
                        info.images,
                        info.vertices,
                        info.triangles,
                        info.bounds_min,
                        info.bounds_max,
                        info.bounds_size
                    );
                    finish(args, "info", Ok(value), started, |_| println!("{human}"))
                }
                Err(e) => fail(args, "info", ErrorInfo::from_run(&e), started),
            }
        }
        "example" => {
            let started = Instant::now();
            match args.flag_str("--out") {
                Some(path) => {
                    if let Err(e) = std::fs::write(path, schema::EXAMPLE_SCENE) {
                        return fail(
                            args,
                            "example",
                            ErrorInfo::new("io", format!("写入 {path} 失败: {e}")),
                            started,
                        );
                    }
                    let value = json!({ "output": path, "bytes": schema::EXAMPLE_SCENE.len() });
                    finish(args, "example", Ok(value), started, |_| {
                        println!("已写入示例场景 {path}\n直接运行：quench3d render {path} --json")
                    })
                }
                None => {
                    print!("{}", schema::EXAMPLE_SCENE);
                    EXIT_OK
                }
            }
        }
        "validate" => {
            let started = Instant::now();
            let Some(path) = args.positional.first() else {
                return missing(
                    args,
                    "validate",
                    "需要场景文件路径",
                    "quench3d validate scene.json",
                );
            };
            match SceneDoc::from_file(path) {
                Ok(mut doc) => {
                    apply_asset_paths(args, &mut doc);
                    let diags = doc.validate();
                    let errors = diags
                        .iter()
                        .filter(|d| d.severity == Severity::Error)
                        .count();
                    let warnings = diags.len() - errors;
                    let value = json!({
                        "scene": path,
                        "valid": errors == 0,
                        "errors": errors,
                        "warnings": warnings,
                        "diagnostics": diags,
                    });
                    if errors > 0 {
                        if args.json {
                            let env = Envelope::ok(
                                "validate",
                                value,
                                started.elapsed().as_millis() as u64,
                            );
                            println!("{}", env.to_json());
                        } else {
                            println!("场景不合法：{errors} 个错误、{warnings} 条警告");
                            for d in &diags {
                                let tag = match d.severity {
                                    Severity::Error => "错误",
                                    Severity::Warning => "警告",
                                };
                                println!("  [{tag}] {}: {}", d.path, d.message);
                                if let Some(h) = &d.hint {
                                    println!("         建议: {h}");
                                }
                            }
                        }
                        // 校验失败用 2，agent 据此修正输入
                        return EXIT_INVALID;
                    }
                    finish(args, "validate", Ok(value), started, |_| {
                        println!("场景合法（{warnings} 条警告）");
                        for d in &diags {
                            println!("  [警告] {}: {}", d.path, d.message);
                        }
                    })
                }
                Err(e) => fail(args, "validate", report::doc_error_info(&e), started),
            }
        }
        "render" => {
            let started = Instant::now();
            let Some(path) = args.positional.first() else {
                return missing(
                    args,
                    "render",
                    "需要场景文件路径",
                    "quench3d render scene.json",
                );
            };
            let mut doc = match load_scene(args, path, started) {
                Ok(d) => d,
                Err(code) => return code,
            };
            if let Err(code) = apply_overrides(args, &mut doc, started) {
                return code;
            }
            if args.has("--no-simulate") {
                doc.simulate.steps = 0;
            }
            let out = args.flag_str("--out").map(PathBuf::from);
            match runner::run_render(&doc, out.as_deref(), output_policy(args)) {
                Ok(report) if report.dry_run => {
                    let value = serde_json::to_value(&report).unwrap_or(Value::Null);
                    let human = format!(
                        "（dry-run）本会渲染 : {}（{}×{}）\n物理        : {} 步{}\n耗时        : {} ms",
                        report.output,
                        report.width,
                        report.height,
                        report.simulate.as_ref().map(|s| s.steps).unwrap_or(0),
                        report
                            .simulate
                            .as_ref()
                            .map(|s| format!(
                                "，{} 个刚体，最大速度 {:.2} m/s，NaN {}",
                                s.bodies, s.max_speed, s.nan_bodies
                            ))
                            .unwrap_or_default(),
                        report.elapsed_ms
                    );
                    finish(args, "render", Ok(value), started, |_| println!("{human}"))
                }
                Ok(report) => {
                    let value = serde_json::to_value(&report).unwrap_or(Value::Null);
                    let human = format!(
                        "已渲染    : {}（{}×{}，{} KB）\n绘制/实例 : {} / {}\n三角形    : {}\n阴影实例  : {}\n场景物体  : {}\n相机位置  : {:?}\n耗时      : {} ms{}",
                        report.output,
                        report.width,
                        report.height,
                        report.bytes / 1024,
                        report.draw_calls,
                        report.instances,
                        report.triangles,
                        report.shadow_instances,
                        report.objects,
                        report.camera_position,
                        report.elapsed_ms,
                        report
                            .simulate
                            .as_ref()
                            .map(|s| format!(
                                "\n物理      : {} 步，{} 个刚体，最大速度 {:.2} m/s，NaN {}",
                                s.steps, s.bodies, s.max_speed, s.nan_bodies
                            ))
                            .unwrap_or_default()
                    );
                    finish(args, "render", Ok(value), started, |_| println!("{human}"))
                }
                Err(e) => fail(args, "render", ErrorInfo::from_run(&e), started),
            }
        }
        "movie" => {
            let started = Instant::now();
            let Some(path) = args.positional.first() else {
                return missing(
                    args,
                    "movie",
                    "需要场景文件路径",
                    "quench3d movie scene.json --frames 120 --frames-dir frames",
                );
            };
            let mut doc = match load_scene(args, path, started) {
                Ok(d) => d,
                Err(code) => return code,
            };
            if let Err(code) = apply_overrides(args, &mut doc, started) {
                return code;
            }
            let opts = match movie_options(args, &doc, started) {
                Ok(o) => o,
                Err(code) => return code,
            };
            match runner::run_movie(&doc, &opts) {
                Ok(report) if report.dry_run => {
                    let value = serde_json::to_value(&report).unwrap_or(Value::Null);
                    let human = format!(
                        "（dry-run）本会写入 : {}\ntrace       : {}\n帧数/总步数 : {} 帧 / {} 步（{} 步/帧 × {} s）\n编码        : {}（{}×{}，{} fps）\n耗时        : {} ms",
                        report
                            .would_write
                            .first()
                            .cloned()
                            .unwrap_or_else(|| report.frames_dir.clone()),
                        report.trace,
                        report.frames,
                        report.total_steps,
                        report.steps_per_frame,
                        report.dt,
                        report.encoder,
                        report.width,
                        report.height,
                        report.fps,
                        report.elapsed_ms
                    );
                    finish(args, "movie", Ok(value), started, |_| println!("{human}"))
                }
                Ok(report) => {
                    let value = serde_json::to_value(&report).unwrap_or(Value::Null);
                    // 单文件编码器（gif/apng）没有「帧目录」，报告口径要跟着变，
                    // 否则用户会去一个根本不存在的目录里找帧。
                    let target = if report.output.is_empty() {
                        format!(
                            "帧目录    : {}（{} 帧，{}×{}，{} KB{}）",
                            report.frames_dir,
                            report.frames,
                            report.width,
                            report.height,
                            report.bytes / 1024,
                            if report.skipped > 0 {
                                format!("，跳过已存在 {} 帧", report.skipped)
                            } else {
                                String::new()
                            },
                        )
                    } else {
                        format!(
                            "输出文件  : {}（{} 编码，{} 帧，{}×{}，{} KB，{}）",
                            report.output,
                            report.encoder,
                            report.frames,
                            report.width,
                            report.height,
                            report.bytes / 1024,
                            if report.encoder == "gif" {
                                "256 色，可直接播放"
                            } else {
                                "无损，可直接播放"
                            },
                        )
                    };
                    let mut tail = if report.ffmpeg.is_empty() {
                        "内置编码已产出可直接播放的文件；要 mp4 再自行转码".to_string()
                    } else {
                        report.ffmpeg.clone()
                    };
                    for note in &report.notes {
                        tail.push_str(&format!("\n注意      : {note}"));
                    }
                    let human = format!(
                        "{target}\n轨迹      : {}\n时长/帧率 : {:.2} s @ {:.2} fps（{} 步/帧 × {} s = {} 步）\n预热帧    : {}\n像素摘要  : {}\n场景物体  : {}\n相机位置  : {:?}\n耗时      : {} ms\n下一步    : {}",
                        report.trace,
                        report.duration,
                        report.fps,
                        report.steps_per_frame,
                        report.dt,
                        report.total_steps,
                        report.warmup_frames,
                        report.digest,
                        report.objects,
                        report.camera_position,
                        report.elapsed_ms,
                        tail,
                    );
                    finish(args, "movie", Ok(value), started, |_| println!("{human}"))
                }
                Err(e) => fail(args, "movie", ErrorInfo::from_run(&e), started),
            }
        }
        "simulate" => {
            let started = Instant::now();
            let Some(path) = args.positional.first() else {
                return missing(
                    args,
                    "simulate",
                    "需要场景文件路径",
                    "quench3d simulate scene.json --steps 600",
                );
            };
            let mut doc = match load_scene(args, path, started) {
                Ok(d) => d,
                Err(code) => return code,
            };
            if let Err(code) = apply_overrides(args, &mut doc, started) {
                return code;
            }
            // 轨迹采样：--trace 名字（逗号分隔）、--trace-all、--trace-every
            let trace = trace_targets(args, &doc);
            let every = args
                .flag_parse::<u32>("--trace-every")
                .ok()
                .flatten()
                .unwrap_or(1)
                .max(1);
            // `--no-clobber` / `--dry-run` 只在"顺带渲染"这条支路上有意义：
            // simulate 自己一个字节都不写。悄悄忽略参数是这个项目最不想要的失败模式。
            let policy = output_policy(args);
            if (policy.no_clobber || policy.dry_run) && args.flag_str("--render").is_none() {
                return fail(
                    args,
                    "simulate",
                    ErrorInfo::new(
                        "invalid",
                        "simulate 本身不写任何文件；--dry-run / --no-clobber 只在同时传 --render 时有意义",
                    )
                    .hint("只想校验场景请用 quench3d validate；想渲染请加 --render <路径>"),
                    started,
                );
            }
            match runner::run_simulate_traced(&doc, &trace, every) {
                Ok((report, trajectories)) => {
                    let mut value = serde_json::to_value(&report).unwrap_or(Value::Null);
                    if !trajectories.is_empty() {
                        value["trajectories"] =
                            serde_json::to_value(&trajectories).unwrap_or(Value::Null);
                    }
                    let mut human = format!(
                        "步数      : {}（dt = {:.5}s，重力 {:?}）\n刚体      : {}（动态 {}，休眠 {}）\n接触/关节 : {} / {}\n最大速度  : {:.3} m/s\n最低点 y  : {:.3}\nNaN 刚体  : {}\n",
                        report.steps,
                        report.dt,
                        report.gravity,
                        report.bodies,
                        report.dynamic,
                        report.sleeping,
                        report.contacts,
                        report.joints,
                        report.max_speed,
                        report.lowest_y,
                        report.nan_bodies
                    );
                    for s in &report.states {
                        human.push_str(&format!(
                            "  {:<14} {:<9} 位置 ({:>7.2},{:>7.2},{:>7.2}) 速度 {:>6.2} 位移 {:>6.2}{}\n",
                            s.name,
                            s.kind,
                            s.position[0],
                            s.position[1],
                            s.position[2],
                            s.speed,
                            s.displacement,
                            if s.asleep { " 休眠" } else { "" }
                        ));
                    }
                    for w in &report.warnings {
                        human.push_str(&format!("警告      : {w}\n"));
                    }
                    for t in &trajectories {
                        human.push_str(&trajectory_human(t));
                    }
                    // 可选同时渲染
                    if let Some(out) = args.flag_str("--render") {
                        match runner::run_render(&doc, Some(std::path::Path::new(out)), policy) {
                            Ok(r) if r.dry_run => {
                                human.push_str(&format!("（dry-run）本会渲染 : {}\n", r.output))
                            }
                            Ok(r) => human.push_str(&format!(
                                "已渲染    : {}（{} KB）\n",
                                r.output,
                                r.bytes / 1024
                            )),
                            Err(e) => human.push_str(&format!("渲染失败  : {e}\n")),
                        }
                    }
                    finish(args, "simulate", Ok(value), started, |_| print!("{human}"))
                }
                Err(e) => fail(args, "simulate", ErrorInfo::from_run(&e), started),
            }
        }
        "query" => {
            let started = Instant::now();
            let Some(path) = args.positional.first() else {
                return missing(
                    args,
                    "query",
                    "需要场景文件路径",
                    "quench3d query scene.json --ray 0,5,0,0,-1,0",
                );
            };
            let doc = match load_scene(args, path, started) {
                Ok(d) => d,
                Err(code) => return code,
            };
            let kind = match parse_query_kind(args) {
                Ok(k) => k,
                Err(msg) => {
                    return fail(
                        args,
                        "query",
                        ErrorInfo::new("invalid", msg).hint(
                            "--ray 形如 0,5,0,0,-1,0（起点+方向）；--overlap 形如 box:1,2,1@0,1,0 或 sphere:0.5@0,1,0",
                        ),
                        started,
                    )
                }
            };
            // query 一个字节都不写：写文件相关的开关在这里没有意义，
            // 悄悄忽略它们是这个项目最不想要的失败模式。
            if args.has("--out") || args.has("--no-clobber") || args.has("--dry-run") {
                return fail(
                    args,
                    "query",
                    ErrorInfo::new(
                        "invalid",
                        "query 只读不写：--out / --no-clobber / --dry-run 在它这里没有意义",
                    )
                    .hint("要落盘结果请用 shell 重定向（stdout 是单行 JSON）"),
                    started,
                );
            }
            let all = args.has("--all");
            if all && matches!(kind, runner::QueryKind::Overlap { .. }) {
                return fail(
                    args,
                    "query",
                    ErrorInfo::new(
                        "invalid",
                        "--all 只对 --ray 有意义：重叠查询本来就返回全部命中",
                    )
                    .hint("去掉 --all，或改用 --ray"),
                    started,
                );
            }
            let mask = match args.flag_parse::<u32>("--mask") {
                Ok(Some(m)) => m,
                Ok(None) => u32::MAX,
                Err(e) => {
                    return fail(
                        args,
                        "query",
                        ErrorInfo::new(
                            "invalid",
                            format!("--mask 不是合法的 32 位无符号整数：{e}"),
                        )
                        .hint("层掩码，例如 --mask 5 表示第 0 层与第 2 层"),
                        started,
                    )
                }
            };
            let max_distance = match args.flag_parse::<f32>("--max") {
                Ok(Some(m)) if m.is_finite() && m > 0.0 => m,
                Ok(Some(m)) => {
                    return fail(
                        args,
                        "query",
                        ErrorInfo::new("invalid", format!("--max 必须是正数：{m}")),
                        started,
                    )
                }
                Ok(None) => runner::DEFAULT_QUERY_MAX,
                Err(e) => {
                    return fail(
                        args,
                        "query",
                        ErrorInfo::new("invalid", format!("--max 不是合法数字：{e}")),
                        started,
                    )
                }
            };
            let steps = match args.flag_parse::<u32>("--steps") {
                Ok(Some(n)) => n,
                Ok(None) => 0,
                Err(e) => {
                    return fail(
                        args,
                        "query",
                        ErrorInfo::new("invalid", format!("--steps 不是合法步数：{e}"))
                            .hint("--steps N：先跑 N 步物理再查询（缺省 0，按场景初始状态查）"),
                        started,
                    )
                }
            };
            match runner::run_query(
                &doc,
                &kind,
                runner::QueryOptions {
                    mask,
                    sensors: !args.has("--no-sensors"),
                    max_distance,
                    all,
                    steps,
                    use_index: args.has("--index"),
                },
            ) {
                Ok(report) => {
                    let value = serde_json::to_value(&report).unwrap_or(Value::Null);
                    let human = query_human(&report);
                    finish(args, "query", Ok(value), started, |_| print!("{human}"))
                }
                Err(e) => fail(args, "query", ErrorInfo::from_run(&e), started),
            }
        }
        "ask" => cmd_ask(args),
        "chat" => cmd_chat(args),
        "mcp" => {
            if let Err(e) = quench3d_cli::mcp::serve_stdio() {
                eprintln!("MCP 服务器错误: {e}");
                return EXIT_RUNTIME;
            }
            EXIT_OK
        }
        other => {
            eprintln!("未知命令: {other}\n");
            print_help();
            EXIT_INVALID
        }
    }
}

/// `--asset-path`：一次运行用的额外资源搜索路径。
///
/// 多个目录用 `:` 分隔（Windows 用 `;`），与 `PATH` 的写法一致——`Args` 的旗标表是
/// "一个旗标一个值"，所以这里不复用重复旗标。空项忽略；相对路径以场景目录为基准
/// （与 `assets.search_paths` 同一套规则），顺序排在场景声明之后。
fn asset_paths_from_args(args: &Args) -> Vec<std::path::PathBuf> {
    let Some(list) = args.flag_str("--asset-path") else {
        return Vec::new();
    };
    let sep = if cfg!(windows) { ';' } else { ':' };
    list.split(sep)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .collect()
}

/// 把 `--asset-path` 记进文档与进程级（进程级那份保证"命令内部又 from_file 一次"的
/// 路径也看得到旗标）。
fn apply_asset_paths(args: &Args, doc: &mut SceneDoc) {
    doc.extra_asset_paths = asset_paths_from_args(args);
}

fn load_scene(args: &Args, path: &str, started: Instant) -> Result<SceneDoc, i32> {
    match SceneDoc::from_file(path) {
        Ok(doc) => {
            let diags = doc.validate();
            let errors: Vec<_> = diags
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .cloned()
                .collect();
            if errors.is_empty() {
                // `--camera <名字>`：把命名相机搬到 `camera` 段上——下游（apply_camera、
                // 渲染报告）只看 `camera`，所以这里换一次就够，不必改渲染路径。
                let mut doc = doc;
                apply_asset_paths(args, &mut doc);
                if let Some(name) = args.flag_str("--camera") {
                    if let Err(message) = doc.select_camera(name) {
                        return Err(fail(
                            args,
                            &args.command,
                            ErrorInfo::new("unknown_camera", message).hint(
                                "名字见场景的 `cameras` 段；不写 `--camera` 就用 `camera` 段那台",
                            ),
                            started,
                        ));
                    }
                }
                Ok(doc)
            } else {
                Err(fail(
                    args,
                    "validate",
                    ErrorInfo::new(
                        "invalid",
                        format!("场景校验失败（{} 个错误）", errors.len()),
                    )
                    .hint("先跑 quench3d validate 看全部诊断")
                    .with_diagnostics(diags),
                    started,
                ))
            }
        }
        Err(e) => Err(fail(args, "validate", report::doc_error_info(&e), started)),
    }
}

fn apply_overrides(args: &Args, doc: &mut SceneDoc, started: Instant) -> Result<(), i32> {
    macro_rules! set {
        ($flag:literal, $field:expr) => {
            match args.flag_parse::<u32>($flag) {
                Ok(Some(v)) => $field = v,
                Ok(None) => {}
                Err(e) => {
                    return Err(fail(args, "args", ErrorInfo::new("invalid", e), started));
                }
            }
        };
    }
    set!("--width", doc.render.width);
    set!("--height", doc.render.height);
    set!("--steps", doc.simulate.steps);
    // 分辨率上限：挡住 `--width 99999` 这类会直接把显存/内存打满的输入
    for (flag, value) in [
        ("--width", doc.render.width),
        ("--height", doc.render.height),
    ] {
        if value == 0 || value > MAX_RENDER_SIDE {
            return Err(fail(
                args,
                "args",
                ErrorInfo::new(
                    "invalid",
                    format!("{flag} 必须在 1..={MAX_RENDER_SIDE} 之间，当前为 {value}"),
                )
                .hint("正常范围：320~1920，软件光栅超过约 960×640 会明显变慢"),
                started,
            ));
        }
    }
    if doc.render.width.saturating_mul(doc.render.height) > MAX_RENDER_PIXELS {
        return Err(fail(
            args,
            "args",
            ErrorInfo::new(
                "invalid",
                format!(
                    "分辨率 {}×{} 超过 {} 像素上限",
                    doc.render.width, doc.render.height, MAX_RENDER_PIXELS
                ),
            ),
            started,
        ));
    }
    if let Some(dt) = args
        .flag_parse::<f32>("--dt")
        .map_err(|e| fail(args, "args", ErrorInfo::new("invalid", e), started))?
    {
        // 物理内部会把非法 dt 当作「不推进」，但那样命令会成功返回一个什么都没算的报告，
        // 容易让 agent 误以为仿真跑了——所以这里直接拒绝。
        if !(dt.is_finite() && dt > 0.0 && dt <= MAX_FIXED_DT) {
            return Err(fail(
                args,
                "args",
                ErrorInfo::new(
                    "invalid",
                    format!("--dt 必须是 (0, {MAX_FIXED_DT}] 之间的有限数，当前为 {dt}"),
                )
                .hint("常用值：1/60 ≈ 0.0166667；不要传 0、负数、NaN 或 Infinity"),
                started,
            ));
        }
        doc.simulate.dt = dt;
    }
    if let Some(out) = args.flag_str("--out") {
        doc.render.output = PathBuf::from(out);
    }
    Ok(())
}

/// 组装录制参数：所有取值都校验，不做静默兜底。
///
/// 与 `simulate` 的步数控制刻意分开——本命令的帧数由 `--frames × --steps-per-frame`
/// 决定，所以继续传 `--steps`/`--trace` 这类 simulate 专用参数会**直接报错**而不是被默默忽略。
fn movie_options(
    args: &Args,
    doc: &SceneDoc,
    started: Instant,
) -> Result<runner::MovieOptions, i32> {
    for flag in [
        "--steps",
        "--no-simulate",
        "--render",
        "--trace",
        "--trace-all",
        "--trace-every",
    ] {
        if args.has(flag) {
            return Err(fail(
                args,
                "movie",
                ErrorInfo::new("invalid", format!("movie 不支持 {flag}"))
                    .hint("帧数与时长用 --frames / --steps-per-frame 控制，帧序列写在 --frames-dir 下；simulate 专用参数请用 simulate"),
                started,
            ));
        }
    }

    let frames = args
        .flag_parse::<u32>("--frames")
        .map_err(|e| fail(args, "movie", ErrorInfo::new("invalid", e), started))?
        .unwrap_or(60);
    if frames == 0 || frames > MAX_MOVIE_FRAMES {
        return Err(fail(
            args,
            "movie",
            ErrorInfo::new(
                "invalid",
                format!("--frames 需要在 1..={MAX_MOVIE_FRAMES} 之间，当前为 {frames}"),
            ),
            started,
        ));
    }

    let steps_per_frame = args
        .flag_parse::<u32>("--steps-per-frame")
        .map_err(|e| fail(args, "movie", ErrorInfo::new("invalid", e), started))?
        .unwrap_or(1);
    if steps_per_frame == 0 || steps_per_frame > MAX_STEPS_PER_FRAME {
        return Err(fail(
            args,
            "movie",
            ErrorInfo::new(
                "invalid",
                format!("--steps-per-frame 需要在 1..={MAX_STEPS_PER_FRAME} 之间，当前为 {steps_per_frame}"),
            ),
            started,
        ));
    }

    let warmup_frames = args
        .flag_parse::<u32>("--warmup")
        .map_err(|e| fail(args, "movie", ErrorInfo::new("invalid", e), started))?
        .unwrap_or(0);
    if warmup_frames > MAX_MOVIE_WARMUP {
        return Err(fail(
            args,
            "movie",
            ErrorInfo::new(
                "invalid",
                format!("--warmup 不能超过 {MAX_MOVIE_WARMUP}，当前为 {warmup_frames}"),
            ),
            started,
        ));
    }

    let dt = doc.simulate.dt;
    if !(dt.is_finite() && dt > 0.0 && dt <= MAX_FIXED_DT) {
        return Err(fail(
            args,
            "movie",
            ErrorInfo::new(
                "invalid",
                format!("simulate.dt 必须是 (0, {MAX_FIXED_DT}] 之间的有限数，当前为 {dt}"),
            ),
            started,
        ));
    }

    let fps = match args
        .flag_parse::<f32>("--fps")
        .map_err(|e| fail(args, "movie", ErrorInfo::new("invalid", e), started))?
    {
        Some(f) => f,
        // 缺省 = 等价帧率：视频时间与仿真时间一致（1 步/帧、dt=1/60 → 60 fps）。
        None => 1.0 / (dt * steps_per_frame as f32),
    };
    if !(fps.is_finite() && fps > 0.1 && fps <= 1000.0) {
        return Err(fail(
            args,
            "movie",
            ErrorInfo::new(
                "invalid",
                format!("--fps 需要在 0.1..=1000 之间，当前为 {fps}"),
            )
            .hint("缺省是等价帧率 1/(dt×steps_per_frame)：调小=慢动作，调大=加速"),
            started,
        ));
    }

    let frames_dir = args
        .flag_str("--frames-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("frames"));

    let encoder = match args.flag_str("--encoder") {
        None => runner::MovieEncoder::Png,
        Some(raw) => runner::MovieEncoder::parse(raw).ok_or_else(|| {
            fail(
                args,
                "movie",
                ErrorInfo::new("invalid", format!("未知的 --encoder 取值 \"{raw}\""))
                    .hint("可选：png（缺省，帧序列）/ gif（内置，免外部工具）/ apng（内置，无损）"),
                started,
            )
        })?,
    };

    let output = args.flag_str("--out").map(PathBuf::from);
    // PNG 序列没有「单个输出文件」的概念；反过来单文件编码器也不需要帧目录，
    // 两者都显式给出来就是意图冲突，直接说清楚而不是猜。
    if encoder.single_file() && args.has("--frames-dir") {
        return Err(fail(
            args,
            "movie",
            ErrorInfo::new(
                "invalid",
                format!(
                    "--encoder {} 输出单个文件，不与 --frames-dir 同时使用",
                    encoder.as_str()
                ),
            )
            .hint("单文件编码用 --out 指定路径（缺省 movie.gif / movie.png）"),
            started,
        ));
    }
    if !encoder.single_file() && output.is_some() {
        return Err(fail(
            args,
            "movie",
            ErrorInfo::new(
                "invalid",
                "--out 只对单文件编码器有效（--encoder gif|apng）",
            )
            .hint("PNG 序列请用 --frames-dir 指定目录"),
            started,
        ));
    }

    Ok(runner::MovieOptions {
        no_clobber: args.has("--no-clobber"),
        dry_run: args.has("--dry-run"),
        frames,
        fps,
        frames_dir,
        steps_per_frame,
        warmup_frames,
        width: doc.render.width,
        height: doc.render.height,
        encoder,
        output,
    })
}

/// 解析 `--ray` / `--overlap`。
fn parse_query_kind(args: &Args) -> Result<runner::QueryKind, String> {
    let ray = args.flag_str("--ray");
    let overlap = args.flag_str("--overlap");
    match (ray, overlap) {
        (Some(_), Some(_)) => Err("--ray 与 --overlap 只能给一个".into()),
        (None, None) => Err("需要 --ray（射线）或 --overlap（形状重叠）之一".into()),
        (Some(text), None) => runner::parse_ray_query(text),
        (None, Some(text)) => runner::parse_overlap_query(text),
    }
}

/// `query` 的人可读输出。
fn query_human(report: &runner::QueryReport) -> String {
    let mut out = match report.query.as_str() {
        "overlap" => format!(
            "查询      : 重叠 {} 在 ({:.3},{:.3},{:.3}){}，最远 {:.3}
",
            report.shape.clone().unwrap_or_default(),
            report.at.map(|a| a[0]).unwrap_or(0.0),
            report.at.map(|a| a[1]).unwrap_or(0.0),
            report.at.map(|a| a[2]).unwrap_or(0.0),
            match report.rotation_deg {
                Some(r) if r != [0.0, 0.0, 0.0] =>
                    format!("，旋转 ({:.1},{:.1},{:.1})°", r[0], r[1], r[2]),
                _ => String::new(),
            },
            report.max_distance,
        ),
        _ => format!(
            "查询      : 射线 从 ({:.3},{:.3},{:.3}) 朝 ({:.3},{:.3},{:.3})，最远 {:.3}
",
            report.origin.map(|o| o[0]).unwrap_or(0.0),
            report.origin.map(|o| o[1]).unwrap_or(0.0),
            report.origin.map(|o| o[2]).unwrap_or(0.0),
            report.direction.map(|o| o[0]).unwrap_or(0.0),
            report.direction.map(|o| o[1]).unwrap_or(0.0),
            report.direction.map(|o| o[2]).unwrap_or(0.0),
            report.max_distance,
        ),
    };
    out.push_str(&format!(
        "过滤      : 层掩码 0x{:08x} · {}
",
        report.mask,
        if report.include_sensors {
            "含传感器"
        } else {
            "不含传感器"
        }
    ));
    out.push_str(&format!(
        "刚体      : {}（本次检查 {} 个{}{}）
",
        report.bodies,
        report.tested,
        if report.indexed {
            "，查询索引"
        } else {
            ""
        },
        if report.fallback {
            "，索引过期已回退线性扫描"
        } else {
            ""
        }
    ));
    out.push_str(&format!(
        "命中      : {}
",
        report.count
    ));
    for h in &report.hits {
        out.push_str(&format!(
            "  {:<14} {:<9} 距离 {:>8.3}  点 ({:>7.3},{:>7.3},{:>7.3})  法线 ({:>6.3},{:>6.3},{:>6.3})  层 {}{}
",
            h.name,
            h.kind,
            h.distance,
            h.point[0],
            h.point[1],
            h.point[2],
            h.normal[0],
            h.normal[1],
            h.normal[2],
            h.layer,
            if h.sensor { " 传感器" } else { "" }
        ));
    }
    for w in &report.warnings {
        out.push_str(&format!(
            "警告      : {w}
"
        ));
    }
    out
}

/// 解析 `--trace` / `--trace-all` 指定的刚体名。
fn trace_targets(args: &Args, doc: &quench3d_cli::scene_doc::SceneDoc) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Some(list) = args.flag_str("--trace") {
        for part in list.split(',') {
            let name = part.trim();
            if !name.is_empty() {
                names.push(name.to_string());
            }
        }
    }
    if args.has("--trace-all") {
        // 只跟踪会动的：静态地面/墙的轨迹没有意义。
        // 用**展开后**的文档：`instances` 摆出来的副本也是独立刚体，也该被跟踪。
        let expanded = doc.expanded();
        for (index, o) in expanded.objects.iter().enumerate() {
            if !matches!(o.body, quench3d_cli::scene_doc::BodyKind::Static)
                && !matches!(o.body, quench3d_cli::scene_doc::BodyKind::None)
            {
                let name = quench3d_cli::runner::object_name(&expanded, index);
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}

/// 轨迹的人可读摘要：结论 + 高度曲线。
fn trajectory_human(t: &quench3d_cli::runner::Trajectory) -> String {
    let mut out = format!(
        "轨迹 {}  : 起点 ({:.2},{:.2},{:.2}) → 终点 ({:.2},{:.2},{:.2})\n",
        t.name, t.start[0], t.start[1], t.start[2], t.end[0], t.end[1], t.end[2]
    );
    out.push_str(&format!(
        "            下坠 {:.2} m · 爬升 {:.2} m · 位移 {:.2} m · 路程 {:.2} m · 最高速 {:.2} m/s\n",
        t.fall, t.rise, t.travel, t.path_length, t.max_speed
    ));
    out.push_str(&format!(
        "            形状 {} · 最大倾斜 {:.1}° · {}{}\n",
        t.shape,
        t.max_tilt_deg,
        if !t.tilt_meaningful {
            "倾斜无意义（球/平面会随意自转）"
        } else if t.tipped_over {
            "翻倒"
        } else {
            "站住了"
        },
        match t.last_motion_step {
            Some(step) => format!(" · 最后运动在第 {step} 步"),
            None => " · 全程静止".to_string(),
        }
    ));
    if let Some(plot) = height_plot(&t.samples, 48) {
        out.push_str(&format!("            高度曲线 {plot}\n"));
    }
    if t.truncated {
        out.push_str("            （采样点过多，已抽稀）\n");
    }
    out
}

/// 把 y 随步数的变化画成一行 sparkline：掉落、弹跳、静止一眼可见。
fn height_plot(samples: &[quench3d_cli::runner::TraceSample], width: usize) -> Option<String> {
    if samples.len() < 2 || width == 0 {
        return None;
    }
    let levels = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let ys: Vec<f32> = samples.iter().map(|s| s.position[1]).collect();
    let lo = ys.iter().cloned().fold(f32::INFINITY, f32::min);
    let hi = ys.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let span = (hi - lo).max(1e-3);
    let n = ys.len();
    let mut out = String::new();
    for i in 0..width {
        let idx = (i * n) / width;
        let y = ys.get(idx).copied().unwrap_or(lo);
        let t = ((y - lo) / span).clamp(0.0, 1.0);
        let level = (t * (levels.len() - 1) as f32).round() as usize;
        out.push(levels[level]);
    }
    Some(format!("{out}（{lo:.2} ~ {hi:.2} m）"))
}

fn cmd_ask(args: &Args) -> i32 {
    use quench3d_cli::generate::{generate, GenerateOptions};

    let started = Instant::now();
    let prompt = args.positional.join(" ");
    if prompt.trim().is_empty() {
        return missing(
            args,
            "ask",
            "需要自然语言描述",
            "quench3d ask \"三层箱子堆叠并倒塌\"",
        );
    }

    let mut config = quench3d_cli::llm::LlmConfig::from_env();
    if let Some(b) = args.flag_str("--base-url") {
        config.base_url = b.to_string();
    }
    if let Some(m) = args.flag_str("--model") {
        config.model = m.to_string();
    }
    let repair_rounds = args
        .flag_parse::<u32>("--repair-rounds")
        .ok()
        .flatten()
        .unwrap_or(2);
    let describe = config.describe();

    let head = format!(
        "正在请求 {describe} …（最多 {} 次尝试，出错会自动按诊断修复）",
        repair_rounds + 1
    );
    if args.json {
        eprintln!("{head}");
    } else {
        println!("{head}");
    }

    let client = LlmClient::new(config);
    let options = GenerateOptions::new(prompt.clone(), repair_rounds);
    let generated = match generate(&client, &options) {
        Ok(g) => g,
        Err(e) => {
            return fail(
                args,
                "ask",
                ErrorInfo::new(e.kind, e.message).hint(e.hint.unwrap_or_else(|| {
                    "本地服务示例：ollama serve && ollama pull qwen2.5:7b，\
                     然后 quench3d ask \"...\" --base-url http://localhost:11434/v1 --model qwen2.5:7b"
                        .into()
                })),
                started,
            );
        }
    };

    let valid = generated.valid();
    let errors = generated.errors();
    let diagnostics = serde_json::to_value(&generated.diagnostics).unwrap_or(Value::Null);

    // 落盘：即使不合法也写出去，agent 能看到模型的原始输出与诊断
    let mut scene_path: Option<String> = None;
    if let Some(out) = args.flag_str("--out") {
        if let Err(e) = std::fs::write(out, &generated.scene_json) {
            return fail(
                args,
                "ask",
                ErrorInfo::new("io", format!("写入 {out} 失败: {e}")),
                started,
            );
        }
        scene_path = Some(out.to_string());
    }

    // 渲染 / 仿真（只有合法场景才做）
    let mut render_report: Option<runner::RenderReport> = None;
    let mut sim_report: Option<runner::SimReport> = None;
    let mut extra_notes: Vec<String> = Vec::new();

    if let Some(doc) = &generated.scene {
        if valid {
            if args.has("--simulate") || args.flag_str("--render").is_some() {
                match runner::run_simulate(doc) {
                    Ok(r) => sim_report = Some(r),
                    Err(e) => extra_notes.push(format!("仿真失败: {e}")),
                }
            }
            if let Some(out) = args.flag_str("--render") {
                match runner::run_render(
                    doc,
                    Some(std::path::Path::new(out)),
                    runner::OutputPolicy::default(),
                ) {
                    Ok(r) => render_report = Some(r),
                    Err(e) => extra_notes.push(format!("渲染失败: {e}")),
                }
            }
        }
    }
    if !valid {
        extra_notes.push(format!(
            "模型输出未通过校验（共 {} 次尝试）；diagnostics 可直接回喂给模型。",
            generated.attempts_count()
        ));
    } else if generated.repair_rounds_used() > 0 {
        extra_notes.push(format!(
            "自动修复 {} 轮后通过（共 {} 次尝试）。",
            generated.repair_rounds_used(),
            generated.attempts_count()
        ));
    }

    let value = json!({
        "prompt": prompt,
        "model": generated.model,
        "scene": generated.value,
        "scene_json": generated.scene_json,
        "scene_path": scene_path,
        "valid": valid,
        "errors": errors,
        "diagnostics": diagnostics,
        "attempts": generated.attempts.iter().map(|a| a.to_json()).collect::<Vec<_>>(),
        "repair_rounds": generated.repair_rounds_used(),
        "llm_output_chars": generated.text.chars().count(),
        "usage": generated.usage.map(|t| json!({ "total_tokens": t })),
        "simulate": sim_report.as_ref().and_then(|r| serde_json::to_value(r).ok()),
        "render": render_report.as_ref().and_then(|r| serde_json::to_value(r).ok()),
        "notes": extra_notes,
    });

    let human = format!(
        "模型      : {}\n尝试      : {} 次{}\n输出长度  : {} 字符\n场景合法  : {}{}\n{}{}{}{}",
        generated.model,
        generated.attempts_count(),
        if generated.repair_rounds_used() > 0 {
            format!("（自动修复 {} 轮）", generated.repair_rounds_used())
        } else {
            String::new()
        },
        generated.text.chars().count(),
        if valid { "是" } else { "否" },
        if let Some(p) = &scene_path {
            format!("\n已写入    : {p}")
        } else {
            String::new()
        },
        render_report
            .as_ref()
            .map(|r| format!("已渲染    : {}（{} KB）\n", r.output, r.bytes / 1024))
            .unwrap_or_default(),
        sim_report
            .as_ref()
            .map(|s| format!(
                "物理      : {} 步，最大速度 {:.2} m/s，NaN {}\n",
                s.steps, s.max_speed, s.nan_bodies
            ))
            .unwrap_or_default(),
        if valid {
            String::new()
        } else {
            let mut t = String::from("诊断      :\n");
            if let Some(arr) = diagnostics.as_array() {
                for d in arr.iter().take(8) {
                    t.push_str(&format!(
                        "  {} : {}\n",
                        d["path"].as_str().unwrap_or("$"),
                        d["message"].as_str().unwrap_or("")
                    ));
                }
            }
            t
        },
        if scene_path.is_none() {
            format!("\n生成的场景：\n{}\n", generated.scene_json)
        } else {
            String::new()
        }
    );

    let ok = finish(args, "ask", Ok(value), started, |_| print!("{human}"));
    // 场景不合法时用 2，方便 agent 区分「模型输出不能用」与「网络/环境问题」
    if !valid && ok == EXIT_OK {
        EXIT_INVALID
    } else {
        ok
    }
}

/// `quench3d chat`：命令行 agent（模型自己调工具）。
fn cmd_chat(args: &Args) -> i32 {
    use quench3d_cli::agent::Agent;
    use std::io::Write;

    let started = Instant::now();
    let mut config = quench3d_cli::llm::LlmConfig::from_env();
    if let Some(b) = args.flag_str("--base-url") {
        config.base_url = b.to_string();
    }
    if let Some(m) = args.flag_str("--model") {
        config.model = m.to_string();
    }
    let max_rounds: u32 = args
        .flag_parse::<u32>("--max-rounds")
        .ok()
        .flatten()
        .unwrap_or(6);

    let mut agent = Agent::new(config);
    let prompt = args.positional.join(" ");

    // 单次模式：quench3d chat "需求"
    if !prompt.trim().is_empty() {
        return chat_once(args, &mut agent, &prompt, max_rounds, started);
    }

    // 交互模式
    println!("Quench3D agent（模型：{}）", agent.describe());
    println!("输入需求后回车；:help 看命令，:quit 退出。\n");
    let stdin = std::io::stdin();
    loop {
        print!("> ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        match stdin.read_line(&mut line) {
            Ok(0) => {
                println!();
                return EXIT_OK;
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("读取输入失败: {e}");
                return EXIT_RUNTIME;
            }
        }
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        match input {
            ":quit" | ":q" | ":exit" => return EXIT_OK,
            ":help" | ":h" => {
                println!(
                    ":help            显示帮助\n:tools           列出可用工具\n:reset           清空对话上下文\n:quit            退出\n其它输入         交给模型（它会自己调用工具）"
                );
                continue;
            }
            ":tools" => {
                if let Some(arr) = Agent::openai_tools().as_array() {
                    for t in arr {
                        println!(
                            "  {:<20} {}",
                            t["function"]["name"].as_str().unwrap_or("?"),
                            t["function"]["description"].as_str().unwrap_or("")
                        );
                    }
                }
                continue;
            }
            ":reset" => {
                agent.reset();
                println!("（上下文已清空）");
                continue;
            }
            _ => {}
        }
        match run_and_print(&mut agent, input, max_rounds) {
            Ok(()) => {}
            Err(code) => return code,
        }
    }
}

/// 单次提问。
fn chat_once(
    args: &Args,
    agent: &mut quench3d_cli::agent::Agent,
    prompt: &str,
    max_rounds: u32,
    started: Instant,
) -> i32 {
    let before = agent.history_len();
    match agent.run_turn(prompt, max_rounds) {
        Ok(turn) => {
            if args.json {
                let value = json!({
                    "prompt": prompt,
                    "answer": turn.text,
                    "rounds": turn.rounds,
                    "usage": turn.usage.map(|t| json!({ "total_tokens": t })),
                    "tools": turn.trace.iter().map(|t| json!({
                        "name": t.name,
                        "ok": t.ok,
                        "arguments": t.args,
                        "summary": t.summary,
                    })).collect::<Vec<_>>(),
                    "result": turn.structured,
                    "history_len": agent.history_len() - before,
                });
                let envelope =
                    Envelope::ok("chat", value, started.elapsed().as_millis() as u64);
                println!("{}", envelope.to_json());
            } else {
                for t in &turn.trace {
                    println!(
                        "  [工具] {}{} → {}",
                        t.name,
                        if t.ok { "" } else { "（失败）" },
                        t.summary
                    );
                }
                println!("\n{}", turn.text);
            }
            EXIT_OK
        }
        Err(e) => fail(
            args,
            "chat",
            ErrorInfo::new(e.kind, e.message).hint(e.hint.unwrap_or_else(|| {
                "本地服务示例：ollama serve && ollama pull qwen2.5:7b，                 然后 quench3d chat --base-url http://localhost:11434/v1 --model qwen2.5:7b"
                    .into()
            })),
            started,
        ),
    }
}

fn run_and_print(
    agent: &mut quench3d_cli::agent::Agent,
    input: &str,
    max_rounds: u32,
) -> Result<(), i32> {
    match agent.run_turn(input, max_rounds) {
        Ok(turn) => {
            for t in &turn.trace {
                println!(
                    "  [工具] {}{} → {}",
                    t.name,
                    if t.ok { "" } else { "（失败）" },
                    t.summary
                );
            }
            println!("{}\n", turn.text);
            Ok(())
        }
        Err(e) => {
            eprintln!("错误[{}]: {}", e.kind, e.message);
            if let Some(h) = e.hint {
                eprintln!("建议: {h}");
            }
            Err(match e.kind {
                "network" | "http" | "config" | "parse" => 4,
                _ => EXIT_RUNTIME,
            })
        }
    }
}

/// `--no-clobber` / `--dry-run`：两个都在"真正动手写盘/建 GPU"之前生效。
fn output_policy(args: &Args) -> runner::OutputPolicy {
    runner::OutputPolicy {
        no_clobber: args.has("--no-clobber"),
        dry_run: args.has("--dry-run"),
    }
}

fn missing(args: &Args, command: &str, message: &str, example: &str) -> i32 {
    fail(
        args,
        command,
        ErrorInfo::new("invalid", message).hint(format!("例如：{example}")),
        Instant::now(),
    )
}

fn fail(args: &Args, command: &str, error: ErrorInfo, started: Instant) -> i32 {
    let envelope = Envelope::fail(command, error, started.elapsed().as_millis() as u64);
    if args.json {
        println!("{}", envelope.to_json());
    } else if let Some(e) = &envelope.error {
        eprintln!("错误[{}]: {}", e.kind, e.message);
        if let Some(p) = &e.path {
            eprintln!("位置: {p}");
        }
        if let Some(h) = &e.hint {
            eprintln!("建议: {h}");
        }
        for d in e.diagnostics.iter().take(10) {
            eprintln!("  - {}: {}", d.path, d.message);
        }
    }
    envelope.exit_code()
}

fn finish(
    args: &Args,
    command: &str,
    result: Result<Value, ErrorInfo>,
    started: Instant,
    human: impl FnOnce(&Value),
) -> i32 {
    match result {
        Ok(value) => {
            let envelope =
                Envelope::ok(command, value.clone(), started.elapsed().as_millis() as u64);
            if args.json {
                println!("{}", envelope.to_json());
            } else {
                human(&value);
            }
            EXIT_OK
        }
        Err(info) => fail(args, command, info, started),
    }
}

/// 帮助文本（抽成函数是为了让测试能钉住"旗标必须在帮助里出现"）。
fn help_text() -> String {
    format!(
        r#"quench3d {version} — 纯 Rust 3D 引擎的命令行工具（渲染 / 物理 / 模型 / 大模型）

用法
  quench3d <命令> [参数...] [--json]

命令
  schema                输出能力清单（agent 自描述入口，建议先读这个）
  doctor                环境自检：平台、CPU、GPU 适配器、图形后端
  info <模型>            检查 glTF/GLB/OBJ：网格/材质/顶点/三角形/包围盒
  validate <场景>        校验场景 JSON，返回带路径的诊断
  render <场景>          无窗口渲染成 PNG（可先步进物理）
  movie <场景>           录制仿真视频：PNG 序列 + trace.jsonl（交 ffmpeg），或直接出 GIF/APNG
  simulate <场景>        只跑物理，输出每个刚体的位置/速度/位移/休眠
  bench                  性能与资源基准：物理吞吐、每步分配、轨迹开销、单帧渲染、常驻内存
  example [--out 文件]   输出一个可直接运行的示例场景 JSON
  ask "<需求>"           让大模型生成场景 JSON，可接着渲染或仿真
  chat [需求]            命令行 agent：模型自己调用工具（校验/仿真/渲染），不带需求则进交互模式
  mcp                    以 MCP 服务器方式运行（stdio，供 agent 调用）

常用参数
  --out <路径>           覆盖 render.output
  --camera <名字>        渲染 `cameras` 段里的命名相机（缺省用 `camera` 段那台）
  --asset-path <目录>    额外资源搜索路径：多个用 `:` 分隔（Windows `;`），只影响这次运行
  --width/--height <n>   覆盖渲染尺寸
  --steps <n>            覆盖 simulate.steps
  --trace <名字[,名字]>  simulate 时跟踪这些刚体的轨迹（位置/速度/倾斜）
  --trace-all            simulate 时跟踪所有会动的刚体
  --trace-every <n>      轨迹采样间隔（步，缺省 1）
  --dt <秒>              覆盖 simulate.dt
  --render <路径>        simulate 时顺带渲染
  --bodies <n[,n...]>    bench 要测的刚体规模（缺省 100,500,1000）
  --layout <布局>        bench 布局：scatter（单层平铺）或 stack（逐层堆叠）
  --warmup <n>           bench 预热步数（缺省 30）/ movie 预热帧数（缺省 0）
  --frames <n>           movie 输出帧数（缺省 60）
  --steps-per-frame <n>  movie 每帧推进的物理步数（缺省 1）
  --frames-dir <目录>    movie 帧序列目录（缺省 frames；仅 --encoder png）
  --encoder <格式>       movie 封装：png（缺省，帧序列）| gif | apng（后两者内置，免 ffmpeg）
  --out <文件>           movie 单文件输出（仅 --encoder gif|apng；缺省 movie.gif / movie.png）
  --fps <n>              movie 视频帧率（缺省 = 等价帧率 1/(dt×steps-per-frame)）
  --no-sleep             bench 关闭睡眠，测「一直在算」的最坏情况
  --no-render            bench 跳过渲染测量（无 GPU 时更快）
  --no-simulate          渲染前完全不步进物理
  --no-clobber           输出已存在就报错（退出码 2）而不是覆盖：多 agent 并行时用
  --dry-run              只校验 + 跑物理 + 采探针，不渲染、不写任何文件（render/movie）
  --base-url/--model     覆盖大模型服务地址与模型名
  --max-rounds <n>       agent 每轮最多往返多少次工具（缺省 6）
  --repair-rounds <n>    ask 生成场景失败后最多自动修复几次（缺省 2）
  --json                 输出单行 JSON 信封（agent 用）

大模型（OpenAI 兼容，本地优先）
  ollama serve && ollama pull qwen2.5:7b
  quench3d ask "斜坡上滚球" --base-url http://localhost:11434/v1 --model qwen2.5:7b
  也可以设环境变量 QUENCH3D_LLM_BASE_URL / QUENCH3D_LLM_MODEL / QUENCH3D_LLM_API_KEY

退出码
  0 成功 · 1 运行期错误 · 2 输入不合法 · 3 环境问题（无 GPU） · 4 网络/大模型问题

示例
  quench3d example --out scene.json
  quench3d validate scene.json --json
  quench3d render scene.json --out out.png
  quench3d simulate scene.json --steps 600 --json
  quench3d ask "三层箱子塔倒塌" --out tower.json --render tower.png
  quench3d chat "三层箱子塔五秒后还在原地吗" --json
  quench3d chat                              # 交互模式
"#,
        version = schema::VERSION
    )
}

fn print_help() {
    print!("{}", help_text());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: &[&str]) -> Args {
        Args::parse(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn schema_global_flags_are_registered_and_documented() {
        // schema 是 agent 的自描述入口，它说的每一句都得是真的：
        //  · 每个 kind=value 的条目必须在 VALUE_FLAGS 里（否则解析成布尔标志、值变位置参数）
        //  · 每个条目都必须在 --help 里出现（否则人和 agent 看到的说明书不一致）
        //  · 反向：VALUE_FLAGS 里的"面向所有人"的旗标必须进 schema，不然 agent 发现不了
        const MUST_BE_IN_SCHEMA: &[&str] = &["--json", "--camera", "--asset-path"];
        let schema = quench3d_cli::schema::schema();
        let flags = schema["global_flags"]
            .as_array()
            .expect("schema.global_flags 必须存在（agent 靠它发现全局旗标）");
        assert!(!flags.is_empty());
        let help = help_text();
        for f in flags {
            let name = f["name"].as_str().unwrap_or_default();
            let kind = f["kind"].as_str().unwrap_or_default();
            assert!(
                name.starts_with("--"),
                "global_flags 里有非旗标条目：{name}"
            );
            assert!(
                f["description"].as_str().is_some_and(|d| !d.is_empty()),
                "{name} 缺少 description——schema 是给 agent 看的，没描述等于没有"
            );
            if kind == "value" {
                assert!(
                    VALUE_FLAGS.contains(&name),
                    "{name} 标成 kind=value 但不在 VALUE_FLAGS 里"
                );
            } else {
                assert_eq!(kind, "boolean", "{name} 的 kind 只能是 value/boolean");
                assert!(
                    !VALUE_FLAGS.contains(&name),
                    "{name} 标成 boolean 却在 VALUE_FLAGS 里"
                );
            }
            assert!(help.contains(name), "{name} 在 schema 里但没写进 --help");
        }
        for name in MUST_BE_IN_SCHEMA {
            assert!(
                flags.iter().any(|f| f["name"] == *name),
                "{name} 必须在 schema.global_flags 里，否则 agent 发现不了它"
            );
        }
    }

    #[test]
    fn every_valued_flag_is_in_help_or_explicitly_deferred() {
        // 旗标加了、帮助里却没写 → 用户与 agent 只能靠猜（`schema` 目前不描述旗标）。
        // 钉死规则：要么在帮助里出现，要么写进下面的白名单。
        // 白名单是"已知没写"的**诚实清单，只应该变短**（这些多数是 query/bench 用的）。
        const DEFERRED: &[&str] = &[
            "--scene",
            "--device",
            "--ray",
            "--overlap",
            "--mask",
            "--max",
        ];
        // 按"词"匹配，不能只看子串：`--max` 是 `--max-rounds` 的前缀，
        // 用 contains 会把 `--max` 误判成"已经写进帮助"（这个误判正是第一版守卫的 bug）。
        fn mentions(help: &str, flag: &str) -> bool {
            help.match_indices(flag).any(|(i, _)| {
                let rest = &help[i + flag.len()..];
                !rest.starts_with('-')
                    && !rest
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric())
            })
        }
        let help = help_text();
        let mut missing = Vec::new();
        for flag in VALUE_FLAGS {
            if !mentions(&help, flag) && !DEFERRED.contains(flag) {
                missing.push(*flag);
            }
        }
        assert!(
            missing.is_empty(),
            "这些带值旗标既没写进 --help 也不在白名单里：{missing:?}"
        );
        // 白名单不许留已经写进帮助的项（白名单只能变短）
        for flag in DEFERRED {
            assert!(
                !mentions(&help, flag),
                "{flag} 已经写进帮助了，请从白名单里删掉"
            );
        }
    }

    #[test]
    fn every_valued_flag_in_schema_is_registered() {
        // 忘了把新参数加进 VALUE_FLAGS 会让 `--flag value` 变成一个布尔标志，
        // 值被当成位置参数——现象隐蔽，所以用测试钉死。
        let schema = quench3d_cli::schema::schema();
        let commands = schema["commands"].as_array().expect("commands");
        let empty = Vec::new();
        let mut checked = 0usize;
        for cmd in commands {
            for arg in cmd["args"].as_array().unwrap_or(&empty) {
                let name = arg["name"].as_str().unwrap_or("");
                let ty = arg["type"].as_str().unwrap_or("");
                if !name.starts_with("--") || ty == "flag" {
                    continue;
                }
                assert!(
                    VALUE_FLAGS.contains(&name),
                    "参数 {name}（{}）没有登记进 VALUE_FLAGS",
                    cmd["name"]
                );
                checked += 1;
            }
        }
        assert!(checked >= 15, "检查到的参数太少: {checked}");
    }

    #[test]
    fn parses_command_and_value_flags() {
        let a = parse(&[
            "render",
            "scene.json",
            "--out",
            "a.png",
            "--width",
            "640",
            "--json",
        ]);
        assert_eq!(a.command, "render");
        assert_eq!(a.positional, vec!["scene.json".to_string()]);
        assert_eq!(a.flag_str("--out"), Some("a.png"));
        assert_eq!(a.flag_parse::<u32>("--width").unwrap(), Some(640));
        assert!(a.json);
    }

    #[test]
    fn parses_equals_form_and_boolean_flags() {
        let a = parse(&["ask", "做", "个", "塔", "--simulate", "--model=qwen2.5:7b"]);
        assert_eq!(a.command, "ask");
        assert_eq!(a.positional.join(" "), "做 个 塔");
        assert!(a.has("--simulate"));
        assert_eq!(a.flag_str("--model"), Some("qwen2.5:7b"));
    }

    #[test]
    fn missing_value_is_an_error() {
        let err = Args::parse(&["render".into(), "s.json".into(), "--width".into()]).unwrap_err();
        assert!(err.contains("--width"), "{err}");
    }
}
