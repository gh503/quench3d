//! Web 端入口：`tools/build_web.sh web_client` 之后打开 `web/index.html`。
//!
//! 页面与桌面端跑的是**同一套代码**（`quench3d_editor::run`），区别只有：
//! 页面在 `start()` 之前先探好后端（见 `web/index.html` 的 `pickBackend`），
//! 并且用 `on_scene_file` 把浏览器选中的场景文件交回 Rust。
//!
//! 另外还导出几个自检探针给 `tools/check_web.mjs`：它们是「Web 产物真的能用」
//! 的硬证据 —— `cargo check --target wasm32` 只能证明能编译。

/// 启动编辑器（页面在探好后端之后调用）。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn start() -> Result<(), wasm_bindgen::JsValue> {
    quench3d_editor::run(quench3d_editor::EditorConfig::default())
        .map_err(|e| wasm_bindgen::JsValue::from_str(&e.to_string()))
}

/// 页面把用户选中的场景文件交回 Rust（由 `web/index.html` 调用）。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn on_scene_file(name: String, text: String) {
    quench3d_editor::scene_io::on_scene_file(name, text);
}

/// 外部姿态注入入口：页面脚本（例如 MediaPipe Tasks Vision）把 2D 骨架交给编辑器。
///
/// JSON 形如 `{"present":true,"confidence":0.9,"landmarks":[{"x":0.5,"y":0.5},…]}`；
/// 归一化图像坐标，至少 2 个点。内置推理仍不在浏览器里。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn set_camera_pose(json: String) {
    quench3d_editor::camera::set_web_pose(&json);
}

/// 清空外部姿态。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn clear_camera_pose() {
    quench3d_editor::camera::clear_web_pose();
}

/// 时钟探针：读一次引擎时钟（秒）。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn clock_probe() -> f64 {
    quench3d::web_probe::clock_probe()
}

/// 路径探针：Web 端按路径加载模型必须给出能照着做的报错。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn model_path_probe() -> String {
    quench3d::web_probe::model_path_probe()
}

/// 字节探针：Web 端从字节加载模型必须真的能跑通。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn model_bytes_probe() -> String {
    quench3d::web_probe::model_bytes_probe()
}

/// 场景格式探针：wasm 里解析同一个 `quench3d.scene` 并建成物理世界（纯 CPU）。
///
/// 这是「Web 端用同一套场景格式」的硬证据（`cargo check` 只能证明能编译）。
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn scene_roundtrip_probe() -> String {
    quench3d_editor::scene_io::roundtrip_probe()
}

/// wasm 产物是 example，需要一个（Web 端不会调用的）`main`。
#[cfg(target_arch = "wasm32")]
fn main() {}

/// 桌面端也能直接跑这个 example（等价于 `cargo run -p quench3d-editor`）。
#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    quench3d_editor::run(quench3d_editor::EditorConfig::default())
}
