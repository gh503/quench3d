//! 桌面端入口：`cargo run -p quench3d-editor`（release 更流畅）。

#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config = quench3d_editor::EditorConfig::default();
    // 支持 `quench3d-editor [场景.json]`：启动即载入。
    if let Some(path) = std::env::args().nth(1) {
        config.demo_scene = false;
        config.title = format!("Quench3D 编辑器 — {path}");
        config.startup_scene = Some(std::path::PathBuf::from(path));
    }
    quench3d_editor::run(config)
}

#[cfg(target_arch = "wasm32")]
fn main() {}
