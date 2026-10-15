//! 命令行 agent 的工具循环测试：用假模型驱动真实的引擎工具。
//!
//! 这里验证的是「模型说要调工具 → 我们真的执行 → 结果回喂 → 模型收尾」这条链路，
//! 包括工具失败、达到往返上限、以及请求体里确实带上了工具定义与 tool 回执。

mod common;

use std::time::Duration;

use common::MockLlm;
use quench3d_cli::agent::Agent;
use quench3d_cli::llm::LlmConfig;
use serde_json::{json, Value};

fn agent_for(server: &MockLlm) -> Agent {
    Agent::new(LlmConfig {
        base_url: server.base_url.clone(),
        model: "mock-model".to_string(),
        timeout_secs: 10,
        ..Default::default()
    })
}

/// 造一条带单个函数调用的 assistant 回复。
fn tool_call_response(id: &str, name: &str, args: Value) -> String {
    json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "model": "mock-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": id,
                    "type": "function",
                    "function": { "name": name, "arguments": args.to_string() }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "total_tokens": 42 }
    })
    .to_string()
}

/// 造一条普通文本回复。
fn text_response(text: &str) -> String {
    json!({
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "model": "mock-model",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop"
        }],
        "usage": { "total_tokens": 7 }
    })
    .to_string()
}

fn ball_scene() -> Value {
    json!({
        "ground": { "size": 20 },
        "objects": [{
            "name": "球",
            "shape": { "type": "sphere", "radius": 0.5 },
            "position": [0, 5, 0],
            "body": "dynamic"
        }]
    })
}

#[test]
fn agent_runs_simulate_tool_and_feeds_result_back() {
    let server = MockLlm::start_sequence(
        vec![
            tool_call_response(
                "call_1",
                "quench3d_simulate",
                json!({ "steps": 180, "scene": ball_scene() }),
            ),
            text_response("球最终停在地面上，中心高度约 0.5 米。"),
        ],
        true,
    );

    let mut agent = agent_for(&server);
    let turn = agent
        .run_turn("这个球最后停在多高？", 4)
        .expect("agent 回合应当成功");

    assert_eq!(turn.rounds, 1, "应该只往返一轮");
    assert_eq!(turn.trace.len(), 1);
    assert_eq!(turn.trace[0].name, "quench3d_simulate");
    assert!(turn.trace[0].ok, "工具应当成功: {:?}", turn.trace[0]);
    assert!(turn.text.contains("0.5"));
    assert_eq!(turn.usage, Some(7));
    // 工具的结构化结果应当被保留下来，供 UI 展示
    let structured = turn.structured.expect("应当带仿真结构化结果");
    assert_eq!(structured["nan_bodies"], 0);
    assert_eq!(structured["bodies"], 2);

    // 第一次请求：带工具定义、还没有 tool 回执
    let first = server
        .next_request(Duration::from_secs(5))
        .expect("第一次请求");
    let sent: Value = serde_json::from_str(&first).unwrap();
    let tools = sent["tools"].as_array().expect("必须带上 tools");
    assert!(tools.len() >= 7, "工具数量不对: {}", tools.len());
    assert_eq!(sent["tool_choice"], "auto");
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"quench3d_simulate"));
    assert_eq!(sent["messages"][0]["role"], "system");
    assert!(sent["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("quench3d.scene"));

    // 第二次请求：assistant 的 tool_calls + tool 的回执都要在上下文里
    let second = server
        .next_request(Duration::from_secs(5))
        .expect("第二次请求");
    let sent2: Value = serde_json::from_str(&second).unwrap();
    let roles: Vec<&str> = sent2["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(
        roles,
        vec!["system", "user", "assistant", "tool"],
        "{sent2:#?}"
    );
    assert!(
        sent2["messages"][2]["tool_calls"].is_array(),
        "assistant 消息必须回填 tool_calls"
    );
    let tool_msg = &sent2["messages"][3];
    assert_eq!(tool_msg["tool_call_id"], "call_1");
    let content = tool_msg["content"].as_str().unwrap();
    assert!(content.contains("最大速度"), "回执应是仿真报告: {content}");
    server.stop();
}

#[test]
fn agent_handles_multiple_tools_in_one_turn() {
    // 一次回复里要求同时校验和渲染（渲染在没有 GPU 的环境会失败，但链路要通）
    let server = MockLlm::start_sequence(
        vec![
            json!({
                "choices": [{
                    "message": {
                        "role": "assistant", "content": "",
                        "tool_calls": [
                            { "id": "c1", "type": "function",
                              "function": { "name": "quench3d_validate",
                                            "arguments": json!({ "scene": ball_scene() }).to_string() } },
                            { "id": "c2", "type": "function",
                              "function": { "name": "quench3d_schema", "arguments": "{}" } }
                        ]
                    },
                    "finish_reason": "tool_calls"
                }]
            })
            .to_string(),
            text_response("场景合法，能力清单已读取。"),
        ],
        true,
    );

    let mut agent = agent_for(&server);
    let turn = agent.run_turn("帮我检查这个球场景", 4).unwrap();
    assert_eq!(turn.trace.len(), 2);
    assert_eq!(turn.trace[0].name, "quench3d_validate");
    assert!(turn.trace[0].ok);
    assert_eq!(turn.trace[1].name, "quench3d_schema");
    assert!(turn.trace[1].ok);
    assert!(turn.trace[1].summary.contains("quench3d") || turn.trace[1].summary.contains("name"));
}

#[test]
fn broken_tool_arguments_are_reported_back_to_the_model() {
    // 模型给了一个非法场景（shape.type 写错）→ 工具应当失败，并把诊断回喂
    let bad_scene = json!({
        "objects": [{ "name": "a", "shape": { "type": "pyramid" }, "body": "dynamic" }]
    });
    let server = MockLlm::start_sequence(
        vec![
            tool_call_response(
                "call_bad",
                "quench3d_simulate",
                json!({ "scene": bad_scene, "steps": 10 }),
            ),
            text_response("我把形状改成 box 再试。"),
        ],
        true,
    );

    let mut agent = agent_for(&server);
    let turn = agent.run_turn("跑一下这个场景", 4).unwrap();
    assert_eq!(turn.trace.len(), 1);
    assert!(!turn.trace[0].ok, "非法场景应当让工具失败");
    assert!(
        turn.trace[0].summary.contains("invalid") || turn.trace[0].summary.contains("错误"),
        "摘要要说明失败原因: {}",
        turn.trace[0].summary
    );

    let _ = server.next_request(Duration::from_secs(5));
    let second = server.next_request(Duration::from_secs(5)).unwrap();
    let sent: Value = serde_json::from_str(&second).unwrap();
    let tool_content = sent["messages"][3]["content"].as_str().unwrap();
    assert!(
        tool_content.contains("objects[0].shape.type") || tool_content.contains("pyramid"),
        "回执里要带带路径的诊断，模型才能自己改: {tool_content}"
    );
    server.stop();
}

#[test]
fn agent_stops_at_the_round_limit_instead_of_looping_forever() {
    // 模型一直要调工具：agent 必须在上限处停下并如实说明
    let replies: Vec<String> = (0..10)
        .map(|i| tool_call_response(&format!("c{i}"), "quench3d_schema", json!({})))
        .collect();
    let server = MockLlm::start_sequence(replies, true);

    let mut agent = agent_for(&server);
    let turn = agent.run_turn("一直调用工具试试", 3).unwrap();
    assert_eq!(turn.rounds, 3, "往返轮数应当等于上限");
    assert_eq!(turn.trace.len(), 3);
    assert!(
        turn.text.contains("上限"),
        "必须告诉用户达到了上限: {}",
        turn.text
    );
    server.stop();
}

#[test]
fn plain_text_answer_needs_no_tools() {
    let server = MockLlm::start_sequence(vec![text_response("你好，我可以帮你做物理仿真。")], true);
    let mut agent = agent_for(&server);
    let turn = agent.run_turn("你能做什么", 2).unwrap();
    assert_eq!(turn.rounds, 0);
    assert!(turn.trace.is_empty());
    assert!(turn.text.contains("物理仿真"));
    // 上下文里应当留下 system/user/assistant 三条
    assert_eq!(agent.history_len(), 3);
    agent.reset();
    assert_eq!(agent.history_len(), 1);
}
