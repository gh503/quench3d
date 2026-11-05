//! Local, versioned AI conversation history, independent of model connections.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

const VERSION: u32 = 1;
const MAX_BYTES: usize = 16 * 1024 * 1024;
#[cfg(target_arch = "wasm32")]
const WEB_KEY: &str = "quench3d.ai.history.v1";
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// Persisted conversations; deliberately does not implement `Debug`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct History {
    pub(crate) version: u32,
    pub(crate) active_id: Option<String>,
    pub(crate) conversations: Vec<Conversation>,
}

/// One conversation's transcript and draft; no provider configuration is stored.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Conversation {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) updated_at: u64,
    pub(crate) messages: Vec<Value>,
    pub(crate) trace: Vec<Value>,
    pub(crate) draft: String,
    pub(crate) error: Option<String>,
}

impl Default for History {
    fn default() -> Self {
        Self {
            version: VERSION,
            active_id: None,
            conversations: Vec::new(),
        }
    }
}

impl Conversation {
    /// Derive a short Unicode-safe title from the first prompt.
    pub(crate) fn title_from_prompt(prompt: &str) -> String {
        let title: String = prompt
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(30)
            .collect();
        if title.is_empty() {
            "新会话".into()
        } else {
            title
        }
    }
}

fn now_millis() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64
    }
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now().max(0.0) as u64
    }
}

impl History {
    /// Create and select a new empty conversation.
    pub(crate) fn new_chat(&mut self) -> String {
        let updated_at = now_millis();
        let id = loop {
            let id = format!(
                "chat-{updated_at}-{}",
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            );
            if !self.conversations.iter().any(|c| c.id == id) {
                break id;
            }
        };
        self.conversations.push(Conversation {
            id: id.clone(),
            title: "新会话".into(),
            updated_at,
            messages: Vec::new(),
            trace: Vec::new(),
            draft: String::new(),
            error: None,
        });
        self.active_id = Some(id.clone());
        id
    }

    /// Return the currently selected conversation.
    pub(crate) fn active(&self) -> Option<&Conversation> {
        self.active_id
            .as_ref()
            .and_then(|id| self.conversations.iter().find(|c| &c.id == id))
    }

    /// Mutably access the currently selected conversation.
    pub(crate) fn active_mut(&mut self) -> Option<&mut Conversation> {
        let id = self.active_id.as_ref()?;
        self.conversations.iter_mut().find(|c| &c.id == id)
    }

    /// Select an existing conversation, leaving the current selection intact on failure.
    pub(crate) fn select(&mut self, id: &str) -> bool {
        if self.conversations.iter().any(|c| c.id == id) {
            self.active_id = Some(id.into());
            true
        } else {
            false
        }
    }

    /// Delete a conversation, selecting the most recently updated remaining one.
    pub(crate) fn remove(&mut self, id: &str) {
        if !self.conversations.iter().any(|c| c.id == id) {
            return;
        }
        self.conversations.retain(|c| c.id != id);
        self.active_id = self
            .conversations
            .iter()
            .max_by_key(|c| c.updated_at)
            .map(|c| c.id.clone());
    }

    fn validate(&self) -> Result<(), String> {
        if self.version != VERSION {
            return Err("不支持的 AI 历史版本".into());
        }
        let mut ids = HashSet::new();
        for c in &self.conversations {
            if c.id.is_empty()
                || c.id.len() > 256
                || c.id.chars().any(char::is_control)
                || !ids.insert(c.id.as_str())
            {
                return Err("AI 历史会话 ID 无效或重复".into());
            }
            for message in &c.messages {
                let m = message.as_object().ok_or("AI 历史消息格式无效")?;
                let role = m
                    .get("role")
                    .and_then(Value::as_str)
                    .ok_or("AI 历史消息缺少角色")?;
                if !matches!(role, "system" | "user" | "assistant" | "tool") {
                    return Err("AI 历史消息角色无效".into());
                }
                let has_calls = m
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .is_some_and(|calls| !calls.is_empty());
                if !matches!(m.get("content"), Some(Value::String(_) | Value::Null))
                    && !(role == "assistant" && m.get("content").is_none() && has_calls)
                {
                    return Err("AI 历史消息内容无效".into());
                }
                if role == "tool"
                    && !m
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .is_some_and(|s| !s.is_empty())
                {
                    return Err("AI 历史工具消息缺少调用 ID".into());
                }
                if let Some(calls) = m.get("tool_calls").filter(|v| !v.is_null()) {
                    if role != "assistant" {
                        return Err("AI 历史工具调用角色无效".into());
                    }
                    for call in calls.as_array().ok_or("AI 历史工具调用无效")? {
                        if !call
                            .get("id")
                            .and_then(Value::as_str)
                            .is_some_and(|s| !s.is_empty())
                            || call.get("type").and_then(Value::as_str) != Some("function")
                            || !call
                                .pointer("/function/name")
                                .and_then(Value::as_str)
                                .is_some_and(|s| !s.is_empty())
                            || call
                                .pointer("/function/arguments")
                                .and_then(Value::as_str)
                                .is_none()
                        {
                            return Err("AI 历史工具调用无效".into());
                        }
                    }
                }
                if contains_credentials(message) {
                    return Err("AI 历史不能包含连接凭据".into());
                }
            }
            if c.trace.iter().any(contains_credentials) {
                return Err("AI 历史不能包含连接凭据".into());
            }
        }
        if self
            .active_id
            .as_ref()
            .is_some_and(|id| !ids.contains(id.as_str()))
        {
            return Err("AI 历史当前会话不存在".into());
        }
        Ok(())
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("AI 历史超过 16 MiB 上限".into());
        }
        let value: Self =
            serde_json::from_slice(bytes).map_err(|_| "AI 历史格式无效".to_owned())?;
        value.validate()?;
        Ok(value)
    }

    fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let mut writer = LimitedWriter(Vec::new());
        serde_json::to_writer(&mut writer, self)
            .map_err(|_| "AI 历史无法保存或超过 16 MiB 上限".to_owned())?;
        Ok(writer.0)
    }

    /// Load local history; an absent store produces an empty library.
    #[cfg(not(test))]
    pub(crate) fn load() -> Result<Self, String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self::load_from(
                &crate::agent_config::ConfigLibrary::default_path()
                    .with_file_name("ai-history.json"),
            )
        }
        #[cfg(target_arch = "wasm32")]
        {
            let storage = web_sys::window()
                .and_then(|w| w.local_storage().ok().flatten())
                .ok_or("localStorage 不可用")?;
            match storage.get_item(WEB_KEY).map_err(|_| "读取 AI 历史失败")? {
                Some(text) => Self::decode(text.as_bytes()),
                None => Ok(Self::default()),
            }
        }
    }

    /// Atomically save bounded local history independently of provider settings.
    #[cfg(not(test))]
    pub(crate) fn save(&self) -> Result<(), String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.save_to(
                &crate::agent_config::ConfigLibrary::default_path()
                    .with_file_name("ai-history.json"),
            )
        }
        #[cfg(target_arch = "wasm32")]
        {
            let bytes = self.encode()?;
            let text = std::str::from_utf8(&bytes).map_err(|_| "AI 历史编码无效")?;
            let storage = web_sys::window()
                .and_then(|w| w.local_storage().ok().flatten())
                .ok_or("localStorage 不可用")?;
            storage
                .set_item(WEB_KEY, text)
                .map_err(|_| "保存 AI 历史失败".into())
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn load_from(path: &std::path::Path) -> Result<Self, String> {
        use std::io::Read;
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(_) => return Err("读取 AI 历史失败".into()),
        };
        if file.metadata().map_err(|_| "读取 AI 历史大小失败")?.len() > MAX_BYTES as u64 {
            return Err("AI 历史超过 16 MiB 上限".into());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "读取 AI 历史失败")?;
        Self::decode(&bytes)
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        let bytes = self.encode()?;
        let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(parent).map_err(|_| "创建 AI 历史目录失败")?;
        let mut temporary = None;
        for _ in 0..100 {
            let candidate = parent.join(format!(
                ".ai-history-{}-{}.tmp",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&candidate) {
                Ok(file) => {
                    temporary = Some((candidate, file));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err("创建 AI 历史临时文件失败".into()),
            }
        }
        let (temp_path, mut file) = temporary.ok_or("无法创建 AI 历史临时文件")?;
        let result = (|| {
            file.write_all(&bytes).map_err(|_| "写入 AI 历史失败")?;
            file.sync_all().map_err(|_| "同步 AI 历史失败")?;
            drop(file);
            std::fs::rename(&temp_path, path).map_err(|_| "替换 AI 历史失败")?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp_path);
        }
        result
    }
}

fn contains_credentials(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(k, v)| {
            matches!(
                k.to_ascii_lowercase().as_str(),
                "api_key" | "authorization" | "provider_key" | "api-key"
            ) || contains_credentials(v)
        }),
        Value::Array(values) => values.iter().any(contains_credentials),
        _ => false,
    }
}

struct LimitedWriter(Vec<u8>);
impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("history size limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unique_chats_selection_deletion_and_unicode_titles() {
        let mut h = History::default();
        assert!(h.active().is_none());
        let a = h.new_chat();
        let b = h.new_chat();
        assert_ne!(a, b);
        assert!(h.select(&a));
        h.active_mut().unwrap().updated_at = u64::MAX;
        assert!(!h.select("missing"));
        h.remove(&b);
        assert_eq!(h.active().unwrap().id, a);
        h.remove(&a);
        assert!(h.active().is_none());
        assert_eq!(Conversation::title_from_prompt(" \n "), "新会话");
        assert_eq!(
            Conversation::title_from_prompt(&"碰".repeat(40))
                .chars()
                .count(),
            30
        );
    }

    #[test]
    fn invalid_histories_and_credentials_are_rejected_without_echoing_input() {
        let mut h = History::default();
        h.new_chat();
        h.active_mut()
            .unwrap()
            .messages
            .push(json!({"role":"user", "content":"secret-prompt"}));
        assert!(h.encode().is_ok());
        let duplicate = h.conversations[0].clone();
        h.conversations.push(duplicate);
        assert!(h.encode().is_err());
        h.conversations.pop();
        h.active_mut()
            .unwrap()
            .messages
            .push(json!({"role":"tool","content":"x"}));
        assert!(h.encode().is_err());
        h.active_mut().unwrap().messages.pop();
        h.active_mut()
            .unwrap()
            .trace
            .push(json!({"api_key":"secret-key"}));
        let error = h.encode().err().unwrap();
        assert!(!error.contains("secret"));
        assert!(History::decode(br#"{"version":2,"active_id":null,"conversations":[]}"#).is_err());
        assert!(History::decode(b"private invalid json").is_err());
        let mut valid = History::default();
        valid.new_chat();
        valid.active_mut().unwrap().messages.push(json!({"role":"assistant","tool_calls":[{"id":"call","type":"function","function":{"name":"inspect_scene","arguments":"{}"}}],"reasoning_content":"provider metadata"}));
        assert!(valid.encode().is_ok());
        valid
            .active_mut()
            .unwrap()
            .messages
            .push(json!({"role":"unknown","content":"x"}));
        assert!(valid.encode().is_err());
        valid.active_mut().unwrap().messages.pop();
        valid.active_id = Some("missing".into());
        assert!(valid.encode().is_err());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn file_roundtrip_permissions_and_bounded_io() {
        let dir = std::env::temp_dir().join(format!(
            "quench-history-test-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let path = dir.join("ai-history.json");
        assert!(History::load_from(&path).unwrap().conversations.is_empty());
        let mut h = History::default();
        h.new_chat();
        let c = h.active_mut().unwrap();
        c.messages = vec![
            json!({"role":"user","content":"创建碰撞场景"}),
            json!({"role":"assistant","content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":"inspect_scene","arguments":"{}"}}]}),
            json!({"role":"tool","tool_call_id":"call-1","content":"{}"}),
        ];
        c.trace = vec![json!({"tool":"inspect_scene","ok":true})];
        c.draft = "下一条".into();
        h.save_to(&path).unwrap();
        let loaded = History::load_from(&path).unwrap();
        assert_eq!(loaded.encode().unwrap(), h.encode().unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let before = std::fs::read(&path).unwrap();
        h.active_mut().unwrap().draft = "x".repeat(MAX_BYTES);
        assert!(h.save_to(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(MAX_BYTES as u64 + 1).unwrap();
        assert!(History::load_from(&path).is_err());
        // Only the exact fixture paths allocated above are removed.
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
