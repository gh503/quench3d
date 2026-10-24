//! 编辑器 UI 的**正向冒烟**：把整块 `ui::panels` 在无窗口引擎上真的跑一帧。
//!
//! 目的不是替代像素测试，而是钉住"面板代码能同时画出来"：工具栏、场景树、属性、
//! 视口、状态栏、帮助、摄像头窗口只要有一个在真实布局里 panic / 重复 ID / 借用到
//! 坏状态，这一条就会红。它也是 `?ui_debug=1` 的 CDP 探针之外的**纯 Rust** 证据。
//!
//! **显式 opt-in**：要真的创建 wgpu 上下文，所以只有设了 `QUENCH3D_GPU_TESTS=1` 才执行。
#![cfg(not(target_arch = "wasm32"))]

use quench3d::assets::{
    LoadedAnimation, LoadedMesh, LoadedModel, LoadedMorphTarget, LoadedNode, ModelAnimationPlayer,
};
use quench3d::prelude::*;
use quench3d::render::context::with_gpu_tests;
use quench3d::scene::{ModelAnimationInstance, ModelAnimatorRuntime, ModelAnimatorState};
use quench3d_editor::camera::CameraFeed;
use quench3d_editor::doc::Doc;
use quench3d_editor::gizmo::Gizmo;
use quench3d_editor::scene_io;
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

/// 一个不依赖磁盘/GPU 的播放实例：只给动画面板读播放器与状态机字段。
/// 片段列表为空，所以 `refresh_animations` 不会碰任何物体。
fn fake_animation_instance() -> ModelAnimationInstance {
    let model = std::sync::Arc::new(LoadedModel {
        name: "ui-smoke".into(),
        meshes: Vec::new(),
        materials: Vec::new(),
        images: Vec::new(),
        nodes: Vec::new(),
        animations: vec![
            LoadedAnimation {
                name: "idle".into(),
                duration: 1.0,
                ..Default::default()
            },
            LoadedAnimation {
                name: "walk".into(),
                duration: 1.0,
                ..Default::default()
            },
        ],
        skins: Vec::new(),
    });
    ModelAnimationInstance {
        model,
        objects: Vec::new(),
        base: Vec::new(),
        player: ModelAnimationPlayer {
            animation: "idle".into(),
            duration: 1.0,
            time: 0.25,
            speed: 1.0,
            looping: true,
            finished: false,
        },
        secondary: Some(ModelAnimationPlayer {
            animation: "walk".into(),
            duration: 1.0,
            time: 0.5,
            speed: 1.0,
            looping: true,
            finished: false,
        }),
        blend_weight: 0.5,
        blend_target: 0.5,
        blend_fade: 0.0,
        blend_elapsed: 0.0,
        morph_weights: None,
        ik: vec![quench3d::scene::IkChain {
            root: "大腿".into(),
            mid: "小腿".into(),
            tip: "脚".into(),
            target: Vec3::new(0.0, 0.2, 0.1),
            pole: Some(Vec3::Z),
            weight: 1.0,
        }],
        events: vec![quench3d::scene::SceneAnimationEvent {
            name: "footstep".into(),
            time: 0.5,
            clip: None,
            payload: Some("step=1".into()),
        }],
        animator: Some(ModelAnimatorRuntime {
            states: vec![
                ModelAnimatorState {
                    name: "待机".into(),
                    animation: "idle".into(),
                    speed: 1.0,
                    looping: true,
                    start: 0.0,
                },
                ModelAnimatorState {
                    name: "走".into(),
                    animation: "idle".into(),
                    speed: 1.0,
                    looping: true,
                    start: 0.0,
                },
            ],
            transitions: vec![quench3d::scene::ModelAnimatorTransition {
                from: "待机".into(),
                to: "走".into(),
                after: 0.5,
                on_finished: false,
                fade: 0.0,
                conditions: vec![quench3d::scene::AnimatorCondition {
                    parameter: "速度".into(),
                    op: quench3d::scene::AnimatorOp::Greater,
                    value: 0.1,
                }],
                on_event: Some("跳跃".into()),
            }],
            current: 0,
            elapsed: 0.25,
            pending: None,
            parameters: vec![quench3d::scene::ModelAnimatorParameter {
                name: "速度".into(),
                value: 0.5,
                default: 0.0,
            }],
            event_buffer: std::collections::VecDeque::new(),
            event_ages: std::collections::VecDeque::new(),
            event_window: 0.0,
        }),
        root_motion: None,
    }
}

/// 普通（非状态机）播放实例：资产里真的有一个 `walk` clip，用来显示片段选择器。
fn fake_plain_animation_instance() -> ModelAnimationInstance {
    let mut model = LoadedModel {
        name: "ui-smoke-plain".into(),
        meshes: vec![LoadedMesh {
            morph_targets: vec![LoadedMorphTarget::default()],
            ..Default::default()
        }],
        materials: Vec::new(),
        images: Vec::new(),
        nodes: vec![LoadedNode {
            name: "root".into(),
            ..Default::default()
        }],
        animations: Vec::new(),
        skins: Vec::new(),
    };
    model.animations.push(LoadedAnimation {
        name: "walk".into(),
        duration: 1.0,
        ..Default::default()
    });
    ModelAnimationInstance {
        model: std::sync::Arc::new(model),
        objects: Vec::new(),
        base: Vec::new(),
        player: ModelAnimationPlayer {
            animation: "walk".into(),
            duration: 1.0,
            time: 0.25,
            speed: 1.0,
            looping: true,
            finished: false,
        },
        secondary: None,
        blend_weight: 0.0,
        blend_target: 0.0,
        blend_fade: 0.0,
        blend_elapsed: 0.0,
        morph_weights: None,
        ik: Vec::new(),
        events: Vec::new(),
        animator: None,
        root_motion: Some(quench3d::scene::ModelRootMotion {
            node: 0,
            origin: Vec3::ZERO,
            origin_rotation: Quat::IDENTITY,
        }),
    }
}

/// 在 egui 的绘制图元里递归找一段文字，证明面板的**内容**真的被排版了，
/// 而不只是"没 panic"。
fn shapes_contain_text(shapes: &[egui::epaint::ClippedShape], needle: &str) -> bool {
    fn walk(shape: &egui::Shape, needle: &str) -> bool {
        match shape {
            egui::Shape::Text(text) => text.galley.text().contains(needle),
            egui::Shape::Vec(shapes) => shapes.iter().any(|s| walk(s, needle)),
            _ => false,
        }
    }
    shapes.iter().any(|clipped| walk(&clipped.shape, needle))
}

#[test]
fn outliner_drop_request_reparents_a_static_object() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    let material = engine.assets.solid_material(
        &mut engine.renderer,
        &engine.gpu,
        Vec4::new(0.8, 0.8, 0.2, 1.0),
        0.0,
        0.5,
    );
    let parent = engine.scene.add_named(
        engine.assets.unit_cube,
        material,
        Transform::new(Vec3::new(-3.0, 0.0, 0.0), Quat::IDENTITY, Vec3::ONE),
        "静态父",
    );
    let child = engine.scene.add_named(
        engine.assets.unit_cube,
        material,
        Transform::new(Vec3::new(3.0, 0.0, 0.0), Quat::IDENTITY, Vec3::ONE),
        "静态子",
    );

    doc.reparent_request = Some((child, Some(parent)));
    let mut view = Viewport::default();
    let mut gizmo = Gizmo::default();
    let mut camera = CameraFeed::default();
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 900.0),
        )),
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        let _ = ui::panels(
            ui,
            &mut engine,
            &mut doc,
            &mut view,
            &mut gizmo,
            &mut camera,
        );
    });
    assert_eq!(
        engine.scene.objects[child].parent,
        Some(parent),
        "场景树拖放的 reparent_request 没有被执行"
    );
    assert!(
        engine.scene.objects[child].transform.position.x > 0.0,
        "改父后应保持世界位姿，不能把子物体吸到父原点"
    );
    output.textures_delta.clear();
}

#[test]
fn all_editor_panels_draw_in_one_frame() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.layout.outliner_width = 300.0;
    doc.layout.inspector_width = 340.0;
    doc.build_default_scene(&mut engine);
    engine.scene.animations.push(fake_animation_instance());
    engine
        .scene
        .animations
        .push(fake_plain_animation_instance());
    doc.selected = Some(1);
    doc.show_help = true;
    doc.recovery = Some("{\"format\":\"quench3d.scene\",\"version\":2}".to_string());

    let mut view = Viewport::default();
    let mut gizmo = Gizmo::default();
    let mut camera = CameraFeed::default();
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 1100.0),
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
        shapes_contain_text(&output.shapes, "导出 glTF"),
        "工具栏没有画出「导出 glTF」按钮"
    );
    assert!(
        shapes_contain_text(&output.shapes, "历史"),
        "工具栏没有画出「历史」菜单"
    );
    assert!(
        shapes_contain_text(&output.shapes, "选中中心"),
        "工具栏没有画出轴心选择"
    );
    assert!(
        shapes_contain_text(&output.shapes, "关节"),
        "工具栏没有画出关节调试开关"
    );
    assert!(
        shapes_contain_text(&output.shapes, "导出 GLB"),
        "工具栏没有画出「导出 GLB」按钮"
    );
    assert!(
        shapes_contain_text(&output.shapes, "布局"),
        "工具栏没有画出「布局」菜单"
    );
    assert!(
        shapes_contain_text(&output.shapes, "时间轴"),
        "动画面板没有画出时间轴"
    );
    assert!(
        shapes_contain_text(&output.shapes, "0.250s / 1.000s"),
        "时间轴的读数没有跟着 player.time 显示（预期 0.250s / 1.000s）"
    );
    assert!(
        shapes_contain_text(&output.shapes, "事件 1 条"),
        "动画面板没有显示事件标记数"
    );
    assert!(
        shapes_contain_text(&output.shapes, "待处理事件 0"),
        "状态机没有显示输入缓冲深度"
    );
    assert!(
        shapes_contain_text(&output.shapes, "权重"),
        "IK 链没有显示 IK/FK 权重控件"
    );
    assert!(
        shapes_contain_text(&output.shapes, "事件标记"),
        "动画面板没有显示事件标记编辑器"
    );
    assert!(
        shapes_contain_text(&output.shapes, "时域抗锯齿（TAA）"),
        "渲染面板没有显示 TAA 开关"
    );
    assert!(
        shapes_contain_text(&output.shapes, "负载"),
        "事件行没有显示负载输入框"
    );
    assert!(
        shapes_contain_text(&output.shapes, "SSAO 去噪"),
        "渲染面板没有显示 SSAO 去噪开关"
    );
    assert!(
        shapes_contain_text(&output.shapes, "事件窗口"),
        "状态机面板没有显示输入事件窗口"
    );
    assert!(
        shapes_contain_text(&output.shapes, "根运动 · root"),
        "动画面板没有显示根运动节点"
    );
    let first_requests = requests.expect("panels 必须返回本帧请求");
    assert!(
        (first_requests.outliner_width - 300.0).abs() < 1.0,
        "左栏宽度没有按布局值生效：{}",
        first_requests.outliner_width
    );
    assert!(
        (first_requests.inspector_width - 340.0).abs() < 1.0,
        "右栏宽度没有按布局值生效：{}",
        first_requests.inspector_width
    );
    assert!(
        shapes_contain_text(&output.shapes, "动画（2 个实例）"),
        "动画面板标题没有出现在绘制图元里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "idle"),
        "动画 clip 名没有出现在面板内容里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "状态机：待机"),
        "状态机当前状态没有出现在面板内容里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "片段"),
        "动画片段选择器没有出现在面板内容里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "倍速"),
        "动画倍速控件没有出现在面板内容里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "循环"),
        "动画循环控件没有出现在面板内容里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "加状态"),
        "加状态按钮没有出现在面板内容里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "加过渡"),
        "加过渡按钮没有出现在面板内容里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "#0 待机"),
        "状态机状态行没有出现在面板内容里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "景深（DoF）"),
        "景深开关没有出现在面板内容里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "正交"),
        "相机投影开关没有出现在面板内容里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "发现自动保存"),
        "崩溃恢复提示没有出现在工具栏里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "丢弃"),
        "崩溃恢复的丢弃按钮没有绘制"
    );
    assert!(
        shapes_contain_text(&output.shapes, "初始（立即切换）"),
        "初始状态控件没有出现在面板内容里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "起始"),
        "状态起始时间控件没有出现在面板内容里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "手动 morph 覆盖"),
        "morph 覆盖开关没有出现在面板内容里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "目标 #0"),
        "morph 目标滑杆没有出现在面板内容里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "参数（输入 → 过渡条件）"),
        "状态机参数区没有出现在面板内容里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "重置参数"),
        "参数重置按钮没有绘制"
    );
    assert!(
        shapes_contain_text(&output.shapes, "发送 跳跃"),
        "事件发送按钮没有绘制"
    );
    assert!(
        shapes_contain_text(&output.shapes, "待机 → 走"),
        "状态机过渡行没有出现在面板内容里"
    );
    assert!(
        shapes_contain_text(&output.shapes, "播完触发"),
        "状态机过渡的结束触发控件没有出现在面板内容里"
    );
    // `FullOutput` 的纹理增量必须显式消费，否则 epaint 在析构时会 panic；
    // 真实渲染器会把它们上传/释放，测试里只需要认账。字体纹理是否在本帧新建
    // 受测试执行顺序影响（egui 的字体纹理缓存是进程级的），所以不拿它当断言。
    output.textures_delta.clear();

    // 正向验证多选的 UI 反馈：第二帧把两个物体标成选中，场景树标题必须出现计数。
    doc.select_only(Some(1));
    doc.toggle_selected(2);
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 1100.0),
        )),
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        let _ = ui::panels(
            ui,
            &mut engine,
            &mut doc,
            &mut view,
            &mut gizmo,
            &mut camera,
        );
    });
    assert!(
        shapes_contain_text(&output.shapes, "已选 2 个"),
        "多选时场景树没有显示选中数量"
    );
    output.textures_delta.clear();
    let requests = requests.expect("`panels` 必须被调用");
    assert!(!requests.save && !requests.load && !requests.screenshot);
    // 状态栏/属性面板会写 `doc.status`，不该被 UI 清空。
    assert!(!doc.status.is_empty(), "默认场景的状态提示不该为空");
}

#[test]
fn inspector_lists_particle_emitters_with_live_parameters() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    let material = engine.assets.solid_material(
        &mut engine.renderer,
        &engine.gpu,
        Vec4::new(0.9, 0.4, 0.1, 1.0),
        0.0,
        0.5,
    );
    engine.scene.add_particle_system(
        engine.assets.unit_cube,
        material,
        quench3d::scene::ParticleDesc {
            count: 7,
            lifetime: 3.5,
            shape: quench3d::scene::ParticleShape::Box,
            box_size: Vec3::new(2.0, 1.0, 4.0),
            drag: 0.5,
            rotation: 0.0,
            rotation_end: std::f32::consts::PI,
            ..Default::default()
        },
    );

    let mut view = Viewport::default();
    let mut gizmo = Gizmo::default();
    let mut camera = CameraFeed::default();
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 900.0),
        )),
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        let _ = ui::panels(
            ui,
            &mut engine,
            &mut doc,
            &mut view,
            &mut gizmo,
            &mut camera,
        );
    });
    assert!(
        shapes_contain_text(&output.shapes, "粒子发射器（1 组）"),
        "粒子面板没有显示发射器组数"
    );
    assert!(
        shapes_contain_text(&output.shapes, "#0 · 7 颗"),
        "粒子面板没有显示编号和数量"
    );
    assert!(
        shapes_contain_text(&output.shapes, "寿命"),
        "粒子面板没有寿命编辑控件"
    );
    assert!(
        shapes_contain_text(&output.shapes, "盒 2.0×1.0×4.0"),
        "粒子面板没有显示盒尺寸"
    );
    assert!(
        shapes_contain_text(&output.shapes, "阻尼 0.50/s"),
        "粒子面板没有显示阻尼"
    );
    assert!(
        shapes_contain_text(&output.shapes, "自转 0°→180°"),
        "粒子面板没有显示自转"
    );
    assert!(
        shapes_contain_text(&output.shapes, "启用"),
        "粒子面板没有启用开关"
    );
    assert!(
        shapes_contain_text(&output.shapes, "发射器贴图"),
        "粒子面板没有贴图控件"
    );
    assert!(
        shapes_contain_text(&output.shapes, "清除粒子贴图"),
        "粒子面板没有清除贴图按钮"
    );
    assert!(
        shapes_contain_text(&output.shapes, "收束"),
        "粒子面板没有 size_end 编辑控件"
    );
    assert!(
        shapes_contain_text(&output.shapes, "半径"),
        "粒子面板没有半径编辑控件"
    );
    assert!(
        shapes_contain_text(&output.shapes, "数量"),
        "粒子面板没有数量编辑控件"
    );
    assert!(
        shapes_contain_text(&output.shapes, "形状"),
        "粒子面板没有形状下拉框"
    );
    assert!(
        shapes_contain_text(&output.shapes, "朝向"),
        "粒子面板没有朝向下拉框"
    );
    assert!(
        shapes_contain_text(&output.shapes, "颜色"),
        "粒子面板没有颜色控件"
    );
    assert!(
        shapes_contain_text(&output.shapes, "终点色"),
        "粒子面板没有终点色控件"
    );
    output.textures_delta.clear();
}
#[test]
fn named_camera_selector_applies_scene_camera() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    let text = r#"{
        "cameras": {
            "侧视": { "position": [10, 5, 0], "look_at": [0, 1, 0], "fov_deg": 45 }
        },
        "objects": [
            { "name": "箱", "shape": { "type": "box", "size": [1, 1, 1] }, "body": "static" }
        ]
    }"#;
    scene_io::load_text_into_doc(&mut engine, &mut doc, text).expect("场景应能装载");
    assert!(
        doc.select_named_camera(&mut engine, "侧视"),
        "命名相机应能被选中"
    );
    assert_eq!(
        engine.orbit.focus,
        Vec3::new(0.0, 1.0, 0.0),
        "look_at 应写进轨道焦点"
    );
    assert!(
        (engine.orbit.distance - (100.0f32 + 16.0).sqrt()).abs() < 1e-4,
        "距离应由 position-look_at 反解"
    );
    assert!((engine.orbit.fov_y.to_degrees() - 45.0).abs() < 1e-4);
    assert_eq!(doc.selected_camera.as_deref(), Some("侧视"));

    let mut view = Viewport::default();
    let mut gizmo = Gizmo::default();
    let mut camera = CameraFeed::default();
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 900.0),
        )),
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        let _ = ui::panels(
            ui,
            &mut engine,
            &mut doc,
            &mut view,
            &mut gizmo,
            &mut camera,
        );
    });
    assert!(
        shapes_contain_text(&output.shapes, "侧视"),
        "相机下拉框没有显示当前命名相机"
    );
    assert!(
        shapes_contain_text(&output.shapes, "画中画"),
        "相机面板没有画中画入口"
    );
    output.textures_delta.clear();
}
#[test]
fn named_camera_cycle_wraps_in_name_order() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    let text = r#"{
        "cameras": {
            "A": { "position": [5, 5, 0], "look_at": [0, 1, 0] },
            "B": { "position": [10, 5, 0], "look_at": [0, 1, 0] },
            "C": { "position": [15, 5, 0], "look_at": [0, 1, 0] }
        },
        "objects": [
            { "name": "箱", "shape": { "type": "box", "size": [1, 1, 1] }, "body": "static" }
        ]
    }"#;
    scene_io::load_text_into_doc(&mut engine, &mut doc, text).expect("场景应能装载");
    assert_eq!(doc.cycle_named_camera(&mut engine, 1).as_deref(), Some("A"));
    assert_eq!(doc.cycle_named_camera(&mut engine, 1).as_deref(), Some("B"));
    assert_eq!(doc.cycle_named_camera(&mut engine, 1).as_deref(), Some("C"));
    assert_eq!(doc.cycle_named_camera(&mut engine, 1).as_deref(), Some("A"));
    assert_eq!(
        doc.cycle_named_camera(&mut engine, -1).as_deref(),
        Some("C")
    );
    assert_eq!(doc.selected_camera.as_deref(), Some("C"));
}

#[test]
fn inspector_lists_decals_from_base_doc() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    let text = r#"{
        "decals": [
            { "position": [0, 0.01, 0], "size": [2, 1], "rotation": 45 }
        ]
    }"#;
    scene_io::load_text_into_doc(&mut engine, &mut doc, text).expect("贴花场景应能装载");
    let mut view = Viewport::default();
    let mut gizmo = Gizmo::default();
    let mut camera = CameraFeed::default();
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 900.0),
        )),
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        let _ = ui::panels(
            ui,
            &mut engine,
            &mut doc,
            &mut view,
            &mut gizmo,
            &mut camera,
        );
    });
    assert!(
        shapes_contain_text(&output.shapes, "贴花（1 个）"),
        "属性面板没有贴花组"
    );
    assert!(
        shapes_contain_text(&output.shapes, "位置"),
        "属性面板没有贴花位置控件"
    );
    assert!(
        shapes_contain_text(&output.shapes, "法线"),
        "属性面板没有贴花法线控件"
    );
    assert!(
        shapes_contain_text(&output.shapes, "尺寸"),
        "属性面板没有贴花尺寸控件"
    );
    assert!(
        shapes_contain_text(&output.shapes, "贴图"),
        "属性面板没有贴花贴图控件"
    );
    assert!(
        shapes_contain_text(&output.shapes, "清除贴图"),
        "属性面板没有清除贴花贴图按钮"
    );
    assert!(
        shapes_contain_text(&output.shapes, "旋转"),
        "属性面板没有贴花旋转控件"
    );
    assert!(
        shapes_contain_text(&output.shapes, "纯色"),
        "属性面板没有贴花贴图占位"
    );
    output.textures_delta.clear();
}

#[test]
fn viewport_draws_pip_texture_when_selected() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    let text = r#"{
        "cameras": {
            "侧视": { "position": [10, 5, 0], "look_at": [0, 1, 0] }
        }
    }"#;
    scene_io::load_text_into_doc(&mut engine, &mut doc, text).expect("场景应能装载");
    let ctx = egui::Context::default();
    let pixels = vec![
        255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
    ];
    let image = egui::ColorImage::from_rgba_unmultiplied([2, 2], &pixels);
    let texture = ctx.load_texture("pip-test", image, egui::TextureOptions::NEAREST);
    let texture_id = texture.id();
    doc.pip_texture = Some(texture);
    doc.pip_camera = Some("侧视".to_string());

    let mut view = Viewport::default();
    let mut gizmo = Gizmo::default();
    let mut camera = CameraFeed::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 900.0),
        )),
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        let _ = ui::panels(
            ui,
            &mut engine,
            &mut doc,
            &mut view,
            &mut gizmo,
            &mut camera,
        );
    });
    assert!(
        shapes_contain_texture(&output.shapes, texture_id),
        "选绘画中画后视口应绘制第二相机纹理"
    );
    let bottom_right =
        shapes_texture_rect(&output.shapes, texture_id).expect("画中画纹理 mesh 必须有包围矩形");
    assert!(
        bottom_right.center().x > 640.0 && bottom_right.center().y > 450.0,
        "默认小窗应贴右下角，实际 {bottom_right:?}"
    );
    let default_width = bottom_right.width();
    output.textures_delta.clear();

    doc.pip_scale = 0.4;
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 900.0),
        )),
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        let _ = ui::panels(
            ui,
            &mut engine,
            &mut doc,
            &mut view,
            &mut gizmo,
            &mut camera,
        );
    });
    let bigger =
        shapes_texture_rect(&output.shapes, texture_id).expect("放大后画中画纹理 mesh 仍应存在");
    assert!(
        bigger.width() > default_width,
        "0.40 的小窗应比默认 0.28 宽：{default_width} -> {}",
        bigger.width()
    );
    output.textures_delta.clear();

    doc.pip_corner = 3;
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 900.0),
        )),
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        let _ = ui::panels(
            ui,
            &mut engine,
            &mut doc,
            &mut view,
            &mut gizmo,
            &mut camera,
        );
    });
    let top_left = shapes_texture_rect(&output.shapes, texture_id)
        .expect("切换角落后画中画纹理 mesh 仍应存在");
    assert!(
        top_left.center().x < 640.0 && top_left.center().y < 450.0,
        "切到左上后小窗应贴左上角，实际 {top_left:?}"
    );
    output.textures_delta.clear();

    // 点小窗应把主相机切到小窗相机；这个点击不能再触发主视口拾取。
    doc.pip_corner = 0;
    doc.selected_camera = None;
    let click = bottom_right.center();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 900.0),
        )),
        events: vec![
            egui::Event::PointerMoved(click),
            egui::Event::PointerButton {
                pos: click,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerButton {
                pos: click,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ],
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        let _ = ui::panels(
            ui,
            &mut engine,
            &mut doc,
            &mut view,
            &mut gizmo,
            &mut camera,
        );
    });
    output.textures_delta.clear();
    assert_eq!(
        doc.selected_camera.as_deref(),
        Some("侧视"),
        "点画中画应把主相机切到小窗相机"
    );
}

fn shapes_texture_rect(
    shapes: &[egui::epaint::ClippedShape],
    texture_id: egui::TextureId,
) -> Option<egui::Rect> {
    for clipped in shapes {
        if let Some(rect) = shape_texture_rect(&clipped.shape, texture_id) {
            return Some(rect);
        }
    }
    None
}

fn shape_texture_rect(shape: &egui::Shape, texture_id: egui::TextureId) -> Option<egui::Rect> {
    match shape {
        egui::Shape::Mesh(mesh) if mesh.texture_id == texture_id => {
            let mut rect = egui::Rect::NOTHING;
            for vertex in &mesh.vertices {
                rect.extend_with(vertex.pos);
            }
            Some(rect)
        }
        egui::Shape::Vec(inner) => inner
            .iter()
            .find_map(|shape| shape_texture_rect(shape, texture_id)),
        _ => None,
    }
}

/// 物体材质面板：显式设置过贴图的物体自动展开材质段，四个槽位有「应用/清除」。
#[test]
fn material_texture_slots_have_apply_and_clear_buttons() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    let text = r#"{
        "objects": [
            { "name": "箱", "shape": { "type": "box", "size": [1, 1, 1] } }
        ]
    }"#;
    scene_io::load_text_into_doc(&mut engine, &mut doc, text).expect("场景应能装载");
    doc.selected = Some(0);
    doc.textures.insert(
        "箱".to_string(),
        quench3d_editor::doc::TexturePaths {
            base_color: Some("tex.png".to_string()),
            ..Default::default()
        },
    );
    let ctx = egui::Context::default();
    let mut view = Viewport::default();
    let mut gizmo = Gizmo::default();
    let mut camera = CameraFeed::default();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1280.0, 2200.0),
        )),
        ..Default::default()
    };
    let mut output = ctx.run_ui(input, |ui| {
        let _ = ui::panels(
            ui,
            &mut engine,
            &mut doc,
            &mut view,
            &mut gizmo,
            &mut camera,
        );
    });
    let shapes = std::mem::take(&mut output.shapes);
    output.textures_delta.clear();
    assert!(
        shapes_contain_text(&shapes, "应用基础色贴图（从磁盘读取）"),
        "物体材质面板没有应用基础色贴图按钮"
    );
    assert!(
        shapes_contain_text(&shapes, "清除基础色贴图"),
        "物体材质面板没有清除基础色贴图按钮"
    );
}

fn shapes_contain_texture(
    shapes: &[egui::epaint::ClippedShape],
    texture_id: egui::TextureId,
) -> bool {
    shapes
        .iter()
        .any(|clipped| shape_contains_texture(&clipped.shape, texture_id))
}

fn shape_contains_texture(shape: &egui::Shape, texture_id: egui::TextureId) -> bool {
    match shape {
        egui::Shape::Mesh(mesh) => mesh.texture_id == texture_id,
        egui::Shape::Vec(inner) => inner
            .iter()
            .any(|shape| shape_contains_texture(shape, texture_id)),
        _ => false,
    }
}
