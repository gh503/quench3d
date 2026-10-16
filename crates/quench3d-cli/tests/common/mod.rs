//! 测试用的假 OpenAI 服务：验证 HTTP 客户端与 `ask` 命令，不需要真的模型。
//!
//! 用 `std::net::TcpListener` 起一个只实现 `/v1/chat/completions` 的最小服务，
//! 支持 Content-Length 与 chunked 两种响应，并把收到的请求体回传给测试。

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 假服务：记录收到的请求体，按给定内容回复。
pub struct MockLlm {
    /// 可以直接给 `--base-url` 的地址（含 `/v1`）。
    pub base_url: String,
    /// 收到的请求体（每条消息一个）。
    pub requests: Receiver<String>,
    stop: Arc<AtomicBool>,
}

impl MockLlm {
    /// 启动假服务。`reply` 会被包成标准 OpenAI 响应里的 `choices[0].message.content`。
    pub fn start(reply: &str) -> Self {
        Self::start_with(reply.to_string(), 200, false)
    }

    /// 启动假服务并指定状态码与是否使用 chunked 传输（仍然包装成 OpenAI 响应）。
    pub fn start_with(reply: String, status: u16, chunked: bool) -> Self {
        Self::start_inner(vec![reply], status, chunked, true)
    }

    /// 启动假服务，把 `body` 原样当响应体返回（用于测试非标准协议或坏 JSON）。
    pub fn start_raw(body: &str) -> Self {
        Self::start_inner(vec![body.to_string()], 200, false, false)
    }

    /// 按顺序返回多条回复（第 n 个请求拿第 n 条，用完后一直重复最后一条）。
    ///
    /// `body` 为 `true` 时表示回复已经是完整的响应体（如带 tool_calls 的响应）。
    pub fn start_sequence(replies: Vec<String>, raw: bool) -> Self {
        Self::start_inner(replies, 200, false, !raw)
    }

    fn start_inner(replies: Vec<String>, status: u16, chunked: bool, wrap: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑定本地端口");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let (tx, rx) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let replies = Arc::new(replies);
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        std::thread::spawn(move || {
            let started = Instant::now();
            while !stop_thread.load(Ordering::Relaxed)
                && started.elapsed() < Duration::from_secs(30)
            {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // **必须显式设回阻塞**：BSD / macOS 的 `accept()` 会**继承** listener 的
                        // `O_NONBLOCK`（Linux 的 `accept()` 不继承，所以本地永远复现不出来）。
                        // 不设的话，`read_request` 里的 `set_read_timeout` 在非阻塞 socket 上
                        // 不生效：请求字节只要还没到就立刻 `WouldBlock` → 被当成"对端没发请求"
                        // → 线程退出、socket 关闭 → 客户端读到
                        // `Connection reset by peer (os error 54)`。这就是 macOS 上偶发红的
                        // 那一类（以前只在 accept 出错时补过重试，治的是另一个症状）。
                        if stream.set_nonblocking(false).is_err() {
                            continue;
                        }
                        let tx = tx.clone();
                        let counter = counter.clone();
                        let replies = replies.clone();
                        std::thread::spawn(move || {
                            // 先读完整请求，再决定"下一条回复"：客户端可能先建连接再发请求、
                            // 甚至只建连接不发请求（重试/预连接）。按**连接**计数会让回复顺序错位，
                            // 表现为 macOS 上偶发断言失败（已经抓到过两次不同用例）。
                            // **一条连接可以承载多个请求**（HTTP/1.1 keep-alive）：
                            // 客户端的连接池会复用 socket，而以前这里只答一个请求就丢连接，
                            // macOS 上表现为"第二次请求拿到空响应/连接被关"，断言随机失败。
                            // 循环读到对端关闭（read_request 返回 None）为止。
                            while let Some(body) = read_request(stream.try_clone().unwrap()) {
                                let _ = tx.send(body);
                                let n = counter.fetch_add(1, Ordering::Relaxed);
                                let reply = replies
                                    .get(n.min(replies.len().saturating_sub(1)))
                                    .cloned()
                                    .unwrap_or_default();
                                if respond(
                                    stream.try_clone().unwrap(),
                                    status,
                                    &reply,
                                    chunked,
                                    wrap,
                                )
                                .is_err()
                                {
                                    break;
                                }
                            }
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    // 瞬时错误不能让假服务退出：非阻塞 accept() 在 macOS 上偶发
                    // `Interrupted` / `ConnectionAborted`，以前这里直接 break，
                    // 服务在测试中途死掉 → 后续请求失败 → 表现为"偶发红"。
                    Err(ref e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::Interrupted
                                | std::io::ErrorKind::ConnectionAborted
                                | std::io::ErrorKind::ConnectionReset
                        ) =>
                    {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            requests: rx,
            stop,
        }
    }

    /// 等一条请求（超时返回 `None`）。
    pub fn next_request(&self, timeout: Duration) -> Option<String> {
        self.requests.recv_timeout(timeout).ok()
    }

    /// 收工。
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for MockLlm {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 读一个完整的 HTTP 请求，返回请求体。
fn read_request(stream: TcpStream) -> Option<String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    reader.read_exact(&mut body).ok()?;
    Some(String::from_utf8_lossy(&body).to_string())
}

/// 回一个 OpenAI 风格的响应。
fn respond(
    mut stream: TcpStream,
    status: u16,
    content: &str,
    chunked: bool,
    wrap: bool,
) -> std::io::Result<()> {
    let json = if !wrap {
        content.to_string()
    } else {
        serde_json::json!({
            "id": "chatcmpl-mock",
            "object": "chat.completion",
            "model": "mock-model",
            "choices": [ { "index": 0, "message": { "role": "assistant", "content": content }, "finish_reason": "stop" } ],
            "usage": { "prompt_tokens": 11, "completion_tokens": 22, "total_tokens": 33 }
        })
        .to_string()
    };

    let head = if chunked {
        format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        )
    } else {
        format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            json.len()
        )
    };
    stream.write_all(head.as_bytes())?;
    if chunked {
        // 故意拆成两块，逼客户端正确处理分块边界
        let bytes = json.as_bytes();
        let mid = bytes.len() / 2;
        for part in [&bytes[..mid], &bytes[mid..]] {
            write!(stream, "{:x}\r\n", part.len())?;
            stream.write_all(part)?;
            stream.write_all(b"\r\n")?;
        }
        stream.write_all(b"0\r\n\r\n")?;
    } else {
        stream.write_all(json.as_bytes())?;
    }
    stream.flush()
}

/// 一段能被解析成合法场景的模型输出（故意包在 Markdown 代码块里，模拟真实模型行为）。
pub fn mock_scene_reply() -> String {
    format!(
        "好的，这是一个塔：\n```json\n{}\n```\n希望有帮助！",
        r#"{
  "format": "quench3d.scene",
  "version": 2,
  "name": "模型生成的塔",
  "gravity": [0, -9.81, 0],
  "ground": { "size": 20 },
  "materials": { "wood": { "color": [0.8, 0.5, 0.25], "roughness": 0.6 } },
  "objects": [
    { "name": "底", "shape": { "type": "box", "size": [1.2, 0.4, 1.2] }, "position": [0, 0.2, 0], "material": "wood", "body": "dynamic" },
    { "name": "中", "shape": { "type": "box", "size": [1.0, 0.4, 1.0] }, "position": [0, 0.6, 0], "material": "wood", "body": "dynamic" },
    { "name": "顶", "shape": { "type": "box", "size": [0.8, 0.4, 0.8] }, "position": [0, 1.0, 0], "material": "wood", "body": "dynamic" }
  ],
  "camera": { "position": [5, 3, 6], "look_at": [0, 0.8, 0] },
  "simulate": { "steps": 120 },
  "render": { "output": "tower.png", "width": 640, "height": 400 }
}"#
    )
}

/// 一段结构不对的模型输出（字段名写错），用于验证校验诊断能回喂。
pub fn mock_broken_reply() -> String {
    r#"{"objects": [{"name": "a", "shape": {"type": "pyramid"}, "body": "dynamic"}]}"#.to_string()
}
