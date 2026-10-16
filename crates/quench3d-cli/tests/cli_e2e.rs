//! 端到端测试：真的去跑 `quench3d` 二进制，检查 JSON 信封与退出码。
//!
//! 这些用例是 agent 契约的回归测试——退出码、单行 JSON、诊断路径一旦变了就算失败。

mod common;

use std::process::{Command, Stdio};
use std::time::Duration;

use common::{mock_scene_reply, MockLlm};

const BIN: &str = env!("CARGO_BIN_EXE_quench3d");

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    /// 把 stdout 解析成单行 JSON 信封。
    fn json(&self) -> serde_json::Value {
        let line = self
            .stdout
            .lines()
            .find(|l| l.trim_start().starts_with('{'))
            .unwrap_or_else(|| panic!("stdout 里没有 JSON：\n{}", self.stdout));
        serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("stdout 不是合法 JSON（{e}）：\n{}", self.stdout))
    }
}

fn run(args: &[&str]) -> Run {
    let out = Command::new(BIN)
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("运行 quench3d");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

/// 临时目录（每个用例一个，避免互相踩）。
fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("quench3d_cli_test_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

#[test]
fn version_and_help_are_friendly() {
    let v = run(&["--version"]);
    assert_eq!(v.code, 0);
    assert!(v.stdout.contains("quench3d"));

    let h = run(&["--help"]);
    assert_eq!(h.code, 0);
    for cmd in [
        "schema", "doctor", "info", "validate", "render", "simulate", "ask", "mcp",
    ] {
        assert!(h.stdout.contains(cmd), "帮助里缺少命令 {cmd}");
    }
}

#[test]
fn schema_json_describes_every_command() {
    let r = run(&["schema", "--json"]);
    assert_eq!(r.code, 0);
    let env = r.json();
    assert_eq!(env["ok"], true);
    let commands = env["data"]["commands"].as_array().unwrap();
    let names: Vec<&str> = commands
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for expected in [
        "validate", "render", "simulate", "ask", "mcp", "doctor", "info",
    ] {
        assert!(names.contains(&expected), "能力清单缺少 {expected}");
    }
    // 场景语法要能被 agent 直接读懂
    assert_eq!(env["data"]["scene"]["format"], "quench3d.scene");
    assert!(env["data"]["exit_codes"]["2"].as_str().is_some());
}

#[test]
fn doctor_reports_platform_and_gpu_state() {
    let r = run(&["doctor", "--json"]);
    assert_eq!(r.code, 0);
    let env = r.json();
    let d = &env["data"];
    assert_eq!(d["os"], std::env::consts::OS);
    assert_eq!(d["arch"], std::env::consts::ARCH);
    assert!(d["gpu_available"].is_boolean());
    assert!(d["notes"].is_array());
}

#[test]
fn example_scene_validates_and_simulates() {
    let dir = temp_dir("example");
    let scene = dir.join("scene.json");

    let e = run(&["example", "--out", scene.to_str().unwrap()]);
    assert_eq!(e.code, 0, "{}", e.stderr);
    assert!(scene.exists());

    let v = run(&["validate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(v.code, 0, "示例场景必须合法: {}", v.stdout);
    let env = v.json();
    assert_eq!(env["data"]["valid"], true);
    assert_eq!(env["data"]["errors"], 0);

    let s = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(s.code, 0);
    let env = s.json();
    let data = &env["data"];
    assert_eq!(data["nan_bodies"], 0, "仿真不允许出现 NaN");
    assert!(data["bodies"].as_u64().unwrap() >= 4);
    assert!(data["states"].as_array().unwrap().len() >= 4);
    // 每个动态刚体都不该穿到地面以下
    for st in data["states"].as_array().unwrap() {
        if st["kind"] == "dynamic" {
            let y = st["position"][1].as_f64().unwrap();
            assert!(y > 0.0, "{} 掉到地面以下: {y}", st["name"]);
        }
    }
    // 步数覆盖生效
    let s2 = run(&[
        "simulate",
        scene.to_str().unwrap(),
        "--steps",
        "600",
        "--json",
    ]);
    assert_eq!(s2.json()["data"]["steps"], 600);
}

#[test]
fn simulate_traces_bodies_and_reports_fall_and_tilt() {
    let dir = temp_dir("trace");
    let scene = dir.join("tower.json");
    std::fs::write(
        &scene,
        r#"{
          "ground": { "size": 30 },
          "objects": [
            { "name": "塔1", "shape": { "type": "box", "size": [0.6, 1.2, 0.6] },
              "position": [0, 3.0, 0], "body": "dynamic" },
            { "name": "塔2", "shape": { "type": "box", "size": [0.6, 1.2, 0.6] },
              "position": [0, 4.2, 0], "body": "dynamic" }
          ],
          "simulate": { "steps": 240, "dt": 0.0166667 }
        }"#,
    )
    .unwrap();

    let r = run(&[
        "simulate",
        scene.to_str().unwrap(),
        "--trace",
        "塔1,塔2",
        "--trace-every",
        "4",
        "--json",
    ]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let env = r.json();
    let trajs = env["data"]["trajectories"].as_array().expect("应当有轨迹");
    assert_eq!(trajs.len(), 2);
    let first = &trajs[0];
    assert_eq!(first["name"], "塔1");
    assert_eq!(first["shape"], "box");
    assert!(first["fall"].as_f64().unwrap() > 1.0, "{}", first);
    assert_eq!(first["tilt_meaningful"], true);
    let samples = first["samples"].as_array().unwrap();
    assert!(samples.len() > 10, "采样点太少: {}", samples.len());
    assert_eq!(samples[0]["step"], 0);
    assert!(
        samples.last().unwrap()["step"].as_u64().unwrap() <= 240,
        "采样不应超过总步数"
    );
    // 每一步都有位置与倾斜角
    for s in samples {
        assert!(s["position"].as_array().unwrap().len() == 3);
        assert!(s["tilt_deg"].as_f64().is_some());
    }

    // 名字写错时给出可用名字，退出码是 2（输入问题）
    let bad = run(&[
        "simulate",
        scene.to_str().unwrap(),
        "--trace",
        "不存在",
        "--json",
    ]);
    assert_eq!(bad.code, 2, "{}", bad.stdout);
    let bad_env = bad.json();
    let msg = bad_env["error"]["message"].as_str().unwrap();
    assert!(msg.contains("塔1"), "错误信息要列出可用名字: {msg}");
}

#[test]
fn invalid_scene_exits_with_code_2_and_reports_paths() {
    let dir = temp_dir("invalid");
    let scene = dir.join("bad.json");
    std::fs::write(
        &scene,
        r#"{
            "materials": { "ok": { "color": [1, 0, 0] } },
            "objects": [
                { "name": "a", "material": "nope", "body": "dynamic" },
                { "name": "b", "shape": { "type": "pyramid" }, "body": "dynamic" }
            ],
            "ground": { "unknown_field": 1 }
        }"#,
    )
    .unwrap();

    let r = run(&["validate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 2, "输入不合法必须是退出码 2：{}", r.stdout);
    let env = r.json();
    assert_eq!(env["ok"], false);
    let diags = env["error"]["diagnostics"].as_array().unwrap();
    assert!(diags.len() >= 2, "应当报出多个问题: {diags:?}");
    let paths: Vec<&str> = diags.iter().map(|d| d["path"].as_str().unwrap()).collect();
    assert!(
        paths.iter().any(|p| p.starts_with("objects")),
        "诊断要带 JSON 路径: {paths:?}"
    );
    for d in diags {
        assert!(
            d["hint"].is_string() || d["message"].is_string(),
            "诊断必须有说明: {d:?}"
        );
    }

    // render 遇到非法场景也应当在入口就拦住（不会去开 GPU）
    let rr = run(&["render", scene.to_str().unwrap(), "--json"]);
    assert_eq!(rr.code, 2);
}

#[test]
fn missing_file_is_reported_as_invalid_not_crash() {
    let r = run(&["validate", "/nonexistent/nope.json", "--json"]);
    assert_eq!(r.code, 2);
    let env = r.json();
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["kind"], "invalid");
    assert!(env["error"]["hint"].is_string());

    // 缺少参数同样是 2
    let r2 = run(&["render", "--json"]);
    assert_eq!(r2.code, 2);
    let env2 = r2.json();
    assert_eq!(env2["error"]["kind"], "invalid");
}

#[test]
fn json_output_is_a_single_line_envelope() {
    // agent 依赖「一行一个 JSON」，任何多行输出都会破坏解析
    for args in [
        vec!["schema", "--json"],
        vec!["doctor", "--json"],
        vec!["simulate", "--json"],
    ] {
        let r = if args == ["simulate", "--json"] {
            let dir = temp_dir("oneline");
            let scene = dir.join("s.json");
            std::fs::write(
                &scene,
                r#"{"ground":{},"objects":[{"name":"b","shape":{"type":"box","size":[1,1,1]},"position":[0,3,0],"body":"dynamic"}]}"#,
            )
            .unwrap();
            run(&["simulate", scene.to_str().unwrap(), "--json"])
        } else {
            run(&args)
        };
        assert_eq!(r.code, 0, "{}", r.stderr);
        let json_lines = r
            .stdout
            .lines()
            .filter(|l| l.trim_start().starts_with('{'))
            .count();
        assert_eq!(json_lines, 1, "stdout 必须只有一行 JSON：\n{}", r.stdout);
    }
}

#[test]
fn ask_uses_mock_llm_and_returns_valid_scene() {
    let server = MockLlm::start(&mock_scene_reply());
    let dir = temp_dir("ask");
    let scene = dir.join("tower.json");

    let r = run(&[
        "ask",
        "来一座三层箱子塔",
        "--base-url",
        &server.base_url,
        "--model",
        "mock-model",
        "--out",
        scene.to_str().unwrap(),
        "--simulate",
        "--json",
    ]);
    assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    let env = r.json();
    let data = &env["data"];
    assert_eq!(data["valid"], true, "生成的场景应当通过校验: {data}");
    assert_eq!(data["model"], "mock-model");
    assert_eq!(data["scene"]["objects"].as_array().unwrap().len(), 3);
    assert!(data["usage"]["total_tokens"].as_u64().unwrap() >= 1);
    assert!(data["simulate"]["nan_bodies"] == 0);

    // 落盘内容必须能被独立校验
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&scene).unwrap()).unwrap();
    assert_eq!(written["name"], "模型生成的塔");
    let v = run(&["validate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(v.code, 0, "{}", v.stdout);

    // 请求确实带上了系统提示与用户需求
    let request = server
        .next_request(Duration::from_secs(5))
        .expect("收到请求");
    assert!(request.contains("quench3d.scene"), "系统提示没发出去");
    assert!(request.contains("三层箱子塔"));
    server.stop();
}

#[test]
fn ask_reports_network_failure_with_exit_code_4() {
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let r = run(&[
        "ask",
        "随便",
        "--base-url",
        &format!("http://127.0.0.1:{port}/v1"),
        "--json",
    ]);
    assert_eq!(r.code, 4, "网络问题必须是退出码 4：{}", r.stdout);
    let env = r.json();
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["kind"], "network");
    assert!(
        env["error"]["hint"].as_str().unwrap().contains("ollama"),
        "提示要给出可操作的下一步: {env}"
    );
}

#[test]
fn chat_command_runs_the_tool_loop_and_returns_json() {
    // 假模型先要求仿真，再用自然语言收尾；验证命令行 agent 的完整链路
    let tool_call = serde_json::json!({
        "choices": [{
            "message": {
                "role": "assistant", "content": "",
                "tool_calls": [{
                    "id": "c1", "type": "function",
                    "function": {
                        "name": "quench3d_simulate",
                        "arguments": serde_json::json!({
                            "steps": 120,
                            "scene": {
                                "ground": { "size": 20 },
                                "objects": [{ "name": "球", "shape": { "type": "sphere", "radius": 0.5 },
                                              "position": [0, 5, 0], "body": "dynamic" }]
                            }
                        }).to_string()
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }]
    })
    .to_string();
    let final_text = serde_json::json!({
        "choices": [{ "message": { "role": "assistant", "content": "球停在地面上，高度 0.5 米。" },
                      "finish_reason": "stop" }]
    })
    .to_string();
    let server = MockLlm::start_sequence(vec![tool_call, final_text], true);

    let r = run(&[
        "chat",
        "球最后停多高",
        "--base-url",
        &server.base_url,
        "--model",
        "mock-model",
        "--json",
    ]);
    assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    let env = r.json();
    let data = &env["data"];
    assert_eq!(data["answer"], "球停在地面上，高度 0.5 米。");
    assert_eq!(data["rounds"], 1);
    let tools = data["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["name"], "quench3d_simulate");
    assert_eq!(tools[0]["ok"], true);
    assert_eq!(data["result"]["nan_bodies"], 0);
    assert_eq!(data["result"]["bodies"], 2);

    // 两次请求：第一次带工具定义，第二次带 tool 回执
    let first = server.next_request(Duration::from_secs(5)).unwrap();
    let second = server.next_request(Duration::from_secs(5)).unwrap();
    assert!(first.contains("\"tools\""), "第一次请求应带工具定义");
    let sent: serde_json::Value = serde_json::from_str(&second).unwrap();
    assert_eq!(sent["messages"][3]["role"], "tool");
    assert_eq!(sent["messages"][3]["tool_call_id"], "c1");
    server.stop();

    // 模型服务不可达时退出码是 4
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let bad = run(&[
        "chat",
        "随便",
        "--base-url",
        &format!("http://127.0.0.1:{port}/v1"),
        "--json",
    ]);
    assert_eq!(bad.code, 4, "{}", bad.stdout);
}

#[test]
fn mcp_over_stdio_handshake_list_and_call() {
    let scene = serde_json::json!({
        "ground": { "size": 12 },
        "objects": [{
            "name": "球",
            "shape": { "type": "sphere", "radius": 0.5 },
            "position": [0, 4, 0],
            "body": "dynamic"
        }]
    });
    let mcp_frames = temp_dir("mcp_movie");

    let requests = [
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"quench3d_validate","arguments":{"scene": scene}}}),
        serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"quench3d_simulate","arguments":{"scene": scene, "steps": 90}}}),
        serde_json::json!({"jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"quench3d_schema","arguments":{}}}),
        serde_json::json!({"jsonrpc":"2.0","id":6,"method":"tools/call",
            "params":{"name":"quench3d_movie","arguments":{
                "scene": scene, "frames": 2, "width": 64, "height": 48,
                "frames_dir": mcp_frames.to_str().unwrap()}}}),
    ];
    let input = requests
        .iter()
        .map(|r| r.to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    let mut child = Command::new(BIN)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .spawn()
        .expect("启动 mcp");
    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().expect("等待 mcp 结束");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    let lines: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("非法 JSON 行: {l} ({e})")))
        .collect();
    // 通知不回复，所以 7 条请求 → 6 条回复
    assert_eq!(lines.len(), 6, "回复条数不对: {lines:#?}");

    let by_id = |id: i64| {
        lines
            .iter()
            .find(|l| l["id"] == id)
            .unwrap_or_else(|| panic!("没有 id={id} 的回复: {lines:#?}"))
    };

    let init = by_id(1);
    assert_eq!(init["result"]["serverInfo"]["name"], "quench3d");
    assert!(init["result"]["capabilities"]["tools"].is_object());

    let tools = by_id(2);
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for t in [
        "quench3d_schema",
        "quench3d_validate",
        "quench3d_render",
        "quench3d_movie",
        "quench3d_simulate",
        "quench3d_ask",
        "quench3d_doctor",
        "quench3d_info",
    ] {
        assert!(names.contains(&t), "工具列表缺少 {t}: {names:?}");
    }

    // movie 工具必须带 schema 且声明为写盘（帧数有上限，agent 才不会误触发长任务）
    let movie = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "quench3d_movie")
        .expect("movie 工具定义");
    assert!(movie["inputSchema"]["properties"]["frames"].is_object());
    assert!(
        movie["description"].as_str().unwrap().contains("写盘"),
        "描述里要说明会写盘：{}",
        movie["description"]
    );

    let validated = by_id(3);
    assert_eq!(validated["result"]["isError"], false);
    assert_eq!(validated["result"]["structuredContent"]["valid"], true);

    let simulated = by_id(4);
    let sim = &simulated["result"]["structuredContent"];
    assert_eq!(sim["nan_bodies"], 0);
    assert_eq!(sim["bodies"], 2);
    let y = sim["states"][1]["position"][1].as_f64().unwrap();
    assert!((0.4..0.7).contains(&y), "球应当停在地面上，实际 y={y}");

    let schema = by_id(5);
    let text = schema["result"]["content"][0]["text"].as_str().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["name"], "quench3d");

    // movie 工具：有 GPU 就必须真的录出帧序列，没 GPU 必须干净地报环境错误
    let movie = by_id(6);
    if movie["result"]["isError"] == false {
        let data = &movie["result"]["structuredContent"];
        assert_eq!(data["frames"], 2, "{movie:#?}");
        assert!(data["ffmpeg"].as_str().unwrap().contains("yuv420p"));
        let png = mcp_frames.join("frame_00001.png");
        let bytes = std::fs::read(&png).unwrap_or_else(|e| panic!("缺末帧 {png:?}: {e}"));
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let tracelines = std::fs::read_to_string(mcp_frames.join("trace.jsonl"))
            .expect("trace.jsonl")
            .lines()
            .count();
        assert_eq!(tracelines, 2);
    } else {
        let err = &movie["result"]["structuredContent"];
        assert_eq!(
            err["kind"], "environment",
            "无 GPU 时必须是环境错误：{movie:#?}"
        );
    }
    let _ = std::fs::remove_dir_all(&mcp_frames);
}

#[test]
fn render_produces_png_or_reports_missing_gpu_cleanly() {
    let doctor = run(&["doctor", "--json"]).json();
    let gpu = doctor["data"]["gpu_available"].as_bool().unwrap();

    let dir = temp_dir("render");
    let scene = dir.join("s.json");
    let png = dir.join("out.png");
    std::fs::write(
        &scene,
        r#"{
            "ground": { "size": 20, "material": "floor" },
            "materials": { "floor": { "color": [0.5, 0.52, 0.56], "roughness": 0.9 } },
            "objects": [
                { "name": "箱", "shape": { "type": "box", "size": [1, 1, 1] },
                  "position": [0, 3, 0], "body": "dynamic", "density": 500 }
            ],
            "camera": { "position": [6, 3.5, 7], "look_at": [0, 0.8, 0] },
            "simulate": { "steps": 120 },
            "render": { "output": "ignored.png", "width": 480, "height": 320, "msaa": 1 }
        }"#,
    )
    .unwrap();

    let r = run(&[
        "render",
        scene.to_str().unwrap(),
        "--out",
        png.to_str().unwrap(),
        "--json",
    ]);

    if !gpu {
        assert_eq!(r.code, 3, "没有 GPU 时必须用退出码 3");
        let env = r.json();
        assert_eq!(env["error"]["kind"], "environment");
        return;
    }

    assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    let env = r.json();
    let data = &env["data"];
    assert_eq!(
        data["output"],
        png.to_str().unwrap(),
        "--out 应当覆盖 render.output"
    );
    assert_eq!(data["width"], 480);
    assert!(data["draw_calls"].as_u64().unwrap() >= 1);
    assert!(data["triangles"].as_u64().unwrap() > 0);
    assert_eq!(data["simulate"]["steps"], 120);
    // 相机反解应当精确还原
    let cp: Vec<f64> = data["camera_position"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    assert!(
        (cp[0] - 6.0).abs() < 0.05 && (cp[1] - 3.5).abs() < 0.05 && (cp[2] - 7.0).abs() < 0.05,
        "相机位置反解不精确: {cp:?}"
    );

    // PNG 真的存在、是 PNG、尺寸正确，而且不是纯色
    let bytes = std::fs::read(&png).expect("PNG 应当存在");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "PNG magic 不对");
    assert!(
        bytes.len() > 2000,
        "图像太小，可能是空帧: {} 字节",
        bytes.len()
    );
}

/// 写一张纯色 RGBA PNG（测试贴图用）。
fn write_test_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) {
    let file = std::fs::File::create(path).expect("建贴图文件");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().expect("写 PNG 头");
    writer.write_image_data(rgba).expect("写 PNG 像素");
}

/// 解码渲染输出的 PNG（RGBA8）。
fn decode_rgba(path: &std::path::Path) -> (u32, u32, Vec<u8>) {
    let file = std::fs::File::open(path).expect("打开 PNG");
    let mut reader = png::Decoder::new(std::io::BufReader::new(file))
        .read_info()
        .expect("读 PNG 头");
    assert_eq!(
        reader.output_color_type().0,
        png::ColorType::Rgba,
        "渲染输出应当是 RGBA8"
    );
    let size = reader.output_buffer_size().expect("输出缓冲大小");
    let mut buf = vec![0u8; size];
    let info = reader.next_frame(&mut buf).expect("解码 PNG");
    buf.truncate(info.line_size * info.height as usize);
    (info.width, info.height, buf)
}

/// 图像中心区块的平均颜色（避免背景把均值冲淡）。
fn center_mean(w: u32, h: u32, rgba: &[u8]) -> (f64, f64, f64) {
    let (mut r, mut g, mut b, mut n) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for y in (h / 2 - 10)..(h / 2 + 10) {
        for x in (w / 2 - 10)..(w / 2 + 10) {
            let i = ((y * w + x) * 4) as usize;
            r += rgba[i] as f64;
            g += rgba[i + 1] as f64;
            b += rgba[i + 2] as f64;
            n += 1.0;
        }
    }
    (r / n, g / n, b / n)
}

/// 材质贴图：路径相对**场景文件**解析，并且真的改变画面（`unlit` 保证只看贴图乘法）。
#[test]
fn material_textures_resolve_next_to_the_scene_and_change_pixels() {
    let doctor = run(&["doctor", "--json"]).json();
    let gpu = doctor["data"]["gpu_available"].as_bool().unwrap();

    let dir = temp_dir("texture");
    write_test_png(&dir.join("green.png"), 2, 2, &[0, 255, 0, 255].repeat(4));

    let scene = |texture: &str| {
        format!(
            r#"{{
                "materials": {{ "m": {{ "color": [1, 1, 1], "unlit": true{texture} }} }},
                "objects": [
                    {{ "name": "box", "shape": {{ "type": "box", "size": [1, 1, 1] }},
                       "body": "none", "material": "m" }}
                ],
                "camera": {{ "position": [0, 0, 6], "look_at": [0, 0, 0] }},
                "simulate": {{ "steps": 0 }},
                "render": {{ "output": "ignored.png", "width": 160, "height": 120, "msaa": 1 }}
            }}"#
        )
    };
    let plain = dir.join("plain.json");
    let textured = dir.join("textured.json");
    std::fs::write(&plain, scene("")).unwrap();
    std::fs::write(&textured, scene(r#", "base_color_texture": "green.png""#)).unwrap();

    // 缺贴图必须当场报错，并指到具体字段（以前是渲染成纯色糊过去）
    let missing = dir.join("missing.json");
    std::fs::write(&missing, scene(r#", "base_color_texture": "nope.png""#)).unwrap();
    let bad = run(&["validate", missing.to_str().unwrap(), "--json"]);
    assert_eq!(bad.code, 2, "缺贴图应当校验失败：{}", bad.stdout);
    // 契约：校验发现的问题走 data.diagnostics，退出码 2 表示"有问题"
    let bad_env = bad.json();
    assert_eq!(bad_env["ok"], true);
    assert_eq!(bad_env["data"]["valid"], false);
    let diags = &bad_env["data"]["diagnostics"];
    assert!(
        diags.as_array().unwrap().iter().any(|d| {
            d["path"] == "materials.m.base_color_texture"
                && d["severity"] == "error"
                && d["message"].as_str().unwrap().contains("找不到贴图文件")
        }),
        "诊断应当指向 materials.m.base_color_texture：{diags}"
    );

    if !gpu {
        // 没有 GPU 时前两步仍然验证了"路径解析 + 校验"，渲染部分按约定跳过
        return;
    }

    let plain_png = dir.join("plain.png");
    let textured_png = dir.join("textured.png");
    for (scene, out) in [(&plain, &plain_png), (&textured, &textured_png)] {
        let r = run(&[
            "render",
            scene.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    }

    let (w, h, plain_px) = decode_rgba(&plain_png);
    let (w2, h2, tex_px) = decode_rgba(&textured_png);
    assert_eq!((w, h), (w2, h2));
    let (pr, pg, pb) = center_mean(w, h, &plain_px);
    let (tr, tg, tb) = center_mean(w, h, &tex_px);
    assert!(
        pr > 200.0 && pg > 200.0 && pb > 200.0,
        "对照组应当是白色 unlit：({pr:.1}, {pg:.1}, {pb:.1})"
    );
    assert!(
        tg > 200.0 && tr < 80.0 && tb < 80.0,
        "贴图组应当是绿色（贴图真的被采样了）：({tr:.1}, {tg:.1}, {tb:.1})"
    );
}

#[test]
fn bench_reports_throughput_and_resource_usage() {
    let r = run(&[
        "bench",
        "--bodies",
        "50",
        "--steps",
        "30",
        "--warmup",
        "10",
        "--no-render",
        "--no-sleep",
        "--json",
    ]);
    assert_eq!(r.code, 0, "bench 应当成功：{}", r.stderr);
    let v = r.json();
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["command"], "bench");
    let data = &v["data"];
    assert!(data["threads"].as_u64().unwrap() >= 1);
    assert!(data["startup_ms"].as_f64().unwrap() >= 0.0);
    let scales = data["scales"].as_array().expect("要有 scales");
    assert_eq!(scales.len(), 1);
    let s = &scales[0];
    assert_eq!(s["bodies"], 50);
    assert_eq!(s["steps"], 30);
    assert!(s["ms_per_step"].as_f64().unwrap() > 0.0);
    assert!(s["body_steps_per_sec"].as_f64().unwrap() > 0.0);
    // Linux 上能读到内存；其它平台允许为 null
    if cfg!(target_os = "linux") {
        assert!(data["peak_rss_mb"].as_f64().unwrap() > 0.0, "{data}");
    }
    assert!(!data["notes"].as_array().unwrap().is_empty(), "要有备注");
}

#[test]
fn bench_rejects_bad_bodies_argument_with_exit_code_2() {
    let r = run(&["bench", "--bodies", "abc", "--no-render", "--json"]);
    assert_eq!(r.code, 2, "非法参数应当是退出码 2：{}", r.stdout);
    let v = r.json();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["kind"], "invalid");
    assert_eq!(v["command"], "bench");
}

#[test]
fn absurd_scene_sizes_are_rejected_with_clear_diagnostics() {
    // 步数超上限：给明确诊断而不是跑死
    let path = std::env::temp_dir().join("quench3d_huge_steps.json");
    std::fs::write(
        &path,
        r#"{"objects":[{"name":"a","shape":{"type":"sphere","radius":1}}],
            "simulate":{"steps":2000000}}"#,
    )
    .expect("写临时场景");
    let r = run(&["validate", path.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 2, "超上限应当退出码 2：{}", r.stdout);
    let v = r.json();
    assert_eq!(v["ok"], true, "validate 用 data.diagnostics 报告问题：{v}");
    assert_eq!(v["data"]["valid"], false, "{v}");
    let diags = v["data"]["diagnostics"].as_array().expect("要有诊断");
    assert!(
        diags
            .iter()
            .any(|d| d["path"].as_str() == Some("simulate.steps")),
        "诊断里要有 simulate.steps 路径：{diags:?}"
    );
    let _ = std::fs::remove_file(&path);
}

/// 物理材质接线（gaps #22）：静摩擦与摩擦合成规则**真的改变仿真结果**。
///
/// 只断言"字段解析成功了"是不够的：这两个参数的全部价值就是让箱子
/// "按住 / 滑走""滑得远 / 滑得近"，所以这里用 `simulate` 报告里的 `displacement`
/// 直接量它。场景不需要显卡。
#[test]
fn static_friction_and_combine_rule_change_the_simulation() {
    let dir = temp_dir("materials");
    // 水平推力 5000 N，箱子 1 m³、密度 1000 → 1000 kg → 折合 5 m/s²。
    // 地面摩擦 0.5（静摩擦跟随）。几何平均下 μ = sqrt(摩擦 × 0.5)。
    let scene = |obj_extra: serde_json::Value, physics: serde_json::Value| {
        let mut o = serde_json::json!({
            "name": "箱子", "shape": { "type": "box", "size": [1, 1, 1] },
            "body": "dynamic", "position": [0, 0.5, 0], "friction": 0.3
        });
        for (k, v) in obj_extra.as_object().expect("extra 要是对象") {
            o[k] = v.clone();
        }
        serde_json::json!({
            "gravity": [0.0, -9.81, 0.0],
            "ground": { "size": 40, "friction": 0.5 },
            "physics": physics,
            "objects": [o],
            "actions": [ { "type": "force", "target": "箱子", "at_step": 0, "steps": 120,
                           "value": [5000, 0, 0] } ],
            "simulate": { "steps": 150 }
        })
    };
    let displacement = |file: &str, doc: serde_json::Value| -> f64 {
        let path = write_scene(&dir, file, doc);
        let v = run(&["simulate", path.to_str().unwrap(), "--json"]).json();
        assert_eq!(v["ok"], true, "{file}: {v}");
        // **按名字取**：`states` 里第一个是地面（静态，永远零位移），
        // 直接取 `[0]` 会得到"位移恒为 0"的假证据（这条坑踩过一次）。
        v["data"]["states"]
            .as_array()
            .expect("要有 states")
            .iter()
            .find(|s| s["name"] == "箱子")
            .and_then(|s| s["displacement"].as_f64())
            .unwrap_or_else(|| panic!("{file} 缺 箱子 的 displacement: {v}"))
    };

    // 1. 静摩擦 1.0（μ_s = sqrt(1.0 × 0.5) ≈ 0.707 → 阈值 6.9 m/s² > 5）：按住
    let held = displacement(
        "held.json",
        scene(
            serde_json::json!({ "static_friction": 1.0 }),
            serde_json::json!({}),
        ),
    );
    // 2. 静摩擦 0.05（μ_s ≈ 0.158 → 阈值 1.6 m/s²）：滑走
    let slid = displacement(
        "slid.json",
        scene(
            serde_json::json!({ "static_friction": 0.05 }),
            serde_json::json!({}),
        ),
    );
    assert!(held < 0.05, "静摩擦 1.0 该按住，实际 {held:.4} m");
    assert!(slid > 0.5, "静摩擦 0.05 该滑走，实际 {slid:.4} m");

    // 3. 世界级合成规则：同样的场景，`min` 的摩擦比 `max` 小 → 滑得更远
    //    （箱子 0.3 / 地面 0.5，静摩擦设 0 让它一开始就在滑动，测的是动摩擦）
    let soft = displacement(
        "soft.json",
        scene(
            serde_json::json!({ "static_friction": 0.0 }),
            serde_json::json!({ "friction_combine": "min" }),
        ),
    );
    let hard = displacement(
        "hard.json",
        scene(
            serde_json::json!({ "static_friction": 0.0 }),
            serde_json::json!({ "friction_combine": "max" }),
        ),
    );
    assert!(
        soft > hard + 0.3,
        "取小的摩擦该滑得更远：min → {soft:.3} m，max → {hard:.3} m"
    );

    // 4. 物体级规则覆盖世界缺省：世界 `max`、只有箱子写 `min` → 与世界 `min` 一致
    let overridden = displacement(
        "override.json",
        scene(
            serde_json::json!({ "static_friction": 0.0, "friction_combine": "min" }),
            serde_json::json!({ "friction_combine": "max" }),
        ),
    );
    assert!(
        (overridden - soft).abs() < 1e-3,
        "物体级规则该盖过世界缺省：{overridden:.4} vs {soft:.4}"
    );
}

/// `wake` 动作的 `target: "*"`：一次唤醒**所有**睡着的刚体（引擎 `wake_up_all`）。
///
/// 为什么用"整场景都睡着的零重力场景"：它把睡眠这件事变成**确定**的
/// （静止 0.5 秒即入睡），于是"报告里 sleeping 从 2 变成 0"就是一条干净的行为证据。
/// 顺带钉住"`*` 只对 wake 有效"：别的动作写 `*` 必须报错，不能悄悄只作用在一个物体上。
#[test]
fn wake_action_with_star_wakes_every_sleeper() {
    let dir = temp_dir("wake_all");
    let scene = |actions: serde_json::Value| {
        serde_json::json!({
            "gravity": [0.0, 0.0, 0.0],
            "objects": [
                { "name": "a", "shape": { "type": "box", "size": [1, 1, 1] },
                  "body": "dynamic", "position": [0, 0, 0] },
                { "name": "b", "shape": { "type": "box", "size": [1, 1, 1] },
                  "body": "dynamic", "position": [4, 0, 0] }
            ],
            "actions": actions,
            "simulate": { "steps": 120 }
        })
    };

    // 1. 没有动作：两个盒子一直静止 → 都睡着
    let plain = write_scene(&dir, "plain.json", scene(serde_json::json!([])));
    let v = run(&["simulate", plain.to_str().unwrap(), "--json"]).json();
    assert_eq!(v["data"]["sleeping"], 2, "前提：两个都该睡着：{v}");

    // 2. 第 119 步唤醒全部：最后一步跑完时它们刚醒，来不及再睡
    let woken = write_scene(
        &dir,
        "woken.json",
        scene(serde_json::json!([
            { "type": "wake", "target": "*", "at_step": 119 }
        ])),
    );
    let v = run(&["simulate", woken.to_str().unwrap(), "--json"]).json();
    assert_eq!(
        v["data"]["sleeping"], 0,
        "`target: \"*\"` 应当唤醒全部：{v}"
    );
    for st in v["data"]["states"].as_array().expect("要有 states") {
        assert_eq!(st["asleep"], false, "每个都该是醒的：{st}");
    }

    // 3. 只唤醒其中一个：另一个仍然是睡着的（证明 `*` 是"全部"而不是"碰巧动了谁"）
    let one = write_scene(
        &dir,
        "one.json",
        scene(serde_json::json!([
            { "type": "wake", "target": "a", "at_step": 119 }
        ])),
    );
    let v = run(&["simulate", one.to_str().unwrap(), "--json"]).json();
    assert_eq!(v["data"]["sleeping"], 1, "只唤醒 a：{v}");
    let states = v["data"]["states"].as_array().unwrap();
    let a = states.iter().find(|s| s["name"] == "a").unwrap();
    let b = states.iter().find(|s| s["name"] == "b").unwrap();
    assert_eq!(a["asleep"], false, "a 应当被唤醒");
    assert_eq!(b["asleep"], true, "b 不该受影响");

    // 4. `*` 只对 wake 有效：别的动作写 `*` 要报错（不能悄悄只作用于一个物体）
    let bad = write_scene(
        &dir,
        "bad.json",
        scene(serde_json::json!([
            { "type": "impulse", "target": "*", "value": [0.0, 1.0, 0.0], "at_step": 1 }
        ])),
    );
    let v = run(&["validate", bad.to_str().unwrap(), "--json"]).json();
    assert_eq!(v["data"]["valid"], false, "impulse 配 `*` 必须不合法：{v}");
    let paths: Vec<&str> = v["data"]["diagnostics"]
        .as_array()
        .expect("要有诊断")
        .iter()
        .map(|d| d["path"].as_str().unwrap())
        .collect();
    assert!(
        paths.contains(&"actions[0].target"),
        "诊断要指向 target：{paths:?}"
    );
    let rr = run(&["simulate", bad.to_str().unwrap(), "--json"]);
    assert_eq!(rr.code, 2, "非法场景不该跑仿真：{}", rr.stdout);
}

/// 光照补全（`lighting.fog` / `lighting.shadow_quality` / `lighting.sky_colors`）接线：
/// 三节都能校验、能渲染、**不写 = 关闭**（逐位相同），而且真的改变画面。
///
/// 为什么值得端到端测：这些字段引擎里本来就有，只是格式没暴露 —— 接错了最典型的失败
/// 是"字段能写、校验能过、像素一点没变"（静默失效），本地单测抓不到这一层。
#[test]
fn fog_and_shadow_quality_are_wired_to_the_renderer() {
    let dir = temp_dir("lighting_extras");
    let scene = |out: &str, extra: serde_json::Value| {
        // 地板 + 悬空遮挡物：遮挡物的影子会落在地板上（否则"阴影强度"无从观察）
        serde_json::json!({
            "objects": [
                { "name": "地板", "shape": { "type": "box", "size": [24, 0.5, 24] },
                  "position": [0, -0.25, 0], "material": "grey" },
                { "name": "浮箱", "shape": { "type": "box", "size": [2, 1.6, 2] },
                  "position": [0, 0.8, 0], "material": "warm" }
            ],
            "materials": {
                "grey": { "color": [0.85, 0.85, 0.85], "roughness": 0.9 },
                "warm": { "color": [0.9, 0.6, 0.3], "roughness": 0.7 }
            },
            "camera": { "position": [0, 5, 8], "look_at": [0, 0, 0] },
            "render": { "output": out, "width": 192, "height": 128, "msaa": 1 },
            "lighting": extra
        })
    };

    let plain = write_scene(
        &dir,
        "plain.json",
        scene(
            "plain.png",
            serde_json::json!({ "sky": false, "grid": false }),
        ),
    );
    let fog_off = write_scene(
        &dir,
        "fog-off.json",
        scene(
            "fog-off.png",
            serde_json::json!({ "sky": false, "grid": false,
                                "fog": { "color": [1.0, 0.0, 0.0], "density": 0.0 } }),
        ),
    );
    let fog_on = write_scene(
        &dir,
        "fog-on.json",
        scene(
            "fog-on.png",
            serde_json::json!({ "sky": false, "grid": false,
                                "fog": { "color": [1.0, 0.0, 0.0], "density": 0.35 } }),
        ),
    );
    let shadow_none = write_scene(
        &dir,
        "shadow-none.json",
        scene(
            "shadow-none.png",
            serde_json::json!({ "sky": false, "grid": false, "shadows": true,
                                "shadow_quality": { "strength": 0.0, "radius": 20 } }),
        ),
    );
    let shadow_full = write_scene(
        &dir,
        "shadow-full.json",
        scene(
            "shadow-full.png",
            serde_json::json!({ "sky": false, "grid": false, "shadows": true,
                                "shadow_quality": { "strength": 1.0, "radius": 20 } }),
        ),
    );
    let sky_colors = write_scene(
        &dir,
        "sky-colors.json",
        scene(
            "sky-colors.png",
            serde_json::json!({ "sky": true, "grid": false,
                                "sky_colors": { "zenith": [0.9, 0.1, 0.1], "horizon": [1.0, 0.5, 0.0] } }),
        ),
    );

    // 1. 校验：这几节都要能过（不需要 GPU）
    for path in [
        &plain,
        &fog_off,
        &fog_on,
        &shadow_none,
        &shadow_full,
        &sky_colors,
    ] {
        let v = run(&["validate", path.to_str().unwrap(), "--json"]).json();
        assert_eq!(v["data"]["valid"], true, "场景应当合法：{v}");
    }

    // 2. 非法数值必须在**打开 GPU 之前**被拦下，并且路径指向具体字段
    let bad = write_scene(
        &dir,
        "bad.json",
        scene(
            "bad.png",
            serde_json::json!({ "fog": { "color": [-1.0, 0.2, 0.2], "density": -0.5 },
                                "shadow_quality": { "strength": 3.0, "radius": 0.0 },
                                "sky_colors": { "horizon": [0.5, -0.1, 0.5] } }),
        ),
    );
    let v = run(&["validate", bad.to_str().unwrap(), "--json"]).json();
    assert_eq!(v["data"]["valid"], false, "非法光照必须判为不合法：{v}");
    let paths: Vec<&str> = v["data"]["diagnostics"]
        .as_array()
        .expect("要有诊断")
        .iter()
        .map(|d| d["path"].as_str().unwrap())
        .collect();
    for expected in [
        "lighting.fog.color",
        "lighting.fog.density",
        "lighting.shadow_quality.strength",
        "lighting.shadow_quality.radius",
        "lighting.sky_colors.horizon",
    ] {
        assert!(paths.contains(&expected), "缺少 {expected} 诊断：{paths:?}");
    }
    let rr = run(&["render", bad.to_str().unwrap(), "--json"]);
    assert_eq!(rr.code, 2, "非法场景不该打开 GPU：{}", rr.stdout);

    // 3. 拼错的字段要报未知字段（不被静默忽略）
    let typo = write_scene(
        &dir,
        "typo.json",
        scene("typo.png", serde_json::json!({ "fog": { "densty": 0.1 } })),
    );
    let v = run(&["validate", typo.to_str().unwrap(), "--json"]).json();
    assert_eq!(v["ok"], false, "拼错的字段必须报错：{v}");

    // 4. 像素：不写 = 关闭（逐位相同），写了就真的变
    let doctor = run(&["doctor", "--json"]).json();
    if !doctor["data"]["gpu_available"].as_bool().unwrap() {
        eprintln!("跳过渲染断言：没有可用 GPU");
        return;
    }
    // 输出走 `--out`（绝对路径）：场景里的 `render.output` 是相对量，别写到仓库目录里。
    for (path, out) in [
        (&plain, "plain.png"),
        (&fog_off, "fog-off.png"),
        (&fog_on, "fog-on.png"),
        (&shadow_none, "shadow-none.png"),
        (&shadow_full, "shadow-full.png"),
        (&sky_colors, "sky-colors.png"),
    ] {
        let out = dir.join(out);
        let r = run(&[
            "render",
            path.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(r.code, 0, "渲染失败：{}", r.stderr);
    }

    let (w, h, plain_px) = decode_rgba(&dir.join("plain.png"));
    let (_, _, fog_off_px) = decode_rgba(&dir.join("fog-off.png"));
    let (_, _, fog_on_px) = decode_rgba(&dir.join("fog-on.png"));
    let (_, _, shadow_none_px) = decode_rgba(&dir.join("shadow-none.png"));
    let (_, _, shadow_full_px) = decode_rgba(&dir.join("shadow-full.png"));
    let (_, _, sky_px) = decode_rgba(&dir.join("sky-colors.png"));

    assert_eq!(
        plain_px, fog_off_px,
        "雾密度 0 与不写 `lighting.fog` 必须逐位相同（「不写 = 关闭」不能是句空话）"
    );
    assert_ne!(fog_on_px, plain_px, "开了雾必须改变画面");
    assert_ne!(shadow_full_px, shadow_none_px, "阴影强度必须改变画面");
    assert_ne!(sky_px, plain_px, "自定义天空颜色必须改变画面");

    // 统计工具：整幅图的均值、明显不同的像素数、亮度总和
    let mean_rgb = |img: &[u8]| -> (f64, f64, f64) {
        let (mut r, mut g, mut b) = (0.0f64, 0.0f64, 0.0f64);
        let n = (img.len() / 4) as f64;
        for px in img.as_chunks::<4>().0 {
            r += px[0] as f64;
            g += px[1] as f64;
            b += px[2] as f64;
        }
        (r / n, g / n, b / n)
    };
    let diff_count = |a: &[u8], b: &[u8]| -> usize {
        a.as_chunks::<4>()
            .0
            .iter()
            .zip(b.as_chunks::<4>().0)
            .filter(|(pa, pb)| (0..3).any(|c| pa[c].abs_diff(pb[c]) > 8))
            .count()
    };
    let lum_sum = |img: &[u8]| -> i64 {
        img.as_chunks::<4>()
            .0
            .iter()
            .map(|px| px[0] as i64 + px[1] as i64 + px[2] as i64)
            .sum()
    };

    // 红色浓雾：画面整体应当更红（R 相对 B 的差增大），而且变化面积不小
    let (pr, _, pb) = mean_rgb(&plain_px);
    let (fr, _, fb) = mean_rgb(&fog_on_px);
    assert!(
        (fr - fb) > (pr - pb) + 5.0,
        "红色雾应当把画面推向红色：不写时 R-B={:.1}，开雾后 R-B={:.1}",
        pr - pb,
        fr - fb
    );
    let fog_changed = diff_count(&plain_px, &fog_on_px);
    assert!(
        fog_changed > (w * h / 20) as usize,
        "雾只改了 {fog_changed} 个像素（共 {}），像是没接上",
        w * h
    );

    // 阴影强度：拉满之后整体更暗，且暗化只发生在被遮住的那块地上
    assert!(
        lum_sum(&shadow_full_px) < lum_sum(&shadow_none_px),
        "阴影强度拉满应当让画面更暗"
    );
    let shadow_changed = diff_count(&shadow_none_px, &shadow_full_px);
    assert!(
        shadow_changed > 24,
        "阴影强度只改了 {shadow_changed} 个像素：影子没画上？"
    );

    // 像素级细节（"只暗在影子里"）由 `quench3d-render/tests/fog_and_shadow_quality.rs` 覆盖，
    // 这里只保证"从场景文件一路传到渲染器"这条链是通的。
}

#[test]
fn malformed_overrides_are_rejected_instead_of_silently_doing_nothing() {
    let scene = std::env::temp_dir().join("quench3d_override_guard.json");
    std::fs::write(
        &scene,
        r#"{"objects":[{"name":"球","shape":{"type":"sphere","radius":0.5},"position":[0,3,0]}]}"#,
    )
    .expect("写临时场景");
    let path = scene.to_str().unwrap();

    // dt：0 / 负数 / NaN / 超过 1 秒，都必须拒绝（否则会返回一个「什么都没算」的成功报告）
    for bad in ["0", "-0.5", "nan", "5"] {
        let r = run(&["simulate", path, "--dt", bad, "--json"]);
        assert_eq!(r.code, 2, "--dt {bad} 应当退出码 2：{}", r.stdout);
        let v = r.json();
        assert_eq!(v["ok"], false, "--dt {bad}");
        assert_eq!(v["error"]["kind"], "invalid", "--dt {bad}");
        assert!(
            v["error"]["hint"].as_str().unwrap_or("").contains("/60")
                || v["error"]["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("有限数"),
            "--dt {bad} 的诊断要能看懂：{v}"
        );
    }

    // 合法 dt 仍然可用
    let r = run(&["simulate", path, "--dt", "0.02", "--json"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.json()["ok"], true);

    // 分辨率：0 与超大值都拒绝，避免打满显存/内存
    for (w, h) in [("0", "600"), ("99999", "600"), ("1920", "0")] {
        let r = run(&["simulate", path, "--width", w, "--height", h, "--json"]);
        assert_eq!(r.code, 2, "{w}×{h} 应当退出码 2：{}", r.stdout);
        assert_eq!(r.json()["error"]["kind"], "invalid", "{w}×{h}");
    }

    let _ = std::fs::remove_file(&scene);
}

/// 仿真视频：帧序列 + trace.jsonl，且同场景两次录制必须逐像素一致。
#[test]
fn movie_records_deterministic_frame_sequence() {
    let doctor = run(&["doctor", "--json"]).json();
    let gpu = doctor["data"]["gpu_available"].as_bool().unwrap();

    let dir = temp_dir("movie");
    let scene = dir.join("s.json");
    std::fs::write(
        &scene,
        r#"{
            "ground": { "size": 20, "material": "floor" },
            "materials": { "floor": { "color": [0.5, 0.52, 0.56], "roughness": 0.9 } },
            "objects": [
                { "name": "箱", "shape": { "type": "box", "size": [1, 1, 1] },
                  "position": [0, 3, 0], "body": "dynamic", "density": 500 }
            ],
            "camera": { "position": [6, 3.5, 7], "look_at": [0, 0.8, 0] },
            "render": { "width": 320, "height": 240, "msaa": 1 }
        }"#,
    )
    .unwrap();
    let path = scene.to_str().unwrap();

    let dir_a = dir.join("a");
    let dir_b = dir.join("b");
    let a = run(&[
        "movie",
        path,
        "--frames",
        "3",
        "--frames-dir",
        dir_a.to_str().unwrap(),
        "--json",
    ]);
    if !gpu {
        assert_eq!(a.code, 3, "没有 GPU 时必须用退出码 3");
        assert_eq!(a.json()["error"]["kind"], "environment");
        return;
    }
    assert_eq!(a.code, 0, "stderr={} stdout={}", a.stderr, a.stdout);
    let data = a.json()["data"].clone();
    assert_eq!(data["frames"], 3);
    assert_eq!(data["total_steps"], 3, "3 帧 × 1 步/帧");
    assert_eq!(data["warmup_frames"], 0);
    assert!(
        (data["duration"].as_f64().unwrap() - 0.05).abs() < 1e-4,
        "3 帧 @1/60 应当是 0.05 秒：{}",
        data["duration"]
    );
    assert!(
        (data["fps"].as_f64().unwrap() - 60.0).abs() < 1e-3,
        "缺省帧率应当是等价帧率 60：{}",
        data["fps"]
    );
    assert!(
        data["ffmpeg"].as_str().unwrap().contains("yuv420p"),
        "建议命令必须写得开（yuv420p）：{}",
        data["ffmpeg"]
    );

    // 三帧都是合法 PNG，且 trace.jsonl 一行一帧、时间与步数单调
    for i in 0..3 {
        let png = dir_a.join(format!("frame_{i:05}.png"));
        let bytes = std::fs::read(&png).unwrap_or_else(|e| panic!("缺帧 {png:?}: {e}"));
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "第 {i} 帧不是 PNG");
    }
    let trace = std::fs::read_to_string(dir_a.join("trace.jsonl")).expect("trace.jsonl 应当存在");
    let rows: Vec<serde_json::Value> = trace
        .lines()
        .map(|l| serde_json::from_str(l).expect("每行都是 JSON"))
        .collect();
    assert_eq!(rows.len(), 3, "一帧一行");
    assert_eq!(rows[0]["steps"], 1);
    assert_eq!(rows[2]["steps"], 3);
    assert!(
        rows[2]["time"].as_f64().unwrap() > rows[0]["time"].as_f64().unwrap(),
        "仿真时间必须前进"
    );

    // 确定性：换一个目录重录，像素摘要必须完全相同（这是「仿真视频」可复现的硬证据）
    let b = run(&[
        "movie",
        path,
        "--frames",
        "3",
        "--frames-dir",
        dir_b.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(b.code, 0, "{}", b.stdout);
    assert_eq!(
        b.json()["data"]["digest"],
        data["digest"],
        "同一场景两次录制的像素摘要必须一致"
    );

    // 断点续录：重跑同一目录，全部跳过而不是重写
    let again = run(&[
        "movie",
        path,
        "--frames",
        "3",
        "--frames-dir",
        dir_a.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(again.code, 0, "{}", again.stdout);
    assert_eq!(again.json()["data"]["skipped"], 3);
    assert_eq!(again.json()["data"]["bytes"], 0, "跳过的帧不写盘");

    // 参数校验：帧数为 0、以及 simulate 专用参数混用，都必须报错而不是默默忽略
    for bad in [
        vec!["--frames", "0"],
        vec!["--frames", "100001"],
        vec!["--steps-per-frame", "0"],
        vec!["--fps", "0"],
        vec!["--steps", "10"],
        vec!["--trace-all"],
    ] {
        let mut args = vec![
            "movie",
            path,
            "--frames-dir",
            dir_a.to_str().unwrap(),
            "--json",
        ];
        args.extend(bad.iter().copied());
        let r = run(&args);
        assert_eq!(r.code, 2, "{bad:?} 应当退出码 2：{}", r.stdout);
        assert_eq!(r.json()["error"]["kind"], "invalid", "{bad:?}");
    }

    // 预热帧：只推进不输出（首帧的累计步数应当已经含预热）
    let warm = run(&[
        "movie",
        path,
        "--frames",
        "2",
        "--warmup",
        "3",
        "--frames-dir",
        dir.join("warm").to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(warm.code, 0, "{}", warm.stdout);
    let wdata = warm.json()["data"].clone();
    assert_eq!(wdata["frames"], 2);
    assert_eq!(wdata["total_steps"], 5, "预热 3 + 输出 2");
    assert_eq!(wdata["first_frame"]["steps"], 4, "首帧已含 3 帧预热");

    // render.enabled=false 与「录视频」冲突：必须明确报错，而不是照样渲染一堆帧
    let disabled = dir.join("disabled.json");
    std::fs::write(
        &disabled,
        r#"{"objects":[{"name":"箱","shape":{"type":"box","size":[1,1,1]},"position":[0,3,0],"body":"dynamic"}],
            "render":{"enabled":false}}"#,
    )
    .unwrap();
    let r = run(&[
        "movie",
        disabled.to_str().unwrap(),
        "--frames",
        "2",
        "--frames-dir",
        dir.join("disabled_frames").to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(r.code, 2, "{}", r.stdout);
    assert_eq!(r.json()["error"]["kind"], "scene");
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("render.enabled"),
        "诊断要指出具体字段：{}",
        r.stdout
    );
    assert!(
        !dir.join("disabled_frames").exists(),
        "报错时不应留下帧目录"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 内置编码器：GIF / APNG 都要产出**真的能播**的单文件，且像素与 PNG 序列一致。
#[test]
fn movie_builtin_encoders_produce_playable_files() {
    let doctor = run(&["doctor", "--json"]).json();
    let gpu = doctor["data"]["gpu_available"].as_bool().unwrap();

    let dir = temp_dir("movie_enc");
    let scene = dir.join("s.json");
    std::fs::write(
        &scene,
        r#"{
            "ground": { "size": 20, "material": "floor" },
            "materials": { "floor": { "color": [0.5, 0.52, 0.56], "roughness": 0.9 } },
            "objects": [
                { "name": "箱", "shape": { "type": "box", "size": [1, 1, 1] },
                  "position": [0, 3, 0], "body": "dynamic", "density": 500 }
            ],
            "camera": { "position": [6, 3.5, 7], "look_at": [0, 0.8, 0] },
            "render": { "width": 160, "height": 120, "msaa": 1 }
        }"#,
    )
    .unwrap();
    let path = scene.to_str().unwrap();

    // 参照组：PNG 序列的像素摘要（编码器只换壳，摘要必须一致）
    let png_dir = dir.join("png");
    let png = run(&[
        "movie",
        path,
        "--frames",
        "4",
        "--frames-dir",
        png_dir.to_str().unwrap(),
        "--json",
    ]);
    if !gpu {
        assert_eq!(png.code, 3, "没有 GPU 时必须用退出码 3");
        assert_eq!(png.json()["error"]["kind"], "environment");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }
    assert_eq!(png.code, 0, "{}", png.stdout);
    let png_digest = png.json()["data"]["digest"].as_str().unwrap().to_string();
    assert_eq!(png.json()["data"]["encoder"], "png");
    assert_eq!(png.json()["data"]["output"], "");

    // --- GIF ---
    let gif_path = dir.join("demo.gif");
    let g = run(&[
        "movie",
        path,
        "--frames",
        "4",
        "--encoder",
        "gif",
        "--out",
        gif_path.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(g.code, 0, "stderr={} stdout={}", g.stderr, g.stdout);
    let gdata = g.json()["data"].clone();
    assert_eq!(gdata["encoder"], "gif");
    assert_eq!(gdata["frames"], 4);
    assert_eq!(
        gdata["digest"], png_digest,
        "换编码器不能改变像素：GIF 的摘要必须与 PNG 序列一致"
    );
    assert!(
        gdata["ffmpeg"].as_str().unwrap().is_empty(),
        "内置编码器不需要 ffmpeg 命令"
    );
    // 进程的工作目录是 crate 根，`--frames-dir` 缺省就是那里的 `frames/`：
    // 单文件编码器不该建出这个空目录（曾经会，只留一个孤零零的 trace）。
    assert!(
        !std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("frames")
            .exists(),
        "单文件编码器不该建出 PNG 帧目录"
    );
    let gif = std::fs::read(&gif_path).expect("应当写出 GIF");
    assert_eq!(&gif[..6], b"GIF89a", "GIF 魔数");
    assert_eq!(*gif.last().unwrap(), 0x3b, "GIF 结束标记");
    assert!(gif.len() > 1000, "4 帧 GIF 不该只有 {} 字节", gif.len());
    assert_eq!(
        gdata["bytes"].as_u64().unwrap(),
        gif.len() as u64,
        "报告的字节数必须是文件真实大小"
    );
    // trace 跟着产物走，不落在 frames/ 里
    assert!(
        dir.join("demo.trace.jsonl").exists(),
        "trace 应当与输出文件同目录同名"
    );

    // --- APNG ---
    let apng_path = dir.join("demo.png");
    let a = run(&[
        "movie",
        path,
        "--frames",
        "4",
        "--encoder",
        "apng",
        "--out",
        apng_path.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(a.code, 0, "stderr={} stdout={}", a.stderr, a.stdout);
    let adata = a.json()["data"].clone();
    assert_eq!(adata["encoder"], "apng");
    assert_eq!(adata["digest"], png_digest);
    let apng = std::fs::read(&apng_path).expect("应当写出 APNG");
    assert_eq!(&apng[..8], b"\x89PNG\r\n\x1a\n", "APNG 也是 PNG 容器");
    // acTL：帧数 + 循环次数（0 = 无限）
    let actl = apng
        .windows(4)
        .position(|w| w == b"acTL")
        .expect("APNG 必须有 acTL 块");
    let num_frames = u32::from_be_bytes(apng[actl + 4..actl + 8].try_into().unwrap());
    let num_plays = u32::from_be_bytes(apng[actl + 8..actl + 12].try_into().unwrap());
    assert_eq!(num_frames, 4, "acTL.num_frames 必须等于输出帧数");
    assert_eq!(num_plays, 0, "0 = 无限循环");
    // 首帧 IDAT + 其余 fdAT（APNG 规范要求）
    assert_eq!(apng.windows(4).filter(|w| *w == b"IDAT").count(), 1);
    assert_eq!(apng.windows(4).filter(|w| *w == b"fdAT").count(), 3);

    // 不给 --out：缺省输出落在**当前工作目录**（曾经默认落进被禁用的 frames/ 直接报错）
    let def = run(&["movie", path, "--frames", "2", "--encoder", "gif", "--json"]);
    assert_eq!(def.code, 0, "stderr={} stdout={}", def.stderr, def.stdout);
    let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let default_file = crate_dir.join("movie.gif");
    assert!(default_file.exists(), "缺省输出应当是 ./movie.gif");
    assert!(
        def.json()["data"]["output"]
            .as_str()
            .unwrap()
            .ends_with("movie.gif"),
        "报告要写出真实的缺省路径：{}",
        def.stdout
    );
    let _ = std::fs::remove_file(&default_file);
    let _ = std::fs::remove_file(crate_dir.join("movie.trace.jsonl"));

    // --- 参数校验：意图冲突要明确报错，而不是猜 ---
    for bad in [
        // 单文件编码器 + 帧目录
        vec![
            "--frames-dir",
            dir.join("x").to_str().unwrap(),
            "--encoder",
            "gif",
        ],
        // PNG 序列 + 单文件输出
        vec!["--out", dir.join("y.png").to_str().unwrap()],
        // 未知编码器
        vec![
            "--encoder",
            "webm",
            "--out",
            dir.join("y.webm").to_str().unwrap(),
        ],
    ] {
        let mut args = vec!["movie", path, "--frames", "2"];
        args.extend(bad.iter().copied());
        args.push("--json");
        let r = run(&args);
        assert_eq!(r.code, 2, "{bad:?} 应当退出码 2：{}", r.stdout);
        assert_eq!(r.json()["error"]["kind"], "invalid", "{bad:?}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn render_refuses_scene_that_disables_rendering() {
    // render.enabled 以前是死字段（写 false 也照渲染）；现在与命令意图冲突时明确报错，
    // 而且这条检查在碰 GPU 之前就发生，所以没有 GPU 的机器也会走到
    let dir = temp_dir("render_disabled");
    let scene = dir.join("s.json");
    let png = dir.join("x.png");
    std::fs::write(
        &scene,
        r#"{"objects":[{"name":"箱","shape":{"type":"box","size":[1,1,1]},
             "position":[0,3,0],"body":"dynamic"}],"render":{"enabled":false}}"#,
    )
    .unwrap();

    let r = run(&[
        "render",
        scene.to_str().unwrap(),
        "--out",
        png.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(r.code, 2, "{}", r.stdout);
    assert_eq!(r.json()["error"]["kind"], "scene");
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("render.enabled"),
        "诊断要指出具体字段：{}",
        r.stdout
    );
    assert!(!png.exists(), "报错时不应写图");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Bloom 接线：**不写** `lighting.bloom` 与显式 `enabled: false` 必须逐位相同；
/// 打开后亮块周围要出现单调衰减的晕圈；非法/可疑数值在校验阶段就指出具体字段。
///
/// 晕圈必须"只加在亮区周围"是 bloom 的**定义**：如果整幅图都被抬亮，那是把
/// 亮度提取写成了全屏提亮；如果亮区中心过曝成一片白，那是把 bloom 加在了
/// tonemap **之后**（本引擎加在之前，所以不会）。
#[test]
fn bloom_is_wired_to_the_renderer() {
    let dir = temp_dir("bloom");
    let scene = |out: &str, bloom: Option<serde_json::Value>| {
        let mut lighting = serde_json::json!({
            "preset": "none", "ambient": 0.0, "sky": false, "grid": false
        });
        if let Some(b) = bloom {
            lighting["bloom"] = b;
        }
        serde_json::json!({
            "objects": [{
                "name": "glow",
                "shape": { "type": "box", "size": [0.8, 0.8, 0.8] },
                "body": "none",
                "material": "glow"
            }],
            "materials": {
                "glow": { "color": [0, 0, 0], "emissive": 14.0,
                          "emissive_color": [1, 1, 1], "unlit": true }
            },
            "camera": { "position": [0, 0, 6], "look_at": [0, 0, 0] },
            "simulate": { "steps": 0 },
            "render": { "output": out, "width": 192, "height": 128, "msaa": 1 },
            "lighting": lighting
        })
    };

    let none = write_scene(&dir, "none.json", scene("none.png", None));
    let off = write_scene(
        &dir,
        "off.json",
        scene(
            "off.png",
            Some(serde_json::json!({ "enabled": false, "intensity": 4.0 })),
        ),
    );
    let on = write_scene(
        &dir,
        "on.json",
        scene(
            "on.png",
            Some(serde_json::json!({ "intensity": 1.6, "threshold": 0.5 })),
        ),
    );

    for path in [&none, &off, &on] {
        let v = run(&["validate", path.to_str().unwrap(), "--json"]).json();
        assert_eq!(v["data"]["valid"], true, "场景应当合法：{v}");
    }

    let doctor = run(&["doctor", "--json"]).json();
    if !doctor["data"]["gpu_available"].as_bool().unwrap() {
        eprintln!("跳过渲染断言：没有可用 GPU");
        return;
    }
    for (path, out) in [(&none, "none.png"), (&off, "off.png"), (&on, "on.png")] {
        let out = dir.join(out);
        let r = run(&[
            "render",
            path.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(r.code, 0, "渲染失败：{}", r.stderr);
    }

    let (w, h, a) = decode_rgba(&dir.join("none.png"));
    let (_, _, b) = decode_rgba(&dir.join("off.png"));
    let (_, _, c) = decode_rgba(&dir.join("on.png"));
    assert_eq!(
        a, b,
        "不写 bloom 与显式关闭必须逐位相同（缺省不能悄悄改画面）"
    );
    assert_ne!(a, c, "打开 bloom 必须改变画面");

    // 找亮块的右边缘（中心行上最后一个接近饱和的像素）
    let row = h / 2;
    let bright = |x: u32| {
        let i = ((row * w + x) * 4) as usize;
        a[i] > 200
    };
    let edge = (0..w)
        .filter(|&x| bright(x))
        .max()
        .expect("亮块应当在画面里");
    assert!(edge + 16 < w, "亮块右边要留出背景，实测边缘 x={edge}");

    // 紧贴边缘外的一带：开 bloom 必须明显变亮
    let band = |img: &[u8], from: u32, to: u32| -> f64 {
        let mut sum = 0.0f64;
        let mut n = 0.0f64;
        for x in from..to {
            let i = ((row * w + x) * 4) as usize;
            sum += img[i] as f64;
            n += 1.0;
        }
        sum / n.max(1.0)
    };
    let near_off = band(&a, edge + 2, edge + 8);
    let near_on = band(&c, edge + 2, edge + 8);
    let far_on = band(&c, edge + 14, edge + 20);
    assert!(
        near_on > near_off + 4.0,
        "亮区外侧没有被 bloom 提亮：关={near_off:.1} 开={near_on:.1}"
    );
    assert!(
        near_on > far_on,
        "晕圈没有向外衰减：近处 {near_on:.1} 应高于远处 {far_on:.1}"
    );

    // 远角（离亮块 ~90 像素，远超半分辨率高斯的半径）：不该被抬起
    let corner = |img: &[u8]| -> f64 {
        let mut sum = 0.0f64;
        let mut n = 0.0f64;
        for y in 0..8u32 {
            for x in 0..8u32 {
                let i = ((y * w + x) * 4) as usize;
                sum += img[i] as f64;
                n += 1.0;
            }
        }
        sum / n
    };
    let (c_off, c_on) = (corner(&a), corner(&c));
    assert!(
        (c_on - c_off).abs() <= 2.0,
        "bloom 不该抬亮整幅图：远角 关={c_off:.1} 开={c_on:.1}"
    );

    // 数值校验：负数报错、0 与过大阈值给警告，且路径指向具体字段
    let bad = write_scene(
        &dir,
        "bad.json",
        scene(
            "bad.png",
            Some(serde_json::json!({ "intensity": -1.0, "threshold": 50.0 })),
        ),
    );
    let v = run(&["validate", bad.to_str().unwrap(), "--json"]).json();
    assert_eq!(v["data"]["valid"], false, "负强度必须判为不合法：{v}");
    let diags = v["data"]["diagnostics"].as_array().expect("要有诊断");
    let paths: Vec<&str> = diags.iter().map(|d| d["path"].as_str().unwrap()).collect();
    assert!(
        paths.contains(&"lighting.bloom.intensity"),
        "诊断要指向具体字段：{paths:?}"
    );
    assert!(
        paths.contains(&"lighting.bloom.threshold"),
        "过大的阈值也要提醒：{paths:?}"
    );
    let rr = run(&["render", bad.to_str().unwrap(), "--json"]);
    assert_eq!(rr.code, 2, "非法场景要在打开 GPU 之前拦住");

    // 每个字段都写错名字时要报未知字段，而不是静默忽略
    let typo = write_scene(
        &dir,
        "typo.json",
        scene("typo.png", Some(serde_json::json!({ "knee": 1.0 }))),
    );
    let v = run(&["validate", typo.to_str().unwrap(), "--json"]).json();
    // 整个文档解析失败时走 error.diagnostics（与"场景能解析但语义不合法"是两条路）
    assert_eq!(v["ok"], false, "拼错的字段必须报错：{v}");
    let msgs: Vec<&str> = v["error"]["diagnostics"]
        .as_array()
        .expect("要有诊断")
        .iter()
        .map(|d| d["message"].as_str().unwrap())
        .collect();
    assert!(
        msgs.iter().any(|m| m.contains("knee")),
        "报错要提到拼错的字段名：{msgs:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 写一个场景 JSON 并返回路径。
fn write_scene(dir: &std::path::Path, name: &str, value: serde_json::Value) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&value).expect("序列化场景"),
    )
    .expect("写场景文件");
    path
}

/// 接近纯白的像素数（用来量"箱子在屏幕上占多大"）。
fn bright_pixels(rgba: &[u8]) -> usize {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > 200 && p[1] > 200 && p[2] > 200)
        .count()
}

/// 点光源接线：**不写**字段 = 保留预设补光，写 `[]` = 显式清空，给一盏红灯 = 画面真的变红。
///
/// 这条链路此前是坏的：Rust 侧把点光源写成两个并列数组（SoA），WGSL 侧却是结构体数组
/// （AoS），布局错位导致只有一盏灯时完全不亮、多盏灯时颜色取的是下一盏灯的位置。
#[test]
fn point_lights_are_wired_to_the_renderer() {
    let dir = temp_dir("point_lights");
    let scene = |out: &str, preset: &str, lights: Option<serde_json::Value>| {
        let mut lighting = serde_json::json!({ "preset": preset });
        if let Some(l) = lights {
            lighting["point_lights"] = l;
        }
        serde_json::json!({
            "objects": [{ "name": "box", "shape": { "type": "box", "size": [1, 1, 1] },
                          "body": "none", "material": "m" }],
            "materials": { "m": { "color": [1, 1, 1], "roughness": 0.5 } },
            "camera": { "position": [0, 0, 6], "look_at": [0, 0, 0] },
            "simulate": { "steps": 0 },
            "render": { "output": out, "width": 160, "height": 120, "msaa": 1 },
            "lighting": lighting
        })
    };
    // ① studio 预设自带两盏补光：写了 `[]` 就应当把它们清掉，画面必然不同
    let preset = write_scene(&dir, "preset.json", scene("preset.png", "studio", None));
    let empty = write_scene(
        &dir,
        "empty.json",
        scene("empty.png", "studio", Some(serde_json::json!([]))),
    );
    // ② 关掉环境光，只留一盏红灯：箱子必须被照成红的（旧布局下这盏灯完全不亮）
    let dark = write_scene(
        &dir,
        "dark.json",
        scene("dark.png", "none", Some(serde_json::json!([]))),
    );
    let red = write_scene(
        &dir,
        "red.json",
        scene(
            "red.png",
            "none",
            Some(serde_json::json!([
                { "position": [0, 0, 4], "color": [1, 0, 0], "intensity": 200, "radius": 60 }
            ])),
        ),
    );

    // 四种写法都必须通过语义校验（`[]` 是合法写法，不是错误）
    for path in [&preset, &empty, &dark, &red] {
        let v = run(&["validate", path.to_str().unwrap(), "--json"]).json();
        assert_eq!(v["data"]["valid"], true, "场景应当合法：{v}");
    }

    let doctor = run(&["doctor", "--json"]).json();
    if !doctor["data"]["gpu_available"].as_bool().unwrap() {
        eprintln!("跳过渲染断言：没有可用 GPU");
        return;
    }

    for (path, out) in [
        (&preset, "preset.png"),
        (&empty, "empty.png"),
        (&dark, "dark.png"),
        (&red, "red.png"),
    ] {
        let out = dir.join(out);
        let r = run(&[
            "render",
            path.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(r.code, 0, "渲染失败：{}", r.stderr);
    }
    let (w, h, a) = decode_rgba(&dir.join("preset.png"));
    let (_, _, b) = decode_rgba(&dir.join("empty.png"));
    let (_, _, d) = decode_rgba(&dir.join("dark.png"));
    let (_, _, c) = decode_rgba(&dir.join("red.png"));

    assert_ne!(
        a, b,
        "显式清空点光源必须改变画面：studio 预设自带两盏补光，去掉了图就该变"
    );
    let (r, g, bl) = center_mean(w, h, &c);
    assert!(
        r > 120.0 && r > g + 80.0 && r > bl + 80.0,
        "一盏红灯应当把箱子照红，实测 R={r:.1} G={g:.1} B={bl:.1}"
    );
    assert_ne!(
        c, d,
        "加上那盏红灯必须改变画面（旧布局下 Rust 写的是两个并列数组、WGSL 读的是结构体数组，\n\
         结果单盏灯完全不亮、多盏灯的颜色取的是下一盏灯的位置）"
    );
    let (dr, dg, db) = center_mean(w, h, &d);
    // 注意 B 通道：`preset: none` 只是没有太阳、环境光强度为 0，着色器仍会混入
    // 夜色天空色（偏蓝），所以箱子不是纯黑，蓝色能到 90 上下。
    assert!(
        dr < 80.0 && dg < 80.0 && db < 130.0,
        "没有灯时箱子应当是暗的，实测 R={dr:.1} G={dg:.1} B={db:.1}"
    );
}

/// 相机裁剪面与正交投影真的生效（用箱子在画面里的像素面积量）。
#[test]
fn camera_clip_planes_and_orthographic_projection_are_wired() {
    let dir = temp_dir("camera_projection");
    let scene = |out: &str, camera: serde_json::Value| {
        serde_json::json!({
            "objects": [{ "name": "box", "shape": { "type": "box", "size": [1, 1, 1] },
                          "body": "none", "material": "m" }],
            "materials": { "m": { "color": [1, 1, 1], "unlit": true } },
            "camera": camera,
            "simulate": { "steps": 0 },
            "render": { "output": out, "width": 160, "height": 120, "msaa": 1 },
            "lighting": { "preset": "none", "sky": false, "grid": false }
        })
    };
    let persp = write_scene(
        &dir,
        "persp.json",
        scene(
            "persp.png",
            serde_json::json!({ "position": [0, 0, 6], "look_at": [0, 0, 0] }),
        ),
    );
    // far = 3 而箱子在 6 米外 → 应当被远裁剪面切掉，一个像素都不剩
    let far = write_scene(
        &dir,
        "far.json",
        scene(
            "far.png",
            serde_json::json!({ "position": [0, 0, 6], "look_at": [0, 0, 0], "far": 3.0 }),
        ),
    );
    // 正交高度 1 米 = 箱子正好占满画面高度（120px），宽度按 4:3 画布算 120px → 约 14400px
    let ortho = write_scene(
        &dir,
        "ortho.json",
        scene(
            "ortho.png",
            serde_json::json!({ "position": [0, 0, 6], "look_at": [0, 0, 0],
                                "orthographic_height": 1.0 }),
        ),
    );
    // 语义错误必须被拦下
    let bad_far = write_scene(
        &dir,
        "bad_far.json",
        scene(
            "bad.png",
            serde_json::json!({ "position": [0, 0, 6], "near": 5.0, "far": 1.0 }),
        ),
    );
    let bad = run(&["validate", bad_far.to_str().unwrap(), "--json"]);
    assert_eq!(bad.code, 2, "far < near 应当报错");
    let bad_json = bad.json();
    assert_eq!(bad_json["data"]["valid"], false);
    assert!(
        bad_json["data"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["path"] == "camera.far"),
        "诊断应当指向 camera.far：{bad_json}"
    );

    let doctor = run(&["doctor", "--json"]).json();
    if !doctor["data"]["gpu_available"].as_bool().unwrap() {
        eprintln!("跳过渲染断言：没有可用 GPU");
        return;
    }
    for (path, out) in [
        (&persp, "persp.png"),
        (&far, "far.png"),
        (&ortho, "ortho.png"),
    ] {
        let out = dir.join(out);
        let r = run(&[
            "render",
            path.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(r.code, 0, "渲染失败：{}", r.stderr);
    }
    let (_, _, a) = decode_rgba(&dir.join("persp.png"));
    let (_, _, b) = decode_rgba(&dir.join("far.png"));
    let (_, _, c) = decode_rgba(&dir.join("ortho.png"));
    let (n_persp, n_far, n_ortho) = (bright_pixels(&a), bright_pixels(&b), bright_pixels(&c));
    assert!(
        (200..700).contains(&n_persp),
        "透视投影下箱子应当约 300 像素，实测 {n_persp}"
    );
    assert_eq!(n_far, 0, "far=3 时 6 米外的箱子必须被裁掉，实测 {n_far}");
    assert!(
        n_ortho > 10_000,
        "正交高度 1 米时箱子应当占满画面高度（约 14400 像素），实测 {n_ortho}"
    );
    assert!(
        n_ortho > n_persp * 10,
        "正交与透视必须明显不同：正交 {n_ortho} vs 透视 {n_persp}"
    );
}

/// 刚体参数真的进了求解器：重力缩放、阻尼、质量覆盖、角速度、睡眠开关。
#[test]
fn rigid_body_params_reach_the_solver() {
    let dir = temp_dir("body_params");
    let boxed = |extra: serde_json::Value| {
        let mut o = serde_json::json!({
            "name": "box", "shape": { "type": "box", "size": [1, 1, 1] },
            "body": "dynamic", "material": "m", "position": [0, 0, 0]
        });
        for (k, v) in extra.as_object().unwrap() {
            o[k] = v.clone();
        }
        o
    };
    let scene = |name: &str, objects: serde_json::Value, ground: bool, steps: u32| {
        let mut s = serde_json::json!({
            "objects": objects,
            "materials": { "m": { "color": [1, 1, 1] } },
            "camera": { "position": [0, 3, 10], "look_at": [0, 0, 0] },
            "simulate": { "steps": steps, "dt": 0.016666667 },
            "render": { "output": "ignored.png", "width": 32, "height": 32, "msaa": 1 }
        });
        if ground {
            s["ground"] = serde_json::json!({ "friction": 0.9, "restitution": 0.0 });
        }
        write_scene(&dir, name, s)
    };
    let sim = |path: &std::path::Path| {
        let r = run(&["simulate", path.to_str().unwrap(), "--json"]);
        assert_eq!(r.code, 0, "仿真失败：{}", r.stderr);
        r.json()["data"].clone()
    };
    let body = |report: &serde_json::Value, name: &str| -> serde_json::Value {
        report["states"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("报告里没有刚体 {name}：{report}"))
    };

    // ① gravity_scale = 0 → 悬浮不动；缺省 → 掉下去
    let floating = scene(
        "floating.json",
        serde_json::json!([boxed(
            serde_json::json!({ "position": [0, 5, 0], "gravity_scale": 0.0 })
        )]),
        false,
        120,
    );
    let falling = scene(
        "falling.json",
        serde_json::json!([boxed(serde_json::json!({ "position": [0, 5, 0] }))]),
        false,
        120,
    );
    let y_float = body(&sim(&floating), "box")["position"][1]
        .as_f64()
        .unwrap();
    let y_fall = body(&sim(&falling), "box")["position"][1].as_f64().unwrap();
    assert!(
        (y_float - 5.0).abs() < 0.01,
        "gravity_scale=0 应当悬停在 5 米，实测 {y_float}"
    );
    assert!(y_fall < 0.0, "默认重力应当掉到 0 以下，实测 {y_fall}");

    // ② linear_damping → 同样初速度下走得更近
    let damped = scene(
        "damped.json",
        serde_json::json!([boxed(
            serde_json::json!({ "position": [0, 5, 0], "velocity": [10, 0, 0],
                                                     "linear_damping": 3.0 })
        )]),
        false,
        120,
    );
    let undamped = scene(
        "undamped.json",
        serde_json::json!([boxed(
            serde_json::json!({ "position": [0, 5, 0], "velocity": [10, 0, 0] })
        )]),
        false,
        120,
    );
    let x_damped = body(&sim(&damped), "box")["position"][0].as_f64().unwrap();
    let x_undamped = body(&sim(&undamped), "box")["position"][0]
        .as_f64()
        .unwrap();
    assert!(
        x_damped < x_undamped * 0.6,
        "线性阻尼应当显著减速：阻尼后 x={x_damped:.2}，无阻尼 x={x_undamped:.2}"
    );

    // ③ allow_sleep = false → 永远不睡
    let napping = scene(
        "napping.json",
        serde_json::json!([boxed(serde_json::json!({ "position": [0, 0.5, 0] }))]),
        true,
        400,
    );
    let awake = scene(
        "awake.json",
        serde_json::json!([boxed(
            serde_json::json!({ "position": [0, 0.5, 0], "allow_sleep": false })
        )]),
        true,
        400,
    );
    let sleeping_default = sim(&napping)["sleeping"].as_u64().unwrap();
    let sleeping_disabled = sim(&awake)["sleeping"].as_u64().unwrap();
    assert!(sleeping_default >= 1, "静止的箱子默认应当睡着");
    assert_eq!(sleeping_disabled, 0, "allow_sleep=false 的箱子不该睡");

    // ④ angular_velocity → 原地转起来（位置不变，朝向变了）
    let still = scene(
        "still.json",
        serde_json::json!([boxed(serde_json::json!({ "gravity_scale": 0.0 }))]),
        false,
        60,
    );
    let spinning = scene(
        "spinning.json",
        serde_json::json!([boxed(serde_json::json!({ "gravity_scale": 0.0,
                                                     "angular_velocity": [0, 0, 8] }))]),
        false,
        60,
    );
    let q_still = body(&sim(&still), "box")["rotation"].clone();
    let q_spin = body(&sim(&spinning), "box")["rotation"].clone();
    assert!(
        (q_still[3].as_f64().unwrap() - 1.0).abs() < 1e-3,
        "没给角速度就应当保持初始朝向，实测 {q_still}"
    );
    assert!(
        q_spin[3].as_f64().unwrap().abs() < 0.9,
        "给了 8 rad/s 转 1 秒，朝向必须明显改变，实测 {q_spin}"
    );

    // ⑤ mass 覆盖密度算出来的质量：重的球把轻的推得更远
    let collision = |mass: f64| {
        serde_json::json!([
            boxed(
                serde_json::json!({ "name": "A", "position": [-3, 0.5, 0], "velocity": [6, 0, 0],
                                      "mass": mass, "friction": 0.0 })
            ),
            boxed(
                serde_json::json!({ "name": "B", "position": [0, 0.5, 0], "mass": 1.0,
                                      "friction": 0.0 })
            )
        ])
    };
    let light = scene("light.json", collision(1.0), true, 90);
    let heavy = scene("heavy.json", collision(50.0), true, 90);
    let b_light = body(&sim(&light), "B")["displacement"].as_f64().unwrap();
    let b_heavy = body(&sim(&heavy), "B")["displacement"].as_f64().unwrap();
    assert!(
        b_heavy > b_light * 1.3,
        "50 倍质量的 A 应当把 B 推得更远：轻 A → B {b_light:.2} 米，重 A → B {b_heavy:.2} 米"
    );
}

/// 关节锚点：局部锚点错开之后，约束真的按"离锚点多远"来解。
#[test]
fn joint_anchors_change_the_constraint() {
    let dir = temp_dir("joint_anchor");
    let scene = |anchor_b: Option<serde_json::Value>| {
        let mut joint = serde_json::json!({ "type": "ball", "a": "A", "b": "B" });
        if let Some(v) = anchor_b {
            joint["anchor_b"] = v;
        }
        serde_json::json!({
            "objects": [
                { "name": "A", "shape": { "type": "box", "size": [0.3, 0.3, 0.3] },
                  "body": "static", "material": "m", "position": [0, 0, 0] },
                { "name": "B", "shape": { "type": "box", "size": [0.3, 0.3, 0.3] },
                  "body": "dynamic", "material": "m", "position": [-1, 0, 0] }
            ],
            "joints": [joint],
            "materials": { "m": { "color": [1, 1, 1] } },
            "camera": { "position": [0, 0, 10], "look_at": [0, 0, 0] },
            "simulate": { "steps": 180, "dt": 0.016666667 },
            "render": { "output": "ignored.png", "width": 32, "height": 32, "msaa": 1 }
        })
    };
    let plain = write_scene(&dir, "plain.json", scene(None));
    let anchored = write_scene(
        &dir,
        "anchored.json",
        scene(Some(serde_json::json!([1, 0, 0]))),
    );

    let sim = |path: &std::path::Path| {
        let r = run(&["simulate", path.to_str().unwrap(), "--json"]);
        assert_eq!(r.code, 0, "仿真失败：{}", r.stderr);
        r.json()["data"].clone()
    };
    let distance = |report: &serde_json::Value| -> f64 {
        let b = report["states"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "B")
            .unwrap();
        let p: Vec<f64> = b["position"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt()
    };

    let d_plain = distance(&sim(&plain));
    let d_anchored = distance(&sim(&anchored));
    assert!(
        d_plain < 0.2,
        "不给锚点时 B 应当被拉回 A 的原点（距离 ≈0），实测 {d_plain:.3} 米"
    );
    assert!(
        (d_anchored - 1.0).abs() < 0.15,
        "anchor_b=[1,0,0] 时 B 的质心必须被约束在离 A 原点 1 米处，实测 {d_anchored:.3} 米"
    );
}

/// 定时动作：真的会改变仿真结果，而且 `at_time` 与等价的 `at_step` **逐位一致**。
///
/// 这条链路以前完全不存在——场景只能给初始速度，"第 2 秒推一下"只能靠宿主代码。
#[test]
fn scheduled_actions_reach_the_solver() {
    let dir = temp_dir("actions");
    let scene = |actions: Option<serde_json::Value>| {
        let mut v = serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "ground": { "size": 20 },
            "objects": [{
                "name": "球", "shape": { "type": "sphere", "radius": 0.5 },
                "position": [0, 5, 0], "body": "dynamic", "mass": 5.0,
                "restitution": 0.2, "friction": 0.4
            }],
            "camera": { "position": [6, 3, 6], "look_at": [0, 1, 0] },
            "simulate": { "steps": 120, "dt": 0.01666667 }
        });
        if let Some(a) = actions {
            v["actions"] = a;
        }
        v
    };
    let ball = |path: &std::path::Path, want: &str| -> (Vec<f64>, f64, Vec<f64>) {
        let r = run(&["simulate", path.to_str().unwrap(), "--json"]);
        assert_eq!(r.code, 0, "{}", r.stderr);
        let d = r.json();
        assert_eq!(d["ok"], true, "{d}");
        let st = d["data"]["states"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == want)
            .unwrap_or_else(|| panic!("报告里应有 {want}：{d}"))
            .clone();
        let pos: Vec<f64> = st["position"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        (pos, st["speed"].as_f64().unwrap(), {
            st["rotation"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap())
                .collect()
        })
    };

    // 基线：落在 y≈0.5 静止
    let plain = write_scene(&dir, "plain.json", scene(None));
    let (p0, s0, _) = ball(&plain, "球");
    assert!((p0[1] - 0.5).abs() < 0.05, "基线应静止在地面，实测 {p0:?}");
    assert!(s0 < 0.01, "基线速度应≈0，实测 {s0}");

    // 第 100 步（已静止）给一次向上冲量：Δv = 30 / 5 = 6 m/s，末态必须明显离地
    let imp = write_scene(
        &dir,
        "impulse.json",
        scene(Some(serde_json::json!([
            { "type": "impulse", "target": "球", "at_step": 100, "value": [0, 30, 0] }
        ]))),
    );
    let (p1, s1, _) = ball(&imp, "球");
    assert!(
        p1[1] > 1.5 && s1 > 1.0,
        "第 100 步的冲量必须把球重新打上天，实测 pos={p1:?} speed={s1}"
    );

    // 直接设速度：横向 2 m/s、向上 4 m/s
    let vel = write_scene(
        &dir,
        "vel.json",
        scene(Some(serde_json::json!([
            { "type": "velocity", "target": "球", "at_step": 30, "value": [2, 4, 0] }
        ]))),
    );
    let (p2, _, _) = ball(&vel, "球");
    assert!(
        p2[0] > 2.0,
        "设定横向速度后必须飞出去，实测 x={}（基线 x={}）",
        p2[0],
        p0[0]
    );

    // 瞬移 + 朝向：位置与四元数都要跟着变
    let tp = write_scene(
        &dir,
        "teleport.json",
        scene(Some(serde_json::json!([
            { "type": "teleport", "target": "球", "at_step": 60,
              "position": [3, 4, 0], "rotation_deg": [0, 0, 45] }
        ]))),
    );
    let (p3, _, q3) = ball(&tp, "球");
    assert!((p3[0] - 3.0).abs() < 0.05, "瞬移后 x 必须≈3，实测 {p3:?}");
    // 绕 Z 转 45° → 四元数 (0,0,sin22.5°,cos22.5°) ≈ (0,0,0.383,0.924)
    assert!(
        (q3[2] - 0.3827).abs() < 0.02 && (q3[3] - 0.9239).abs() < 0.02,
        "朝向没按 rotation_deg 设置：{q3:?}"
    );

    // 角冲量 / 角速度作用在**盒子**上才看得见（球是对称的）
    let spin = |kind: &str, value: serde_json::Value| {
        write_scene(
            &dir,
            &format!("spin_{kind}.json"),
            serde_json::json!({
                "format": "quench3d.scene", "version": 2,
                "gravity": [0, 0, 0],
                "objects": [{
                    "name": "盒", "shape": { "type": "box", "size": [1, 0.4, 2] },
                    "position": [0, 3, 0], "body": "dynamic", "mass": 2.0
                }],
                "simulate": { "steps": 60, "dt": 0.01666667 },
                "actions": [{ "type": kind, "target": "盒", "at_step": 0, "value": value }]
            }),
        )
    };
    // 绕 Y 设定 3 rad/s，跑 1 秒 → 转 3 rad ≈ 171.9°
    let (_, _, q_av) = ball(
        &spin("angular_velocity", serde_json::json!([0, 3, 0])),
        "盒",
    );
    let ang = 2.0 * q_av[3].abs().clamp(-1.0, 1.0).acos().to_degrees();
    assert!(
        (ang - 171.9).abs() < 2.0,
        "角速度 3 rad/s 跑 1 秒应转约 171.9°，实测 {ang:.1}°"
    );

    // `at_time: 1.0` 与 `at_step: 60` 必须**逐位相同**（dt=1/60）
    let by_step = write_scene(
        &dir,
        "eq_step.json",
        scene(Some(serde_json::json!([
            { "type": "impulse", "target": "球", "at_step": 60, "value": [0, 30, 0] }
        ]))),
    );
    let by_time = write_scene(
        &dir,
        "eq_time.json",
        scene(Some(serde_json::json!([
            { "type": "impulse", "target": "球", "at_time": 1.0, "value": [0, 30, 0] }
        ]))),
    );
    let a = run(&["simulate", by_step.to_str().unwrap(), "--json"]);
    let b = run(&["simulate", by_time.to_str().unwrap(), "--json"]);
    assert_eq!(a.code, 0);
    assert_eq!(
        a.json()["data"]["states"],
        b.json()["data"]["states"],
        "at_time = 1.0 秒必须与 at_step = 60 逐位一致"
    );
}

/// 区域探针：只读（不改变物理）、报告进入/离开步号与区内最高速度。
#[test]
fn probes_observe_without_touching_physics() {
    let dir = temp_dir("probes");
    let mut scene = serde_json::json!({
        "format": "quench3d.scene", "version": 2,
        "ground": { "size": 20 },
        "objects": [{
            "name": "球", "shape": { "type": "sphere", "radius": 0.5 },
            "position": [0, 5, 0], "body": "dynamic", "mass": 5.0,
            "restitution": 0.2, "friction": 0.4
        }],
        "simulate": { "steps": 120, "dt": 0.01666667 }
    });
    let plain = write_scene(&dir, "plain.json", scene.clone());
    scene["probes"] = serde_json::json!([
        { "name": "落点", "shape": "sphere", "position": [0, 0.5, 0], "radius": 1.0 },
        { "name": "中途", "shape": "box", "position": [0, 2.5, 0], "size": [1, 1, 1] }
    ]);
    let with_probes = write_scene(&dir, "probes.json", scene);

    let a = run(&["simulate", plain.to_str().unwrap(), "--json"]);
    let b = run(&["simulate", with_probes.to_str().unwrap(), "--json"]);
    assert_eq!(a.code, 0);
    assert_eq!(b.code, 0);

    // 只读：刚体状态必须逐位相同，报告里没写 probes 时也不该冒出这个字段
    assert_eq!(
        a.json()["data"]["states"],
        b.json()["data"]["states"],
        "探针只能观测，不允许改变物理结果"
    );
    assert!(
        a.json()["data"].get("probes").is_none(),
        "没写 probes 时报告里不该有 probes 字段：{}",
        a.json()["data"]
    );

    let d = b.json();
    let probes = d["data"]["probes"].as_array().expect("应有探针报告");
    assert_eq!(probes.len(), 2, "{d}");
    let landing = probes.iter().find(|p| p["name"] == "落点").unwrap();
    assert_eq!(landing["shape"], "sphere");
    assert_eq!(landing["occupied"], true, "球最终停在落点区域内：{landing}");
    assert_eq!(landing["inside"], serde_json::json!(["球"]));
    let hit = &landing["bodies"][0];
    assert_eq!(hit["name"], "球");
    assert_eq!(hit["entries"], 1);
    // 初速 0 从 5 米落下：0.5·g·t² = 4.5 → t≈0.958s ≈ 57 步，
    // 但半径 0.5 的球进入半径 1.0 的区域会早一些（第 46 步左右，实测值）
    let enter = hit["enter_step"].as_u64().unwrap();
    assert!(
        (40..=52).contains(&enter),
        "进入步号应≈46，实测 {enter}（球在 5 米高处落下）"
    );
    assert!(hit.get("exit_step").is_none(), "结束时仍在区域内：{hit}");
    let max_speed = hit["max_speed"].as_f64().unwrap();
    assert!(
        (8.5..=10.0).contains(&max_speed),
        "区内最高速度应≈9.3 m/s（落地前），实测 {max_speed}"
    );
    assert!(hit["steps_inside"].as_u64().unwrap() > 50, "{hit}");

    // 穿过式探针必须同时给出进入与离开
    let mid = probes.iter().find(|p| p["name"] == "中途").unwrap();
    assert_eq!(mid["occupied"], false);
    let mh = &mid["bodies"][0];
    let enter_m = mh["enter_step"].as_u64().unwrap();
    let exit_m = mh["exit_step"].as_u64().unwrap();
    assert!(enter_m < exit_m, "进入必须早于离开：{mh}");
    assert_eq!(
        mh["steps_inside"].as_u64().unwrap(),
        exit_m - enter_m,
        "停留步数应等于离开-进入：{mh}"
    );
}

/// 三条步进路径（simulate / render / movie）都要认 actions 与 probes。
#[test]
fn actions_and_probes_work_on_all_three_paths() {
    let dir = temp_dir("paths");
    let scene = |with_action: bool| {
        let mut v = serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "gravity": [0, 0, 0],
            "objects": [{
                "name": "盒", "shape": { "type": "box", "size": [1, 1, 1] },
                "position": [0, 2, 0], "body": "dynamic", "mass": 1.0
            }],
            "probes": [{ "name": "起点", "position": [0, 2, 0], "radius": 0.5 }],
            "simulate": { "steps": 12, "dt": 0.01666667 },
            "camera": { "position": [0, 2, 8], "look_at": [0, 2, 0] },
            "render": { "output": "unused.png", "width": 120, "height": 120, "msaa": 1 }
        });
        if with_action {
            // 12 步里横向推出去，画面与探针结果都必须变
            v["actions"] = serde_json::json!([
                { "type": "velocity", "target": "盒", "at_step": 0, "value": [6, 0, 0] }
            ]);
        }
        v
    };
    let still = write_scene(&dir, "still.json", scene(false));
    let moving = write_scene(&dir, "moving.json", scene(true));

    // render：报告里带探针，且加了动作之后 PNG 必须不一样
    let out_a = dir.join("a.png");
    let out_b = dir.join("b.png");
    let ra = run(&[
        "render",
        still.to_str().unwrap(),
        "--out",
        out_a.to_str().unwrap(),
        "--json",
    ]);
    let rb = run(&[
        "render",
        moving.to_str().unwrap(),
        "--out",
        out_b.to_str().unwrap(),
        "--json",
    ]);
    if ra.code != 0 {
        // 无 GPU 的机器上渲染按约定返回 3：只检查契约，不假装验证了像素
        assert_eq!(ra.code, 3, "{}", ra.stderr);
        eprintln!("本机没有 GPU，跳过 render/movie 的像素断言");
        return;
    }
    assert_eq!(rb.code, 0, "带动作的场景也必须渲染成功：{}", rb.stderr);
    let da = ra.json();
    assert_eq!(da["ok"], true, "{da}");
    let probes = da["data"]["probes"]
        .as_array()
        .expect("render 报告应有探针");
    assert_eq!(probes[0]["name"], "起点");
    assert_eq!(
        probes[0]["bodies"][0]["enter_step"], 0,
        "起点探针应在第 0 步就记录到盒子：{probes:?}"
    );
    let png_a = std::fs::read(&out_a).expect("读 a.png");
    let png_b = std::fs::read(&out_b).expect("读 b.png");
    assert_ne!(png_a, png_b, "加了动作之后渲染结果必须不同");
    // 探针是只读的：render 报告里嵌入的 simulate 报告也必须带探针
    assert_eq!(
        da["data"]["simulate"]["probes"][0]["name"], "起点",
        "render 内嵌的 simulate 报告也应带探针"
    );

    // movie：动作改画面、探针出现在录制报告里
    let gif_a = dir.join("a.gif");
    let gif_b = dir.join("b.gif");
    let ma = run(&[
        "movie",
        still.to_str().unwrap(),
        "--frames",
        "3",
        "--steps-per-frame",
        "4",
        "--encoder",
        "gif",
        "--out",
        gif_a.to_str().unwrap(),
        "--json",
    ]);
    let mb = run(&[
        "movie",
        moving.to_str().unwrap(),
        "--frames",
        "3",
        "--steps-per-frame",
        "4",
        "--encoder",
        "gif",
        "--out",
        gif_b.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(ma.code, 0, "{}", ma.stderr);
    assert_eq!(mb.code, 0, "{}", mb.stderr);
    let mda = ma.json();
    let mdb = mb.json();
    assert_ne!(
        mda["data"]["digest"], mdb["data"]["digest"],
        "movie 路径上的动作必须改变像素摘要"
    );
    let mp = mda["data"]["probes"]
        .as_array()
        .expect("movie 报告应有探针");
    assert_eq!(mp[0]["name"], "起点");
    // 12 步 × 6 m/s ÷ 60 → 盒子横向移动 1.2 米；边长 1 的盒子半宽 0.5，
    // 所以它必须走出半径 0.5 的探针（球心到盒子 AABB 的距离会涨到 0.7）
    assert!(
        mdb["data"]["probes"][0]["occupied"] == false
            || mdb["data"]["probes"][0]["bodies"][0]["exit_step"].is_number(),
        "被推走的盒子应记录离开步号：{}",
        mdb["data"]["probes"]
    );
}

/// `--dry-run`：只校验 + 跑物理，**一个字节都不写**，而且不需要 GPU。
#[test]
fn dry_run_touches_nothing() {
    let dir = temp_dir("dry_run");
    let scene = write_scene(
        &dir,
        "s.json",
        serde_json::json!({
            "ground": { "size": 20 },
            "objects": [{
                "name": "球", "shape": { "type": "sphere", "radius": 0.5 },
                "position": [0, 5, 0], "body": "dynamic"
            }],
            "camera": { "position": [4, 3, 4], "look_at": [0, 1, 0] },
            "simulate": { "steps": 90 }
        }),
    );
    let png = dir.join("out.png");
    let gif = dir.join("out.gif");
    let frames = dir.join("frames");

    // render --dry-run：即使机器上没有 GPU 也必须成功（它根本不碰 GPU）
    let r = run(&[
        "render",
        scene.to_str().unwrap(),
        "--dry-run",
        "--out",
        png.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(
        r.code, 0,
        "--dry-run 不需要 GPU，必须成功：{} {}",
        r.stdout, r.stderr
    );
    let d = r.json();
    assert_eq!(d["data"]["dry_run"], true, "{d}");
    assert_eq!(
        d["data"]["would_write"],
        serde_json::json!([png.to_str().unwrap()])
    );
    assert_eq!(d["data"]["bytes"], 0);
    assert_eq!(d["data"]["draw_calls"], 0);
    // 物理是真跑了（这是 dry-run 的价值：先把物理跑明白，再决定要不要花 GPU 时间）
    assert_eq!(d["data"]["simulate"]["steps"], 90, "{d}");
    assert!(d["data"]["simulate"]["bodies"].as_u64().unwrap() >= 1);
    assert!(!png.exists(), "dry-run 不许写盘：{}", png.display());

    // movie --dry-run：会把"本来要写的东西"全列出来，但都不写
    let m = run(&[
        "movie",
        scene.to_str().unwrap(),
        "--frames",
        "3",
        "--steps-per-frame",
        "4",
        "--encoder",
        "gif",
        "--out",
        gif.to_str().unwrap(),
        "--dry-run",
        "--json",
    ]);
    assert_eq!(m.code, 0, "{} {}", m.stdout, m.stderr);
    let md = m.json();
    assert_eq!(md["data"]["dry_run"], true, "{md}");
    assert_eq!(md["data"]["frames"], 3);
    assert_eq!(md["data"]["total_steps"], 12, "3 帧 × 4 步 = 12 步");
    assert_eq!(md["data"]["digest"], "", "没渲染就没有像素摘要");
    let would = md["data"]["would_write"].as_array().unwrap();
    assert_eq!(would.len(), 2, "单文件编码器：视频 + trace：{md}");
    assert!(would
        .iter()
        .any(|p| p.as_str().unwrap().ends_with("out.gif")));
    assert!(would
        .iter()
        .any(|p| p.as_str().unwrap().ends_with("trace.jsonl")));
    assert!(
        !gif.exists() && !frames.exists(),
        "dry-run 不许建目录/写文件"
    );

    // PNG 序列的 dry-run 也不能建帧目录
    let m2 = run(&[
        "movie",
        scene.to_str().unwrap(),
        "--frames",
        "2",
        "--frames-dir",
        frames.to_str().unwrap(),
        "--dry-run",
        "--json",
    ]);
    assert_eq!(m2.code, 0, "{}", m2.stderr);
    assert!(!frames.exists(), "dry-run 不许建帧目录");
}

/// `--no-clobber`：输出已存在就报错（退出码 2 + `output_exists` + 路径），不覆盖。
#[test]
fn no_clobber_refuses_to_overwrite() {
    let dir = temp_dir("no_clobber");
    let scene = write_scene(
        &dir,
        "s.json",
        serde_json::json!({
            "ground": { "size": 10 },
            "objects": [{ "name": "盒", "position": [0, 2, 0], "body": "dynamic" }],
            "camera": { "position": [4, 3, 4], "look_at": [0, 1, 0] },
            "simulate": { "steps": 10 },
            "render": { "width": 80, "height": 60, "msaa": 1 }
        }),
    );
    let png = dir.join("t.png");
    // 先造一个"上一次的产物"：故意写进去一个能被认出来的内容
    std::fs::write(&png, b"PREVIOUS-OUTPUT").unwrap();

    let r = run(&[
        "render",
        scene.to_str().unwrap(),
        "--no-clobber",
        "--out",
        png.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(r.code, 2, "{} {}", r.stdout, r.stderr);
    let d = r.json();
    assert_eq!(d["ok"], false);
    assert_eq!(d["error"]["kind"], "output_exists", "{d}");
    assert_eq!(d["error"]["path"], png.to_str().unwrap(), "{d}");
    assert!(d["error"]["hint"].is_string(), "要给出怎么改：{d}");
    assert_eq!(
        std::fs::read(&png).unwrap(),
        b"PREVIOUS-OUTPUT",
        "拒绝覆盖就必须一个字节都不动"
    );

    // dry-run 与 no-clobber 一起用时，覆盖检查照样生效：dry-run 能提前预测真正的运行会失败
    let both = run(&[
        "render",
        scene.to_str().unwrap(),
        "--no-clobber",
        "--dry-run",
        "--out",
        png.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(both.code, 2, "{}", both.stdout);
    assert_eq!(both.json()["error"]["kind"], "output_exists");

    // 没有这个开关时照旧覆盖（缺省行为不变）
    let ok = run(&[
        "render",
        scene.to_str().unwrap(),
        "--out",
        png.to_str().unwrap(),
        "--json",
    ]);
    if ok.code == 0 {
        assert_ne!(
            std::fs::read(&png).unwrap(),
            b"PREVIOUS-OUTPUT",
            "应当被覆盖"
        );
    } else {
        assert_eq!(ok.code, 3, "无 GPU 的机器上只可能是环境错误：{}", ok.stderr);
    }

    // movie 单文件：同样拒绝覆盖
    let gif = dir.join("t.gif");
    std::fs::write(&gif, b"OLD-GIF").unwrap();
    let m = run(&[
        "movie",
        scene.to_str().unwrap(),
        "--frames",
        "2",
        "--encoder",
        "gif",
        "--out",
        gif.to_str().unwrap(),
        "--no-clobber",
        "--json",
    ]);
    assert_eq!(m.code, 2, "{} {}", m.stdout, m.stderr);
    assert_eq!(m.json()["error"]["kind"], "output_exists");
    assert_eq!(std::fs::read(&gif).unwrap(), b"OLD-GIF");

    // movie PNG 序列：查的是帧目录里有没有东西
    let frames = dir.join("frames");
    std::fs::create_dir_all(&frames).unwrap();
    std::fs::write(frames.join("frame_00000.png"), b"OLD-FRAME").unwrap();
    let mp = run(&[
        "movie",
        scene.to_str().unwrap(),
        "--frames",
        "2",
        "--frames-dir",
        frames.to_str().unwrap(),
        "--no-clobber",
        "--json",
    ]);
    assert_eq!(mp.code, 2, "{} {}", mp.stdout, mp.stderr);
    assert_eq!(mp.json()["error"]["kind"], "output_exists");
    assert_eq!(
        std::fs::read(frames.join("frame_00000.png")).unwrap(),
        b"OLD-FRAME"
    );
}

/// 参数组合没有意义时要**报错**，不能悄悄忽略。
#[test]
fn meaningless_output_flags_are_rejected() {
    let dir = temp_dir("flags");
    let scene = write_scene(
        &dir,
        "s.json",
        serde_json::json!({
            "objects": [{ "name": "盒", "position": [0, 2, 0], "body": "dynamic" }],
            "simulate": { "steps": 5 }
        }),
    );
    for flag in ["--dry-run", "--no-clobber"] {
        let r = run(&["simulate", scene.to_str().unwrap(), flag, "--json"]);
        assert_eq!(r.code, 2, "{flag} 单独给 simulate 应当报错：{}", r.stdout);
        let d = r.json();
        assert_eq!(d["error"]["kind"], "invalid", "{d}");
        let msg = d["error"]["message"].as_str().unwrap();
        assert!(msg.contains("--render"), "要说清楚怎么才有意义：{msg}");
    }
    // 配 --render 时就是有意义的：dry-run 只报告、不渲染
    let out = dir.join("r.png");
    let r = run(&[
        "simulate",
        scene.to_str().unwrap(),
        "--render",
        out.to_str().unwrap(),
        "--dry-run",
        "--json",
    ]);
    assert_eq!(r.code, 0, "{} {}", r.stdout, r.stderr);
    assert!(!out.exists(), "simulate --render --dry-run 也不许写盘");
}

/// 碰撞过滤 / 传感器 / CCD 端到端：三个功能都要能从**场景文件**走到报告里。
#[test]
fn simulate_reports_sensors_and_respects_layers_and_ccd() {
    let dir = temp_dir("filter_sensor_ccd");
    let scene = dir.join("probe.json");
    std::fs::write(
        &scene,
        r#"{
          "objects": [
            { "name": "触发区", "shape": { "type": "box", "size": [3, 0.4, 3] },
              "position": [0, 0, 0], "body": "static", "sensor": true },
            { "name": "落体", "shape": { "type": "box", "size": [0.4, 0.4, 0.4] },
              "position": [0, 3, 0], "body": "dynamic" },
            { "name": "薄墙", "shape": { "type": "box", "size": [0.1, 4, 4] },
              "position": [-9, 0, 0], "body": "static", "collision_layer": 5,
              "collides_with": [5] },
            { "name": "子弹", "shape": { "type": "sphere", "radius": 0.1 },
              "position": [-14, 0, 0], "body": "dynamic", "ccd": true,
              "velocity": [600, 0, 0], "gravity_scale": 0 }
          ],
          "simulate": { "steps": 120, "dt": 0.0166667 }
        }"#,
    )
    .unwrap();

    let r = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let data = r.json();
    let sim = &data["data"];

    // ① 传感器：落体穿过触发区 → 报告里有进入与离开
    let sensors = sim["sensors"].as_array().expect("应当有 sensors 报告");
    let s = sensors
        .iter()
        .find(|s| s["name"] == "触发区")
        .expect("触发区应当在报告里");
    assert_eq!(s["occupied"], false, "结束时落体应当已经离开");
    let hit = s["bodies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "落体")
        .expect("落体应当进过触发区");
    assert_eq!(hit["entries"], 1);
    assert!(hit["steps_inside"].as_u64().unwrap() > 0);
    assert!(
        hit["exit_step"].is_u64(),
        "离开时应当记下 exit_step：{hit:?}"
    );

    // ② 传感器不产生冲量：落体一路掉下去（不被触发区挡住）
    let states = sim["states"].as_array().unwrap();
    let faller = states.iter().find(|b| b["name"] == "落体").unwrap();
    assert!(
        faller["position"][1].as_f64().unwrap() < -3.0,
        "落体应当穿过触发区继续下落：{faller:?}"
    );

    // ③ 分组过滤：薄墙只和第 5 层碰、子弹在第 0 层 → 子弹直接穿过去
    // （注意 CCD 也要遵守分组：否则高速物体会被它"看不到"的墙拦住）
    let bullet = states.iter().find(|b| b["name"] == "子弹").unwrap();
    let x = bullet["position"][0].as_f64().unwrap();
    assert!(
        x > 0.0,
        "薄墙只在自己的层、不碰第 0 层，子弹应当穿过去，实际 x = {x}"
    );

    // ④ CCD 对照：同一颗子弹，墙愿意碰、但不开 CCD → 一步 10 米直接跳过薄墙
    let scene2 = dir.join("no_ccd.json");
    std::fs::write(
        &scene2,
        r#"{
          "objects": [
            { "name": "薄墙", "shape": { "type": "box", "size": [0.1, 4, 4] },
              "position": [0, 0, 0], "body": "static" },
            { "name": "子弹", "shape": { "type": "sphere", "radius": 0.1 },
              "position": [-5, 0, 0], "body": "dynamic",
              "velocity": [600, 0, 0], "gravity_scale": 0 }
          ],
          "simulate": { "steps": 1, "dt": 0.0166667 }
        }"#,
    )
    .unwrap();
    let r2 = run(&["simulate", scene2.to_str().unwrap(), "--json"]);
    assert_eq!(r2.code, 0);
    let x2 = r2.json()["data"]["states"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "子弹")
        .unwrap()["position"][0]
        .as_f64()
        .unwrap();
    assert!(x2 > 0.15, "不开 CCD 时应当穿过去，实际 x = {x2}");

    // ⑤ 开 CCD 的同一场景：停在墙前
    let scene3 = dir.join("with_ccd.json");
    let text = std::fs::read_to_string(&scene2).unwrap().replace(
        "\"body\": \"dynamic\"",
        "\"body\": \"dynamic\", \"ccd\": true",
    );
    std::fs::write(&scene3, text).unwrap();
    let r3 = run(&["simulate", scene3.to_str().unwrap(), "--json"]);
    assert_eq!(r3.code, 0);
    let x3 = r3.json()["data"]["states"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "子弹")
        .unwrap()["position"][0]
        .as_f64()
        .unwrap();
    assert!(x3 <= -0.14, "开了 CCD 不该穿墙，实际 x = {x3}");

    // ⑥ render / movie 这两条路径也会带上传感器报告（字段与 simulate 一致）。
    // 为什么值得钉：报告是三条路各写一遍的，只接一条就等于"换个命令就看不见了"。
    let png = dir.join("out.png");
    let r4 = run(&[
        "render",
        scene.to_str().unwrap(),
        "--dry-run",
        "--out",
        png.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(r4.code, 0, "stdout: {} stderr: {}", r4.stdout, r4.stderr);
    let d4 = r4.json();
    assert_eq!(d4["data"]["sensors"][0]["name"], "触发区", "{d4}");
    assert_eq!(
        d4["data"]["simulate"]["sensors"][0]["name"], "触发区",
        "内嵌的 simulate 报告也要带上：{d4}"
    );

    let gif = dir.join("out.gif");
    let r5 = run(&[
        "movie",
        scene.to_str().unwrap(),
        "--frames",
        "3",
        "--steps-per-frame",
        "20",
        "--encoder",
        "gif",
        "--out",
        gif.to_str().unwrap(),
        "--dry-run",
        "--json",
    ]);
    assert_eq!(r5.code, 0, "stdout: {} stderr: {}", r5.stdout, r5.stderr);
    let d5 = r5.json();
    assert_eq!(d5["data"]["sensors"][0]["name"], "触发区", "{d5}");
    assert!(
        d5["data"]["sensors"][0]["bodies"][0]["steps_inside"]
            .as_u64()
            .unwrap()
            > 0,
        "逐帧路径也要能看见在里面待了多久：{d5}"
    );
}

#[test]
fn query_rays_overlaps_filters_and_flags() {
    let dir = temp_dir("query");
    let scene = dir.join("q.json");
    std::fs::write(
        &scene,
        r#"{
          "objects": [
            { "name": "地面", "shape": { "type": "box", "size": [40, 0.2, 40] },
              "position": [0, -0.1, 0], "body": "static" },
            { "name": "墙", "shape": { "type": "box", "size": [0.4, 3, 10] },
              "position": [4, 1.5, 0], "body": "static" },
            { "name": "触发区", "shape": { "type": "box", "size": [2, 1, 2] },
              "position": [9, 1, 0], "body": "static", "sensor": true, "collision_layer": 3 },
            { "name": "球", "shape": { "type": "sphere", "radius": 0.5 },
              "position": [0, 6, 0], "body": "dynamic" }
          ],
          "simulate": { "steps": 60, "dt": 0.0166667 }
        }"#,
    )
    .unwrap();
    let path = scene.to_str().unwrap();

    // ① 射线：只给最近的一个
    let r = run(&["query", path, "--ray", "0,1,0,1,0,0", "--json"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let d = r.json();
    assert_eq!(d["command"], "query");
    let data = &d["data"];
    assert_eq!(data["query"], "ray");
    assert_eq!(data["count"], 1, "缺省只返回最近命中：{data}");
    assert_eq!(data["hits"][0]["name"], "墙");
    assert!((data["hits"][0]["distance"].as_f64().unwrap() - 3.8).abs() < 1e-3);
    assert!(data["hits"][0]["normal"][0].as_f64().unwrap() < -0.9);
    assert_eq!(data["bodies"], 4);
    assert_eq!(data["indexed"], false);

    // ② --all：墙 + 触发区（按距离升序），传感器被标出来
    let r = run(&["query", path, "--ray", "0,1,0,1,0,0", "--all", "--json"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let data = r.json()["data"].clone();
    assert_eq!(data["count"], 2, "{data}");
    assert_eq!(data["hits"][0]["name"], "墙");
    assert_eq!(data["hits"][1]["name"], "触发区");
    assert_eq!(data["hits"][1]["sensor"], true);
    assert_eq!(data["hits"][1]["layer"], 3);

    // ③ --no-sensors：只剩墙
    let data = run(&[
        "query",
        path,
        "--ray",
        "0,1,0,1,0,0",
        "--all",
        "--no-sensors",
        "--json",
    ])
    .json()["data"]
        .clone();
    assert_eq!(data["count"], 1, "{data}");
    assert_eq!(data["hits"][0]["name"], "墙");

    // ④ --mask：单向层过滤，只查第 3 层
    let data = run(&[
        "query",
        path,
        "--ray",
        "0,1,0,1,0,0",
        "--all",
        "--mask",
        "8",
        "--json",
    ])
    .json()["data"]
        .clone();
    assert_eq!(data["count"], 1, "{data}");
    assert_eq!(data["hits"][0]["name"], "触发区");

    // ⑤ --index：与线性路径逐条一致
    let fast = run(&[
        "query",
        path,
        "--ray",
        "0,1,0,1,0,0",
        "--all",
        "--index",
        "--json",
    ]);
    assert_eq!(fast.code, 0, "stderr: {}", fast.stderr);
    let fd = fast.json()["data"].clone();
    assert_eq!(fd["indexed"], true);
    assert_eq!(fd["fallback"], false);
    assert_eq!(fd["count"], 2, "索引路径要和 ② 一样给出两条命中：{fd}");
    assert_eq!(fd["hits"][0]["name"], "墙");
    assert_eq!(fd["hits"][1]["name"], "触发区");

    // ⑥ 重叠：3×3×3 的探针盒罩住墙与地面（相交 → 距离 0）
    let r = run(&["query", path, "--overlap", "box:3,3,3@3,1,0", "--json"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let data = r.json()["data"].clone();
    assert_eq!(data["query"], "overlap");
    assert_eq!(data["shape"], "box 3,3,3");
    assert_eq!(data["at"], serde_json::json!([3.0, 1.0, 0.0]));
    assert!(data["count"].as_u64().unwrap() >= 2, "{data}");
    let names: Vec<&str> = data["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"墙"), "{names:?}");
    assert!(names.contains(&"地面"), "{names:?}");
    for h in data["hits"].as_array().unwrap() {
        assert_eq!(h["distance"], 0.0, "相交时距离应当是 0：{h}");
    }

    // ⑦ 带旋转的薄探针盒：45° 斜着插进墙里（验证旋转真的生效）
    let data = run(&[
        "query",
        path,
        "--overlap",
        "box:1,0.1,0.1@4,1.5,0~0,0,45",
        "--json",
    ])
    .json()["data"]
        .clone();
    assert_eq!(data["rotation_deg"], serde_json::json!([0.0, 0.0, 45.0]));
    assert_eq!(data["count"], 1, "{data}");
    assert_eq!(data["hits"][0]["name"], "墙");

    // ⑧ 距离上限：把探针放大到能"够到"球（球在 6 米高，探针在 3 米处）
    let near = run(&[
        "query",
        path,
        "--overlap",
        "box:1,1,1@3,1,0",
        "--max",
        "1.5",
        "--json",
    ])
    .json()["data"]
        .clone();
    let far = run(&[
        "query",
        path,
        "--overlap",
        "box:1,1,1@3,1,0",
        "--max",
        "100",
        "--json",
    ])
    .json()["data"]
        .clone();
    assert!(
        far["count"].as_u64().unwrap() >= near["count"].as_u64().unwrap(),
        "放宽 --max 不该减少命中：near={near} far={far}"
    );
    assert_eq!(far["max_distance"], 100.0);

    // ⑨ --steps：先跑物理再查（球落到地面附近）
    let data = run(&[
        "query",
        path,
        "--ray",
        "0,0.2,0,0,-1,0",
        "--all",
        "--steps",
        "90",
        "--json",
    ])
    .json()["data"]
        .clone();
    let names: Vec<&str> = data["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"球"), "球落地后应当被射线打到：{data}");
    assert!(names.contains(&"地面"), "{data}");
    // 不步进时球还在 6 米高，y=0.2 的射线打不到它
    let data =
        run(&["query", path, "--ray", "0,0.2,0,0,-1,0", "--all", "--json"]).json()["data"].clone();
    let names: Vec<&str> = data["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["地面"], "{data}");
}

#[test]
fn query_rejects_bad_flags_instead_of_ignoring_them() {
    let dir = temp_dir("query_bad");
    let scene = dir.join("q.json");
    std::fs::write(
        &scene,
        r#"{
          "objects": [
            { "name": "地面", "shape": { "type": "box", "size": [10, 0.2, 10] },
              "position": [0, -0.1, 0], "body": "static" }
          ]
        }"#,
    )
    .unwrap();
    let path = scene.to_str().unwrap();

    // 两个查询方式都给
    let r = run(&[
        "query",
        path,
        "--ray",
        "0,1,0,0,-1,0",
        "--overlap",
        "sphere:1@0,1,0",
        "--json",
    ]);
    assert_eq!(r.code, 2, "stderr: {}", r.stderr);
    assert_eq!(r.json()["error"]["kind"], "invalid");

    // 一个都不给
    let r = run(&["query", path, "--json"]);
    assert_eq!(r.code, 2);
    assert_eq!(r.json()["error"]["kind"], "invalid");

    // --all 对重叠没有意义
    let r = run(&[
        "query",
        path,
        "--overlap",
        "sphere:1@0,1,0",
        "--all",
        "--json",
    ]);
    assert_eq!(r.code, 2);
    assert!(r.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("--all"));

    // 射线数字个数不对 / 方向是零向量
    let r = run(&["query", path, "--ray", "0,1,0,0,-1", "--json"]);
    assert_eq!(r.code, 2);
    let r = run(&["query", path, "--ray", "0,1,0,0,0,0", "--json"]);
    assert_eq!(r.code, 2);
    assert!(r.json()["error"]["message"]
        .as_str()
        .unwrap()
        .contains("零向量"));

    // 形状语法错误
    let r = run(&["query", path, "--overlap", "torus:1", "--json"]);
    assert_eq!(r.code, 2);
    let r = run(&["query", path, "--overlap", "box:1,2", "--json"]);
    assert_eq!(r.code, 2);
    let r = run(&["query", path, "--overlap", "sphere:0.5@0,1", "--json"]);
    assert_eq!(r.code, 2);

    // query 不写盘：写文件相关的开关一律拒绝（而不是悄悄忽略）
    for flag in ["--dry-run", "--no-clobber"] {
        let r = run(&["query", path, "--ray", "0,1,0,0,-1,0", flag, "--json"]);
        assert_eq!(r.code, 2, "{flag} 应当被拒绝：{}", r.stdout);
    }
    let out = dir.join("x.json");
    let r = run(&[
        "query",
        path,
        "--ray",
        "0,1,0,0,-1,0",
        "--out",
        out.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(r.code, 2);
    assert!(!out.exists(), "被拒绝的 query 不该写文件");

    // 场景不存在
    let r = run(&[
        "query",
        dir.join("nope.json").to_str().unwrap(),
        "--ray",
        "0,1,0,0,-1,0",
        "--json",
    ]);
    assert_ne!(r.code, 0);
}

/// 关节补全：铰链伺服、球窝锥形限位、断裂阈值——三条都从场景文件走到仿真报告。
#[test]
fn joint_servo_cone_limit_and_break_force_are_wired() {
    let dir = temp_dir("joint_completion");

    // ---- ① 铰链伺服：目标 30°，5 秒后门就该停在 30°（不是"一直转"也不是"停住不动"）----
    let servo_scene = |target: Option<f64>, collide: Option<bool>| {
        let mut joint = serde_json::json!({
            "type": "hinge", "a": "锚", "b": "门", "axis": [0, 1, 0],
            "anchor_a": [0, 0, 0], "anchor_b": [-0.4, 0, 0],
            "motor_target_deg": target, "motor_max_impulse": 60.0,
            "motor_stiffness": 10.0, "motor_damping": 1.0
        });
        if target.is_none() {
            joint["motor_target_deg"] = serde_json::Value::Null;
        }
        if let Some(c) = collide {
            joint["collide_connected"] = serde_json::json!(c);
        }
        serde_json::json!({
            "objects": [
                { "name": "锚", "shape": { "type": "box", "size": [0.1, 0.1, 0.1] },
                  "body": "static", "material": "m" },
                { "name": "门", "shape": { "type": "box", "size": [0.7, 1.0, 0.1] },
                  "body": "dynamic", "material": "m", "position": [0.4, 0, 0], "density": 500 }
            ],
            "joints": [joint],
            "materials": { "m": { "color": [1, 1, 1] } },
            "camera": { "position": [0, 0, 10], "look_at": [0, 0, 0] },
            "simulate": { "steps": 300, "dt": 0.016666667 },
            "render": { "output": "ignored.png", "width": 32, "height": 32, "msaa": 1 }
        })
    };

    // ---- ② 球窝锥形限位：初始 60° 摆角，半角 25° 时 1 秒内必须被压回锥面 ----
    let cone_scene = |half_angle: Option<f64>| {
        let mut joint = serde_json::json!({
            "type": "ball", "a": "锚", "b": "摆", "anchor_b": [0, 0.5, 0]
        });
        if let Some(h) = half_angle {
            joint["cone_limit"] = serde_json::json!({ "half_angle_deg": h });
        }
        serde_json::json!({
            "objects": [
                { "name": "锚", "shape": { "type": "sphere", "radius": 0.05 },
                  "body": "static", "material": "m", "position": [0, 2, 0] },
                { "name": "摆", "shape": { "type": "box", "size": [0.1, 1.0, 0.1] },
                  "body": "dynamic", "material": "m",
                  "position": [0.4330127, 1.75, 0], "rotation_deg": [0, 0, 60] }
            ],
            // 零重力：这样"有没有锥形限位"是唯一的差别。有重力的话自由摆也会往竖直方向
            // 落回去（实测 1 秒从 60° 掉到 52°），两条曲线就分不清是谁的功劳了。
            "gravity": [0, 0, 0],
            "joints": [joint],
            "materials": { "m": { "color": [1, 1, 1] } },
            "camera": { "position": [0, 0, 10], "look_at": [0, 0, 0] },
            "simulate": { "steps": 60, "dt": 0.016666667 },
            "render": { "output": "ignored.png", "width": 32, "height": 32, "msaa": 1 }
        })
    };

    // ---- ③ 断裂：64 kg 的载荷（≈628 N）挂在 500 N 的关节上 → 断；1e6 N → 不断 ----
    let break_scene = |break_force: f64| {
        serde_json::json!({
            "objects": [
                { "name": "锚", "shape": { "type": "sphere", "radius": 0.05 },
                  "body": "static", "material": "m", "position": [0, 3, 0] },
                { "name": "载", "shape": { "type": "box", "size": [0.4, 0.4, 0.4] },
                  "body": "dynamic", "material": "m", "position": [0, 1, 0] }
            ],
            "joints": [{ "type": "distance", "a": "锚", "b": "载", "rest_length": 2.0,
                         "stiffness": 0.0, "damping": 0.0, "break_force": break_force }],
            "materials": { "m": { "color": [1, 1, 1] } },
            "camera": { "position": [0, 0, 10], "look_at": [0, 0, 0] },
            "simulate": { "steps": 180, "dt": 0.016666667 },
            "render": { "output": "ignored.png", "width": 32, "height": 32, "msaa": 1 }
        })
    };

    let sim = |name: &str, value: serde_json::Value| {
        let path = write_scene(&dir, name, value);
        let r = run(&["simulate", path.to_str().unwrap(), "--json"]);
        assert_eq!(r.code, 0, "仿真失败（{name}）：{}", r.stderr);
        r.json()["data"].clone()
    };
    // 绕某根轴转过的角度（度）。报告的 rotation 是 [x, y, z, w]。
    let spin_about = |report: &serde_json::Value, name: &str, axis: usize| -> f64 {
        let s = report["states"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .unwrap();
        let q: Vec<f64> = s["rotation"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        (2.0 * q[axis].atan2(q[3])).to_degrees().abs()
    };
    let y_of = |report: &serde_json::Value, name: &str| -> f64 {
        report["states"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .unwrap()["position"][1]
            .as_f64()
            .unwrap()
    };

    // ① 伺服
    let servo = sim("servo.json", servo_scene(Some(30.0), None));
    assert_eq!(servo["joints"].as_u64().unwrap(), 1);
    assert_eq!(servo["joints_broken"].as_u64().unwrap(), 0);
    let door = spin_about(&servo, "门", 1);
    assert!(
        (door - 30.0).abs() < 3.0,
        "伺服目标 30°，门实际停在 {door:.2}°（差得远说明马达没接线或没保持清醒）"
    );

    // ② 锥形限位：同样的初始 60° 摆角，有 25° 限位 vs 没有
    let limited = sim("cone_limited.json", cone_scene(Some(25.0)));
    let free = sim("cone_free.json", cone_scene(None));
    let limited_swing = spin_about(&limited, "摆", 2);
    let free_swing = spin_about(&free, "摆", 2);
    assert!(
        (free_swing - 60.0).abs() < 1.5,
        "没有锥形限位时球窝不该产生任何恢复力矩，摆角应当还是 60°，实测 {free_swing:.2}°"
    );
    assert!(
        limited_swing < 33.0,
        "25° 锥形限位该把 1 秒后的摆角压回锥面附近，实测 {limited_swing:.2}°"
    );

    // ③ 断裂
    let broken = sim("break_small.json", break_scene(500.0));
    assert_eq!(
        broken["joints_broken"].as_u64().unwrap(),
        1,
        "628 N 的载荷挂在 500 N 的关节上该断"
    );
    assert_eq!(
        broken["joints"].as_u64().unwrap(),
        0,
        "断掉的关节不该还在计数里"
    );
    let fallen = y_of(&broken, "载");
    assert!(fallen < 0.5, "断了之后载荷该掉下去，实测 y = {fallen:.2}");

    let held = sim("break_big.json", break_scene(1.0e6));
    assert_eq!(held["joints_broken"].as_u64().unwrap(), 0);
    assert_eq!(held["joints"].as_u64().unwrap(), 1);
    let hanging = y_of(&held, "载");
    assert!(
        (hanging - 1.0).abs() < 0.05,
        "1e6 N 的关节该把载荷吊在原处，实测 y = {hanging:.3}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 事件总线：`simulate` 的报告里要能看到"发生了什么"，而且是**物体名**不是句柄。
#[test]
fn simulate_report_includes_an_event_stream() {
    let dir = temp_dir("events");
    let scene = dir.join("events.json");
    std::fs::write(
        &scene,
        r#"{
          "gravity": [0, -9.81, 0],
          "ground": { "size": 20 },
          "objects": [
            { "name": "触发区", "shape": { "type": "box", "size": [2, 0.4, 2] },
              "position": [0, 1.6, 0], "body": "static", "sensor": true },
            { "name": "球", "shape": { "type": "sphere", "radius": 0.25 },
              "position": [0, 4, 0], "body": "dynamic", "restitution": 0.4 },
            { "name": "吊点", "shape": { "type": "sphere", "radius": 0.05 },
              "position": [6, 5, 0], "body": "static" },
            { "name": "重物", "shape": { "type": "box", "size": [0.4, 0.4, 0.4] },
              "position": [6, 4, 0], "body": "dynamic" }
          ],
          "joints": [
            { "type": "distance", "a": "吊点", "b": "重物", "rest_length": 1.0,
              "break_force": 400.0 }
          ],
          "simulate": { "steps": 300, "dt": 0.0166667 }
        }"#,
    )
    .unwrap();

    let r = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let data = r.json();
    let sim = &data["data"];
    let events = sim["events"]
        .as_array()
        .unwrap_or_else(|| panic!("报告里该有 events：{sim}"));
    assert_eq!(sim["events_dropped"], 0, "300 步不该丢事件");

    let kind_of =
        |k: &str| -> Vec<&serde_json::Value> { events.iter().filter(|e| e["kind"] == k).collect() };

    // ① 球落到地面：contact_begin，两端用物体名
    let begin = kind_of("contact_begin");
    assert!(!begin.is_empty(), "该有接触开始：{events:?}");
    // 地面在句柄表里的名字就是 `$ground`（与 `states` 里的名字一致）
    assert!(
        begin
            .iter()
            .any(|e| (e["a"] == "球" && e["b"] == "$ground")
                || (e["a"] == "$ground" && e["b"] == "球")),
        "接触事件该用物体名（球 / $ground）：{begin:?}"
    );

    // ② 球穿过触发区：sensor_enter + sensor_exit
    assert_eq!(kind_of("sensor_enter").len(), 1, "{events:?}");
    assert_eq!(kind_of("sensor_exit").len(), 1, "{events:?}");
    assert_eq!(kind_of("sensor_enter")[0]["a"], "触发区");

    // ③ 睡眠：球最终停下并睡着（同一子步只报一次）
    let sleeps: Vec<_> = kind_of("sleep")
        .into_iter()
        .filter(|e| e["a"] == "球")
        .collect();
    assert_eq!(sleeps.len(), 1, "一次入睡只该有一条 sleep：{events:?}");
    assert_eq!(kind_of("wake").len(), 0, "没东西被唤醒：{events:?}");

    // ④ 断裂：吊点到重物的距离关节被拉断，事件里有力量级
    let broken = kind_of("joint_broken");
    assert_eq!(broken.len(), 1, "绳该断一次：{events:?}");
    assert_eq!(broken[0]["a"], "吊点");
    assert!(
        broken[0]["force"].as_f64().unwrap() > 400.0,
        "事件里的力量级该超过阈值：{}",
        broken[0]["force"]
    );
    assert_eq!(sim["joints_broken"], 1);

    // ⑤ 事件是**按发生顺序**的（步号单调不减），而不是乱序的集合
    let steps: Vec<u64> = events.iter().map(|e| e["step"].as_u64().unwrap()).collect();
    assert!(
        steps.windows(2).all(|w| w[0] <= w[1]),
        "事件顺序该按时间：{steps:?}"
    );
    // ⑥ 同一场景跑两次，事件流逐条相同（确定性）
    let r2 = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    let data2 = r2.json();
    assert_eq!(
        data2["data"]["events"], sim["events"],
        "事件流必须确定性（两次运行逐条相同）"
    );
}
