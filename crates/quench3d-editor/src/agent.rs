//! Shared desktop/browser agent conversation and nonblocking editor panel.
use crate::doc::Doc;
use quench3d::Engine;
use quench3d_cli::llm::{parse_chat_response, ToolCall};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const MAX_CALLS: usize = 16;
const SYSTEM: &str = "You operate the live Quench3D editor through the provided tools. Explain a short plan, then execute it. Before editing, call set_playback with paused:true, then inspect_scene. Reference exact unique names from inspect_scene. All tools except inspect_scene and set_playback require expected_revision from the most recent inspection or tool receipt; never guess revisions. If stale_revision occurs inspect again and reconsider. Use real tools to complete the goal, report errors honestly, and do not claim an edit without a successful tool receipt. Keep requests small. For a sustained planetary revolution demonstration, use set_orbit on each planet and set_body_mode static on the sun, then start playback. set_orbit is a saved kinematic loop animation, not Newtonian gravity: report that distinction honestly. set_orbit accepts exactly one center coordinate or center_name referencing a moving body. For a moon orbiting a planet, set center_name to that planet's exact name, not its current coordinates; nested motion follows the center each step. Circular center dependencies are invalid. Outer planets can use longer periods (for Kepler-like timing period proportional to radius^1.5). Do not simulate sustained motion with repeated set_transform calls or leave planets as falling dynamic bodies. Use clear_orbit to stop the loop. To display names beside scene objects, call set_object_label with visible:true for each named object; labels follow motion and support Chinese. Hide them with visible:false. Never request secrets or credentials through scene tools.";

type Mailbox = Arc<Mutex<VecDeque<(u64, Result<Value, String>)>>>;
const MAX_CATALOG_QUEUE: usize = 32;
const MAX_CATALOG_BYTES: usize = 1024 * 1024;
const MAX_CATALOG_MODELS: usize = 2048;

// Intentionally no Debug: connection snapshots contain credentials.
#[derive(Clone, PartialEq, Eq)]
struct CatalogRequest {
    token: u64,
    provider: String,
    base_url: String,
    api_key: Option<String>,
}
struct CatalogCompletion {
    request: CatalogRequest,
    result: Result<Vec<String>, String>,
}
type CatalogMailbox = Arc<Mutex<VecDeque<CatalogCompletion>>>;

/// AI panel using the client's explicitly saved local provider/model library.
pub struct AgentPanel {
    /// Whether the panel is visible.
    pub open: bool,
    session: Session,
    goal: String,
    configuration: crate::agent_config::ConfigLibrary,
    config_error: Option<String>,
    history: crate::agent_history::History,
    history_error: Option<String>,
    history_saved: String,
    catalog_pending: Vec<String>,
    catalog_active: Option<CatalogRequest>,
    catalog_token: u64,
    catalog_mailbox: CatalogMailbox,
    repaint_context: Option<egui::Context>,
    mailbox: Mailbox,
    #[cfg(not(target_arch = "wasm32"))]
    workers: Vec<std::thread::JoinHandle<()>>,
    #[cfg(target_arch = "wasm32")]
    abort: Option<web_sys::AbortController>,
}
impl Default for AgentPanel {
    fn default() -> Self {
        #[cfg(test)]
        let (configuration, config_error) = (crate::agent_config::ConfigLibrary::default(), None);
        #[cfg(not(test))]
        let (configuration, config_error) = match crate::agent_config::ConfigLibrary::load() {
            Ok(configuration) => (configuration, None),
            Err(error) => (crate::agent_config::ConfigLibrary::default(), Some(error)),
        };
        #[cfg(test)]
        let (mut history, history_error) = (crate::agent_history::History::default(), None);
        #[cfg(not(test))]
        let (mut history, history_error) = match crate::agent_history::History::load() {
            Ok(history) => (history, None),
            Err(error) => (crate::agent_history::History::default(), Some(error)),
        };
        if history.active().is_none() {
            history.new_chat();
        }
        let session = history
            .active()
            .map(Session::from_conversation)
            .unwrap_or_default();
        let goal = history
            .active()
            .map(|c| c.draft.clone())
            .unwrap_or_default();
        let history_saved = serde_json::to_string(&history).unwrap_or_default();
        Self {
            open: false,
            session,
            goal,
            configuration,
            config_error,
            history,
            history_error,
            history_saved,
            catalog_pending: Vec::new(),
            catalog_active: None,
            catalog_token: 0,
            catalog_mailbox: Arc::new(Mutex::new(VecDeque::new())),
            repaint_context: None,
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
        self.persist_history();
    }

    fn persist_history(&mut self) {
        if let Some(conversation) = self.history.active_mut() {
            if conversation.messages != self.session.messages
                || conversation.trace != self.session.trace
                || conversation.draft != self.goal
                || conversation.error != self.session.error
            {
                conversation.messages = self.session.messages.clone();
                conversation.trace = self.session.trace.clone();
                conversation.draft = self.goal.clone();
                conversation.error = self.session.error.clone();
                #[cfg(not(target_arch = "wasm32"))]
                {
                    conversation.updated_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis()
                        .min(u64::MAX as u128) as u64;
                }
                #[cfg(target_arch = "wasm32")]
                {
                    conversation.updated_at = js_sys::Date::now().max(0.0) as u64;
                }
            }
        }
        let Ok(snapshot) = serde_json::to_string(&self.history) else {
            self.history_error = Some("无法保存会话记录".into());
            return;
        };
        if snapshot == self.history_saved {
            return;
        }
        // Unit tests exercise in-memory conversations without touching user stores.
        #[cfg(test)]
        let save_result: Result<(), String> = Ok(());
        #[cfg(not(test))]
        let save_result = self.history.save();
        match save_result {
            Ok(()) => {
                self.history_saved = snapshot;
                self.history_error = None;
            }
            Err(error) => {
                // Retain the dirty snapshot to avoid retrying a failed disk write each frame.
                self.history_saved = snapshot;
                self.history_error = Some(error);
            }
        }
    }

    fn restore_active_conversation(&mut self) {
        let generation = self.session.generation;
        self.session = self
            .history
            .active()
            .map(Session::from_conversation)
            .unwrap_or_default();
        self.session.generation = generation;
        self.goal = self
            .history
            .active()
            .map(|c| c.draft.clone())
            .unwrap_or_default();
        self.persist_history();
    }

    fn new_conversation(&mut self) {
        self.cancel();
        self.history.new_chat();
        self.restore_active_conversation();
    }

    fn select_conversation(&mut self, id: &str) {
        if self.history.active_id.as_deref() == Some(id) {
            return;
        }
        self.cancel();
        if self.history.select(id) {
            self.restore_active_conversation();
        }
    }

    fn remove_current_conversation(&mut self) {
        self.cancel();
        if let Some(id) = self.history.active_id.clone() {
            self.history.remove(&id);
        }
        if self.history.active().is_none() {
            self.history.new_chat();
        }
        self.restore_active_conversation();
    }

    /// Start a goal using the same session machinery as the visible Send button.
    pub fn start_goal(&mut self, prompt: &str) {
        if prompt.trim().is_empty() {
            return;
        }
        self.cancel();
        if self.history.active().is_none() {
            self.history.new_chat();
        }
        if !self.session.messages.iter().any(|m| m["role"] == "user") {
            if let Some(conversation) = self.history.active_mut() {
                conversation.title = crate::agent_history::Conversation::title_from_prompt(prompt);
            }
        }
        self.session.start(prompt);
        self.goal.clear();
        self.persist_history();
    }

    /// Return observable session state without connection credentials.
    pub fn snapshot(&self) -> Value {
        json!({"status":self.session.status,"messages":self.session.messages,
            "trace":self.session.trace,"error":self.session.error,
            "rounds":self.session.rounds,"generation":self.session.generation,
            "context_compressed":self.session.context_compressed,"context_bytes":self.session.context_bytes,
            "conversation_id":self.history.active_id,"conversation_count":self.history.conversations.len()})
    }

    /// Draw the assistant inside the editor's right sidebar.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.repaint_context = Some(ui.ctx().clone());
        let mut send = false;
        let mut stop = false;
        let mut configuration_changed = false;
        ui.heading("AI 助手");
        let mut selected_chat = None;
        let mut new_chat = false;
        let mut delete_chat = false;
        ui.horizontal(|ui| {
            new_chat = ui.button("新建会话").clicked();
            delete_chat = ui.button("删除会话").clicked();
        });
        let active_title = self
            .history
            .active()
            .map(|c| c.title.as_str())
            .unwrap_or("新会话");
        ui.label("历史会话");
        egui::ComboBox::from_id_salt("agent-conversation-history")
            .selected_text(active_title)
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for conversation in self.history.conversations.iter().rev() {
                    if ui
                        .selectable_label(
                            self.history.active_id.as_deref() == Some(&conversation.id),
                            &conversation.title,
                        )
                        .clicked()
                    {
                        selected_chat = Some(conversation.id.clone());
                    }
                }
            });
        if new_chat {
            self.new_conversation();
        } else if delete_chat {
            self.remove_current_conversation();
        } else if let Some(id) = selected_chat {
            self.select_conversation(&id);
        }
        if let Some(error) = &self.history_error {
            ui.colored_label(egui::Color32::LIGHT_RED, format!("会话保存失败：{error}"));
            if ui.button("重试保存会话").clicked() {
                self.history_saved.clear();
                self.persist_history();
            }
        }
        ui.collapsing("本地模型配置", |ui| {
            ui.small("有效修改会自动保存到本机，启动时自动加载。");
            configuration_changed = self.configuration.ui(ui);
        });
        if configuration_changed {
            // Persist on the edit frame rather than on close or after a timer:
            // switching tabs or immediately exiting must not lose valid edits.
            self.config_error = self.configuration.save().err();
        }
        if let Some(error) = &self.config_error {
            ui.colored_label(egui::Color32::LIGHT_RED, format!("本地配置：{error}"));
        }
        if let Ok(selected) = self.configuration.selected() {
            ui.label(format!("当前模型：{}", selected.model));
        }
        ui.label(format!(
            "状态：{} · 已请求 {} 次",
            self.session.status, self.session.rounds
        ));
        if self.session.context_compressed {
            if let Some((before, after)) = self.session.context_bytes {
                ui.small(format!(
                    "上下文已自动压缩：{} → {} KiB，完整历史保留",
                    before / 1024,
                    after / 1024
                ));
            }
        }
        let transcript_height = (ui.available_height() - 130.0).max(100.0);
        egui::ScrollArea::vertical()
            .id_salt("agent-transcript")
            .max_height(transcript_height)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for (index, message) in self.session.messages.iter().enumerate() {
                    let role = message["role"].as_str().unwrap_or("");
                    if role == "system" {
                        continue;
                    }
                    ui.push_id(index, |ui| {
                        if role == "tool" {
                            let id = message["tool_call_id"].as_str().unwrap_or("");
                            let name = self
                                .session
                                .trace
                                .iter()
                                .find(|t| t["id"] == id)
                                .and_then(|t| t["name"].as_str())
                                .unwrap_or("工具");
                            ui.collapsing(format!("执行回执 · {name}"), |ui| {
                                ui.add(
                                    egui::Label::new(message["content"].as_str().unwrap_or(""))
                                        .wrap()
                                        .selectable(true),
                                );
                            });
                            return;
                        }
                        let fill = if role == "user" {
                            ui.visuals().faint_bg_color
                        } else {
                            ui.visuals().extreme_bg_color
                        };
                        egui::Frame::new()
                            .fill(fill)
                            .inner_margin(8.0)
                            .show(ui, |ui| {
                                ui.set_max_width(ui.available_width());
                                ui.strong(if role == "user" { "你" } else { "AI 助手" });
                                if let Some(text) =
                                    message["content"].as_str().filter(|s| !s.is_empty())
                                {
                                    ui.add(egui::Label::new(text).wrap().selectable(true));
                                }
                                if let Some(calls) = message["tool_calls"].as_array() {
                                    for call in calls {
                                        let name =
                                            call["function"]["name"].as_str().unwrap_or("工具");
                                        ui.collapsing(format!("调用 · {name}"), |ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    call["function"]["arguments"]
                                                        .as_str()
                                                        .unwrap_or(""),
                                                )
                                                .wrap()
                                                .selectable(true),
                                            );
                                        });
                                    }
                                }
                            });
                        ui.add_space(6.0);
                    });
                }
                if self.session.messages.is_empty() {
                    ui.weak("开始新对话，或从历史会话继续。");
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
        if stop || configuration_changed {
            self.cancel();
        }
        if send {
            let goal = self.goal.clone();
            self.start_goal(&goal);
        }
        self.persist_history();
        self.collect_catalog_requests();
        if matches!(self.session.status, "requesting" | "tools") || self.catalog_busy() {
            ui.ctx().request_repaint();
        }
    }

    /// Advance at most one editor tool per frame; network work stays off thread.
    pub fn poll(&mut self, engine: &mut Engine, doc: &mut Doc) {
        self.poll_session(engine, doc);
        self.persist_history();
    }

    fn poll_session(&mut self, engine: &mut Engine, doc: &mut Doc) {
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
        self.collect_catalog_requests();
        self.receive_catalog_completions();
        let catalog_launched = self.launch_catalog();
        if self.catalog_busy() {
            if let Some(ctx) = &self.repaint_context {
                ctx.request_repaint();
            }
        }
        if !self.session.pending.is_empty() {
            self.session
                .tool_tick(|name, args| crate::commands::dispatch(engine, doc, name, args));
            return;
        }
        if self.session.status == "requesting" && !self.session.inflight && !catalog_launched {
            #[cfg(not(target_arch = "wasm32"))]
            if self.workers.len() >= 2 {
                return;
            }
            self.session.rounds = self.session.rounds.saturating_add(1);
            self.session.inflight = true;
            self.launch();
        }
    }

    fn catalog_busy(&self) -> bool {
        self.catalog_active.is_some() || !self.catalog_pending.is_empty()
    }

    fn collect_catalog_requests(&mut self) {
        for provider in self.configuration.take_catalog_requests() {
            if self.catalog_pending.contains(&provider) {
                continue;
            }
            // Editing the connection clears Loading and permits a new request;
            // retain that retry until the old connection's response is discarded.
            if self.catalog_pending.len() < MAX_CATALOG_QUEUE {
                self.catalog_pending.push(provider);
            } else {
                self.configuration
                    .receive_catalog(&provider, Err("待获取的服务商过多，请稍后重试".into()));
            }
        }
    }

    fn catalog_matches(&self, request: &CatalogRequest) -> bool {
        self.configuration
            .catalog_connection(&request.provider)
            .is_ok_and(|(url, key, _)| url == request.base_url && key == request.api_key)
    }

    fn receive_catalog_completions(&mut self) {
        let completed: Vec<_> = match self.catalog_mailbox.lock() {
            Ok(mut queue) => queue.drain(..).collect(),
            Err(_) => return,
        };
        for completion in completed {
            if self.catalog_active.as_ref() != Some(&completion.request) {
                continue;
            }
            self.catalog_active = None;
            if self.catalog_matches(&completion.request) {
                self.configuration
                    .receive_catalog(&completion.request.provider, completion.result);
            }
        }
    }

    fn launch_catalog(&mut self) -> bool {
        if self.catalog_active.is_some() || self.catalog_pending.is_empty() {
            return false;
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.workers.len() >= 2 {
            return false;
        }
        let provider = self.catalog_pending.remove(0);
        let (base_url, api_key, timeout_secs) =
            match self.configuration.catalog_connection(&provider) {
                Ok(connection) => connection,
                Err(_) => {
                    // A removed provider must not be recreated by a late error.
                    if self
                        .configuration
                        .providers
                        .iter()
                        .any(|p| p.id == provider)
                    {
                        self.configuration
                            .receive_catalog(&provider, Err("服务商连接配置无效".into()));
                    }
                    return false;
                }
            };
        self.catalog_token = self.catalog_token.wrapping_add(1);
        let request = CatalogRequest {
            token: self.catalog_token,
            provider,
            base_url,
            api_key,
        };
        self.configuration.catalog_loading(&request.provider);
        self.catalog_active = Some(request.clone());
        let mailbox = self.catalog_mailbox.clone();
        let repaint = self.repaint_context.clone();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let worker = std::thread::spawn(move || {
                let url = models_url(&request.base_url);
                let mut headers = vec![("Accept".into(), "application/json".into())];
                if let Some(key) = &request.api_key {
                    headers.push(("Authorization".into(), format!("Bearer {key}")));
                }
                let result = quench3d_cli::llm::http_get_json_with_curl(
                    &url,
                    &headers,
                    std::time::Duration::from_secs(timeout_secs.min(30)),
                    None,
                )
                .map_err(|_| "获取模型列表失败，请检查地址、密钥和网络".to_owned())
                .and_then(|text| parse_model_ids(&text));
                if let Ok(mut queue) = mailbox.lock() {
                    if queue.len() < MAX_CATALOG_QUEUE {
                        queue.push_back(CatalogCompletion { request, result });
                    }
                }
                if let Some(ctx) = repaint {
                    ctx.request_repaint();
                }
            });
            self.workers.push(worker);
        }
        #[cfg(target_arch = "wasm32")]
        {
            let body = json!({"connection":{"base_url":request.base_url,"api_key":request.api_key.as_deref().unwrap_or(""),"timeout_secs":timeout_secs.min(30)}}).to_string();
            wasm_bindgen_futures::spawn_local(async move {
                let result = fetch_models(&body).await;
                if let Ok(mut queue) = mailbox.lock() {
                    if queue.len() < MAX_CATALOG_QUEUE {
                        queue.push_back(CatalogCompletion { request, result });
                    }
                }
                if let Some(ctx) = repaint {
                    ctx.request_repaint();
                }
            });
        }
        true
    }

    fn request_context(&mut self) -> crate::agent_context::Context {
        let context = crate::agent_context::compact(&self.session.messages);
        self.session.context_bytes = Some((context.original_bytes, context.sent_bytes));
        self.session.context_compressed = context.compressed;
        context
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
        let context = self.request_context();
        let messages: Vec<_> = context
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
        let context = self.request_context();
        let body =
            browser_request(&context.messages, crate::commands::tools(), &config).to_string();
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

#[cfg(any(not(target_arch = "wasm32"), test))]
fn models_url(base: &str) -> String {
    let mut url = base.trim_end_matches('/').to_owned();
    if url.ends_with("/chat/completions") {
        url.truncate(url.len() - "/chat/completions".len());
    }
    if !url.ends_with("/models") {
        url.push_str("/models");
    }
    url
}
#[cfg(any(not(target_arch = "wasm32"), test))]
fn parse_model_ids(text: &str) -> Result<Vec<String>, String> {
    if text.len() > MAX_CATALOG_BYTES {
        return Err("模型列表超过 1 MiB 限制".into());
    }
    let value: Value = serde_json::from_str(text).map_err(|_| "模型列表格式无效")?;
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or("模型列表格式无效")?;
    validated_model_ids(
        data.iter()
            .map(|item| item.get("id").and_then(Value::as_str)),
    )
}

fn validated_model_ids<'a>(
    items: impl Iterator<Item = Option<&'a str>>,
) -> Result<Vec<String>, String> {
    let mut ids = std::collections::BTreeSet::new();
    for (index, item) in items.enumerate() {
        if index >= MAX_CATALOG_MODELS {
            return Err("模型数量超过 2048 限制".into());
        }
        let id = item.ok_or("模型列表格式无效")?;
        if id.trim().is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
            return Err("模型列表包含无效模型 ID".into());
        }
        ids.insert(id.to_owned());
    }
    Ok(ids.into_iter().collect())
}

#[cfg(target_arch = "wasm32")]
async fn fetch_models(body: &str) -> Result<Vec<String>, String> {
    use wasm_bindgen::{JsCast, JsValue};
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    init.set_mode(web_sys::RequestMode::SameOrigin);
    init.set_body(&JsValue::from_str(body));
    let request = web_sys::Request::new_with_str_and_init("/api/agent/models", &init)
        .map_err(|_| "无法创建模型列表请求".to_owned())?;
    request
        .headers()
        .set("Content-Type", "application/json")
        .map_err(|_| "无法设置请求头".to_owned())?;
    let window = web_sys::window().ok_or_else(|| "浏览器不可用".to_owned())?;
    let value = wasm_bindgen_futures::JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|_| "模型列表请求失败".to_owned())?;
    let response: web_sys::Response = value.dyn_into().map_err(|_| "代理响应无效".to_owned())?;
    if !response.ok() {
        return Err(format!("模型代理 HTTP {}", response.status()));
    }
    if response
        .headers()
        .get("Content-Length")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|size| size > MAX_CATALOG_BYTES)
    {
        return Err("模型列表超过 1 MiB 限制".into());
    }
    let text = wasm_bindgen_futures::JsFuture::from(
        response.text().map_err(|_| "读取模型列表失败".to_owned())?,
    )
    .await
    .map_err(|_| "读取模型列表失败".to_owned())?
    .as_string()
    .ok_or_else(|| "模型列表响应无效".to_owned())?;
    if text.len() > MAX_CATALOG_BYTES {
        return Err("模型列表超过 1 MiB 限制".into());
    }
    let value: Value = serde_json::from_str(&text).map_err(|_| "模型列表响应无效".to_owned())?;
    let models = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| "模型列表响应无效".to_owned())?;
    validated_model_ids(models.iter().map(Value::as_str))
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
    context_compressed: bool,
    context_bytes: Option<(usize, usize)>,
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
            context_compressed: false,
            context_bytes: None,
        }
    }
}
impl Session {
    fn from_conversation(conversation: &crate::agent_history::Conversation) -> Self {
        let mut session = Self {
            messages: conversation.messages.clone(),
            trace: conversation.trace.clone(),
            error: conversation.error.clone(),
            ..Self::default()
        };
        // A transcript is data, never a queue of work to resume after reload.
        session.close_unresolved_tools();
        session.status = if session.messages.is_empty() {
            "idle"
        } else {
            "ready"
        };
        session
    }

    fn close_unresolved_tools(&mut self) {
        let mut unresolved: Vec<(String, String)> = Vec::new();
        for message in &self.messages {
            if message["role"] == "assistant" {
                if let Some(calls) = message["tool_calls"].as_array() {
                    for call in calls {
                        if let Some(id) = call["id"].as_str() {
                            unresolved.push((
                                id.to_owned(),
                                call["function"]["name"]
                                    .as_str()
                                    .unwrap_or("tool")
                                    .to_owned(),
                            ));
                        }
                    }
                }
            } else if message["role"] == "tool" {
                if let Some(id) = message["tool_call_id"].as_str() {
                    unresolved.retain(|(pending, _)| pending != id);
                }
            }
        }
        for (id, name) in unresolved {
            let result = json!({"ok":false,"error":"cancelled","message":"This tool call was cancelled; no execution was resumed."});
            self.messages
                .push(json!({"role":"tool","tool_call_id":id,"content":result.to_string()}));
            self.trace
                .push(json!({"id":id,"name":name,"result":result}));
        }
    }

    fn start(&mut self, prompt: &str) {
        self.cancel();
        self.error = None;
        self.rounds = 0;
        if prompt.trim().is_empty() {
            self.fail("Goal must not be empty".into());
            return;
        }
        if self.messages.is_empty() {
            self.messages
                .push(json!({"role":"system","content":SYSTEM}));
        }
        self.messages.push(json!({"role":"user","content":prompt}));
        self.status = "requesting";
    }
    fn fail(&mut self, error: String) {
        self.error = Some(error);
        self.status = "error";
        self.inflight = false;
        self.pending.clear();
        self.close_unresolved_tools();
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
        self.close_unresolved_tools();
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
    fn catalog_parser_bounds_and_validates_every_id() {
        assert_eq!(
            parse_model_ids(r#"{"data":[{"id":"z"},{"id":"a"},{"id":"z"}]}"#).unwrap(),
            vec!["a", "z"]
        );
        for invalid in [
            r#"{"data":[{"id":"bad\nmodel"}]}"#,
            r#"{"data":[{"id":7}]}"#,
            r#"{"data":[{"id":" "}]}"#,
        ] {
            assert!(parse_model_ids(invalid).is_err());
        }
        let excessive =
            json!({"data": vec![json!({"id":"same"}); MAX_CATALOG_MODELS + 1]}).to_string();
        assert!(parse_model_ids(&excessive).is_err());
        assert!(parse_model_ids(&" ".repeat(MAX_CATALOG_BYTES + 1)).is_err());
    }

    #[test]
    fn catalog_endpoint_handles_chat_base_and_complete_url() {
        assert_eq!(
            models_url("http://localhost:11434/v1/"),
            "http://localhost:11434/v1/models"
        );
        assert_eq!(
            models_url("https://example.test/v1/chat/completions/"),
            "https://example.test/v1/models"
        );
        assert_eq!(
            models_url("https://example.test/v1/models"),
            "https://example.test/v1/models"
        );
    }

    #[test]
    fn catalog_snapshot_rejects_changed_credentials_endpoint_and_deleted_provider() {
        let mut panel = AgentPanel {
            configuration: crate::agent_config::ConfigLibrary::default(),
            ..AgentPanel::default()
        };
        let provider = panel.configuration.providers[0].id.clone();
        let (base_url, api_key, _) = panel.configuration.catalog_connection(&provider).unwrap();
        let request = CatalogRequest {
            token: 4,
            provider,
            base_url,
            api_key,
        };
        assert!(panel.catalog_matches(&request));
        panel.configuration.providers[0].api_key = Some("new-secret".into());
        assert!(!panel.catalog_matches(&request));
        panel.configuration.providers[0].api_key = request.api_key.clone();
        panel.configuration.providers[0]
            .base_url
            .push_str("/changed");
        assert!(!panel.catalog_matches(&request));
        panel.configuration.providers.remove(0);
        assert!(!panel.catalog_matches(&request));
    }

    #[test]
    fn catalog_completion_token_is_separate_from_chat_generation() {
        let mut panel = AgentPanel {
            configuration: crate::agent_config::ConfigLibrary::default(),
            ..AgentPanel::default()
        };
        let provider = panel.configuration.providers[0].id.clone();
        let (base_url, api_key, _) = panel.configuration.catalog_connection(&provider).unwrap();
        let request = CatalogRequest {
            token: 4,
            provider,
            base_url,
            api_key,
        };
        panel.catalog_active = Some(request.clone());
        let mut stale = request.clone();
        stale.token = 3;
        panel
            .catalog_mailbox
            .lock()
            .unwrap()
            .push_back(CatalogCompletion {
                request: stale,
                result: Err("stale".into()),
            });
        let generation = panel.session.generation;
        panel.receive_catalog_completions();
        assert!(panel.catalog_active.is_some());
        panel.configuration.providers[0]
            .base_url
            .push_str("/changed");
        panel
            .catalog_mailbox
            .lock()
            .unwrap()
            .push_back(CatalogCompletion {
                request,
                result: Ok(vec!["old-endpoint-model".into()]),
            });
        panel.receive_catalog_completions();
        assert!(panel.catalog_active.is_none());
        assert_eq!(panel.session.generation, generation);
        assert!(panel.session.error.is_none());
        assert!(panel.mailbox.lock().unwrap().is_empty());
    }

    #[test]
    fn followup_keeps_transcript_and_resets_per_turn_rounds() {
        let mut session = Session::default();
        session.start("first request");
        let first_generation = session.generation;
        session
            .accept(
                first_generation,
                json!({"choices":[{"message":{"role":"assistant","content":"first answer"}}]}),
            )
            .unwrap();
        session.rounds = 5;
        session.start("followup request");
        assert_eq!(session.messages.len(), 4);
        assert_eq!(session.messages[0]["role"], "system");
        assert_eq!(session.messages[1]["content"], "first request");
        assert_eq!(session.messages[2]["content"], "first answer");
        assert_eq!(session.messages[3]["content"], "followup request");
        assert_eq!(session.rounds, 0);
        assert!(!session.inflight);
        assert_ne!(session.generation, first_generation);
    }

    #[test]
    fn restored_tool_transcript_never_executes_and_can_continue() {
        let mut history = crate::agent_history::History::default();
        history.new_chat();
        let conversation = history.active_mut().unwrap();
        conversation.messages = vec![
            json!({"role":"user","content":"inspect"}),
            response("{}")["choices"][0]["message"].clone(),
        ];
        let mut session = Session::from_conversation(conversation);
        assert_eq!(session.status, "ready");
        assert!(!session.inflight);
        assert!(session.pending.is_empty());
        assert_eq!(session.messages[2]["tool_call_id"], "call-real-17");
        let receipt: Value =
            serde_json::from_str(session.messages[2]["content"].as_str().unwrap()).unwrap();
        assert_eq!(receipt["error"], "cancelled");
        session.start("continue");
        assert_eq!(session.messages.len(), 4);
        assert_eq!(session.messages[3]["content"], "continue");
    }

    #[test]
    fn switching_conversations_isolates_late_responses_and_restores_draft() {
        let mut panel = AgentPanel::default();
        let first = panel.history.active_id.clone().unwrap();
        panel.start_goal("first conversation");
        let old_generation = panel.session.generation;
        panel.goal = "unfinished draft".into();
        panel.new_conversation();
        assert!(panel.session.messages.is_empty());
        assert!(!panel.session.inflight);
        panel
            .session
            .accept(old_generation, response("{}"))
            .unwrap();
        assert!(panel.session.messages.is_empty());
        panel.start_goal("second conversation");
        panel.select_conversation(&first);
        assert_eq!(panel.goal, "unfinished draft");
        assert_eq!(
            panel.session.messages.last().unwrap()["content"],
            "first conversation"
        );
        assert_eq!(panel.session.status, "ready");
        assert!(panel.session.pending.is_empty());
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
        assert_eq!(panel.snapshot()["messages"].as_array().unwrap().len(), 4);
        assert_eq!(panel.session.messages[2]["tool_call_id"], "second");
        assert_eq!(panel.session.messages[3]["content"], "new goal");
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
