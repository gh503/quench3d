//! 命令行 agent：让大模型自己决定调用哪些引擎工具。
//!
//! `quench3d chat` 走的就是这里。和 `ask`（一次性把自然语言变成场景）不同，agent 循环
//! 把工具**交给模型自己选**：
//!
//! ```text
//! 用户：做一个三层箱子塔，然后告诉我五秒后还在不在原地
//!   → 模型调用 quench3d_validate   （校验场景）
//!   → 模型调用 quench3d_simulate   （跑物理，看结果）
//!   → 模型调用 quench3d_render     （出图）
//!   → 模型用自然语言回答
//! ```
//!
//! 工具的执行复用 [`crate::mcp::McpServer::dispatch_tool`]，所以 MCP 客户端与命令行
//! agent 看到的是同一套行为、同一套诊断。

use serde_json::{json, Value};

use crate::llm::{ChatOutcome, LlmClient, LlmConfig, LlmError, Message};
use crate::mcp::McpServer;
use crate::schema;

/// 一次工具调用的记录（给用户看的轨迹）。
#[derive(Debug, Clone)]
pub struct ToolTrace {
    /// 工具名。
    pub name: String,
    /// 模型给的参数。
    pub args: Value,
    /// 是否成功。
    pub ok: bool,
    /// 结果摘要（单行）。
    pub summary: String,
}

/// 一轮对话的结果。
#[derive(Debug, Clone)]
pub struct Turn {
    /// 模型最终给出的文本。
    pub text: String,
    /// 工具调用轨迹。
    pub trace: Vec<ToolTrace>,
    /// 往返轮数。
    pub rounds: u32,
    /// token 用量。
    pub usage: Option<u64>,
    /// 工具产生的最后一份结构化数据（渲染报告 / 仿真报告）。
    pub structured: Option<Value>,
}

/// 命令行 agent 会话。
pub struct Agent {
    client: LlmClient,
    server: McpServer,
    messages: Vec<Message>,
}

impl Agent {
    /// 用给定配置创建（工具配置沿用环境变量）。
    pub fn new(config: LlmConfig) -> Self {
        let server = McpServer {
            llm: config.clone(),
        };
        Self {
            client: LlmClient::new(config),
            server,
            messages: vec![Message::system(agent_system_prompt())],
        }
    }

    /// 当前模型与地址描述。
    pub fn describe(&self) -> String {
        self.client.config().describe()
    }

    /// 上下文里的消息条数。
    pub fn history_len(&self) -> usize {
        self.messages.len()
    }

    /// 清空对话（保留系统提示）。
    pub fn reset(&mut self) {
        self.messages.truncate(1);
    }

    /// 工具定义，转成 OpenAI 的 `tools` 参数格式。
    ///
    /// 与 MCP 的 `tools/list` 同源，避免两处漂移。
    pub fn openai_tools() -> Value {
        Value::Array(
            crate::mcp::tool_definitions()
                .into_iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t["name"],
                            "description": t["description"],
                            "parameters": t["inputSchema"],
                        }
                    })
                })
                .collect(),
        )
    }

    /// 跑一轮：把 `user` 发给模型，模型要工具就去执行，直到它给出文本答案。
    ///
    /// `max_rounds` 是工具往返上限，防止模型在两个工具之间反复横跳。
    pub fn run_turn(&mut self, user: &str, max_rounds: u32) -> Result<Turn, LlmError> {
        self.messages.push(Message::user(user));
        let tools = Self::openai_tools();
        let mut trace = Vec::new();
        let mut structured = None;

        for round in 0..=max_rounds {
            let outcome = self.client.chat_with_tools(&self.messages, Some(&tools))?;

            // 把 assistant 消息（可能带 tool_calls）原样放回上下文
            self.messages.push(Message {
                role: "assistant",
                content: outcome.text.clone(),
                tool_calls: outcome
                    .assistant_message
                    .get("tool_calls")
                    .cloned()
                    .filter(|c| !c.is_null()),
                tool_call_id: None,
            });

            if outcome.tool_calls.is_empty() {
                return Ok(Turn {
                    text: outcome.text,
                    trace,
                    rounds: round,
                    usage: outcome.total_tokens,
                    structured,
                });
            }

            if round == max_rounds {
                // 到达上限：如实告诉用户，而不是无限循环
                self.messages.push(Message::user(format!(
                    "已经连续调用工具 {max_rounds} 轮，请直接用已有信息回答，不要再调用工具。"
                )));
                return Ok(Turn {
                    text: format!(
                        "（达到工具调用上限 {max_rounds} 轮）{}",
                        summarize_outcome(&outcome)
                    ),
                    trace,
                    rounds: round,
                    usage: outcome.total_tokens,
                    structured,
                });
            }

            for call in &outcome.tool_calls {
                let args = call.args();
                let (ok, text, data) = match self.server.dispatch_tool(&call.name, &args) {
                    Ok(result) => {
                        let ok = result["isError"] != json!(true);
                        let text = result["content"]
                            .as_array()
                            .and_then(|c| c.first())
                            .and_then(|c| c["text"].as_str())
                            .unwrap_or("(没有文本结果)")
                            .to_string();
                        (ok, text, result.get("structuredContent").cloned())
                    }
                    Err((code, message)) => {
                        (false, format!("工具调用失败（{code}）：{message}"), None)
                    }
                };
                if let Some(data) = data {
                    if !data.is_null() {
                        structured = Some(data);
                    }
                }
                trace.push(ToolTrace {
                    name: call.name.clone(),
                    args: args.clone(),
                    ok,
                    summary: first_line(&text),
                });
                self.messages
                    .push(Message::tool_result(&call.id, clip(&text, 6000)));
            }
        }
        unreachable!("循环在上限处返回")
    }
}

/// agent 的系统提示：在场景语法之上，讲清楚「你可以调工具」。
pub fn agent_system_prompt() -> String {
    format!(
        r#"你是 Quench3D 引擎的现场助手，工作在一个可以调用工具的循环里。

{grammar}

工作方式：
1. 收到需求后，先用 quench3d_validate 校验你写的场景（场景可以内联成 JSON 对象传参，不必落盘）。
2. 涉及运动、堆叠、碰撞的问题，用 quench3d_simulate 跑物理看真实结果，不要凭想象回答；
   要判断「会不会倒、滑多远、弹多高」时传 trace 参数（如 trace: ["塔1"]），
   返回里会有下坠/爬升/位移/路程/最高速/最大倾斜与是否翻倒。
   要表达「第 2 秒推一下」「到点松手」就写场景里的 actions（at_step 或 at_time）；
   要判断「有没有经过某处、第几秒进去的」就写 probes，别靠人眼数帧。
   注意质量缺省由 density×体积 推出（边长 1 的箱子密度 1000 → 1000 kg），
   冲量太小会看不出效果，必要时显式给 mass。
3. 需要画面时用 quench3d_render，输出会因为多模态而直接返回图像；记得给 output 一个明确路径。
   需要「过程」而不是单帧时（下落、碰撞、堆叠倒塌、摆动）用 quench3d_movie 录帧序列，
   它返回 ffmpeg 命令与像素摘要（同场景两次录制摘要相同，可用于自检）；帧数别开太大（缺省 30）。
4. 模型文件相关问题用 quench3d_info；不确定环境能不能渲染时用 quench3d_doctor。
5. 工具返回的错误诊断里带有 JSON 路径，照它改；改完再校验一次。
6. 回答用中文，简洁，必要时给出关键数值（位置、速度、是否稳定）。
7. 最后必须用自然语言给出结论；不要只返回工具调用。"#,
        grammar = schema::system_prompt()
    )
}

fn summarize_outcome(outcome: &ChatOutcome) -> String {
    match &outcome.finish_reason {
        Some(r) => format!("（模型结束原因：{r}）"),
        None => String::new(),
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim().to_string();
    clip(&line, 160)
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect::<String>() + "\n…（结果过长已截断）"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_tools_mirror_mcp_definitions() {
        let tools = Agent::openai_tools();
        let arr = tools.as_array().unwrap();
        assert!(arr.len() >= 7, "工具太少: {}", arr.len());
        for t in arr {
            assert_eq!(t["type"], "function");
            let f = &t["function"];
            assert!(f["name"].as_str().unwrap().starts_with("quench3d_"));
            assert!(f["description"].as_str().is_some_and(|d| !d.is_empty()));
            assert_eq!(f["parameters"]["type"], "object");
        }
    }

    #[test]
    fn system_prompt_explains_tool_usage_and_scene_grammar() {
        let p = agent_system_prompt();
        for needle in [
            "quench3d_validate",
            "quench3d_simulate",
            "quench3d_render",
            "gravity",
            "rotation_deg",
            "trace",
        ] {
            assert!(p.contains(needle), "提示缺少 {needle}");
        }
    }

    #[test]
    fn reset_keeps_only_system_message() {
        let mut agent = Agent::new(LlmConfig::default());
        assert_eq!(agent.history_len(), 1);
        agent.messages.push(Message::user("喂"));
        agent.messages.push(Message::assistant("在"));
        assert_eq!(agent.history_len(), 3);
        agent.reset();
        assert_eq!(agent.history_len(), 1);
        assert_eq!(agent.messages[0].role, "system");
    }
}
