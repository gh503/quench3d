//! Shared desktop/browser agent conversation and nonblocking editor panel.
use crate::doc::Doc;
use quench3d::Engine;
use quench3d_cli::llm::{parse_chat_response, ToolCall};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const MAX_ROUNDS: usize = 32;
const MAX_CALLS: usize = 16;
const SYSTEM: &str = "You operate the live Quench3D editor through the provided tools. Explain a short plan, then execute it. Before editing, call set_playback with paused:true, then inspect_scene. Reference exact unique names from inspect_scene. All tools except inspect_scene and set_playback require expected_revision from the most recent inspection or tool receipt; never guess revisions. If stale_revision occurs inspect again and reconsider. Use real tools to complete the goal, report errors honestly, and do not claim an edit without a successful tool receipt. Keep requests small. Never request secrets or credentials through scene tools.";

type Mailbox = Arc<Mutex<VecDeque<(u64, Result<Value, String>)>>>;

/// AI panel using the client's explicitly saved local provider/model library.
pub struct AgentPanel {
    /// Whether the panel is visible.
    pub open: bool,
    session: Session,
    goal: String,
    configuration: crate::agent_config::ConfigLibrary,
    config_error: Option<String>,
    mailbox: Mailbox,
    #[cfg(not(target_arch = "wasm32"))]
    workers: Vec<std::thread::JoinHandle<()>>,
    #[cfg(target_arch = "wasm32")]
    abort: Option<web_sys::AbortController>,
}
impl Default for AgentPanel {
    fn default() -> Self {
        let (configuration, config_error) = match crate::agent_config::ConfigLibrary::load() {
            Ok(configuration) => (configuration, None),
            Err(error) => (crate::agent_config::ConfigLibrary::default(), Some(error)),
        };
        Self {
            open: false,
            session: Session::default(),
            goal: String::new(),
            configuration,
            config_error,
            mailbox: Arc::new(Mutex::new(VecDeque::new())),
            #[cfg(not(target_arch = "wasm32"))]
            workers: Vec::new(),
            #[cfg(target_arch = "wasm32")]
            abort: None,
        }
    }
}
impl AgentPanel {
    /// Cancel pending work and invalidate late responses after a scene change.
    pub fn cancel(&mut self) {
        self.session.cancel();
        #[cfg(target_arch = "wasm32")]
        if let Some(abort) = self.abort.take() {
            abort.abort();
        }
    }

    /// Start a goal using the same session machinery as the visible Send button.
    pub fn start_goal(&mut self, prompt: &str) {
        self.cancel();
        self.session.start(prompt);
    }

    /// Return observable session state without connection credentials.
    pub fn snapshot(&self) -> Value {
        json!({"status":self.session.status,"messages":self.session.messages,
            "trace":self.session.trace,"error":self.session.error,
            "rounds":self.session.rounds,"generation":self.session.generation})
    }

    /// Draw the assistant inside the editor's right sidebar.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let mut send = false;
        let mut stop = false;
        let mut configuration_changed = false;
        ui.heading("AI 助手");
        ui.collapsing("本地模型配置", |ui| {
            configuration_changed = self.configuration.ui(ui);
            if let Some(error) = &self.config_error {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
        });
        if let Ok(selected) = self.configuration.selected() {
            ui.label(format!("当前模型：{}", selected.model));
        }
        ui.label(format!(
            "状态：{} · 请求轮数 {}/{}",
            self.session.status, self.session.rounds, MAX_ROUNDS
        ));
        egui::ScrollArea::vertical()
            .max_height(330.0)
            .show(ui, |ui| {
                for message in &self.session.messages {
                    let role = message["role"].as_str().unwrap_or("");
                    if role == "system" {
                        continue;
                    }
                    if let Some(text) = message["content"].as_str() {
                        if !text.is_empty() {
                            ui.label(format!("{role}: {text}"));
                        }
                    }
                    if let Some(calls) = message["tool_calls"].as_array() {
                        for call in calls {
                            ui.label(format!(
                                "工具：{}",
                                call["function"]["name"].as_str().unwrap_or("?")
                            ));
                        }
                    }
                }
            });
        if let Some(error) = &self.session.error {
            ui.colored_label(egui::Color32::LIGHT_RED, error);
        }
        ui.add(
            egui::TextEdit::multiline(&mut self.goal)
                .desired_rows(3)
                .hint_text("描述要在当前场景完成的操作"),
        );
        ui.horizontal(|ui| {
            send = ui
                .add_enabled(!self.goal.trim().is_empty(), egui::Button::new("发送"))
                .clicked();
            stop = ui.button("停止").clicked();
        });
        if configuration_changed {
            self.config_error = None;
        }
        if stop || configuration_changed {
            self.cancel();
        }
        if send {
            let goal = self.goal.clone();
            self.start_goal(&goal);
        }
        if matches!(self.session.status, "requesting" | "tools") {
            ui.ctx().request_repaint();
        }
    }

    /// Advance at most one editor tool per frame; network work stays off thread.
    pub fn poll(&mut self, engine: &mut Engine, doc: &mut Doc) {
        #[cfg(not(target_arch = "wasm32"))]
        self.workers.retain(|worker| !worker.is_finished());
        let completed: Vec<_> = match self.mailbox.lock() {
            Ok(mut queue) => queue.drain(..).collect(),
            Err(_) => {
                self.session.fail("Agent response mailbox failed".into());
                return;
            }
        };
        for (generation, result) in completed {
            if generation != self.session.generation {
                continue;
            }
            match result {
                Ok(value) => {
                    if let Err(error) = self.session.accept(generation, value) {
                        self.session.fail(error);
                    }
                }
                Err(error) => self.session.fail(error),
            }
        }
        if !self.session.pending.is_empty() {
            self.session
                .tool_tick(|name, args| crate::commands::dispatch(engine, doc, name, args));
            return;
        }
        if self.session.status == "requesting" && !self.session.inflight {
            #[cfg(not(target_arch = "wasm32"))]
            if self.workers.len() >= 2 {
                return;
            }
            if self.session.rounds >= MAX_ROUNDS {
                self.session.fail(
                    "Agent reached the 32-request limit; inspect the scene before continuing."
                        .into(),
                );
                return;
            }
            self.session.rounds += 1;
            self.session.inflight = true;
            self.launch();
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn launch(&mut self) {
        use quench3d_cli::llm::{LlmClient, Message};
        let selected = match self.configuration.selected() {
            Ok(selected) => selected,
            Err(error) => {
                self.session.fail(error);
                return;
            }
        };
        let config = quench3d_cli::llm::LlmConfig {
            base_url: selected.base_url,
            model: selected.model,
            api_key: selected.api_key,
            temperature: selected.temperature,
            max_tokens: selected.max_tokens,
            timeout_secs: selected.timeout_secs,
        };
        let key = config.api_key.clone();
        let messages: Vec<_> = self
            .session
            .messages
            .iter()
            .map(|raw| {
                let role = match raw["role"].as_str() {
                    Some("system") => "system",
                    Some("user") => "user",
                    Some("tool") => "tool",
                    _ => "assistant",
                };
                Message {
                    role,
                    content: raw["content"].as_str().unwrap_or("").into(),
                    tool_calls: raw.get("tool_calls").cloned(),
                    tool_call_id: raw["tool_call_id"].as_str().map(str::to_owned),
                }
            })
            .collect();
        let tools = crate::commands::tools();
        let generation = self.session.generation;
        let mailbox = self.mailbox.clone();
        let worker = std::thread::spawn(move || {
            let result = LlmClient::new(config)
                .chat_with_tools(&messages, Some(&tools))
                .map(|outcome| json!({"choices":[{"message":outcome.assistant_message}]}))
                .map_err(|error| redact(error.to_string(), key.as_deref()));
            if let Ok(mut queue) = mailbox.lock() {
                queue.push_back((generation, result));
            }
        });
        self.workers.push(worker);
    }

    #[cfg(target_arch = "wasm32")]
    fn launch(&mut self) {
        use wasm_bindgen::{JsCast, JsValue};
        let generation = self.session.generation;
        let mailbox = self.mailbox.clone();
        let controller = match web_sys::AbortController::new() {
            Ok(controller) => controller,
            Err(_) => {
                self.session
                    .fail("Browser cancellation controller unavailable".into());
                return;
            }
        };
        let signal = controller.signal();
        self.abort = Some(controller);
        let config = match self.configuration.selected() {
            Ok(config) => config,
            Err(error) => {
                self.session.fail(error);
                return;
            }
        };
        let body =
            browser_request(&self.session.messages, crate::commands::tools(), &config).to_string();
        wasm_bindgen_futures::spawn_local(async move {
            let result: Result<Value, String> = async {
                let init = web_sys::RequestInit::new();
                init.set_method("POST");
                init.set_mode(web_sys::RequestMode::SameOrigin);
                init.set_body(&JsValue::from_str(&body));
                init.set_signal(Some(&signal));
                let request = web_sys::Request::new_with_str_and_init("/api/agent/chat", &init)
                    .map_err(|_| "Cannot create model request".to_string())?;
                request
                    .headers()
                    .set("Content-Type", "application/json")
                    .map_err(|_| "Cannot set request headers".to_string())?;
                let window = web_sys::window().ok_or("Browser window unavailable")?;
                let value =
                    wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&request))
                        .await
                        .map_err(|_| "Model proxy request failed or was cancelled".to_string())?;
                let response: web_sys::Response = value
                    .dyn_into()
                    .map_err(|_| "Invalid proxy response".to_string())?;
                if !response.ok() {
                    return Err(format!("Model proxy HTTP {}", response.status()));
                }
                let text = wasm_bindgen_futures::JsFuture::from(
                    response
                        .text()
                        .map_err(|_| "Cannot read proxy response".to_string())?,
                )
                .await
                .map_err(|_| "Cannot read proxy response".to_string())?
                .as_string()
                .ok_or("Proxy response is not text")?;
                // Parse with the same protocol validation as the native client.
                let outcome = parse_chat_response(&text, "")
                    .map_err(|_| "Model proxy returned an invalid chat response".to_string())?;
                Ok(json!({"choices":[{"message":outcome.assistant_message}]}))
            }
            .await;
            if let Ok(mut queue) = mailbox.lock() {
                queue.push_back((generation, result));
            }
        });
    }
}

#[cfg(any(target_arch = "wasm32", test))]
fn browser_request(
    messages: &[Value],
    tools: Value,
    config: &crate::agent_config::LlmConfig,
) -> Value {
    json!({"messages": messages, "tools": tools, "connection": {
        "base_url": config.base_url, "model": config.model,
        "api_key": config.api_key.as_deref().unwrap_or(""),
        "temperature": config.temperature, "max_tokens": config.max_tokens,
        "timeout_secs": config.timeout_secs,
    }})
}

#[cfg(any(not(target_arch = "wasm32"), test))]
fn redact(mut text: String, key: Option<&str>) -> String {
    if let Some(key) = key.filter(|key| !key.is_empty()) {
        text = text.replace(key, "[secret]");
    }
    text
}

struct Session {
    generation: u64,
    messages: Vec<Value>,
    pending: VecDeque<Value>,
    trace: Vec<Value>,
    status: &'static str,
    error: Option<String>,
    rounds: usize,
    inflight: bool,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            generation: 0,
            messages: Vec::new(),
            pending: VecDeque::new(),
            trace: Vec::new(),
            status: "idle",
            error: None,
            rounds: 0,
            inflight: false,
        }
    }
}
impl Session {
    fn start(&mut self, prompt: &str) {
        self.messages.clear();
        self.trace.clear();
        self.error = None;
        self.rounds = 0;
        if prompt.trim().is_empty() {
            self.fail("Goal must not be empty".into());
            return;
        }
        self.messages
            .push(json!({"role":"system","content":SYSTEM}));
        self.messages.push(json!({"role":"user","content":prompt}));
        self.status = "requesting";
    }
    fn fail(&mut self, error: String) {
        self.error = Some(error);
        self.status = "error";
        self.inflight = false;
        self.pending.clear();
    }
    fn accept(&mut self, generation: u64, response: Value) -> Result<(), String> {
        if generation != self.generation {
            return Ok(());
        }
        let outcome =
            parse_chat_response(&response.to_string(), "").map_err(|error| error.to_string())?;
        if outcome
            .assistant_message
            .get("role")
            .and_then(Value::as_str)
            != Some("assistant")
        {
            return Err("Model response role must be assistant; no tools were executed".into());
        }
        if outcome
            .assistant_message
            .get("tool_calls")
            .is_some_and(|calls| !calls.is_null() && !calls.is_array())
        {
            return Err("Model returned a malformed tool call list".into());
        }
        let raw_calls = outcome
            .assistant_message
            .get("tool_calls")
            .and_then(Value::as_array);
        if raw_calls.is_some_and(|calls| calls.len() != outcome.tool_calls.len()) {
            return Err("Model returned a malformed tool call; no tools were executed".into());
        }
        if outcome.tool_calls.len() > MAX_CALLS {
            return Err("Model exceeded the 16-tool-call response limit".into());
        }
        let mut ids = std::collections::HashSet::new();
        if outcome
            .tool_calls
            .iter()
            .any(|call| !ids.insert(call.id.clone()))
        {
            return Err("Model returned duplicate tool call IDs".into());
        }
        self.inflight = false;
        self.messages.push(outcome.assistant_message.clone());
        self.pending = outcome.assistant_message["tool_calls"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into();
        self.status = if self.pending.is_empty() {
            "done"
        } else {
            "tools"
        };
        Ok(())
    }
    fn cancel(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending.clear();
        self.inflight = false;
        self.status = "cancelled";
    }
    fn tool_tick(&mut self, dispatch: impl FnOnce(&str, Value) -> Value) {
        let Some(raw) = self.pending.pop_front() else {
            return;
        };
        let call = ToolCall {
            id: raw["id"].as_str().unwrap_or("").into(),
            name: raw["function"]["name"].as_str().unwrap_or("").into(),
            arguments: raw["function"]["arguments"].as_str().unwrap_or("").into(),
        };
        let result = match call.try_args() {
            Ok(args) => dispatch(&call.name, args),
            Err(error) => {
                json!({"ok":false,"error":format!("Invalid JSON object arguments: {error}")})
            }
        };
        self.trace
            .push(json!({"id":call.id,"name":call.name,"result":result}));
        self.messages
            .push(json!({"role":"tool","tool_call_id":call.id,"content":result.to_string()}));
        if self.pending.is_empty() {
            self.status = "requesting";
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn response(arguments: &str) -> Value {
        json!({"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[
            {"id":"call-real-17","type":"function","function":{"name":"inspect_scene","arguments":arguments}}
        ]}}]})
    }
    #[test]
    fn agent_session_tool_result_reaches_next_request() {
        let mut s = Session::default();
        s.accept(0, response("{}")).unwrap();
        s.tool_tick(|name, args| {
            assert_eq!(name, "inspect_scene");
            assert_eq!(args, json!({}));
            json!({"ok":true,"result":{"objects":[]},"revision":"r1"})
        });
        assert_eq!(
            s.messages.len(),
            2,
            "assistant call and real tool receipt must enter next request"
        );
        assert!(s.messages[0]["content"].is_null());
        assert_eq!(s.messages[1]["tool_call_id"], "call-real-17");
        let result: Value =
            serde_json::from_str(s.messages[1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(result["revision"], "r1");
        assert_eq!(s.status, "requesting");
    }
    #[test]
    fn agent_cancel_ignores_late_response() {
        let mut s = Session::default();
        s.cancel();
        s.accept(0, response("{}")).unwrap();
        assert!(s.messages.is_empty());
        assert!(s.pending.is_empty());
    }
    #[test]
    fn agent_malformed_arguments_returns_tool_error() {
        let mut s = Session::default();
        s.accept(0, response("{broken")).unwrap();
        s.tool_tick(|_, _| panic!("malformed arguments must never dispatch"));
        assert_eq!(s.messages.len(), 2);
        assert_eq!(s.messages[1]["tool_call_id"], "call-real-17");
        let result: Value =
            serde_json::from_str(s.messages[1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(result["ok"], false);
        assert!(result["error"].as_str().unwrap().contains("JSON"));
    }
    #[test]
    fn agent_one_tool_per_tick_and_new_goal_cancels_queue() {
        let mut panel = AgentPanel::default();
        let mut raw = response("{}");
        raw["choices"][0]["message"]["tool_calls"].as_array_mut().unwrap().push(json!({"id":"second","type":"function","function":{"name":"inspect_scene","arguments":"{}"}}));
        panel.session.accept(0, raw).unwrap();
        panel.session.tool_tick(|_, _| json!({"ok":true}));
        assert_eq!(panel.session.pending.len(), 1);
        panel.start_goal("new goal");
        assert!(panel.session.pending.is_empty());
        assert_eq!(panel.snapshot()["status"], "requesting");
        assert_eq!(panel.snapshot()["messages"].as_array().unwrap().len(), 2);
    }
    #[test]
    fn agent_browser_request_obeys_same_origin_proxy_contract() {
        let messages = vec![json!({"role":"user","content":"inspect"})];
        let config = crate::agent_config::ConfigLibrary::default()
            .selected()
            .unwrap();
        let request = browser_request(&messages, crate::commands::tools(), &config);
        assert_eq!(request["connection"]["base_url"], config.base_url);
        assert_eq!(request["connection"]["model"], config.model);
        assert_eq!(request["messages"], json!(messages));
        assert!(request["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty()));
        let keys: std::collections::BTreeSet<_> = request
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            ["connection", "messages", "tools"].into_iter().collect()
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn agent_native_http_tools_change_live_scene() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::time::{Duration, Instant};
        if std::env::var("QUENCH3D_GPU_TESTS").as_deref() != Ok("1") {
            eprintln!("SKIPPED native agent HTTP GPU test: set QUENCH3D_GPU_TESTS=1");
            return;
        }
        let gpu = pollster::block_on(quench3d::render::GpuContext::headless(
            64,
            64,
            Default::default(),
        ))
        .expect("explicitly requested native agent GPU fixture");
        let mut engine = Engine::new(gpu, &quench3d::EngineConfig::default());
        let mut doc = Doc::new();
        crate::scene_io::load_text_into_doc(&mut engine, &mut doc,
            r#"{"gravity":[0,0,0],"objects":[{"name":"original","body":"dynamic","position":[0,1,0],"shape":{"type":"sphere","radius":0.3}}]}"#).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut receipts = Vec::new();
            for round in 0..5 {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "mock model request deadline");
                            std::thread::park_timeout(Duration::from_millis(5));
                        }
                        Err(error) => panic!("model accept failed: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with("POST /v1/chat/completions "), "{line}");
                let mut length = None;
                loop {
                    line.clear();
                    assert!(
                        reader.read_line(&mut line).unwrap() > 0,
                        "unexpected EOF in HTTP headers"
                    );
                    if line == "\r\n" {
                        break;
                    }
                    let (name, value) = line.split_once(':').expect("HTTP header");
                    if name.eq_ignore_ascii_case("content-length") {
                        length = Some(value.trim().parse::<usize>().unwrap());
                    }
                }
                let length = length.expect("content length");
                assert!(length < 1_000_000);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let request: Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(request["model"], "native-test");
                assert!(request["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|tool| tool["function"]["name"] == "spawn_primitive"));
                let last = request["messages"].as_array().unwrap().last().unwrap();
                let previous = if round > 0 {
                    assert_eq!(last["role"], "tool");
                    assert_eq!(last["tool_call_id"], format!("native-{}", round - 1));
                    let receipt: Value =
                        serde_json::from_str(last["content"].as_str().unwrap()).unwrap();
                    assert_eq!(receipt["ok"], true, "{receipt}");
                    receipts.push(receipt.clone());
                    Some(receipt)
                } else {
                    None
                };
                let (name, args) = match round {
                    0 => ("set_playback", json!({"paused":true})),
                    1 => ("inspect_scene", json!({})),
                    2 => {
                        let inspected = previous.as_ref().unwrap();
                        assert!(inspected["result"]["objects"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|object| object["name"] == "original"));
                        (
                            "spawn_primitive",
                            json!({"name":"native created","shape":{"type":"sphere","radius":0.25},"position":[1,2,3],"expected_revision":inspected["revision"]}),
                        )
                    }
                    3 => ("inspect_scene", json!({})),
                    _ => {
                        let inspected = previous.as_ref().unwrap();
                        let object = inspected["result"]["objects"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .find(|object| object["name"] == "native created")
                            .expect("spawn reflected in actual inspection");
                        assert_eq!(object["position"], json!([1.0, 2.0, 3.0]));
                        assert_eq!(object["body_position"], json!([1.0, 2.0, 3.0]));
                        ("", json!({}))
                    }
                };
                let assistant = if round == 4 {
                    json!({"role":"assistant","content":"Created and inspected native created."})
                } else {
                    json!({"role":"assistant","content":null,"tool_calls":[{"id":format!("native-{round}"),"type":"function","function":{"name":name,"arguments":args.to_string()}}]})
                };
                let response = json!({"choices":[{"message":assistant}]}).to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
            }
            receipts
        });
        let mut configuration = crate::agent_config::ConfigLibrary::default();
        configuration.providers[0].base_url = format!("http://{address}/v1");
        configuration.models[0].model = "native-test".into();
        configuration.models[0].timeout_secs = 2;
        let mut panel = AgentPanel {
            configuration,
            ..AgentPanel::default()
        };
        panel.start_goal(
            "Pause, inspect, create a sphere named native created at [1,2,3], and verify it.",
        );
        let before_frames = engine.frame_count;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut game = |_: &mut Engine, _: f32| {};
        while !matches!(panel.session.status, "done" | "error") && Instant::now() < deadline {
            panel.poll(&mut engine, &mut doc);
            engine.tick(&mut game, 1.0 / 60.0);
            std::thread::park_timeout(Duration::from_millis(5));
        }
        assert_eq!(panel.snapshot()["status"], "done", "{}", panel.snapshot());
        assert!(
            engine.frame_count > before_frames + 4,
            "frames must progress while the real native worker communicates"
        );
        assert!(engine.paused);
        let object = engine
            .scene
            .objects
            .iter()
            .find(|object| object.name == "native created")
            .unwrap();
        assert_eq!(
            object.transform.position,
            quench3d_math::Vec3::new(1.0, 2.0, 3.0)
        );
        assert_eq!(
            engine
                .world
                .body(object.body.unwrap())
                .unwrap()
                .transform()
                .position,
            object.transform.position
        );
        assert_eq!(panel.session.trace.len(), 4);
        assert_eq!(server.join().expect("mock model thread").len(), 4);
    }

    #[test]
    fn agent_rejects_non_assistant_or_missing_role_without_queuing_tools() {
        for role in [Some("user"), Some("tool"), None] {
            let mut raw = response("{}");
            let message = raw["choices"][0]["message"].as_object_mut().unwrap();
            if let Some(role) = role {
                message.insert("role".into(), json!(role));
            } else {
                message.remove("role");
            }
            let mut session = Session::default();
            let error = session.accept(0, raw).unwrap_err();
            assert!(error.contains("role must be assistant"), "{error}");
            assert!(
                session.messages.is_empty(),
                "invalid role must not enter history"
            );
            assert!(
                session.pending.is_empty(),
                "invalid role must not execute tools"
            );
            assert!(session.trace.is_empty());
        }
    }

    #[test]
    fn agent_credentials_redacted_and_duplicate_ids_rejected() {
        assert_eq!(
            redact("error secret-value".into(), Some("secret-value")),
            "error [secret]"
        );
        let mut raw = response("{}");
        let duplicate = raw["choices"][0]["message"]["tool_calls"][0].clone();
        raw["choices"][0]["message"]["tool_calls"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(Session::default().accept(0, raw).is_err());
    }
}
