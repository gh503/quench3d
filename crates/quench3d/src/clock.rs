//! 跨平台单调时钟。
//!
//! 引擎里所有「取一次时间」的地方都走这里，原因是
//! `std::time::Instant::now()` 在 `wasm32-unknown-unknown` 上**直接 panic**：
//!
//! ```text
//! panicked at library/std/src/sys/pal/wasm/../unsupported/time.rs:13:9:
//! time not implemented on this platform
//! ```
//!
//! 这个错误 `cargo check --target wasm32-unknown-unknown` **发现不了**（编译完全通过），
//! 只会在浏览器里第一次跑到就白屏。所以：
//!
//! * 原生：用 `Instant`，基准是进程内第一次调用，单调；
//! * wasm：用 `performance.now()`（浏览器与 Worker 都有，单调），
//!   拿不到时退到 `Date.now()`（墙上时钟、不单调，但总比 panic 好）。
//!
//! 配套的防线在 `tools/check_web.mjs`：它会扫 wasm 产物里有没有
//! `time not implemented on this platform` 这个 panic 字符串，并调用导出里的
//! `clock_probe()` 真的读一次时钟。

/// 读一次单调时钟，返回**秒**。
///
/// 只保证「单调不减」，基准（原点）是任意的：它只适合算两个时刻之间的**间隔**，
/// 不要当成墙上时间用。
#[cfg(not(target_arch = "wasm32"))]
pub fn now_seconds() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static BASE: OnceLock<Instant> = OnceLock::new();
    BASE.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// 读一次单调时钟，返回**秒**（wasm 版：`performance.now()`）。
///
/// 优先 `performance.now()`（单调，精度到微秒级）；在没有 `window` 的环境
/// （例如 Worker 或 Node）退到 `Date.now()`，保证**永远不会 panic**。
#[cfg(target_arch = "wasm32")]
pub fn now_seconds() -> f64 {
    if let Some(window) = web_sys::window() {
        if let Some(perf) = window.performance() {
            return perf.now() / 1000.0;
        }
    }
    js_sys::Date::now() / 1000.0
}
