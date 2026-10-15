//! 结构化输出与退出码：agent 依赖的稳定契约。
//!
//! 所有命令都支持 `--json`，输出统一信封：
//!
//! ```json
//! { "ok": true, "command": "render", "elapsed_ms": 812, "data": { … } }
//! { "ok": false, "command": "render", "error": { "kind": "scene", "message": "…", "hint": "…" } }
//! ```
//!
//! 退出码固定为：[`EXIT_OK`] / [`EXIT_RUNTIME`] / [`EXIT_INVALID`] / [`EXIT_ENVIRONMENT`] /
//! [`EXIT_NETWORK`]。agent 只看退出码就能决定是重试、改参数还是换机器。

use serde::Serialize;
use serde_json::Value;

/// 成功。
pub const EXIT_OK: i32 = 0;
/// 运行期错误（意外失败）。
pub const EXIT_RUNTIME: i32 = 1;
/// 输入不合法（场景 JSON / 参数校验失败）——改输入后重试即可。
pub const EXIT_INVALID: i32 = 2;
/// 环境问题（没有可用 GPU、后端缺失）——换机器或装驱动。
pub const EXIT_ENVIRONMENT: i32 = 3;
/// 网络 / 大模型服务问题——检查服务是否启动、地址与模型名。
pub const EXIT_NETWORK: i32 = 4;

/// 结构化错误。
#[derive(Debug, Clone, Serialize)]
pub struct ErrorInfo {
    /// 类别：`scene` / `invalid` / `environment` / `network` / `io` / `runtime`。
    pub kind: String,
    /// 人可读说明。
    pub message: String,
    /// 相关 JSON 路径（如果有）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// 修复建议。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// 校验诊断（如果有）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub diagnostics: Vec<crate::scene_doc::Diagnostic>,
}

impl ErrorInfo {
    /// 只带类别的错误。
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            message: message.into(),
            path: None,
            hint: None,
            diagnostics: Vec::new(),
        }
    }

    /// 附带 JSON 路径。
    pub fn at(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// 附带建议。
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// 附带校验诊断。
    pub fn with_diagnostics(mut self, diags: Vec<crate::scene_doc::Diagnostic>) -> Self {
        self.diagnostics = diags;
        self
    }

    /// 由运行期错误构造（保留路径与建议——agent 靠这两样决定下一步）。
    pub fn from_run(e: &crate::runner::RunError) -> Self {
        let mut info = Self::new(e.kind, e.message.clone());
        if let Some(path) = &e.path {
            info = info.at(path.clone());
        }
        if e.kind == "output_exists" {
            info = info
                .hint("换一个 --out / --frames-dir，或去掉 --no-clobber；也可以先自己把旧产物移走");
        }
        info
    }

    /// 对应的退出码。
    pub fn exit_code(&self) -> i32 {
        match self.kind.as_str() {
            // 输出已存在（--no-clobber）与场景不合法同属"输入有问题，改参数再跑"，都是 2
            "invalid" | "scene" | "output_exists" => EXIT_INVALID,
            "environment" => EXIT_ENVIRONMENT,
            "network" | "config" | "http" | "parse" => EXIT_NETWORK,
            "io" | "runtime" => EXIT_RUNTIME,
            _ => EXIT_RUNTIME,
        }
    }
}

/// 把场景解析/校验错误转成结构化错误，**保留全部诊断**。
///
/// 这是 agent 体验的关键：一次返回所有问题，每条都带 JSON 路径与建议。
pub fn doc_error_info(e: &crate::scene_doc::DocError) -> ErrorInfo {
    use crate::scene_doc::{DocError, Severity};
    match e {
        DocError::Invalid(diags) => {
            let errors = diags
                .iter()
                .filter(|d| d.severity == Severity::Error)
                .count();
            let warnings = diags.len() - errors;
            let mut info = ErrorInfo::new(
                "invalid",
                format!("场景不合法：{errors} 个错误、{warnings} 条警告"),
            )
            .with_diagnostics(diags.clone());
            // 只有一条诊断时，把它的建议提升到顶层，agent 不用再往下钻
            if diags.len() == 1 {
                if let Some(hint) = &diags[0].hint {
                    info = info.hint(hint.clone());
                }
                if diags[0].path != "$" {
                    info = info.at(diags[0].path.clone());
                }
            }
            info
        }
        DocError::Json(je) => ErrorInfo::new("invalid", format!("JSON 语法错误: {je}"))
            .hint("检查括号、逗号与引号；可用 `quench3d example` 打印模板对照"),
    }
}

/// 统一输出信封。
#[derive(Debug, Clone, Serialize)]
pub struct Envelope {
    /// 是否成功。
    pub ok: bool,
    /// 命令名。
    pub command: String,
    /// 工具版本。
    pub version: String,
    /// 耗时（毫秒）。
    pub elapsed_ms: u64,
    /// 成功时的数据。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// 失败时的错误。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
}

impl Envelope {
    /// 成功信封。
    pub fn ok(command: &str, data: Value, elapsed_ms: u64) -> Self {
        Self {
            ok: true,
            command: command.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            elapsed_ms,
            data: Some(data),
            error: None,
        }
    }

    /// 失败信封。
    pub fn fail(command: &str, error: ErrorInfo, elapsed_ms: u64) -> Self {
        Self {
            ok: false,
            command: command.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            elapsed_ms,
            data: None,
            error: Some(error),
        }
    }

    /// 序列化为单行 JSON（agent 友好）。
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            format!(
                r#"{{"ok":false,"command":"{}","error":{{"kind":"runtime","message":"序列化输出失败: {e}"}}}}"#,
                self.command
            )
        })
    }

    /// 退出码。
    pub fn exit_code(&self) -> i32 {
        if self.ok {
            EXIT_OK
        } else {
            self.error
                .as_ref()
                .map(|e| e.exit_code())
                .unwrap_or(EXIT_RUNTIME)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_map_to_error_kinds() {
        assert_eq!(ErrorInfo::new("scene", "x").exit_code(), EXIT_INVALID);
        assert_eq!(
            ErrorInfo::new("environment", "x").exit_code(),
            EXIT_ENVIRONMENT
        );
        assert_eq!(ErrorInfo::new("network", "x").exit_code(), EXIT_NETWORK);
        // `--no-clobber` 的输出冲突：2（改个路径/去掉开关就能重试），不是 1
        assert_eq!(
            ErrorInfo::new("output_exists", "x").exit_code(),
            EXIT_INVALID
        );
        assert_eq!(ErrorInfo::new("io", "x").exit_code(), EXIT_RUNTIME);
        assert_eq!(ErrorInfo::new("whatever", "x").exit_code(), EXIT_RUNTIME);
    }

    #[test]
    fn envelope_round_trips_as_single_line_json() {
        let env = Envelope::ok("info", serde_json::json!({"triangles": 12}), 7);
        let text = env.to_json();
        assert!(!text.contains('\n'), "JSON 输出必须是单行");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["data"]["triangles"], 12);
        assert_eq!(env.exit_code(), EXIT_OK);

        let env = Envelope::fail(
            "render",
            ErrorInfo::new("environment", "没有 GPU").hint("装驱动"),
            1,
        );
        let v: Value = serde_json::from_str(&env.to_json()).unwrap();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["hint"], "装驱动");
        assert_eq!(env.exit_code(), EXIT_ENVIRONMENT);
    }
}
