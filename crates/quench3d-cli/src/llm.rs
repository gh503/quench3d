//! 大模型接入：OpenAI 兼容的 `/v1/chat/completions` 协议。
//!
//! 兼容所有「本地优先」的推理服务，因为它们都实现了同一套协议：
//!
//! | 服务 | 默认地址 | 启动命令示例 |
//! | --- | --- | --- |
//! | Ollama | `http://localhost:11434/v1` | `ollama serve` |
//! | llama.cpp server | `http://localhost:8080/v1` | `llama-server -m model.gguf` |
//! | LM Studio | `http://localhost:1234/v1` | 打开 GUI 的 Local Server |
//! | vLLM | `http://localhost:8000/v1` | `vllm serve <model>` |
//! | 任意云服务 | `https://api.example.com/v1` | 需要 `QUENCH3D_LLM_API_KEY` |
//!
//! 之所以自己写 HTTP 客户端：`http://` 只需 `std::net::TcpStream`（零依赖），
//! `https://` 则交给系统自带的 `curl`（Windows 10+ / macOS / 主流 Linux 都自带），
//! 这样 CLI 不需要背上 TLS 依赖树。若环境里没有 curl，用本地 http 服务即可。
//!
//! **API key 不进命令行参数**：`https` 路径把 url 与 `Authorization` 头写进一个
//! 0600 权限的临时配置文件，用 `curl -K <文件>` 读入，请求体照旧走 stdin。
//! 命令行参数同机可读（`/proc/<pid>/cmdline`、`ps`、审计日志），密钥绝不能放在那里；
//! 配置文件只有同一个用户能读，且请求一结束就删除。

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

/// LLM 相关错误。
#[derive(Debug)]
pub struct LlmError {
    /// 类别：`config` / `network` / `http` / `parse`。
    pub kind: &'static str,
    /// 说明。
    pub message: String,
    /// 修复建议。
    pub hint: Option<String>,
}

impl LlmError {
    /// 造一个配置类错误。
    pub fn config(message: impl Into<String>) -> Self {
        Self {
            kind: "config",
            message: message.into(),
            hint: None,
        }
    }

    fn network(message: impl Into<String>) -> Self {
        Self {
            kind: "network",
            message: message.into(),
            hint: None,
        }
    }

    fn http(message: impl Into<String>) -> Self {
        Self {
            kind: "http",
            message: message.into(),
            hint: None,
        }
    }

    fn parse(message: impl Into<String>) -> Self {
        Self {
            kind: "parse",
            message: message.into(),
            hint: None,
        }
    }

    fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for LlmError {}

/// 连接配置。
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// 基础地址，例如 `http://localhost:11434/v1`。
    pub base_url: String,
    /// 模型名，例如 `qwen2.5:7b`。
    pub model: String,
    /// API Key（本地服务通常不需要）。
    pub api_key: Option<String>,
    /// 采样温度。
    pub temperature: f32,
    /// 最大输出 token。
    pub max_tokens: u32,
    /// 超时（秒）。
    pub timeout_secs: u64,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            base_url: "http://localhost:11434/v1".into(),
            model: "qwen2.5:7b".into(),
            api_key: None,
            temperature: 0.2,
            max_tokens: 2048,
            timeout_secs: 180,
        }
    }
}

impl LlmConfig {
    /// 从环境变量读取，缺省值指向本地 Ollama。
    ///
    /// 识别的变量：
    /// `QUENCH3D_LLM_BASE_URL` / `QUENCH3D_LLM_MODEL` / `QUENCH3D_LLM_API_KEY` /
    /// `QUENCH3D_LLM_TEMPERATURE` / `QUENCH3D_LLM_MAX_TOKENS` / `QUENCH3D_LLM_TIMEOUT`，
    /// 并回退到通用的 `OPENAI_BASE_URL` / `OPENAI_MODEL` / `OPENAI_API_KEY` / `OLLAMA_HOST`。
    pub fn from_env() -> Self {
        let mut c = Self::default();
        if let Some(v) = env_first(&["QUENCH3D_LLM_BASE_URL", "OPENAI_BASE_URL"]) {
            c.base_url = v;
        } else if let Some(host) = env_first(&["OLLAMA_HOST"]) {
            let host = host.trim_end_matches('/').to_string();
            c.base_url = if host.starts_with("http") {
                format!("{host}/v1")
            } else {
                format!("http://{host}/v1")
            };
        }
        if let Some(v) = env_first(&["QUENCH3D_LLM_MODEL", "OPENAI_MODEL"]) {
            c.model = v;
        }
        if let Some(v) = env_first(&["QUENCH3D_LLM_API_KEY", "OPENAI_API_KEY"]) {
            c.api_key = Some(v);
        }
        if let Some(v) = env_first(&["QUENCH3D_LLM_TEMPERATURE"]).and_then(|v| v.parse().ok()) {
            c.temperature = v;
        }
        if let Some(v) = env_first(&["QUENCH3D_LLM_MAX_TOKENS"]).and_then(|v| v.parse().ok()) {
            c.max_tokens = v;
        }
        if let Some(v) = env_first(&["QUENCH3D_LLM_TIMEOUT"]).and_then(|v| v.parse().ok()) {
            c.timeout_secs = v;
        }
        c
    }

    /// 补全 chat/completions 的完整 URL。
    pub fn endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        if base.ends_with("/chat/completions") {
            base.to_string()
        } else {
            format!("{base}/chat/completions")
        }
    }

    /// 描述当前配置（用于 `--json` 输出与错误提示，不含密钥本身）。
    pub fn describe(&self) -> String {
        format!(
            "{} @ {}（key: {}）",
            self.model,
            self.base_url,
            if self.api_key.is_some() {
                "已设置"
            } else {
                "无"
            }
        )
    }
}

fn env_first(names: &[&str]) -> Option<String> {
    for n in names {
        if let Ok(v) = std::env::var(n) {
            if !v.trim().is_empty() {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

/// 一次对话的消息。
///
/// 除了最常用的 system / user / assistant，还支持 OpenAI 的函数调用协议：
/// assistant 消息可以带 `tool_calls`，随后用 `role = "tool"` + `tool_call_id` 回执结果。
#[derive(Debug, Clone)]
pub struct Message {
    /// 角色：`system` / `user` / `assistant` / `tool`。
    pub role: &'static str,
    /// 内容（函数调用消息可能为空）。
    pub content: String,
    /// assistant 发起的工具调用（原始 JSON）。
    pub tool_calls: Option<serde_json::Value>,
    /// `role = "tool"` 时对应的调用 id。
    pub tool_call_id: Option<String>,
}

impl Message {
    /// 系统提示。
    pub fn system(content: impl Into<String>) -> Self {
        Self::text("system", content)
    }

    /// 用户消息。
    pub fn user(content: impl Into<String>) -> Self {
        Self::text("user", content)
    }

    /// 助手消息（纯文本）。
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::text("assistant", content)
    }

    /// 任意角色的纯文本消息。
    pub fn text(role: &'static str, content: impl Into<String>) -> Self {
        Self {
            role,
            content: content.into(),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    /// 工具执行结果回执。
    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool",
            content: content.into(),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
        }
    }

    /// 转成协议里的 JSON 对象。
    pub fn to_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({ "role": self.role, "content": self.content });
        if let Some(calls) = &self.tool_calls {
            v["tool_calls"] = calls.clone();
        }
        if let Some(id) = &self.tool_call_id {
            v["tool_call_id"] = serde_json::json!(id);
        }
        v
    }
}

/// 一次函数调用请求。
#[derive(Debug, Clone)]
pub struct ToolCall {
    /// 服务给的调用 id（回执时必须带上）。
    pub id: String,
    /// 函数名。
    pub name: String,
    /// 参数（JSON 文本，模型可能给出不合法 JSON）。
    pub arguments: String,
}

impl ToolCall {
    /// 解析参数；不合法时退化成空对象，让工具自己报参数错误。
    pub fn args(&self) -> serde_json::Value {
        serde_json::from_str(&self.arguments).unwrap_or_else(|_| serde_json::json!({}))
    }
}

/// 一次调用的结果。
#[derive(Debug, Clone)]
pub struct ChatOutcome {
    /// 模型输出的文本。
    pub text: String,
    /// 服务返回的模型名（如果有）。
    pub model: String,
    /// token 用量（如果有）。
    pub total_tokens: Option<u64>,
    /// 原始响应体。
    pub raw: String,
    /// assistant 消息里请求的工具调用。
    pub tool_calls: Vec<ToolCall>,
    /// assistant 消息的原始 JSON（需要原样回填进上下文）。
    pub assistant_message: serde_json::Value,
    /// 结束原因。
    pub finish_reason: Option<String>,
}

/// 简单的对话客户端。
pub struct LlmClient {
    config: LlmConfig,
}

impl LlmClient {
    /// 用给定配置创建。
    pub fn new(config: LlmConfig) -> Self {
        Self { config }
    }

    /// 使用环境变量配置创建。
    pub fn from_env() -> Self {
        Self::new(LlmConfig::from_env())
    }

    /// 当前配置。
    pub fn config(&self) -> &LlmConfig {
        &self.config
    }

    /// 发一轮对话，返回模型输出文本。
    pub fn chat(&self, messages: &[Message]) -> Result<ChatOutcome, LlmError> {
        self.chat_with_tools(messages, None)
    }

    /// 发一轮对话，并把可用工具一并告诉模型（OpenAI 函数调用协议）。
    pub fn chat_with_tools(
        &self,
        messages: &[Message],
        tools: Option<&serde_json::Value>,
    ) -> Result<ChatOutcome, LlmError> {
        let mut body = serde_json::json!({
            "model": self.config.model,
            "temperature": self.config.temperature,
            "max_tokens": self.config.max_tokens,
            "stream": false,
            "messages": messages.iter().map(|m| m.to_json()).collect::<Vec<_>>(),
        });
        if let Some(tools) = tools {
            body["tools"] = tools.clone();
            body["tool_choice"] = serde_json::json!("auto");
        }
        let body = serde_json::to_string(&body)
            .map_err(|e| LlmError::parse(format!("序列化请求失败: {e}")))?;

        let mut headers: Vec<(String, String)> =
            vec![("Content-Type".into(), "application/json".into())];
        if let Some(key) = &self.config.api_key {
            headers.push(("Authorization".into(), format!("Bearer {key}")));
        }

        let raw = http_post_json(
            &self.config.endpoint(),
            &headers,
            &body,
            Duration::from_secs(self.config.timeout_secs),
        )?;

        let value: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|e| LlmError::parse(format!("响应不是合法 JSON: {e}")))?;

        if let Some(err) = value.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or_else(|| err.as_str().unwrap_or("未知错误"));
            return Err(LlmError::http(format!("模型服务返回错误: {msg}"))
                .with_hint("检查 base_url / model 名 / API Key 是否正确（本地服务不需要 Key）"));
        }

        let text = value
            .pointer("/choices/0/message/content")
            .and_then(|c| c.as_str())
            .or_else(|| value.pointer("/choices/0/text").and_then(|c| c.as_str()))
            .ok_or_else(|| {
                LlmError::parse("响应里没有 choices[0].message.content".to_string())
                    .with_hint("该服务可能不是 OpenAI 兼容协议")
            })?
            .to_string();

        let assistant_message = value
            .pointer("/choices/0/message")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({ "role": "assistant", "content": text }));
        let tool_calls = assistant_message
            .get("tool_calls")
            .and_then(|c| c.as_array())
            .map(|calls| {
                calls
                    .iter()
                    .filter_map(|c| {
                        Some(ToolCall {
                            id: c.get("id")?.as_str()?.to_string(),
                            name: c.pointer("/function/name")?.as_str()?.to_string(),
                            arguments: c
                                .pointer("/function/arguments")
                                .and_then(|a| a.as_str())
                                .unwrap_or("{}")
                                .to_string(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Ok(ChatOutcome {
            text,
            model: value
                .get("model")
                .and_then(|m| m.as_str())
                .unwrap_or(&self.config.model)
                .to_string(),
            total_tokens: value
                .pointer("/usage/total_tokens")
                .and_then(|t| t.as_u64()),
            raw,
            tool_calls,
            assistant_message,
            finish_reason: value
                .pointer("/choices/0/finish_reason")
                .and_then(|f| f.as_str())
                .map(|s| s.to_string()),
        })
    }

    /// 发一轮对话并解析出 JSON。
    ///
    /// 大模型经常把 JSON 包在 ```json 代码块或一段解释里，这里会自动把第一个
    /// 完整的 JSON 对象抠出来再解析。
    pub fn chat_json(
        &self,
        messages: &[Message],
    ) -> Result<(serde_json::Value, ChatOutcome), LlmError> {
        let outcome = self.chat(messages)?;
        let json = extract_json(&outcome.text).ok_or_else(|| {
            LlmError::parse(format!(
                "模型输出里找不到 JSON 对象，原始输出前 400 字：\n{}",
                truncate(&outcome.text, 400)
            ))
            .with_hint("可以换更大的模型，或在提示里强调“只输出 JSON”")
        })?;
        let value: serde_json::Value = serde_json::from_str(&json).map_err(|e| {
            LlmError::parse(format!(
                "模型给出的 JSON 无法解析: {e}\n片段：{}",
                truncate(&json, 400)
            ))
        })?;
        Ok((value, outcome))
    }
}

/// 从一段文本里抠出第一个配平的 JSON 对象。
pub fn extract_json(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let start = bytes.iter().position(|b| *b == b'{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (i, b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if *b == b'\\' {
                escaped = true;
            } else if *b == b'"' {
                in_string = false;
            }
            continue;
        }
        match *b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

// ---------------------------------------------------------------------------
// 极简 HTTP/1.1 客户端
// ---------------------------------------------------------------------------

/// 解析后的 URL。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    /// `http` 或 `https`。
    pub scheme: String,
    /// 主机名。
    pub host: String,
    /// 端口。
    pub port: u16,
    /// 路径 + 查询串。
    pub path: String,
}

impl Url {
    /// 解析 `scheme://host[:port][/path]`。
    pub fn parse(url: &str) -> Result<Self, LlmError> {
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| LlmError::config(format!("URL 缺少协议头: {url}")))?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(LlmError::config(format!("不支持的协议: {scheme}")));
        }
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (
                h.to_string(),
                p.parse::<u16>()
                    .map_err(|_| LlmError::config(format!("端口不合法: {p}")))?,
            ),
            _ => (
                authority.to_string(),
                if scheme == "https" { 443 } else { 80 },
            ),
        };
        if host.is_empty() {
            return Err(LlmError::config(format!("URL 里没有主机名: {url}")));
        }
        Ok(Self {
            scheme,
            host,
            port,
            path: path.to_string(),
        })
    }
}

/// POST 一段 JSON，返回响应体。`http` 走自研客户端，`https` 交给 curl。
pub fn http_post_json(
    url: &str,
    headers: &[(String, String)],
    body: &str,
    timeout: Duration,
) -> Result<String, LlmError> {
    http_post_json_with_curl(url, headers, body, timeout, None)
}

/// 同 [`http_post_json`]，但可以指定 curl 可执行文件（`None` = 用 `PATH` 里的 `curl`）。
///
/// 留这个入口有两个用处：给测试一个**假 curl**，用来断言"API key 不在命令行参数里"；
/// 以及给便携安装（比如 Windows 上把 curl.exe 放在程序目录）留一条路。
pub fn http_post_json_with_curl(
    url: &str,
    headers: &[(String, String)],
    body: &str,
    timeout: Duration,
    curl_program: Option<&std::path::Path>,
) -> Result<String, LlmError> {
    let parsed = Url::parse(url)?;
    if parsed.scheme == "https" {
        post_via_curl(url, headers, body, timeout, curl_program)
    } else {
        post_http(&parsed, headers, body, timeout)
    }
}

fn post_http(
    url: &Url,
    headers: &[(String, String)],
    body: &str,
    timeout: Duration,
) -> Result<String, LlmError> {
    let addr = format!("{}:{}", url.host, url.port);
    let stream = TcpStream::connect(&addr).map_err(|e| {
        LlmError::network(format!("连接 {addr} 失败: {e}"))
            .with_hint("确认本地推理服务已启动（例如 `ollama serve`），或用 --base-url 指定地址")
    })?;
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|_| stream.set_write_timeout(Some(timeout)))
        .map_err(|e| LlmError::network(format!("设置超时失败: {e}")))?;

    let mut request = format!(
        "POST {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nContent-Length: {}\r\nAccept: application/json\r\n",
        url.path,
        url.host,
        body.len()
    );
    for (k, v) in headers {
        request.push_str(&format!("{k}: {v}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);

    let mut stream = stream;
    stream
        .write_all(request.as_bytes())
        .map_err(|e| LlmError::network(format!("发送请求失败: {e}")))?;
    stream
        .flush()
        .map_err(|e| LlmError::network(format!("刷新缓冲区失败: {e}")))?;

    let mut reader = BufReader::new(stream);
    read_http_response(&mut reader)
}

/// 读取并解码一个 HTTP 响应（支持 Content-Length 与 chunked）。
fn read_http_response<R: BufRead>(reader: &mut R) -> Result<String, LlmError> {
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| LlmError::network(format!("读取状态行失败: {e}")))?;
    if status_line.is_empty() {
        return Err(LlmError::network("服务端没有返回任何内容"));
    }
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut content_length: Option<usize> = None;
    let mut chunked = false;
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| LlmError::network(format!("读取响应头失败: {e}")))?;
        if n == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().ok();
        } else if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
    }

    let mut body = String::new();
    if chunked {
        loop {
            let mut size_line = String::new();
            reader
                .read_line(&mut size_line)
                .map_err(|e| LlmError::network(format!("读取分块长度失败: {e}")))?;
            let size_hex = size_line.trim().split(';').next().unwrap_or("0");
            let size = usize::from_str_radix(size_hex, 16).unwrap_or(0);
            if size == 0 {
                break;
            }
            let mut chunk = vec![0u8; size];
            reader
                .read_exact(&mut chunk)
                .map_err(|e| LlmError::network(format!("读取分块失败: {e}")))?;
            body.push_str(&String::from_utf8_lossy(&chunk));
            let mut crlf = [0u8; 2];
            let _ = reader.read_exact(&mut crlf);
        }
    } else if let Some(len) = content_length {
        let mut buf = vec![0u8; len];
        reader
            .read_exact(&mut buf)
            .map_err(|e| LlmError::network(format!("读取响应体失败: {e}")))?;
        body = String::from_utf8_lossy(&buf).to_string();
    } else {
        let mut buf = Vec::new();
        reader
            .read_to_end(&mut buf)
            .map_err(|e| LlmError::network(format!("读取响应体失败: {e}")))?;
        body = String::from_utf8_lossy(&buf).to_string();
    }

    if !(200..300).contains(&status) {
        return Err(LlmError::http(format!(
            "HTTP {status}：{}",
            truncate(body.trim(), 500)
        )));
    }
    Ok(body)
}

/// 含密钥的 curl 配置文件：0600 权限、进程退出即删。
///
/// 为什么不再用 `-H "Authorization: Bearer …"`：命令行参数对**同机的其他用户**可读
/// （`/proc/<pid>/cmdline`、`ps`、某些系统的审计日志），密钥会当着他们的面泄露。
/// 配置文件是 0600，只有同一个用户能读，而且请求结束就删除。
struct CurlConfigFile {
    path: std::path::PathBuf,
}

impl CurlConfigFile {
    fn create(contents: &str) -> Result<Self, LlmError> {
        use std::io::Write;
        let dir = std::env::temp_dir();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        for attempt in 0..16u32 {
            let path = dir.join(format!(
                "quench3d-curl-{}-{nanos}-{attempt}.conf",
                std::process::id()
            ));
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create_new(true);
            // `create_new` 不会跟随已存在的符号链接，名字也不可预测
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            match opts.open(&path) {
                Ok(mut f) => {
                    f.write_all(contents.as_bytes())
                        .and_then(|_| f.sync_all())
                        .map_err(|e| LlmError::network(format!("写 curl 配置文件失败: {e}")))?;
                    return Ok(Self { path });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(LlmError::network(format!("创建 curl 配置文件失败: {e}"))),
            }
        }
        Err(LlmError::network(
            "创建 curl 配置文件失败：临时目录里同名的文件一直存在",
        ))
    }
}

impl Drop for CurlConfigFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// curl 配置文件的字符串转义（反斜杠、双引号与控制字符）。
fn curl_config_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {} // 其余控制字符直接丢掉，别让它们截断配置行
            c => out.push(c),
        }
    }
    out
}

fn post_via_curl(
    url: &str,
    headers: &[(String, String)],
    body: &str,
    timeout: Duration,
    curl_program: Option<&std::path::Path>,
) -> Result<String, LlmError> {
    use std::process::{Command, Stdio};
    // url 与 header 走配置文件（其中含密钥），请求体仍然走 stdin：提示词不落盘。
    let mut config = String::new();
    config.push_str(&format!("url = \"{}\"\n", curl_config_escape(url)));
    config.push_str("header = \"Content-Type: application/json\"\n");
    for (k, v) in headers {
        config.push_str(&format!(
            "header = \"{}: {}\"\n",
            curl_config_escape(k),
            curl_config_escape(v)
        ));
    }
    let config_file = CurlConfigFile::create(&config)?;
    let mut cmd = Command::new(curl_program.unwrap_or(std::path::Path::new("curl")));
    cmd.arg("-sS")
        .arg("--max-time")
        .arg(timeout.as_secs().to_string())
        .arg("-X")
        .arg("POST")
        .arg("--data-binary")
        .arg("@-")
        // 密钥在这里，而不是在 argv 里
        .arg("-K")
        .arg(&config_file.path);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        LlmError::network(format!("调用 curl 失败: {e}"))
            .with_hint("https 端点需要系统 curl；也可以把 --base-url 指向本地 http 推理服务")
    })?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(body.as_bytes())
            .map_err(|e| LlmError::network(format!("写入 curl 标准输入失败: {e}")))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|e| LlmError::network(format!("等待 curl 结束失败: {e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(LlmError::network(format!(
            "curl 退出码 {:?}: {}",
            output.status.code(),
            truncate(err.trim(), 300)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curl_config_escaping_neutralizes_quotes_and_control_chars() {
        // 反斜杠与双引号必须转义，否则能提前闭合配置里的字符串
        assert_eq!(curl_config_escape(r#"a"b\c"#), r#"a\"b\\c"#);
        // 换行/回车/制表符转成转义序列：否则可以伪造一条新的配置行（比如换掉 url）
        assert_eq!(curl_config_escape("x\ny\rz\tw"), r"x\ny\rz\tw");
        // 其余控制字符直接丢弃
        assert_eq!(curl_config_escape("a\u{7}b\u{0}c"), "abc");
        // 普通字符原样保留
        assert_eq!(
            curl_config_escape("Bearer sk-abc_123.456"),
            "Bearer sk-abc_123.456"
        );
    }

    #[test]
    fn url_parsing_handles_ports_and_paths() {
        let u = Url::parse("http://localhost:11434/v1/chat/completions").unwrap();
        assert_eq!(u.scheme, "http");
        assert_eq!(u.host, "localhost");
        assert_eq!(u.port, 11434);
        assert_eq!(u.path, "/v1/chat/completions");

        let u = Url::parse("https://api.example.com/v1").unwrap();
        assert_eq!(u.port, 443);
        assert_eq!(u.path, "/v1");

        let u = Url::parse("http://127.0.0.1").unwrap();
        assert_eq!(u.port, 80);
        assert_eq!(u.path, "/");
    }

    #[test]
    fn endpoint_join_is_idempotent() {
        let mut c = LlmConfig {
            base_url: "http://localhost:8080/v1/".into(),
            ..Default::default()
        };
        assert_eq!(c.endpoint(), "http://localhost:8080/v1/chat/completions");
        c.base_url = "http://localhost:8080/v1/chat/completions".into();
        assert_eq!(c.endpoint(), "http://localhost:8080/v1/chat/completions");
    }

    #[test]
    fn extract_json_ignores_prose_and_code_fences() {
        let text = "好的，这是场景：\n```json\n{\"name\": \"a\", \"nested\": {\"x\": [1,2,3]}}\n```\n希望有帮助！";
        let json = extract_json(text).unwrap();
        assert_eq!(json, r#"{"name": "a", "nested": {"x": [1,2,3]}}"#);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["nested"]["x"][2], 3);
    }

    #[test]
    fn extract_json_handles_braces_inside_strings() {
        let text = r#"prefix {"a": "}{ not a brace", "b": 1} suffix"#;
        assert_eq!(
            extract_json(text).unwrap(),
            r#"{"a": "}{ not a brace", "b": 1}"#
        );
    }
}
