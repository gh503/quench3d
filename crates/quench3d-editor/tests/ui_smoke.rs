//! 编辑器 UI 的**正向冒烟**：把整块 `ui::panels` 在无窗口引擎上真的跑一帧。
//!
//! 目的不是替代像素测试，而是钉住"面板代码能同时画出来"：工具栏、场景树、属性、
//! 视口、状态栏、帮助、摄像头窗口只要有一个在真实布局里 panic / 重复 ID / 借用到
//! 坏状态，这一条就会红。它也是 `?ui_debug=1` 的 CDP 探针之外的**纯 Rust** 证据。
//!
//! **显式 opt-in**：要真的创建 wgpu 上下文，所以只有设了 `QUENCH3D_GPU_TESTS=1` 才执行。
#![cfg(not(target_arch = "wasm32"))]

use quench3d::prelude::*;
use quench3d::render::context::with_gpu_tests;
use quench3d_editor::camera::CameraFeed;
use quench3d_editor::doc::Doc;
use quench3d_editor::gizmo::Gizmo;
use quench3d_editor::ui::{self, Viewport};

fn headless_engine() -> Option<Engine> {
    let gpu = with_gpu_tests(
        "编辑器 UI 正向冒烟（真的创建 wgpu 上下文）",
        || {
            pollster::block_on(quench3d::render::GpuContext::headless(
                128,
                96,
                Default::default(),
            ))
        },
    )?;
    let gpu = match gpu {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("跳过：没有可用的 GPU（{e}）");
            return None;
        }
    };
    Some(Engine::new(
        gpu,
        &EngineConfig {
            width: 128,
            height: 96,
            ..Default::default()
        },
    ))
}

#[test]
fn all_editor_panels_draw_in_one_frame() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    doc.selected = Some(1);
    doc.show_help = true;

    let mut view = Viewport::default();
    let mut gizmo = Gizmo::default();
    let mut camera = CameraFeed::default();
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 800.0),
        )),
        ..Default::default()
    };
    let mut requests = None;
    let mut output = ctx.run_ui(input, |ui| {
        requests = Some(ui::panels(
            ui,
            &mut engine,
            &mut doc,
            &mut view,
            &mut gizmo,
            &mut camera,
        ));
    });
    assert!(
        !output.shapes.is_empty(),
        "整块 UI 一帧没产出任何绘制图元：面板没画出来"
    );
    assert!(
        output.textures_delta.set.len() + output.textures_delta.free.len() > 0,
        "字体/图标纹理没有更新，egui 可能根本没跑"
    );
    // `FullOutput` 的纹理增量必须显式消费，否则 epaint 在析构时会 panic；
    // 真实渲染器会把它们上传/释放，测试里只需要认账。
    output.textures_delta.clear();
    let requests = requests.expect("`panels` 必须被调用");
    assert!(!requests.save && !requests.load && !requests.screenshot);
    // 状态栏/属性面板会写 `doc.status`，不该被 UI 清空。
    assert!(!doc.status.is_empty(), "默认场景的状态提示不该为空");
}
