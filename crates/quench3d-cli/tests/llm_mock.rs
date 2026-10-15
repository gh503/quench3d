//! 大模型客户端测试：对着假 OpenAI 服务跑真实 HTTP 往返。

mod common;

use std::time::Duration;

use common::{mock_broken_reply, mock_scene_reply, MockLlm};
use quench3d_cli::llm::{LlmClient, LlmConfig, Message, Url};
use quench3d_cli::scene_doc::{SceneDoc, Severity};

fn client(base_url: &str) -> LlmClient {
    LlmClient::new(LlmConfig {
        base_url: base_url.to_string(),
        model: "mock-model".to_string(),
        timeout_secs: 10,
        ..Default::default()
    })
}

#[test]
fn chat_reads_content_length_response_and_sends_expected_request() {
    let server = MockLlm::start("你好，我是模型");
    let outcome = client(&server.base_url)
        .chat(&[Message::system("你是助手"), Message::user("说点什么")])
        .expect("请求应当成功");

    assert_eq!(outcome.text, "你好，我是模型");
    assert_eq!(outcome.model, "mock-model");
    assert_eq!(outcome.total_tokens, Some(33));

    let request = server
        .next_request(Duration::from_secs(5))
        .expect("收到请求");
    let sent: serde_json::Value = serde_json::from_str(&request).unwrap();
    assert_eq!(sent["model"], "mock-model");
    assert_eq!(sent["stream"], false);
    assert_eq!(sent["messages"][0]["role"], "system");
    assert_eq!(sent["messages"][1]["content"], "说点什么");
}

#[test]
fn chat_decodes_chunked_responses() {
    let server = MockLlm::start_with("分块也能读".to_string(), 200, true);
    let outcome = client(&server.base_url)
        .chat(&[Message::user("hi")])
        .expect("分块响应应当被正确解码");
    assert_eq!(outcome.text, "分块也能读");
}

#[test]
fn chat_json_extracts_scene_from_markdown_code_fence() {
    let server = MockLlm::start(&mock_scene_reply());
    let (value, outcome) = client(&server.base_url)
        .chat_json(&[Message::system("生成场景"), Message::user("来一座塔")])
        .expect("应当能抠出 JSON");

    let doc: SceneDoc = serde_json::from_value(value).expect("应当是合法场景结构");
    let errors: Vec<_> = doc
        .validate()
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .collect();
    assert!(errors.is_empty(), "模型给的场景应当能通过校验: {errors:?}");
    assert_eq!(doc.objects.len(), 3);
    assert!(outcome.text.contains("```json"), "原始输出应当保留");
}

#[test]
fn broken_scene_output_is_rejected_by_validation_with_paths() {
    let server = MockLlm::start(&mock_broken_reply());
    let (value, _) = client(&server.base_url)
        .chat_json(&[Message::user("随便")])
        .expect("JSON 本身能抠出来");

    // 语法合法（字段名未知会被 deny_unknown_fields 拦下）
    let doc: Result<SceneDoc, _> = serde_json::from_value(value);
    match doc {
        Err(e) => assert!(
            e.to_string().contains("pyramid") || e.to_string().contains("unknown"),
            "错误信息要能指出问题字段: {e}"
        ),
        Ok(doc) => {
            let errors: Vec<_> = doc
                .validate()
                .into_iter()
                .filter(|d| d.severity == Severity::Error)
                .collect();
            assert!(!errors.is_empty(), "坏场景必须产生错误");
            assert!(
                errors.iter().any(|d| d.path.starts_with("objects[0]")),
                "诊断必须带 JSON 路径: {errors:?}"
            );
        }
    }
}

#[test]
fn http_error_status_is_reported_with_body() {
    let server = MockLlm::start_with("模型不存在".to_string(), 404, false);
    let err = client(&server.base_url)
        .chat(&[Message::user("hi")])
        .expect_err("404 应当报错");
    assert_eq!(err.kind, "http");
    assert!(err.message.contains("404"), "{}", err.message);
    assert!(err.message.contains("模型不存在"), "{}", err.message);
}

#[test]
fn refused_connection_gives_actionable_hint() {
    // 先占一个端口再关掉，确保没人监听
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let err = client(&format!("http://127.0.0.1:{port}/v1"))
        .chat(&[Message::user("hi")])
        .expect_err("连接应当失败");
    assert_eq!(err.kind, "network");
    let hint = err.hint.unwrap_or_default();
    assert!(
        hint.contains("ollama") || hint.contains("base-url"),
        "提示要能指导修复: {hint}"
    );
}

#[test]
fn env_config_round_trip() {
    std::env::set_var("QUENCH3D_LLM_BASE_URL", "http://192.168.1.9:8000/v1/");
    std::env::set_var("QUENCH3D_LLM_MODEL", "qwen2.5:7b");
    std::env::set_var("QUENCH3D_LLM_API_KEY", "sk-test");
    std::env::set_var("QUENCH3D_LLM_TEMPERATURE", "0.7");
    std::env::set_var("QUENCH3D_LLM_MAX_TOKENS", "512");

    let c = LlmConfig::from_env();
    assert_eq!(c.base_url, "http://192.168.1.9:8000/v1/");
    assert_eq!(c.model, "qwen2.5:7b");
    assert_eq!(c.api_key.as_deref(), Some("sk-test"));
    assert!((c.temperature - 0.7).abs() < 1e-6);
    assert_eq!(c.max_tokens, 512);
    // 结尾斜杠不能导致重复
    assert_eq!(c.endpoint(), "http://192.168.1.9:8000/v1/chat/completions");
    assert!(c.describe().contains("已设置"));

    for k in [
        "QUENCH3D_LLM_BASE_URL",
        "QUENCH3D_LLM_MODEL",
        "QUENCH3D_LLM_API_KEY",
        "QUENCH3D_LLM_TEMPERATURE",
        "QUENCH3D_LLM_MAX_TOKENS",
    ] {
        std::env::remove_var(k);
    }

    // 只给主机名时自动补 /v1（Ollama 习惯）
    std::env::remove_var("OPENAI_BASE_URL");
    std::env::set_var("OLLAMA_HOST", "127.0.0.1:11434");
    assert_eq!(LlmConfig::from_env().base_url, "http://127.0.0.1:11434/v1");
    std::env::remove_var("OLLAMA_HOST");
}

#[test]
fn url_parse_rejects_unsupported_scheme() {
    let err = Url::parse("ftp://example.com/x").unwrap_err();
    assert_eq!(err.kind, "config");
    assert!(Url::parse("localhost:8080").is_err(), "缺协议头应当报错");
}

/// `https` 路径把密钥交给 curl 时，**绝不能出现在命令行参数里**。
///
/// 用的是"假 curl"：它把自己的 argv、读到的配置文件、请求体全部记下来，
/// 然后回一个合法的 OpenAI 响应。这样可以把"密钥在哪"这件事**当成数据来断言**，
/// 而不是靠人读代码相信它。
#[cfg(unix)]
#[test]
fn api_key_never_reaches_the_curl_command_line() {
    use quench3d_cli::llm::http_post_json_with_curl;
    use std::os::unix::fs::PermissionsExt;

    const SECRET: &str = "sk-super-secret-KEY-do-not-leak-9f3a";
    let dir = std::env::temp_dir().join(format!("quench3d_curl_argv_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");

    let argv_log = dir.join("argv.txt");
    let config_log = dir.join("config.txt");
    let body_log = dir.join("body.txt");
    let fake_curl = dir.join("curl");
    // 假 curl：记 argv（含 -K 之后那个配置文件的 `ls -l`，用来验证 0600），
    // 抄一份配置文件，读走 stdin，再吐一个标准响应。
    std::fs::write(
        &fake_curl,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$@" > "{argv}"
prev=""
for a in "$@"; do
  if [ "$prev" = "-K" ]; then
    cfg="$a"
  fi
  prev="$a"
done
if [ -n "$cfg" ]; then
  cp "$cfg" "{config}" 2>/dev/null
  ls -l "$cfg" >> "{argv}" 2>/dev/null
fi
cat > "{body}"
printf '%s' '{{"model":"fake-model","choices":[{{"index":0,"message":{{"role":"assistant","content":"curl 路径通了"}},"finish_reason":"stop"}}],"usage":{{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}}}'
"#,
            argv = argv_log.display(),
            config = config_log.display(),
            body = body_log.display(),
        ),
    )
    .expect("写假 curl");
    let mut perms = std::fs::metadata(&fake_curl).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&fake_curl, perms).unwrap();

    let headers = vec![
        ("Content-Type".to_string(), "application/json".to_string()),
        ("Authorization".to_string(), format!("Bearer {SECRET}")),
    ];
    let body = r#"{"model":"fake-model","messages":[{"role":"user","content":"说点什么"}]}"#;
    let response = http_post_json_with_curl(
        "https://api.example.invalid/v1/chat/completions",
        &headers,
        body,
        Duration::from_secs(10),
        Some(&fake_curl),
    )
    .expect("假 curl 应当成功");
    assert!(response.contains("curl 路径通了"), "{response}");

    let argv = std::fs::read_to_string(&argv_log).expect("应当记下 argv");
    let config = std::fs::read_to_string(&config_log).expect("应当记下配置文件");
    let sent_body = std::fs::read_to_string(&body_log).expect("应当记下请求体");

    // ① 命令行参数里没有密钥——这是本轮要修的那个洞
    assert!(
        !argv.contains(SECRET),
        "API key 出现在命令行参数里了：\n{argv}"
    );
    assert!(
        !argv.contains("Bearer"),
        "命令行参数里不该有认证头：\n{argv}"
    );
    // ② 配置文件的权限必须是 0600（只有本用户可读）
    assert!(
        argv.contains("-rw-------"),
        "curl 配置文件必须是 0600：\n{argv}"
    );
    // ③ 密钥确实被送出去了（只是走配置文件，不是 argv）
    assert!(
        config.contains(&format!("Authorization: Bearer {SECRET}")),
        "配置文件里应当有认证头：\n{config}"
    );
    assert!(
        config.contains("url = \"https://api.example.invalid/v1/chat/completions\""),
        "配置文件里应当有 url：\n{config}"
    );
    // ④ 请求体仍然走 stdin，不落盘、也不进 argv
    assert!(sent_body.contains("说点什么"), "{sent_body}");
    assert!(!argv.contains("说点什么"), "提示词不该出现在 argv 里");

    let _ = std::fs::remove_dir_all(&dir);
}
