//! 「自然语言 → 场景」的共享实现。
//!
//! 命令行（`quench3d ask`）与 MCP 工具（`quench3d_ask`）都走这里，保证两边行为一致。
//!
//! 最关键的一点是**自动修复**：模型第一次给出的场景经常差一点（枚举写错、少了必填字段、
//! 引用了不存在的材质）。与其把诊断丢给用户，不如把诊断原样回喂给模型让它自己改——
//! 本地小模型在这条闭环上的成功率提升非常明显。

use serde_json::{json, Value};

use crate::llm::{ChatOutcome, LlmClient, LlmError, Message};
use crate::scene_doc::{Diagnostic, SceneDoc, Severity};
use crate::schema;

/// 生成参数。
#[derive(Debug, Clone)]
pub struct GenerateOptions {
    /// 用户需求。
    pub prompt: String,
    /// 出错后最多再让模型修几次（0 表示一次成型）。
    pub repair_rounds: u32,
    /// 请求超时（秒），仅用于展示。
    pub timeout_secs: u64,
}

impl GenerateOptions {
    /// 用需求与修复轮数构造。
    pub fn new(prompt: impl Into<String>, repair_rounds: u32) -> Self {
        Self {
            prompt: prompt.into(),
            repair_rounds,
            timeout_secs: 0,
        }
    }
}

/// 单次尝试的记录。
#[derive(Debug, Clone)]
pub struct Attempt {
    /// 第几次（从 1 开始）。
    pub round: u32,
    /// 是否是修复轮。
    pub repair: bool,
    /// 模型输出长度。
    pub output_chars: usize,
    /// 是否通过校验。
    pub valid: bool,
    /// 错误条数。
    pub errors: usize,
    /// 诊断摘要（最多几条）。
    pub messages: Vec<String>,
}

impl Attempt {
    /// 转成 JSON。
    pub fn to_json(&self) -> Value {
        json!({
            "round": self.round,
            "repair": self.repair,
            "output_chars": self.output_chars,
            "valid": self.valid,
            "errors": self.errors,
            "messages": self.messages,
        })
    }
}

/// 生成结果。
#[derive(Debug, Clone)]
pub struct GeneratedScene {
    /// 解析成功的场景（失败时为 None）。
    pub scene: Option<SceneDoc>,
    /// 原始 JSON（失败时是模型给的那个对象）。
    pub value: Value,
    /// 落盘用的 JSON 文本。
    pub scene_json: String,
    /// 模型最后一次的原始输出。
    pub text: String,
    /// 模型名。
    pub model: String,
    /// token 用量。
    pub usage: Option<u64>,
    /// 逐次尝试记录。
    pub attempts: Vec<Attempt>,
    /// 最终诊断（合法时为空）。
    pub diagnostics: Vec<Diagnostic>,
}

impl GeneratedScene {
    /// 是否拿到了合法场景。
    pub fn valid(&self) -> bool {
        self.scene.is_some() && self.errors() == 0
    }

    /// 错误条数。
    pub fn errors(&self) -> usize {
        self.diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    /// 修复了几轮（0 表示一次成型）。
    pub fn repair_rounds_used(&self) -> u32 {
        self.attempts.iter().filter(|a| a.repair).count() as u32
    }

    /// 尝试次数。
    pub fn attempts_count(&self) -> u32 {
        self.attempts.len() as u32
    }
}

/// 生成并（必要时）自动修复。
///
/// 每一轮：把模型输出解析成场景 → 校验 → 合法就返回；不合法就把诊断拼成一条
/// 用户消息接着问，直到用完 `repair_rounds`。
pub fn generate(client: &LlmClient, options: &GenerateOptions) -> Result<GeneratedScene, LlmError> {
    let mut messages = vec![
        Message::system(schema::system_prompt()),
        Message::user(options.prompt.clone()),
    ];
    let mut attempts = Vec::new();
    let total = options.repair_rounds + 1;

    let mut last: Option<(Value, String, ChatOutcome, Vec<Diagnostic>)> = None;

    for round in 1..=total {
        let (value, outcome) = client.chat_json(&messages)?;
        let text = outcome.text.clone();
        let diagnostics = parse_and_validate(&value);
        let errors = diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        attempts.push(Attempt {
            round,
            repair: round > 1,
            output_chars: text.chars().count(),
            valid: errors == 0,
            errors,
            messages: diagnostics
                .iter()
                .take(6)
                .map(|d| format!("{}: {}", d.path, d.message))
                .collect(),
        });

        if errors == 0 {
            return Ok(finish(value, text, outcome, diagnostics, attempts));
        }

        // 准备下一轮：把 assistant 的原话与诊断一起放回上下文
        messages.push(Message::assistant(text.clone()));
        if round < total {
            messages.push(Message::user(repair_prompt(&value, &diagnostics)));
        }
        last = Some((value, text, outcome, diagnostics));
    }

    let (value, text, outcome, diagnostics) =
        last.ok_or_else(|| LlmError::config("生成循环没有产生任何结果"))?;
    Ok(finish(value, text, outcome, diagnostics, attempts))
}

/// 组装最终结果。
fn finish(
    value: Value,
    text: String,
    outcome: ChatOutcome,
    diagnostics: Vec<Diagnostic>,
    attempts: Vec<Attempt>,
) -> GeneratedScene {
    let scene = serde_json::from_value::<SceneDoc>(value.clone()).ok();
    let scene_json = match &scene {
        // 规范化的场景（缺省值补全、字段顺序统一），agent 拿到的就是引擎理解的版本
        Some(doc) => doc.to_json_pretty(),
        None => serde_json::to_string_pretty(&value).unwrap_or_default(),
    };
    GeneratedScene {
        scene,
        value,
        scene_json,
        text,
        model: outcome.model,
        usage: outcome.total_tokens,
        attempts,
        diagnostics,
    }
}

/// 解析 + 结构诊断 + 语义校验。
///
/// 结构都不合法时用 [`crate::scene_doc::structural_diagnostics`]，它能一次列出
/// 所有字段级问题（带 JSON 路径），这正是回喂给模型最有用的信息。
fn parse_and_validate(value: &Value) -> Vec<Diagnostic> {
    match serde_json::from_value::<SceneDoc>(value.clone()) {
        Ok(doc) => {
            let mut diags = doc.validate();
            // 错误排前面，模型先看到必须改的东西
            diags.sort_by_key(|d| d.severity != Severity::Error);
            diags
        }
        Err(_) => crate::scene_doc::structural_diagnostics(value),
    }
}

/// 给模型的修复指令：只返回 JSON + 逐条诊断 + 上一次的输出。
pub fn repair_prompt(previous: &Value, diagnostics: &[Diagnostic]) -> String {
    let mut list = String::new();
    for d in diagnostics.iter().take(12) {
        list.push_str(&format!("- {}: {}", d.path, d.message));
        if let Some(hint) = &d.hint {
            list.push_str(&format!("（建议：{hint}）"));
        }
        list.push('\n');
    }
    let previous_text = serde_json::to_string_pretty(previous).unwrap_or_default();
    format!(
        "你上一次的输出不是合法场景，请修正后**只返回完整的场景 JSON**，不要解释、不要 Markdown 代码块。\n\n\
         诊断：\n{list}\n\
         上一次的输出：\n{previous_text}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::LlmConfig;

    #[test]
    fn repair_prompt_lists_paths_and_previous_output() {
        let diags = vec![
            Diagnostic::error("objects[0].shape.type", "unknown variant `pyramid`")
                .with_hint("可用: box / sphere"),
            Diagnostic::error("objects[0].body", "unknown variant `flying`"),
        ];
        let prompt = repair_prompt(&json!({ "objects": [] }), &diags);
        assert!(prompt.contains("objects[0].shape.type"));
        assert!(prompt.contains("可用: box / sphere"));
        assert!(prompt.contains("objects[0].body"));
        assert!(prompt.contains("只返回完整的场景 JSON"));
        assert!(prompt.contains("\"objects\""));
    }

    #[test]
    fn parse_and_validate_reports_structural_errors_without_crashing() {
        let diags = parse_and_validate(&json!({
            "objects": [{ "shape": { "type": "pyramid" }, "body": "dynamic" }]
        }));
        assert!(diags.iter().any(|d| d.path == "objects[0].shape.type"));
    }

    #[test]
    fn parse_and_validate_accepts_default_scene() {
        let diags = parse_and_validate(&json!({}));
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn generate_with_zero_repair_rounds_only_calls_model_once() {
        // 没有服务时应当直接返回网络错误，而不是卡住
        let client = LlmClient::new(LlmConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            model: "x".into(),
            timeout_secs: 1,
            ..Default::default()
        });
        let options = GenerateOptions::new("随便", 0);
        assert!(generate(&client, &options).is_err());
    }
}
