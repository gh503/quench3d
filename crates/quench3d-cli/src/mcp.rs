//! MCP（Model Context Protocol）服务器：把 Quench3D 的能力暴露给支持 MCP 的 agent。
//!
//! 传输层是 stdio，消息是一行一个 JSON-RPC 2.0 对象（MCP 的 stdio 约定）。
//!
//! 支持的方法：
//!
//! | 方法 | 说明 |
//! | --- | --- |
//! | `initialize` | 握手，返回协议版本与能力 |
//! | `tools/list` | 列出全部工具及其 JSON Schema |
//! | `tools/call` | 调用工具 |
//! | `ping` | 保活 |
//! | `resources/list` | 返回空列表（本服务器只提供工具） |
//!
//! 工具一览：`quench3d_schema`、`quench3d_doctor`、`quench3d_info`、`quench3d_validate`、
//! `quench3d_render`、`quench3d_simulate`、`quench3d_query`、`quench3d_ask`。
//!
//! 关键设计：场景既可以传**文件路径**，也可以**直接内联 JSON 对象**——agent 通常是
//! 现生成场景，不该被迫先落盘。`quench3d_render` 会同时返回文本报告与 base64 PNG
//! 图像内容块，支持多模态的 agent 可以直接看图。

use std::io::{BufRead, Write};

use serde_json::{json, Value};

use crate::generate::{generate, GenerateOptions};
use crate::llm::{LlmClient, LlmConfig};
use crate::report::ErrorInfo;
use crate::runner;
use crate::scene_doc::{SceneDoc, Severity};

/// MCP 协议版本（与客户端协商，取双方都支持的一个）。
const PROTOCOL_VERSION: &str = "2025-06-18";

/// `quench3d_movie` 单次允许的最大帧数。
///
/// 比 CLI 的上限（10 万帧）小得多：agent 是通过工具调用的，逐帧渲染是分钟级操作，
/// 让它一次录不完比让它默默跑十分钟更安全。
const MAX_MOVIE_FRAMES: u32 = 600;

/// 服务器。
pub struct McpServer {
    /// 缺省的 LLM 配置（可被工具参数覆盖）。
    pub llm: LlmConfig,
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}

impl McpServer {
    /// 用环境变量里的 LLM 配置创建。
    pub fn new() -> Self {
        Self {
            llm: LlmConfig::from_env(),
        }
    }

    /// 处理一条 JSON-RPC 消息；返回 `None` 表示这是通知，不需要回复。
    pub fn handle(&mut self, request: &Value) -> Option<Value> {
        let id = request.get("id").cloned();
        let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        // 通知（没有 id）一律不回复
        if id.is_none() && method.starts_with("notifications/") {
            return None;
        }

        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": "quench3d",
                    "version": env!("CARGO_PKG_VERSION"),
                    "title": "Quench3D 3D 引擎 / 物理仿真"
                },
                "instructions": "用 quench3d_schema 读取完整能力清单；场景可以内联 JSON 或文件路径。\
                                 推荐流程：quench3d_schema → quench3d_validate → quench3d_render/quench3d_simulate；\
                                 想知道射线或某块区域里有什么，用 quench3d_query（不需要 GPU，只读）。"
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_definitions() })),
            "tools/call" => self.call_tool(&params),
            "resources/list" => Ok(json!({ "resources": [] })),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            _ => Err((-32601, format!("不支持的方法: {method}"))),
        };

        let response = match result {
            Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
            Err((code, message)) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": code, "message": message }
            }),
        };
        Some(response)
    }

    fn call_tool(&mut self, params: &Value) -> Result<Value, (i32, String)> {
        let name = params
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| (-32602, "tools/call 缺少 name".to_string()))?;
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        self.dispatch_tool(name, &args)
    }

    /// 执行一个工具。
    ///
    /// MCP 的 `tools/call` 与命令行的 agent 循环都走这里，保证「agent 看到的工具行为」
    /// 与「MCP 客户端看到的行为」完全一致。
    pub fn dispatch_tool(&mut self, name: &str, args: &Value) -> Result<Value, (i32, String)> {
        match name {
            "quench3d_schema" => Ok(text_result(&crate::schema::schema().to_string())),
            "quench3d_doctor" => {
                let report = runner::run_doctor();
                let mut text = format!(
                    "平台 {} / {}，逻辑核心 {}\nGPU: {}（{}，{}）\n软件光栅: {}\n",
                    report.os,
                    report.arch,
                    report.cpu_threads,
                    if report.gpu_available {
                        &report.adapter
                    } else {
                        "不可用"
                    },
                    report.backend,
                    report.device_type,
                    report.software_rasterizer
                );
                for note in &report.notes {
                    text.push_str(&format!("提示: {note}\n"));
                }
                Ok(structured_result(
                    &text,
                    serde_json::to_value(&report).unwrap_or(Value::Null),
                ))
            }
            "quench3d_info" => {
                let path = arg_str(args, "path")
                    .ok_or_else(|| (-32602, "quench3d_info 需要 path 参数".to_string()))?;
                match runner::run_info(std::path::Path::new(&path)) {
                    Ok(info) => {
                        let text = format!(
                            "{}: {} 个子网格、{} 顶点、{} 三角形、{} 材质、{} 贴图\n包围盒 min {:?} max {:?} 尺寸 {:?}",
                            info.path,
                            info.meshes,
                            info.vertices,
                            info.triangles,
                            info.materials,
                            info.images,
                            info.bounds_min,
                            info.bounds_max,
                            info.bounds_size
                        );
                        Ok(structured_result(
                            &text,
                            serde_json::to_value(&info).unwrap_or(Value::Null),
                        ))
                    }
                    Err(e) => Ok(error_result(&ErrorInfo::from_run(&e))),
                }
            }
            "quench3d_validate" => match scene_from_args(args) {
                Ok(doc) => {
                    let diags = doc.validate();
                    let errors = diags
                        .iter()
                        .filter(|d| d.severity == Severity::Error)
                        .count();
                    let warnings = diags.len() - errors;
                    let text = if errors == 0 {
                        format!("场景合法（{warnings} 条警告）")
                    } else {
                        let mut t = format!("场景有 {errors} 个错误、{warnings} 条警告：\n");
                        for d in diags
                            .iter()
                            .filter(|d| d.severity == Severity::Error)
                            .take(10)
                        {
                            t.push_str(&format!("- {}: {}\n", d.path, d.message));
                        }
                        t
                    };
                    Ok(structured_result(
                        &text,
                        json!({
                            "valid": errors == 0,
                            "errors": errors,
                            "warnings": warnings,
                            "diagnostics": diags,
                        }),
                    ))
                }
                Err(info) => Ok(error_result(&info)),
            },
            "quench3d_render" => {
                let doc = match scene_from_args(args) {
                    Ok(d) => d,
                    Err(info) => return Ok(error_result(&info)),
                };
                let output = arg_str(args, "output").map(std::path::PathBuf::from);
                let policy = runner::OutputPolicy {
                    no_clobber: arg_bool(args, "no_clobber"),
                    dry_run: arg_bool(args, "dry_run"),
                };
                match runner::run_render(&doc, output.as_deref(), policy) {
                    Ok(report) if report.dry_run => Ok(json!({
                        "content": vec![json!({ "type": "text", "text": format!(
                            "--dry-run：没有渲染、没有写盘。本来会写入 {}（{}×{}），物理跑了 {} 步",
                            report.output,
                            report.width,
                            report.height,
                            report.simulate.as_ref().map(|s| s.steps).unwrap_or(0),
                        ) })],
                        "structuredContent": serde_json::to_value(&report).unwrap_or(Value::Null),
                        "isError": false
                    })),
                    Ok(report) => {
                        let text = format!(
                            "已渲染 {}（{}×{}，{} 字节）\n绘制调用 {} · 实例 {} · 三角形 {} · 阴影实例 {}\n相机位置 {:?} · 耗时 {} ms",
                            report.output,
                            report.width,
                            report.height,
                            report.bytes,
                            report.draw_calls,
                            report.instances,
                            report.triangles,
                            report.shadow_instances,
                            report.camera_position,
                            report.elapsed_ms
                        );
                        let mut content = vec![json!({ "type": "text", "text": text })];
                        if let Some(image) = image_content(std::path::Path::new(&report.output)) {
                            content.push(image);
                        }
                        Ok(json!({
                            "content": content,
                            "structuredContent": serde_json::to_value(&report).unwrap_or(Value::Null),
                            "isError": false
                        }))
                    }
                    Err(e) => Ok(error_result(&ErrorInfo::from_run(&e))),
                }
            }
            "quench3d_movie" => {
                let doc = match scene_from_args(args) {
                    Ok(d) => d,
                    Err(info) => return Ok(error_result(&info)),
                };
                // agent 场景下必须给上限：录制是逐帧渲染，帧数一多就是分钟级。
                let frames = args.get("frames").and_then(|v| v.as_u64()).unwrap_or(30) as u32;
                if frames == 0 || frames > MAX_MOVIE_FRAMES {
                    return Ok(error_result(&ErrorInfo::new(
                        "invalid",
                        format!("frames 需要在 1..={MAX_MOVIE_FRAMES} 之间，当前 {frames}"),
                    )));
                }
                let steps_per_frame = args
                    .get("steps_per_frame")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1)
                    .clamp(1, 1000) as u32;
                let warmup_frames = args
                    .get("warmup")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    .min(1000) as u32;
                let frames_dir = arg_str(args, "frames_dir")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| std::path::PathBuf::from("quench3d_frames"));
                let dt = doc.simulate.dt;
                let fps = args
                    .get("fps")
                    .and_then(|v| v.as_f64())
                    .map(|f| f as f32)
                    .unwrap_or_else(|| {
                        if dt > 0.0 {
                            1.0 / (dt * steps_per_frame as f32)
                        } else {
                            30.0
                        }
                    });
                let opts = runner::MovieOptions {
                    frames,
                    fps,
                    frames_dir,
                    steps_per_frame,
                    warmup_frames,
                    width: doc.render.width,
                    height: doc.render.height,
                    // 工具调用走 PNG 序列：可断点续录，也不把大文件塞进对话
                    encoder: runner::MovieEncoder::Png,
                    output: None,
                    no_clobber: arg_bool(args, "no_clobber"),
                    dry_run: arg_bool(args, "dry_run"),
                };
                match runner::run_movie(&doc, &opts) {
                    Ok(report) if report.dry_run => Ok(json!({
                        "content": vec![json!({ "type": "text", "text": format!(
                            "--dry-run：没有渲染、没有写盘。本来会录 {} 帧（共 {} 步）到 {}，trace 写到 {}",
                            report.frames, report.total_steps, report.frames_dir, report.trace,
                        ) })],
                        "structuredContent": serde_json::to_value(&report).unwrap_or(Value::Null),
                        "isError": false
                    })),
                    Ok(report) => {
                        let text = format!(
                            "已录制 {} 帧到 {}（{}×{}，{} 字节，{}）\n轨迹 {} · 时长 {:.2} s @ {:.2} fps（{} 步/帧 × {} s）\n像素摘要 {}\n下一步：{}",
                            report.frames,
                            report.frames_dir,
                            report.width,
                            report.height,
                            report.bytes,
                            if report.skipped > 0 {
                                format!("跳过已存在 {} 帧", report.skipped)
                            } else {
                                "全部新写".to_string()
                            },
                            report.trace,
                            report.duration,
                            report.fps,
                            report.steps_per_frame,
                            report.dt,
                            report.digest,
                            report.ffmpeg
                        );
                        let mut content = vec![json!({ "type": "text", "text": text })];
                        // 顺带把末帧当作图像返回：模型能直接看到仿真结束时的画面。
                        if report.frames > 0 {
                            let last = std::path::Path::new(&report.frames_dir)
                                .join(format!("frame_{:05}.png", report.frames - 1));
                            if let Some(image) = image_content(&last) {
                                content.push(image);
                            }
                        }
                        Ok(json!({
                            "content": content,
                            "structuredContent": serde_json::to_value(&report).unwrap_or(Value::Null),
                            "isError": false
                        }))
                    }
                    Err(e) => Ok(error_result(&ErrorInfo::from_run(&e))),
                }
            }
            "quench3d_query" => {
                let doc = match scene_from_args(args) {
                    Ok(d) => d,
                    Err(info) => return Ok(error_result(&info)),
                };
                let kind = match (arg_str(args, "ray"), arg_str(args, "overlap")) {
                    (Some(_), Some(_)) => {
                        return Ok(error_result(&ErrorInfo::new(
                            "invalid",
                            "ray 与 overlap 只能给一个（二者选其一）",
                        )))
                    }
                    (None, None) => {
                        return Ok(error_result(&ErrorInfo::new(
                            "invalid",
                            "需要 ray（如 0,5,0,0,-1,0）或 overlap（如 box:1,2,1@0,1,0）之一",
                        )))
                    }
                    (Some(text), None) => match crate::runner::parse_ray_query(&text) {
                        Ok(k) => k,
                        Err(msg) => return Ok(error_result(&ErrorInfo::new("invalid", msg))),
                    },
                    (None, Some(text)) => match crate::runner::parse_overlap_query(&text) {
                        Ok(k) => k,
                        Err(msg) => return Ok(error_result(&ErrorInfo::new("invalid", msg))),
                    },
                };
                let mask = args
                    .get("mask")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
                    .unwrap_or(u32::MAX);
                let sensors = args
                    .get("sensors")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let max_distance = args
                    .get("max_distance")
                    .and_then(|v| v.as_f64())
                    .map(|v| v as f32)
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .unwrap_or(crate::runner::DEFAULT_QUERY_MAX);
                let all = args.get("all").and_then(|v| v.as_bool()).unwrap_or(false);
                let steps = args.get("steps").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                match crate::runner::run_query(
                    &doc,
                    &kind,
                    crate::runner::QueryOptions {
                        mask,
                        sensors,
                        max_distance,
                        all,
                        steps,
                        use_index: false,
                    },
                ) {
                    Ok(report) => {
                        let mut text = format!(
                            "{}：命中 {} 个（刚体 {}，检查过 {} 个）\n",
                            match report.query.as_str() {
                                "overlap" => format!(
                                    "重叠查询 {} @ {:?}",
                                    report.shape.clone().unwrap_or_default(),
                                    report.at.unwrap_or([0.0; 3])
                                ),
                                _ => format!(
                                    "射线查询 {:?} → {:?}",
                                    report.origin.unwrap_or([0.0; 3]),
                                    report.direction.unwrap_or([0.0; 3])
                                ),
                            },
                            report.count,
                            report.bodies,
                            report.tested
                        );
                        for h in &report.hits {
                            text.push_str(&format!(
                                "- {}（{}）距离 {:.3} 点 ({:.3},{:.3},{:.3}) 法线 ({:.3},{:.3},{:.3}) 层 {}{}\n",
                                h.name,
                                h.kind,
                                h.distance,
                                h.point[0],
                                h.point[1],
                                h.point[2],
                                h.normal[0],
                                h.normal[1],
                                h.normal[2],
                                h.layer,
                                if h.sensor { " 传感器" } else { "" }
                            ));
                        }
                        Ok(structured_result(
                            &text,
                            serde_json::to_value(&report).unwrap_or(Value::Null),
                        ))
                    }
                    Err(e) => Ok(error_result(&ErrorInfo::from_run(&e))),
                }
            }
            "quench3d_simulate" => {
                let mut doc = match scene_from_args(args) {
                    Ok(d) => d,
                    Err(info) => return Ok(error_result(&info)),
                };
                if let Some(steps) = args.get("steps").and_then(|v| v.as_u64()) {
                    doc.simulate.steps = steps as u32;
                }
                if let Some(dt) = args.get("dt").and_then(|v| v.as_f64()) {
                    doc.simulate.dt = dt as f32;
                }
                // 可选：跟踪若干刚体的轨迹（agent 用它判断「会不会倒 / 滑多远」）
                let trace: Vec<String> = args
                    .get("trace")
                    .map(|v| match v {
                        Value::String(s) => s
                            .split(',')
                            .map(|p| p.trim().to_string())
                            .filter(|p| !p.is_empty())
                            .collect(),
                        Value::Array(items) => items
                            .iter()
                            .filter_map(|i| i.as_str().map(|s| s.to_string()))
                            .collect(),
                        _ => Vec::new(),
                    })
                    .unwrap_or_default();
                let trace_every = args
                    .get("trace_every")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1)
                    .max(1) as u32;
                match runner::run_simulate_traced(&doc, &trace, trace_every) {
                    Ok((report, trajectories)) => {
                        let mut text = format!(
                            "步进 {} 步（dt={:.5}s）：{} 个刚体（动态 {}）、{} 个接触、{} 个关节\n最大速度 {:.3} m/s · 最低点 y {:.3} · NaN {}\n",
                            report.steps,
                            report.dt,
                            report.bodies,
                            report.dynamic,
                            report.contacts,
                            report.joints,
                            report.max_speed,
                            report.lowest_y,
                            report.nan_bodies
                        );
                        for state in &report.states {
                            if state.kind == "dynamic" {
                                text.push_str(&format!(
                                    "- {} 位置 ({:.2},{:.2},{:.2}) 速度 {:.2} 位移 {:.2}{}\n",
                                    state.name,
                                    state.position[0],
                                    state.position[1],
                                    state.position[2],
                                    state.speed,
                                    state.displacement,
                                    if state.asleep { " [已休眠]" } else { "" }
                                ));
                            }
                        }
                        for w in &report.warnings {
                            text.push_str(&format!("警告: {w}\n"));
                        }
                        for t in &trajectories {
                            text.push_str(&format!(
                                "轨迹 {}（{}）：起点 ({:.2},{:.2},{:.2}) → 终点 ({:.2},{:.2},{:.2})；下坠 {:.2} m、爬升 {:.2} m、位移 {:.2} m、路程 {:.2} m、最高速 {:.2} m/s、最大倾斜 {:.1}°{}\n",
                                t.name,
                                t.shape,
                                t.start[0],
                                t.start[1],
                                t.start[2],
                                t.end[0],
                                t.end[1],
                                t.end[2],
                                t.fall,
                                t.rise,
                                t.travel,
                                t.path_length,
                                t.max_speed,
                                t.max_tilt_deg,
                                if !t.tilt_meaningful {
                                    ""
                                } else if t.tipped_over {
                                    "（翻倒）"
                                } else {
                                    "（站住了）"
                                }
                            ));
                        }
                        let mut payload = serde_json::to_value(&report).unwrap_or(Value::Null);
                        if !trajectories.is_empty() {
                            payload["trajectories"] =
                                serde_json::to_value(&trajectories).unwrap_or(Value::Null);
                        }
                        Ok(structured_result(&text, payload))
                    }
                    Err(e) => Ok(error_result(&ErrorInfo::from_run(&e))),
                }
            }
            "quench3d_ask" => self.call_ask(args),
            other => Err((-32602, format!("未知工具: {other}"))),
        }
    }

    fn call_ask(&mut self, args: &Value) -> Result<Value, (i32, String)> {
        let prompt = arg_str(args, "prompt")
            .ok_or_else(|| (-32602, "quench3d_ask 需要 prompt 参数".to_string()))?;
        let mut config = self.llm.clone();
        if let Some(m) = arg_str(args, "model") {
            config.model = m;
        }
        if let Some(b) = arg_str(args, "base_url") {
            config.base_url = b;
        }
        let repair_rounds = args
            .get("repair_rounds")
            .and_then(|v| v.as_u64())
            .unwrap_or(2) as u32;
        let client = LlmClient::new(config);
        let options = GenerateOptions::new(prompt, repair_rounds);
        let generated =
            match generate(&client, &options) {
                Ok(g) => g,
                Err(e) => {
                    return Ok(error_result(&ErrorInfo::new(e.kind, e.message).hint(
                        e.hint.unwrap_or_else(|| {
                            "检查大模型服务是否启动（如 `ollama serve`）".into()
                        }),
                    )))
                }
            };

        let valid = generated.valid();
        let errors = generated.errors();
        let repair_used = generated.repair_rounds_used();
        let summary = if !valid {
            format!(
                "模型 {} 的输出未通过校验（共 {} 次尝试、{errors} 个错误）",
                generated.model,
                generated.attempts_count()
            )
        } else if repair_used > 0 {
            format!(
                "模型 {} 生成场景成功（自动修复 {repair_used} 轮，共 {} 次尝试）",
                generated.model,
                generated.attempts_count()
            )
        } else {
            format!("模型 {} 一次生成了合法场景", generated.model)
        };
        let mut text = format!(
            "{summary}：\n```json\n{}\n```",
            truncate(&generated.scene_json, 4000)
        );
        if !valid {
            text.push_str("\n\n诊断（可据此让模型再修正一次）：\n");
            for d in generated.diagnostics.iter().take(8) {
                text.push_str(&format!("- {}: {}\n", d.path, d.message));
            }
        }

        let mut structured = json!({
            "valid": valid,
            "errors": errors,
            "diagnostics": generated.diagnostics,
            "attempts": generated.attempts.iter().map(|a| a.to_json()).collect::<Vec<_>>(),
            "repair_rounds": repair_used,
            "model": generated.model,
            "usage": generated.usage.map(|t| json!({ "total_tokens": t })),
            "scene": generated.scene,
        });

        // 合法的话顺手仿真一下，agent 立刻能判断物理是否合理
        if arg_bool(args, "simulate") {
            if let Some(doc) = &generated.scene {
                if let Ok(report) = runner::run_simulate(doc) {
                    text.push_str(&format!(
                        "\n\n仿真 {} 步：最大速度 {:.2} m/s，NaN {}",
                        report.steps, report.max_speed, report.nan_bodies
                    ));
                    if let Some(obj) = structured.as_object_mut() {
                        obj.insert(
                            "simulate".into(),
                            serde_json::to_value(&report).unwrap_or(Value::Null),
                        );
                    }
                }
            }
        }

        if valid {
            Ok(structured_result(&text, structured))
        } else {
            Ok(error_result(
                &ErrorInfo::new("invalid", "模型生成的场景未通过校验")
                    .with_diagnostics(generated.diagnostics),
            ))
        }
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "\n…（已截断）"
    }
}

fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn arg_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// 从工具参数里取场景：`scene`（对象或 JSON 文本）或 `scene_path`（文件）。
fn scene_from_args(args: &Value) -> Result<SceneDoc, ErrorInfo> {
    if let Some(scene) = args.get("scene") {
        let text = match scene {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        if text.trim_start().starts_with('{') {
            return SceneDoc::from_json(&text).map_err(|e| crate::report::doc_error_info(&e));
        }
        return SceneDoc::from_file(&text).map_err(|e| crate::report::doc_error_info(&e));
    }
    if let Some(path) = arg_str(args, "scene_path") {
        return SceneDoc::from_file(&path).map_err(|e| crate::report::doc_error_info(&e));
    }
    Err(ErrorInfo::new("invalid", "缺少 scene 或 scene_path 参数")
        .hint("scene 可以是内联 JSON 对象/字符串，scene_path 是文件路径"))
}

fn text_result(text: &str) -> Value {
    json!({ "content": [ { "type": "text", "text": text } ], "isError": false })
}

fn structured_result(text: &str, structured: Value) -> Value {
    json!({
        "content": [ { "type": "text", "text": text } ],
        "structuredContent": structured,
        "isError": false
    })
}

fn error_result(info: &ErrorInfo) -> Value {
    let mut text = format!("[{}] {}", info.kind, info.message);
    if let Some(path) = &info.path {
        text.push_str(&format!("\n位置: {path}"));
    }
    if let Some(hint) = &info.hint {
        text.push_str(&format!("\n建议: {hint}"));
    }
    for d in info.diagnostics.iter().take(10) {
        text.push_str(&format!("\n- {}: {}", d.path, d.message));
    }
    json!({
        "content": [ { "type": "text", "text": text } ],
        "structuredContent": serde_json::to_value(info).unwrap_or(Value::Null),
        "isError": true
    })
}

/// 把 PNG 读成 MCP 图像内容块（含 base64）。
fn image_content(path: &std::path::Path) -> Option<Value> {
    let data = std::fs::read(path).ok()?;
    Some(json!({
        "type": "image",
        "data": base64_encode(&data),
        "mimeType": "image/png"
    }))
}

/// 标准 base64 编码（避免为此引入依赖）。
pub fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(n >> 6) as usize & 63] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[n as usize & 63] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// 工具定义（`tools/list`）。
pub fn tool_definitions() -> Vec<Value> {
    let mut tools = raw_tool_definitions();
    annotate_tools(&mut tools);
    tools
}

/// 给每个工具补 MCP 的 `annotations` 与 `outputSchema`。
///
/// agent 靠这些**机器可读**的提示决定"能不能并行、能不能重试、要不要先确认"，
/// 而不是读一遍自然语言描述去猜：
///
/// * `readOnlyHint`：绝不写盘（可以并行、可以随便重复调用）；
/// * `destructiveHint`：会覆盖已有文件（配 `no_clobber: true` 就安全了）；
/// * `idempotentHint`：同样输入重复调用结果一致（渲染/仿真的确定性使然；
///   大模型两条路 `ask`/`chat` 不是——它们每次都可能不一样）；
/// * `openWorldHint`：会出网。
///
/// `outputSchema` 描述 `structuredContent` 的字段，agent 可以据此直接取数，
/// 不必先跑一次再猜结构。
fn annotate_tools(tools: &mut [Value]) {
    let props = |names: &[(&str, &str)]| {
        let mut map = serde_json::Map::new();
        for (n, t) in names {
            map.insert((*n).to_string(), json!({ "type": t }));
        }
        json!({ "type": "object", "properties": Value::Object(map) })
    };
    for tool in tools.iter_mut() {
        let name = tool["name"].as_str().unwrap_or_default().to_string();
        let (read_only, destructive, open_world, idempotent, output) = match name.as_str() {
            "quench3d_schema" => (
                true,
                false,
                false,
                true,
                props(&[
                    ("commands", "array"),
                    ("scene", "object"),
                    ("exit_codes", "object"),
                ]),
            ),
            "quench3d_doctor" => (
                true,
                false,
                false,
                true,
                props(&[
                    ("platform", "string"),
                    ("adapter", "string"),
                    ("backend", "string"),
                    ("software", "boolean"),
                ]),
            ),
            "quench3d_info" => (
                true,
                false,
                false,
                true,
                props(&[
                    ("path", "string"),
                    ("format", "string"),
                    ("meshes", "integer"),
                    ("materials", "integer"),
                    ("images", "integer"),
                    ("vertices", "integer"),
                    ("triangles", "integer"),
                    ("bounds_size", "array"),
                ]),
            ),
            "quench3d_validate" => (
                true,
                false,
                false,
                true,
                props(&[
                    ("valid", "boolean"),
                    ("errors", "integer"),
                    ("warnings", "integer"),
                    ("diagnostics", "array"),
                ]),
            ),
            "quench3d_simulate" => (
                true,
                false,
                false,
                true,
                props(&[
                    ("steps", "integer"),
                    ("bodies", "integer"),
                    ("max_speed", "number"),
                    ("states", "array"),
                    ("probes", "array"),
                    ("trajectories", "array"),
                    ("warnings", "array"),
                ]),
            ),
            "quench3d_query" => (
                true,
                false,
                false,
                true,
                props(&[
                    ("query", "string"),
                    ("count", "integer"),
                    ("hits", "array"),
                    ("bodies", "integer"),
                    ("tested", "integer"),
                    ("indexed", "boolean"),
                    ("fallback", "boolean"),
                    ("warnings", "array"),
                ]),
            ),
            "quench3d_render" => (
                false,
                true,
                false,
                true,
                props(&[
                    ("output", "string"),
                    ("width", "integer"),
                    ("height", "integer"),
                    ("bytes", "integer"),
                    ("objects", "integer"),
                    ("simulate", "object"),
                    ("probes", "array"),
                    ("dry_run", "boolean"),
                    ("would_write", "array"),
                ]),
            ),
            "quench3d_movie" => (
                false,
                true,
                false,
                true,
                props(&[
                    ("frames_dir", "string"),
                    ("frames", "integer"),
                    ("digest", "string"),
                    ("first_frame", "object"),
                    ("last_frame", "object"),
                    ("probes", "array"),
                    ("notes", "array"),
                    ("dry_run", "boolean"),
                ]),
            ),
            "quench3d_ask" => (
                false,
                true,
                true,
                false,
                props(&[
                    ("scene_json", "string"),
                    ("valid", "boolean"),
                    ("errors", "integer"),
                    ("attempts", "integer"),
                    ("usage", "object"),
                ]),
            ),
            other => (
                // 新工具必须自己声明；忘了声明就会被这条断言挡住（见单测）
                false,
                true,
                true,
                false,
                json!({ "type": "object", "description": format!("未声明（{other}）") }),
            ),
        };
        tool["annotations"] = json!({
            "title": tool["title"].clone(),
            "readOnlyHint": read_only,
            "destructiveHint": destructive,
            "idempotentHint": idempotent,
            "openWorldHint": open_world,
        });
        tool["outputSchema"] = output;
    }
}

fn raw_tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "quench3d_schema",
            "title": "读取 Quench3D 能力清单",
            "description": "返回全部命令、场景文件语法、单位约定、退出码。生成场景前先读它。",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "quench3d_doctor",
            "title": "环境自检",
            "description": "检查平台、CPU、GPU 适配器与图形后端；渲染前可用它确认环境是否可用。",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "quench3d_info",
            "title": "检查模型文件",
            "description": "读取 glTF/GLB/OBJ 的网格数、顶点数、三角形数、材质数与包围盒。",
            "inputSchema": {
                "type": "object",
                "properties": { "path": { "type": "string", "description": "模型文件路径" } },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "quench3d_validate",
            "title": "校验场景",
            "description": "校验场景 JSON，返回错误与警告列表（带 JSON 路径），不会渲染。建议生成场景后先调用它。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scene": { "description": "场景对象或 JSON 字符串", "type": ["object", "string"] },
                    "scene_path": { "type": "string", "description": "场景文件路径（与 scene 二选一）" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "quench3d_render",
            "title": "渲染场景为 PNG",
            "description": "无窗口渲染场景（可选先步进物理），返回文本报告与 PNG 图像内容块。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scene": { "description": "场景对象或 JSON 字符串", "type": ["object", "string"] },
                    "scene_path": { "type": "string" },
                    "output": { "type": "string", "description": "输出 PNG 路径，缺省 quench3d_render.png" },
                    "width": { "type": "integer", "description": "覆盖宽度（≤1280 更稳）" },
                    "height": { "type": "integer" },
                    "steps": { "type": "integer", "description": "覆盖 simulate.steps" },
                    "dry_run": { "type": "boolean", "description": "只校验 + 跑物理，不渲染不写盘（缺省 false）" },
                    "no_clobber": { "type": "boolean", "description": "输出已存在就报错而不是覆盖；多 agent 并行时建议传 true" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "quench3d_movie",
            "title": "录制仿真视频帧序列",
            "description": "逐帧推进物理并渲染成 PNG 序列（<frames_dir>/frame_%05d.png）与 trace.jsonl，返回可直接执行的 ffmpeg 命令。帧数由物理步决定而非墙钟，所以同一场景两次录制逐像素一致（digest 可比对）。会写盘：缺省只录 30 帧、上限 600 帧。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scene": { "description": "场景对象或 JSON 字符串", "type": ["object", "string"] },
                    "scene_path": { "type": "string", "description": "场景文件路径（与 scene 二选一）" },
                    "frames": { "type": "integer", "description": "输出帧数（缺省 30，上限 600）" },
                    "steps_per_frame": { "type": "integer", "description": "每帧推进的物理步数（缺省 1）" },
                    "frames_dir": { "type": "string", "description": "帧序列目录（缺省 quench3d_frames）" },
                    "fps": { "type": "number", "description": "视频帧率（缺省 = 等价帧率 1/(dt×steps_per_frame)）" },
                    "warmup": { "type": "integer", "description": "预热帧数（缺省 0，只推进不输出）" },
                    "width": { "type": "integer", "description": "覆盖宽度（≤640 更省时间）" },
                    "height": { "type": "integer" },
                    "dry_run": { "type": "boolean", "description": "只校验 + 跑物理，不渲染不写盘（缺省 false）" },
                    "no_clobber": { "type": "boolean", "description": "帧目录/输出已存在就报错而不是复用；多 agent 并行时建议传 true" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "quench3d_simulate",
            "title": "物理仿真（不需要 GPU）",
            "description": "只跑物理，返回每个刚体的位置、速度、位移、休眠与接触统计。适合快速验证物理是否合理。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scene": { "description": "场景对象或 JSON 字符串", "type": ["object", "string"] },
                    "scene_path": { "type": "string" },
                    "steps": { "type": "integer", "description": "覆盖 simulate.steps" },
                    "dt": { "type": "number", "description": "覆盖 simulate.dt" },
                    "trace": {
                        "description": "要跟踪轨迹的刚体名（数组或逗号分隔字符串）；返回起点/终点、下坠、爬升、位移、路程、最高速、最大倾斜与是否翻倒",
                        "type": ["array", "string"],
                        "items": { "type": "string" }
                    },
                    "trace_every": { "type": "integer", "description": "轨迹采样间隔（步，缺省 1）" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "quench3d_query",
            "title": "几何查询：射线 / 形状重叠（不需要 GPU，只读）",
            "description": "在场景上做一次查询，不改动场景、不渲染。ray 给 \"ox,oy,oz,dx,dy,dz\"（配合 all 返回沿路的全部命中，按距离升序；缺省只给最近的一个）；overlap 给 \"sphere:0.5\" / \"box:1,2,1\" / \"capsule:0.6,0.3\" / \"cylinder:0.5,0.5\" / \"cone:0.5,0.5\" / \"plane:0,1,0\"，可接 @x,y,z 与 ~rx,ry,rz（度）。层过滤是单向的（mask 只看被查方在不在掩码里）；sensors=false 可排除触发区。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scene": { "description": "场景对象或 JSON 字符串", "type": ["object", "string"] },
                    "scene_path": { "type": "string" },
                    "ray": { "type": "string", "description": "射线：ox,oy,oz,dx,dy,dz（与 overlap 二选一）" },
                    "overlap": { "type": "string", "description": "形状重叠：sphere:0.5 / box:1,2,1 / capsule:0.6,0.3 / cylinder:0.5,0.5 / cone:0.5,0.5 / plane:0,1,0，可接 @x,y,z 与 ~rx,ry,rz（度）" },
                    "all": { "type": "boolean", "description": "ray 返回全部命中（缺省 false，只给最近的一个）；对 overlap 无意义" },
                    "mask": { "type": "integer", "description": "层掩码（单向判据），缺省 4294967295 全查" },
                    "sensors": { "type": "boolean", "description": "是否把传感器（触发区）算作可命中，缺省 true" },
                    "max_distance": { "type": "number", "description": "最远距离（ray）/ 距离上限（overlap 只报距离不超过它的刚体），缺省 10000" },
                    "steps": { "type": "integer", "description": "先跑这么多步物理再查（缺省 0：按场景初始状态查）" }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "quench3d_ask",
            "title": "用自然语言生成场景",
            "description": "调用 OpenAI 兼容的大模型（本地 Ollama / llama.cpp / LM Studio / vLLM 或云端）把自然语言需求变成场景 JSON，并给出校验结果。",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "自然语言需求" },
                    "model": { "type": "string", "description": "覆盖模型名" },
                    "base_url": { "type": "string", "description": "覆盖服务地址" },
                    "simulate": { "type": "boolean", "description": "生成后顺带跑一次物理仿真" }
                },
                "required": ["prompt"],
                "additionalProperties": false
            }
        }),
    ]
}

/// 以 stdio 方式运行 MCP 服务器（读到 EOF 结束）。
pub fn serve_stdio() -> Result<(), String> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut server = McpServer::new();
    let mut writer = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| format!("读取 stdin 失败: {e}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("JSON 解析失败: {e}") }
                });
                writeln!(writer, "{response}").map_err(|e| e.to_string())?;
                writer.flush().ok();
                continue;
            }
        };
        if let Some(response) = server.handle(&request) {
            writeln!(writer, "{response}").map_err(|e| format!("写 stdout 失败: {e}"))?;
            writer
                .flush()
                .map_err(|e| format!("刷新 stdout 失败: {e}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        // PNG 文件头，确认二进制不会被破坏
        assert_eq!(base64_encode(&[0x89, 0x50, 0x4E, 0x47]), "iVBORw==");
    }

    fn init_request(id: i64) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": "initialize", "params": {} })
    }

    #[test]
    fn handshake_advertises_tools_capability() {
        let mut server = McpServer::new();
        let response = server.handle(&init_request(1)).unwrap();
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert!(response["result"]["capabilities"]["tools"].is_object());
        assert_eq!(response["result"]["serverInfo"]["name"], "quench3d");
        assert!(!response["result"]["protocolVersion"]
            .as_str()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn notifications_get_no_response() {
        let mut server = McpServer::new();
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(server.handle(&note).is_none());
    }

    #[test]
    fn tools_list_is_well_formed() {
        let mut server = McpServer::new();
        let response = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
            .unwrap();
        let tools = response["result"]["tools"].as_array().unwrap();
        assert!(tools.len() >= 7, "工具太少: {}", tools.len());
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            assert!(name.starts_with("quench3d_"), "工具命名不规范: {name}");
            assert!(
                tool["description"].as_str().is_some_and(|d| !d.is_empty()),
                "工具 {name} 缺少描述"
            );
            assert_eq!(
                tool["inputSchema"]["type"], "object",
                "工具 {name} 的 inputSchema 必须是 object"
            );
        }
    }

    /// 每个工具都要自己声明"能不能并行/重试/出网"，而且声明必须与事实一致。
    #[test]
    fn tools_declare_annotations_and_output_schema() {
        let tools = tool_definitions();
        let by_name = |n: &str| {
            tools
                .iter()
                .find(|t| t["name"] == n)
                .unwrap_or_else(|| panic!("缺少工具 {n}"))
                .clone()
        };
        for tool in &tools {
            let name = tool["name"].as_str().unwrap();
            let a = &tool["annotations"];
            for key in [
                "readOnlyHint",
                "destructiveHint",
                "idempotentHint",
                "openWorldHint",
            ] {
                assert!(a[key].is_boolean(), "工具 {name} 的 {key} 必须是布尔");
            }
            assert!(
                a["title"].as_str().is_some_and(|t| !t.is_empty()),
                "工具 {name} 的 annotations.title 不能空"
            );
            assert_eq!(
                tool["outputSchema"]["type"], "object",
                "工具 {name} 的 outputSchema 必须是 object"
            );
        }
        // 只读工具：绝不写盘 → 不能同时标"会破坏数据"
        let read_only = [
            "quench3d_schema",
            "quench3d_doctor",
            "quench3d_info",
            "quench3d_validate",
            "quench3d_simulate",
            "quench3d_query",
        ];
        for n in read_only {
            let t = by_name(n);
            assert_eq!(t["annotations"]["readOnlyHint"], true, "{n} 应当只读");
            assert_eq!(t["annotations"]["destructiveHint"], false, "{n} 不写盘");
            assert_eq!(t["annotations"]["openWorldHint"], false, "{n} 不出网");
        }
        // 写盘工具
        for n in ["quench3d_render", "quench3d_movie"] {
            let t = by_name(n);
            assert_eq!(t["annotations"]["readOnlyHint"], false, "{n} 会写盘");
            assert_eq!(
                t["annotations"]["destructiveHint"], true,
                "{n} 可能覆盖文件"
            );
        }
        // 出网工具：大模型两条路，且都不幂等
        // （MCP 工具里只有 ask 会出网；chat 是 CLI 的 agent 循环，不在这里）
        let t = by_name("quench3d_ask");
        assert_eq!(t["annotations"]["openWorldHint"], true, "ask 会出网");
        assert_eq!(t["annotations"]["idempotentHint"], false, "ask 不幂等");
        assert_eq!(
            tools.len(),
            9,
            "工具数量变了就要同步这份断言：{}",
            tools.len()
        );
        // 幂等的：仿真/渲染的确定性
        for n in [
            "quench3d_simulate",
            "quench3d_query",
            "quench3d_render",
            "quench3d_movie",
        ] {
            assert_eq!(by_name(n)["annotations"]["idempotentHint"], true, "{n}");
        }
        // outputSchema 要给出真字段，而不是空壳
        assert!(
            by_name("quench3d_simulate")["outputSchema"]["properties"]["states"].is_object(),
            "simulate 的 outputSchema 应当包含 states"
        );
        assert!(
            by_name("quench3d_query")["outputSchema"]["properties"]["hits"].is_object(),
            "query 的 outputSchema 应当包含 hits"
        );
    }

    #[test]
    fn unknown_method_returns_jsonrpc_error() {
        let mut server = McpServer::new();
        let response = server
            .handle(&json!({ "jsonrpc": "2.0", "id": 3, "method": "nope/nope" }))
            .unwrap();
        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn validate_tool_reports_invalid_scene_with_paths() {
        let mut server = McpServer::new();
        let response = server
            .handle(&json!({
                "jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": {
                    "name": "quench3d_validate",
                    "arguments": { "scene": {
                        "objects": [ { "name": "a", "material": "missing", "body": "dynamic" } ]
                    } }
                }
            }))
            .unwrap();
        let result = &response["result"];
        assert_eq!(result["isError"], false, "校验本身应当成功返回");
        assert_eq!(result["structuredContent"]["valid"], false);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("objects[0].material"),
            "文本里应带路径: {text}"
        );
    }

    #[test]
    fn simulate_tool_runs_pure_physics_without_gpu() {
        let mut server = McpServer::new();
        let response = server
            .handle(&json!({
                "jsonrpc": "2.0", "id": 5, "method": "tools/call",
                "params": {
                    "name": "quench3d_simulate",
                    "arguments": {
                        "steps": 120,
                        "scene": {
                            "ground": { "size": 20 },
                            "objects": [{
                                "name": "球", "shape": { "type": "sphere", "radius": 0.5 },
                                "position": [0, 5, 0], "body": "dynamic"
                            }]
                        }
                    }
                }
            }))
            .unwrap();
        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["nan_bodies"], 0);
        assert_eq!(structured["bodies"], 2, "地面 + 球");
        let y = structured["states"][1]["position"][1].as_f64().unwrap();
        assert!((0.4..0.7).contains(&y), "球应停在半径高度附近，实际 {y}");
    }

    #[test]
    fn simulate_tool_can_trace_a_body() {
        let mut server = McpServer::new();
        let response = server
            .handle(&json!({
                "jsonrpc": "2.0", "id": 9, "method": "tools/call",
                "params": {
                    "name": "quench3d_simulate",
                    "arguments": {
                        "steps": 120,
                        "trace": ["塔"],
                        "scene": {
                            "ground": { "size": 20 },
                            "objects": [{
                                "name": "塔",
                                "shape": { "type": "box", "size": [0.6, 1.5, 0.6] },
                                "position": [0, 3.0, 0],
                                "body": "dynamic"
                            }]
                        }
                    }
                }
            }))
            .unwrap();
        let structured = &response["result"]["structuredContent"];
        let trajectories = structured["trajectories"].as_array().unwrap();
        assert_eq!(trajectories.len(), 1);
        let t = &trajectories[0];
        assert_eq!(t["name"], "塔");
        assert_eq!(t["shape"], "box");
        assert!(t["fall"].as_f64().unwrap() > 1.0, "箱子应当掉下来");
        assert_eq!(t["tipped_over"], false, "竖着落下不该判定为翻倒");
        assert!(t["samples"].as_array().unwrap().len() >= 100);
        // 文本里也要有轨迹行，纯文本 agent 才看得到
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("轨迹 塔"), "{text}");
    }

    #[test]
    fn tool_call_with_unknown_tool_is_an_error() {
        let mut server = McpServer::new();
        let response = server
            .handle(&json!({
                "jsonrpc": "2.0", "id": 6, "method": "tools/call",
                "params": { "name": "quench3d_nope", "arguments": {} }
            }))
            .unwrap();
        assert_eq!(response["error"]["code"], -32602);
    }

    #[test]
    fn missing_scene_argument_is_reported_as_invalid() {
        let mut server = McpServer::new();
        let response = server
            .handle(&json!({
                "jsonrpc": "2.0", "id": 7, "method": "tools/call",
                "params": { "name": "quench3d_simulate", "arguments": {} }
            }))
            .unwrap();
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(response["result"]["structuredContent"]["kind"], "invalid");
    }
}
