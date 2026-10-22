//! 崩溃恢复的自动保存（桌面文件 / Web localStorage）。
//!
//! 目标很窄：编辑器崩了 / 被强杀之后，下一次启动还能把**最近一个自动保存点**找回来，
//! 而不是只有"上次手动保存"的那个文件。它不替代存盘，也不进撤销历史。
//!
//! 桌面端位置：`$XDG_STATE_HOME/quench3d/autosave.scene.json`，没有
//! `XDG_STATE_HOME` 时退回 `$HOME/.local/state/...`，再不行用系统临时目录。写入是
//! "临时文件 + rename"，中途断电也不会留下半截 JSON。Web 端写到
//! `localStorage["quench3d.autosave.scene.v1"]`，刷新/崩溃后仍能读回。
//!
//! **正常退出会删掉它**（Web 端在手动保存/丢弃时删）：只有崩溃/被停电/被强杀
//! 才会留下恢复文件。这样下次启动的"发现自动保存"提示才不是每次都来烦人。

use std::path::{Path, PathBuf};
use std::time::Duration;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;

/// 默认自动保存间隔。
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(30);

/// 自动保存器。
pub struct AutoSave {
    path: PathBuf,
    interval: Duration,
    /// 桌面：单调时钟。Web 的 `std::time::Instant::now()` 会 panic，所以 Web 用
    /// `performance.now()`（`quench3d::clock::now_seconds`）单独记秒数。
    #[cfg(not(target_arch = "wasm32"))]
    last: Option<Instant>,
    #[cfg(target_arch = "wasm32")]
    last_seconds: Option<f64>,
    saved: Option<String>,
}

impl AutoSave {
    /// 默认恢复文件路径。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            })
            .unwrap_or_else(std::env::temp_dir);
        base.join("quench3d").join("autosave.scene.json")
    }

    /// Web 端没有文件系统：路径只是展示/测试用的占位。
    #[cfg(target_arch = "wasm32")]
    pub fn default_path() -> PathBuf {
        PathBuf::from("localStorage:quench3d.autosave.scene.v1")
    }

    /// 指定路径与间隔。
    pub fn new(path: impl Into<PathBuf>, interval: Duration) -> Self {
        Self {
            path: path.into(),
            interval,
            #[cfg(not(target_arch = "wasm32"))]
            last: None,
            #[cfg(target_arch = "wasm32")]
            last_seconds: None,
            saved: None,
        }
    }

    /// 恢复文件路径（测试/提示用）。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 距离下一次可写是否已经到点。**先判它，再序列化场景**——避免每帧都
    /// 把整个场景转成 JSON。
    pub fn due(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            match self.last {
                Some(last) => last.elapsed() >= self.interval,
                None => true,
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            match self.last_seconds {
                Some(last) => quench3d::clock::now_seconds() - last >= self.interval.as_secs_f64(),
                None => true,
            }
        }
    }

    /// 本次到点但没写成（场景不可序列化/磁盘错）：把计时推后，避免每帧重试。
    pub fn defer(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.last = Some(Instant::now());
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.last_seconds = Some(quench3d::clock::now_seconds());
        }
    }

    /// 到点且内容变了才写；返回是否真的写了。
    pub fn tick(&mut self, json: &str) -> std::io::Result<bool> {
        if !self.due() || self.saved.as_deref() == Some(json) {
            return Ok(false);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.last = Some(Instant::now());
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.last_seconds = Some(quench3d::clock::now_seconds());
        }
        #[cfg(not(target_arch = "wasm32"))]
        write_atomic(&self.path, json)?;
        #[cfg(target_arch = "wasm32")]
        web_storage()?
            .set_item(WEB_KEY, json)
            .map_err(|e| io_other(&format!("localStorage 写入失败：{e:?}")))?;
        self.saved = Some(json.to_string());
        Ok(true)
    }

    /// 删掉恢复文件（正常退出 / 手动存盘 / 用户丢弃时）。
    pub fn clear(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        let _ = std::fs::remove_file(&self.path);
        #[cfg(target_arch = "wasm32")]
        if let Ok(storage) = web_storage() {
            let _ = storage.remove_item(WEB_KEY);
        }
        self.saved = None;
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.last = None;
        }
        #[cfg(target_arch = "wasm32")]
        {
            self.last_seconds = None;
        }
    }

    /// 读一次指定路径的恢复文件；不存在或为空时 `None`。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn read_recovery_at(path: &Path) -> Option<String> {
        let text = std::fs::read_to_string(path).ok()?;
        (!text.trim().is_empty()).then_some(text)
    }

    /// 读一次默认位置的恢复内容；不存在或为空时 `None`。
    pub fn read_recovery() -> Option<String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self::read_recovery_at(&Self::default_path())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let text = web_storage().ok()?.get_item(WEB_KEY).ok()??;
            (!text.trim().is_empty()).then_some(text)
        }
    }
}

/// Web 端 localStorage 的键（改了等于丢掉旧版本恢复点；所以带版本号）。
#[cfg(target_arch = "wasm32")]
pub const WEB_KEY: &str = "quench3d.autosave.scene.v1";

#[cfg(target_arch = "wasm32")]
fn io_other(message: &str) -> std::io::Error {
    std::io::Error::other(message.to_string())
}

#[cfg(target_arch = "wasm32")]
fn web_storage() -> std::io::Result<web_sys::Storage> {
    let window = web_sys::window().ok_or_else(|| io_other("没有 window（不在浏览器里）"))?;
    window
        .local_storage()
        .map_err(|e| io_other(&format!("localStorage 不可用：{e:?}")))?
        .ok_or_else(|| io_other("localStorage 被禁用"))
}

/// 先写同目录临时文件，再 rename 覆盖；失败时清掉临时文件。
#[cfg(not(target_arch = "wasm32"))]
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    // Windows 的 `rename` 不能覆盖已存在的目标（Unix 可以）。先删旧文件再换名；
    // 这一步的窗口很短，最坏情况是这次自动保存没落地，不会留下半截 JSON。
    #[cfg(windows)]
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "quench3d-autosave-{}-{tag}.json",
            std::process::id()
        ))
    }

    #[test]
    fn autosave_writes_only_on_change_and_clears() {
        let path = temp_path("basic");
        let _ = std::fs::remove_file(&path);
        let mut save = AutoSave::new(&path, Duration::ZERO);
        assert!(save.due(), "第一次一定到点");
        assert!(save.tick("{\"a\":1}").expect("写入必须成功"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}");
        assert_eq!(
            AutoSave::read_recovery_at(&path).as_deref(),
            Some("{\"a\":1}"),
            "恢复读到的必须是刚写的同一份字节"
        );
        assert!(!save.tick("{\"a\":1}").expect("同内容不再写"));
        assert!(save.tick("{\"a\":2}").expect("内容变了要写"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":2}");
        save.clear();
        assert!(!path.exists(), "clear 必须删掉恢复文件");
    }

    #[test]
    fn interval_gates_writes() {
        let path = temp_path("interval");
        let _ = std::fs::remove_file(&path);
        let mut save = AutoSave::new(&path, Duration::from_secs(3600));
        assert!(save.tick("{}").expect("首次写入"));
        assert!(!save.due(), "一小时间隔内不该到点");
        assert!(!save.tick("{\"b\":1}").expect("未到点不写"));
        save.clear();
    }
}
