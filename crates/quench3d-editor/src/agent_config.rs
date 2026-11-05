//! Persisted provider/model configuration for the editor's OpenAI-compatible client.
//!
//! The library is deliberately independent of the agent runtime: callers can load it,
//! edit it in egui, and pass [`LlmConfig`] to whichever client owns the wire protocol.
//! API keys are persisted only when the user explicitly saves this library. They are
//! omitted from `Debug` output.

use serde::{Deserialize, Serialize};
use std::fmt;

const CONFIG_VERSION: u32 = 1;
#[cfg(target_arch = "wasm32")]
const WEB_KEY: &str = "quench3d.ai.config.v1";

/// One OpenAI-compatible provider route.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Stable identifier used by model references.
    pub id: String,
    /// Display name shown in the editor.
    pub name: String,
    /// OpenAI-compatible HTTP endpoint.
    pub base_url: String,
    /// Optional credential for this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// A model and its request controls, belonging to a provider route.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Stable identifier used by selection and persistence.
    pub id: String,
    /// Display name shown in the editor.
    pub name: String,
    /// Identifier of the provider used for this model.
    pub provider_id: String,
    /// Provider-specific model name sent to the API.
    pub model: String,
    /// Sampling temperature, from 0 through 2.
    pub temperature: f32,
    /// Maximum generated tokens, from 1 through 1,048,576.
    pub max_tokens: u32,
    /// Request timeout in seconds, from 1 through 300.
    pub timeout_secs: u64,
}

/// Concrete request configuration consumed by an OpenAI-compatible client.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmConfig {
    /// OpenAI-compatible HTTP endpoint.
    pub base_url: String,
    /// Provider-specific model name.
    pub model: String,
    /// Optional provider credential.
    pub api_key: Option<String>,
    /// Sampling temperature.
    pub temperature: f32,
    /// Maximum generated tokens.
    pub max_tokens: u32,
    /// Request timeout in seconds.
    pub timeout_secs: u64,
}

impl fmt::Debug for LlmConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LlmConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("temperature", &self.temperature)
            .field("max_tokens", &self.max_tokens)
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

/// Persisted editor AI configuration. `selected_model` is a model id.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigLibrary {
    /// Schema version for persisted configuration.
    pub version: u32,
    /// Configured provider routes.
    pub providers: Vec<ProviderConfig>,
    /// Configured models and their provider assignments.
    pub models: Vec<ModelConfig>,
    /// Id of the model currently selected in the editor.
    pub selected_model: Option<String>,
    #[serde(skip)]
    last_error: Option<String>,
}

impl fmt::Debug for ConfigLibrary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfigLibrary")
            .field("version", &self.version)
            .field("providers", &self.providers)
            .field("models", &self.models)
            .field("selected_model", &self.selected_model)
            .finish()
    }
}

impl Default for ConfigLibrary {
    fn default() -> Self {
        let providers = vec![
            ProviderConfig {
                id: "ollama".into(),
                name: "Ollama（本地）".into(),
                base_url: "http://127.0.0.1:11434/v1".into(),
                api_key: None,
            },
            ProviderConfig {
                id: "openai-compatible".into(),
                name: "OpenAI 兼容服务".into(),
                base_url: "https://api.openai.com/v1".into(),
                api_key: None,
            },
        ];
        let models = vec![
            ModelConfig {
                id: "ollama-qwen2.5:7b".into(),
                name: "Qwen 2.5 7B（Ollama）".into(),
                provider_id: "ollama".into(),
                model: "qwen2.5:7b".into(),
                temperature: 0.2,
                max_tokens: 2048,
                timeout_secs: 180,
            },
            ModelConfig {
                id: "openai-gpt-4o-mini".into(),
                name: "GPT-4o mini".into(),
                provider_id: "openai-compatible".into(),
                model: "gpt-4o-mini".into(),
                temperature: 0.2,
                max_tokens: 2048,
                timeout_secs: 180,
            },
        ];
        Self {
            version: CONFIG_VERSION,
            providers,
            models,
            selected_model: Some("ollama-qwen2.5:7b".into()),
            last_error: None,
        }
    }
}

impl ConfigLibrary {
    /// Parse JSON and validate the result without exposing input values in errors.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let mut value: Self =
            serde_json::from_str(text).map_err(|_| "AI 配置格式无效".to_owned())?;
        value.last_error = None;
        value.validate()?;
        Ok(value)
    }

    /// Serialize without exposing secrets through logs or diagnostics.
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|_| "AI 配置无法序列化".to_owned())
    }

    /// Select and materialize a concrete LLM request configuration.
    pub fn selected(&self) -> Result<LlmConfig, String> {
        self.validate()?;
        let id = self.selected_model.as_deref().ok_or("未选择 AI 模型")?;
        let model = self
            .models
            .iter()
            .find(|m| m.id == id)
            .ok_or("找不到当前 AI 模型")?;
        let provider = self
            .providers
            .iter()
            .find(|p| p.id == model.provider_id)
            .ok_or("找不到模型对应的服务商")?;
        Ok(LlmConfig {
            base_url: provider.base_url.clone(),
            model: model.model.clone(),
            api_key: provider.api_key.clone(),
            temperature: model.temperature,
            max_tokens: model.max_tokens,
            timeout_secs: model.timeout_secs,
        })
    }

    /// Validate identifiers, provider references, URLs, and request controls.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != CONFIG_VERSION {
            return Err(format!("不支持的 AI 配置版本 {}", self.version));
        }
        if self.providers.is_empty() {
            return Err("至少需要一个 AI 服务商".into());
        }
        for p in &self.providers {
            nonempty("服务商 ID", &p.id)?;
            nonempty("服务商名称", &p.name)?;
            validate_url(&p.base_url)?;
            if p.api_key
                .as_deref()
                .is_some_and(|key| key.chars().any(|c| c.is_control()))
            {
                return Err("API key 不能包含控制字符".into());
            }
        }
        for (i, p) in self.providers.iter().enumerate() {
            if self.providers[..i].iter().any(|other| other.id == p.id) {
                return Err(format!("服务商 ID 重复 `{}`", p.id));
            }
        }
        for m in &self.models {
            nonempty("模型 ID", &m.id)?;
            nonempty("模型名称", &m.name)?;
            nonempty("模型名", &m.model)?;
            nonempty("服务商 ID", &m.provider_id)?;
            if !self.providers.iter().any(|p| p.id == m.provider_id) {
                return Err(format!("模型 `{}` 引用了不存在的服务商", m.id));
            }
            if !m.temperature.is_finite() || !(0.0..=2.0).contains(&m.temperature) {
                return Err(format!("模型 `{}` 的 temperature 必须为 0..=2", m.id));
            }
            if !(1..=1_048_576).contains(&m.max_tokens) {
                return Err(format!("模型 `{}` 的 max_tokens 必须为 1..=1048576", m.id));
            }
            if !(1..=300).contains(&m.timeout_secs) {
                return Err(format!("模型 `{}` 的 timeout_secs 必须为 1..=300", m.id));
            }
        }
        for (i, m) in self.models.iter().enumerate() {
            if self.models[..i].iter().any(|other| other.id == m.id) {
                return Err(format!("模型 ID 重复 `{}`", m.id));
            }
        }
        if let Some(selected) = &self.selected_model {
            if !self.models.iter().any(|m| &m.id == selected) {
                return Err("找不到当前 AI 模型".into());
            }
        }
        Ok(())
    }

    /// Load from the platform's persisted store.
    pub fn load() -> Result<Self, String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self::load_from(&Self::default_path())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let storage = web_sys::window()
                .and_then(|w| w.local_storage().ok().flatten())
                .ok_or("localStorage 不可用")?;
            match storage
                .get_item(WEB_KEY)
                .map_err(|_| "读取 localStorage 失败".to_owned())?
            {
                Some(text) => Self::from_json(&text),
                None => Ok(Self::default()),
            }
        }
    }

    /// Save to the platform's persisted store, returning errors for the caller to display.
    pub fn save(&self) -> Result<(), String> {
        self.validate()?;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.save_to(&Self::default_path())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let storage = web_sys::window()
                .and_then(|w| w.local_storage().ok().flatten())
                .ok_or("localStorage 不可用")?;
            storage
                .set_item(WEB_KEY, &self.to_json()?)
                .map_err(|_| "写入 localStorage 失败".to_owned())
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    /// Return the platform-specific configuration path.
    pub fn default_path() -> std::path::PathBuf {
        let root = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|p| !p.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| {
                #[cfg(target_os = "windows")]
                {
                    std::env::var_os("APPDATA").map(std::path::PathBuf::from)
                }
                #[cfg(not(target_os = "windows"))]
                {
                    std::env::var_os("HOME")
                        .map(|home| std::path::PathBuf::from(home).join(".config"))
                }
            })
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        root.join("quench3d").join("ai.json")
    }

    #[cfg(not(target_arch = "wasm32"))]
    /// Load and validate a configuration from a specific path.
    pub fn load_from(path: &std::path::Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_json(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(_) => Err("读取 AI 配置失败".into()),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    /// Validate and atomically save a configuration with owner-only permissions.
    pub fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        self.validate()?;
        let text = self.to_json()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| "创建 AI 配置目录失败".to_owned())?;
        }
        use std::io::Write;
        let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("ai.json");
        let mut temp_path = None;
        let mut file = None;
        for attempt in 0..100u32 {
            let candidate = parent.join(format!(".{name}.{}.{}.tmp", std::process::id(), attempt));
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&candidate) {
                Ok(opened) => {
                    temp_path = Some(candidate);
                    file = Some(opened);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err("创建 AI 配置临时文件失败".into()),
            }
        }
        let temp_path = temp_path.ok_or("无法创建唯一的 AI 配置临时文件")?;
        let mut file = file.expect("temporary file accompanies path");
        let result = (|| {
            file.write_all(text.as_bytes())
                .map_err(|_| "写入 AI 配置临时文件失败".to_owned())?;
            file.sync_all()
                .map_err(|_| "同步 AI 配置临时文件失败".to_owned())?;
            drop(file);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o600))
                    .map_err(|_| "保护 AI 配置失败".to_owned())?;
            }
            std::fs::rename(&temp_path, path).map_err(|_| "替换 AI 配置失败".to_owned())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                    .map_err(|_| "保护 AI 配置失败".to_owned())?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp_path);
        }
        result
    }

    /// Draw provider/model editors. Returns whether the library changed this frame.
    pub fn ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.heading("AI 服务商与模型");
        if !self.models.is_empty() {
            let selected_text = self
                .selected_model
                .as_deref()
                .and_then(|id| self.models.iter().find(|m| m.id == id))
                .map(|m| m.name.as_str())
                .unwrap_or("请选择模型");
            egui::ComboBox::from_id_salt("selected-ai-model")
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    for model in &self.models {
                        if ui
                            .selectable_label(
                                self.selected_model.as_deref() == Some(&model.id),
                                &model.name,
                            )
                            .clicked()
                        {
                            self.selected_model = Some(model.id.clone());
                            changed = true;
                        }
                    }
                });
        }
        let mut remove_provider = None;
        for provider in &mut self.providers {
            let provider_id = provider.id.clone();
            ui.push_id(&provider_id, |ui| {
                let collapse_id = ui.make_persistent_id(("provider-collapse", &provider_id));
                let mut collapse = egui::collapsing_header::CollapsingState::load_with_default_open(
                    ui.ctx(),
                    collapse_id,
                    true,
                );
                let mut provider_changed = false;
                collapse
                    .clone()
                    .show_header(ui, |ui| {
                        ui.label(format!("服务商：{}", provider.name));
                    })
                    .body(|ui| {
                        provider_changed |= ui.text_edit_singleline(&mut provider.name).changed();
                        ui.label("OpenAI 兼容地址");
                        provider_changed |=
                            ui.text_edit_singleline(&mut provider.base_url).changed();
                        ui.label("API key（可选）");
                        provider_changed |= ui
                            .add(
                                egui::TextEdit::singleline(
                                    provider.api_key.get_or_insert_default(),
                                )
                                .password(true),
                            )
                            .changed();
                        if ui.button("删除服务商").clicked() {
                            remove_provider = Some(provider_id.clone());
                        }
                    });
                if provider_changed {
                    changed = true;
                    collapse.set_open(false);
                    collapse.store(ui.ctx());
                }
            });
        }
        if let Some(provider_id) = remove_provider {
            let dependent = self
                .models
                .iter()
                .filter(|m| m.provider_id == provider_id)
                .count();
            if dependent > 0 {
                ui.colored_label(
                    egui::Color32::YELLOW,
                    format!("删除服务商将同时删除 {dependent} 个关联模型（级联删除）"),
                );
                self.models.retain(|m| m.provider_id != provider_id);
                if self
                    .selected_model
                    .as_deref()
                    .is_some_and(|id| !self.models.iter().any(|m| m.id == id))
                {
                    self.selected_model = self.models.first().map(|m| m.id.clone());
                }
            }
            self.providers.retain(|p| p.id != provider_id);
            changed = true;
        }
        if ui.button("添加服务商").clicked() {
            let id = next_id("provider", self.providers.iter().map(|p| p.id.as_str()));
            self.providers.push(ProviderConfig {
                id,
                name: "新服务商".into(),
                base_url: "http://127.0.0.1:11434/v1".into(),
                api_key: None,
            });
            changed = true;
        }
        ui.separator();
        let mut remove_model = None;
        for model in &mut self.models {
            let model_id = model.id.clone();
            ui.push_id(&model_id, |ui| {
                ui.collapsing(format!("模型：{}", model.name), |ui| {
                    changed |= ui.text_edit_singleline(&mut model.name).changed();
                    ui.label("模型对应的服务商");
                    let provider_name = self
                        .providers
                        .iter()
                        .find(|p| p.id == model.provider_id)
                        .map(|p| p.name.as_str())
                        .unwrap_or("请选择服务商");
                    egui::ComboBox::from_id_salt("model-provider")
                        .selected_text(provider_name)
                        .show_ui(ui, |ui| {
                            for provider in &self.providers {
                                if ui
                                    .selectable_label(
                                        model.provider_id == provider.id,
                                        &provider.name,
                                    )
                                    .clicked()
                                {
                                    model.provider_id = provider.id.clone();
                                    changed = true;
                                }
                            }
                        });
                    ui.label("服务商模型名");
                    changed |= ui.text_edit_singleline(&mut model.model).changed();
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut model.temperature)
                                .range(0.0..=2.0)
                                .prefix("温度 "),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut model.max_tokens)
                                .range(1..=1_048_576)
                                .prefix("最大 token 数 "),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut model.timeout_secs)
                                .range(1..=300)
                                .prefix("超时秒数 "),
                        )
                        .changed();
                    if ui.button("删除模型").clicked() {
                        remove_model = Some(model_id.clone());
                    }
                });
            });
        }
        if let Some(id) = remove_model {
            self.models.retain(|m| m.id != id);
            if self.selected_model.as_deref() == Some(&id) {
                self.selected_model = self.models.first().map(|m| m.id.clone());
            }
            changed = true;
        }
        if ui.button("添加模型").clicked() {
            let provider_id = self
                .providers
                .first()
                .map(|p| p.id.clone())
                .unwrap_or_default();
            let id = next_id("model", self.models.iter().map(|m| m.id.as_str()));
            self.models.push(ModelConfig {
                id,
                name: "新模型".into(),
                provider_id,
                model: "model-name".into(),
                temperature: 0.2,
                max_tokens: 2048,
                timeout_secs: 180,
            });
            changed = true;
        }
        if ui.button("保存到本机").clicked() {
            self.last_error = self.save().err();
            changed = true;
        }
        if let Some(error) = &self.last_error {
            ui.colored_label(egui::Color32::RED, error);
        }
        changed
    }
}

fn next_id<'a>(prefix: &str, existing: impl Iterator<Item = &'a str>) -> String {
    let existing: std::collections::HashSet<&str> = existing.collect();
    (1u32..)
        .map(|n| format!("{prefix}-{n}"))
        .find(|candidate| !existing.contains(candidate.as_str()))
        .expect("finite ID space")
}

fn nonempty(label: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("{label}不能为空"))
    } else if value.chars().any(|c| c.is_control()) {
        Err(format!("{label}包含控制字符"))
    } else {
        Ok(())
    }
}

fn validate_url(value: &str) -> Result<(), String> {
    let url = value.trim();
    if url != value || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("服务商地址不能包含空白或控制字符".into());
    }
    let (scheme, rest) = url
        .split_once("://")
        .ok_or("服务商地址必须使用 http:// 或 https://")?;
    if scheme != "http" && scheme != "https" {
        return Err("服务商地址必须使用 http:// 或 https://".into());
    }
    if rest.is_empty()
        || rest.starts_with('/')
        || rest.contains('@')
        || rest.contains('?')
        || rest.contains('#')
    {
        return Err("服务商地址必须有主机，且不能包含用户信息、查询参数或片段".into());
    }
    let authority = rest.split('/').next().unwrap_or_default();
    let (host, port) = if authority.starts_with('[') {
        let (host, suffix) = authority.split_once(']').ok_or("服务商地址主机无效")?;
        host.trim_start_matches('[')
            .parse::<std::net::Ipv6Addr>()
            .map_err(|_| "服务商 IPv6 地址无效")?;
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').ok_or("服务商地址端口无效")?)
        };
        (host, port)
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        if host.contains(':') {
            return Err("服务商地址必须有有效主机".into());
        }
        (host, Some(port))
    } else {
        (authority, None)
    };
    if host.is_empty() {
        return Err("服务商地址必须有有效主机".into());
    }
    if let Some(port) = port {
        if port.is_empty() || port.parse::<u16>().is_err() {
            return Err("服务商地址端口无效".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv6_provider_without_port_is_valid() {
        assert!(validate_url("http://[::1]/v1").is_ok());
        assert!(validate_url("http://[::1]:11434/v1").is_ok());
        assert!(validate_url("http://[invalid]/v1").is_err());
    }

    #[test]
    fn defaults_are_local_and_selectable() {
        let c = ConfigLibrary::default();
        assert_eq!(c.selected().unwrap().base_url, "http://127.0.0.1:11434/v1");
        assert!(!format!("{c:?}").contains("secret"));
    }

    #[test]
    fn model_provider_mapping_and_ranges_are_validated() {
        let mut c = ConfigLibrary::default();
        c.models[0].provider_id = "openai-compatible".into();
        assert_eq!(c.selected().unwrap().base_url, "https://api.openai.com/v1");
        c.models[0].max_tokens = 1_048_577;
        assert!(c.validate().is_err());
        c.models[0].max_tokens = 1;
        c.models[0].timeout_secs = 301;
        assert!(c.validate().is_err());
    }

    #[test]
    fn ids_reuse_only_unused_slots_after_deletion() {
        let mut c = ConfigLibrary::default();
        c.providers[0].id = "provider-1".into();
        c.providers[1].id = "provider-3".into();
        assert_eq!(
            next_id("provider", c.providers.iter().map(|p| p.id.as_str())),
            "provider-2"
        );
        c.models.remove(0);
        assert_eq!(
            next_id("model", c.models.iter().map(|m| m.id.as_str())),
            "model-1"
        );
    }

    #[test]
    fn invalid_json_does_not_echo_secret() {
        let secret = "super-secret-key";
        let error = ConfigLibrary::from_json(&format!(r#"{{"api_key": "{secret}"}}"#)).unwrap_err();
        assert!(!error.contains(secret));
    }

    #[test]
    fn rejects_unsafe_urls() {
        for url in [
            "https://u:p@example.com/v1",
            "https://example.com/v1?x=1",
            "https://example.com:bad/v1",
            "ftp://example.com",
        ] {
            let mut c = ConfigLibrary::default();
            c.providers[0].base_url = url.into();
            assert!(c.validate().is_err());
        }
    }

    #[test]
    fn json_roundtrip_and_redaction() {
        let mut c = ConfigLibrary::default();
        c.providers[0].api_key = Some("secret-value".into());
        let text = c.to_json().unwrap();
        assert!(text.contains("secret-value"));
        let back = ConfigLibrary::from_json(&text).unwrap();
        assert_eq!(back, c);
        assert!(!format!("{c:?}").contains("secret-value"));
        assert!(!format!("{:?}", c.selected().unwrap()).contains("secret-value"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn file_roundtrip_uses_restricted_mode() {
        let path =
            std::env::temp_dir().join(format!("quench3d-ai-{}-test.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        ConfigLibrary::default().save_to(&path).unwrap();
        assert_eq!(
            ConfigLibrary::load_from(&path).unwrap(),
            ConfigLibrary::default()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_file(path);
    }
}
