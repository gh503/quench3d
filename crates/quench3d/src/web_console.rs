//! Web 端的诊断通道：把 `log` 接到浏览器控制台，并把致命错误显示到页面上。
//!
//! 之前 Web 端**没有任何可观测性**：`run_web` 只装了 panic hook，没装 logger，
//! 所以 `log::info!("Quench3D 启动：{adapter}")`、wgpu 的校验错误、
//! 以及「GPU 初始化失败」这类 `log::error!` **全部被丢掉**；
//! 更糟的是致命错误走的是 `event_loop.exit()`，页面只会看到一个空画布，
//! 看起来和「渲染了但场景是空的」一模一样，根本没法查。
//!
//! 现在：
//! * [`install_logger`] 把 `log` 记录转发到 `console.log/warn/error`（带级别前缀）；
//! * [`report_fatal`] 除了打印，还把错误写进页面上的 `#status` 并露出 `#overlay`；
//! * [`mark_ready`] 在**第一帧真的画出来之后**才隐藏遮罩——页面从此不会
//!   「假装启动成功」然后给你一块黑屏。
//!
//! 调试级别默认 `Info`，地址栏带 `?debug=1` 时升到 `Debug`。

#[cfg(target_arch = "wasm32")]
mod imp {
    use std::sync::atomic::{AtomicU8, Ordering};

    use log::{Level, LevelFilter, Log, Metadata, Record};

    /// 0=off 1=error 2=warn 3=info 4=debug 5=trace（与 `log::Level` 的判别值一致）。
    static MAX_LEVEL: AtomicU8 = AtomicU8::new(3);

    struct ConsoleLogger;

    fn out(level: Level, message: &str) {
        let text = format!("[{level}] {message}");
        let value = wasm_bindgen::JsValue::from_str(&text);
        match level {
            Level::Error => web_sys::console::error_1(&value),
            Level::Warn => web_sys::console::warn_1(&value),
            Level::Info => web_sys::console::log_1(&value),
            Level::Debug | Level::Trace => web_sys::console::debug_1(&value),
        }
    }

    impl Log for ConsoleLogger {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            metadata.level() as u8 <= MAX_LEVEL.load(Ordering::Relaxed)
        }

        fn log(&self, record: &Record<'_>) {
            if self.enabled(record.metadata()) {
                out(record.level(), &record.args().to_string());
            }
        }

        fn flush(&self) {}
    }

    static LOGGER: ConsoleLogger = ConsoleLogger;

    /// 是否处于 `?debug=1`。
    fn debug_requested() -> bool {
        web_sys::window()
            .and_then(|w| w.location().search().ok())
            .map(|s| s.contains("debug=1"))
            .unwrap_or(false)
    }

    /// 装上控制台 logger（重复调用无副作用）。返回是否装上了。
    pub fn install_logger() -> bool {
        if debug_requested() {
            MAX_LEVEL.store(4, Ordering::Relaxed);
        }
        log::set_max_level(if debug_requested() {
            LevelFilter::Debug
        } else {
            LevelFilter::Info
        });
        log::set_logger(&LOGGER).is_ok()
    }

    fn element(id: &str) -> Option<web_sys::Element> {
        web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id(id))
    }

    /// 把一条消息写进页面的 `#status`，并露出 `#overlay`。
    fn set_status(text: &str, is_error: bool) {
        if let Some(status) = element("status") {
            status.set_text_content(Some(text));
            if is_error {
                let _ = status.class_list().add_1("error");
            } else {
                let _ = status.class_list().remove_1("error");
            }
        }
        if let Some(overlay) = element("overlay") {
            let _ = overlay.class_list().remove_1("hidden");
        }
    }

    /// 致命错误：控制台 + 页面都写。
    pub fn report_fatal(message: &str) {
        log::error!("{message}");
        set_status(message, true);
    }

    /// 第一帧画完了：隐藏遮罩。
    pub fn mark_ready() {
        if let Some(overlay) = element("overlay") {
            let _ = overlay.class_list().add_1("hidden");
        }
    }

    /// 初始化中的提示（不标记为错误）。
    pub fn set_progress(message: &str) {
        set_status(message, false);
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    /// 桌面端由 `env_logger` 负责，这里全是空操作。
    pub fn install_logger() -> bool {
        false
    }
    /// 空操作（桌面端没有遮罩）。
    pub fn report_fatal(message: &str) {
        log::error!("{message}");
    }
    /// 空操作。
    pub fn mark_ready() {}
    /// 空操作。
    pub fn set_progress(_message: &str) {}
}

pub use imp::{install_logger, mark_ready, report_fatal, set_progress};
