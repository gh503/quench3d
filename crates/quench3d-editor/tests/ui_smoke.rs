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

/// 无底稿的运行态流体不能被菜单添加错配；拒绝也不能悄悄多记历史。
#[test]
fn add_menu_particle_fluid_rejects_unknown_runtime_origin_without_mutation() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.spawn(&mut engine, quench3d_editor::doc::Primitive::Cube);
    doc.reset_history(&engine);
    doc.spawn(&mut engine, quench3d_editor::doc::Primitive::Sphere);
    doc.record_edit(&engine);
    engine
        .world
        .add_fluid(quench3d_physics::FluidDesc::default())
        .unwrap();
    engine.paused = false;
    engine.renderer.options.water.enabled = false;
    let positions = engine.world.fluids()[0].particles().to_vec();
    let before = scene_io::scene_to_json(&engine, &doc).unwrap().json;
    let objects = engine.scene.len();
    let selected = doc.selected;
    let mut view = Viewport::default();
    let mut gizmo = Gizmo::default();
    let mut camera = CameraFeed::default();
    let ctx = egui::Context::default();
    let mut time = 0.0;
    let mut run = |engine: &mut Engine, doc: &mut Doc, events| {
        time += 1.0;
        let mut output = ctx.run_ui(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 900.0),
                )),
                events,
                focused: true,
                ..Default::default()
            },
            |ui| {
                ui::panels(ui, engine, doc, &mut view, &mut gizmo, &mut camera);
            },
        );
        output.textures_delta.clear();
        output.shapes
    };
    let click = |pos| {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
        ]
    };
    run(&mut engine, &mut doc, vec![]);
    let shapes = run(&mut engine, &mut doc, vec![]);
    let add = text_shape_rect(&shapes, "添加").unwrap().center();
    run(&mut engine, &mut doc, click(add));
    let shapes = run(&mut engine, &mut doc, vec![]);
    let menu_shapes: Vec<_> = shapes
        .into_iter()
        .filter(|s| {
            text_shape_rect(std::slice::from_ref(s), "粒子流体")
                .is_some_and(|rect| rect.center().x < 900.0)
        })
        .collect();
    let fluid = text_shape_rect(&menu_shapes, "粒子流体")
        .expect("菜单入口可见")
        .center();
    run(
        &mut engine,
        &mut doc,
        vec![egui::Event::PointerMoved(fluid)],
    );
    run(&mut engine, &mut doc, vec![]);
    let tooltip = run(&mut engine, &mut doc, vec![]);
    assert!(
        shapes_contain_text(&tooltip, "缺少完整流体来源配置，无法安全添加"),
        "disabled Add must explain the missing origin to the user"
    );
    run(&mut engine, &mut doc, click(fluid));
    assert_eq!(
        engine.world.fluid_count(),
        1,
        "unknown runtime fluid must block Add before world mutation"
    );
    assert_eq!(engine.world.fluids()[0].particles(), positions);
    assert_eq!(engine.scene.len(), objects);
    assert_eq!(doc.selected, selected);
    assert!(doc.base_doc().is_none());
    assert!(!engine.paused);
    assert!(!engine.renderer.options.water.enabled);
    assert_eq!(scene_io::scene_to_json(&engine, &doc).unwrap().json, before);
    assert!(!doc.add_particle_fluid(&mut engine));
    assert!(doc.status.contains("缺少完整流体来源配置"));
    assert_eq!(engine.world.fluid_count(), 1);
    assert_eq!(engine.world.fluids()[0].particles(), positions);
    assert!(doc.base_doc().is_none());
    // 菜单拒绝后第一步撤销应直接回到先前的单物体状态。
    doc.undo(&mut engine);
    assert_eq!(
        engine.scene.len(),
        objects - 1,
        "rejected Add must not insert a history step"
    );
}

/// 只接受可对应的底稿；空的新文档仍可直接添加。
#[test]
fn particle_fluid_add_origin_guard_covers_empty_and_mismatched_documents() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    assert!(doc.can_add_particle_fluid(&engine));
    assert!(doc.add_particle_fluid(&mut engine));
    assert_eq!(engine.world.fluid_count(), 1);
    assert_eq!(doc.base_doc().unwrap().fluids.len(), 1);
    assert!(doc.can_add_particle_fluid(&engine));
    for configured in [0, 2] {
        let spec = doc.base_doc().unwrap().fluids[0].clone();
        doc.base_doc_mut().unwrap().fluids = vec![spec.clone(); configured];
        let before = serde_json::to_value(doc.base_doc().unwrap()).unwrap();
        let positions = engine.world.fluids()[0].particles().to_vec();
        let velocities = engine.world.fluids()[0].velocities().to_vec();
        let revision = engine.world.fluids()[0].revision();
        let objects = engine.scene.len();
        engine.paused = false;
        engine.renderer.options.water.enabled = false;
        assert!(!doc.can_add_particle_fluid(&engine));
        assert!(!doc.add_particle_fluid(&mut engine));
        assert!(doc.status.contains("缺少完整流体来源配置"));
        assert_eq!(engine.world.fluid_count(), 1);
        assert_eq!(engine.world.fluids()[0].particles(), positions);
        assert_eq!(engine.world.fluids()[0].velocities(), velocities);
        assert_eq!(engine.world.fluids()[0].revision(), revision);
        assert_eq!(engine.scene.len(), objects);
        assert!(!engine.paused);
        assert!(!engine.renderer.options.water.enabled);
        assert_eq!(
            serde_json::to_value(doc.base_doc().unwrap()).unwrap(),
            before
        );
        doc.base_doc_mut().unwrap().fluids = vec![spec];
    }
    // 完整标准加载的零流体底稿也允许开始添加。
    scene_io::load_text_into_doc(&mut engine, &mut doc, r#"{"fluids":[]}"#).unwrap();
    assert!(doc.can_add_particle_fluid(&engine));
    assert!(doc.add_particle_fluid(&mut engine));
    assert_eq!(engine.world.fluid_count(), 1);
    assert_eq!(doc.base_doc().unwrap().fluids.len(), 1);
}

/// 添加菜单必须真实创建粒子、表面代理，并支持标准存盘与历史。
#[test]
fn add_menu_particle_fluid_survives_save_reload_and_history() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    doc.reset_history(&engine);
    let mut view = Viewport::default();
    let mut gizmo = Gizmo::default();
    let mut camera = CameraFeed::default();
    let ctx = egui::Context::default();
    let mut run = |engine: &mut Engine, doc: &mut Doc, events| {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 900.0),
                )),
                events,
                focused: true,
                ..Default::default()
            },
            |ui| {
                ui::panels(ui, engine, doc, &mut view, &mut gizmo, &mut camera);
            },
        );
        output.textures_delta.clear();
        output.shapes
    };
    let click = |pos| {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
        ]
    };
    run(&mut engine, &mut doc, vec![]);
    let shapes = run(&mut engine, &mut doc, vec![]);
    let add = text_shape_rect(&shapes, "添加")
        .expect("添加菜单可见")
        .center();
    run(&mut engine, &mut doc, click(add));
    let shapes = run(&mut engine, &mut doc, vec![]);
    let menu_shapes: Vec<_> = shapes
        .into_iter()
        .filter(|s| {
            text_shape_rect(std::slice::from_ref(s), "粒子流体")
                .is_some_and(|rect| rect.center().x < 900.0)
        })
        .collect();
    let fluid = text_shape_rect(&menu_shapes, "粒子流体")
        .expect("添加菜单必须提供粒子流体入口")
        .center();
    run(&mut engine, &mut doc, click(fluid));
    assert_eq!(engine.world.fluids().len(), 1);
    let points = engine.world.fluids()[0].particles().to_vec();
    assert!(!points.is_empty() && points.len() < 2000);
    assert!(engine.paused);
    assert!(engine
        .scene
        .objects
        .iter()
        .any(|o| o.fluid_surface.is_some_and(|v| v.fluid == 0 && v.mesh)));
    let saved = scene_io::scene_to_json(&engine, &doc).unwrap();
    let parsed = quench3d_cli::scene_doc::SceneDoc::from_json(&saved.json).unwrap();
    let spec = &parsed.fluids[0];
    assert_eq!(spec.center, Some([0.0, 1.0, 0.0]));
    assert_eq!(spec.size, [1.0; 3]);
    assert_eq!(spec.spacing, 0.15);
    assert_eq!(spec.radius, Some(0.06));
    assert_eq!(spec.max_particles, 2000);
    assert!(spec.surface_mesh);
    assert!(spec.initial_positions.is_none());
    doc.undo(&mut engine);
    assert!(engine.world.fluids().is_empty());
    doc.redo(&mut engine);
    assert_eq!(engine.world.fluids()[0].particles(), points.as_slice());
    scene_io::load_text_into_doc(&mut engine, &mut doc, &saved.json).unwrap();
    assert_eq!(engine.world.fluids().len(), 1);
    assert_eq!(engine.world.fluids()[0].particles(), points.as_slice());
    assert_eq!(engine.world.fluids()[0].desc().spacing, 0.15);
    assert!(engine
        .scene
        .objects
        .iter()
        .any(|o| o.fluid_surface.is_some_and(|v| v.mesh)));
    run(&mut engine, &mut doc, vec![]);
    let shapes = run(&mut engine, &mut doc, vec![]);
    let add = text_shape_rect(&shapes, "添加").unwrap().center();
    run(&mut engine, &mut doc, click(add));
    let shapes = run(&mut engine, &mut doc, vec![]);
    let menu_shapes: Vec<_> = shapes
        .into_iter()
        .filter(|s| {
            text_shape_rect(std::slice::from_ref(s), "粒子流体")
                .is_some_and(|rect| rect.center().x < 900.0)
        })
        .collect();
    let second = text_shape_rect(&menu_shapes, "粒子流体").unwrap().center();
    run(&mut engine, &mut doc, click(second));
    assert_eq!(engine.world.fluids().len(), 2);
    assert_eq!(engine.world.fluids()[0].particles(), points.as_slice());
    assert_eq!(engine.world.fluids()[1].particles(), points.as_slice());
    let surfaces: Vec<_> = (0..2)
        .map(|fluid| {
            engine
                .scene
                .objects
                .iter()
                .find(|o| o.fluid_surface.is_some_and(|v| v.fluid == fluid && v.mesh))
                .expect("每块流体必须有自己的连续表面")
        })
        .collect();
    assert_ne!(
        surfaces[0].mesh, surfaces[1].mesh,
        "表面刷新不能覆盖另一块流体的可变网格"
    );
    // 同一菜单构造的三块流体，删中间再加，不能复用仍在使用的旧 index 命名网格。
    doc.add_particle_fluid(&mut engine);
    assert!(doc.remove_particle_fluid(&mut engine, 1));
    let survivor = engine
        .scene
        .objects
        .iter()
        .find(|o| o.fluid_surface.is_some_and(|v| v.fluid == 1))
        .unwrap()
        .mesh;
    doc.add_particle_fluid(&mut engine);
    let added = engine
        .scene
        .objects
        .iter()
        .find(|o| o.fluid_surface.is_some_and(|v| v.fluid == 2))
        .unwrap()
        .mesh;
    assert_ne!(
        survivor, added,
        "delete-middle then Add must allocate an independent mutable surface"
    );
}

/// 真正点右侧列表删除中间块，验证物理、配置、代理及历史均指向幸存块。
#[test]
fn inspector_delete_middle_particle_fluid_preserves_mapping_and_history() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    let json = serde_json::json!({
        "format": "quench3d.scene", "version": 2,
        "objects": [{"name":"keep", "shape":{"type":"box", "size":[1.0,1.0,1.0]}, "position":[9.0,3.0,0.0]}],
        "waters": [{"size":[2.0,2.0], "level":-5.0}],
        "fluids": ([-3.0,0.0,3.0].map(|x| serde_json::json!({
            "center":[x,2.0,0.0], "size":[0.3,0.3,0.3], "spacing":0.15,
            "surface_mesh":true, "max_particles":30
        })))
    }).to_string();
    scene_io::load_text_into_doc(&mut engine, &mut doc, &json).unwrap();
    // 显式覆盖加载/运行时可能存在的四类绑定；辅助代理不作为普通对象导出。
    for fluid in 0..3 {
        for kind in 0..3 {
            let source = engine.scene.find("keep").unwrap();
            let material = engine.scene.objects[source].material;
            let object = engine.scene.add_named(
                engine.assets.unit_sphere,
                material,
                Transform::IDENTITY,
                &format!("probe-{fluid}-{kind}"),
            );
            let proxy = &mut engine.scene.objects[object];
            proxy.generated = true;
            match kind {
                0 => {
                    proxy.fluid_particle = Some(quench3d::scene::FluidParticleVisual {
                        fluid,
                        particle: 0,
                        radius: 0.05,
                    })
                }
                1 => {
                    proxy.fluid_foam = Some(quench3d::scene::FluidFoamVisual {
                        fluid,
                        index: 0,
                        radius: 0.05,
                    })
                }
                _ => {
                    proxy.fluid_bubble = Some(quench3d::scene::FluidBubbleVisual {
                        fluid,
                        index: 0,
                        radius: 0.05,
                    })
                }
            }
        }
    }
    let material = engine.scene.objects[engine.scene.find("keep").unwrap()].material;
    let tail = engine.scene.add_named(
        engine.assets.unit_cube,
        material,
        Transform::IDENTITY,
        "selected-tail",
    );
    doc.select_only(Some(tail));
    doc.extra_selected.push(engine.scene.find("keep").unwrap());
    doc.hovered = Some(tail);
    doc.reset_history(&engine);
    let body_count = engine.world.body_count();
    let positions: Vec<_> = engine
        .world
        .fluids()
        .iter()
        .map(|f| f.particles().to_vec())
        .collect();
    let meshes: Vec<_> = (0..3)
        .map(|i| {
            engine
                .scene
                .objects
                .iter()
                .find(|o| o.fluid_surface.is_some_and(|v| v.mesh && v.fluid == i))
                .unwrap()
                .mesh
        })
        .collect();
    let mut view = Viewport::default();
    let mut gizmo = Gizmo::default();
    let mut camera = CameraFeed::default();
    let ctx = egui::Context::default();
    let mut run = |engine: &mut Engine, doc: &mut Doc, events| {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 1800.0),
                )),
                events,
                focused: true,
                ..Default::default()
            },
            |ui| {
                ui::panels(ui, engine, doc, &mut view, &mut gizmo, &mut camera);
            },
        );
        output.textures_delta.clear();
        output.shapes
    };
    run(&mut engine, &mut doc, vec![]);
    let shapes = run(&mut engine, &mut doc, vec![]);
    let row = text_shape_rect(&shapes, "流体 1 · 27 颗粒子")
        .expect("独立粒子流体列表必须显示中间块 index 与粒子数");
    let button = text_shape_rect(&shapes, "删除流体 1").expect("右侧必须有中间块删除按钮");
    assert!((row.center().y - button.center().y).abs() < 20.0);
    let pos = button.center();
    run(
        &mut engine,
        &mut doc,
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
        ],
    );
    assert_eq!(engine.world.fluid_count(), 2);
    assert_eq!(engine.world.body_count(), body_count);
    assert_eq!(engine.world.waters().len(), 1);
    assert_eq!(
        engine.scene.objects[doc.selected.unwrap()].name,
        "selected-tail"
    );
    assert_eq!(engine.scene.objects[doc.extra_selected[0]].name, "keep");
    for kind in 0..3 {
        assert!(engine.scene.find(&format!("probe-1-{kind}")).is_none());
        let survivor =
            &engine.scene.objects[engine.scene.find(&format!("probe-2-{kind}")).unwrap()];
        match kind {
            0 => assert_eq!(survivor.fluid_particle.unwrap().fluid, 1),
            1 => assert_eq!(survivor.fluid_foam.unwrap().fluid, 1),
            _ => assert_eq!(survivor.fluid_bubble.unwrap().fluid, 1),
        }
    }
    for (new, old) in [(0, 0), (1, 2)] {
        assert_eq!(engine.world.fluids()[new].particles(), positions[old]);
        let surface = engine
            .scene
            .objects
            .iter()
            .find(|o| o.fluid_surface.is_some_and(|v| v.mesh && v.fluid == new))
            .unwrap();
        assert_eq!(surface.mesh, meshes[old]);
    }
    assert_eq!(
        engine
            .scene
            .objects
            .iter()
            .filter(|o| o.fluid_surface.is_some())
            .count(),
        2
    );
    engine
        .scene
        .refresh_fluid_surfaces(&engine.world, &mut engine.renderer, &engine.gpu);
    for (new, old) in [(0, 0), (1, 2)] {
        let (_, count, bounds) = engine.renderer.mesh_info(meshes[old]).unwrap();
        assert!(count > 0);
        assert!((bounds.center().x - engine.world.fluids()[new].desc().center.x).abs() < 0.3);
    }
    let saved = scene_io::scene_to_json(&engine, &doc).unwrap();
    let parsed = quench3d_cli::scene_doc::SceneDoc::from_json(&saved.json).unwrap();
    assert_eq!(parsed.fluids.len(), 2);
    assert_eq!(parsed.fluids[1].center, Some([3.0, 2.0, 0.0]));
    assert!(parsed.fluids.iter().all(|f| f.initial_positions.is_none()));
    doc.undo(&mut engine);
    assert_eq!(engine.world.fluid_count(), 3);
    doc.redo(&mut engine);
    assert_eq!(engine.world.fluid_count(), 2);
    engine.world.step(1.0 / 60.0);
    assert!(engine.world.fluids().iter().all(|f| f
        .particles()
        .iter()
        .chain(f.velocities())
        .all(|p| p.is_finite())));
    scene_io::load_text_into_doc(&mut engine, &mut doc, &saved.json).unwrap();
    assert_eq!(engine.world.fluid_count(), 2);
    assert_eq!(engine.world.fluids()[1].particles(), positions[2]);
}

#[test]
fn particle_fluid_inspector_empty_and_missing_config_are_explicit() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    let mut view = Viewport::default();
    let mut gizmo = Gizmo::default();
    let mut camera = CameraFeed::default();
    let ctx = egui::Context::default();
    let mut run = |engine: &mut Engine, doc: &mut Doc| {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1280.0, 900.0),
                )),
                ..Default::default()
            },
            |ui| {
                ui::panels(ui, engine, doc, &mut view, &mut gizmo, &mut camera);
            },
        );
        output.textures_delta.clear();
        output.shapes
    };
    run(&mut engine, &mut doc);
    let shapes = run(&mut engine, &mut doc);
    assert!(shapes_contain_text(&shapes, "请从「添加」菜单添加"));
    engine
        .world
        .add_fluid(quench3d_physics::FluidDesc::default())
        .unwrap();
    let positions = engine.world.fluids()[0].particles().to_vec();
    let shapes = run(&mut engine, &mut doc);
    assert!(shapes_contain_text(
        &shapes,
        "缺少流体场景配置，无法安全删除"
    ));
    assert!(!doc.remove_particle_fluid(&mut engine, 0));
    assert_eq!(engine.world.fluids()[0].particles(), positions);
    assert!(doc.base_doc().is_none());
}

#[test]
fn loaded_particle_fluid_meshes_keep_separate_current_bounds() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    scene_io::load_text_into_doc(
        &mut engine,
        &mut doc,
        r#"{
        "fluids":[
            {"center":[-3,2,0],"size":[0.6,0.6,0.6],"spacing":0.15,"surface_mesh":true},
            {"center":[3,2,0],"size":[0.9,0.6,0.6],"spacing":0.15,"surface_mesh":true}
        ]
    }"#,
    )
    .unwrap();
    engine
        .scene
        .refresh_fluid_surfaces(&engine.world, &mut engine.renderer, &engine.gpu);
    let meshes: Vec<_> = (0..2)
        .map(|i| {
            engine
                .scene
                .objects
                .iter()
                .find(|o| o.fluid_surface.is_some_and(|v| v.mesh && v.fluid == i))
                .unwrap()
                .mesh
        })
        .collect();
    for (index, mesh) in meshes.iter().enumerate() {
        let (_, count, bounds) = engine.renderer.mesh_info(*mesh).unwrap();
        assert!(count > 0);
        let center = engine.world.fluids()[index].desc().center;
        assert!(
            (bounds.center().x - center.x).abs() < 0.3,
            "each uploaded surface must cover its own particle cloud: {index} {bounds:?}"
        );
    }
    assert_ne!(meshes[0], meshes[1]);
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
            start_time: 0.0,
            time: 0.25,
            speed: 1.0,
            looping: true,
            finished: false,
        },
        secondary: Some(ModelAnimationPlayer {
            animation: "walk".into(),
            duration: 1.0,
            start_time: 0.0,
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
                    loop_start: 0.0,
                },
                ModelAnimatorState {
                    name: "走".into(),
                    animation: "idle".into(),
                    speed: 1.0,
                    looping: true,
                    start: 0.0,
                    loop_start: 0.0,
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
            start_time: 0.0,
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
fn text_shape_rect(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<egui::Rect> {
    fn walk(shape: &egui::Shape, needle: &str) -> Option<egui::Rect> {
        match shape {
            egui::Shape::Text(text) if text.galley.text().contains(needle) => {
                Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
            }
            egui::Shape::Vec(inner) => inner.iter().find_map(|s| walk(s, needle)),
            _ => None,
        }
    }
    shapes
        .iter()
        .find_map(|clipped| walk(&clipped.shape, needle))
}

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
            egui::vec2(1280.0, 1600.0),
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
    for label in [
        "文件", "编辑", "添加", "变换", "播放", "视图", "渲染", "布局",
    ] {
        assert!(
            shapes_contain_text(&output.shapes, label),
            "菜单栏没有画出「{label}」菜单"
        );
    }
    assert!(
        shapes_contain_text(&output.shapes, "搜索"),
        "场景树没有搜索框"
    );
    assert!(
        shapes_contain_text(&output.shapes, "关节"),
        "常用工具条没有画出关节调试开关"
    );
    assert!(
        shapes_contain_text(&output.shapes, "相机与画中画"),
        "属性面板没有画出「相机与画中画」分区"
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
        shapes_contain_text(&output.shapes, "负载"),
        "事件行没有显示负载输入框"
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
        shapes_contain_text(&output.shapes, "多轨总览（2 条）"),
        "多轨总览没有出现在绘制图元里"
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

/// 场景树搜索真的在过滤，而不只是画了一个输入框。
#[test]
fn outliner_search_hides_non_matching_objects() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    doc.outliner_filter = "地面".to_string();
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
    let shapes = std::mem::take(&mut output.shapes);
    output.textures_delta.clear();
    assert!(
        shapes_contain_text(&shapes, "地面"),
        "过滤条件命中的物体应当保留"
    );
    assert!(
        !shapes_contain_text(&shapes, "斜面"),
        "过滤条件没命中的物体（斜面）不应出现在场景树里"
    );
}

/// 光照分区默认折叠；点一下标题后，调色与太阳/环境光颜色控件必须真的画出来。
#[test]
fn lighting_header_opens_and_shows_color_grade_controls() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    let mut view = Viewport::default();
    let mut gizmo = Gizmo::default();
    let mut camera = CameraFeed::default();
    let ctx = egui::Context::default();
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 900.0));

    let mut run = |events: Vec<egui::Event>| {
        let input = egui::RawInput {
            screen_rect: Some(screen),
            events,
            focused: true,
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
        output.shapes
    };

    let first = run(Vec::new());
    let header = text_shape_rect(&first, "光照").expect("光照分区标题没画出来");
    let mut third = Vec::new();
    // 折叠标题的箭头在文字左侧；egui 的命中区随主题/字号变，这里扫几个
    // 可能的位置，点开就停（同时避免点到文字外导致测试假绿）。
    for dx in [-22.0, -18.0, -14.0, -10.0, -6.0, 0.0, 6.0] {
        let pos = header.left_center() + egui::vec2(dx, 0.0);
        let events = vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ];
        let second = run(events);
        third = if shapes_contain_text(&second, "调色 饱和/对比/色温") {
            second
        } else {
            run(Vec::new())
        };
        if shapes_contain_text(&third, "调色 饱和/对比/色温") {
            break;
        }
    }
    for needle in ["调色 饱和/对比/色温", "太阳颜色", "环境光 强度/颜色"] {
        assert!(
            shapes_contain_text(&third, needle),
            "点开光照分区后没有画出「{needle}」"
        );
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
        shapes_contain_text(&shapes, "投射阴影"),
        "属性面板没有「投射阴影」开关"
    );
    assert!(
        shapes_contain_text(&shapes, "接收阴影"),
        "属性面板没有「接收阴影」开关"
    );
    assert!(
        shapes_contain_text(&shapes, "应用基础色贴图（从磁盘读取）"),
        "物体材质面板没有应用基础色贴图按钮"
    );
    assert!(
        shapes_contain_text(&shapes, "清除基础色贴图"),
        "物体材质面板没有清除基础色贴图按钮"
    );
    assert!(
        shapes_contain_text(&shapes, "环境光遮蔽贴图") && shapes_contain_text(&shapes, "AO 强度"),
        "材质面板没有 AO 贴图/强度控件"
    );
    assert!(
        shapes_contain_text(&shapes, "UV 偏移"),
        "物体材质面板没有绘制 UV 偏移"
    );
    assert!(
        shapes_contain_text(&shapes, "UV 旋转（度）"),
        "物体材质面板没有绘制 UV 旋转"
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

/// 视口点击必须回填**窗口绝对坐标**：3D 画在整张窗口纹理上，面板只是盖在上面。
/// 如果这里回填的是 `pos - rect.min`，点选就会整体往左上偏。
#[test]
fn viewport_click_uses_window_coordinates() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let ctx = egui::Context::default();
    let mut doc = Doc::new();
    doc.selected = None;
    let mut view = Viewport::default();
    let mut gizmo = Gizmo::default();
    let mut camera = CameraFeed::new();
    let screen = egui::vec2(1280.0, 900.0);
    let run = |ctx: &egui::Context,
               engine: &mut Engine,
               doc: &mut Doc,
               view: &mut Viewport,
               gizmo: &mut Gizmo,
               camera: &mut CameraFeed,
               events: Vec<egui::Event>| {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
            events,
            ..Default::default()
        };
        ctx.run_ui(input, |ui| {
            let _ = ui::panels(ui, engine, doc, view, gizmo, camera);
        })
        .textures_delta
        .clear();
    };
    // 第一帧只布局，拿到中央视口矩形。
    run(
        &ctx,
        &mut engine,
        &mut doc,
        &mut view,
        &mut gizmo,
        &mut camera,
        vec![],
    );
    let click = view.rect.center();
    assert!(
        click.x > view.rect.width() * 0.5,
        "测试前提：中央视口不是以窗口左上角为原点"
    );
    run(
        &ctx,
        &mut engine,
        &mut doc,
        &mut view,
        &mut gizmo,
        &mut camera,
        vec![
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
    );
    let got = view.click.expect("点击应回填到 Viewport");
    assert!(
        (got.x - click.x).abs() < 0.5 && (got.y - click.y).abs() < 0.5,
        "点击坐标必须是窗口绝对坐标：期望 {click:?}，实际 {got:?}（视口左上角 {:?}）",
        view.rect.min
    );
}

#[test]
fn primitive_shape_panel_changes_radius_through_drag() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.spawn(&mut engine, quench3d_editor::doc::Primitive::Sphere);
    doc.reset_history(&engine);
    let handle = engine.scene.objects[doc.selected.unwrap()].body.unwrap();
    let mut view = Viewport::default();
    let mut gizmo = Gizmo::default();
    let mut camera = CameraFeed::default();
    let ctx = egui::Context::default();
    let mut run = |engine: &mut Engine, events| {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 4000.0),
                )),
                focused: true,
                events,
                ..Default::default()
            },
            |ui| {
                ui::panels(ui, engine, &mut doc, &mut view, &mut gizmo, &mut camera);
            },
        );
        output.textures_delta.clear();
        output.shapes
    };
    run(&mut engine, vec![]);
    let shapes = run(&mut engine, vec![]);
    let label = text_shape_rect(&shapes, "半径").expect("shape radius control shown");
    let value = shapes
        .iter()
        .filter_map(|s| text_shape_rect(std::slice::from_ref(s), "0.5"))
        .filter(|r| {
            (r.center().y - label.center().y).abs() < 8.0 && r.center().x > label.center().x
        })
        .min_by(|a, b| a.min.x.total_cmp(&b.min.x))
        .expect("radius value beside label");
    let pos = value.center();
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    run(
        &mut engine,
        vec![egui::Event::PointerMoved(pos), button(pos, true)],
    );
    let moved = pos + egui::vec2(30.0, 0.0);
    run(&mut engine, vec![egui::Event::PointerMoved(moved)]);
    run(&mut engine, vec![button(moved, false)]);
    assert!(
        matches!(engine.world.body(handle).unwrap().shape, quench3d_physics::Shape::Sphere { radius } if radius > 0.5)
    );
    assert!(engine.paused);
}

#[test]
fn fly_camera_panel_edits_fov_and_switches_orthographic() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    engine.camera_mode = CameraMode::Fly;
    engine.fly.fov_y = 70_f32.to_radians();
    let original_orbit_fov = engine.orbit.fov_y;
    let mut view = Viewport::default();
    let mut gizmo = Gizmo::default();
    let mut camera = CameraFeed::default();
    let ctx = egui::Context::default();
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 2400.0));
    let mut run = |engine: &mut Engine, events: Vec<egui::Event>| {
        let input = egui::RawInput {
            screen_rect: Some(screen),
            events,
            focused: true,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            ui::panels(ui, engine, &mut doc, &mut view, &mut gizmo, &mut camera);
        });
        output.textures_delta.clear();
        output.shapes
    };
    let _ = run(&mut engine, vec![]);
    let shapes = run(&mut engine, vec![]);
    let fov = text_shape_rect(&shapes, "70.0")
        .or_else(|| text_shape_rect(&shapes, "70"))
        .expect("flight FOV value must be shown");
    let pos = fov.center();
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    run(
        &mut engine,
        vec![egui::Event::PointerMoved(pos), button(pos, true)],
    );
    let moved = pos + egui::vec2(30.0, 0.0);
    run(&mut engine, vec![egui::Event::PointerMoved(moved)]);
    let shapes = run(&mut engine, vec![button(moved, false)]);
    assert_ne!(engine.fly.fov_y.to_bits(), 70_f32.to_radians().to_bits());
    assert_eq!(engine.orbit.fov_y.to_bits(), original_orbit_fov.to_bits());
    let ortho = text_shape_rect(&shapes, "正交").expect("orthographic checkbox shown");
    let eye = engine.camera().position;
    let forward = engine.camera().forward();
    let pos = ortho.center();
    run(
        &mut engine,
        vec![
            egui::Event::PointerMoved(pos),
            button(pos, true),
            button(pos, false),
        ],
    );
    assert_eq!(engine.camera_mode, CameraMode::Orbit);
    assert!(engine.orbit.orthographic_height.is_some());
    assert!((engine.camera().position - eye).length() < 1e-4);
    assert!(engine.camera().forward().dot(forward) > 0.99999);
}
