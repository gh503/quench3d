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
        "schema", "doctor", "info", "validate", "export", "render", "simulate", "ask", "mcp",
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
        "validate", "export", "render", "simulate", "ask", "mcp", "doctor", "info",
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
fn water_buoyancy_floats_light_bodies_and_sinks_dense_ones() {
    let dir = temp_dir("water");
    let scene = dir.join("water.json");
    std::fs::write(
        &scene,
        r#"{
          "ground": { "size": 20, "y": -3 },
          "waters": [{ "center": [0, 0], "size": [10, 10], "level": 0.0 }],
          "objects": [
            { "name": "轻箱", "shape": { "type": "box", "size": [1, 0.5, 1] },
              "position": [0, 2, 0], "body": "dynamic", "density": 500 },
            { "name": "重箱", "shape": { "type": "box", "size": [1, 0.5, 1] },
              "position": [3, 2, 0], "body": "dynamic", "density": 2000 }
          ],
          "simulate": { "steps": 360, "dt": 0.0166667 }
        }"#,
    )
    .unwrap();

    let r = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let env = r.json();
    let data = &env["data"];
    assert_eq!(data["waters"], 1, "报告要能看出配了水域: {data}");
    let states = data["states"].as_array().unwrap();
    let light = states.iter().find(|s| s["name"] == "轻箱").unwrap();
    let heavy = states.iter().find(|s| s["name"] == "重箱").unwrap();
    let light_y = light["position"][1].as_f64().unwrap();
    assert!(
        light_y.abs() < 0.2,
        "半密度箱的箱心应停在水面附近，实际 {light_y}"
    );
    let sub = light["submerged"].as_f64().unwrap();
    assert!((sub - 0.5).abs() < 0.12, "浸没占比应接近 0.5，实际 {sub}");
    let heavy_y = heavy["position"][1].as_f64().unwrap();
    assert!(heavy_y < -0.3, "密度两倍的箱应沉底，实际 y = {heavy_y}");

    // 尺寸非法：当场报 waters[0].size，而不是静默没有水。
    let bad = dir.join("bad_water.json");
    std::fs::write(
        &bad,
        r#"{"waters": [{ "size": [0, 4], "level": 0 }], "simulate": { "steps": 1 }}"#,
    )
    .unwrap();
    let r = run(&["validate", bad.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 2, "{}", r.stdout);
    let env = r.json();
    let diags = env["data"]["diagnostics"].as_array().unwrap();
    assert!(
        diags
            .iter()
            .any(|d| d["path"].as_str() == Some("waters[0].size")),
        "要报 waters[0].size: {diags:?}"
    );
}

#[test]
fn fluid_particles_settle_and_are_reported() {
    let dir = temp_dir("fluid");
    let scene = dir.join("fluid.json");
    std::fs::write(
        &scene,
        r#"{
          "ground": { "size": 20, "y": 0 },
          "fluids": [
            { "center": [0, 1.2, 0], "size": [1.2, 1.2, 1.2],
              "spacing": 0.1, "radius": 0.05, "max_particles": 2000 }
          ],
          "objects": [
            { "name": "左壁", "shape": { "type": "box", "size": [0.2, 1, 1.8] },
              "position": [-0.9, 0.5, 0], "body": "static" },
            { "name": "右壁", "shape": { "type": "box", "size": [0.2, 1, 1.8] },
              "position": [0.9, 0.5, 0], "body": "static" },
            { "name": "前壁", "shape": { "type": "box", "size": [1.8, 1, 0.2] },
              "position": [0, 0.5, 0.9], "body": "static" },
            { "name": "后壁", "shape": { "type": "box", "size": [1.8, 1, 0.2] },
              "position": [0, 0.5, -0.9], "body": "static" }
          ],
          "simulate": { "steps": 240, "dt": 0.0166667 }
        }"#,
    )
    .unwrap();

    let r = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let env = r.json();
    let data = &env["data"];
    let fluids = data["fluids"].as_array().expect("报告里要有 fluids");
    assert_eq!(fluids.len(), 1, "{data}");
    let f = &fluids[0];
    let particles = f["particles"].as_u64().unwrap();
    assert!(particles > 500, "粒子太少: {particles}");
    let max_y = f["max_y"].as_f64().unwrap();
    assert!(max_y > 0.25, "水应该堆起来，实际 max_y = {max_y}");
    let min_y = f["min_y"].as_f64().unwrap();
    assert!(min_y > 0.0, "粒子不应穿地，实际 min_y = {min_y}");
    let ratio = f["avg_density_ratio"].as_f64().unwrap();
    assert!((0.5..=2.0).contains(&ratio), "密度比异常: {ratio}");
    let contacts = f["contacts"].as_u64().unwrap_or(0);
    assert!(contacts > 0, "围池的粒子应该与墙/地面有接触: {f}");

    // spacing 非法：当场报 fluids[0].spacing，而不是静默没有粒子。
    let bad = dir.join("bad_fluid.json");
    std::fs::write(
        &bad,
        r#"{"fluids": [{ "size": [1, 1, 1], "spacing": 0 }], "simulate": { "steps": 1 }}"#,
    )
    .unwrap();
    let r = run(&["validate", bad.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 2, "{}", r.stdout);
    let env = r.json();
    let diags = env["data"]["diagnostics"].as_array().unwrap();
    assert!(
        diags
            .iter()
            .any(|d| d["path"].as_str() == Some("fluids[0].spacing")),
        "要报 fluids[0].spacing: {diags:?}"
    );
}

#[test]
fn fluid_buoyancy_floats_a_light_body() {
    let dir = temp_dir("fluid_buoyancy");
    let scene = dir.join("float.json");
    std::fs::write(
        &scene,
        r#"{
          "ground": { "size": 20, "y": 0 },
          "fluids": [
            { "center": [0, 1.2, 0], "size": [1.2, 1.2, 1.2],
              "spacing": 0.1, "radius": 0.05, "max_particles": 2000,
              "drag": 6 }
          ],
          "objects": [
            { "name": "浮球", "shape": { "type": "sphere", "radius": 0.25 },
              "position": [0, 1.2, 0], "body": "dynamic", "density": 300 }
          ],
          "simulate": { "steps": 240, "dt": 0.0166667 }
        }"#,
    )
    .unwrap();
    let r = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let env = r.json();
    let data = &env["data"];
    let states = data["states"].as_array().unwrap();
    let ball = states.iter().find(|s| s["name"] == "浮球").unwrap();
    let y = ball["position"][1].as_f64().unwrap();
    assert!(y > 0.5, "300 kg/m³ 的球应该被流体托住，实际 y = {y}");
    let f = &data["fluids"].as_array().unwrap()[0];
    assert!(
        f["contacts"].as_u64().unwrap_or(0) > 0,
        "应有粒子-刚体接触: {f}"
    );
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
            "waters": [{ "center": [0, 0], "size": [20, 20], "level": 0.2 }],
            "water": { "color": [0.08, 0.32, 0.52], "clarity": 0.7,
                       "wave_strength": 1.2, "foam": 0.8, "refraction": 0.9 },
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
    assert_eq!(
        data["water_post"], true,
        "有 waters 时应当启用屏幕空间水后处理"
    );
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

/// 写一个平铺 Radiance RGBE（无 RLE）测试环境图。
fn write_test_hdr(path: &std::path::Path, w: usize, h: usize, rgb: [u8; 3], e: u8) {
    let mut bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n".to_vec();
    bytes.extend_from_slice(format!("-Y {h} +X {w}\n").as_bytes());
    for _ in 0..w * h {
        bytes.extend_from_slice(&[rgb[0], rgb[1], rgb[2], e]);
    }
    std::fs::write(path, bytes).expect("写 HDR 环境图");
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

/// 诊断码（第 55 轮）：agent 按 `code` 分支，不再按中文文案分支。
///
/// 这个用例钉三件事：① 每条诊断都带稳定 `code`；② 具体问题给具体码（不是一律兜底）；
/// ③ 码表能从 `schema --json` 查到、并且文本输出里也带码。全程不需要显卡。
#[test]
fn diagnostics_carry_stable_machine_readable_codes() {
    let dir = temp_dir("diag_codes");
    let scene = write_scene(
        &dir,
        "bad.json",
        serde_json::json!({
            "objects": [
                { "name": "a", "body": "dynamic", "shape": { "type": "box", "size": [1, 1, 1] } },
                // 没有刚体却写了 velocity：会被忽略（ineffective）
                { "name": "b", "body": "none", "velocity": [1, 0, 0],
                  "shape": { "type": "sphere", "radius": 0.5 } }
            ],
            // 引用了不存在的物体 b（unknown_reference）
            "joints": [ { "type": "fixed", "a": "a", "b": "nope" } ],
            // 找不到模型文件（asset_not_found）
            "models": [ { "path": "definitely_missing_xyz_55b.glb" } ],
            // 视场角越界（out_of_range）
            "camera": { "fov_deg": 400 }
        }),
    );
    let r = run(&["validate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 2, "坏场景要退出码 2：{}", r.stdout);
    let v = r.json();
    let diags = v["data"]["diagnostics"].as_array().expect("要有诊断");
    assert!(!diags.is_empty(), "{v}");

    // ① 每条诊断都必须有非空 code
    for d in diags {
        let code = d["code"]
            .as_str()
            .unwrap_or_else(|| panic!("诊断缺少 code：{d}"));
        assert!(!code.is_empty(), "code 不能为空：{d}");
    }
    // ② 具体问题给具体码（兜底码 invalid 只该在没归类时出现）
    let pairs: Vec<(&str, &str)> = diags
        .iter()
        .map(|d| {
            (
                d["code"].as_str().unwrap(),
                d["path"].as_str().unwrap_or(""),
            )
        })
        .collect();
    for expected in [
        ("unknown_reference", "joints[0].b"),
        ("asset_not_found", "models[0].path"),
        ("out_of_range", "camera.fov_deg"),
        ("ineffective", "objects[1].velocity"),
    ] {
        assert!(
            pairs.contains(&expected),
            "缺少 {expected:?}，实际：{pairs:?}"
        );
    }
    assert!(
        !pairs.iter().any(|(c, _)| *c == "invalid"),
        "这段场景不该出现兜底码：{pairs:?}"
    );

    // ③ 码表可从 schema 查到（agent 的自描述入口）
    let s = run(&["schema", "--json"]).json();
    let codes: Vec<String> = s["data"]["error_codes"]
        .as_array()
        .expect("schema 要有 error_codes")
        .iter()
        .map(|e| e["code"].as_str().unwrap().to_string())
        .collect();
    for c in [
        "unknown_reference",
        "asset_not_found",
        "out_of_range",
        "ineffective",
    ] {
        assert!(codes.contains(&c.to_string()), "码表里缺 {c}：{codes:?}");
    }
    assert_eq!(
        codes.len(),
        quench3d_cli::scene_doc::ErrorCode::TABLE.len(),
        "schema 里的码数量必须等于源码总表（不能只写一半）"
    );

    // ④ 人读的文本输出也带码（grep 一行就能分流，不用先解析 JSON）
    let text = run(&["validate", scene.to_str().unwrap()]);
    assert_eq!(text.code, 2);
    assert!(
        text.stdout.contains("asset_not_found") && text.stdout.contains("unknown_reference"),
        "文本输出里也要有码：{}",
        text.stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

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

/// MP4：外部 ffmpeg 存在时才跑；输出必须是带 ftyp box 的 MP4。
#[test]
fn movie_encodes_mp4_when_ffmpeg_is_available() {
    let ffmpeg = matches!(
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
        Ok(status) if status.success()
    );
    if !ffmpeg {
        eprintln!("跳过：没有 ffmpeg");
        return;
    }
    let doctor = run(&["doctor", "--json"]).json();
    if !doctor["data"]["gpu_available"].as_bool().unwrap_or(false) {
        eprintln!("跳过：没有 GPU");
        return;
    }
    let dir = temp_dir("movie_mp4");
    let scene = dir.join("s.json");
    std::fs::write(
        &scene,
        r#"{
            "ground": { "size": 10 },
            "objects": [{ "name": "箱", "shape": { "type": "box", "size": [1, 1, 1] },
                          "position": [0, 3, 0], "body": "dynamic" }],
            "camera": { "position": [6, 3.5, 7], "look_at": [0, 0.8, 0] },
            "render": { "width": 64, "height": 64, "msaa": 1 }
        }"#,
    )
    .unwrap();
    let out = dir.join("clip.mp4");
    let r = run(&[
        "movie",
        scene.to_str().unwrap(),
        "--frames",
        "3",
        "--encoder",
        "mp4",
        "--out",
        out.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(r.code, 0, "stderr={}\nstdout={}", r.stderr, r.stdout);
    let data = r.json()["data"].clone();
    assert_eq!(data["encoder"], "mp4");
    assert_eq!(data["frames"], 3);
    assert!(data["bytes"].as_u64().unwrap() > 0);
    let bytes = std::fs::read(&out).expect("MP4 应落盘");
    assert!(bytes.len() > 32, "MP4 太小");
    assert_eq!(&bytes[4..8], b"ftyp", "MP4 缺少 ftyp box");
    let trace = data["trace"].as_str().unwrap();
    assert!(
        std::path::Path::new(trace).exists(),
        "trace 应跟着 MP4 落在旁边：{trace}"
    );
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

/// 单帧黄金图：同场景重渲必须逐像素一致；换颜色必须报 `golden_mismatch`；
/// 用法错误（缺参考图 / 配 dry-run / 多输出）必须**不碰 GPU**就拒绝。
#[test]
fn golden_png_comparison_is_wired_and_fails_loudly() {
    let doctor = run(&["doctor", "--json"]).json();
    if !doctor["data"]["gpu_available"].as_bool().unwrap_or(false) {
        eprintln!("跳过：没有 GPU");
        return;
    }
    let dir = temp_dir("golden");
    let scene = |name: &str, color: &str| {
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!(
                r#"{{
                    "lighting": {{ "preset": "none", "ambient": 0.0, "sky": false, "grid": false }},
                    "materials": {{ "m": {{ "color": {color}, "roughness": 0.8 }} }},
                    "objects": [{{ "name": "球", "shape": {{ "type": "sphere", "radius": 1 }},
                                   "position": [0, 1, 0], "material": "m" }}],
                    "camera": {{ "position": [0, 1.5, 4], "look_at": [0, 1, 0] }},
                    "render": {{ "output": "ignored.png", "width": 64, "height": 48, "msaa": 1 }}
                }}"#
            ),
        )
        .unwrap();
        path
    };
    let red = scene("red.json", "[1.0, 0.1, 0.1]");
    let blue = scene("blue.json", "[0.1, 0.1, 1.0]");
    let golden = dir.join("golden.png");
    let out = dir.join("out.png");
    let red_render = run(&[
        "render",
        red.to_str().unwrap(),
        "--out",
        golden.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(red_render.code, 0, "{}", red_render.stderr);
    assert!(
        red_render.json()["data"]["golden"].is_null(),
        "不传 --golden 时报告里不该多出字段"
    );

    let same = run(&[
        "render",
        red.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--golden",
        golden.to_str().unwrap(),
        "--golden-tolerance",
        "0",
        "--json",
    ]);
    assert_eq!(same.code, 0, "同场景重渲必须匹配：{}", same.stdout);
    let g = &same.json()["data"]["golden"];
    assert_eq!(g["matched"], true);
    assert_eq!(g["changed_pixels"], 0);
    assert_eq!(g["total_pixels"], 64 * 48);
    assert_eq!(g["tolerance"], 0);

    let different = run(&[
        "render",
        blue.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--golden",
        golden.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(
        different.code, 1,
        "黄金图不一致应当是运行错误（退出码 1）：{}",
        different.stdout
    );
    let env = different.json();
    assert_eq!(env["ok"], false);
    assert_eq!(env["error"]["kind"], "golden_mismatch");
    let msg = env["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("超出容差"), "消息要给出差异规模：{msg}");

    let missing = run(&[
        "render",
        red.to_str().unwrap(),
        "--out",
        dir.join("missing.png").to_str().unwrap(),
        "--golden",
        dir.join("nope.png").to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(
        missing.code, 1,
        "参考图不存在应当是运行错误（退出码 1）：{}",
        missing.stdout
    );
    assert_eq!(missing.json()["error"]["kind"], "io");

    // 以下三条都是**参数错误**，必须在创建 GPU 之前返回 2。
    let no_ref = run(&[
        "render",
        red.to_str().unwrap(),
        "--golden-tolerance",
        "1",
        "--json",
    ]);
    assert_eq!(no_ref.code, 2, "{}", no_ref.stdout);
    assert_eq!(no_ref.json()["error"]["kind"], "invalid");

    let dry = run(&[
        "render",
        red.to_str().unwrap(),
        "--dry-run",
        "--golden",
        golden.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(dry.code, 2, "{}", dry.stdout);
    assert_eq!(dry.json()["error"]["kind"], "scene");

    let multi = dir.join("multi.json");
    std::fs::write(
        &multi,
        r#"{
            "objects": [{ "name": "箱", "shape": { "type": "box", "size": [1,1,1] }, "position": [0,1,0] }],
            "render": { "output": "ignored.png", "width": 32, "height": 32, "msaa": 1,
                        "outputs": [{ "name": "a" }, { "name": "b" }] }
        }"#,
    )
    .unwrap();
    let multi_run = run(&[
        "render",
        multi.to_str().unwrap(),
        "--golden",
        golden.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(multi_run.code, 2, "{}", multi_run.stdout);
    assert_eq!(multi_run.json()["error"]["kind"], "scene");

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

/// IBL：环境贴图真的进入着色（没有太阳/环境光时，黑球被 HDR 天空照亮）。
#[test]
fn environment_map_changes_pixels() {
    let doctor = run(&["doctor", "--json"]).json();
    let gpu = doctor["data"]["gpu_available"].as_bool().unwrap();

    let dir = temp_dir("environment");
    write_test_hdr(&dir.join("sky.hdr"), 8, 4, [180, 150, 110], 128);
    let scene = |env: bool, out: &str| {
        let env_line = if env {
            r#", "environment_map": "sky.hdr", "environment_intensity": 1.0"#
        } else {
            ""
        };
        format!(
            r#"{{
                "lighting": {{ "preset": "none", "ambient": 0.0, "sky": false{env_line} }},
                "materials": {{ "m": {{ "color": [1,1,1], "metallic": 0.0, "roughness": 0.4 }} }},
                "objects": [
                    {{ "name": "ball", "shape": {{ "type": "sphere", "radius": 1 }}, "material": "m", "position": [0,1,0] }}
                ],
                "camera": {{ "position": [0,1,5], "look_at": [0,1,0] }},
                "simulate": {{ "steps": 0 }},
                "render": {{ "output": "{out}", "width": 160, "height": 120, "msaa": 1 }}
            }}"#
        )
    };
    let plain = dir.join("plain.json");
    let env = dir.join("env.json");
    std::fs::write(&plain, scene(false, "plain.png")).unwrap();
    std::fs::write(&env, scene(true, "env.png")).unwrap();
    if !gpu {
        let d = run(&["validate", env.to_str().unwrap(), "--json"]).json();
        assert_eq!(d["ok"], true, "没有 GPU 也应当通过场景校验：{d}");
        return;
    }
    for scene_path in [&plain, &env] {
        let out = dir.join(if *scene_path == plain {
            "plain.png"
        } else {
            "env.png"
        });
        let r = run(&[
            "render",
            scene_path.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    }
    let (w, h, plain_px) = decode_rgba(&dir.join("plain.png"));
    let (_, _, env_px) = decode_rgba(&dir.join("env.png"));
    let (pr, pg, pb) = center_mean(w, h, &plain_px);
    let (er, eg, eb) = center_mean(w, h, &env_px);
    assert!(
        er + eg + eb > pr + pg + pb + 60.0,
        "HDR 环境贴图应当照亮黑球：无图 ({pr:.1},{pg:.1},{pb:.1}) vs 有图 ({er:.1},{eg:.1},{eb:.1})"
    );
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

/// #15 的第二半：**局部轴按端拆分**（`axis_a` / `axis_b`）。
///
/// 锚点接线之后还剩一个坑：格式里只有一根 `axis`，装配时被同时喂给 `local_axis_a`
/// 和 `local_axis_b`。这只在"两端初始朝向相同"时才等于"同一个世界轴"——门板转了
/// 90°，世界 Y 在门板局部是 Z。此时两端共用一根局部轴，求解器会先把门板扭平
/// （表现成"门装上去自己弹回原位"），而且**没有任何诊断**。
///
/// 判据（纯物理报告，无显卡）：门板薄轴（局部 Y）在世界的指向。
/// * 按端给轴：1 秒后门绕世界 Y 转到 90°，薄轴始终**垂直**于铰链轴（还是块立着的门）；
/// * 两端共用 `axis`：薄轴被扭到与铰链轴**平行**（门被拧成了平躺的板）。
#[test]
fn hinge_local_axes_can_differ_per_side() {
    let dir = temp_dir("joint_axis_per_side");
    // 门板 size [2, 0.1, 1]：局部薄轴是 Y；rotation_deg [90,0,0] 后立在 XY 平面里，
    // 铰链边取局部 [-1,0,0]（正好落在静止的"框"原点上）。零重力；初始状态**正好在
    // 铰链的约束流形上**——绕世界 Y 以 90°/s 转，质心速度就是该圆周运动的速度
    // （`velocity` = ω × (质心 − 锚点) = [0,0,-1.5708]），所以理想铰链不该改变它。
    let scene = |joint: serde_json::Value| {
        serde_json::json!({
            "gravity": [0, 0, 0],
            "objects": [
                { "name": "框", "shape": { "type": "box", "size": [0.2, 0.2, 0.2] },
                  "body": "static", "material": "m" },
                { "name": "门", "shape": { "type": "box", "size": [2, 0.1, 1] },
                  "body": "dynamic", "material": "m", "position": [1, 0, 0],
                  // 90°/s：写 FRAC_PI_2 而不是字面量（clippy::approx_constant 会拒绝近似值）
                  "rotation_deg": [90, 0, 0],
                  "angular_velocity": [0, std::f64::consts::FRAC_PI_2, 0],
                  "velocity": [0, 0, -std::f64::consts::FRAC_PI_2], "density": 500 }
            ],
            "joints": [joint],
            "materials": { "m": { "color": [1, 1, 1] } },
            "camera": { "position": [0, 0, 10], "look_at": [0, 0, 0] },
            "simulate": { "steps": 60, "dt": 0.016666667 },
            "render": { "output": "ignored.png", "width": 32, "height": 32, "msaa": 1 }
        })
    };
    // 把报告的旋转（四元数 xyzw）作用在向量上：不引引擎的数学库，独立算一遍更可信。
    fn rotate(q: &[f64], v: [f64; 3]) -> [f64; 3] {
        let cross = |a: [f64; 3], b: [f64; 3]| {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let qv = [q[0], q[1], q[2]];
        let t = cross(qv, v);
        let t = [2.0 * t[0], 2.0 * t[1], 2.0 * t[2]];
        let u = cross(qv, t);
        [
            v[0] + q[3] * t[0] + u[0],
            v[1] + q[3] * t[1] + u[1],
            v[2] + q[3] * t[2] + u[2],
        ]
    }
    let report = |path: &std::path::Path| -> serde_json::Value {
        let r = run(&["simulate", path.to_str().unwrap(), "--json"]);
        assert_eq!(r.code, 0, "仿真失败：{}", r.stderr);
        r.json()
    };
    let door = |d: &serde_json::Value| -> (Vec<f64>, Vec<f64>) {
        let st = d["data"]["states"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "门")
            .unwrap_or_else(|| panic!("报告里应有「门」：{d}"))
            .clone();
        let nums = |v: &serde_json::Value| -> Vec<f64> {
            v.as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_f64().unwrap())
                .collect()
        };
        (nums(&st["position"]), nums(&st["rotation"]))
    };

    // 三份场景只差关节那一行：按端给轴 / 两端显式同轴 / 老写法（只有 axis）。
    let per_side = |axis_b: [f64; 3]| {
        serde_json::json!({ "type": "hinge", "a": "框", "b": "门",
                            "axis_a": [0, 1, 0], "axis_b": axis_b,
                            "anchor_a": [0, 0, 0], "anchor_b": [-1, 0, 0] })
    };
    // 正确：门板局部 Z 就是世界 -Y，所以铰链轴两端在世界里是同一根 Y 轴。
    let good = write_scene(&dir, "good.json", scene(per_side([0.0, 0.0, 1.0])));
    let (pos, rot) = door(&report(&good));
    let thin = rotate(&rot, [0.0, 1.0, 0.0]);
    assert!(
        thin[1].abs() < 0.05,
        "门板的薄轴必须始终垂直于铰链轴（世界 Y），实测 |n·Y|={:.3}（四元数 {rot:?}）",
        thin[1].abs()
    );
    let radius = (pos[0] * pos[0] + pos[1] * pos[1] + pos[2] * pos[2]).sqrt();
    let swept = (-pos[2]).atan2(pos[0]).to_degrees();
    assert!(
        (radius - 1.0).abs() < 0.02 && pos[1].abs() < 0.02,
        "铰链必须把锚点按住：质心到锚点的距离应恒为 1 米、不上下漂，实测 {pos:?}"
    );
    assert!(
        (swept - 90.0).abs() < 15.0,
        "初始状态就在约束流形上（90°/s），1 秒后应绕世界 Y 转过 ≈90°，实测 {swept:.1}°（质心 {pos:?}）"
    );

    // 装错：两端共用同一根局部轴（= 旧格式唯一能写出来的东西），门板被扭平。
    let explicit = scene(per_side([0.0, 1.0, 0.0]));
    let bad = write_scene(&dir, "same_local_axis.json", explicit.clone());
    let (pos_bad, rot_bad) = door(&report(&bad));
    let thin_bad = rotate(&rot_bad, [0.0, 1.0, 0.0]);
    assert!(
        thin_bad[1].abs() > 0.9,
        "两端共用局部轴时求解器会把门板扭平（薄轴转到世界 Y），实测 |n·Y|={:.3}（四元数 {rot_bad:?}）",
        thin_bad[1].abs()
    );
    // 扭平之后仍受点约束（锚点按住）——说明差别确实只出在"轴"上，不是求解器崩了。
    let radius_bad =
        (pos_bad[0] * pos_bad[0] + pos_bad[1] * pos_bad[1] + pos_bad[2] * pos_bad[2]).sqrt();
    assert!(
        (radius_bad - 1.0).abs() < 0.02,
        "点约束与轴无关，被扭平的门板仍应被按在半径 1 米上，实测 {pos_bad:?}"
    );
    // 对照：0.0002（正确）对 1.000（共用局部轴）——两个数相差三个数量级，判据不靠边。
    assert!(
        thin[1].abs() + 0.5 < thin_bad[1].abs(),
        "两种写法必须给出明显不同的门板朝向：正确 |n·Y|={:.4} vs 共用局部轴 |n·Y|={:.4}",
        thin[1].abs(),
        thin_bad[1].abs()
    );

    // **老场景逐位不变**：只写 `axis`（旧格式）与"把 axis 分别抄到 axis_a/axis_b"
    // 必须给出逐字节相同的刚体状态——这正是 `unwrap_or(axis)` 那条回落路径。
    // （两份场景里唯一的差别是关节字段的写法，物理输入完全同构。）
    let mut legacy = explicit;
    let j = legacy["joints"][0].as_object_mut().unwrap();
    j.remove("axis_a");
    j.remove("axis_b");
    j.insert("axis".into(), serde_json::json!([0, 1, 0]));
    let legacy = write_scene(&dir, "legacy_axis.json", legacy);
    assert_eq!(
        report(&bad)["data"]["states"],
        report(&legacy)["data"]["states"],
        "只写 axis 必须与两端显式写同一个轴逐字节一致（不写 = 老行为）"
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

/// 复合形状：一个刚体多个 Shape，端到端（装配 → 物理 → 报告 → 渲染场景）。
#[test]
fn compound_shapes_stand_on_their_own_legs() {
    let dir = temp_dir("compound");
    let scene = dir.join("table.json");
    // 桌子的"主形状"是顶板（悬在 y≈0.7），两条腿是附加部件（伸到 y=0）。
    // 如果附加部件不参与碰撞，刚体会带着顶板一路落到地面（位移 ≈ 0.7）——
    // 这就是这条端到端测试的判据。
    std::fs::write(
        &scene,
        r#"{
          "ground": { "size": 20 },
          "objects": [
            { "name": "桌子", "body": "dynamic", "position": [0, 0, 0],
              "shapes": [
                { "shape": { "type": "box", "size": [1.2, 0.1, 0.8] }, "position": [0, 0.75, 0] },
                { "shape": { "type": "box", "size": [0.1, 0.7, 0.1] }, "position": [0.5, 0.35, 0.3] },
                { "shape": { "type": "box", "size": [0.1, 0.7, 0.1] }, "position": [-0.5, 0.35, -0.3] } ] }
          ],
          "simulate": { "steps": 180, "dt": 0.0166667 }
        }"#,
    )
    .unwrap();

    let r = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let data = r.json();
    let sim = &data["data"];
    // 三个部件仍然是**一个**刚体
    assert_eq!(sim["dynamic"], 1, "三个部件该是一个动态刚体：{sim}");
    let table = sim["states"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "桌子")
        .expect("报告里该有桌子");
    let y = table["position"][1].as_f64().unwrap();
    assert!(
        y.abs() < 0.1,
        "桌子该靠两条腿站住（物体原点基本不动），实际 y = {y}"
    );
    assert!(
        table["displacement"].as_f64().unwrap() < 0.1,
        "站住的桌子位移该接近 0（掉下去会是 0.7 量级）：{table}"
    );

    // 对照：只有顶板（不带 shapes），放在同一高度 → 直接落到地面（位移 ≈ 0.95）。
    let plain = dir.join("plate.json");
    std::fs::write(
        &plain,
        r#"{
          "ground": { "size": 20 },
          "objects": [
            { "name": "桌子", "body": "dynamic", "position": [0, 1.0, 0],
              "shape": { "type": "box", "size": [1.2, 0.1, 0.8] } }
          ],
          "simulate": { "steps": 180, "dt": 0.0166667 }
        }"#,
    )
    .unwrap();
    let r2 = run(&["simulate", plain.to_str().unwrap(), "--json"]);
    assert_eq!(r2.code, 0, "stderr: {}", r2.stderr);
    let plate = r2.json()["data"]["states"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "桌子")
        .cloned()
        .expect("对照场景里该有桌子");
    let plate_drop = plate["displacement"].as_f64().unwrap();
    assert!(
        plate_drop > 0.5,
        "没有腿的顶板该落到地面（位移 ≈ 0.95），实际 {plate_drop}"
    );
    // 带腿的桌子站在地面附近（位移 ≈ 0）——这就是"附加部件真的在碰撞"的端到端证据
    assert!(
        table["displacement"].as_f64().unwrap() < plate_drop - 0.5,
        "带腿的桌子位移该远小于裸顶板：{} vs {plate_drop}",
        table["displacement"]
    );
}

/// 模板 + 实例：一段描述摆出多个独立刚体，端到端（展开 → 物理 → 报告）。
#[test]
fn templates_and_instances_make_several_independent_bodies() {
    let dir = temp_dir("templates");
    let scene = dir.join("stack.json");
    let text = r#"{
      "ground": { "size": 20 },
      "templates": {
        "块": { "shape": { "type": "box", "size": [1.0, 0.4, 1.0] },
                "body": "dynamic", "restitution": 0.0, "friction": 0.8 }
      },
      "objects": [
        { "name": "塔", "template": "块",
          "instances": [ { "position": [0, 0.2, 0] },
                         { "position": [0, 0.6, 0] },
                         { "position": [0, 1.0, 0] } ] }
      ],
      "simulate": { "steps": 240, "dt": 0.0166667 }
    }"#;
    std::fs::write(&scene, text).unwrap();

    let r = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let dat = r.json();
    let sim = &dat["data"];
    // 一份描述 → 三个**独立**刚体（不是共享一个刚体，也不是 GPU 实例化）
    assert_eq!(sim["dynamic"], 3, "三个副本该是三个独立动态刚体：{sim}");

    let states = sim["states"].as_array().unwrap();
    let y_of = |name: &str| -> f64 {
        states
            .iter()
            .find(|s| s["name"] == name)
            .unwrap_or_else(|| panic!("报告里该有 {name}: {sim}"))["position"][1]
            .as_f64()
            .unwrap()
    };
    // 名字缺省 `名字#序号`；三层箱子各自站在该在的高度（穿模/塌陷会立刻看出来）
    for (name, want) in [("塔#1", 0.2), ("塔#2", 0.6), ("塔#3", 1.0)] {
        let y = y_of(name);
        assert!((y - want).abs() < 0.12, "{name} 该停在 y≈{want}，实际 {y}");
    }
    // 三个副本都是独立物体：报告里名字各不相同，且各自的位移都很小（只是落到接触）
    let names: Vec<&str> = states
        .iter()
        .map(|s| s["name"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(
        names.iter().filter(|n| n.starts_with("塔#")).count(),
        3,
        "{names:?}"
    );

    // 关节/动作按名字引用实例（`塔#2` 是真实存在的刚体名）
    let pushed = dir.join("push.json");
    std::fs::write(
        &pushed,
        text.replace(
            "\"simulate\"",
            r#""actions": [ { "type": "velocity", "target": "塔#3", "at_step": 30, "value": [2, 0, 0] } ],
      "simulate""#,
        )
        .as_str(),
    )
    .unwrap();
    let r2 = run(&["simulate", pushed.to_str().unwrap(), "--json"]);
    assert_eq!(r2.code, 0, "stderr: {}", r2.stderr);
    let moved = r2.json()["data"]["states"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "塔#3")
        .cloned()
        .unwrap()["position"][0]
        .as_f64()
        .unwrap();
    // 顶上那个副本被推走（下面的两层还在原处）——"实例名是真实的刚体名"的证据
    assert!(moved > 0.1, "被推的副本该往 +x 走，实际 x = {moved}");
}

/// `schema --json-schema`：机器可读的 JSON Schema（编辑器补全 / agent 自检用）。
#[test]
fn json_schema_is_machine_readable() {
    // 不带 --json 时直接打印 schema 本身（好 pipe 进文件 / 编辑器配置）
    let raw = run(&["schema", "--json-schema"]);
    assert_eq!(raw.code, 0, "stderr: {}", raw.stderr);
    assert!(
        raw.stdout.trim_start().starts_with('{') && raw.stdout.contains("$defs"),
        "不带 --json 应当直接给 schema 本身：{}",
        raw.stdout.chars().take(80).collect::<String>()
    );
    let r = run(&["schema", "--json-schema", "--json"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let v = r.json();
    assert_eq!(v["ok"], true);
    let s = &v["data"];
    assert_eq!(
        s["$schema"], "https://json-schema.org/draft/2020-12/schema",
        "必须是标准 draft：{s}"
    );
    assert_eq!(s["$defs"]["object"]["additionalProperties"], false);
    assert_eq!(s["$defs"]["scene"]["additionalProperties"], false);
    // 新增过的东西必须在里面（templates / instances / shapes 都是最近几轮加的）
    let obj = &s["$defs"]["object"]["properties"];
    for key in ["template", "instances", "shape", "shapes", "ccd", "sensor"] {
        assert!(obj.get(key).is_some(), "object 少了 {key}：{obj}");
    }
    let scene = &s["$defs"]["scene"]["properties"];
    for key in ["templates", "objects", "joints", "actions", "probes"] {
        assert!(scene.get(key).is_some(), "scene 少了 {key}：{scene}");
    }
    // 枚举与校验器同一份事实
    let kinds: Vec<&str> = s["$defs"]["shapeKind"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "box",
            "sphere",
            "capsule",
            "cylinder",
            "cone",
            "rounded_box",
            "heightfield",
            "plane",
            "convex"
        ]
    );
    let joints: Vec<&str> = s["$defs"]["jointKind"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(joints, vec!["fixed", "ball", "hinge", "distance", "slider"]);

    // 不带 flag 的老用法不变：还是那套人读的语法表
    let g = run(&["schema", "--json"]).json();
    assert!(
        g["data"]["scene"]["object"]["shape"].is_string(),
        "不带 flag 时应当还是语法说明：{}",
        g["data"]
    );
}

/// `--camera <名字>`：未知名属于"输入有问题，改参数再跑"——退出码 2、
/// `error.kind = unknown_camera`、消息里有那个名字、hint 指路。
///
/// 用 `--dry-run`：这条路径在**取相机**时就失败了，根本不该初始化 GPU
/// （CI 上没有显卡，能跑到这里本身就是一条约束）。
/// 真渲染多输出：每张真的按自己的 width/height 写出来（有 GPU 才跑）。
#[test]
fn multiple_render_outputs_write_each_requested_size_when_a_gpu_is_present() {
    let doctor = run(&["doctor", "--json"]).json();
    if !doctor["data"]["gpu_available"].as_bool().unwrap_or(false) {
        eprintln!("跳过多输出真渲染：没有可用 GPU");
        return;
    }
    let dir = temp_dir("multi_output_png");
    let shots = dir.join("shots");
    let path = dir.join("s.json");
    let scene = format!(
        r#"{{
            "format": "quench3d.scene", "version": 2, "name": "多输出真渲染",
            "ground": {{ "size": 10 }},
            "camera": {{ "position": [7, 4.5, 8], "look_at": [0, 1, 0] }},
            "render": {{
                "output": {out},
                "outputs": [
                    {{ "name": "big", "width": 64, "height": 32 }},
                    {{ "name": "small", "width": 32, "height": 16 }}
                ]
            }}
        }}"#,
        out = serde_json::to_string(&shots.join("shot.png")).unwrap()
    );
    std::fs::write(&path, scene).unwrap();

    let r = run(&["render", path.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    let v = r.json();
    let outs = v["data"]["outputs"].as_array().expect("要有 outputs 明细");
    assert_eq!(outs.len(), 2);
    for (i, (name, w, h)) in [("big", 64, 32), ("small", 32, 16)].iter().enumerate() {
        let out = std::path::Path::new(outs[i]["output"].as_str().unwrap());
        assert!(out.exists(), "第 {i} 张不存在：{}", out.display());
        assert_eq!(outs[i]["name"], *name);
        let (dw, dh, _) = decode_rgba(out);
        assert_eq!((dw, dh), (*w, *h), "第 {i} 张尺寸不对：{name}");
    }
}

#[test]
fn camera_flag_reports_unknown_names_without_touching_the_gpu() {
    let dir = temp_dir("camera_flag");
    let path = dir.join("s.json");
    std::fs::write(
        &path,
        r#"{
            "format": "quench3d.scene", "version": 2,
            "name": "相机选择",
            "ground": { "size": 10 },
            "camera": { "position": [7, 4.5, 8], "look_at": [0, 1, 0] },
            "cameras": { "正面": { "position": [0, 1, 8], "look_at": [0, 1, 0] } }
        }"#,
    )
    .unwrap();
    let r = run(&[
        "render",
        path.to_str().unwrap(),
        "--dry-run",
        "--camera",
        "斜后方",
        "--json",
    ]);
    assert_eq!(r.code, 2, "未知名应当是退出码 2：{}", r.stderr);
    let v = r.json();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["kind"], "unknown_camera");
    let msg = v["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("斜后方"), "消息里要有那个名字：{msg}");
    assert!(msg.contains("正面"), "消息里要列出可用名字：{msg}");
    let hint = v["error"]["hint"].as_str().unwrap_or_default();
    assert!(hint.contains("cameras"), "hint 要指到 cameras 段：{hint}");

    // `default` 是 camera 段的别名：不该被当成未知名
    let ok = run(&[
        "render",
        path.to_str().unwrap(),
        "--dry-run",
        "--camera",
        "default",
        "--json",
    ]);
    assert_ne!(ok.code, 2, "default 不该报 unknown_camera：{}", ok.stdout);
    assert_eq!(
        ok.json()["data"]["camera_name"],
        "default",
        "显式选 default 也要在报告里说清"
    );

    // 已知名字：报告要说清实际用的是哪台（多输出/多相机排查靠这个）
    let known = run(&[
        "render",
        path.to_str().unwrap(),
        "--dry-run",
        "--camera",
        "正面",
        "--json",
    ]);
    assert_eq!(known.code, 0, "{}", known.stderr);
    assert_eq!(known.json()["data"]["camera_name"], "正面");
}

/// 多输出（gap #16 第 3、4 步）：`render.outputs[]` 要在**写任何东西之前**把每张的
/// 目标路径与尺寸说清楚；`--no-clobber` 必须整体先查，不能留下半套文件。
///
/// 全部走 `--dry-run`：不需要显卡，CI 上也能跑。
#[test]
fn multiple_render_outputs_resolve_before_writing_and_honour_no_clobber() {
    let dir = temp_dir("multi_output");
    let shots = dir.join("shots");
    let path = dir.join("s.json");
    let scene = format!(
        r#"{{
            "format": "quench3d.scene", "version": 2, "name": "多输出",
            "ground": {{ "size": 10 }},
            "camera": {{ "position": [7, 4.5, 8], "look_at": [0, 1, 0] }},
            "cameras": {{ "俯视": {{ "position": [0, 9, 0.1], "look_at": [0, 1, 0], "fov_deg": 35 }} }},
            "render": {{
                "output": {out},
                "width": 100, "height": 50,
                "outputs": [
                    {{ "name": "a", "width": 320, "height": 200 }},
                    {{ "name": "b", "camera": "俯视", "width": 640, "height": 400 }}
                ]
            }}
        }}"#,
        out = serde_json::to_string(&shots.join("shot.png")).unwrap()
    );
    std::fs::write(&path, scene).unwrap();

    let r = run(&["render", path.to_str().unwrap(), "--dry-run", "--json"]);
    assert_eq!(r.code, 0, "stderr: {}", r.stderr);
    let v = r.json();
    let d = &v["data"];
    assert_eq!(d["dry_run"], true);
    let outs = d["outputs"].as_array().expect("要有 outputs 明细");
    assert_eq!(outs.len(), 2, "两张都要报出来：{d}");
    assert_eq!(outs[0]["name"], "a");
    assert_eq!(outs[0]["width"], 320);
    assert_eq!(outs[0]["height"], 200);
    assert!(
        outs[0]["output"].as_str().unwrap().ends_with("shot-a.png"),
        "第一张写到目录里：{}",
        outs[0]["output"]
    );
    assert_eq!(outs[1]["name"], "b");
    assert_eq!(outs[1]["width"], 640, "每张自己的尺寸");
    assert_eq!(outs[1]["height"], 400);
    assert_eq!(outs[1]["camera_name"], "俯视", "要报告这一张实际用的相机");
    assert!(
        outs[0].get("camera_name").is_none(),
        "用缺省 camera 段的那张不该多出 camera_name：{}",
        outs[0]
    );
    assert!(
        outs[1]["output"].as_str().unwrap().ends_with("shot-b.png"),
        "带扩展名的 output 当前缀：{}",
        outs[1]["output"]
    );
    let would: Vec<&str> = d["would_write"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(would.len(), 2, "两张都该在 would_write 里：{would:?}");
    // 路径断言必须与平台无关：Windows 用 `\`，所以只查文件名与父目录名
    for o in outs {
        let p = std::path::Path::new(o["output"].as_str().unwrap());
        assert!(
            p.parent().unwrap().ends_with("shots"),
            "两张都该放在 shots/ 下：{}",
            p.display()
        );
    }
    assert!(!shots.exists(), "--dry-run 不该建目录/写盘");

    // --no-clobber：任一目标已存在 → 整体退出 2，且**另一张**也不能被写出来
    std::fs::create_dir_all(&shots).unwrap();
    std::fs::write(shots.join("shot-a.png"), b"x").unwrap();
    let r = run(&[
        "render",
        path.to_str().unwrap(),
        "--dry-run",
        "--no-clobber",
        "--json",
    ]);
    assert_eq!(r.code, 2, "已存在要报 2：{}", r.stdout);
    let v = r.json();
    assert_eq!(v["error"]["kind"], "output_exists");
    assert!(
        v["error"]["path"]
            .as_str()
            .unwrap_or_default()
            .ends_with("shot-a.png"),
        "要说清是哪个目标：{}",
        v["error"]
    );
    assert!(!shots.join("shot-b.png").exists(), "整体先查：不该留下半套");

    // 向后兼容：没写 outputs 的场景，报告里没有 outputs 字段，would_write 只有那一个
    let plain = dir.join("plain.json");
    std::fs::write(
        &plain,
        format!(
            r#"{{"format":"quench3d.scene","version":2,"ground":{{"size":10}},
                "render":{{"output":{out},"width":64,"height":32}}}}"#,
            out = serde_json::to_string(&dir.join("one.png")).unwrap()
        ),
    )
    .unwrap();
    let r = run(&["render", plain.to_str().unwrap(), "--dry-run", "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let v = r.json();
    assert!(
        v["data"].get("outputs").is_none(),
        "老场景不该多出 outputs 字段：{}",
        v["data"]
    );
    assert!(
        v["data"].get("camera_name").is_none(),
        "没传 --camera 的老场景不该多出 camera_name：{}",
        v["data"]
    );
    assert_eq!(v["data"]["would_write"].as_array().unwrap().len(), 1);
}

/// 关键帧动画：`animations[]` 真的按时间推动物体（用 `simulate` 报告的 `states` 观测）。
///
/// 观测点选得很"笨"但很硬：0.5 秒时 y 应当到 5，循环播放 1.5 秒后应当回到起点附近。
#[test]
fn keyframe_animations_move_bodies_on_a_schedule() {
    let dir = temp_dir("animation");
    let scene = |extra: &str| -> String {
        format!(
            r#"{{
                "format": "quench3d.scene", "version": 2, "name": "动画",
                "ground": {{ "size": 20 }},
                "objects": [{{
                    "name": "箱", "shape": {{ "type": "box" }},
                    "position": [0, 1, 0], "body": "kinematic"
                }}],
                "simulate": {{ "steps": 30, "dt": 0.01666667 }},
                "animations": [{extra}]
            }}"#
        )
    };
    let y_at = |path: &std::path::Path| -> f64 {
        let r = run(&["simulate", path.to_str().unwrap(), "--json"]);
        assert_eq!(r.code, 0, "stderr: {}", r.stderr);
        let d = r.json();
        assert_eq!(d["ok"], true, "{d}");
        d["data"]["states"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "箱")
            .unwrap_or_else(|| panic!("报告里应有 箱：{d}"))["position"][1]
            .as_f64()
            .unwrap()
    };

    // 0 → 0.5 → 1.0 秒，y 从 1 升到 5 再回落；30 步 × 1/60 秒 = 0.5 秒
    let path = dir.join("up.json");
    std::fs::write(
        &path,
        scene(
            r#"{ "target": "箱", "position": {
            "times": [0.0, 0.5, 1.0], "values": [[0,1,0], [0,5,0], [0,1,0]] } }"#,
        ),
    )
    .unwrap();
    let y = y_at(&path);
    assert!((y - 5.0).abs() < 0.35, "0.5 秒时应当接近顶点 5，实际 {y}");

    // 循环：轨道只有 1 秒，跑 1.5 秒 ≈ 回到 0.5 秒处，也就是又接近顶部
    let path = dir.join("loop.json");
    std::fs::write(
        &path,
        scene(
            r#"{ "target": "箱", "loop": true, "position": {
            "times": [0.0, 0.5, 1.0], "values": [[0,1,0], [0,5,0], [0,1,0]] } }"#,
        ),
    )
    .unwrap();
    let r = run(&[
        "simulate",
        path.to_str().unwrap(),
        "--json",
        "--steps",
        "90",
    ]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let y = r.json()["data"]["states"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "箱")
        .unwrap()["position"][1]
        .as_f64()
        .unwrap();
    assert!(y > 4.5, "循环 1.5 秒后应当又接近顶部，实际 {y}");

    // 目标不存在：这是"跑了半天没动"的静默失效，必须当场报错
    let path = dir.join("missing.json");
    std::fs::write(
        &path,
        scene(
            r#"{ "target": "不存在的物体", "position": { "times": [0.0], "values": [[0,1,0]] } }"#,
        ),
    )
    .unwrap();
    let r = run(&["simulate", path.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 2, "{}", r.stdout);
    let v = r.json();
    assert_eq!(v["ok"], false);
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("不存在的物体"),
        "错误里要点出目标名：{}",
        v["error"]
    );

    // 轨道自相矛盾（时间不递增）：validate 要报出来，而不是等运行期
    let path = dir.join("bad.json");
    std::fs::write(
        &path,
        scene(r#"{ "target": "箱", "position": { "times": [0.5, 0.5], "values": [[0,1,0], [0,2,0]] } }"#),
    )
    .unwrap();
    let r = run(&["validate", path.to_str().unwrap(), "--json"]);
    // 注意 `validate` 的约定：**命令本身**成功（ok: true），"场景不合法"体现在
    // `data.valid = false` 与 diagnostics 里。
    let v = r.json();
    assert_eq!(
        v["data"]["valid"], false,
        "时间不严格递增必须判为不合法：{v}"
    );
    assert_eq!(v["data"]["errors"], 1, "{v}");
    assert!(
        v["data"]["diagnostics"]
            .as_array()
            .map(|d| d.iter().any(|x| x["message"]
                .as_str()
                .unwrap_or_default()
                .contains("严格递增")))
            .unwrap_or(false),
        "要指出是严格递增的问题：{v}"
    );
}

/// 角速度推算 + "关键帧存姿态"的语义边界。
///
/// 两件事都用无显卡的路径观测：
/// 1. 被动画转起来的球，靠**摩擦**滚起来（如果只设姿态不补角速度，接触点没有切向速度，
///    球会原地空转、位移恒为 0——这正是本轮要修的）；
/// 2. 旋转关键帧一段最多转 180°（slerp 走最短弧），跨度超了要给**警告**而不是让人干瞪眼。
#[test]
fn animated_rotation_rolls_through_friction_and_warns_about_half_turn_segments() {
    let dir = temp_dir("rolling");
    let scene = |values: &str| -> String {
        format!(
            r#"{{
                "format": "quench3d.scene", "version": 2, "name": "滚动",
                "ground": {{ "size": 20 }},
                "objects": [{{
                    "name": "球", "shape": {{ "type": "sphere", "radius": 0.5 }},
                    "position": [0, 0.5, 0], "body": "dynamic", "mass": 1.0,
                    "friction": 0.8, "restitution": 0.0
                }}],
                "simulate": {{ "steps": 60, "dt": 0.01666667 }},
                "animations": [{{ "target": "球", "rotation": {{
                    "times": [0.0, 1.0], "values": {values} }} }}]
            }}"#
        )
    };
    let path = dir.join("roll.json");
    std::fs::write(&path, scene("[[0,0,0], [0,0,90]]")).unwrap();
    let r = run(&["simulate", path.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let d = r.json();
    let st = d["data"]["states"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "球")
        .unwrap()
        .clone();
    let x = st["position"][0].as_f64().unwrap();
    let disp = st["displacement"].as_f64().unwrap();
    // 绕 +Z 转 → 接触点切向朝 -X → 摩擦把球推向 -X（数值见 CHANGELOG：60 步约 -0.38 m）
    assert!(x < -0.25, "球应当被摩擦推向 -X，实测 x={x}（报告 {d}）");
    assert!(disp > 0.25, "位移应当可观，实测 {disp}");
    assert!(
        st["touching"].as_bool().unwrap_or(false),
        "它得真的贴在地上才会被摩擦推动"
    );

    // 720° 一段：slerp 走最短弧 → 实际退化（这里 0° 与 720° 是同一个姿态，等于完全不动），
    // 校验必须给出**警告**（不是错误：场景还是合法的），并提示补中间关键帧。
    let path = dir.join("cap.json");
    std::fs::write(&path, scene("[[0,0,0], [0,0,720]]")).unwrap();
    let r = run(&["validate", path.to_str().unwrap(), "--json"]);
    let v = r.json();
    assert_eq!(v["data"]["valid"], true, "只是语义警告，不该判不合法：{v}");
    assert_eq!(v["data"]["errors"], 0, "{v}");
    assert!(
        v["data"]["warnings"].as_u64().unwrap_or(0) >= 1,
        "要有警告：{v}"
    );
    assert!(
        v["data"]["diagnostics"]
            .as_array()
            .map(|d| d.iter().any(|x| {
                x["severity"] == "warning"
                    && x["message"].as_str().unwrap_or_default().contains("180")
            }))
            .unwrap_or(false),
        "警告要说清 180° 这件事：{v}"
    );
}

/// 关键帧动画的**混合权重**：一条动画的 `weight` 是"从当前姿态朝它走多远"，
/// 所以同一个物体挂两条动画时，后一条的权重就是两条 clip 的混合比例。
///
/// 观测口径还是 `simulate` 报告的 `states`（无显卡）。
#[test]
fn animation_weights_blend_two_clips_and_zero_means_no_effect() {
    let dir = temp_dir("blend");
    // 箱子的 y：clip A 让它 0.5 秒时到 5；clip B 让它一直停在 1。
    // A 权重 1、B 权重 0.5 → 最终应当在 3（lerp(5, 1, 0.5)）。
    let scene = |weights: &str| -> String {
        format!(
            r#"{{
                "format": "quench3d.scene", "version": 2, "name": "混合",
                "ground": {{ "size": 20 }},
                "objects": [{{
                    "name": "箱", "shape": {{ "type": "box" }},
                    "position": [0, 1, 0], "body": "kinematic"
                }}],
                "simulate": {{ "steps": 30, "dt": 0.01666667 }},
                "animations": [{weights}]
            }}"#
        )
    };
    let y_of = |path: &std::path::Path| -> f64 {
        let r = run(&["simulate", path.to_str().unwrap(), "--json"]);
        assert_eq!(r.code, 0, "{}", r.stderr);
        r.json()["data"]["states"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == "箱")
            .unwrap()["position"][1]
            .as_f64()
            .unwrap()
    };

    let path = dir.join("two.json");
    std::fs::write(
        &path,
        scene(
            r#"{ "target": "箱", "weight": 1.0, "position": { "times": [0.0, 1.0], "values": [[0,1,0], [0,9,0]] } },
               { "target": "箱", "weight": 0.5, "position": { "times": [0.0, 1.0], "values": [[0,1,0], [0,1,0]] } }"#,
        ),
    )
    .unwrap();
    // 0.5 秒时 clip A 在 y=5、clip B 恒为 1 → lerp(5, 1, 0.5) = 3
    let y = y_of(&path);
    assert!(
        (y - 3.0).abs() < 0.4,
        "两条 clip 的混合应当落在中点 3，实测 {y}"
    );

    // 权重 0：这条动画完全不参与（姿态保持出生点）
    let path = dir.join("zero.json");
    std::fs::write(
        &path,
        scene(r#"{ "target": "箱", "weight": 0.0, "position": { "times": [0.0, 1.0], "values": [[0,1,0], [0,9,0]] } }"#),
    )
    .unwrap();
    let y = y_of(&path);
    assert!((y - 1.0).abs() < 0.05, "权重 0 = 不生效，实测 {y}");

    // 校验：权重必须在 0~1
    let path = dir.join("bad.json");
    std::fs::write(
        &path,
        scene(r#"{ "target": "箱", "weight": 1.5, "position": { "times": [0.0], "values": [[0,1,0]] } }"#),
    )
    .unwrap();
    let r = run(&["validate", path.to_str().unwrap(), "--json"]);
    let v = r.json();
    assert_eq!(v["data"]["valid"], false, "权重超范围要判不合法：{v}");
    assert!(
        v["data"]["diagnostics"]
            .as_array()
            .map(|d| d
                .iter()
                .any(|x| x["message"].as_str().unwrap_or_default().contains("0~1")))
            .unwrap_or(false),
        "要指出权重范围：{v}"
    );
}

/// 模型缓存（gap #27 的 CPU 侧）：同一次运行里同一个文件只解析一次。
///
/// 观测口径是 `simulate` 报告里的 `model_cache`（不需要显卡：`models[]` 的碰撞体
/// 是 CPU 侧建出来的）。同一路径写两条 `models[]` 就是"两把相同的椅子"这种真实用法。
#[test]
fn model_cache_parses_each_file_once_per_run() {
    let dir = temp_dir("model_cache");
    let obj = dir.join("chair.obj");
    std::fs::write(&obj, "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n").unwrap();
    let path = dir.join("scene.json");
    let scene = |models: &str| -> String {
        format!(
            r#"{{
                "format": "quench3d.scene", "version": 2, "name": "缓存",
                "ground": {{ "size": 20 }},
                "objects": [{{ "name": "球", "shape": {{ "type": "sphere", "radius": 0.5 }},
                              "position": [0, 2, 0], "body": "dynamic" }}],
                "models": [{models}],
                "simulate": {{ "steps": 5, "dt": 0.01666667 }}
            }}"#
        )
    };
    let entry = |x: f32| {
        format!(
            r#"{{ "path": {:?}, "collide": true, "position": [{x}, 0, 0] }}"#,
            obj.to_string_lossy()
        )
    };

    // 同一个文件两条 → 1 次解析 + 1 次命中
    std::fs::write(&path, scene(&format!("{}, {}", entry(0.0), entry(3.0)))).unwrap();
    let r = run(&["simulate", path.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let d = r.json();
    let mc = d["data"]["model_cache"].clone();
    assert_eq!(mc["misses"], 1, "同一个文件只该解析一次：{d}");
    assert_eq!(mc["hits"], 1, "第二条引用应当命中缓存：{d}");
    assert_eq!(mc["entries"], 1, "缓存里就该留一个条目：{d}");
    assert!(mc["bytes"].as_u64().unwrap_or(0) > 0, "要报出占用：{d}");
    // 预算存在且比占用大（免得有人把预算调成 0 还宣称有缓存）
    assert!(
        mc["budget"].as_u64().unwrap() > mc["bytes"].as_u64().unwrap(),
        "{d}"
    );

    // 没有模型的场景：报告里**不出现** model_cache（老报告逐字节不变）
    std::fs::write(&path, scene("")).unwrap();
    let r = run(&["simulate", path.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let d = r.json();
    assert!(
        d["data"].get("model_cache").is_none(),
        "没模型就不该有缓存统计：{d}"
    );

    // 两个**不同**的文件 → 两次解析、零命中（不同的 key 不能互相顶替）
    let obj2 = dir.join("table.obj");
    std::fs::write(&obj2, "v 0 0 0\nv 2 0 0\nv 0 2 0\nf 1 2 3\n").unwrap();
    std::fs::write(
        &path,
        scene(&format!(
            "{}, {{ \"path\": {:?}, \"collide\": true }}",
            entry(0.0),
            obj2.to_string_lossy()
        )),
    )
    .unwrap();
    let r = run(&["simulate", path.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let mc = r.json()["data"]["model_cache"].clone();
    assert_eq!(mc["misses"], 2, "两个不同文件要各解析一次：{mc}");
    assert_eq!(mc["hits"], 0, "不同文件之间不该有命中：{mc}");
    assert_eq!(mc["entries"], 2, "{mc}");

    // 说明：**文件改动后失效**没法在这里观测——每个 `run()` 都是新进程，
    // 缓存本来就是"每次运行一份"。那条行为由 `assets::cache_tests`
    // 的进程内单测（`cache_hits_misses_and_invalidates_on_file_change`）钉住。
}

/// 级联阴影（gap #24）：字段校验 + 从场景文件真的接进渲染器。
///
/// 拆成"不依赖显卡"和"依赖显卡"两半：
/// 1. 校验部分（0/5、split_lambda 越界必须报错）**任何机器都能跑**；
/// 2. 渲染部分要有 GPU——无 GPU 时退出码 3 就算跳过（CI 是这种情况）。
///    有 GPU 时断言"级联数真的改变了画面"：这是"字段接上了"的证据，
///    而不是"代码看起来接上了"。
#[test]
fn shadow_cascades_are_validated_and_change_the_image_when_a_gpu_is_present() {
    let dir = temp_dir("cascades");
    let scene = |out: &str, cascades: u32, lambda: f64| {
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
            "lighting": {
                "sky": false, "grid": false,
                "shadow_quality": { "cascades": cascades, "split_lambda": lambda }
            }
        })
    };

    // ---- 1. 校验（不需要显卡）----
    for (n, lambda, ok) in [
        (1u32, 0.5f64, true),
        (4, 0.5, true),
        (0, 0.5, false),
        (5, 0.5, false),
    ] {
        let p = write_scene(
            &dir,
            &format!("v{n}-{lambda}.json"),
            scene("v.png", n, lambda),
        );
        let r = run(&["validate", p.to_str().unwrap(), "--json"]);
        let v = r.json();
        assert_eq!(v["data"]["valid"], ok, "cascades={n} 的合法性判断不对：{v}");
        if !ok {
            assert!(
                v["data"]["diagnostics"]
                    .as_array()
                    .map(|d| d
                        .iter()
                        .any(|x| x["path"] == "lighting.shadow_quality.cascades"))
                    .unwrap_or(false),
                "要指出是 cascades 的问题：{v}"
            );
        }
    }
    let p = write_scene(&dir, "bad-lambda.json", scene("v.png", 2, 1.5));
    let v = run(&["validate", p.to_str().unwrap(), "--json"]).json();
    assert_eq!(v["data"]["valid"], false, "{v}");
    assert!(
        v["data"]["diagnostics"]
            .as_array()
            .map(|d| d
                .iter()
                .any(|x| x["path"] == "lighting.shadow_quality.split_lambda"))
            .unwrap_or(false),
        "要指出是 split_lambda 的问题：{v}"
    );

    // ---- 2. 渲染（有 GPU 才有意义）----
    let single = dir.join("single.png");
    let quad = dir.join("quad.png");
    let p1 = write_scene(&dir, "single.json", scene("single.png", 1, 0.5));
    let p4 = write_scene(&dir, "quad.json", scene("quad.png", 4, 0.5));
    // 输出走 `--out`（绝对路径）：场景里的 `render.output` 是相对量，
    // 相对路径会落到**进程当前目录**（也就是仓库里的 crates/quench3d-cli/），
    // 跑一次测试就往仓库里丢两张 PNG——`git add -A` 很容易顺手把它们提交上去。
    let r1 = run(&[
        "render",
        p1.to_str().unwrap(),
        "--out",
        single.to_str().unwrap(),
        "--json",
    ]);
    let r4 = run(&[
        "render",
        p4.to_str().unwrap(),
        "--out",
        quad.to_str().unwrap(),
        "--json",
    ]);
    if r1.code == 3 && r4.code == 3 {
        return; // 没有 GPU：这台机器上只能验证校验部分
    }
    assert_eq!(r1.code, 0, "{}", r1.stderr);
    assert_eq!(r4.code, 0, "{}", r4.stderr);
    let (_, _, a) = decode_rgba(&single);
    let (_, _, b) = decode_rgba(&quad);
    let changed = a
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0.iter())
        .filter(|(x, y)| {
            (x[0] as i32 - y[0] as i32).abs()
                + (x[1] as i32 - y[1] as i32).abs()
                + (x[2] as i32 - y[2] as i32).abs()
                > 6
        })
        .count();
    assert!(
        changed > 24,
        "级联数只改了 {changed} 个像素：字段像是没接进渲染器"
    );
    // 两种模式下都得真有影子：最暗的像素要明显暗过平均值
    for (name, img) in [("单级", &a), ("四级", &b)] {
        let lum: Vec<f64> = img
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| 0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64)
            .collect();
        let mean = lum.iter().sum::<f64>() / lum.len() as f64;
        let min = lum.iter().cloned().fold(f64::MAX, f64::min);
        assert!(
            mean - min > 20.0,
            "{name}阴影：画面最暗 {min:.1} 与均值 {mean:.1} 差得太少，可能根本没画出影子"
        );
    }
}

/// 资产搜索路径（gap #11）：贴图与模型走**同一套**候选，且不依赖进程当前工作目录。
///
/// 修掉的那个不一致：贴图按"场景目录 → 当前工作目录 → 引擎搜索路径"解析，
/// 模型却只按加载器自己的规则（原路径、`assets/`、`quench3d/assets/`）——
/// **不含场景目录**。于是"模型就放在场景文件旁边"这种最自然的写法，
/// 在工作目录不等于场景目录时直接加载失败（而 `validate` 用的又是第三套规则）。
///
/// 这条测试全程不需要显卡（用 `simulate` 走物理侧的模型加载）。
#[test]
fn scene_relative_model_paths_do_not_depend_on_the_working_directory() {
    let dir = temp_dir("assets_search");
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let obj = "o tri\nv 0 0 0\nv 2 0 0\nv 0 2 0\nf 1 2 3\n";
    std::fs::write(dir.join("near.obj"), obj).unwrap();
    std::fs::write(lib.join("far.obj"), obj).unwrap();

    let scene = |name: &str, models: serde_json::Value, assets: Option<serde_json::Value>| {
        let mut v = serde_json::json!({
            "objects": [
                { "name": "球", "shape": { "type": "sphere", "radius": 0.4 },
                  "position": [0, 3, 0], "body": "dynamic" }
            ],
            "models": models,
            "simulate": { "steps": 2, "dt": 0.016 }
        });
        if let Some(a) = assets {
            v["assets"] = a;
        }
        write_scene(&dir, name, v)
    };

    // 1) 模型就在场景文件旁边：不写 assets 也要加载得到（回归：修之前这里必失败）
    let p = scene(
        "near.json",
        serde_json::json!([{ "path": "near.obj", "collide": true }]),
        None,
    );
    let r = run(&["simulate", p.to_str().unwrap(), "--json"]);
    assert_eq!(
        r.code, 0,
        "场景目录里的模型应当加载得到（工作目录不是场景目录）：{}",
        r.stderr
    );
    assert_eq!(
        run(&["validate", p.to_str().unwrap(), "--json"]).json()["data"]["valid"],
        true
    );

    // 2) 模型在 `../lib`：靠 assets.search_paths 找得到（相对路径以场景目录为基准）
    let p = scene(
        "far.json",
        serde_json::json!([{ "path": "far.obj", "collide": true }]),
        Some(serde_json::json!({ "search_paths": ["lib"] })),
    );
    let r = run(&["simulate", p.to_str().unwrap(), "--json"]);
    assert_eq!(
        r.code, 0,
        "assets.search_paths 里的模型应当找得到：{}",
        r.stderr
    );

    // 3) 找不到：错误里要列出"找过"的位置（含搜索路径），而不是一句 CWD 相对路径的 IO 错误
    let p = scene(
        "missing.json",
        serde_json::json!([{ "path": "nope_3d7f.obj", "collide": true }]),
        Some(serde_json::json!({ "search_paths": ["lib"] })),
    );
    let r = run(&["simulate", p.to_str().unwrap(), "--json"]);
    // 退出码 2（输入有问题）：`validate` 这一步就会拦下来，附上"找过哪些位置"
    assert_eq!(r.code, 2, "{:?}", r.json());
    let msg = r.json().to_string();
    assert!(
        msg.contains("找过：") && msg.contains("lib"),
        "错误要列出找过哪里：{msg}"
    );
}

/// `--asset-path`：一次运行用的额外资源搜索路径（不改场景文件）。
///
/// 与 `assets.search_paths` 的区别是"谁的约定"：搜索路径写进场景是这份场景自己的规矩，
/// `--asset-path` 是"这一次运行"的临时补充（渲染农场、CI、临时素材目录）。
/// 多个目录用 `:` 分隔（与 PATH 一致）。这条测试不需要显卡。
#[test]
fn cli_asset_path_flag_resolves_assets_without_touching_the_scene() {
    let dir = temp_dir("cli_asset_path");
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(
        lib.join("tri.obj"),
        "o tri\nv 0 0 0\nv 2 0 0\nv 0 2 0\nf 1 2 3\n",
    )
    .unwrap();
    let scene = write_scene(
        &dir,
        "level.json",
        serde_json::json!({
            "objects": [ { "name": "球", "shape": { "type": "sphere", "radius": 0.4 },
                           "position": [0, 3, 0], "body": "dynamic" } ],
            "models": [ { "path": "tri.obj", "collide": true } ],
            "simulate": { "steps": 2, "dt": 0.016 }
        }),
    );

    // 不写搜索路径、不加旗标 → 找不到（确认这条测试真的在测旗标）
    let r = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 2, "没有搜索路径时应当报找不到模型");

    // 加旗标 → 找得到；顺带确认场景文件**没被改写**（旗标不是写进场景的）
    let before = std::fs::read(&scene).unwrap();
    let r = run(&[
        "simulate",
        scene.to_str().unwrap(),
        "--asset-path",
        lib.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(
        r.code,
        0,
        "加上 --asset-path 应当找得到模型：{:?}",
        r.json()
    );
    assert_eq!(
        std::fs::read(&scene).unwrap(),
        before,
        "旗标只影响这一次运行，不该改场景文件"
    );

    // 多个目录用 `:` 分隔；不存在的那些只是多试一次，不影响结果
    let bogus = dir.join("nope");
    let sep = if cfg!(windows) { ';' } else { ':' };
    let list = format!("{}{sep}{}", bogus.display(), lib.display());
    let r = run(&[
        "simulate",
        scene.to_str().unwrap(),
        "--asset-path",
        &list,
        "--json",
    ]);
    assert_eq!(
        r.code,
        0,
        "`:` 分隔的列表里只要有一个命中就行：{:?}",
        r.json()
    );
}

// ---------------------------------------------------------------------------
// include：跨文件组合（gap §3.2 #13）
// ---------------------------------------------------------------------------

/// 把场景拆成多文件之后，**物理结果必须与手写一份单文件逐位相同**。
///
/// 这是"合并语义正确"最强的判据：顺序、模板展开、材质解析、关节引用只要有一处
/// 因为"并进来"而变了，`states` 里的坐标就会差出浮点位。
#[test]
fn include_split_scene_simulates_bit_identically_to_a_single_file() {
    let dir = temp_dir("include_equiv");

    // 多文件版：根文件 + 一个定义模板与材质的片段
    std::fs::create_dir_all(dir.join("parts")).unwrap();
    std::fs::write(
        dir.join("parts/blocks.json"),
        r#"{
          "format": "quench3d.scene", "version": 2,
          "templates": { "块": { "shape": { "type": "box", "size": [0.5, 0.5, 0.5] },
                                 "body": "dynamic", "material": "石" } },
          "materials": { "石": { "color": [0.6, 0.6, 0.62], "roughness": 0.8 } }
        }"#,
    )
    .unwrap();
    let split = write_scene(
        &dir,
        "split.json",
        serde_json::json!({
            "format": "quench3d.scene",
            "version": 2,
            "include": ["parts/blocks.json"],
            "ground": { "size": 20 },
            "objects": [
                { "name": "塔基", "shape": { "type": "box", "size": [2, 0.2, 2] },
                  "position": [0, 0.1, 0], "body": "static" },
                { "name": "堆", "template": "块",
                  "instances": [ { "position": [0, 0.45, 0] },
                                 { "position": [0, 0.95, 0] },
                                 { "position": [0, 1.45, 0], "rotation_deg": [0, 20, 0] } ] }
            ],
            "simulate": { "steps": 150, "dt": 0.0166667 }
        }),
    );

    // 单文件版：把片段里的模板展开成三个具体物体，字段逐项相同
    let single = write_scene(
        &dir,
        "single.json",
        serde_json::json!({
            "format": "quench3d.scene",
            "version": 2,
            "ground": { "size": 20 },
            "materials": { "石": { "color": [0.6, 0.6, 0.62], "roughness": 0.8 } },
            "objects": [
                { "name": "塔基", "shape": { "type": "box", "size": [2, 0.2, 2] },
                  "position": [0, 0.1, 0], "body": "static" },
                { "name": "堆#1", "shape": { "type": "box", "size": [0.5, 0.5, 0.5] },
                  "position": [0, 0.45, 0], "body": "dynamic", "material": "石" },
                { "name": "堆#2", "shape": { "type": "box", "size": [0.5, 0.5, 0.5] },
                  "position": [0, 0.95, 0], "body": "dynamic", "material": "石" },
                { "name": "堆#3", "shape": { "type": "box", "size": [0.5, 0.5, 0.5] },
                  "position": [0, 1.45, 0], "rotation_deg": [0, 20, 0],
                  "body": "dynamic", "material": "石" }
            ],
            "simulate": { "steps": 150, "dt": 0.0166667 }
        }),
    );

    let a = run(&["simulate", split.to_str().unwrap(), "--json"]);
    assert_eq!(a.code, 0, "多文件版要能跑：{}", a.stdout);
    let b = run(&["simulate", single.to_str().unwrap(), "--json"]);
    assert_eq!(b.code, 0, "单文件版要能跑：{}", b.stdout);

    let sa = &a.json()["data"];
    let sb = &b.json()["data"];
    assert_eq!(sa["bodies"], sb["bodies"], "刚体数必须相同");
    assert_eq!(sa["steps"], sb["steps"]);
    assert_eq!(sa["lowest_y"], sb["lowest_y"], "最低点必须逐位相同");
    assert_eq!(sa["max_speed"], sb["max_speed"]);
    // 逐物体逐坐标：名字与位置都要一样
    for (x, y) in sa["states"]
        .as_array()
        .unwrap()
        .iter()
        .zip(sb["states"].as_array().unwrap().iter())
    {
        assert_eq!(x["name"], y["name"], "物体顺序/名字必须一致");
        assert_eq!(
            x["position"], y["position"],
            "{} 的位置必须逐位相同",
            x["name"]
        );
        assert_eq!(
            x["rotation"], y["rotation"],
            "{} 的朝向必须逐位相同",
            x["name"]
        );
    }

    // 多文件版也应当能被 `render --dry-run` 接受（不进 GPU 也能验证装配链路）
    let dry = run(&["render", split.to_str().unwrap(), "--dry-run", "--json"]);
    assert_eq!(dry.code, 0, "{}", dry.stdout);
}

/// 片段的相对贴图路径以**片段自己的目录**为基准（不是场景目录，也不是工作目录）。
#[test]
fn include_rebases_fragment_relative_asset_paths() {
    let dir = temp_dir("include_assets");
    std::fs::create_dir_all(dir.join("parts/tex")).unwrap();
    // 贴图**只**放在片段自己的目录里：如果实现按"场景目录"解析，这里会直接找不到
    // （"两个位置都有时谁赢"由 scene_doc 的单元测试钉住：那里断言重定基后的**路径字符串**
    //  正好是 `parts/tex/brick.png`，比"能不能找到"精确得多）。
    write_test_png(
        &dir.join("parts/tex/brick.png"),
        2,
        2,
        &[0, 255, 0, 255].repeat(4),
    );
    std::fs::write(
        dir.join("parts/wall.json"),
        r#"{ "format": "quench3d.scene", "version": 2,
             "materials": { "砖": { "color": [1,1,1], "base_color_texture": "tex/brick.png" } },
             "objects": [ { "name": "墙", "shape": { "type": "box", "size": [1,1,1] },
                            "position": [0, 0.5, 0], "material": "砖" } ] }"#,
    )
    .unwrap();
    let scene = write_scene(
        &dir,
        "main.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "include": ["parts/wall.json"],
            "ground": { "size": 10 }
        }),
    );
    let ok = run(&["validate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(
        ok.code, 0,
        "片段里的 tex/brick.png 应当按片段目录找到：{}",
        ok.stdout
    );

    // 反证：把片段挪到没有 tex/ 的子目录里，同一个相对路径必须**找不到**
    // （如果实现是"相对场景目录"，这条会误判为合法 —— 那正是要防的偏差）
    std::fs::create_dir_all(dir.join("moved")).unwrap();
    std::fs::write(
        dir.join("moved/wall.json"),
        std::fs::read_to_string(dir.join("parts/wall.json")).unwrap(),
    )
    .unwrap();
    let moved = write_scene(
        &dir,
        "moved_main.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "include": ["moved/wall.json"],
            "ground": { "size": 10 }
        }),
    );
    let bad = run(&["validate", moved.to_str().unwrap(), "--json"]);
    assert_eq!(bad.code, 2, "片段目录里没有 tex/ 就该报错：{}", bad.stdout);
    assert!(
        bad.json()["data"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "asset_not_found"),
        "应当是 asset_not_found：{}",
        bad.stdout
    );
}

/// `--asset-path` 与 `assets.search_paths` 对 include 同样生效（"一次运行"的补充路径）。
#[test]
fn include_honours_search_paths_and_asset_path_flag() {
    let dir = temp_dir("include_search");
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(
        lib.join("common.json"),
        r#"{ "format": "quench3d.scene", "version": 2,
             "objects": [ { "name": "公共", "shape": { "type": "box", "size": [1,1,1] },
                            "position": [0, 0.5, 0], "body": "static" } ] }"#,
    )
    .unwrap();
    let scene = write_scene(
        &dir,
        "main.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "include": ["common.json"],
            "ground": { "size": 10 }
        }),
    );
    // 不指路时找不到
    let missing = run(&["validate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(missing.code, 2, "{}", missing.stdout);
    assert!(
        missing.json()["data"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "asset_not_found" && d["path"] == "include[0]"),
        "{}",
        missing.stdout
    );
    // `--asset-path` 之后找得到
    let ok = run(&[
        "validate",
        scene.to_str().unwrap(),
        "--asset-path",
        lib.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(
        ok.code, 0,
        "--asset-path 应当能被 include 用上：{}",
        ok.stdout
    );
    // 场景文件自己声明 search_paths 也找得到
    let declared = write_scene(
        &dir,
        "declared.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "assets": { "search_paths": ["lib"] },
            "include": ["common.json"],
            "ground": { "size": 10 }
        }),
    );
    let ok2 = run(&["validate", declared.to_str().unwrap(), "--json"]);
    assert_eq!(
        ok2.code, 0,
        "assets.search_paths 应当能被 include 用上：{}",
        ok2.stdout
    );
    // `simulate` 也真的把片段里的物体建出来（不只是校验通过）
    let sim = run(&[
        "simulate",
        scene.to_str().unwrap(),
        "--asset-path",
        lib.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(sim.code, 0, "{}", sim.stdout);
    let states = sim.json()["data"]["states"].as_array().unwrap().clone();
    assert!(
        states.iter().any(|s| s["name"] == "公共"),
        "片段里的物体必须真的进世界：{states:?}"
    );
}

/// 跨文件 prefab：片段只定义 `templates`，根场景用 `instances` 摆多份。
#[test]
fn include_gives_cross_file_prefabs() {
    let dir = temp_dir("include_prefab");
    std::fs::create_dir_all(dir.join("prefabs")).unwrap();
    std::fs::write(
        dir.join("prefabs/crate.json"),
        r#"{ "format": "quench3d.scene", "version": 2,
             "templates": { "木箱": { "shape": { "type": "box", "size": [0.4, 0.4, 0.4] },
                                      "body": "dynamic", "material": "木", "mass": 5 } },
             "materials": { "木": { "color": [0.7, 0.45, 0.2], "roughness": 0.7 } } }"#,
    )
    .unwrap();
    let scene = write_scene(
        &dir,
        "main.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "include": ["prefabs/crate.json"],
            "ground": { "size": 12 },
            "objects": [
                { "template": "木箱", "instances": [
                    { "name": "箱A", "position": [0, 0.2, 0] },
                    { "name": "箱B", "position": [0.5, 0.2, 0] },
                    { "name": "箱C", "position": [-0.5, 0.2, 0] }
                ] }
            ],
            "simulate": { "steps": 60 }
        }),
    );
    let r = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stdout);
    let states = r.json()["data"]["states"].as_array().unwrap().clone();
    for name in ["箱A", "箱B", "箱C"] {
        assert!(
            states.iter().any(|s| s["name"] == name),
            "跨文件模板的实例 {name} 必须进世界：{:?}",
            states.iter().map(|s| s["name"].clone()).collect::<Vec<_>>()
        );
    }
    assert_eq!(r.json()["data"]["dynamic"], 3, "三个副本 = 三个动态刚体");
}

/// 片段层的问题：缺失、成环、重名、根独有的段 —— 每条都有稳定码与精确路径。
#[test]
fn include_failures_have_stable_codes_and_precise_paths() {
    let dir = temp_dir("include_fail");
    std::fs::create_dir_all(dir.join("parts")).unwrap();

    // ① 缺失
    let miss = write_scene(
        &dir,
        "miss.json",
        serde_json::json!({ "include": ["parts/nope.json"] }),
    );
    let r = run(&["validate", miss.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 2);
    let diags = r.json()["data"]["diagnostics"].as_array().unwrap().clone();
    assert!(
        diags
            .iter()
            .any(|d| d["code"] == "asset_not_found" && d["path"] == "include[0]"),
        "{diags:?}"
    );

    // ② 成环（a → b → a）
    std::fs::write(
        dir.join("parts/a.json"),
        r#"{ "format": "quench3d.scene", "version": 2, "include": ["b.json"] }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("parts/b.json"),
        r#"{ "format": "quench3d.scene", "version": 2, "include": ["a.json"] }"#,
    )
    .unwrap();
    let cyc = write_scene(
        &dir,
        "cycle.json",
        serde_json::json!({ "include": ["parts/a.json"] }),
    );
    let r = run(&["validate", cyc.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 2);
    let diags = r.json()["data"]["diagnostics"].as_array().unwrap().clone();
    assert!(
        diags.iter().any(|d| d["code"] == "include_cycle"),
        "成环应当是 include_cycle：{diags:?}"
    );

    // ③ 材质重名（静默覆盖是最坏的形态：引用会挂到错的东西上）
    std::fs::write(
        dir.join("parts/dup.json"),
        r#"{ "materials": { "木": { "color": [0, 1, 0] } } }"#,
    )
    .unwrap();
    let dup = write_scene(
        &dir,
        "dup.json",
        serde_json::json!({
            "materials": { "木": { "color": [1, 0, 0] } },
            "include": ["parts/dup.json"]
        }),
    );
    let r = run(&["validate", dup.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 2);
    let diags = r.json()["data"]["diagnostics"].as_array().unwrap().clone();
    assert!(
        diags
            .iter()
            .any(|d| d["code"] == "duplicate_name" && d["path"] == "include[0].materials.木"),
        "重名要报在片段那一份上：{diags:?}"
    );

    // ④ 片段写了根独有的段（光照只有一个实例，谁赢没有答案）
    std::fs::write(
        dir.join("parts/lit.json"),
        r#"{ "lighting": { "preset": "day" } }"#,
    )
    .unwrap();
    let lit = write_scene(
        &dir,
        "lit.json",
        serde_json::json!({ "include": ["parts/lit.json"] }),
    );
    let r = run(&["validate", lit.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 2);
    let diags = r.json()["data"]["diagnostics"].as_array().unwrap().clone();
    assert!(
        diags
            .iter()
            .any(|d| d["code"] == "invalid_value" && d["path"] == "include[0].lighting"),
        "{diags:?}"
    );

    // ⑤ 片段里的内容错误要带上是哪一份片段（`include[0]:objects[...]`）
    std::fs::write(
        dir.join("parts/bad.json"),
        r#"{ "objects": [ { "name": "坏", "shape": { "type": "pyramid" } } ] }"#,
    )
    .unwrap();
    let bad = write_scene(
        &dir,
        "bad.json",
        serde_json::json!({ "include": ["parts/bad.json"] }),
    );
    let r = run(&["validate", bad.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 2);
    let diags = r.json()["data"]["diagnostics"].as_array().unwrap().clone();
    assert!(
        diags.iter().any(|d| d["path"]
            .as_str()
            .unwrap_or("")
            .starts_with("include[0]:objects[0]")),
        "内容错误要挂在引用点上：{diags:?}"
    );

    // ⑥ 菱形包含：同一个文件被包含两次只并入一次，并且**说出来**（不是静默跳过）
    std::fs::write(
        dir.join("parts/common.json"),
        r#"{ "objects": [ { "name": "公共", "shape": { "type": "box", "size": [1,1,1] },
                            "position": [0, 0.5, 0], "body": "static" } ] }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("parts/left.json"),
        r#"{ "include": ["common.json"] }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("parts/right.json"),
        r#"{ "include": ["common.json"] }"#,
    )
    .unwrap();
    let diamond = write_scene(
        &dir,
        "diamond.json",
        serde_json::json!({ "include": ["parts/left.json", "parts/right.json"] }),
    );
    let r = run(&["validate", diamond.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "菱形包含是合法的：{}", r.stdout);
    assert_eq!(
        r.json()["data"]["warnings"],
        1,
        "应当有一条警告：{}",
        r.stdout
    );
    let diags = r.json()["data"]["diagnostics"].as_array().unwrap().clone();
    assert!(
        diags.iter().any(|d| d["code"] == "ineffective"),
        "跳过第二次要说出来：{diags:?}"
    );
    // 只该有一个"公共"物体（并入两次就会 duplicate_name）
    let sim = run(&["simulate", diamond.to_str().unwrap(), "--json"]);
    assert_eq!(sim.code, 0, "{}", sim.stdout);
    let states = sim.json()["data"]["states"].as_array().unwrap().clone();
    assert_eq!(
        states.iter().filter(|s| s["name"] == "公共").count(),
        1,
        "{states:?}"
    );
}

/// 不写 `include` 的场景：报错/报告/退出码一切照旧（新字段不该改变老行为）。
#[test]
fn scenes_without_include_are_unaffected() {
    let dir = temp_dir("include_absent");
    let scene = write_scene(
        &dir,
        "plain.json",
        serde_json::json!({
            "ground": { "size": 10 },
            "objects": [ { "name": "箱", "shape": { "type": "box", "size": [1,1,1] },
                           "position": [0, 0.5, 0], "body": "dynamic" } ],
            "simulate": { "steps": 30 }
        }),
    );
    let r = run(&["validate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.json()["data"]["valid"], true);
    assert_eq!(r.json()["data"]["warnings"], 0);
}

/// 圆锥（第 61 轮补的第 7 种形状）：端到端真的落在地上、报告里形状名对得上、
/// 坏参数给出稳定码与精确路径。
#[test]
fn cone_shape_simulates_validates_and_reports() {
    let dir = temp_dir("cone");
    let scene = write_scene(
        &dir,
        "cone.json",
        serde_json::json!({
            "format": "quench3d.scene",
            "version": 2,
            "ground": { "size": 20 },
            "objects": [
                // 底面朝下（默认朝向）：底面半径 0.4、半高 0.5 ⇒ 静止时原点在 y=0.5
                { "name": "桩", "shape": { "type": "cone", "radius": 0.4, "half_height": 0.5 },
                  "position": [0, 1.2, 0], "body": "dynamic", "friction": 0.8 },
                // 顶点朝下 + 一点倾斜：应该翻倒，但不穿地
                { "name": "陀螺", "shape": { "type": "cone", "radius": 0.3, "half_height": 0.4 },
                  "position": [2.0, 1.0, 0], "rotation_deg": [0, 0, 189], "body": "dynamic" },
                // 静态锥：只用来钉住"静态形状也能建"
                { "name": "锥标", "shape": { "type": "cone", "radius": 0.25, "half_height": 0.6 },
                  "position": [-2.0, 0.6, 0], "body": "static", "material": "橙" }
            ],
            "materials": { "橙": { "color": [0.95, 0.45, 0.15], "roughness": 0.6 } },
            "simulate": { "steps": 300, "dt": 0.0166667 }
        }),
    );

    let v = run(&["validate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(v.code, 0, "圆锥场景必须合法：{}", v.stdout);
    assert_eq!(v.json()["data"]["valid"], true);

    let s = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(s.code, 0, "{}", s.stdout);
    let data = s.json()["data"].clone();
    assert_eq!(data["nan_bodies"], 0, "圆锥仿真不允许 NaN");
    let states = data["states"].as_array().unwrap();
    let find = |name: &str| {
        states
            .iter()
            .find(|st| st["name"] == name)
            .unwrap_or_else(|| panic!("报告里缺 {name}：{states:?}"))
            .clone()
    };
    // 立着的那根：原点停在 half_height 附近（底面贴地）
    let post = find("桩");
    let y = post["position"][1].as_f64().unwrap();
    assert!(
        (y - 0.5).abs() < 0.08,
        "底面朝下的圆锥应停在 y≈0.5，实际 {y}（{post}）"
    );
    // 翻倒的那个：姿态明显偏开（四元数 x/z 分量大），且没穿地
    let top = find("陀螺");
    let ty = top["position"][1].as_f64().unwrap();
    assert!(ty > 0.0, "翻倒的圆锥不该穿地：{top}");
    let rot = top["rotation"].as_array().unwrap();
    let tilt = (rot[0].as_f64().unwrap().powi(2) + rot[2].as_f64().unwrap().powi(2)).sqrt();
    assert!(tilt > 0.3, "顶点朝下的圆锥应该翻倒，实际姿态 {rot:?}");
    // 静态的那个：一动不动
    let marker = find("锥标");
    assert!((marker["position"][1].as_f64().unwrap() - 0.6).abs() < 1e-6);

    // 倾斜报告里形状名 = "cone"（`--trace` 的每步姿态统计）
    let t = run(&[
        "simulate",
        scene.to_str().unwrap(),
        "--trace",
        "桩",
        "--json",
    ]);
    assert_eq!(t.code, 0, "{}", t.stdout);
    let data = t.json()["data"].clone();
    let traj = &data["trajectories"][0];
    assert_eq!(traj["name"], "桩");
    assert_eq!(
        traj["shape"], "cone",
        "轨迹报告里的形状名应为 cone：{}",
        traj["shape"]
    );

    // 坏参数：半径 0 → out_of_range，路径指到 objects[0].shape.radius
    let bad = write_scene(
        &dir,
        "bad_cone.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "objects": [ { "name": "坏", "shape": { "type": "cone", "radius": 0.0, "half_height": 0.5 },
                          "body": "dynamic" } ],
            "simulate": { "steps": 1 }
        }),
    );
    let b = run(&["validate", bad.to_str().unwrap(), "--json"]);
    assert_eq!(
        b.code, 2,
        "坏圆锥必须校验失败（输入问题 = 退出码 2）：{}",
        b.stdout
    );
    let env = b.json();
    // 契约：场景**能解析**但语义不合法 → `data.diagnostics`（解析失败才走 `error.diagnostics`）
    let diags = env["data"]["diagnostics"].as_array().unwrap();
    let hit = diags
        .iter()
        .find(|d| d["path"] == "objects[0].shape.radius")
        .unwrap_or_else(|| panic!("缺 objects[0].shape.radius 的诊断：{diags:?}"));
    assert_eq!(hit["code"], "out_of_range");
    // 半高为负 → 同一个码，另一条路径
    let bad2 = write_scene(
        &dir,
        "bad_cone2.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "objects": [ { "name": "坏", "shape": { "type": "cone", "half_height": -1.0 },
                          "body": "dynamic" } ],
            "simulate": { "steps": 1 }
        }),
    );
    let b2 = run(&["validate", bad2.to_str().unwrap(), "--json"]);
    assert_eq!(b2.code, 2);
    let diags2 = b2.json()["data"]["diagnostics"].as_array().unwrap().clone();
    assert!(
        diags2
            .iter()
            .any(|d| d["path"] == "objects[0].shape.half_height" && d["code"] == "out_of_range"),
        "缺 half_height 的诊断：{diags2:?}"
    );
}

/// 高度场（gap #18 第二件）：地形真的是可站立的静态三角网，射线打得到，
/// 轨迹报告里形状名对得上，坏数据给稳定码 —— 全程不需要显卡。
#[test]
fn heightfield_terrain_simulates_and_queries() {
    let dir = temp_dir("heightfield");
    // 3×3 格点、格子 2 米 ⇒ 地形 X/Z 各跨 4 米（以物体原点为中心）；高度恒为 1.5（平地，
    // 结果确定：球停在 1.5 + 半径）。坡面/台阶的网格几何由 render 的单元测试钉住。
    let scene = write_scene(
        &dir,
        "terrain.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "objects": [
                // 高度是**相对物体原点**的：物体抬到 y=1.5、高度全 0 ⇒ 地面在 1.5
                { "name": "地形", "body": "static", "material": "岩", "position": [0.0, 1.5, 0.0],
                  "shape": { "type": "heightfield", "cell_size": 2.0,
                             "heights": [[0, 0, 0], [0, 0, 0], [0, 0, 0]] } },
                { "name": "左球", "body": "dynamic", "position": [-1.0, 3.0, 0.0],
                  "shape": { "type": "sphere", "radius": 0.3 } },
                { "name": "右球", "body": "dynamic", "position": [1.0, 4.0, 0.5],
                  "shape": { "type": "sphere", "radius": 0.3 } }
            ],
            "materials": { "岩": { "color": [0.5, 0.45, 0.4], "roughness": 0.9 } },
            "simulate": { "steps": 300, "dt": 0.0166667 }
        }),
    );

    // ① 校验通过（heightfield + static 是合法组合）
    let v = run(&["validate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(v.code, 0, "高度场场景必须合法：{}", v.stdout);
    assert_eq!(v.json()["data"]["errors"], 0);

    // ② 仿真：两个球都停在 y ≈ 1.8（地形 1.5 + 半径 0.3），没有穿地、没有 NaN
    let s = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(s.code, 0, "{}", s.stdout);
    let data = s.json()["data"].clone();
    assert_eq!(data["nan_bodies"], 0);
    let states = data["states"].as_array().unwrap();
    let find = |name: &str| {
        states
            .iter()
            .find(|st| st["name"] == name)
            .unwrap_or_else(|| panic!("报告里缺 {name}"))
            .clone()
    };
    assert_eq!(find("地形")["kind"], "static");
    for name in ["左球", "右球"] {
        let st = find(name);
        let y = st["position"][1].as_f64().unwrap();
        assert!(
            (y - 1.8).abs() < 0.05,
            "{name} 应停在地形上（y≈1.8），实际 {y}（{st}）"
        );
    }
    // `lowest_y` 数的是**刚体原点**（地形原点在 1.5），所以它同时证明了两件事：
    // 物体 position 真的抬起了整块地形、而且没有球穿到地形下面
    let lowest = data["lowest_y"].as_f64().unwrap();
    assert!(
        lowest >= 1.4,
        "地形原点该在 1.5，且没有球穿下去：lowest_y = {lowest}"
    );

    // ③ 轨迹报告：从 3 米掉到 1.8 米（fall ≈ 1.2），形状名对得上
    let t = run(&[
        "simulate",
        scene.to_str().unwrap(),
        "--trace",
        "左球",
        "--json",
    ]);
    assert_eq!(t.code, 0, "{}", t.stdout);
    let left = t.json()["data"]["trajectories"][0].clone();
    assert_eq!(left["name"], "左球");
    assert_eq!(left["shape"], "sphere");
    let fall = left["fall"].as_f64().unwrap();
    assert!(fall > 1.0, "左球该真的掉下来：fall = {fall}");

    // 静态地形也能被 trace：形状名是 heightfield（`shape_kind_of` 的映射表）
    let tt = run(&[
        "simulate",
        scene.to_str().unwrap(),
        "--trace",
        "地形",
        "--json",
    ]);
    assert_eq!(tt.code, 0, "{}", tt.stdout);
    let terrain = tt.json()["data"]["trajectories"][0].clone();
    assert_eq!(terrain["shape"], "heightfield", "{terrain}");

    // ④ 射线：从 (0, 5, 0) 垂直向下，命中地形；x=0 处高度 1.5 ⇒ 距离 3.5、法线朝上
    let q = run(&[
        "query",
        scene.to_str().unwrap(),
        "--ray",
        "0,5,0,0,-1,0",
        "--json",
    ]);
    assert_eq!(q.code, 0, "{}", q.stdout);
    let hit = q.json()["data"]["hits"][0].clone();
    assert_eq!(hit["name"], "地形");
    let dist = hit["distance"].as_f64().unwrap();
    assert!((dist - 3.5).abs() < 1e-3, "射线距离应为 3.5，实际 {dist}");
    assert!(
        hit["normal"][1].as_f64().unwrap() > 0.99,
        "平地法线必须朝上：{hit}"
    );

    // ⑤ 高度场不能当重叠查询形状：给一句人话（退出码 2 = 输入问题）
    let o = run(&[
        "query",
        scene.to_str().unwrap(),
        "--overlap",
        "heightfield:1,2",
        "--json",
    ]);
    assert_eq!(o.code, 2, "{}", o.stdout);
    let msg = o.json()["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(msg.contains("高度场"), "错误信息要说清为什么：{msg}");

    // ⑥ 静止不动的地形体不能是动态刚体：校验拦下来并指到 body 上
    let bad = write_scene(
        &dir,
        "bad_body.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "objects": [ { "name": "飘浮地形", "body": "dynamic",
                           "shape": { "type": "heightfield", "heights": [[0, 0], [0, 1]] } } ],
            "simulate": { "steps": 1 }
        }),
    );
    let b = run(&["validate", bad.to_str().unwrap(), "--json"]);
    assert_eq!(b.code, 2, "{}", b.stdout);
    let diags = b.json()["data"]["diagnostics"].as_array().unwrap().clone();
    assert!(
        diags
            .iter()
            .any(|d| d["path"] == "objects[0].body" && d["code"] == "invalid_value"),
        "缺 objects[0].body 的诊断：{diags:?}"
    );
}

/// 圆角盒（gap #18 第三件）：小圆角时行为必须和普通箱子一致（停在半高、能射线打、能查重叠），
/// 大圆角/坏数据给稳定码 —— 全程不需要显卡。
#[test]
fn rounded_box_behaves_like_a_box_and_validates_radius() {
    let dir = temp_dir("rounded_box");
    // 半径 0.05 的圆角盒与同尺寸普通箱子并排掉下来：两者都停在 y ≈ 0.5
    let scene = write_scene(
        &dir,
        "rb.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "objects": [
                { "name": "圆角", "body": "dynamic", "position": [-0.8, 2.0, 0.0],
                  "shape": { "type": "rounded_box", "size": [1.0, 1.0, 1.0], "radius": 0.05 } },
                { "name": "方盒", "body": "dynamic", "position": [0.8, 2.0, 0.0],
                  "shape": { "type": "box", "size": [1.0, 1.0, 1.0] } }
            ],
            "ground": { "size": 20 },
            "simulate": { "steps": 240, "dt": 0.0166667 }
        }),
    );
    let v = run(&["validate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(v.code, 0, "{}", v.stdout);

    let s = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(s.code, 0, "{}", s.stdout);
    let data = s.json()["data"].clone();
    assert_eq!(data["nan_bodies"], 0);
    let states = data["states"].as_array().unwrap().clone();
    for name in ["圆角", "方盒"] {
        let st = states.iter().find(|x| x["name"] == name).unwrap();
        let y = st["position"][1].as_f64().unwrap();
        assert!(
            (y - 0.5).abs() < 0.05,
            "{name} 该停在 y≈0.5，实际 {y}（{st}）"
        );
        // 没翻倒：up 向量仍朝上
        let rot = st["rotation"].as_array().unwrap();
        assert!(
            rot[0].as_f64().unwrap().abs() < 0.1,
            "{name} 不该翻倒：{st}"
        );
    }

    // 轨迹报告里形状名是 rounded_box
    let t = run(&[
        "simulate",
        scene.to_str().unwrap(),
        "--trace",
        "圆角",
        "--json",
    ]);
    assert_eq!(t.code, 0, "{}", t.stdout);
    assert_eq!(t.json()["data"]["trajectories"][0]["shape"], "rounded_box");

    // 重叠查询：圆角盒能当查询形状（有解析的"点在内"判据），而且能查到箱子
    // （`query` 用的是文档里的**初始**位置，不是仿真后的位置，所以这里用 y=2）
    let o = run(&[
        "query",
        scene.to_str().unwrap(),
        "--overlap",
        "rounded_box:1,1,1,0.1@-0.8,2,0",
        "--json",
    ]);
    assert_eq!(o.code, 0, "{}", o.stdout);
    let names: Vec<String> = o.json()["data"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        names.contains(&"圆角".to_string()),
        "重叠查询该命中：{names:?}"
    );

    // 半径超过最短半边长：校验拦下并指到 radius
    let bad = write_scene(
        &dir,
        "bad_radius.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "objects": [ { "name": "太圆", "body": "dynamic",
                           "shape": { "type": "rounded_box", "size": [1.0, 0.4, 1.0], "radius": 0.3 } } ],
            "simulate": { "steps": 1 }
        }),
    );
    let b = run(&["validate", bad.to_str().unwrap(), "--json"]);
    assert_eq!(b.code, 2, "{}", b.stdout);
    assert!(
        b.json()["data"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["path"] == "objects[0].shape.radius" && d["code"] == "out_of_range"),
        "{}",
        b.stdout
    );

    // `--overlap` 的 radius 超过半边长时给一句人话
    let o2 = run(&[
        "query",
        scene.to_str().unwrap(),
        "--overlap",
        "rounded_box:1,1,1,0.6",
        "--json",
    ]);
    assert_eq!(o2.code, 2, "{}", o2.stdout);
    let msg = o2.json()["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(msg.contains("胶囊") || msg.contains("球"), "{msg}");
}

/// 接触余量（`physics.speculative_margin`）的**专门 e2e**：它必须真的生效。
///
/// 判据不是"配置读到了"（那有单测），而是**物理结果变了**：同一个球掉到地上，
/// 开了 6 cm 余量的那个第一帧接触更早，所以整条轨迹里到达的最低点更高（吃得浅）。
/// 这条同时钉住第 61 轮修掉的串线 bug —— 那次 `speculative_margin` 压根没被读，
/// 两个场景的低点会**逐位相同**。
#[test]
fn speculative_margin_reduces_penetration_on_impact() {
    let dir = temp_dir("margin");
    let scene = |name: &str, physics: serde_json::Value| {
        write_scene(
            &dir,
            name,
            serde_json::json!({
                "format": "quench3d.scene", "version": 2,
                "objects": [ { "name": "球", "body": "dynamic", "position": [0.0, 3.0, 0.0],
                               "shape": { "type": "sphere", "radius": 0.5 } } ],
                "ground": { "size": 10 },
                "physics": physics,
                "simulate": { "steps": 90, "dt": 0.0166667 }
            }),
        )
    };
    let lowest = |path: &std::path::Path| -> f64 {
        let t = run(&[
            "simulate",
            path.to_str().unwrap(),
            "--trace",
            "球",
            "--json",
        ]);
        assert_eq!(t.code, 0, "{}", t.stdout);
        let traj = t.json()["data"]["trajectories"][0].clone();
        let samples = traj["samples"].as_array().unwrap().clone();
        assert!(samples.len() > 10, "轨迹样本太少：{traj}");
        samples
            .iter()
            .map(|s| s["position"][1].as_f64().unwrap())
            .fold(f64::INFINITY, f64::min)
    };
    let plain = scene("plain.json", serde_json::json!({}));
    let margin = scene(
        "margin.json",
        serde_json::json!({ "speculative_margin": 0.06 }),
    );
    let y_plain = lowest(&plain);
    let y_margin = lowest(&margin);
    // 球半径 0.5：没余量时会吃进地面一点（低点 < 0.5），有余量时提前接触（低点更高）
    assert!(
        y_plain < 0.5,
        "没余量的场景该出现一点穿透（低点 < 0.5），实际 {y_plain}"
    );
    assert!(
        y_margin > y_plain + 0.01,
        "6 cm 余量必须让低点更高（吃得更浅）：{y_margin} vs {y_plain} —— 相等说明余量没被读到"
    );
    assert!(y_margin > 0.48, "余量下的低点不该深于 2 cm：{y_margin}");

    // 余量过大 → 校验报 out_of_range（给退出码 2 与稳定码）
    let bad = write_scene(
        &dir,
        "bad_margin.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "objects": [ { "name": "球", "shape": { "type": "sphere" } } ],
            "physics": { "speculative_margin": 2.0 },
            "simulate": { "steps": 1 }
        }),
    );
    let v = run(&["validate", bad.to_str().unwrap(), "--json"]);
    assert_eq!(v.code, 2, "{}", v.stdout);
    assert!(
        v.json()["data"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["path"] == "physics.speculative_margin" && d["code"] == "out_of_range"),
        "{}",
        v.stdout
    );
}

/// 手搓一个带 1 关节蒙皮 + 1 秒平移动画的 GLB，用于验 `info` 的蒙皮字段。
fn skinned_glb_bytes() -> Vec<u8> {
    fn glb_from_parts(json: &str, mut bin: Vec<u8>) -> Vec<u8> {
        let mut json_bytes = json.replace('\n', "").into_bytes();
        while !json_bytes.len().is_multiple_of(4) {
            json_bytes.push(b' ');
        }
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
        let mut glb = Vec::with_capacity(total);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
        glb.extend_from_slice(&json_bytes);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x004E_4942u32.to_le_bytes());
        glb.extend_from_slice(&bin);
        glb
    }
    let mut bin = Vec::new();
    for f in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        bin.extend_from_slice(&f.to_le_bytes()); // 0..36 POSITION
    }
    for _ in 0..3 {
        bin.extend_from_slice(&[0, 0, 0, 0]); // 36..48 JOINTS_0
    }
    for _ in 0..3 {
        for f in [1.0f32, 0.0, 0.0, 0.0] {
            bin.extend_from_slice(&f.to_le_bytes()); // 48..96 WEIGHTS_0
        }
    }
    for f in [
        1.0f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ] {
        bin.extend_from_slice(&f.to_le_bytes()); // 96..160 单位逆绑定矩阵
    }
    for f in [0.0f32, 1.0] {
        bin.extend_from_slice(&f.to_le_bytes()); // 160..168 时间
    }
    for f in [0.0f32, 0.0, 0.0, 0.0, 2.0, 0.0] {
        bin.extend_from_slice(&f.to_le_bytes()); // 168..192 平移
    }
    let json = r#"{"asset":{"version":"2.0"},"scene":0,
"scenes":[{"nodes":[1]}],
"nodes":[{"name":"joint"},{"name":"skin","mesh":0,"skin":0}],
"meshes":[{"name":"tri","primitives":[{"attributes":{"POSITION":0,"JOINTS_0":1,"WEIGHTS_0":2}}]}],
"skins":[{"joints":[0],"inverseBindMatrices":3}],
"accessors":[
 {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0.0,0.0,0.0],"max":[1.0,1.0,0.0]},
 {"bufferView":1,"componentType":5121,"count":3,"type":"VEC4"},
 {"bufferView":2,"componentType":5126,"count":3,"type":"VEC4"},
 {"bufferView":3,"componentType":5126,"count":1,"type":"MAT4"},
 {"bufferView":4,"componentType":5126,"count":2,"type":"SCALAR","min":[0.0],"max":[1.0]},
 {"bufferView":5,"componentType":5126,"count":2,"type":"VEC3"}],
"bufferViews":[
 {"buffer":0,"byteOffset":0,"byteLength":36,"target":34962},
 {"buffer":0,"byteOffset":36,"byteLength":12,"target":34962},
 {"buffer":0,"byteOffset":48,"byteLength":48,"target":34962},
 {"buffer":0,"byteOffset":96,"byteLength":64},
 {"buffer":0,"byteOffset":160,"byteLength":8},
 {"buffer":0,"byteOffset":168,"byteLength":24}],
"buffers":[{"byteLength":192}],
"animations":[{"name":"lift","samplers":[{"input":4,"output":5,"interpolation":"LINEAR"}],
 "channels":[{"sampler":0,"target":{"node":0,"path":"translation"}}]}]}"#;
    glb_from_parts(json, bin)
}

/// 手搓一个带 1 个 morph target 的 GLB：三个顶点整体 +y 0.5（静态形变）。
fn morph_glb_bytes() -> Vec<u8> {
    fn glb_from_parts(json: &str, mut bin: Vec<u8>) -> Vec<u8> {
        let mut json_bytes = json.replace('\n', "").into_bytes();
        while !json_bytes.len().is_multiple_of(4) {
            json_bytes.push(b' ');
        }
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
        let mut glb = Vec::with_capacity(total);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
        glb.extend_from_slice(&json_bytes);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x004E_4942u32.to_le_bytes());
        glb.extend_from_slice(&bin);
        glb
    }
    let mut bin = Vec::new();
    for f in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        bin.extend_from_slice(&f.to_le_bytes()); // 0..36 POSITION
    }
    for _ in 0..3 {
        for f in [0.0f32, 0.0, 1.0] {
            bin.extend_from_slice(&f.to_le_bytes()); // 36..72 NORMAL
        }
    }
    for i in [0u16, 1, 2] {
        bin.extend_from_slice(&i.to_le_bytes()); // 72..78 INDICES
    }
    bin.extend_from_slice(&[0, 0]); // 对齐到 80
    for _ in 0..3 {
        for f in [0.0f32, 0.5, 0.0] {
            bin.extend_from_slice(&f.to_le_bytes()); // 80..116 MORPH POSITION
        }
    }
    for f in [0.0f32, 1.0] {
        bin.extend_from_slice(&f.to_le_bytes()); // 116..124 动画时间
    }
    for f in [0.0f32, 1.0] {
        bin.extend_from_slice(&f.to_le_bytes()); // 124..132 morph 权重
    }
    let json = r#"{"asset":{"version":"2.0"},"scene":0,
"scenes":[{"nodes":[0]}],
"nodes":[{"name":"tri","mesh":0}],
"meshes":[{"name":"morph","weights":[0.0],"primitives":[{"attributes":{"POSITION":0,"NORMAL":1},"indices":2,"targets":[{"POSITION":3}]}]}],
"accessors":[
 {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0.0,0.0,0.0],"max":[1.0,1.0,0.0]},
 {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"},
 {"bufferView":2,"componentType":5123,"count":3,"type":"SCALAR"},
 {"bufferView":3,"componentType":5126,"count":3,"type":"VEC3"},
 {"bufferView":4,"componentType":5126,"count":2,"type":"SCALAR","min":[0.0],"max":[1.0]},
 {"bufferView":5,"componentType":5126,"count":2,"type":"SCALAR"}],
"bufferViews":[
 {"buffer":0,"byteOffset":0,"byteLength":36,"target":34962},
 {"buffer":0,"byteOffset":36,"byteLength":36,"target":34962},
 {"buffer":0,"byteOffset":72,"byteLength":6,"target":34963},
 {"buffer":0,"byteOffset":80,"byteLength":36,"target":34962},
 {"buffer":0,"byteOffset":116,"byteLength":8},
 {"buffer":0,"byteOffset":124,"byteLength":8}],
"buffers":[{"byteLength":132}],
"animations":[{"name":"pulse","samplers":[{"input":4,"output":5,"interpolation":"LINEAR"}],
 "channels":[{"sampler":0,"target":{"node":0,"path":"weights"}}]}]}"#;
    glb_from_parts(json, bin)
}

#[test]
fn info_reports_morph_targets() {
    let dir = temp_dir("info_morph");
    std::fs::write(dir.join("morph.glb"), morph_glb_bytes()).expect("写 morph 夹具");
    let path = dir.join("morph.glb");
    let r = run(&["info", path.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let d = r.json();
    assert_eq!(d["data"]["morph_targets"], 1, "{d}");
    let human = run(&["info", path.to_str().unwrap()]);
    assert_eq!(human.code, 0, "{}", human.stderr);
    assert!(
        human.stdout.contains("morph target"),
        "人类可读输出要有形变行：{}",
        human.stdout
    );
}

/// morph 权重必须真的改画面：同一个静态模型，权重 0 与 1 的 PNG 不同。
#[test]
fn scene_morph_weights_change_cli_render() {
    let dir = temp_dir("morph_render");
    std::fs::write(dir.join("morph.glb"), morph_glb_bytes()).expect("写 morph 夹具");
    let render_with = |weight: f32| {
        let scene = write_scene(
            &dir,
            &format!("morph-{weight}.json"),
            serde_json::json!({
                "format": "quench3d.scene", "version": 2,
                "models": [{ "path": "morph.glb", "morph_weights": [weight] }],
                "camera": { "position": [0.5, 0.75, 4.0], "look_at": [0.5, 0.75, 0.0], "fov_deg": 45.0 },
                "render": { "output": "ignored.png", "width": 160, "height": 120, "msaa": 1 }
            }),
        );
        let png = dir.join(format!("morph-{weight}.png"));
        let r = run(&[
            "render",
            scene.to_str().unwrap(),
            "--out",
            png.to_str().unwrap(),
        ]);
        (r, png)
    };
    let (r0, png0) = render_with(0.0);
    if r0.code == 3 {
        return; // 无 GPU
    }
    assert_eq!(r0.code, 0, "stderr={} stdout={}", r0.stderr, r0.stdout);
    let (r1, png1) = render_with(1.0);
    assert_eq!(r1.code, 0, "stderr={} stdout={}", r1.stderr, r1.stdout);
    let (w, h, a) = decode_rgba(&png0);
    let (w2, h2, b) = decode_rgba(&png1);
    assert_eq!((w, h), (w2, h2));
    let changed = a
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0)
        .filter(|(x, y)| x != y)
        .count();
    assert!(
        changed > 10,
        "morph 权重 0 与 1 应当改变画面，实际 {changed} 像素"
    );
}

/// morph 权重动画必须真的改变画面：同一条 `pulse`，start=0 与 start=0.5 的 PNG 不同。
#[test]
fn scene_morph_animation_changes_cli_render() {
    let dir = temp_dir("morph_anim_render");
    std::fs::write(dir.join("morph.glb"), morph_glb_bytes()).expect("写 morph 夹具");
    // 报告：morph 轨道被发现
    let scene = write_scene(
        &dir,
        "pulse-report.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "models": [{ "path": "morph.glb", "animation": { "name": "pulse", "start": 0.5 } }],
            "render": { "output": "ignored.png", "width": 160, "height": 120, "msaa": 1 }
        }),
    );
    let r = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let d = r.json();
    assert_eq!(d["data"]["models"][0]["animation"]["found"], true, "{d}");
    assert_eq!(
        d["data"]["models"][0]["animation"]["morph_tracks"], 1,
        "{d}"
    );

    let render_at = |start: f32| {
        let scene = write_scene(
            &dir,
            &format!("pulse-{start}.json"),
            serde_json::json!({
                "format": "quench3d.scene", "version": 2,
                "models": [{ "path": "morph.glb", "animation": { "name": "pulse", "start": start } }],
                "camera": { "position": [0.5, 0.75, 4.0], "look_at": [0.5, 0.75, 0.0], "fov_deg": 45.0 },
                "render": { "output": "ignored.png", "width": 160, "height": 120, "msaa": 1 }
            }),
        );
        let png = dir.join(format!("pulse-{start}.png"));
        let r = run(&[
            "render",
            scene.to_str().unwrap(),
            "--out",
            png.to_str().unwrap(),
        ]);
        (r, png)
    };
    let (r0, png0) = render_at(0.0);
    if r0.code == 3 {
        return; // 无 GPU
    }
    assert_eq!(r0.code, 0, "stderr={} stdout={}", r0.stderr, r0.stdout);
    let (r1, png1) = render_at(0.5);
    assert_eq!(r1.code, 0, "stderr={} stdout={}", r1.stderr, r1.stdout);
    let (w, h, a) = decode_rgba(&png0);
    let (w2, h2, b) = decode_rgba(&png1);
    assert_eq!((w, h), (w2, h2));
    let changed = a
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0)
        .filter(|(x, y)| x != y)
        .count();
    assert!(
        changed > 10,
        "morph 动画 start=0 与 0.5 应当改变画面，实际 {changed} 像素"
    );
}

/// 手工 LOD：场景格式 → runner 真的装载替换层并注册 LOD 组；
/// 距离切换本身由引擎侧 `lods_switch_visibility_by_camera_distance_with_hysteresis` 钉住。
#[test]
fn scene_model_lods_load_replacements_without_breaking_simulate() {
    let dir = temp_dir("model_lods");
    std::fs::write(dir.join("base.glb"), morph_glb_bytes()).expect("写基础模型");
    std::fs::write(dir.join("mid.glb"), morph_glb_bytes()).expect("写中模");
    std::fs::write(dir.join("low.glb"), morph_glb_bytes()).expect("写粗模");
    let scene = write_scene(
        &dir,
        "lods.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "models": [{
                "path": "base.glb",
                "lods": [
                    { "path": "mid.glb", "min_distance": 10.0 },
                    { "path": "low.glb", "min_distance": 25.0 }
                ]
            }],
            "camera": { "position": [0.5, 0.75, 4.0], "look_at": [0.5, 0.75, 0.0] },
            "simulate": { "steps": 0 },
            "render": { "output": "ignored.png", "width": 160, "height": 120, "msaa": 1 }
        }),
    );
    let r = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    let d = r.json();
    assert_eq!(d["ok"], true, "LOD 场景应当成功装载：{d}");
}

/// 状态机：报告字段 + 立即过渡（after=0, fade=0）必须改变首帧画面。
#[test]
fn scene_animator_is_reported_and_changes_cli_render() {
    let dir = temp_dir("animator");
    std::fs::write(dir.join("morph.glb"), morph_glb_bytes()).expect("写 morph 夹具");
    let animator_scene = write_scene(
        &dir,
        "animator.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "models": [{
                "path": "morph.glb",
                "animator": {
                    "initial": "a",
                    "states": [
                        { "name": "a", "animation": "pulse", "start": 0.0 },
                        { "name": "b", "animation": "pulse", "start": 0.5 }
                    ],
                    "transitions": [
                        { "from": "a", "to": "b", "after": 0.0, "fade": 0.0 }
                    ]
                }
            }],
            "camera": { "position": [0.5, 0.75, 4.0], "look_at": [0.5, 0.75, 0.0], "fov_deg": 45.0 },
            "render": { "output": "ignored.png", "width": 160, "height": 120, "msaa": 1 }
        }),
    );
    let r = run(&["simulate", animator_scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    let d = r.json();
    let m = &d["data"]["models"][0];
    assert_eq!(m["animator"]["found"], true, "{d}");
    assert_eq!(m["animator"]["states"], 2, "{d}");
    assert_eq!(m["animator"]["transitions"], 1, "{d}");
    assert_eq!(m["animator"]["initial"], "a", "{d}");

    let baseline = write_scene(
        &dir,
        "animator-baseline.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "models": [{ "path": "morph.glb", "animation": { "name": "pulse", "start": 0.0 } }],
            "camera": { "position": [0.5, 0.75, 4.0], "look_at": [0.5, 0.75, 0.0], "fov_deg": 45.0 },
            "render": { "output": "ignored.png", "width": 160, "height": 120, "msaa": 1 }
        }),
    );
    let anim_png = dir.join("animator.png");
    let r = run(&[
        "render",
        animator_scene.to_str().unwrap(),
        "--out",
        anim_png.to_str().unwrap(),
    ]);
    if r.code == 3 {
        return; // 无 GPU
    }
    assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    let base_png = dir.join("animator-baseline.png");
    let r = run(&[
        "render",
        baseline.to_str().unwrap(),
        "--out",
        base_png.to_str().unwrap(),
    ]);
    assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    let (w, h, a) = decode_rgba(&anim_png);
    let (w2, h2, b) = decode_rgba(&base_png);
    assert_eq!((w, h), (w2, h2));
    let changed = a
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0)
        .filter(|(x, y)| x != y)
        .count();
    assert!(
        changed > 10,
        "状态机立即过渡应改变首帧画面，实际 {changed} 像素"
    );
}

/// `on_finished` 的**视觉**证据：初始状态已经播完时，首帧必须切到目标 clip，
/// 与直接播放目标 `start` 的基线逐像素一致（不切就会停在旧 clip 末帧）。
#[test]
fn scene_animator_finished_trigger_changes_first_frame() {
    let dir = temp_dir("animator_finished_pixels");
    std::fs::write(dir.join("morph.glb"), morph_glb_bytes()).expect("写 morph 夹具");
    let camera = serde_json::json!({
        "position": [0.5, 0.75, 4.0], "look_at": [0.5, 0.75, 0.0], "fov_deg": 45.0
    });
    let render_block = serde_json::json!({
        "output": "ignored.png", "width": 160, "height": 120, "msaa": 1
    });
    let triggered = write_scene(
        &dir,
        "finished-pixels.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "models": [{
                "path": "morph.glb",
                "animator": {
                    "initial": "once",
                    "states": [
                        // start 超过 clip 时长 → play_model_animator 的 advance(0)
                        // 已经把它夹到末尾并置 finished。
                        { "name": "once", "animation": "pulse", "start": 2.0, "loop": false },
                        { "name": "after", "animation": "pulse", "start": 0.5, "loop": false }
                    ],
                    "transitions": [
                        { "from": "once", "to": "after", "on_finished": true, "fade": 0.0 }
                    ]
                }
            }],
            "camera": camera,
            "render": render_block
        }),
    );
    let baseline = write_scene(
        &dir,
        "finished-baseline.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "models": [{ "path": "morph.glb", "animation": { "name": "pulse", "start": 0.5 } }],
            "camera": camera,
            "render": render_block
        }),
    );
    let triggered_png = dir.join("finished-trigger.png");
    let r = run(&[
        "render",
        triggered.to_str().unwrap(),
        "--out",
        triggered_png.to_str().unwrap(),
    ]);
    if r.code == 3 {
        return;
    }
    assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    let baseline_png = dir.join("finished-baseline.png");
    let r = run(&[
        "render",
        baseline.to_str().unwrap(),
        "--out",
        baseline_png.to_str().unwrap(),
    ]);
    assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    let (w, h, a) = decode_rgba(&triggered_png);
    let (w2, h2, b) = decode_rgba(&baseline_png);
    assert_eq!((w, h), (w2, h2));
    let changed = a
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0)
        .filter(|(x, y)| x != y)
        .count();
    assert_eq!(
        changed, 0,
        "首帧应切到 after(start=.5)，与基线逐像素一致；实际 {changed} 像素不同"
    );
}

/// `on_finished` 可以省略 `after`：校验通过，报告仍是同一个状态机。
#[test]
fn scene_animator_accepts_finished_trigger_without_after() {
    let dir = temp_dir("animator_finished");
    std::fs::write(dir.join("morph.glb"), morph_glb_bytes()).expect("写 morph 夹具");
    let scene = write_scene(
        &dir,
        "finished.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "models": [{
                "path": "morph.glb",
                "animator": {
                    "initial": "once",
                    "states": [
                        { "name": "once", "animation": "pulse", "loop": false },
                        { "name": "after", "animation": "pulse", "start": 0.5, "loop": false }
                    ],
                    "transitions": [
                        { "from": "once", "to": "after", "on_finished": true, "fade": 0.0 }
                    ]
                }
            }]
        }),
    );
    let r = run(&["validate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    let d = r.json();
    assert_eq!(d["data"]["valid"], true, "{d}");

    let r = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    let d = r.json();
    let a = &d["data"]["models"][0]["animator"];
    assert_eq!(a["found"], true, "{d}");
    assert_eq!(a["states"], 2, "{d}");
    assert_eq!(a["transitions"], 1, "{d}");
}

#[test]
fn scene_animator_is_validated() {
    let dir = temp_dir("animator_validate");
    std::fs::write(dir.join("morph.glb"), morph_glb_bytes()).expect("写 morph 夹具");
    let scene = write_scene(
        &dir,
        "bad-animator.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "models": [{
                "path": "morph.glb",
                "animation": { "name": "pulse" },
                "animator": {
                    "initial": "missing",
                    "states": [{ "name": "a", "animation": "pulse" }],
                    "transitions": [
                        { "from": "a", "to": "nope", "after": -1.0, "fade": -0.5 }
                    ]
                }
            }]
        }),
    );
    let r = run(&["validate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 2, "stderr={} stdout={}", r.stderr, r.stdout);
    let d = r.json();
    let diags = d["data"]["diagnostics"].as_array().unwrap();
    for path in [
        "models[0].animator",
        "models[0].animator.initial",
        "models[0].animator.transitions[0].to",
        "models[0].animator.transitions[0].after",
        "models[0].animator.transitions[0].fade",
    ] {
        assert!(
            diags.iter().any(|x| x["path"] == path),
            "缺少 {path} 诊断：{d}"
        );
    }
}

#[test]
fn scene_morph_weights_are_validated() {
    let dir = temp_dir("morph_validate");
    std::fs::write(dir.join("morph.glb"), morph_glb_bytes()).expect("写 morph 夹具");
    let scene = write_scene(
        &dir,
        "bad-morph.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "models": [{ "path": "morph.glb", "morph_weights": [1.5, -0.25] }]
        }),
    );
    let r = run(&["validate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 2, "stderr={} stdout={}", r.stderr, r.stdout);
    let d = r.json();
    let diags = d["data"]["diagnostics"].as_array().unwrap();
    assert!(
        diags
            .iter()
            .any(|x| x["path"] == "models[0].morph_weights[0]" && x["code"] == "out_of_range"),
        "{d}"
    );
    assert!(
        diags
            .iter()
            .any(|x| x["path"] == "models[0].morph_weights[1]" && x["code"] == "out_of_range"),
        "{d}"
    );
}

#[test]
fn info_reports_skins_and_skinned_meshes() {
    let dir = temp_dir("info_skin");
    std::fs::write(dir.join("skin.glb"), skinned_glb_bytes()).expect("写蒙皮夹具");
    let path = dir.join("skin.glb");
    let r = run(&["info", path.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let d = r.json();
    assert_eq!(d["data"]["skins"], 1, "{d}");
    assert_eq!(d["data"]["skinned_meshes"], 1, "{d}");
    let human = run(&["info", path.to_str().unwrap()]);
    assert_eq!(human.code, 0, "{}", human.stderr);
    assert!(
        human.stdout.contains("蒙皮"),
        "人类可读输出要有蒙皮行：{}",
        human.stdout
    );
}

/// CLI 端的蒙皮正向验证：同一条 `lift`，start=0 与 start=0.5 的 PNG 必须不同。
///
/// 只测 `skinned_mesh` 不够——要证明场景里的 `models[].animation` 真的经过
/// `render_offscreen_stats` 的 setup、`advance_animations_with_renderer` 写回 GPU、
/// 再被 `--out` 写成用户能看的图。没有 GPU 时按既有口径退出码 3，跳过断言。
#[test]
fn skinned_model_animation_changes_cli_render() {
    let dir = temp_dir("skin_render");
    std::fs::write(dir.join("skin.glb"), skinned_glb_bytes()).expect("写蒙皮夹具");
    let render_at = |start: f32| {
        let scene = write_scene(
            &dir,
            &format!("scene-{start}.json"),
            serde_json::json!({
                "format": "quench3d.scene", "version": 2,
                "models": [{ "path": "skin.glb", "animation": { "name": "lift", "start": start } }],
                "camera": { "position": [0.5, 1.2, 4.0], "look_at": [0.5, 1.2, 0.0], "fov_deg": 45.0 },
                "render": { "output": "ignored.png", "width": 160, "height": 120, "msaa": 1 }
            }),
        );
        let out = dir.join(format!("skin-{start}.png"));
        let r = run(&[
            "render",
            scene.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ]);
        (r, out)
    };
    let (a, a_png) = render_at(0.0);
    if a.code == 3 {
        return; // 无 GPU
    }
    assert_eq!(a.code, 0, "stderr={} stdout={}", a.stderr, a.stdout);
    let (b, b_png) = render_at(0.5);
    assert_eq!(b.code, 0, "stderr={} stdout={}", b.stderr, b.stdout);
    let (w, h, pa) = decode_rgba(&a_png);
    let (w2, h2, pb) = decode_rgba(&b_png);
    assert_eq!((w, h), (w2, h2));
    let changed = pa
        .as_chunks::<4>()
        .0
        .iter()
        .zip(pb.as_chunks::<4>().0)
        .filter(|(x, y)| x != y)
        .count();
    assert!(
        changed > 10,
        "start=0 与 start=0.5 应当有可见差异，实际 {changed} 像素"
    );
}

/// `animation.blend`：报告字段 + 场景渲染必须真的混出第三个姿态。
///
/// 同一个 `lift` 用两次（start 0 与 0.5），weight=0.5：t=0 时主 clip 在 0、
/// 第二 clip 已在半程，混合后应落在两者中间。只验报告字段不够，必须看到 PNG 变化。
#[test]
fn model_animation_blend_is_reported_and_rendered() {
    let dir = temp_dir("model_blend");
    std::fs::write(dir.join("skin.glb"), skinned_glb_bytes()).expect("写蒙皮夹具");
    let scene = write_scene(
        &dir,
        "blend.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "models": [{
                "path": "skin.glb",
                "animation": {
                    "name": "lift",
                    "blend": { "name": "lift", "start": 0.5, "weight": 0.5 }
                }
            }],
            "camera": { "position": [0.5, 1.2, 4.0], "look_at": [0.5, 1.2, 0.0], "fov_deg": 45.0 },
            "render": { "output": "ignored.png", "width": 160, "height": 120, "msaa": 1 }
        }),
    );

    // 1) 报告：两条 clip 都要 found，weight 原样回显
    let r = run(&["simulate", scene.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    let d = r.json();
    let m = &d["data"]["models"][0];
    assert_eq!(m["animation"]["found"], true, "{d}");
    assert_eq!(m["blend"]["found"], true, "{d}");
    assert_eq!(m["blend"]["weight"], 0.5, "{d}");
    assert_eq!(m["blend"]["start"], 0.5, "{d}");
    assert!(m["blend"]["duration"].is_number(), "{d}");

    // 1b) fade 原样回显（0.25 秒淡入）
    let fade = write_scene(
        &dir,
        "blend-fade.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "models": [{ "path": "skin.glb", "animation": { "name": "lift",
                "blend": { "name": "lift", "weight": 0.75, "fade": 0.25 } } }]
        }),
    );
    let r = run(&["simulate", fade.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let d = r.json();
    let b = &d["data"]["models"][0]["blend"];
    assert_eq!(b["weight"], 0.75, "{d}");
    assert_eq!(b["fade"], 0.25, "{d}");

    // 2) 第二节 clip 名字写错：validate 仍然通过，报告与警告必须点名
    let bad = write_scene(
        &dir,
        "blend-bad.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "models": [{ "path": "skin.glb", "animation": { "name": "lift",
                "blend": { "name": "没有这条", "weight": 0.25 } } }]
        }),
    );
    let r = run(&["simulate", bad.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "名字不存在不是结构错误：{}", r.stderr);
    let d = r.json();
    assert_eq!(d["data"]["models"][0]["blend"]["found"], false, "{d}");
    let warnings = d["data"]["warnings"].to_string();
    assert!(warnings.contains("没有这条"), "{warnings}");
    assert!(warnings.contains("绑定姿态"), "{warnings}");

    // 3) 渲染：混合姿态与纯主 clip 的 start=0 必须不同
    let baseline = write_scene(
        &dir,
        "single.json",
        serde_json::json!({
            "format": "quench3d.scene", "version": 2,
            "models": [{ "path": "skin.glb", "animation": { "name": "lift" } }],
            "camera": { "position": [0.5, 1.2, 4.0], "look_at": [0.5, 1.2, 0.0], "fov_deg": 45.0 },
            "render": { "output": "ignored.png", "width": 160, "height": 120, "msaa": 1 }
        }),
    );
    let blend_png = dir.join("blend.png");
    let r = run(&[
        "render",
        scene.to_str().unwrap(),
        "--out",
        blend_png.to_str().unwrap(),
    ]);
    if r.code == 3 {
        return; // 无 GPU
    }
    assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    let base_png = dir.join("single.png");
    let r = run(&[
        "render",
        baseline.to_str().unwrap(),
        "--out",
        base_png.to_str().unwrap(),
    ]);
    assert_eq!(r.code, 0, "stderr={} stdout={}", r.stderr, r.stdout);
    let (w, h, a) = decode_rgba(&blend_png);
    let (w2, h2, b) = decode_rgba(&base_png);
    assert_eq!((w, h), (w2, h2));
    let changed = a
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0)
        .filter(|(x, y)| x != y)
        .count();
    assert!(
        changed > 10,
        "混合与纯主 clip 应有可见差异，实际 {changed} 像素"
    );
}

/// 手搓一个带 1 秒平移轨道的 GLB（单个三角形 + 单个节点）。
///
/// 与 `crates/quench3d/tests/model_loading.rs` 的夹具同源：这里只要"能装载、有一条
/// 叫「摆动」的动画"，用来验**格式字段 → 报告**这条链；几何怎么动由引擎侧的
/// `scene_animation_moves_the_spawned_meshes` 钉住（那一条不需要显卡）。
fn animated_glb_bytes() -> Vec<u8> {
    fn glb_from_parts(json: &str, mut bin: Vec<u8>) -> Vec<u8> {
        let mut json_bytes = json.replace('\n', "").into_bytes();
        while !json_bytes.len().is_multiple_of(4) {
            json_bytes.push(b' ');
        }
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
        let mut glb = Vec::with_capacity(total);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes());
        glb.extend_from_slice(&json_bytes);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(&0x004E_4942u32.to_le_bytes());
        glb.extend_from_slice(&bin);
        glb
    }

    let mut bin = Vec::new();
    for f in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        bin.extend_from_slice(&f.to_le_bytes()); // 36 字节位置
    }
    for i in [0u16, 1, 2] {
        bin.extend_from_slice(&i.to_le_bytes()); // 36..42 索引
    }
    while bin.len() < 44 {
        bin.push(0); // 对齐到 4 字节（时间关键帧从 44 开始）
    }
    for f in [0.0f32, 1.0] {
        bin.extend_from_slice(&f.to_le_bytes()); // 44..52 时间
    }
    for v in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0]] {
        for f in v {
            bin.extend_from_slice(&f.to_le_bytes()); // 52..76 平移
        }
    }
    let json = r#"{"asset":{"version":"2.0"},"scene":0,
"scenes":[{"nodes":[0]}],
"nodes":[{"mesh":0,"name":"块"}],
"meshes":[{"name":"三角","primitives":[{"attributes":{"POSITION":0},"indices":1}]}],
"accessors":[
  {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0.0,0.0,0.0],"max":[1.0,1.0,0.0]},
  {"bufferView":1,"componentType":5123,"count":3,"type":"SCALAR"},
  {"bufferView":2,"componentType":5126,"count":2,"type":"SCALAR","min":[0.0],"max":[1.0]},
  {"bufferView":3,"componentType":5126,"count":2,"type":"VEC3"}
],
"bufferViews":[
  {"buffer":0,"byteOffset":0,"byteLength":36,"target":34962},
  {"buffer":0,"byteOffset":36,"byteLength":6,"target":34963},
  {"buffer":0,"byteOffset":44,"byteLength":8},
  {"buffer":0,"byteOffset":52,"byteLength":24}
],
"animations":[{"name":"摆动",
  "channels":[{"sampler":0,"target":{"node":0,"path":"translation"}}],
  "samplers":[{"input":2,"output":3,"interpolation":"LINEAR"}]}],
"buffers":[{"byteLength":76}]}"#;
    glb_from_parts(json, bin)
}

/// 根运动：报告节点是否存在；loop=true/带 blend 时校验器必须拒绝。
///
/// 几何是否真的前移由引擎侧 `root_motion_moves_the_object_but_not_the_deformed_mesh_twice`
/// 钉住（不需要显卡）；这里钉"字段 → 报告/校验"这条链。
#[test]
fn model_root_motion_is_reported_and_validated() {
    let dir = temp_dir("root_motion");
    std::fs::write(dir.join("anim.glb"), animated_glb_bytes()).expect("写夹具 GLB");
    let scene = |name: &str, animation: serde_json::Value| {
        write_scene(
            &dir,
            name,
            serde_json::json!({
                "format": "quench3d.scene", "version": 2, "name": "根运动",
                "models": [{ "path": "anim.glb", "animation": animation }],
                "simulate": { "steps": 2, "dt": 0.01666667 }
            }),
        )
    };

    let p = scene(
        "ok.json",
        serde_json::json!({ "name": "摆动", "loop": false, "root_motion": "块" }),
    );
    let r = run(&["simulate", p.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let d = r.json();
    let rm = &d["data"]["models"][0]["animation"]["root_motion"];
    assert_eq!(rm["node"], "块", "{d}");
    assert_eq!(rm["found"], true, "{d}");

    // 节点名错：场景仍合法，但报告要说明不会提取。
    let p = scene(
        "missing.json",
        serde_json::json!({ "name": "摆动", "loop": false, "root_motion": "没有这个节点" }),
    );
    let r = run(&["simulate", p.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let d = r.json();
    assert_eq!(
        d["data"]["models"][0]["animation"]["root_motion"]["found"], false,
        "{d}"
    );

    // 循环片段 + 根运动：位移会跳回，直接判非法。
    let p = scene(
        "looping.json",
        serde_json::json!({ "name": "摆动", "root_motion": "块" }),
    );
    let r = run(&["validate", p.to_str().unwrap(), "--json"]);
    assert_ne!(r.code, 0, "循环根运动必须校验失败");
    assert!(
        r.stdout.contains("root_motion") || r.stderr.contains("root_motion"),
        "诊断要指向 root_motion：{} {}",
        r.stdout,
        r.stderr
    );

    // 与 blend 同用也非法。
    let p = scene(
        "blend.json",
        serde_json::json!({
            "name": "摆动", "loop": false, "root_motion": "块",
            "blend": { "name": "摆动", "weight": 0.5 }
        }),
    );
    let r = run(&["validate", p.to_str().unwrap(), "--json"]);
    assert_ne!(r.code, 0, "blend + root_motion 必须校验失败");
}

/// gap #23：`models[].animation` 是场景格式字段，而"动画名在不在资产里"**只能装载后判定**
///（`validate` 不读资产）。这条钉住三个契约：
///
/// 1. 名字对 → `simulate` 报告的 `models[].animation` 给出 `found/duration/tracks`；
/// 2. 名字错 → **不是**校验错误（退出码 0），报告 `found=false` + 警告里列出可用名字；
/// 3. 不写 `animation` → 报告里**没有** `models` 字段（老报告逐字节不变）。
#[test]
fn model_animation_status_is_reported_after_loading() {
    let dir = temp_dir("model_animation");
    std::fs::write(dir.join("anim.glb"), animated_glb_bytes()).expect("写夹具 GLB");
    let scene = |name: &str, model: serde_json::Value| {
        write_scene(
            &dir,
            name,
            serde_json::json!({
                "format": "quench3d.scene", "version": 2, "name": "动画",
                "models": [model],
                "simulate": { "steps": 2, "dt": 0.01666667 }
            }),
        )
    };

    // 1) 名字对：报告给出装载后才有的时长/轨道数，场景里写的播放参数原样回显
    let p = scene(
        "ok.json",
        serde_json::json!({
            "path": "anim.glb",
            "animation": { "name": "摆动", "speed": 2.0, "loop": false, "start": 0.25 }
        }),
    );
    let r = run(&["simulate", p.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let d = r.json();
    let m = &d["data"]["models"][0];
    assert_eq!(m["model"], 0, "{d}");
    assert_eq!(m["animation"]["name"], "摆动", "{d}");
    assert_eq!(m["animation"]["found"], true, "{d}");
    assert!(
        (m["animation"]["duration"].as_f64().unwrap() - 1.0).abs() < 1e-6,
        "时长该是 1 秒：{d}"
    );
    assert_eq!(m["animation"]["tracks"], 1, "{d}");
    assert_eq!(m["animation"]["speed"], 2.0, "{d}");
    assert_eq!(m["animation"]["looping"], false, "{d}");
    assert_eq!(m["animation"]["start"], 0.25, "{d}");
    assert_eq!(m["clips"][0], "摆动", "报告要列出资产里有哪些动画：{d}");

    // 2) 名字错：场景仍然合法（validate 不读资产），但报告必须说清楚不会播
    let p = scene(
        "wrong_name.json",
        serde_json::json!({
            "path": "anim.glb",
            "animation": { "name": "没有这条" }
        }),
    );
    let r = run(&["simulate", p.to_str().unwrap(), "--json"]);
    assert_eq!(
        r.code, 0,
        "动画名写错不是校验错误（validate 读不到资产）：{}",
        r.stderr
    );
    let d = r.json();
    let m = &d["data"]["models"][0];
    assert_eq!(m["animation"]["found"], false, "{d}");
    assert!(
        m["animation"].get("duration").is_none(),
        "found=false 时不该给时长：{d}"
    );
    assert!(
        m["clips"].as_array().is_some_and(|a| a.len() == 1),
        "即使名字错了也要列出可用动画名：{d}"
    );
    let warnings = d["data"]["warnings"].to_string();
    assert!(
        warnings.contains("没有这条") && warnings.contains("摆动"),
        "警告要点名错的名字与可用名字：{warnings}"
    );

    // 3) 不写 animation：老行为，报告里没有 models 字段（静态模型不白解析）
    let p = scene("static.json", serde_json::json!({ "path": "anim.glb" }));
    let r = run(&["simulate", p.to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let d = r.json();
    assert!(
        d["data"].get("models").is_none(),
        "没写 animation 就不该有动画报告：{d}"
    );

    // 4) `info` 也要能列出可用动画名（agent 发现名字的入口）
    let r = run(&["info", dir.join("anim.glb").to_str().unwrap(), "--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let d = r.json();
    assert_eq!(d["data"]["animations"][0], "摆动", "{d}");
}

/// `models[].animation` 的**结构**校验（能在不读资产时判断的部分）。
#[test]
fn model_animation_structure_is_validated() {
    let dir = temp_dir("model_animation_validate");
    std::fs::write(dir.join("anim.glb"), animated_glb_bytes()).expect("写夹具 GLB");
    let scene = |name: &str, animation: serde_json::Value| {
        write_scene(
            &dir,
            name,
            serde_json::json!({
                "format": "quench3d.scene", "version": 2,
                "models": [{ "path": "anim.glb", "animation": animation }]
            }),
        )
    };
    let diagnostics = |p: &std::path::Path| -> (i32, Vec<String>) {
        let r = run(&["validate", p.to_str().unwrap(), "--json"]);
        let d = r.json();
        let paths = d["data"]["diagnostics"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|x| x["severity"] == "error")
                    .filter_map(|x| x["path"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        (r.code, paths)
    };

    let p = scene("empty_name.json", serde_json::json!({ "name": "  " }));
    let (code, paths) = diagnostics(&p);
    assert_eq!(code, 2, "空动画名要拦下");
    assert!(
        paths.contains(&"models[0].animation.name".to_string()),
        "诊断路径要对得上：{paths:?}"
    );

    let p = scene(
        "bad_speed.json",
        serde_json::json!({ "name": "摆动", "speed": 0 }),
    );
    let (code, paths) = diagnostics(&p);
    assert_eq!(code, 2, "倍速 0 要拦下");
    assert!(paths.contains(&"models[0].animation.speed".to_string()));

    let p = scene(
        "bad_start.json",
        serde_json::json!({ "name": "摆动", "start": -1.0 }),
    );
    let (code, paths) = diagnostics(&p);
    assert_eq!(code, 2, "负起始时间要拦下");
    assert!(paths.contains(&"models[0].animation.start".to_string()));

    // 第二条 clip 的结构校验（路径必须落在 animation.blend.*）
    let p = scene(
        "blend_empty.json",
        serde_json::json!({ "name": "摆动", "blend": { "name": " " } }),
    );
    let (code, paths) = diagnostics(&p);
    assert_eq!(code, 2, "空的第二条动画名要拦下");
    assert!(
        paths.contains(&"models[0].animation.blend.name".to_string()),
        "{paths:?}"
    );

    let p = scene(
        "blend_weight.json",
        serde_json::json!({ "name": "摆动", "blend": { "name": "摆动", "weight": 1.5 } }),
    );
    let (code, paths) = diagnostics(&p);
    assert_eq!(code, 2, "越界权重要拦下");
    assert!(
        paths.contains(&"models[0].animation.blend.weight".to_string()),
        "{paths:?}"
    );

    let p = scene(
        "blend_speed.json",
        serde_json::json!({ "name": "摆动", "blend": { "name": "摆动", "speed": 0 } }),
    );
    let (code, paths) = diagnostics(&p);
    assert_eq!(code, 2, "第二条 clip 的倍速 0 要拦下");
    assert!(
        paths.contains(&"models[0].animation.blend.speed".to_string()),
        "{paths:?}"
    );

    let p = scene(
        "blend_fade.json",
        serde_json::json!({ "name": "摆动", "blend": { "name": "摆动", "fade": -0.5 } }),
    );
    let (code, paths) = diagnostics(&p);
    assert_eq!(code, 2, "负的淡入时长要拦下");
    assert!(
        paths.contains(&"models[0].animation.blend.fade".to_string()),
        "{paths:?}"
    );

    // 名字不存在**不是**结构错误：这条必须留给装载后（否则 validate 要读资产）
    let p = scene(
        "unknown_name.json",
        serde_json::json!({ "name": "资产里没有" }),
    );
    let (code, paths) = diagnostics(&p);
    assert_eq!(code, 0, "名字不存在不该让场景非法：{paths:?}");
    assert!(paths.is_empty(), "{paths:?}");
}

#[test]
fn export_writes_obj_and_mtl_that_info_can_read() {
    let dir = temp_dir("export_obj");
    let scene = dir.join("scene.json");
    std::fs::write(
        &scene,
        r#"{
          "format":"quench3d.scene","version":2,"name":"export",
          "objects":[
            {"name":"box","shape":{"type":"box","size":[2,1,3]},"position":[0,1,0]},
            {"name":"ball","shape":{"type":"sphere","radius":0.5},"position":[3,1,0],
             "material":"red"}
          ],
          "materials":{"red":{"color":[1,0,0]}}
        }"#,
    )
    .expect("写临时场景");
    let obj = dir.join("model.obj");
    let r = run(&[
        "export",
        scene.to_str().unwrap(),
        obj.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(r.code, 0, "stderr={}\nstdout={}", r.stderr, r.stdout);
    let env = r.json();
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["objects"], 2);
    assert!(obj.exists(), "OBJ 没有落盘");
    let mtl = dir.join("model.mtl");
    assert!(mtl.exists(), "MTL 没有落盘");
    let text = std::fs::read_to_string(&obj).unwrap();
    assert!(text.contains("mtllib model.mtl"), "{text}");
    assert!(text.contains("usemtl red"), "{text}");
    assert_eq!(
        env["data"]["triangles"].as_u64().unwrap(),
        12 + 768,
        "盒子 12 + 球 768"
    );

    // 真的能被本引擎的导入器读回来：三角形数与包围盒对得上。
    let info = run(&["info", obj.to_str().unwrap(), "--json"]);
    assert_eq!(
        info.code, 0,
        "stderr={}\nstdout={}",
        info.stderr, info.stdout
    );
    let info = info.json();
    assert_eq!(
        info["data"]["triangles"].as_u64().unwrap(),
        env["data"]["triangles"].as_u64().unwrap()
    );
    assert_eq!(info["data"]["meshes"].as_u64().unwrap(), 2);
    let size = info["data"]["bounds_size"].as_array().unwrap();
    assert!((size[0].as_f64().unwrap() - 4.5).abs() < 1e-5, "{size:?}");
    assert!((size[1].as_f64().unwrap() - 1.0).abs() < 1e-5, "{size:?}");
    assert!((size[2].as_f64().unwrap() - 3.0).abs() < 1e-5, "{size:?}");
}

#[test]
fn export_writes_single_file_gltf_that_info_can_read() {
    let dir = temp_dir("export_gltf");
    let scene = dir.join("scene.json");
    std::fs::write(
        &scene,
        r#"{
          "format":"quench3d.scene","version":2,"name":"export",
          "objects":[
            {"name":"box","shape":{"type":"box","size":[2,1,3]},"position":[0,1,0]},
            {"name":"ball","shape":{"type":"sphere","radius":0.5},"position":[3,1,0],
             "material":"red"}
          ],
          "materials":{"red":{"color":[1,0,0]}}
        }"#,
    )
    .expect("写临时场景");
    let gltf = dir.join("model.gltf");
    let r = run(&[
        "export",
        scene.to_str().unwrap(),
        gltf.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(r.code, 0, "stderr={}\nstdout={}", r.stderr, r.stdout);
    let env = r.json();
    assert_eq!(env["ok"], true);
    assert_eq!(env["data"]["format"], "gltf");
    assert_eq!(env["data"]["objects"], 2);
    assert_eq!(env["data"]["triangles"].as_u64().unwrap(), 12 + 768);
    assert!(gltf.exists(), "glTF 没有落盘");

    let text = std::fs::read_to_string(&gltf).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).expect("glTF 必须是合法 JSON");
    assert_eq!(json["asset"]["version"], "2.0");
    assert_eq!(json["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(json["meshes"].as_array().unwrap().len(), 2);
    assert!(
        json["buffers"][0]["uri"]
            .as_str()
            .unwrap()
            .starts_with("data:application/octet-stream;base64,"),
        "几何应内嵌在 .gltf 里，不依赖外部 .bin"
    );

    // 本引擎的 glTF 导入器能读回来。
    let info = run(&["info", gltf.to_str().unwrap(), "--json"]);
    assert_eq!(
        info.code, 0,
        "stderr={}\nstdout={}",
        info.stderr, info.stdout
    );
    let info = info.json();
    assert_eq!(
        info["data"]["triangles"].as_u64().unwrap(),
        env["data"]["triangles"].as_u64().unwrap()
    );
    assert_eq!(info["data"]["meshes"].as_u64().unwrap(), 2);
    let size = info["data"]["bounds_size"].as_array().unwrap();
    assert!((size[0].as_f64().unwrap() - 4.5).abs() < 1e-5, "{size:?}");
}

/// glTF 导出要把相对场景目录的贴图真的内嵌进去，而不是只写个路径。
#[test]
fn export_gltf_embeds_texture_from_scene_directory() {
    let dir = temp_dir("export_gltf_tex");
    let png = dir.join("checker.png");
    // 1x1 PNG；这里只验证按扩展名/魔数嵌入，不验证像素内容。
    let bytes: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xb5,
        0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0xfc,
        0xff, 0x1f, 0x00, 0x02, 0xeb, 0x01, 0xf5, 0x8f, 0x59, 0x97, 0x5b, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    std::fs::write(&png, bytes).expect("写临时贴图");
    let scene = dir.join("scene.json");
    std::fs::write(
        &scene,
        r#"{
          "materials":{"tex":{"color":[1,1,1],"base_color_texture":"checker.png","alpha_cutoff":0.5}},
          "objects":[{"name":"box","shape":{"type":"box","size":[1,1,1]},"material":"tex"}]
        }"#,
    )
    .unwrap();
    let gltf = dir.join("textured.gltf");
    let r = run(&[
        "export",
        scene.to_str().unwrap(),
        gltf.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(r.code, 0, "stderr={}\nstdout={}", r.stderr, r.stdout);
    let env = r.json();
    assert_eq!(env["data"]["textures"], 1, "{}", env);
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&gltf).unwrap()).unwrap();
    assert_eq!(json["images"][0]["mimeType"], "image/png");
    assert_eq!(json["textures"][0]["source"], 0);
    assert_eq!(
        json["materials"][0]["pbrMetallicRoughness"]["baseColorTexture"]["index"],
        0
    );
    assert_eq!(json["materials"][0]["alphaMode"], "MASK");
    // 图片字节和顶点共用一个 data URI buffer。
    let uri = json["buffers"][0]["uri"].as_str().unwrap();
    assert!(
        uri.len() > bytes.len() * 4 / 3,
        "图片没有被 base64 进 buffer"
    );
}

/// GLB：同一份 glTF 场景写成二进制 chunk，`info` 能读回同样的几何。
#[test]
fn export_writes_glb_that_info_can_read() {
    let dir = temp_dir("export_glb");
    let scene = dir.join("scene.json");
    std::fs::write(
        &scene,
        r#"{
          "objects":[
            {"name":"box","shape":{"type":"box","size":[2,1,3]},"position":[0,1,0]},
            {"name":"ball","shape":{"type":"sphere","radius":0.5},"position":[3,1,0],"parent":"box"}
          ]
        }"#,
    )
    .unwrap();
    let glb = dir.join("model.glb");
    let r = run(&[
        "export",
        scene.to_str().unwrap(),
        glb.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(r.code, 0, "stderr={}\nstdout={}", r.stderr, r.stdout);
    let env = r.json();
    assert_eq!(env["data"]["format"], "glb");
    assert_eq!(env["data"]["objects"], 2);
    let bytes = std::fs::read(&glb).expect("GLB 应落盘");
    assert_eq!(&bytes[0..4], b"glTF");
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2);
    assert_eq!(
        u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize,
        bytes.len()
    );
    let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let json: serde_json::Value = serde_json::from_slice(&bytes[20..20 + json_len]).unwrap();
    assert!(json["buffers"][0].get("uri").is_none());
    let info = run(&["info", glb.to_str().unwrap(), "--json"]);
    assert_eq!(
        info.code, 0,
        "stderr={}\nstdout={}",
        info.stderr, info.stdout
    );
    let info = info.json();
    assert_eq!(
        info["data"]["triangles"].as_u64().unwrap(),
        env["data"]["triangles"].as_u64().unwrap()
    );
    assert_eq!(info["data"]["meshes"].as_u64().unwrap(), 2);
}

/// 写一张纯色 RGBA PNG（给粒子贴图这类需要真实文件的测试用）。
fn write_solid_png(path: &std::path::Path, rgba: [u8; 4]) {
    let file = std::fs::File::create(path).expect("创建 PNG");
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), 64, 64);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("写 PNG 头");
    let mut data = Vec::with_capacity(64 * 64 * 4);
    for _ in 0..(64 * 64) {
        data.extend_from_slice(&rgba);
    }
    writer.write_image_data(&data).expect("写 PNG 数据");
}

/// 公告板粒子：同一相机/光照下，有粒子的画面中心必须比没粒子更红。
#[test]
fn particle_billboards_change_pixels() {
    let doctor = run(&["doctor", "--json"]).json();
    if !doctor["data"]["gpu_available"].as_bool().unwrap() {
        eprintln!("跳过渲染断言：没有可用 GPU");
        return;
    }
    let dir = temp_dir("particles");
    let plain = dir.join("plain.json");
    let with = dir.join("with.json");
    let plain_png = dir.join("plain.png");
    let with_png = dir.join("with.png");
    let base = r#"{
        "camera": { "position": [0, 1.5, 6], "look_at": [0, 1.5, 0] },
        "lighting": { "sun_intensity": 0, "ambient": 0, "preset": "none" },
        "render": { "output": "ignored.png", "width": 320, "height": 240, "msaa": 1 }
    }"#;
    std::fs::write(&plain, base).unwrap();
    let particle = r#"{
        "count": 16,
        "position": [0, 1.5, 0],
        "radius": 0,
        "velocity": [0, 0, 0],
        "spread": 0,
        "lifetime": 10,
        "size": 2.0,
        "gravity": [0, 0, 0],
        "color": [1.0, 0.1, 0.05, 1.0]
    }"#;
    let mut doc: serde_json::Value = serde_json::from_str(base).unwrap();
    doc["particles"] = serde_json::from_str(&format!("[{particle}]")).unwrap();
    std::fs::write(&with, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

    for (scene, out) in [(&plain, &plain_png), (&with, &with_png)] {
        let r = run(&[
            "render",
            scene.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(r.code, 0, "渲染失败：{}", r.stderr);
    }
    let (w, h, a) = decode_rgba(&plain_png);
    let (w2, h2, b) = decode_rgba(&with_png);
    assert_eq!((w, h), (w2, h2));
    let (ar, ag, ab) = center_mean(w, h, &a);
    let (br, bg, bb) = center_mean(w, h, &b);
    assert!(
        br > ar + 40.0 && br > bg + 30.0 && br > bb + 30.0,
        "粒子必须让中心变红：无粒子 ({ar:.1},{ag:.1},{ab:.1}) vs 有粒子 ({br:.1},{bg:.1},{bb:.1})"
    );
}

/// 粒子贴图：同样的白色实例色，Green 贴图必须把中心从白变成绿。
#[test]
fn particle_texture_tints_the_billboard() {
    let doctor = run(&["doctor", "--json"]).json();
    if !doctor["data"]["gpu_available"].as_bool().unwrap() {
        eprintln!("跳过渲染断言：没有可用 GPU");
        return;
    }
    let dir = temp_dir("particle_texture");
    let texture = dir.join("green.png");
    write_solid_png(&texture, [0, 255, 0, 255]);
    let base = r#"{
        "camera": { "position": [0, 1.5, 6], "look_at": [0, 1.5, 0] },
        "lighting": { "sun_intensity": 0, "ambient": 0, "preset": "none" },
        "render": { "output": "ignored.png", "width": 320, "height": 240, "msaa": 1 }
    }"#;
    let particle = r#"{
        "count": 16,
        "position": [0, 1.5, 0],
        "radius": 0,
        "velocity": [0, 0, 0],
        "spread": 0,
        "lifetime": 10,
        "size": 2.0,
        "gravity": [0, 0, 0],
        "color": [1.0, 1.0, 1.0, 1.0]
    }"#;
    let plain = dir.join("plain.json");
    let textured = dir.join("textured.json");
    let plain_png = dir.join("plain.png");
    let textured_png = dir.join("textured.png");
    let mut doc: serde_json::Value = serde_json::from_str(base).unwrap();
    doc["particles"] = serde_json::from_str(&format!("[{particle}]")).unwrap();
    std::fs::write(&plain, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    let mut particle_doc: serde_json::Value = serde_json::from_str(particle).unwrap();
    particle_doc["texture"] = serde_json::Value::String(texture.display().to_string());
    doc["particles"] = serde_json::json!([particle_doc]);
    std::fs::write(&textured, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

    for (scene, out) in [(&plain, &plain_png), (&textured, &textured_png)] {
        let r = run(&[
            "render",
            scene.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(r.code, 0, "渲染失败：{}", r.stderr);
    }
    let (w, h, a) = decode_rgba(&plain_png);
    let (w2, h2, b) = decode_rgba(&textured_png);
    assert_eq!((w, h), (w2, h2));
    let (ar, ag, ab) = center_mean(w, h, &a);
    let (br, bg, bb) = center_mean(w2, h2, &b);
    assert!(
        ar > 180.0 && (ar - ag).abs() < 25.0 && (ar - ab).abs() < 25.0,
        "无贴图粒子应是白的：({ar:.1},{ag:.1},{ab:.1})"
    );
    assert!(
        bg > br + 60.0 && bg > bb + 60.0,
        "绿色贴图没有生效：无贴图 ({ar:.1},{ag:.1},{ab:.1}) vs 有贴图 ({br:.1},{bg:.1},{bb:.1})"
    );
}

/// 平面贴花：朝向相机的红色四边形必须让中心变红；没有贴花时中心仍是黑的。
#[test]
fn decal_quad_changes_pixels() {
    let doctor = run(&["doctor", "--json"]).json();
    if !doctor["data"]["gpu_available"].as_bool().unwrap() {
        eprintln!("跳过渲染断言：没有可用 GPU");
        return;
    }
    let dir = temp_dir("decal");
    let plain = dir.join("plain.json");
    let with = dir.join("with.json");
    let plain_png = dir.join("plain.png");
    let with_png = dir.join("with.png");
    let base = r#"{
        "camera": { "position": [0, 1.5, 6], "look_at": [0, 1.5, 0] },
        "lighting": { "sun_intensity": 0, "ambient": 0, "preset": "none" },
        "render": { "output": "ignored.png", "width": 320, "height": 240, "msaa": 1 }
    }"#;
    std::fs::write(&plain, base).unwrap();
    let mut doc: serde_json::Value = serde_json::from_str(base).unwrap();
    doc["decals"] = serde_json::json!([{
        "position": [0.0, 1.5, 0.0],
        "normal": [0.0, 0.0, 1.0],
        "size": [2.0, 2.0],
        "color": [1.0, 0.05, 0.02, 1.0]
    }]);
    std::fs::write(&with, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

    for (scene, out) in [(&plain, &plain_png), (&with, &with_png)] {
        let r = run(&[
            "render",
            scene.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(r.code, 0, "渲染失败：{}", r.stderr);
    }
    let (w, h, a) = decode_rgba(&plain_png);
    let (w2, h2, b) = decode_rgba(&with_png);
    assert_eq!((w, h), (w2, h2));
    let (ar, ag, ab) = center_mean(w, h, &a);
    let (br, bg, bb) = center_mean(w2, h2, &b);
    assert!(
        br > ar + 80.0 && br > bg + 60.0 && br > bb + 60.0,
        "贴花必须让中心变红：无贴花 ({ar:.1},{ag:.1},{ab:.1}) vs 有贴花 ({br:.1},{bg:.1},{bb:.1})"
    );
}
