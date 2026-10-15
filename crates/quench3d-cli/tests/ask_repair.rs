//! `ask` 的自动修复闭环测试：模型第一次给出不合法场景时，诊断要被回喂并重试。

mod common;

use std::time::Duration;

use common::MockLlm;
use quench3d_cli::generate::{generate, GenerateOptions};
use quench3d_cli::llm::{LlmClient, LlmConfig};
use serde_json::{json, Value};

const BAD_SCENE: &str =
    r#"{"objects":[{"name":"塔","shape":{"type":"pyramid"},"body":"dynamic"}]}"#;

fn valid_scene() -> String {
    json!({
        "name": "修好的场景",
        "ground": { "size": 20 },
        "objects": [
            { "name": "塔1", "shape": { "type": "box", "size": [1, 1, 1] },
              "position": [0, 0.5, 0], "body": "dynamic" },
            { "name": "塔2", "shape": { "type": "box", "size": [1, 1, 1] },
              "position": [0, 1.5, 0], "body": "dynamic" }
        ]
    })
    .to_string()
}

fn client_for(server: &MockLlm) -> LlmClient {
    LlmClient::new(LlmConfig {
        base_url: server.base_url.clone(),
        model: "mock-model".to_string(),
        timeout_secs: 10,
        ..Default::default()
    })
}

#[test]
fn invalid_scene_is_repaired_on_the_second_attempt() {
    let server = MockLlm::start_sequence(vec![BAD_SCENE.to_string(), valid_scene()], false);
    let client = client_for(&server);

    let generated = generate(&client, &GenerateOptions::new("堆一个两层塔", 2))
        .expect("生成应当成功（第二轮修好）");

    assert!(generated.valid(), "最终应当是合法场景");
    assert_eq!(generated.attempts_count(), 2, "应当尝试两次");
    assert_eq!(generated.repair_rounds_used(), 1, "应当修复一轮");
    assert!(!generated.attempts[0].valid);
    assert_eq!(generated.attempts[0].errors, 1);
    assert!(generated.attempts[0].messages[0].contains("pyramid"));
    assert!(generated.attempts[1].valid);
    // 落盘的 JSON 是规范化过的
    assert!(
        generated.scene_json.contains("quench3d.scene"),
        "{}",
        generated.scene_json
    );
    assert!(generated.scene.is_some());

    // 第一次请求：只有 system + user
    let first = server
        .next_request(Duration::from_secs(5))
        .expect("第一次请求");
    let sent1: Value = serde_json::from_str(&first).unwrap();
    assert_eq!(sent1["messages"].as_array().unwrap().len(), 2);

    // 第二次请求：assistant 的原话 + 带路径的诊断都在上下文里
    let second = server
        .next_request(Duration::from_secs(5))
        .expect("第二次请求");
    let sent2: Value = serde_json::from_str(&second).unwrap();
    let msgs = sent2["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 4, "{sent2:#?}");
    assert_eq!(msgs[2]["role"], "assistant");
    assert_eq!(msgs[3]["role"], "user");
    let repair = msgs[3]["content"].as_str().unwrap();
    assert!(repair.contains("objects[0].shape.type"), "{repair}");
    assert!(repair.contains("pyramid"), "{repair}");
    assert!(repair.contains("只返回完整的场景 JSON"), "{repair}");
    server.stop();
}

#[test]
fn hopeless_output_stops_after_the_configured_rounds() {
    // 三轮都是坏的：必须停下并如实报告，不能无限重试
    let server = MockLlm::start_sequence(
        vec![
            BAD_SCENE.to_string(),
            BAD_SCENE.to_string(),
            BAD_SCENE.to_string(),
        ],
        false,
    );
    let client = client_for(&server);

    let generated = generate(&client, &GenerateOptions::new("随便", 2)).unwrap();
    assert!(!generated.valid());
    assert_eq!(generated.attempts_count(), 3, "1 次原始 + 2 次修复");
    assert_eq!(generated.repair_rounds_used(), 2);
    assert!(generated.errors() >= 1);
    assert!(generated.scene.is_none());
    // 场景文本仍然给出，便于 agent 看到模型到底写了什么
    assert!(generated.scene_json.contains("pyramid"));
    server.stop();
}

#[test]
fn no_repair_rounds_means_single_shot() {
    let server = MockLlm::start_sequence(vec![BAD_SCENE.to_string()], false);
    let client = client_for(&server);
    let generated = generate(&client, &GenerateOptions::new("随便", 0)).unwrap();
    assert_eq!(generated.attempts_count(), 1);
    assert!(!generated.valid());
    // 只应该有一次请求
    assert!(server.next_request(Duration::from_secs(5)).is_some());
    assert!(server.next_request(Duration::from_millis(300)).is_none());
    server.stop();
}
