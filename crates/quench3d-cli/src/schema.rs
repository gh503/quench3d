//! 能力清单（`quench3d schema`）与大模型系统提示。
//!
//! agent 接入时先读 `quench3d schema --json`，就能知道：有哪些命令、每个参数的类型与
//! 缺省值、场景文件的完整语法、退出码与**诊断码**的含义、以及可以直接照抄的示例。
//!
//! [`system_prompt`] 是同一份能力给大模型看的浓缩版：把场景语法、单位、可用图元、
//! 关节类型和输出要求讲清楚，让模型直接产出可用的场景 JSON。

use crate::scene_doc::ErrorCode;
use serde_json::{json, Value};

/// CLI 版本。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 一个可以直接跑起来的最小场景（也被 `quench3d example` 用来生成模板）。
pub const EXAMPLE_SCENE: &str = r#"{
  "format": "quench3d.scene",
  "version": 2,
  "name": "示例：斜坡上的塔",
  "gravity": [0, -9.81, 0],
  "ground": { "size": 40, "material": "floor" },
  "materials": {
    "floor": { "color": [0.55, 0.58, 0.62], "roughness": 0.9 },
    "wood": { "color": [0.78, 0.48, 0.26], "roughness": 0.55 },
    "steel": { "color": [0.70, 0.72, 0.76], "metallic": 0.9, "roughness": 0.28 }
  },
  "objects": [
    { "name": "斜坡", "shape": { "type": "box", "size": [4, 0.4, 4] },
      "position": [0, 1.0, 0], "rotation_deg": [0, 0, -12], "material": "floor", "body": "static" },
    { "name": "箱子1", "shape": { "type": "box", "size": [0.8, 0.8, 0.8] },
      "position": [-0.6, 2.6, 0], "material": "wood", "body": "dynamic",
      "density": 400, "restitution": 0.15, "friction": 0.7 },
    { "name": "箱子2", "shape": { "type": "box", "size": [0.8, 0.8, 0.8] },
      "position": [0.4, 3.6, 0.3], "material": "wood", "body": "dynamic",
      "density": 400, "restitution": 0.15, "friction": 0.7 },
    { "name": "球", "shape": { "type": "sphere", "radius": 0.35 },
      "position": [2.5, 4.0, -1.5], "material": "steel", "body": "dynamic",
      "density": 2000, "restitution": 0.4 }
  ],
  "camera": { "position": [7, 4.5, 8], "look_at": [0, 1.2, 0], "fov_deg": 50 },
  "lighting": { "preset": "studio", "sun_direction": [-0.4, -1, -0.35], "shadows": true },
  "simulate": { "steps": 180, "dt": 0.0166667 },
  "render": { "output": "scene.png", "width": 960, "height": 600, "msaa": 4 }
}
"#;

/// 诊断码清单（`quench3d schema --json` 的 `error_codes`）。
///
/// **不手写第二份表**：直接从 [`ErrorCode::TABLE`] 生成，所以码表改了这里自动跟上，
/// 不可能出现"文档说支持、schema 里没有"或反过来的情况。
pub fn error_codes() -> Value {
    Value::Array(
        ErrorCode::TABLE
            .iter()
            .map(|(_, wire, meaning, action)| {
                json!({ "code": wire, "meaning": meaning, "agent_action": action })
            })
            .collect(),
    )
}

/// 完整能力清单。
pub fn schema() -> Value {
    json!({
        "name": "quench3d",
        "version": VERSION,
        "description": "纯 Rust 3D 引擎的命令行工具：场景渲染、物理仿真、模型检查、大模型生成场景。",
        "invocation": {
            "binary": "quench3d",
            "install": "cargo install --path crates/quench3d-cli",
            "json_flag": "所有子命令都支持 --json，输出单行 JSON 信封，便于 agent 解析",
            "default_output": "不带 --json 时输出人类可读文本"
        },
        "exit_codes": {
            "0": "成功",
            "1": "运行期错误",
            "2": "输入不合法（场景校验失败 / 参数错误）——改输入后重试",
            "3": "环境问题（无可用 GPU / 后端缺失）——换机器或装驱动",
            "4": "网络或大模型服务问题——检查服务地址、模型名、API Key"
        },
        // 诊断码：agent 按 `code` 分支，而不是按中文文案分支。`code` 说**是什么性质**，
        // `path` 说**在哪里**；两个合起来才是完整定位。这张表由 `error_codes()` 从
        // scene_doc.rs 的 ErrorCode::TABLE 生成，并有测试与 docs/error_codes.md 对账。
        "error_codes": error_codes(),
        "envelope": {
            "success": { "ok": true, "command": "render", "version": VERSION, "elapsed_ms": 812, "data": {} },
            "failure": { "ok": false, "command": "render", "version": VERSION, "elapsed_ms": 3,
                          "error": { "kind": "scene", "message": "…", "path": "objects[0].shape", "hint": "…",
                                     "diagnostics": [ { "severity": "error", "code": "invalid_value", "path": "objects[0].shape.type", "message": "…" } ] } }
        },
        // 全局旗标：`commands[].args` 描述的是"某个命令的参数"，而 `--json`、`--camera`、
        // `--asset-path` 这类对所有命令都生效的旗标以前在 schema 里**完全不存在**，
        // agent 只能靠 --help 或者猜。这张表由测试钉住：每个条目都必须真的登记在
        // VALUE_FLAGS（kind=value）里并且必须出现在 --help 里。
        "global_flags": [
            { "name": "--json", "kind": "boolean", "description": "输出单行 JSON 信封（agent 用）" },
            { "name": "--camera", "kind": "value", "description": "渲染 cameras 段里的命名相机（缺省用 camera 段那台）" },
            { "name": "--asset-path", "kind": "value", "description": "额外资源搜索路径：多个用 : 分隔（Windows ;）——只影响这一次运行" },
            { "name": "--out", "kind": "value", "description": "覆盖 render.output / movie 输出文件" },
            { "name": "--width", "kind": "value", "description": "覆盖渲染宽度" },
            { "name": "--height", "kind": "value", "description": "覆盖渲染高度" },
            { "name": "--base-url", "kind": "value", "description": "覆盖大模型服务地址" },
            { "name": "--model", "kind": "value", "description": "覆盖大模型名" }
        ],
        "commands": [
            {
                "name": "schema",
                "summary": "输出本能力清单（agent 自描述入口）",
                "args": [],
                "examples": ["quench3d schema --json"]
            },
            {
                "name": "doctor",
                "summary": "环境自检：平台、CPU、GPU 适配器、图形后端、是否软件光栅化",
                "args": [],
                "examples": ["quench3d doctor --json"]
            },
            {
                "name": "info",
                "summary": "检查模型文件：网格/材质/贴图数量、顶点与三角形数、包围盒",
                "args": [
                    { "name": "path", "type": "path", "required": true, "description": "glTF/GLB/OBJ 文件路径" }
                ],
                "output": "{ path, format, meshes, materials, images, vertices, triangles, bounds_min, bounds_max, bounds_size, mesh_triangles }",
                "examples": ["quench3d info assets/chair.glb --json"]
            },
            {
                "name": "validate",
                "summary": "校验场景文件，返回全部诊断（错误与警告），不改动任何文件",
                "args": [
                    { "name": "scene", "type": "path", "required": true, "description": "场景 JSON 路径" }
                ],
                "output": "{ valid, errors, warnings, diagnostics[] }",
                "examples": ["quench3d validate scene.json --json"]
            },
            {
                "name": "export",
                "summary": "把场景里的图元物体导出为 Wavefront OBJ + MTL，或按扩展名 .gltf/.glb 导出单文件 glTF 2.0（.gltf 用 base64 内嵌 buffer，.glb 用二进制 BIN chunk）；导入的 models[]、凸包、缺贴图会列进 skipped",
                "args": [
                    { "name": "scene", "type": "path", "required": true, "description": "场景 JSON 路径" },
                    { "name": "out", "type": "path", "required": false, "description": "输出路径；.gltf/.glb 走 glTF 2.0（文本/二进制），否则 OBJ + MTL；缺省与场景同名 .obj" }
                ],
                "output": "{ scene, out, mtl, objects, vertices, triangles, skipped[] }",
                "notes": "只导出 objects/templates/instances 展开后的图元几何；贴图不嵌入 MTL，材质只写标量色/粗糙度/自发光/透明度；导出的是文档初始位姿，不是物理步进后的快照",
                "examples": [
                    "quench3d export scene.json model.obj --json",
                    "quench3d export scene.json model.gltf --json",
                    "quench3d export scene.json model.glb"
                ]
            },
            {
                "name": "render",
                "summary": "无窗口渲染场景为 PNG（可先步进物理）",
                "args": [
                    { "name": "scene", "type": "path", "required": true, "description": "场景 JSON 路径" },
                    { "name": "--out", "type": "path", "required": false, "description": "覆盖 render.output" },
                    { "name": "--width", "type": "u32", "required": false, "description": "覆盖宽度" },
                    { "name": "--height", "type": "u32", "required": false, "description": "覆盖高度" },
                    { "name": "--steps", "type": "u32", "required": false, "description": "覆盖 simulate.steps" },
                    { "name": "--no-simulate", "type": "flag", "required": false, "description": "完全不步进物理" },
                    { "name": "--dry-run", "type": "flag", "required": false, "description": "只校验 + 跑物理 + 采探针，不初始化 GPU、不写任何文件（渲染统计全为 0，报告里给 would_write）" },
                    { "name": "--no-clobber", "type": "flag", "required": false, "description": "输出已存在就报错（退出码 2，error.kind = output_exists，带 path 与 hint）而不是覆盖；多 agent 并行时用" }
                ],
                "output": "{ output, width, height, bytes, draw_calls, instances, triangles, culled, shadow_instances, lines, objects, camera_position, probes[]?, sensors[]?, simulate?, dry_run?, would_write[]?, elapsed_ms }",
                "examples": [
                    "quench3d render scene.json --json",
                    "quench3d render scene.json --out small.png --width 640 --height 400",
                    "quench3d render scene.json --dry-run --json   # 只验证，不花 GPU 时间也不写盘",
                    "quench3d render scene.json --out mine.png --no-clobber --json"
                ]
            },
            {
                "name": "movie",
                "summary": "录制仿真视频：PNG 序列 + trace.jsonl（编码交给外部 ffmpeg），或用内置编码器直接出 GIF/APNG 单文件",
                "args": [
                    { "name": "scene", "type": "path", "required": true, "description": "场景 JSON 路径" },
                    { "name": "--frames", "type": "u32", "required": false, "description": "输出帧数（缺省 60，上限 100000）" },
                    { "name": "--steps-per-frame", "type": "u32", "required": false, "description": "每帧推进的物理步数（缺省 1，上限 1000）" },
                    { "name": "--frames-dir", "type": "path", "required": false, "description": "帧序列目录（缺省 frames）；已存在的帧会跳过，便于续录。仅 --encoder png" },
                    { "name": "--encoder", "type": "enum", "required": false, "description": "封装格式：png（缺省，帧序列）| gif | apng | mp4。gif/apng 为内置纯 Rust 编码器，不需要 ffmpeg；mp4 调外部 ffmpeg（libx264/yuv420p）；gif 只有 256 色且帧延迟粒度为 10 ms" },
                    { "name": "--out", "type": "path", "required": false, "description": "单文件输出路径（仅 --encoder gif|apng；缺省 movie.gif / movie.png）" },
                    { "name": "--fps", "type": "f32", "required": false, "description": "视频帧率（缺省 = 等价帧率 1/(dt×steps-per-frame)）：调小=慢动作，调大=加速" },
                    { "name": "--warmup", "type": "u32", "required": false, "description": "预热帧数（缺省 0）：只推进不输出，丢弃首帧脏画面" },
                    { "name": "--width", "type": "u32", "required": false, "description": "覆盖宽度" },
                    { "name": "--height", "type": "u32", "required": false, "description": "覆盖高度" },
                    { "name": "--dt", "type": "f32", "required": false, "description": "覆盖 simulate.dt（物理步长）" },
                    { "name": "--dry-run", "type": "flag", "required": false, "description": "只校验 + 跑物理，不渲染、不建帧目录、不写 trace；报告里 digest 为空串并给出 would_write" },
                    { "name": "--no-clobber", "type": "flag", "required": false, "description": "单文件输出或帧目录已存在（非空）就报错，而不是覆盖/续录；要续录就别加这个开关" }
                ],
                "output": "{ frames_dir, encoder, output, trace, frames, skipped, fps, duration, steps_per_frame, dt, total_steps, warmup_frames, width, height, bytes, digest, objects, camera_position, probes[]?, sensors[]?, first_frame?, last_frame?, ffmpeg, notes, dry_run?, would_write[]?, elapsed_ms }",
                "notes": "帧数与时长由物理步决定、与墙钟无关；同一场景两次录制 digest 相同（换编码器也不改变 digest）。--encoder png 时 ffmpeg 字段给出编码命令：ffmpeg -framerate N -i frames/frame_%05d.png -c:v libx264 -crf 18 -pix_fmt yuv420p movie.mp4；gif/apng/mp4 时该字段为空串，可直接播放；notes 里会提示格式限制（如 GIF 的 10 ms 延迟粒度）",
                "examples": [
                    "quench3d movie scene.json --frames 120 --frames-dir frames --json",
                    "quench3d movie scene.json --frames 300 --steps-per-frame 1 --fps 30 --frames-dir slowmo",
                    "quench3d movie scene.json --frames 60 --encoder gif --out demo.gif --json"
                ]
            },
            {
                "name": "simulate",
                "summary": "只跑物理仿真（不需要 GPU），输出逐刚体位置、速度、位移、休眠与接触情况",
                "args": [
                    { "name": "scene", "type": "path", "required": true, "description": "场景 JSON 路径" },
                    { "name": "--steps", "type": "u32", "required": false, "description": "覆盖 simulate.steps" },
                    { "name": "--dt", "type": "f32", "required": false, "description": "覆盖 simulate.dt" },
                    { "name": "--render", "type": "path", "required": false, "description": "同时渲染到该 PNG" },
                    { "name": "--trace", "type": "string", "required": false, "description": "跟踪这些刚体的轨迹（名字逗号分隔）" },
                    { "name": "--trace-all", "type": "flag", "required": false, "description": "跟踪所有会动的刚体" },
                    { "name": "--trace-every", "type": "u32", "required": false, "description": "轨迹采样间隔（步，缺省 1）" },
                    { "name": "--dry-run", "type": "flag", "required": false, "description": "配 --render 用时：只校验 + 跑物理，不渲染不写盘。单独用（没有 --render）会报错——simulate 本身不写任何文件" },
                    { "name": "--no-clobber", "type": "flag", "required": false, "description": "配 --render 用时生效：渲染输出已存在就报错" }
                ],
                "output": "{ steps, dt, gravity, bodies, dynamic, sleeping, contacts, joints, joints_broken, nan_bodies, lowest_y, max_speed, states[], events[]?, events_dropped, probes[]?, sensors[]?, warnings[], trajectories[]? }（events 是事件总线：{step, kind, a, b?, force?, torque?}，kind ∈ contact_begin / contact_end / sensor_enter / sensor_exit / sleep / wake / joint_broken）（sensors 是 objects[].sensor = true 的触发区观测：谁的 enter_step/exit_step/steps_inside/entries，按名字排序，输出稳定）",
                "trajectory_fields": "name, shape, samples[{step,time,position,speed,displacement,tilt_deg,asleep}], start, end, min, max, fall, rise, travel, path_length, max_speed, max_tilt_deg, tilt_meaningful, tipped_over, last_motion_step",
                "examples": [
                    "quench3d simulate scene.json --steps 600 --json",
                    "quench3d simulate scene.json --trace 球,箱子 --steps 300",
                    "quench3d simulate scene.json --trace-all --render out.png"
                ]
            },
            {
                "name": "query",
                "summary": "在场景上做一次几何查询（射线 / 任意形状重叠），不需要 GPU、不改动场景",
                "args": [
                    { "name": "scene", "type": "path", "required": true, "description": "场景 JSON 路径" },
                    { "name": "--ray", "type": "string", "required": false, "description": "射线：ox,oy,oz,dx,dy,dz（与 --overlap 二选一）" },
                    { "name": "--overlap", "type": "string", "required": false, "description": "形状重叠：sphere:0.5 / box:1,2,1 / capsule:0.6,0.3 / cylinder:0.5,0.5 / cone:0.5,0.5 / plane:0,1,0，可接 @x,y,z 与 ~rx,ry,rz（度，XYZ 序）" },
                    { "name": "--all", "type": "flag", "required": false, "description": "射线返回全部命中（按距离升序）；缺省只返回最近的一个。对 --overlap 无意义，给了会报错" },
                    { "name": "--mask", "type": "u32", "required": false, "description": "层掩码（单向：只看被查方在不在掩码里），缺省 u32::MAX 全查" },
                    { "name": "--no-sensors", "type": "flag", "required": false, "description": "不把传感器（触发区）算作可命中；缺省算" },
                    { "name": "--max", "type": "f32", "required": false, "description": "最远距离（射线）/距离上限（重叠只报距离不超过它的刚体），缺省 10000" },
                    { "name": "--steps", "type": "u32", "required": false, "description": "先跑这么多步物理再查（缺省 0：按场景初始状态查）" },
                    { "name": "--index", "type": "flag", "required": false, "description": "用查询加速结构（快照网格）代替线性扫描；结果完全一致，报告里的 tested 会变小" }
                ],
                "output": "{ query, origin?, direction?, shape?, at?, rotation_deg?, max_distance, mask, include_sensors, hits[{name, kind, distance, point, normal, layer, sensor}], count, bodies, tested, indexed, fallback, warnings[] }",
                "examples": [
                    "quench3d query scene.json --ray 0,5,0,0,-1,0 --json",
                    "quench3d query scene.json --ray 0,1,0,1,0,0 --all --no-sensors",
                    "quench3d query scene.json --overlap \"box:2,1,2@0,1,0~0,45,0\""
                ]
            },
            {
                "name": "example",
                "summary": "输出一个可直接运行的示例场景 JSON（写入文件或标准输出）",
                "args": [
                    { "name": "--out", "type": "path", "required": false, "description": "写入文件；缺省打印到标准输出" }
                ],
                "examples": ["quench3d example > scene.json", "quench3d example --out scene.json"]
            },
            {
                "name": "ask",
                "summary": "用自然语言让大模型生成场景 JSON，校验后可直接渲染/仿真",
                "args": [
                    { "name": "prompt", "type": "string", "required": true, "description": "自然语言需求" },
                    { "name": "--base-url", "type": "url", "required": false, "description": "OpenAI 兼容服务地址，缺省读环境变量" },
                    { "name": "--model", "type": "string", "required": false, "description": "模型名" },
                    { "name": "--out", "type": "path", "required": false, "description": "把生成的场景写到这里" },
                    { "name": "--render", "type": "path", "required": false, "description": "生成后立刻渲染到该 PNG" },
                    { "name": "--simulate", "type": "flag", "required": false, "description": "生成后跑物理并返回报告" },
                    { "name": "--repair-rounds", "type": "u32", "required": false, "description": "生成不合法时最多自动修复几轮（缺省 2）" }
                ],
                "output": "{ scene, scene_json, scene_path?, valid, errors, diagnostics[], attempts[], repair_rounds, render?, simulate?, model, usage }",
                "examples": [
                    "quench3d ask \"做一个三层箱子堆叠然后倒下的场景\" --json",
                    "quench3d ask \"斜坡上滚球\" --out s.json --render s.png",
                    "quench3d ask \"三层塔\" --repair-rounds 3 --simulate"
                ]
            },
            {
                "name": "chat",
                "summary": "命令行 agent：把渲染/仿真/校验等工具交给大模型自己调用，多轮直到给出结论",
                "args": [
                    { "name": "prompt", "type": "string", "required": false, "description": "不带则进入交互模式（:help / :tools / :reset / :quit）" },
                    { "name": "--base-url", "type": "url", "required": false, "description": "OpenAI 兼容服务地址" },
                    { "name": "--model", "type": "string", "required": false, "description": "模型名" },
                    { "name": "--max-rounds", "type": "u32", "required": false, "description": "工具往返上限，缺省 6" }
                ],
                "tools": ["quench3d_schema", "quench3d_doctor", "quench3d_info", "quench3d_validate", "quench3d_render", "quench3d_movie", "quench3d_simulate", "quench3d_query"],
                "output": "{ prompt, answer, rounds, usage, tools[{name,ok,arguments,summary}], result }",
                "examples": [
                    "quench3d chat \"三层箱子塔五秒后还在原地吗\" --json",
                    "quench3d chat"
                ]
            },
            {
                "name": "bench",
                "summary": "性能与资源基准：物理吞吐、每步分配、轨迹采样开销、单帧渲染耗时、常驻内存",
                "args": [
                    { "name": "--bodies", "type": "csv<usize>", "required": false, "description": "要测的刚体规模，缺省 100,500,1000" },
                    { "name": "--steps", "type": "u32", "required": false, "description": "每个规模测多少步，缺省 300" },
                    { "name": "--warmup", "type": "u32", "required": false, "description": "预热步数，缺省 30" },
                    { "name": "--layout", "type": "scatter|stack", "required": false, "description": "单层平铺（开阔场景）或逐层堆叠（深堆最坏情况），缺省 scatter" },
                    { "name": "--no-sleep", "type": "flag", "required": false, "description": "关闭睡眠，测「一直在算」的最坏情况" },
                    { "name": "--no-render", "type": "flag", "required": false, "description": "跳过渲染测量（无 GPU 的机器上更快）" }
                ],
                "output": "{ platform, arch, threads, startup_ms, rss_mb, peak_rss_mb, layout, sleep, scales[{bodies,ms_per_step,steps_per_sec,body_steps_per_sec,sleeping,contacts}], trace{...}, render{...}, notes[] }",
                "examples": [
                    "quench3d bench --json",
                    "quench3d bench --bodies 500,2000,5000 --steps 300 --no-sleep --no-render",
                    "quench3d bench --layout stack --json"
                ]
            },
            {
                "name": "mcp",
                "summary": "以 MCP（Model Context Protocol）服务器方式运行，通过 stdio 暴露上述工具给支持 MCP 的 agent",
                "args": [],
                "protocol": "JSON-RPC 2.0 / MCP，方法：initialize, tools/list, tools/call, ping",
                "tools": ["quench3d_schema", "quench3d_doctor", "quench3d_info", "quench3d_validate", "quench3d_render", "quench3d_movie", "quench3d_simulate", "quench3d_query", "quench3d_ask"],
                "examples": ["quench3d mcp"]
            }
        ],
        "environment": {
            "QUENCH3D_LLM_BASE_URL": "OpenAI 兼容服务地址，例如 http://localhost:11434/v1（Ollama）",
            "QUENCH3D_LLM_MODEL": "模型名，例如 qwen2.5:7b",
            "QUENCH3D_LLM_API_KEY": "云端服务需要的 Key（本地服务可省略）",
            "QUENCH3D_LLM_TEMPERATURE": "采样温度，缺省 0.2",
            "QUENCH3D_LLM_MAX_TOKENS": "最大输出 token，缺省 2048",
            "QUENCH3D_LLM_TIMEOUT": "超时秒数，缺省 180",
            "OPENAI_BASE_URL/OPENAI_MODEL/OPENAI_API_KEY": "通用回退变量",
            "OLLAMA_HOST": "只给主机名时的回退（自动补 /v1）"
        },
        "scene": scene_grammar(),
        "units": {
            "length": "米",
            "time": "秒",
            "mass": "kg（由密度×体积推导）",
            "angle": "度（字段名以 _deg 结尾）；关节限位中铰链用弧度、滑块用米",
            "color": "0~1 的 sRGB 分量"
        }
    })
}

/// 场景文件语法（供 agent 与文档共用）。
pub fn scene_grammar() -> Value {
    json!({
        "format": "quench3d.scene",
        "version": crate::scene_doc::VERSION,
        "note": "所有字段都有缺省值，`{}` 就是合法场景",
        "top_level": {
            "format": "字符串，缺省 quench3d.scene",
            "version": "整数，当前 2（版本 2 起 parent 生效）。版本 1 的文件：把 version 改成 2 即可——没有 parent 时语义完全一样",
            "name": "场景名（可选）",
            "gravity": "[x, y, z]，缺省 [0, -9.81, 0]",
            "physics": {
                "friction_combine": ["geometric_mean（缺省，= sqrt(a×b)，历史行为）", "average", "min", "max", "multiply"],
                "restitution_combine": ["缺省 max（= max(ea,eb)，历史行为）", "average", "min", "geometric_mean", "multiply"],
                "static_friction_speed": "切向速度低于它就算静止接触（m/s），缺省 0.02；写大 = 更容易「黏住」",
                "note": "世界级缺省；物体上的同名字段优先。不写这一节 = 全用引擎缺省（结果与没有这一节逐位相同）"
            },
            "ground": { "size": "f32 边长，缺省 40", "material": "材质名（可选）", "y": "f32 高度，缺省 0" },
            "waters": [{
                "center": "[x, z]，缺省 [0, 0]",
                "size": "[x, z] 米，必须为正（水面范围，不是池底）",
                "level": "水面高度 y（米）",
                "density": "密度 kg/m³，缺省 1000（水）",
                "drag": "浸没部分线性阻尼 1/s，缺省 6；0 = 无阻尼",
                "note": "一维浮力环境：y <= level 且在 XZ 范围内算浸没；没有粒子/流动/波浪（M1）。不写 = 没有水，物理结果与以前逐位相同",
            }],
            "fluids": [{
                "center": "[x, y, z]，缺省 [0, 0, 0]",
                "size": "[x, y, z] 米，必须为正（长方体发射器）",
                "spacing": "粒子间距（米），缺省 0.1；必须不大于发射体最小边长的 2 倍",
                "radius": "粒子碰撞半径（米），缺省 spacing*0.5；必须不大于 spacing",
                "rest_density": "静止密度 kg/m³，缺省 1000（水）",
                "viscosity": "XSPH 粘性 0~1，缺省 0.05",
                "drag": "浸没刚体线性阻尼 1/s，缺省 6；0 = 无阻尼",
                "iterations": "密度约束迭代次数 1~8，缺省 4",
                "max_particles": "粒子上限 1~20000，缺省 4000；超出按晶格顺序截断",
                "note": "CPU PBF 粒子流体（M3 双向）：粒子被刚体推开并反推冲量；粒子自由表面给刚体浮力/阻尼；渲染为低模球粒子 + 屏幕空间水面（mask 折射/吸收/菲涅耳/边缘泡沫，不是物理光线折射）。不写 = 没有粒子，物理结果与以前逐位相同",
            }],
            "materials": "名字 → 材质对象",
            "particles": [
                "CPU 公告板粒子发射器数组（纯视觉，不参与物理）",
                "每项字段：count（1~4096，缺省 64）、enabled（缺省 true，false 时隐藏且不推进）、position（缺省 [0,1,0]）、radius（初始球形半径，缺省 0.1）、velocity（缺省 [0,1.2,0]）、spread（速度扰动，缺省 0.4）、drag（线性阻尼 1/s，缺省 0）、rotation/rotation_end（公告板自转角，度，缺省 0）、alignment（billboard/velocity，缺省 billboard）、lifetime（秒，必须 >0，缺省 2）、size（初始四边形边长米，必须 >0，缺省 0.12）、size_end（寿命结束时边长，缺省同 size）、shape（初始分布 sphere/box/disc/cone，缺省 sphere）、box_size（box 的全尺寸 / cone 的 y 为高，缺省 [1,1,1]）、gravity（缺省 [0,-1,0]）、color（线性 RGBA，alpha 0~1，缺省 [1,0.65,0.2,0.8]）、color_end（寿命结束时颜色，缺省同 color）、texture（可选 PNG/JPEG 公告板贴图，颜色仍乘上去）",
                "粒子是面向相机的四边形；按寿命淡出/缩放后回到发射器重生。初始位置可在球内或长方体内分布；有可选贴图；按实例 AABB 中心距离参与全局透明排序，但没有逐像素排序/软粒子/GPU 粒子",
            ],
            "decals": [
                "平面贴花数组（纯视觉，不参与物理）",
                "每项字段：position（缺省 [0,0,0]）、normal（缺省 [0,1,0]，会归一化）、size（[宽,高] 米，缺省 [1,1]，必须 >0）、rotation（绕法线角度，缺省 0）、color（线性 RGBA，缺省白）、texture（可选 PNG/JPEG）",
                "贴花是**平的**四边形：沿 normal 抬 1cm 防 z-fight；不做深度投影，曲面/斜坡上会悬空或穿插",
            ],
            "include": [
                "被包含的外部场景片段路径数组（跨文件复用：把一份场景拆成多个文件）",
                "相对路径以**写这一行的文件**所在目录为基准；片段可以再 include（最多 16 层，同一个文件只并入一次，成环报 include_cycle）",
                "片段只能贡献可合并的段：materials/templates/cameras（重名 = duplicate_name 错误，不静默覆盖）+ waters/fluids/objects/models/joints/actions/probes/animations（按声明顺序追加：本文件自己的内容在前，被包含的内容在后）",
                "根独有的段（ground/camera/lighting/simulate/render/gravity/physics/name/assets）写在片段里是错误",
                "片段里的贴图/模型相对路径按**片段自己的目录**解析（装载时重定基），不会因为被谁包含而指错地方",
                "跨文件 prefab 的推荐写法：片段只放 templates，根场景用 objects[].template + instances 实例化",
            ],
            "templates": "名字 → 模板对象（字段集与物体完全相同）。物体写 `template: \"名字\"` 复用；物体自己写的字段覆盖模板；模板里写 name/parent/position/rotation_deg 没有意义（会有警告并被忽略）",
            "objects": "图元物体数组",
            "models": "外部模型数组（gltf/glb/obj）",
            "assets": "资源搜索路径（贴图与模型的资源库）",
            "joints": "关节数组，用物体名连接",
            "actions": "定时动作数组：在指定步/秒对刚体施加冲量、力、速度或瞬移",
            "probes": "区域探针数组：只读记录谁何时进入/离开某个球形或盒形区域",
            "camera": "相机对象",
            "cameras": "命名相机表：名字 → 相机对象（`--camera <名字>` 选一台）",
            "lighting": "光照对象",
            "simulate": "物理步进对象",
            "render": "渲染输出对象"
        },
        "material": {
            "color": "[r, g, b]，0~1，**sRGB**",
            "metallic": "0~1，缺省 0",
            "roughness": "0.05~1，缺省 0.5",
            "emissive": "自发光强度，缺省 0",
            "emissive_color": "[r,g,b]，**sRGB**；给了就用它当自发光颜色，缺省沿用 color",
            "alpha": "0~1，不透明度，缺省 1",
            "uv_scale": "贴图 UV 缩放，缺省 1（>1 为平铺）",
            "uv_offset": "贴图 UV 偏移 [u, v]，缺省 [0, 0]",
            "uv_rotation": "贴图 UV 旋转（度，绕贴图中心），缺省 0",
            "normal_scale": "法线贴图强度，缺省 1",
            "alpha_cutoff": "alpha 裁剪阈值：大于它的像素才画（做树叶/栅栏），缺省 0",
            "alpha_blend": "true 时走半透明混合（与 alpha_cutoff 同时给时裁剪优先，混合不生效）",
            "unlit": "true 时不参与光照，直接用颜色/贴图（做 UI、发光体）",
            "double_sided": "true 时背面也画（单面片、薄墙），缺省 false",
            "base_color_texture": "贴图路径，相对**场景文件**解析（也支持 assets 搜索路径）",
            "normal_texture": "法线贴图路径",
            "metallic_roughness_texture": "金属度/粗糙度贴图路径（glTF 约定：G=粗糙度，B=金属度）",
            "emissive_texture": "自发光贴图路径",
            "ao_texture": "环境光遮蔽（AO）贴图路径，只采样 R 通道",
            "ao_strength": "AO 强度 0~1，缺省 1；0 = 忽略 AO 贴图"
        },
        "shape": {
            "type": ["box", "sphere", "capsule", "cylinder", "cone", "rounded_box", "heightfield", "plane", "convex"],
            "box": { "size": "[x, y, z] 边长（不是半长），缺省 [1,1,1]" },
            "sphere": { "radius": "缺省 0.5" },
            "capsule": { "radius": "缺省 0.5", "half_height": "圆柱段半高，缺省 0.5" },
            "cylinder": { "radius": "缺省 0.5", "half_height": "缺省 0.5" },
            "cone": { "radius": "底面半径，缺省 0.5", "half_height": "缺省 0.5（底面在 -half_height、顶点在 +half_height）" },
            "rounded_box": {
                "size": "[x, y, z] 外接**边长**，缺省 [1,1,1]（和 box 同一套）",
                "radius": "**必填**圆角半径，≤ 最短边长的一半（等于一半时（三边相等）就是球）",
                "note": "几何 = 盒子 ⊕ 半径 radius 的球；惯量按确定性网格采样（误差 < 1%）"
            },
            "heightfield": {
                "heights": "行主序的二维数组（heights[行][列]，行沿 Z、列沿 X），至少 2×2；网格以形状原点为中心，值是相对原点的高度",
                "cell_size": "格子边长（米），缺省 1.0",
                "body": "**【只能 static】**：物理上是静态三角网（没有质量属性），写 dynamic/kinematic 会被校验拦下",
                "note": "格点数（行×列）上限 65536；窄相逐三角形，地形大就切块"
            },
            "plane": { "normal": "[x,y,z]，缺省 [0,1,0]" },
            "convex": { "points": "至少 4 个 [x,y,z] 顶点" }
        },
        "object": {
            "name": "名称（关节引用与报告都用它；缺省 objects[i]）",
            "shape": "形状对象，缺省 1×1×1 立方体；与 shapes **二选一**（两个都写报错）",
            "template": "复用 templates 里的模板（物体显式字段覆盖模板）",
            "instances": "一次摆多个副本：[{ name?, position?, rotation_deg?, scale? }]。每个副本是一个**独立物体/刚体**（不是共享刚体）；名字缺省 `本名#1`、`本名#2`…；只写一个且没写 name 时保持本名。引用它的关节/动作请写 `本名#序号`",
            "shapes": "复合形状（一个刚体多个 Shape）：[{ shape, position?, rotation_deg? }]，部件的 position/rotation_deg 是**相对刚体原点**的局部位姿；质量/质心/惯量按所有部件合成，碰撞/AABB/查询逐部件；【边界】部件形状不能是三角网模型；每对刚体最多保留 8 个接触点（取最深的）",
            "parent": "父物体名（objects[].name）；给了之后 position/rotation_deg 变成**相对父物体**的局部量（继承位置与朝向，**不继承缩放**）。带刚体的物体不能有父物体",
            "position": "[x, y, z]，缺省原点；有 parent 时是局部的",
            "rotation_deg": "欧拉角 XYZ，单位度",
            "scale": "显示缩放（只影响渲染，不影响碰撞）；不受父物体缩放影响",
            "material": "材质名（缺省灰色）",
            "body": ["none（纯显示，缺省）", "static", "dynamic", "kinematic"],
            "density": "kg/m³，缺省 1000",
            "restitution": "0~1，缺省 0.2",
            "friction": "动摩擦系数，缺省 0.5",
            "static_friction": "静摩擦系数（切向速度很小时用），缺省跟随 friction；≥ friction 才「推得动又推不住」",
            "friction_combine": "覆盖 physics.friction_combine（两端都写时取 slot 靠前的那个）",
            "restitution_combine": "覆盖 physics.restitution_combine",
            "velocity": "初始线速度 [x,y,z]",
            "angular_velocity": "初始角速度 [x,y,z]（弧度/秒）",
            "linear_damping": "线性阻尼 ≥0，缺省 0（不衰减）",
            "angular_damping": "角阻尼 ≥0，缺省 0",
            "gravity_scale": "重力缩放：1 正常、0 失重、负值反重力，缺省 1",
            "mass": "直接给质量（kg），覆盖 density×体积",
            "allow_sleep": "false 时永不自动睡眠（缺省 true）",
            "visible": "缺省 true",
            "cast_shadows": "是否投射阴影，缺省 true；false 仍正常显示，只是不写进阴影图",
            "receive_shadows": "是否接收阴影，缺省 true；false 时不被太阳/聚光/点光阴影压暗",
            "collision_layer": "碰撞层 0~31，缺省 0",
            "collides_with": "愿意碰的层号列表，缺省所有层；判断是**对称**的（双方都愿意才碰），[] 表示谁也不碰",
            "sensor": "true = 触发区：照常报进入/离开，但不产生冲量（别的物体穿过去）；需要 body 不为 none",
            "ccd": "true = 连续碰撞检测（每子步从中心往前打一条射线，把这一步的位移截在命中点）；只对 dynamic 生效",
            "note": "linear/angular_damping、gravity_scale、mass、allow_sleep、angular_velocity 只对 body=dynamic 生效"
        },
        "model": {
            "path": "gltf/glb/obj 路径",
            "position": "[x, y, z]",
            "rotation_deg": "欧拉角，度",
            "scale": "均匀缩放，缺省 1",
            "up_axis": "资产的「上」轴：y（缺省，glTF 规范）或 z（Blender/OBJ 常见的 Z-up）。写 z 时在用户 rotation_deg **之前**绕 X 轴 -90°，位置/缩放不变；碰撞体与 LOD 用同一变换",
            "collide": "true 时把模型合并成静态三角网格碰撞体，缺省 false",
            "friction": "参与碰撞时的摩擦，缺省 0.8",
            "morph_weights": "手工覆盖 glTF morph target 的权重数组（0~1，按 target 顺序；缺省用资产默认值，缺项按 0）；写了它就优先于 clip 自带的 morph 权重动画",
            "ik": "两骨 IK 链数组：{ root, mid, tip, target:[x,y,z], pole?, weight? }。按 glTF 节点名引用，目标/极向量在模型局部空间；需要同时有 animation 或 animator；不可达目标夹到骨骼长度，不拉伸；weight 0~1 在 FK 姿态与全 IK 之间混合（缺省 1）",
            "animator": "状态机 { initial, states, transitions?, event_window? }；与 animation 互斥。过渡按停留时间 after 秒、on_finished 播完、conditions 参数比较（float 参数，6 种比较）或 on_event 事件切换；fade 秒淡入（0 = 立即切换）。宿主用 set_animator_parameter / fire_animator_event 输入；事件是有界 FIFO（8 条，每帧消费一条，满时丢最旧），可在当前动作结束前预输入下一招；event_window 秒内未被接住的事件自动过期（0 = 不过期）",
            "animation": "播放模型里的一条 glTF 动画：{ name, start?, speed?, loop?, blend?, root_motion? }。不写 = 静止（老行为）；loop 缺省 true（写 false = 播完停在最后一帧）；带 morph weights 轨道的 clip 会同时驱动形变（手工 morph_weights 优先；双 clip 只取主 clip 的 morph 轨道）；blend: { name, start?, speed?, loop?, weight?, fade? } 是第二条 clip 的交叉淡化（weight 缺省 0.5，fade 缺省 0 = 立即生效）；root_motion 写 glTF 节点名，把该节点三维位移与绕 Y 偏航提取到整个物体上（只支持单 clip、loop=false、无 blend/animator/ik；俯仰/滚转留在模型内）；名字不存在**不是**校验错误（validate 不读资产），会在 simulate/render 报告的 models[].animation.found / models[].blend.found / root_motion.found 里报 false"
        },
        "joint": {
            "type": ["fixed", "ball", "hinge", "distance", "slider"],
            "a": "物体名（必须有刚体）",
            "b": "物体名（必须有刚体）",
            "axis": "铰链/滑块的轴（**各自刚体的局部轴**），缺省 [0,1,0]；两端朝向不同时必须改用 axis_a/axis_b",
            "axis_a": "A 端的局部轴（铰链转轴 / 滑块滑轴）；不写 = 用 axis",
            "axis_b": "B 端的局部轴；不写 = 用 axis。把想要的世界轴乘上 B 初始朝向的逆就是它（门板转了 90° 时，世界 Y 往往是 B 的局部 Z）",
            "anchor_a": "关节在 a 上的连接点（a 的局部坐标，米），缺省原点",
            "anchor_b": "关节在 b 上的连接点（b 的局部坐标，米），缺省原点",
            "frame_a": "**固定关节**（type = fixed）的 A 端参考姿态（欧拉角 XYZ，度）；求解器把两端扭到“参考姿态在世界中重合”（q_a⊗frame_a == q_b⊗frame_b）。不写或两端都写 0 = 老行为：锁定创建那一刻的相对姿态",
            "frame_b": "B 端参考姿态（欧拉角 XYZ，度）；与 frame_a 配套。**只要有一端不是 0** 就切到“扭到参考姿态”那一支；只有 fixed 读它",
            "limits": "铰链为 [最小, 最大] 弧度；滑块为 [最小, 最大] 米",
            "rest_length": "distance 关节的静止长度",
            "stiffness": "distance 关节刚度",
            "damping": "distance 关节阻尼",
            "motor_velocity": "铰链马达目标角速度（弧度/秒）——速度模式",
            "motor_max_impulse": "铰链马达单步最大冲量（N·m·s），缺省不限",
            "motor_target_deg": "铰链马达的**伺服目标角**（度）：给了它就转到这个角度停住（伺服模式，优先于 motor_velocity）",
            "motor_stiffness": "伺服比例增益（1/s），缺省 8",
            "motor_damping": "伺服阻尼增益，缺省 1",
            "cone_limit": { "half_angle_deg": "球窝关节允许的两轴最大夹角（度，0~180）",
                            "axis_a": "A 端局部轴，缺省 [0,1,0]", "axis_b": "B 端局部轴，缺省 [0,1,0]",
                            "note": "只有 type = ball 支持" },
            "break_force": "断裂力（N）：本步关节力超过它就断开；不写 = 永不断",
            "break_torque": "断裂力矩（N·m）",
            "collide_connected": "关节两端是否互相碰撞，缺省 false（不碰，和 Bullet/PhysX 一致）"
        },
        "action": {
            "type": ["impulse", "impulse_at_point", "torque_impulse", "velocity",
                     "angular_velocity", "force", "teleport", "wake"],
            "target": "刚体名（objects[].name / $ground / $model[i]）；`wake` 还接受 \"*\" = 唤醒全部睡着的刚体（别的动作写 \"*\" 会报错）",
            "at_step": "第几步开始前施加，缺省 0（第 1 步是第一次物理步进）",
            "at_time": "第几秒开始施加（与 at_step 互斥，按 round(t/dt) 折算成步）",
            "value": "[x,y,z]：冲量 N·s / 力 N / 速度 m/s / 角速度 rad/s / 角冲量 N·m·s",
            "point": "世界坐标作用点，仅 impulse_at_point（用来推门把手、偏心撞击）",
            "position": "目标位置，仅 teleport",
            "rotation_deg": "目标朝向（欧拉角 XYZ，度），仅 teleport，缺省保持当前朝向",
            "steps": "force 持续多少步，缺省 1",
            "note": "写在静态刚体上的冲量/力不会有任何效果（校验会警告）；冲量会自动唤醒睡眠刚体；manual sleep 后 allow_sleep=false 的刚体会在下一步自动醒来"
        },
        "probe": {
            "name": "探针名（报告里用它，必须唯一）",
            "shape": ["sphere（缺省，用 radius）", "box（用 size 边长）"],
            "position": "区域中心（世界坐标）",
            "radius": "球形半径，缺省 1.0",
            "size": "盒形边长，缺省 [1,1,1]",
            "watch": "只观测这些刚体名；缺省观测全部 dynamic / kinematic 刚体",
            "report": "报告里给出每个刚体的 enter_step / exit_step / steps_inside / entries / max_speed",
            "note": "探针只读：不改变物理结果、不唤醒睡眠刚体；半空间（地面）没有有限包围盒，永远判定为不在区域里"
        },
        "camera": {
            "position": "[x,y,z]；与 look_at 一起用会自动对准",
            "look_at": "[x,y,z]，缺省原点",
            "fov_deg": "1~179，缺省 60",
            "distance": "不给 position 时的环绕距离",
            "yaw_deg": "环绕水平角",
            "pitch_deg": "环绕俯仰角",
            "near": "近裁剪面（米），缺省 0.05",
            "far": "远裁剪面（米），缺省 5000，必须大于 near",
            "orthographic_height": "正交投影的可视高度（世界单位）；给了就切正交，缺省透视",
            "frame_scene": "true 时自动框住整个场景"
        },
        "lighting": {
            "preset": ["studio（缺省）", "day", "night", "none"],
            "sun_direction": "光线传播方向 [x,y,z]",
            "sun_intensity": "太阳强度倍率",
            "sun_color": "太阳颜色 [r,g,b]（**线性空间**），缺省跟随预设的暖白",
            "ambient": "环境光强度",
            "ambient_color": "环境光颜色 [r,g,b]（**线性空间**），缺省跟随预设",
            "exposure": "曝光",
            "environment_map": "IBL 环境贴图：等距柱状 .hdr / .png / .jpg（相对场景文件解析）。不写 = 解析式天空（旧场景像素不变）；写了按预滤波 cubemap 采样，太阳/局部光仍叠加",
            "environment_intensity": "环境贴图强度倍率，缺省 1",
            "environment_rotation_deg": "环境贴图绕 Y 轴旋转（度），缺省 0",
            "sky": "是否绘制天空，缺省 true",
            "grid": "是否绘制无限网格，缺省 true",
            "shadows": "是否投射阴影，缺省 true",
            "spot_lights": "聚光灯数组：{ position:[x,y,z], direction:[x,y,z]（会被归一化）, color:[r,g,b]（**线性**）, intensity（缺省 12）, radius（影响半径，缺省 18）, inner_deg（内锥角，缺省 20）, outer_deg（外锥角，缺省 30）, cast_shadows（缺省 **false** = 只照亮不投影）}；与点光源共享 8 盏名额。**投影整帧只支持一盏**：多盏勾上时按数组顺序取第一盏（校验会警告并说明用哪一盏）；点光源是**另一套**阴影图，同样各自只支持一盏",
            "point_lights": "点光源数组：{ position:[x,y,z], color:[r,g,b]（**线性**、非 sRGB）, intensity（缺省 12）, radius（影响半径，缺省 18）, cast_shadows（缺省 **false** = 只照亮不投影）}；最多 8 盏生效。**不写这个字段 = 保留预设自带的补光**，写 [] 才是显式清空。投影用的是 6 个 90° 面的深度数组（立方体阴影），整帧**只支持一盏**：多盏勾上时按数组顺序取第一盏（校验会警告并说明用哪一盏）；它与聚光灯那张 2D 阴影图是两套独立资源，所以「一盏投影点光源 + 一盏投影聚光灯」可以同时存在",
            "fog": "指数距离雾：{ color:[r,g,b]（**线性**、非 sRGB，缺省跟随预设）, density（每米，缺省 0.02，0 = 关闭）}。**不写这一节 = 关闭**（像素与关闭时逐位相同）。天空与参考网格也会跟着淡出；常见 density 0.02~0.1（0.05 大约 20 米外看不清）",
            "shadow_quality": "阴影质量：{ strength（0~1，缺省 0.85；0 = 阴影不压暗，1 = 全黑）, radius（阴影相机覆盖半径，米，缺省 28）, bias（阴影深度偏移，纹素，缺省 0.75，0~4）, resolution（阴影贴图边长 512~4096，缺省 2048；聚光取一半、点光每面再取一半）}。与开关 `shadows` 分开：这一节只调质量。radius 是「覆盖范围 ↔ 精度」的旋钮，给大能让远处也投上阴影但边缘更糊；resolution 同时决定显存与精度",
            "sky_colors": "天空渐变颜色：{ zenith:[r,g,b], horizon:[r,g,b], ground:[r,g,b] }（**线性**、非 sRGB）。只覆盖写了的那几个通道，没写 = 跟随 preset。sky:false 时没有可见效果",
            "bloom": "泛光：{ enabled（缺省 true）, intensity（缺省 0.6）, threshold（缺省 1.0，线性 Rec.709 亮度）}。**不写这一节 = 关闭**（像素与关闭时逐位相同）。光照/自发光先落到线性 HDR 中间目标，超阈值的部分做半分辨率高斯模糊，再在 tonemap **之前**加回去——所以亮区外溢不会过曝成白块。想让太阳/自发光有「发光感」就写 { threshold: 1.0, intensity: 0.6 }",
            "ssao": "环境光遮蔽：{ enabled（缺省 true）, radius（缺省 0.75，单位**米**）, bias（缺省 0.025）, strength（缺省 1.0）, samples（缺省 16，1~64）, denoise（缺省 false，4 邻域双边去噪，AO 计算量 ×5）}。**不写这一节 = 关闭**（像素与关闭时逐位相同）。让物体与地面的接触缝隙、墙角变暗——廉价版的接触阴影。深度-only 实现（法线由深度重建），结果乘在整幅 HDR 颜色上（不区分直接光/间接光），这是后处理式 SSAO 的固有简化。物体尺度 1~2 米时 radius 用 0.5~1.0",
            "taa": "时域抗锯齿：{ enabled（缺省 true）, feedback（历史权重 0~0.98，缺省 0.9；越大越平滑也越容易拖影）}。**不写这一节 = 关闭**。相机每帧亚像素抖动 + 上一帧历史按深度重投影累积；**只在能读深度且未开 MSAA 的原生后端生效**（GL/WebGL2 与 msaa>1 忽略）",
            "fxaa": "后处理抗锯齿：{ enabled（缺省 true）, edge_threshold（缺省 0.166，越小越敏感）, edge_threshold_min（缺省 0.0833）}。**不写这一节 = 关闭**（像素与关闭时逐位相同）。它在 tonemap **之后**的画面（不是线性 HDR）上找感知边缘，沿边缘方向做一次定向模糊；平坦区域逐位不变。与 render.msaa 不冲突：MSAA 管几何边缘，FXAA 管着色噪声与剩下的锯齿，代价是细节略微变软。斜边多的场景写 {} 就够了",
            "dof": "深度景深：{ enabled（缺省 true）, focus（对焦距离，视空间米，缺省 2.0）, aperture（光圈强度，缺省 0.4，0 = 不模糊）, max_blur（最大模糊半径像素，缺省 8.0，常用 4~16）}。**不写这一节 = 关闭**（像素与关闭时一致）。后处理式：按像素到焦平面的距离做 16 tap 圆盘模糊，天空不参与；**只在能读深度纹理的后端生效**（原生 wgpu），GL/WebGL2 会忽略这一节",
            "grade": "调色（tonemap 之后）：{ saturation（0~4，1 = 不变，0 = 灰度）, contrast（0~4，1 = 不变，以 0.5 为轴）, temperature（-1~1，负数偏冷、正数偏暖）}。**不写这一节 = 全中性**（像素与关闭时逐位相同）"
        },
        "simulate": { "steps": "步数，缺省 0", "dt": "单步秒数，缺省 1/60" },
        "render": {
            "output": "PNG 路径，缺省 output.png",
            "width": "缺省 960",
            "height": "缺省 600",
            "msaa": "1/2/4/8，缺省 4",
            "enabled": "缺省 true"
        }
    })
}

/// 机器可读的 JSON Schema（draft 2020-12）：给编辑器补全、CI 校验、agent 程序化检查用。
///
/// **手写 + 一致性测试**（不引 `schemars` 之类的新依赖）：`json_schema_matches_the_source`
/// 会拿 `scene_doc.rs` 的结构体字段名与这里声明的属性名逐项对账——加字段忘了改这里会红。
/// 分工要说清楚：**schema 管"结构"**（哪些键、嵌套长什么样、`additionalProperties: false`），
/// **`quench3d validate` 管"语义"**（数值范围、互斥、交叉引用）。所以这里的值类型是宽松的
/// （`true` = 任意），不要把"schema 过了"当成"场景能跑"。
pub fn json_schema() -> Value {
    fn def(fields: &[&str]) -> Value {
        let mut props = serde_json::Map::new();
        for f in fields {
            props.insert((*f).to_string(), Value::Bool(true));
        }
        json!({ "type": "object", "additionalProperties": false, "properties": props })
    }
    fn with_type(mut d: Value, ty: Value) -> Value {
        d["properties"]["type"] = ty;
        d
    }
    let mut defs = serde_json::Map::new();
    defs.insert(
        "vec3".into(),
        json!({ "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3,
                "description": "[x, y, z]" }),
    );
    for (name, variants) in [
        (
            "bodyKind",
            ["none", "static", "dynamic", "kinematic"].as_slice(),
        ),
        (
            "shapeKind",
            [
                "box",
                "sphere",
                "capsule",
                "cylinder",
                "cone",
                "rounded_box",
                "heightfield",
                "plane",
                "convex",
            ]
            .as_slice(),
        ),
        (
            "jointKind",
            ["fixed", "ball", "hinge", "distance", "slider"].as_slice(),
        ),
        (
            "actionKind",
            [
                "impulse",
                "impulse_at_point",
                "torque_impulse",
                "velocity",
                "angular_velocity",
                "force",
                "teleport",
                "wake",
            ]
            .as_slice(),
        ),
        ("probeShapeKind", ["sphere", "box"].as_slice()),
        (
            "combineRule",
            ["geometric_mean", "average", "min", "max", "multiply"].as_slice(),
        ),
    ] {
        defs.insert(name.into(), json!({ "type": "string", "enum": variants }));
    }
    defs.insert(
        "shape".into(),
        with_type(
            def(&[
                "type",
                "size",
                "radius",
                "half_height",
                "normal",
                "points",
                "heights",
                "cell_size",
            ]),
            json!({ "$ref": "#/$defs/shapeKind" }),
        ),
    );
    defs.insert(
        "shapePart".into(),
        def(&["shape", "position", "rotation_deg"]),
    );
    defs.insert(
        "instance".into(),
        def(&["name", "position", "rotation_deg", "scale"]),
    );
    defs.insert(
        "object".into(),
        def(&[
            "name",
            "template",
            "instances",
            "shape",
            "shapes",
            "parent",
            "position",
            "rotation_deg",
            "scale",
            "material",
            "body",
            "density",
            "restitution",
            "friction",
            "static_friction",
            "friction_combine",
            "restitution_combine",
            "velocity",
            "angular_velocity",
            "linear_damping",
            "angular_damping",
            "gravity_scale",
            "mass",
            "allow_sleep",
            "visible",
            "cast_shadows",
            "receive_shadows",
            "collision_layer",
            "collides_with",
            "sensor",
            "ccd",
        ]),
    );
    defs.insert(
        "material".into(),
        def(&[
            "color",
            "metallic",
            "roughness",
            "emissive",
            "alpha",
            "emissive_color",
            "uv_scale",
            "uv_offset",
            "uv_rotation",
            "base_color_texture",
            "normal_texture",
            "metallic_roughness_texture",
            "emissive_texture",
            "ao_texture",
            "ao_strength",
            "normal_scale",
            "alpha_cutoff",
            "alpha_blend",
            "unlit",
            "double_sided",
        ]),
    );
    defs.insert(
        "ground".into(),
        def(&["size", "material", "y", "friction", "restitution"]),
    );
    defs.insert("water".into(), {
        let mut d = def(&["center", "size", "level", "density", "drag"]);
        d["description"] = json!(
            "一块水域（浮力环境）：y <= level 且在 XZ 范围内的刚体按排开体积受浮力；不是流体求解器"
        );
        d["required"] = json!(["size", "level"]);
        d
    });
    defs.insert("waterStyle".into(), {
        let mut d = def(&["color", "clarity", "wave_strength", "foam", "refraction"]);
        d["description"] = json!("屏幕空间水外观：所有 waters/fluids 共用；只影响画面，不影响物理");
        d["properties"]["color"] = json!({
            "type": "array", "items": { "type": "number", "minimum": 0, "maximum": 1 },
            "minItems": 3, "maxItems": 3
        });
        d["properties"]["clarity"] = json!({ "type": "number", "minimum": 0, "maximum": 1 });
        d["properties"]["wave_strength"] = json!({ "type": "number", "minimum": 0, "maximum": 4 });
        d["properties"]["foam"] = json!({ "type": "number", "minimum": 0, "maximum": 2 });
        d["properties"]["refraction"] = json!({ "type": "number", "minimum": 0, "maximum": 2 });
        d
    });
    defs.insert("fluid".into(), {
        let mut d = def(&[
            "center",
            "size",
            "spacing",
            "radius",
            "rest_density",
            "viscosity",
            "drag",
            "iterations",
            "max_particles",
        ]);
        d["description"] = json!(
            "一个 CPU PBF 粒子流体：长方体发射器 + 粒子参数；粒子与动态刚体双向耦合（冲量 + 浮力/阻尼）"
        );
        d["required"] = json!(["size"]);
        d
    });
    defs.insert("decal".into(), {
        let mut d = def(&["position", "normal", "size", "rotation", "color", "texture"]);
        d["description"] =
            json!("一个平面贴花（纯视觉）：四边形按 normal 朝向、沿法线抬 1cm；不做深度投影");
        d["properties"]["position"] = json!({ "$ref": "#/$defs/vec3" });
        d["properties"]["normal"] = json!({ "$ref": "#/$defs/vec3" });
        d["properties"]["size"] = json!({ "type": "array", "minItems": 2, "maxItems": 2,
            "items": { "type": "number", "exclusiveMinimum": 0 } });
        d["properties"]["rotation"] = json!({ "type": "number" });
        d["properties"]["color"] = json!({ "type": "array", "minItems": 4, "maxItems": 4,
            "items": { "type": "number", "minimum": 0, "maximum": 1 } });
        d["properties"]["color_end"] = json!({ "type": "array", "minItems": 4, "maxItems": 4,
            "items": { "type": "number", "minimum": 0, "maximum": 1 } });
        d["properties"]["texture"] = json!({ "type": "string" });
        d
    });
    defs.insert("particle".into(), {
        let mut d = def(&[
            "count",
            "enabled",
            "position",
            "radius",
            "shape",
            "box_size",
            "velocity",
            "spread",
            "drag",
            "rotation",
            "rotation_end",
            "alignment",
            "lifetime",
            "size",
            "size_end",
            "gravity",
            "color",
            "color_end",
            "texture",
        ]);
        d["description"] =
            json!("一个 CPU 公告板粒子发射器（纯视觉，不参与物理）：粒子面向相机、按寿命淡出");
        d["properties"]["count"] = json!({ "type": "integer", "minimum": 1, "maximum": 4096 });
        d["properties"]["enabled"] = json!({ "type": "boolean" });
        for name in ["position", "velocity", "gravity"] {
            d["properties"][name] = json!({ "$ref": "#/$defs/vec3" });
        }
        d["properties"]["shape"] =
            json!({ "type": "string", "enum": ["sphere", "box", "disc", "cone"] });
        d["properties"]["box_size"] = json!({ "$ref": "#/$defs/vec3" });
        for name in ["radius", "spread", "drag"] {
            d["properties"][name] = json!({ "type": "number", "minimum": 0 });
        }
        for name in ["lifetime", "size", "size_end"] {
            d["properties"][name] = json!({ "type": "number", "exclusiveMinimum": 0 });
        }
        for name in ["rotation", "rotation_end"] {
            d["properties"][name] = json!({ "type": "number" });
        }
        d["properties"]["alignment"] =
            json!({ "type": "string", "enum": ["billboard", "velocity"] });
        d["properties"]["color"] = json!({
            "type": "array", "items": { "type": "number", "minimum": 0, "maximum": 1 },
            "minItems": 4, "maxItems": 4
        });
        d
    });
    defs.insert("model".into(), {
        let mut d = def(&[
            "path",
            "position",
            "rotation_deg",
            "scale",
            "up_axis",
            "collide",
            "friction",
            "morph_weights",
            "lods",
            "ik",
            "animator",
            "animation",
            "events",
        ]);
        d["properties"]["up_axis"] = json!({
            "type": "string",
            "enum": ["y", "z"],
            "description": "资产的上轴，缺省 y；z 会在用户旋转之前绕 X 轴 -90°"
        });
        d["properties"]["morph_weights"] = json!({
            "type": "array",
            "items": { "type": "number", "minimum": 0.0, "maximum": 1.0 }
        });
        d["properties"]["lods"] = json!({
            "type": "array",
            "items": { "$ref": "#/$defs/modelLod" }
        });
        d["properties"]["ik"] = json!({
            "type": "array",
            "items": { "$ref": "#/$defs/modelIk" }
        });
        d["properties"]["animator"] = json!({ "$ref": "#/$defs/modelAnimator" });
        d["properties"]["animation"] = json!({ "$ref": "#/$defs/modelAnimation" });
        d["properties"]["events"] = json!({
            "type": "array",
            "items": { "$ref": "#/$defs/modelEvent" }
        });
        d
    });
    defs.insert("modelEvent".into(), {
        let mut d = def(&["name", "time"]);
        d["properties"]["time"] = json!({ "type": "number", "minimum": 0.0 });
        d["properties"]["clip"] = json!({ "type": "string", "minLength": 1 });
        d["properties"]["payload"] = json!({ "type": "string", "maxLength": 1024 });
        d["required"] = json!(["name", "time"]);
        d
    });
    defs.insert("modelLod".into(), {
        let mut d = def(&["path", "min_distance"]);
        d["properties"]["min_distance"] = json!({ "type": "number", "exclusiveMinimum": 0.0 });
        d["required"] = json!(["path", "min_distance"]);
        d
    });
    defs.insert("modelIk".into(), {
        let mut d = def(&["root", "mid", "tip", "target", "pole", "weight"]);
        d["required"] = json!(["root", "mid", "tip", "target"]);
        d["properties"]["target"] = json!({
            "type": "array",
            "items": { "type": "number" },
            "minItems": 3,
            "maxItems": 3
        });
        d["properties"]["pole"] = json!({
            "type": "array",
            "items": { "type": "number" },
            "minItems": 3,
            "maxItems": 3
        });
        d["properties"]["weight"] = json!({
            "type": "number",
            "minimum": 0.0,
            "maximum": 1.0
        });
        d
    });
    defs.insert("modelAnimation".into(), {
        let mut d = def(&["name", "start", "speed", "loop", "blend", "root_motion"]);
        d["properties"]["blend"] = json!({ "$ref": "#/$defs/modelAnimationBlend" });
        d["properties"]["root_motion"] = json!({
            "type": "string",
            "minLength": 1,
            "description": "把该 glTF 节点的三维位移与绕 Y 偏航提取到整个物体上；要求 loop=false，且不能与 blend/animator/ik 同用"
        });
        d["required"] = json!(["name"]);
        d
    });
    defs.insert("modelAnimationBlend".into(), {
        let mut d = def(&["name", "start", "speed", "loop", "weight", "fade"]);
        d["required"] = json!(["name"]);
        d
    });
    defs.insert("modelAnimator".into(), {
        let mut d = def(&[
            "initial",
            "states",
            "parameters",
            "transitions",
            "event_window",
        ]);
        d["properties"]["event_window"] = json!({
            "type": "number",
            "minimum": 0.0,
            "maximum": 10.0,
            "default": 0.0,
            "description": "输入事件有效窗口（秒），0 = 不过期"
        });
        d["properties"]["states"] = json!({
            "type": "array",
            "minItems": 1,
            "items": { "$ref": "#/$defs/modelAnimatorState" }
        });
        d["properties"]["parameters"] = json!({
            "type": "array",
            "items": { "$ref": "#/$defs/modelAnimatorParameter" }
        });
        d["properties"]["transitions"] = json!({
            "type": "array",
            "items": { "$ref": "#/$defs/modelAnimatorTransition" }
        });
        d["required"] = json!(["initial", "states"]);
        d
    });
    defs.insert("modelAnimatorState".into(), {
        let mut d = def(&["name", "animation", "speed", "loop", "start"]);
        d["required"] = json!(["name", "animation"]);
        d
    });
    defs.insert("modelAnimatorParameter".into(), {
        let mut d = def(&["name", "default"]);
        d["required"] = json!(["name"]);
        d
    });
    defs.insert("modelAnimatorCondition".into(), {
        let mut d = def(&["parameter", "op", "value"]);
        d["properties"]["op"] = json!({
            "type": "string",
            "enum": ["greater", "greater_or_equal", "less", "less_or_equal", "equal", "not_equal"]
        });
        d["required"] = json!(["parameter", "op", "value"]);
        d
    });
    defs.insert("modelAnimatorTransition".into(), {
        let mut d = def(&[
            "from",
            "to",
            "after",
            "on_finished",
            "fade",
            "conditions",
            "on_event",
        ]);
        d["properties"]["after"] = json!({ "type": "number", "minimum": 0.0 });
        d["properties"]["on_finished"] = json!({ "type": "boolean", "default": false });
        d["properties"]["conditions"] = json!({
            "type": "array",
            "items": { "$ref": "#/$defs/modelAnimatorCondition" }
        });
        d["properties"]["on_event"] = json!({ "type": "string", "minLength": 1 });
        d["required"] = json!(["from", "to"]);
        d
    });
    defs.insert("assets".into(), def(&["search_paths"]));
    defs.insert(
        "coneLimit".into(),
        def(&["half_angle_deg", "axis_a", "axis_b"]),
    );
    defs.insert(
        "joint".into(),
        with_type(
            def(&[
                "type",
                "a",
                "b",
                "axis",
                "axis_a",
                "axis_b",
                "anchor_a",
                "anchor_b",
                "frame_a",
                "frame_b",
                "limits",
                "rest_length",
                "stiffness",
                "damping",
                "motor_velocity",
                "motor_max_impulse",
                "motor_target_deg",
                "motor_stiffness",
                "motor_damping",
                "cone_limit",
                "break_force",
                "break_torque",
                "collide_connected",
            ]),
            json!({ "$ref": "#/$defs/jointKind" }),
        ),
    );
    defs.insert(
        "action".into(),
        with_type(
            def(&[
                "type",
                "target",
                "at_step",
                "at_time",
                "value",
                "point",
                "position",
                "rotation_deg",
                "steps",
            ]),
            json!({ "$ref": "#/$defs/actionKind" }),
        ),
    );
    // 探针的形状字段叫 `shape`（不是 `type`）：`{ "shape": "sphere" | "box" }`
    let mut probe = def(&["name", "shape", "position", "radius", "size", "watch"]);
    probe["properties"]["shape"] = json!({ "$ref": "#/$defs/probeShapeKind" });
    defs.insert("probe".into(), probe);
    defs.insert(
        "camera".into(),
        def(&[
            "position",
            "look_at",
            "fov_deg",
            "distance",
            "yaw_deg",
            "pitch_deg",
            "near",
            "far",
            "orthographic_height",
            "frame_scene",
        ]),
    );
    defs.insert(
        "pointLight".into(),
        def(&["position", "color", "intensity", "radius", "cast_shadows"]),
    );
    defs.insert(
        "spotLight".into(),
        def(&[
            "position",
            "direction",
            "color",
            "intensity",
            "radius",
            "inner_deg",
            "outer_deg",
            "cast_shadows",
        ]),
    );
    defs.insert("fog".into(), def(&["color", "density"]));
    defs.insert(
        "shadowQuality".into(),
        def(&[
            "strength",
            "radius",
            "bias",
            "resolution",
            "cascades",
            "split_lambda",
        ]),
    );
    defs.insert("skyColors".into(), def(&["zenith", "horizon", "ground"]));
    defs.insert(
        "ssao".into(),
        def(&[
            "enabled", "radius", "bias", "strength", "samples", "denoise",
        ]),
    );
    defs.insert(
        "fxaa".into(),
        def(&["enabled", "edge_threshold", "edge_threshold_min"]),
    );
    defs.insert("taa".into(), def(&["enabled", "feedback"]));
    defs.insert("bloom".into(), def(&["enabled", "intensity", "threshold"]));
    defs.insert(
        "dof".into(),
        def(&["enabled", "focus", "aperture", "max_blur"]),
    );
    defs.insert(
        "grade".into(),
        def(&["saturation", "contrast", "temperature"]),
    );
    defs.insert(
        "lighting".into(),
        def(&[
            "preset",
            "sun_direction",
            "sun_intensity",
            "sun_color",
            "ambient",
            "ambient_color",
            "exposure",
            "environment_map",
            "environment_intensity",
            "environment_rotation_deg",
            "sky",
            "grid",
            "shadows",
            "fog",
            "shadow_quality",
            "sky_colors",
            "point_lights",
            "spot_lights",
            "bloom",
            "fxaa",
            "taa",
            "ssao",
            "dof",
            "grade",
        ]),
    );
    defs.insert(
        "physics".into(),
        def(&[
            "friction_combine",
            "restitution_combine",
            "static_friction_speed",
            "speculative_margin",
        ]),
    );
    defs.insert("simulate".into(), def(&["steps", "dt"]));
    defs.insert("track".into(), def(&["times", "values"]));
    defs.insert(
        "animation".into(),
        def(&[
            "name", "target", "start", "speed", "loop", "weight", "position", "rotation",
        ]),
    );
    defs.insert(
        "renderOutput".into(),
        def(&["name", "camera", "width", "height"]),
    );
    defs.insert(
        "render".into(),
        def(&[
            "output", "width", "height", "msaa", "enabled", "outputs", "auto_lod",
        ]),
    );
    defs.insert(
        "scene".into(),
        json!({
            "type": "object",
            "additionalProperties": false,
            "description": "quench3d.scene：一个自包含的 3D 场景 / 物理实验描述",
            "properties": {
                "format": { "const": "quench3d.scene" },
                "version": { "type": "integer", "minimum": 1,
                             "description": "当前 2；版本 1 的文件只有 objects[].parent 不生效" },
                "name": { "type": "string" },
                "gravity": { "$ref": "#/$defs/vec3" },
                "physics": { "$ref": "#/$defs/physics" },
                "ground": { "$ref": "#/$defs/ground" },
                "waters": { "type": "array", "items": { "$ref": "#/$defs/water" },
                            "description": "水域（浮力环境）：只影响动态刚体的浮力/阻尼，不参与碰撞；不写 = 没有水" },
                "fluids": { "type": "array", "items": { "$ref": "#/$defs/fluid" },
                            "description": "CPU PBF 粒子流体：粒子与动态刚体双向耦合（冲量 + 浮力/阻尼）；不写 = 没有粒子" },
                "particles": { "type": "array", "items": { "$ref": "#/$defs/particle" },
                               "description": "CPU 公告板粒子：面向相机、按寿命淡出；纯视觉，不参与物理；不写 = 没有粒子" },
                "decals": { "type": "array", "items": { "$ref": "#/$defs/decal" },
                            "description": "平面贴花：按 normal 朝向的四边形（沿法线抬 1cm），纯视觉；不写 = 没有贴花" },
                "water": { "$ref": "#/$defs/waterStyle",
                           "description": "屏幕空间水外观（颜色/清澈度/波浪/泡沫/折射）；只影响画面，不影响物理；根独有段" },
                "materials": { "type": "object",
                               "additionalProperties": { "$ref": "#/$defs/material" } },
                "include": { "type": "array", "items": { "type": "string" },
                             "description": "被包含的外部场景片段：只贡献可合并的段（材质/模板/相机/水域/流体/物体/模型/关节/动作/探针/动画），具名表重名报错、数组按「自己的在前」追加；相对路径以写这一行的文件所在目录为基准，片段的资源路径按片段自己的目录重定基；最多 16 层，成环报 include_cycle" },
                "templates": { "type": "object",
                               "additionalProperties": { "$ref": "#/$defs/object" },
                               "description": "可复用的物体描述；objects[].template 引用它" },
                "objects": { "type": "array", "items": { "$ref": "#/$defs/object" } },
                "models": { "type": "array", "items": { "$ref": "#/$defs/model" } },
                "assets": { "$ref": "#/$defs/assets" },
                "joints": { "type": "array", "items": { "$ref": "#/$defs/joint" } },
                "actions": { "type": "array", "items": { "$ref": "#/$defs/action" } },
                "probes": { "type": "array", "items": { "$ref": "#/$defs/probe" } },
                "camera": { "$ref": "#/$defs/camera" },
                "animations": { "type": "array",
                                "items": { "$ref": "#/$defs/animation" },
                                "description": "关键帧动画：按时间推着物体走（运动学）" },
                "cameras": { "type": "object",
                             "additionalProperties": { "$ref": "#/$defs/camera" },
                             "description": "命名相机：--camera <名字> 选一台渲染" },
                "lighting": { "$ref": "#/$defs/lighting" },
                "simulate": { "$ref": "#/$defs/simulate" },
                "render": { "$ref": "#/$defs/render" }
            }
        }),
    );
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://github.com/gh503/quench3d/scene-v2.schema.json",
        "title": "quench3d.scene v2",
        "type": "object",
        "additionalProperties": false,
        "properties": {},
        "allOf": [ { "$ref": "#/$defs/scene" } ],
        "$defs": defs
    })
}

/// 给大模型看的**系统提示**：把场景格式讲清楚（字段、单位、边界）。
///
/// 与 [`schema`] / [`json_schema`] 保持同一份事实：字段名靠
/// `system_prompt_mentions_every_top_level_scene_field` 测试盯着。
pub fn system_prompt() -> String {
    r#"你是 Quench3D 引擎的场景生成器。用户会用自然语言描述一个 3D 场景或物理实验，
你只输出一个 JSON 对象（不要解释、不要 Markdown 代码块以外的文字），格式是 quench3d.scene v2。

顶层字段（全部可选）：
- name: 字符串
- gravity: [x, y, z]，缺省 [0, -9.81, 0]
- ground: { size: 40, material: "floor" }   // 地面是无限大静态平面
- waters: [{ center: [0, 0], size: [10, 6], level: 0.0, density: 1000, drag: 6 }]
  // 水域（浮力环境，M1）：y <= level 且在 XZ 范围内算浸没；动态刚体按排开体积受浮力，
  // 睡眠体不受力。没有粒子/流动/波浪；不写 = 没有水。
- fluids: [{ center: [0, 2, 0], size: [2, 2, 2], spacing: 0.1, radius: 0.05,
            rest_density: 1000, viscosity: 0.05, drag: 6, iterations: 4,
            max_particles: 4000 }]
  // CPU PBF 粒子流体（M3 双向）：长方体发射器 + 粒子参数。粒子被刚体推开并反推冲量；
  // 没有表面张力/涡量；渲染为低模球 + 屏幕空间水面（M4.2）。不写 = 没有粒子。
- particles: [{ count: 64, position: [0, 1, 0], radius: 0.1,
                velocity: [0, 1.2, 0], spread: 0.4, lifetime: 2.0,
                size: 0.12, gravity: [0, -1, 0],
                color: [1.0, 0.65, 0.2, 0.8] }]
  // CPU 公告板粒子（纯视觉）：每颗粒子是一个面向相机的四边形，按寿命淡出后
  // 回到发射器重生；没有贴图、没有逐像素排序/软粒子/GPU 粒子。不写 = 没有粒子。
- water: { color: [r, g, b]（线性 0~1）, clarity: 0~1, wave_strength: 0~4,
           foam: 0~2, refraction: 0~2 }
  // 屏幕空间水外观（M4.2）：所有 waters/fluids 共用一份风格；只影响画面，不影响物理。
  // 不写 = 缺省（color [0.06,0.28,0.48]、clarity/wave/foam/refraction 都是 1）。
- materials: { 名字: { color: [r,g,b]（sRGB）, metallic: 0~1, roughness: 0.05~1, emissive: 0,
             alpha: 0~1, emissive_color: [r,g,b], uv_scale: 1, normal_scale: 1,
             alpha_cutoff: 0, alpha_blend: false, unlit: false, double_sided: false,
             base_color_texture: "a.png", normal_texture, metallic_roughness_texture, emissive_texture } }
  （贴图路径相对场景文件解析；alpha_cutoff 与 alpha_blend 同时给时裁剪优先）
- templates: { 名字: { ...与物体相同的字段... } }   // 可复用的一段描述（省 token）
  物体写 "template": "名字" 引用；物体自己写的字段覆盖模板
  （模板里写 name/parent/position/rotation_deg 没有意义，会被忽略并警告）
- include: ["parts/wall.json", "../shared/common.json"]   // 跨文件复用（可选）
  被包含的片段只贡献可合并的段（materials/templates/cameras + waters/fluids/particles/objects/models/
  joints/actions/probes/animations）：具名表重名 = duplicate_name 错误（不静默覆盖），
  数组按声明顺序追加（**本文件自己的内容在前**，被包含的在后）。
  根独有的段（ground/camera/lighting/simulate/render/gravity/physics/name/assets）
  写在片段里是错误 —— 这些段只有一个实例，谁赢没有答案，所以宁可报错也不含糊地合并。
  相对路径以**写这一行的文件**所在目录为基准；片段里的贴图/模型路径按片段自己的目录解析
  （装载时重定基一次，不会因为被谁包含而指错地方）。
  片段可以再 include（最多 16 层，同一文件只并入一次、并在 diagnostics 里给一条
  ineffective 警告，成环 = include_cycle 错误）。
  跨文件 prefab：片段只放 templates，根场景用 objects[].template + instances 实例化。
- objects: 数组，每项：
  - name: 字符串（关节引用与报告用它）
  - template: 模板名（可选）；instances: [{position, rotation_deg, scale, name}]（可选）
    一次摆多个副本 = 多个**独立**刚体；缺省名字 `本名#1`、`本名#2`…（只有一个副本时是本名）
  - shape: { type: "box"|"sphere"|"capsule"|"cylinder"|"cone"|"rounded_box"|"heightfield"|"plane"|"convex",
             size: [x,y,z]（box / rounded_box 的**边长**）, radius, half_height, normal, points,
             radius（rounded_box 必填：圆角半径，≤ 最短边长的一半）,
             heights: [[…]]（heightfield：行主序，行沿 Z、列沿 X，至少 2×2；只能 static）,
             cell_size（heightfield 的格子边长，缺省 1.0） }
  - parent: 父物体的 name（可选）。给了之后 position/rotation_deg 是**相对父物体**的
            （父物体动，子物体跟着动；只继承位置与朝向，不继承缩放；
             带刚体的物体不能有父物体）。父物体必须显式写 name
  - position: [x,y,z]（米，有 parent 时是相对父物体的）, rotation_deg: [x,y,z]（度）
  - material: 材质名
  - body: "none"|"static"|"dynamic"|"kinematic"（缺省 none 表示纯显示，不参与物理）
  - density: kg/m³（缺省 1000）, restitution: 0~1（缺省 0.2）, friction:（动摩擦，缺省 0.5）
  - static_friction（缺省跟随 friction）, friction_combine / restitution_combine（覆盖 world.physics）
- physics: { friction_combine: "min", restitution_combine: "max", static_friction_speed: 0.02 }
  （世界缺省；不写 = 引擎缺省。`friction_combine` 缺省 geometric_mean、`restitution_combine`
   缺省 max —— 这两个缺省值就是老行为，改了会改变已有场景的观感）
  - velocity: [x,y,z] 初始速度, angular_velocity: [x,y,z]（弧度/秒）
  - linear_damping / angular_damping（≥0，缺省 0）, gravity_scale（缺省 1，0 表示失重）
  - mass（kg，覆盖 density×体积）, allow_sleep: false（永不自动睡眠）
  - collision_layer: 0~31（缺省 0）, collides_with: [层号...]（缺省所有层；**对称**匹配，
    A 愿意碰 B 且 B 也愿意碰 A 才碰；[] = 谁也不碰）
  - sensor: true（触发区：照常进 contacts 与"进入/离开"报告，但不产生任何冲量）
  - ccd: true（连续碰撞检测：每子步从刚体中心沿运动方向打一条射线，把这一步的位移截在
    命中点；高速小物体不会一步穿过薄墙。没有旋转扫掠，只扫中心线——擦角可能漏。只对 dynamic 生效）
  - 上面这些物理参数（mass/gravity_scale/damping/allow_sleep/angular_velocity/ccd）只对 body: "dynamic" 生效
- joints: 数组，每项：{ type: "fixed"|"ball"|"hinge"|"distance"|"slider", a: 物体名, b: 物体名,
           cone_limit: { half_angle_deg, axis_a, axis_b }（仅 ball）,
           motor_target_deg / motor_stiffness / motor_damping（铰链伺服）,
           break_force / break_torque（超过就断）,
           axis: [x,y,z]（**各自刚体的局部轴**，缺省 [0,1,0]）,
           axis_a / axis_b: [x,y,z]（**按端**给的局部轴；两端初始朝向不同时必须用它俩——
             同一个局部轴在两端会指向不同的世界方向，求解器会先把 B 扭到与 A 对齐。不写 = 用 axis）,
           limits: [min,max]（铰链弧度 / 滑块米）,
           anchor_a / anchor_b: [x,y,z]（关节在两端的**局部**连接点，做门轴/悬臂时错开它们）,
           frame_a / frame_b: [x,y,z]（欧拉角度；**只有 fixed 读**，是两端的参考姿态：
              求解器把两端扭到参考姿态在世界中重合，即让 B 的 frame_b 转到与 A 的 frame_a 同朝向。
              不写、或两端都写 0 = 老行为：锁定创建那一刻的相对姿态，参考姿态不参与求解）,
           rest_length, stiffness, damping, motor_velocity }
- actions: 数组，每项：{ type: "impulse"|"impulse_at_point"|"torque_impulse"|"velocity"|
           "angular_velocity"|"force"|"teleport"|"wake", target: 刚体名（wake 可以写 "*" = 全部）,
           at_step: 60（或 at_time: 1.0 秒，二选一）, value: [x,y,z], point, position,
           rotation_deg, steps }
  （冲量 N·s / 力 N / 速度 m/s；用来表达"第 2 秒推一下""到点了就松手"这类剧本。
   注意质量默认由 density×体积 推出：体积 0.5 的球密度 1000 → 500 kg，
   6 N·s 的冲量只有 0.012 m/s，想推得动就写 mass 或给大一点的冲量）
- probes: 数组，每项：{ name: "落点", shape: "sphere"|"box", position: [x,y,z],
           radius: 1.0（球形）, size: [1,1,1]（盒形）, watch: ["球"] }
  （只读区域监测：报告里给出谁在第几步进入、第几步离开、区内最高速度。
   半空间（地面）没有有限包围盒，探针监测不到它）
- models: 数组：{ path: "xx.glb"|"xx.obj", position, rotation_deg, scale, collide: false,
           morph_weights: [0.0, 0.5],
           animation: { name: "Walk", start: 0, speed: 1, loop: true,
                        blend: { name: "Run", weight: 0.5, fade: 0.25 } },
           events: [ { name: "footstep", time: 0.3, payload: "step=1" } ] }
  （animation 播放模型里的 glTF 动画片段，是**视觉**动画、不动物理；不写 = 静止。
    loop 缺省 true（循环），写 false = 播完停在最后一帧；名字写错不会让场景非法，
    但会在 simulate/render 报告的 models[].animation.found 里报 false ——
    可用名字看 `quench3d info 模型.glb` 的 animations。可选 blend 再给第二条 clip，
    两条按 weight（0..=1，缺省 0.5）在局部 TRS 上混合，适合行走↔奔跑的固定比例过渡。
    events 给动画播放头挂事件：{ name, time, clip?, payload? }，跨过 time 秒时触发；
    clip 不写 = 主 clip，写了只跟同名 clip；需要 animation，animator 暂不支持）
- camera: { position: [x,y,z], look_at: [x,y,z], fov_deg: 50, near: 0.05, far: 5000,
cameras: 命名相机表（名字 → 相机对象）；`--camera <名字>` 选一台渲染，缺省用 camera 段
animations: [{ target, loop?, start?, speed?, weight?, position?: {times,values}, rotation?: {times,values} }]（关键帧动画；缺省空）
            orthographic_height: 5, frame_scene: false }
  或者 { distance: 12, yaw_deg: 30, pitch_deg: 20, look_at: [0,1,0] }
  （给了 orthographic_height 就是正交投影，没有近大远小）
- lighting: { preset: "studio"|"day"|"night", sun_direction: [x,y,z], sun_intensity, ambient, exposure, sky: true, grid: true, shadows: true,
             point_lights: [ { position: [x,y,z], color: [r,g,b]（线性）, intensity: 12, radius: 18, cast_shadows: false } ],
             spot_lights: [ { position: [x,y,z], direction: [0,-1,0], intensity: 12, radius: 18, inner_deg: 20, outer_deg: 30,
                              cast_shadows: false } ],
             fog: { color: [r,g,b]（线性）, density: 0.03 },
             shadow_quality: { strength: 0.85, radius: 28 },
             sky_colors: { zenith: [r,g,b], horizon: [r,g,b], ground: [r,g,b] }（线性）,
             bloom: { enabled: true, intensity: 0.6, threshold: 1.0 },
             fxaa: { enabled: true, edge_threshold: 0.166 },
             ssao: { enabled: true, radius: 0.75, strength: 1.0 } }
  （point_lights + spot_lights 共享 8 盏名额，点光源优先；不写 point_lights 就保留预设自带补光；
    cast_shadows 缺省 false（只照亮不投影）；点光源与聚光灯**各自**整帧只支持一盏投影灯——多盏勾上时
    按数组顺序取第一盏，校验会警告说明实际用哪一盏；两套阴影图互相独立，所以可以同时各有一盏。
    bloom / fxaa / fog 不写 = 关闭；
    threshold 是线性亮度，太阳圆盘/自发光可以远大于 1，所以 1.0 通常只让它们外溢；
    fxaa 是 tonemap 之后的后处理抗锯齿，和 render.msaa 可以同时开；
    ssao 让接触缝隙/墙角变暗，radius 单位是米，物体尺度 1~2 米时给 0.5~1.0；
    fog 让远处往雾色里淡出（density 0.02~0.1，线性色不是 sRGB）；
    shadow_quality 只调阴影质量（强度 0~1、覆盖半径米），开关仍是 shadows）
- simulate: { steps: 180, dt: 0.0166667 }
- render: { output: "scene.png", width: 960, height: 600, msaa: 4, auto_lod: false }
  // auto_lod=true 时高面数非蒙皮网格在上传时生成 1/2、1/4 两档，按距离切换；缺省 false。

硬性要求：
1. 单位：长度米、时间秒、角度度（字段名带 _deg）；重力是 y 轴向下。
2. **动态刚体必须放在其它物体上方**，不要与静态物体互相穿插，否则会瞬间弹飞。
3. 让物体落在地面或平台上时，y 坐标 ≥ 物体半高 + 平台顶面高度。
4. 需要相互碰撞的物体都必须有 body 字段；关节两端必须是 dynamic/static 刚体。
5. 颜色用 0~1 的 sRGB 分量；不同材质通过 materials 里的名字引用。
6. 相机放在能看清整个场景的位置（例如 position [7,4.5,8] 看向 [0,1.2,0]）。
7. simulate.steps 给 120~300 比较合适（2~5 秒）；物体很多时不要超过 600。
8. 渲染尺寸不要超过 1280×800（目标机器可能是软件光栅）。
   不确定一份大场景值不值得渲染时，先 `render --dry-run`：它只跑物理与校验，
   几毫秒就返回"会写哪个文件、物理跑多少步"，不花 GPU 时间也不写盘。
   多个 agent **并行**时给输出加 `--no-clobber`（输出已存在就报错，而不是互相覆盖），
   并各自用不同的 --out / --frames-dir。
9. 想让物体动起来，优先用 actions（定时）或 velocity/angular_velocity（初始）；
   想判断"有没有经过某处"，用 probes 而不是靠人眼数帧。
10. 想"打一条射线看路上有什么"或"这个盒子附近有什么"，用 query：
   `quench3d query scene.json --ray 0,5,0,0,-1,0 --all`（射线，全部命中按距离升序）、
   `quench3d query scene.json --overlap "box:2,1,2@0,1,0"`（任意形状重叠）。
   层过滤是**单向**的（--mask 只看被查方在不在掩码里），触发区用 --no-sensors 排除。

修复回路（你写错了会拿到这些）：
- 校验失败时每条诊断都有稳定的机器可读 `code`：按 `path` 定位、按 `code` 决定动作。
  完整表见 `quench3d schema --json` 的 `error_codes` 或 `docs/error_codes.md`。
- `unknown_field` / `missing_field` / `invalid_type`：结构问题（字段名、缺字段、JSON 类型）；
  `invalid_value` / `out_of_range`：取值问题（枚举拼错、数字越界或非有限）；
  `unknown_reference`：引用了不存在的物体/材质/模板/相机名；
  `mutually_exclusive`：两个字段只能留一个；`duplicate_name`：同命名空间重名。
- **不要靠缺省绕过**：引擎对未知取值不兜底；`ineffective` 表示这条写了也不会生效（等于没写）。

只输出 JSON，第一个字符必须是 `{`。"#
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene_doc::SceneDoc;

    #[test]
    fn schema_has_all_commands_with_examples() {
        let s = schema();
        let cmds = s["commands"].as_array().unwrap();
        let names: Vec<&str> = cmds.iter().map(|c| c["name"].as_str().unwrap()).collect();
        for expected in [
            "schema", "doctor", "info", "validate", "render", "simulate", "query", "example",
            "ask", "chat", "bench", "mcp",
        ] {
            assert!(names.contains(&expected), "能力清单缺少命令 {expected}");
        }
        for c in cmds {
            let name = c["name"].as_str().unwrap();
            let examples = c["examples"].as_array().unwrap();
            assert!(!examples.is_empty(), "命令 {name} 缺少示例");
        }
    }

    #[test]
    fn example_scene_is_valid_and_parses() {
        let doc = SceneDoc::from_json(EXAMPLE_SCENE).expect("示例场景必须能解析");
        let diags = doc.validate();
        let errors: Vec<&crate::scene_doc::Diagnostic> = diags
            .iter()
            .filter(|d| d.severity == crate::scene_doc::Severity::Error)
            .collect();
        assert!(errors.is_empty(), "示例场景必须无错误: {errors:?}");
    }

    #[test]
    fn example_scene_actually_simulates_sanely() {
        // 示例场景是文档门面，必须真的能跑：箱子落在斜坡上，不掉出地面、不出 NaN。
        let doc = SceneDoc::from_json(EXAMPLE_SCENE).unwrap();
        let report = crate::runner::run_simulate(&doc).unwrap();
        assert_eq!(report.nan_bodies, 0, "示例场景出现 NaN");
        assert!(
            report.warnings.is_empty(),
            "示例场景警告: {:?}",
            report.warnings
        );
        assert_eq!(
            report.bodies, 5,
            "地面 + 3 箱子 + 斜坡 + 球？实际 {}",
            report.bodies
        );
        for state in &report.states {
            if state.kind == "dynamic" {
                assert!(
                    state.position[1] > 0.0,
                    "{} 掉到地面以下: {:?}",
                    state.name,
                    state.position
                );
            }
        }
    }

    #[test]
    fn grammar_covers_every_shape_and_joint_kind() {
        let g = scene_grammar();
        let kinds = g["shape"]["type"].as_array().unwrap();
        for k in [
            "box",
            "sphere",
            "capsule",
            "cylinder",
            "cone",
            "rounded_box",
            "heightfield",
            "plane",
            "convex",
        ] {
            assert!(kinds.iter().any(|v| v == k), "语法缺少形状 {k}");
        }
        let joints = g["joint"]["type"].as_array().unwrap();
        for k in ["fixed", "ball", "hinge", "distance", "slider"] {
            assert!(joints.iter().any(|v| v == k), "语法缺少关节 {k}");
        }
    }

    /// `schema` 里的诊断码表必须与 `ErrorCode::TABLE` 逐格一致，且真的挂在 schema 上。
    ///
    /// 这张表是 agent 唯一的"码 → 该做什么"查询入口，漏一个码就等于让 agent 对那个
    /// 分支退化成读中文文案。
    #[test]
    fn schema_error_codes_match_the_table() {
        use crate::scene_doc::ErrorCode;
        let arr = error_codes();
        let arr = arr.as_array().expect("error_codes 必须是数组");
        assert_eq!(
            arr.len(),
            ErrorCode::TABLE.len(),
            "schema 的码数量与 ErrorCode::TABLE 不一致"
        );
        for (v, (code, wire, meaning, action)) in arr.iter().zip(ErrorCode::TABLE) {
            assert_eq!(v["code"], *wire, "{wire}: code 不一致");
            assert_eq!(v["meaning"], *meaning, "{wire}: 含义不一致");
            assert_eq!(v["agent_action"], *action, "{wire}: agent 动作不一致");
            assert_eq!(code.as_str(), *wire);
        }
        // 必须真的挂在顶层 schema 上（只连 `error_codes()` 忘了接进 `schema()` 就白做）
        let s = schema();
        assert_eq!(
            s["error_codes"].as_array().map(Vec::len),
            Some(ErrorCode::TABLE.len()),
            "schema() 里没有挂上 error_codes"
        );
        assert!(
            s["envelope"]["failure"]["error"]["diagnostics"][0]["code"].is_string(),
            "信封示例里应当带 code"
        );
    }

    /// 系统提示要把"按 code 分支"讲给大模型，否则修复回路仍然只会重读中文。
    #[test]
    fn system_prompt_mentions_diagnostic_codes() {
        let p = system_prompt();
        assert!(p.contains("error_codes"), "系统提示要指向 error_codes 表");
        for code in [
            "unknown_field",
            "missing_field",
            "invalid_type",
            "invalid_value",
            "out_of_range",
            "unknown_reference",
            "mutually_exclusive",
            "duplicate_name",
            "ineffective",
        ] {
            assert!(p.contains(code), "系统提示缺少诊断码 {code}");
        }
    }

    /// 从 `scene_doc.rs` 源码里抽结构体字段：`(JSON 名, 是否 #[serde(skip)])`。
    ///
    /// 直接读源码是有意的——这样"结构体改了但 schema 没改"一定会在测试里红，
    /// 而不是等到用户发现编辑器补全少了字段。
    fn struct_fields(src: &str, name: &str) -> Vec<(String, bool)> {
        let head = format!("pub struct {name} {{");
        let start = src
            .find(&head)
            .unwrap_or_else(|| panic!("源码里没有 {name}"))
            + head.len();
        let rest = &src[start..];
        let body = &rest[..rest.find("\n}").expect("结构体没有结束")];
        let mut out = Vec::new();
        let mut rename: Option<String> = None;
        let mut skip = false;
        for line in body.lines() {
            let t = line.trim();
            if t.starts_with("#[") {
                if let Some(i) = t.find("rename = \"") {
                    let s = &t[i + 10..];
                    if let Some(j) = s.find('"') {
                        rename = Some(s[..j].to_string());
                    }
                }
                if t.contains("serde(skip)") {
                    skip = true;
                }
                continue;
            }
            if let Some(r) = t.strip_prefix("pub ") {
                if let Some(c) = r.find(':') {
                    let rust_name = r[..c].trim().to_string();
                    out.push((rename.take().unwrap_or(rust_name), skip));
                    skip = false;
                }
            }
        }
        out
    }

    /// 从源码抽枚举变体名，并按其 `rename_all` 规则转成 JSON 里的字符串。
    fn enum_variants(src: &str, name: &str) -> Vec<String> {
        let head = format!("pub enum {name} {{");
        let start = src.find(&head).unwrap_or_else(|| panic!("没有 {name}")) + head.len();
        let before = &src[..start];
        let rename_all = match before.rfind("rename_all = \"") {
            Some(i) => {
                let s = &before[i + 14..];
                s[..s.find('"').unwrap()].to_string()
            }
            None => String::new(),
        };
        let rest = &src[start..];
        let body = &rest[..rest.find("\n}").expect("枚举没有结束")];
        let mut out = Vec::new();
        // 逐变体的 `#[serde(rename = "…")]` 也要认：`rename_all` 只是缺省值，
        // 像 `RoundedBox → "rounded_box"` 这种蛇形名必须显式写，否则 schema 对不上。
        let mut pending_rename: Option<String> = None;
        for line in body.lines() {
            let t = line.trim().trim_end_matches(',');
            if let Some(rest) = t.strip_prefix("#[serde(rename = \"") {
                pending_rename = Some(rest.split('"').next().unwrap_or("").to_string());
                continue;
            }
            if t.is_empty() || t.starts_with("///") || t.starts_with("//") || t.starts_with("#[") {
                continue;
            }
            let ident = t.split(['(', ' ']).next().unwrap_or("");
            if ident.is_empty() || !ident.chars().next().unwrap().is_ascii_uppercase() {
                continue;
            }
            let json = match pending_rename.take() {
                Some(renamed) => renamed,
                None => match rename_all.as_str() {
                    "lowercase" => ident.to_lowercase(),
                    "snake_case" => {
                        let mut s = String::new();
                        for (i, c) in ident.chars().enumerate() {
                            if c.is_ascii_uppercase() {
                                if i > 0 {
                                    s.push('_');
                                }
                                s.push(c.to_ascii_lowercase());
                            } else {
                                s.push(c);
                            }
                        }
                        s
                    }
                    _ => ident.to_string(),
                },
            };
            out.push(json);
        }
        out
    }

    /// 机器可读 JSON Schema 必须与 `scene_doc.rs` 的结构体/枚举逐字段一致。
    #[test]
    fn json_schema_matches_the_source_structs() {
        let src = include_str!("scene_doc.rs");
        let js = json_schema();
        let defs = js["$defs"].as_object().expect("要有 $defs");
        for (sname, dname) in [
            ("SceneDoc", "scene"),
            ("ObjectDoc", "object"),
            ("ShapeDoc", "shape"),
            ("ShapePartDoc", "shapePart"),
            ("InstanceDoc", "instance"),
            ("MaterialDoc", "material"),
            ("GroundDoc", "ground"),
            ("WaterDoc", "water"),
            ("FluidDoc", "fluid"),
            ("ModelDoc", "model"),
            ("ModelIkDoc", "modelIk"),
            ("ModelLodDoc", "modelLod"),
            ("ModelAnimatorParameterDoc", "modelAnimatorParameter"),
            ("AnimatorConditionDoc", "modelAnimatorCondition"),
            ("ModelAnimationDoc", "modelAnimation"),
            ("ModelAnimationBlendDoc", "modelAnimationBlend"),
            ("ModelAnimatorDoc", "modelAnimator"),
            ("ModelAnimatorStateDoc", "modelAnimatorState"),
            ("ModelAnimatorTransitionDoc", "modelAnimatorTransition"),
            ("AssetsDoc", "assets"),
            ("JointDoc", "joint"),
            ("ConeLimitDoc", "coneLimit"),
            ("ActionDoc", "action"),
            ("ProbeDoc", "probe"),
            ("CameraDoc", "camera"),
            ("LightingDoc", "lighting"),
            ("PointLightDoc", "pointLight"),
            ("SpotLightDoc", "spotLight"),
            ("FogDoc", "fog"),
            ("ShadowQualityDoc", "shadowQuality"),
            ("SkyColorsDoc", "skyColors"),
            ("SsaoDoc", "ssao"),
            ("FxaaDoc", "fxaa"),
            ("TaaDoc", "taa"),
            ("BloomDoc", "bloom"),
            ("DofDoc", "dof"),
            ("ColorGradeDoc", "grade"),
            ("PhysicsDoc", "physics"),
            ("SimulateDoc", "simulate"),
            ("RenderDoc", "render"),
            ("OutputDoc", "renderOutput"),
            ("TrackDoc", "track"),
            ("AnimationDoc", "animation"),
        ] {
            let fields: Vec<String> = struct_fields(src, sname)
                .into_iter()
                .filter(|(_, skip)| !skip)
                .map(|(n, _)| n)
                .collect();
            let props = defs[dname]["properties"]
                .as_object()
                .unwrap_or_else(|| panic!("{dname} 没有 properties"));
            for f in &fields {
                assert!(
                    props.contains_key(f),
                    "JSON Schema 的 {dname} 少了字段 `{f}`（结构体改了要同步改 schema）"
                );
            }
            for k in props.keys() {
                assert!(
                    fields.contains(k),
                    "JSON Schema 的 {dname} 多了字段 `{k}`（scene_doc.rs 里没有它）"
                );
            }
        }
        for (ename, dname) in [
            ("BodyKind", "bodyKind"),
            ("ShapeKind", "shapeKind"),
            ("JointKind", "jointKind"),
            ("ActionKind", "actionKind"),
            ("ProbeShapeDoc", "probeShapeKind"),
            ("CombineRuleDoc", "combineRule"),
        ] {
            let variants = enum_variants(src, ename);
            let declared: Vec<String> = defs[dname]["enum"]
                .as_array()
                .unwrap_or_else(|| panic!("{dname} 没有 enum"))
                .iter()
                .map(|v| v.as_str().unwrap_or_default().to_string())
                .collect();
            assert_eq!(
                variants, declared,
                "枚举 {ename} 与 schema 的 {dname} 不一致"
            );
        }
    }

    #[test]
    fn json_schema_is_self_consistent() {
        let js = json_schema();
        assert_eq!(
            js["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        let defs = js["$defs"].as_object().expect("要有 $defs");
        // 每个 `$ref` 都必须指向存在的 def（写错名字会让编辑器静默失去补全）
        fn walk(v: &Value, defs: &serde_json::Map<String, Value>) {
            match v {
                Value::Object(m) => {
                    if let Some(Value::String(r)) = m.get("$ref") {
                        let name = r.rsplit('/').next().unwrap_or("");
                        assert!(defs.contains_key(name), "悬空 $ref: {r}");
                    }
                    for x in m.values() {
                        walk(x, defs);
                    }
                }
                Value::Array(a) => {
                    for x in a {
                        walk(x, defs);
                    }
                }
                _ => {}
            }
        }
        walk(&js, defs);
        // 每个 def 都声明"不许有额外字段"（否则 schema 说合法、`validate` 说非法）
        for (name, d) in defs {
            if d["type"] == "object" {
                assert_eq!(
                    d["additionalProperties"],
                    serde_json::json!(false),
                    "{name} 应当 additionalProperties: false"
                );
            }
        }
    }

    /// 关节的**人读字段表**（`scene_grammar()["joint"]`）必须与 `JointDoc` 的字段一一对应。
    ///
    /// 盯关节是因为这一节字段最多（22 个），而且"写了不生效"的坑（`axis_a`/`axis_b` 只有
    /// 铰链/滑块读、`frame_a`/`frame_b` 只有 `fixed` 读）就靠这张表告诉 agent：
    /// 少一行，agent 只能从 JSON Schema 的字段名去猜语义（猜错的那一半不会报错，只会不生效）。
    #[test]
    fn joint_field_table_covers_every_joint_field() {
        let src = include_str!("scene_doc.rs");
        let mut fields: Vec<String> = struct_fields(src, "JointDoc")
            .into_iter()
            .filter(|(_, skip)| !skip)
            .map(|(n, _)| n)
            .collect();
        fields.sort();
        let g = scene_grammar();
        let table = g["joint"].as_object().expect("joint 要有字段表");
        let mut listed: Vec<String> = table.keys().cloned().collect();
        listed.sort();
        assert_eq!(
            listed, fields,
            "scene_grammar() 的 joint 字段表与 JointDoc 对不上：少写 = agent 猜不到语义，\
             多写 = 文档骗人（`quench3d schema` 是 agent 唯一的字段说明来源）"
        );
    }

    #[test]
    fn system_prompt_mentions_every_top_level_scene_field() {
        let prompt = system_prompt();
        for field in [
            "animations",
            "cameras",
            "gravity",
            "ground",
            "waters",
            "fluids",
            "particles",
            "materials",
            "templates",
            "instances",
            "include",
            "objects",
            "shape",
            "body",
            "joints",
            "models",
            "camera",
            "lighting",
            "simulate",
            "render",
            "rotation_deg",
            "restitution",
            "friction",
            "density",
        ] {
            assert!(
                prompt.contains(field),
                "系统提示缺少字段 {field}，模型会写不出合法场景"
            );
        }
    }
}
