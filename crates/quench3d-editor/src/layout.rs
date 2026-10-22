//! 编辑器面板布局的持久化（桌面 JSON 文件 / Web localStorage）。
//!
//! 只存两个宽度，不存场景内容。写失败/文件损坏/旧版本都静默回默认值——布局是
//! 使用偏好，不应该阻止编辑器启动。桌面沿用自动保存的目录：
//! `$XDG_STATE_HOME/quench3d/layout.json`（没有则 `$HOME/.local/state/...`）。
//!
//! 宽度由 `ui::panels` 通过 `min_size/max_size` 强制到面板上；不依赖 egui 的拖拽
//! 分隔条（Canvas 探针里那条命中区曾经不稳定），所以「布局」菜单里的滑杆是唯一入口，
//! 也是可测试的入口。

use serde::{Deserialize, Serialize};

/// 左栏允许宽度范围（逻辑像素）。
/// 左栏最小宽度。
pub const OUTLINER_MIN: f32 = 140.0;
/// 左栏最大宽度。
pub const OUTLINER_MAX: f32 = 340.0;
/// 右栏允许宽度范围（逻辑像素）。
/// 右栏最小宽度。
pub const INSPECTOR_MIN: f32 = 180.0;
/// 右栏最大宽度。
pub const INSPECTOR_MAX: f32 = 420.0;

/// 两个面板的宽度。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    /// 左栏（场景树）宽度。
    #[serde(default = "default_outliner")]
    pub outliner_width: f32,
    /// 右栏（属性）宽度。
    #[serde(default = "default_inspector")]
    pub inspector_width: f32,
}

fn default_outliner() -> f32 {
    190.0
}
fn default_inspector() -> f32 {
    260.0
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            outliner_width: default_outliner(),
            inspector_width: default_inspector(),
        }
    }
}

impl Layout {
    /// 把非法（NaN/Inf/过宽）的值夹回可用范围；所有入口（读文件、滑杆、面板）都走它。
    pub fn clamp(&mut self) {
        if !self.outliner_width.is_finite() {
            self.outliner_width = default_outliner();
        }
        if !self.inspector_width.is_finite() {
            self.inspector_width = default_inspector();
        }
        self.outliner_width = self.outliner_width.clamp(OUTLINER_MIN, OUTLINER_MAX);
        self.inspector_width = self.inspector_width.clamp(INSPECTOR_MIN, INSPECTOR_MAX);
    }

    /// JSON 序列化（`load_from` 的逆操作；Web 也用它写 localStorage）。
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// 从 JSON 读；缺字段/坏值都回默认，不返回错误。
    pub fn from_json(text: &str) -> Self {
        let mut layout: Self = serde_json::from_str(text).unwrap_or_default();
        layout.clamp();
        layout
    }

    /// 桌面：布局文件路径。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn default_path() -> std::path::PathBuf {
        crate::autosave::AutoSave::default_path().with_file_name("layout.json")
    }

    /// 桌面：从任意路径读；坏文件回默认。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn load_from(path: &std::path::Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .map(|s| Self::from_json(&s))
            .unwrap_or_default()
    }

    /// 桌面：原子写到任意路径（临时文件 + rename）。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        let mut layout = self.clone();
        layout.clamp();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("创建布局目录 {} 失败：{e}", parent.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, layout.to_json().as_bytes())
            .map_err(|e| format!("写布局临时文件 {} 失败：{e}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| format!("替换布局文件 {} 失败：{e}", path.display()))
    }

    /// 读当前平台的布局；失败回默认。
    pub fn load() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        {
            Self::load_from(&Self::default_path())
        }
        #[cfg(target_arch = "wasm32")]
        {
            web_sys::window()
                .and_then(|w| w.local_storage().ok().flatten())
                .and_then(|storage| storage.get_item(WEB_KEY).ok().flatten())
                .map(|s| Self::from_json(&s))
                .unwrap_or_default()
        }
    }

    /// 写当前平台的布局；失败只返回错误，不 panic。
    pub fn save(&self) -> Result<(), String> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.save_to(&Self::default_path())
        }
        #[cfg(target_arch = "wasm32")]
        {
            let storage = web_sys::window()
                .and_then(|w| w.local_storage().ok().flatten())
                .ok_or("没有 localStorage")?;
            storage
                .set_item(WEB_KEY, &self.to_json())
                .map_err(|e| format!("{e:?}"))
        }
    }
}

#[cfg(target_arch = "wasm32")]
const WEB_KEY: &str = "quench3d.editor.layout.v1";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip_and_clamp() {
        let mut layout = Layout {
            outliner_width: 300.0,
            inspector_width: 390.0,
        };
        layout.clamp();
        let back = Layout::from_json(&layout.to_json());
        assert_eq!(back, layout);
        let broken = Layout::from_json(r#"{"outliner_width": 0, "inspector_width": 9999}"#);
        assert_eq!(broken.outliner_width, OUTLINER_MIN);
        assert_eq!(broken.inspector_width, INSPECTOR_MAX);
        let mut nan = Layout {
            outliner_width: f32::NAN,
            inspector_width: -2.0,
        };
        nan.clamp();
        assert_eq!(nan.outliner_width, default_outliner());
        assert_eq!(nan.inspector_width, INSPECTOR_MIN);
        assert_eq!(Layout::from_json("not json"), Layout::default());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn file_roundtrip_is_atomic_and_recovers_bad_files() {
        let dir = std::env::temp_dir().join(format!("quench3d-layout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("layout.json");
        let layout = Layout {
            outliner_width: 240.0,
            inspector_width: 320.0,
        };
        layout.save_to(&path).unwrap();
        assert_eq!(Layout::load_from(&path), layout);
        std::fs::write(&path, "{ broken").unwrap();
        assert_eq!(Layout::load_from(&path), Layout::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
