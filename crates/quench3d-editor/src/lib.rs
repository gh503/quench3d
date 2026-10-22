//! # Quench3D 编辑器
//!
//! 桌面端与 Web 端**共用同一套界面代码**（egui），所以这里没有「桌面版」和
//! 「Web 版」两套实现，只有入口不同：
//!
//! * 桌面：`src/bin/quench3d-editor.rs` → [`run`]；
//! * Web：`examples/web_client.rs` 导出 `start()`，页面 `web/index.html` 调用它。
//!
//! 分工：
//!
//! | 模块 | 管什么 |
//! |---|---|
//! | [`doc`] | 「用户做了什么」→ 引擎/物理世界的改动（选中、增删、改参数、重置、拾取） |
//! | [`ui`] | 面板长什么样（工具栏 / 场景树 / 属性 / 状态栏 / 视口交互） |
//! | [`scene_io`] | 与 CLI 共用的 `quench3d.scene` 读写 |
//! | [`app`] | 窗口、输入、GPU、egui 与引擎的接线（一帧怎么拼出来） |
//!
//! 界面里的一切都走引擎自己的 API：`Engine::tick` 推进物理与提交绘制，
//! `Engine::render_to` 把 3D 画进交换链的纹理，然后 egui 在同一张纹理上叠一层 UI。
//! 这也是为什么这个 crate 没有重写一遍渲染循环 —— 只接管「呈现」那一步。

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod app;
pub mod autosave;
pub mod camera;
pub mod doc;
pub mod gizmo;
pub mod history;
pub mod scene_io;
pub mod ui;

pub use app::{run, EditorApp, EditorConfig};
pub use camera::CameraFeed;

/// Web 端：页面探好的后端结论（见 `web/index.html` 的 `pickBackend`）。
///
/// 浏览器里 `navigator.gpu` 存在 ≠ 有可用适配器；而 wgpu 只要看到
/// `BROWSER_WEBGPU` 且 `navigator.gpu` 存在，就只走 WebGPU 那条路，
/// 连 WebGL2 实例都不建。更狠的是**同一张画布请求过 `webgpu` 上下文之后，
/// `getContext("webgl2")` 永远返回 null**。所以顺序必须由页面先探好再告诉我们。
#[cfg(target_arch = "wasm32")]
pub(crate) fn web_backend_hint() -> Option<String> {
    let window = web_sys::window()?;
    let key = wasm_bindgen::JsValue::from_str("__quench3d_backend");
    let value = js_sys::Reflect::get(&window, &key).ok()?;
    value.as_string()
}

/// Web 端异步初始化的回传槽。
#[cfg(target_arch = "wasm32")]
pub(crate) type PendingSlot = std::rc::Rc<
    std::cell::RefCell<
        Option<
            Result<
                (
                    quench3d::render::GpuContext,
                    std::sync::Arc<quench3d::winit::window::Window>,
                ),
                quench3d::render::GpuInitError,
            >,
        >,
    >,
>;

/// 新建一个空的回传槽。
#[cfg(target_arch = "wasm32")]
pub(crate) fn new_pending_slot() -> PendingSlot {
    std::rc::Rc::new(std::cell::RefCell::new(None))
}
