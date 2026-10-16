//! 能力清单（`quench3d schema`）与大模型系统提示。
//!
//! agent 接入时先读 `quench3d schema --json`，就能知道：有哪些命令、每个参数的类型与
//! 缺省值、场景文件的完整语法、退出码含义、以及可以直接照抄的示例。
//!
//! [`system_prompt`] 是同一份能力给大模型看的浓缩版：把场景语法、单位、可用图元、
//! 关节类型和输出要求讲清楚，让模型直接产出可用的场景 JSON。

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
        "envelope": {
            "success": { "ok": true, "command": "render", "version": VERSION, "elapsed_ms": 812, "data": {} },
            "failure": { "ok": false, "command": "render", "version": VERSION, "elapsed_ms": 3,
                          "error": { "kind": "scene", "message": "…", "path": "objects[0].shape", "hint": "…",
                                     "diagnostics": [ { "severity": "error", "path": "objects[0].shape", "message": "…" } ] } }
        },
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
                "output": "{ output, width, height, bytes, draw_calls, instances, triangles, culled, shadow_instances, lines, objects, camera_position, probes[]?, simulate?, dry_run?, would_write[]?, elapsed_ms }",
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
                    { "name": "--encoder", "type": "enum", "required": false, "description": "封装格式：png（缺省，帧序列）| gif | apng。gif/apng 为内置纯 Rust 编码器，直接产出可播放的单文件，不需要 ffmpeg；gif 只有 256 色且帧延迟粒度为 10 ms" },
                    { "name": "--out", "type": "path", "required": false, "description": "单文件输出路径（仅 --encoder gif|apng；缺省 movie.gif / movie.png）" },
                    { "name": "--fps", "type": "f32", "required": false, "description": "视频帧率（缺省 = 等价帧率 1/(dt×steps-per-frame)）：调小=慢动作，调大=加速" },
                    { "name": "--warmup", "type": "u32", "required": false, "description": "预热帧数（缺省 0）：只推进不输出，丢弃首帧脏画面" },
                    { "name": "--width", "type": "u32", "required": false, "description": "覆盖宽度" },
                    { "name": "--height", "type": "u32", "required": false, "description": "覆盖高度" },
                    { "name": "--dt", "type": "f32", "required": false, "description": "覆盖 simulate.dt（物理步长）" },
                    { "name": "--dry-run", "type": "flag", "required": false, "description": "只校验 + 跑物理，不渲染、不建帧目录、不写 trace；报告里 digest 为空串并给出 would_write" },
                    { "name": "--no-clobber", "type": "flag", "required": false, "description": "单文件输出或帧目录已存在（非空）就报错，而不是覆盖/续录；要续录就别加这个开关" }
                ],
                "output": "{ frames_dir, encoder, output, trace, frames, skipped, fps, duration, steps_per_frame, dt, total_steps, warmup_frames, width, height, bytes, digest, objects, camera_position, probes[]?, first_frame?, last_frame?, ffmpeg, notes, dry_run?, would_write[]?, elapsed_ms }",
                "notes": "帧数与时长由物理步决定、与墙钟无关；同一场景两次录制 digest 相同（换编码器也不改变 digest）。--encoder png 时 ffmpeg 字段给出编码命令：ffmpeg -framerate N -i frames/frame_%05d.png -c:v libx264 -crf 18 -pix_fmt yuv420p movie.mp4；gif/apng 时该字段为空串，可直接播放，notes 里会提示格式限制（如 GIF 的 10 ms 延迟粒度）",
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
                "output": "{ steps, dt, gravity, bodies, dynamic, sleeping, contacts, joints, nan_bodies, lowest_y, max_speed, states[], probes[]?, warnings[], trajectories[]? }",
                "trajectory_fields": "name, shape, samples[{step,time,position,speed,displacement,tilt_deg,asleep}], start, end, min, max, fall, rise, travel, path_length, max_speed, max_tilt_deg, tilt_meaningful, tipped_over, last_motion_step",
                "examples": [
                    "quench3d simulate scene.json --steps 600 --json",
                    "quench3d simulate scene.json --trace 球,箱子 --steps 300",
                    "quench3d simulate scene.json --trace-all --render out.png"
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
                "tools": ["quench3d_schema", "quench3d_doctor", "quench3d_info", "quench3d_validate", "quench3d_render", "quench3d_movie", "quench3d_simulate"],
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
                "tools": ["quench3d_schema", "quench3d_doctor", "quench3d_info", "quench3d_validate", "quench3d_render", "quench3d_movie", "quench3d_simulate", "quench3d_ask"],
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
            "version": "整数，当前 2（版本 2 起 parent 生效；不兼容版本 1）",
            "name": "场景名（可选）",
            "gravity": "[x, y, z]，缺省 [0, -9.81, 0]",
            "ground": { "size": "f32 边长，缺省 40", "material": "材质名（可选）", "y": "f32 高度，缺省 0" },
            "materials": "名字 → 材质对象",
            "objects": "图元物体数组",
            "models": "外部模型数组（gltf/glb/obj）",
            "joints": "关节数组，用物体名连接",
            "actions": "定时动作数组：在指定步/秒对刚体施加冲量、力、速度或瞬移",
            "probes": "区域探针数组：只读记录谁何时进入/离开某个球形或盒形区域",
            "camera": "相机对象",
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
            "normal_scale": "法线贴图强度，缺省 1",
            "alpha_cutoff": "alpha 裁剪阈值：大于它的像素才画（做树叶/栅栏），缺省 0",
            "alpha_blend": "true 时走半透明混合（与 alpha_cutoff 同时给时裁剪优先，混合不生效）",
            "unlit": "true 时不参与光照，直接用颜色/贴图（做 UI、发光体）",
            "double_sided": "true 时背面也画（单面片、薄墙），缺省 false",
            "base_color_texture": "贴图路径，相对**场景文件**解析（也支持 assets 搜索路径）",
            "normal_texture": "法线贴图路径",
            "metallic_roughness_texture": "金属度/粗糙度贴图路径（glTF 约定：G=粗糙度，B=金属度）",
            "emissive_texture": "自发光贴图路径"
        },
        "shape": {
            "type": ["box", "sphere", "capsule", "cylinder", "plane", "convex"],
            "box": { "size": "[x, y, z] 边长（不是半长），缺省 [1,1,1]" },
            "sphere": { "radius": "缺省 0.5" },
            "capsule": { "radius": "缺省 0.5", "half_height": "圆柱段半高，缺省 0.5" },
            "cylinder": { "radius": "缺省 0.5", "half_height": "缺省 0.5" },
            "plane": { "normal": "[x,y,z]，缺省 [0,1,0]" },
            "convex": { "points": "至少 4 个 [x,y,z] 顶点" }
        },
        "object": {
            "name": "名称（关节引用与报告都用它；缺省 objects[i]）",
            "shape": "形状对象，缺省 1×1×1 立方体",
            "parent": "父物体名（objects[].name）；给了之后 position/rotation_deg 变成**相对父物体**的局部量（继承位置与朝向，**不继承缩放**）。带刚体的物体不能有父物体",
            "position": "[x, y, z]，缺省原点；有 parent 时是局部的",
            "rotation_deg": "欧拉角 XYZ，单位度",
            "scale": "显示缩放（只影响渲染，不影响碰撞）；不受父物体缩放影响",
            "material": "材质名（缺省灰色）",
            "body": ["none（纯显示，缺省）", "static", "dynamic", "kinematic"],
            "density": "kg/m³，缺省 1000",
            "restitution": "0~1，缺省 0.2",
            "friction": "缺省 0.5",
            "velocity": "初始线速度 [x,y,z]",
            "angular_velocity": "初始角速度 [x,y,z]（弧度/秒）",
            "linear_damping": "线性阻尼 ≥0，缺省 0（不衰减）",
            "angular_damping": "角阻尼 ≥0，缺省 0",
            "gravity_scale": "重力缩放：1 正常、0 失重、负值反重力，缺省 1",
            "mass": "直接给质量（kg），覆盖 density×体积",
            "allow_sleep": "false 时永不自动睡眠（缺省 true）",
            "visible": "缺省 true",
            "note": "linear/angular_damping、gravity_scale、mass、allow_sleep、angular_velocity 只对 body=dynamic 生效"
        },
        "model": {
            "path": "gltf/glb/obj 路径",
            "position": "[x, y, z]",
            "rotation_deg": "欧拉角，度",
            "scale": "均匀缩放，缺省 1",
            "collide": "true 时把模型合并成静态三角网格碰撞体，缺省 false",
            "friction": "参与碰撞时的摩擦，缺省 0.8"
        },
        "joint": {
            "type": ["fixed", "ball", "hinge", "distance", "slider"],
            "a": "物体名（必须有刚体）",
            "b": "物体名（必须有刚体）",
            "axis": "铰链/滑块的轴（**各自刚体的局部轴**），缺省 [0,1,0]",
            "anchor_a": "关节在 a 上的连接点（a 的局部坐标，米），缺省原点",
            "anchor_b": "关节在 b 上的连接点（b 的局部坐标，米），缺省原点",
            "limits": "铰链为 [最小, 最大] 弧度；滑块为 [最小, 最大] 米",
            "rest_length": "distance 关节的静止长度",
            "stiffness": "distance 关节刚度",
            "damping": "distance 关节阻尼",
            "motor_velocity": "铰链马达目标角速度（弧度/秒）",
            "motor_max_impulse": "铰链马达最大冲量"
        },
        "action": {
            "type": ["impulse", "impulse_at_point", "torque_impulse", "velocity",
                     "angular_velocity", "force", "teleport", "wake"],
            "target": "刚体名（objects[].name / $ground / $model[i]）",
            "at_step": "第几步开始前施加，缺省 0（第 1 步是第一次物理步进）",
            "at_time": "第几秒开始施加（与 at_step 互斥，按 round(t/dt) 折算成步）",
            "value": "[x,y,z]：冲量 N·s / 力 N / 速度 m/s / 角速度 rad/s / 角冲量 N·m·s",
            "point": "世界坐标作用点，仅 impulse_at_point（用来推门把手、偏心撞击）",
            "position": "目标位置，仅 teleport",
            "rotation_deg": "目标朝向（欧拉角 XYZ，度），仅 teleport，缺省保持当前朝向",
            "steps": "force 持续多少步，缺省 1",
            "note": "写在静态刚体上的冲量/力不会有任何效果（校验会警告）；冲量会自动唤醒睡眠刚体"
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
            "ambient": "环境光强度",
            "exposure": "曝光",
            "sky": "是否绘制天空，缺省 true",
            "grid": "是否绘制无限网格，缺省 true",
            "shadows": "是否投射阴影，缺省 true",
            "point_lights": "点光源数组：{ position:[x,y,z], color:[r,g,b]（**线性**、非 sRGB）, intensity（缺省 12）, radius（影响半径，缺省 18）}；最多 8 盏生效。**不写这个字段 = 保留预设自带的补光**，写 [] 才是显式清空",
            "bloom": "泛光：{ enabled（缺省 true）, intensity（缺省 0.6）, threshold（缺省 1.0，线性 Rec.709 亮度）}。**不写这一节 = 关闭**（像素与关闭时逐位相同）。光照/自发光先落到线性 HDR 中间目标，超阈值的部分做半分辨率高斯模糊，再在 tonemap **之前**加回去——所以亮区外溢不会过曝成白块。想让太阳/自发光有「发光感」就写 { threshold: 1.0, intensity: 0.6 }",
            "fxaa": "后处理抗锯齿：{ enabled（缺省 true）, edge_threshold（缺省 0.166，越小越敏感）, edge_threshold_min（缺省 0.0833）}。**不写这一节 = 关闭**（像素与关闭时逐位相同）。它在 tonemap **之后**的画面（不是线性 HDR）上找感知边缘，沿边缘方向做一次定向模糊；平坦区域逐位不变。与 render.msaa 不冲突：MSAA 管几何边缘，FXAA 管着色噪声与剩下的锯齿，代价是细节略微变软。斜边多的场景写 {} 就够了"
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

/// 给大模型看的系统提示：把场景语法讲清楚，并要求只输出 JSON。
pub fn system_prompt() -> String {
    r#"你是 Quench3D 引擎的场景生成器。用户会用自然语言描述一个 3D 场景或物理实验，
你只输出一个 JSON 对象（不要解释、不要 Markdown 代码块以外的文字），格式是 quench3d.scene v2。

顶层字段（全部可选）：
- name: 字符串
- gravity: [x, y, z]，缺省 [0, -9.81, 0]
- ground: { size: 40, material: "floor" }   // 地面是无限大静态平面
- materials: { 名字: { color: [r,g,b]（sRGB）, metallic: 0~1, roughness: 0.05~1, emissive: 0,
             alpha: 0~1, emissive_color: [r,g,b], uv_scale: 1, normal_scale: 1,
             alpha_cutoff: 0, alpha_blend: false, unlit: false, double_sided: false,
             base_color_texture: "a.png", normal_texture, metallic_roughness_texture, emissive_texture } }
  （贴图路径相对场景文件解析；alpha_cutoff 与 alpha_blend 同时给时裁剪优先）
- objects: 数组，每项：
  - name: 字符串（关节引用与报告用它）
  - shape: { type: "box"|"sphere"|"capsule"|"cylinder"|"plane"|"convex",
             size: [x,y,z]（box 的**边长**）, radius, half_height, normal, points }
  - parent: 父物体的 name（可选）。给了之后 position/rotation_deg 是**相对父物体**的
            （父物体动，子物体跟着动；只继承位置与朝向，不继承缩放；
             带刚体的物体不能有父物体）。父物体必须显式写 name
  - position: [x,y,z]（米，有 parent 时是相对父物体的）, rotation_deg: [x,y,z]（度）
  - material: 材质名
  - body: "none"|"static"|"dynamic"|"kinematic"（缺省 none 表示纯显示，不参与物理）
  - density: kg/m³（缺省 1000）, restitution: 0~1（缺省 0.2）, friction:（缺省 0.5）
  - velocity: [x,y,z] 初始速度, angular_velocity: [x,y,z]（弧度/秒）
  - linear_damping / angular_damping（≥0，缺省 0）, gravity_scale（缺省 1，0 表示失重）
  - mass（kg，覆盖 density×体积）, allow_sleep: false（永不自动睡眠）
  - 上面这些物理参数只对 body: "dynamic" 生效
- joints: 数组，每项：{ type: "fixed"|"ball"|"hinge"|"distance"|"slider", a: 物体名, b: 物体名,
           axis: [x,y,z]（**各自刚体的局部轴**，缺省 [0,1,0]）, limits: [min,max]（铰链弧度 / 滑块米）,
           anchor_a / anchor_b: [x,y,z]（关节在两端的**局部**连接点，做门轴/悬臂时错开它们）,
           rest_length, stiffness, damping, motor_velocity }
- actions: 数组，每项：{ type: "impulse"|"impulse_at_point"|"torque_impulse"|"velocity"|
           "angular_velocity"|"force"|"teleport"|"wake", target: 刚体名,
           at_step: 60（或 at_time: 1.0 秒，二选一）, value: [x,y,z], point, position,
           rotation_deg, steps }
  （冲量 N·s / 力 N / 速度 m/s；用来表达"第 2 秒推一下""到点了就松手"这类剧本。
   注意质量默认由 density×体积 推出：体积 0.5 的球密度 1000 → 500 kg，
   6 N·s 的冲量只有 0.012 m/s，想推得动就写 mass 或给大一点的冲量）
- probes: 数组，每项：{ name: "落点", shape: "sphere"|"box", position: [x,y,z],
           radius: 1.0（球形）, size: [1,1,1]（盒形）, watch: ["球"] }
  （只读区域监测：报告里给出谁在第几步进入、第几步离开、区内最高速度。
   半空间（地面）没有有限包围盒，探针监测不到它）
- models: 数组：{ path: "xx.glb"|"xx.obj", position, rotation_deg, scale, collide: false }
- camera: { position: [x,y,z], look_at: [x,y,z], fov_deg: 50, near: 0.05, far: 5000,
            orthographic_height: 5, frame_scene: false }
  或者 { distance: 12, yaw_deg: 30, pitch_deg: 20, look_at: [0,1,0] }
  （给了 orthographic_height 就是正交投影，没有近大远小）
- lighting: { preset: "studio"|"day"|"night", sun_direction: [x,y,z], sun_intensity, ambient, exposure, sky: true, grid: true, shadows: true,
             point_lights: [ { position: [x,y,z], color: [r,g,b]（线性）, intensity: 12, radius: 18 } ],
             bloom: { enabled: true, intensity: 0.6, threshold: 1.0 },
             fxaa: { enabled: true, edge_threshold: 0.166 } }
  （point_lights 最多 8 盏；不写就保留预设自带补光。bloom / fxaa 不写 = 关闭；
    threshold 是线性亮度，太阳圆盘/自发光可以远大于 1，所以 1.0 通常只让它们外溢；
    fxaa 是 tonemap 之后的后处理抗锯齿，和 render.msaa 可以同时开）
- simulate: { steps: 180, dt: 0.0166667 }
- render: { output: "scene.png", width: 960, height: 600, msaa: 4 }

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
            "schema", "doctor", "info", "validate", "render", "simulate", "example", "ask", "chat",
            "bench", "mcp",
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
        for k in ["box", "sphere", "capsule", "cylinder", "plane", "convex"] {
            assert!(kinds.iter().any(|v| v == k), "语法缺少形状 {k}");
        }
        let joints = g["joint"]["type"].as_array().unwrap();
        for k in ["fixed", "ball", "hinge", "distance", "slider"] {
            assert!(joints.iter().any(|v| v == k), "语法缺少关节 {k}");
        }
    }

    #[test]
    fn system_prompt_mentions_every_top_level_scene_field() {
        let prompt = system_prompt();
        for field in [
            "gravity",
            "ground",
            "materials",
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
