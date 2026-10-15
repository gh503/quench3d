//! 编辑器存/读场景的往返验证（格式与 CLI 共用 `quench3d.scene`）。
//!
//! 为什么值得单独测：编辑器的核心承诺是「CLI 生成 → 编辑器手工改 → CLI 渲染视频」
//! 这条链能走通，而链上最容易悄悄断掉的一环就是**存一次再读一次之后场景变了**
//! （位置漂移、地面下沉、物体改名、丢物体）。这里把默认场景存成 JSON、
//! 用 CLI 的解析器与校验器读回来、再装回引擎，逐项对账。
//!
//! 没有可用 GPU 时跳过（CI 机器通常没有软件光栅驱动）。
//! 整个文件只编译原生目标：wasm 上没有文件系统与 `pollster`，这条链在浏览器里
//! 由 `web_client` 导出的 `scene_roundtrip_probe()` 覆盖（纯 CPU 那半条）。
#![cfg(not(target_arch = "wasm32"))]

use quench3d::prelude::*;
use quench3d_cli::scene_doc::{SceneDoc, ShapeKind};
use quench3d_editor::doc::Doc;
use quench3d_editor::scene_io;

/// 无窗口引擎；没有 GPU 时返回 `None`。
fn headless_engine() -> Option<Engine> {
    let gpu = match pollster::block_on(quench3d::render::GpuContext::headless(
        64,
        64,
        Default::default(),
    )) {
        Ok(gpu) => gpu,
        Err(e) => {
            eprintln!("跳过：没有可用的 GPU（{e}）");
            return None;
        }
    };
    Some(Engine::new(
        gpu,
        &EngineConfig {
            width: 64,
            height: 64,
            ..Default::default()
        },
    ))
}

#[test]
fn scene_json_roundtrip_keeps_objects_and_ground() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);

    let before: Vec<(String, Vec3)> = engine
        .scene
        .objects
        .iter()
        .map(|o| (o.name.clone(), o.transform.position))
        .collect();
    assert!(before.len() >= 5, "默认场景应当有地面 + 若干物体");

    let saved = scene_io::scene_to_json(&engine, &doc).expect("导出必须成功");
    assert!(
        saved.skipped.is_empty(),
        "默认场景的物体都该能用场景格式表达，这些被跳过了：{:?}",
        saved.skipped
    );

    // 用 CLI 的解析器 + 校验器读回来：格式必须是同一个，而不是"编辑器自己认"。
    let parsed = SceneDoc::from_json(&saved.json).expect("CLI 应当能解析编辑器存的场景");
    let diagnostics = parsed.validate();
    assert!(
        !SceneDoc::has_errors(&diagnostics),
        "编辑器存的场景应当通过 CLI 校验：{diagnostics:?}"
    );
    let ground = parsed.ground.as_ref().expect("应当写成 ground 字段");
    let shape = &parsed.objects[0].shape;
    assert_eq!(shape.kind, ShapeKind::Box, "第一个物体应当是方块");

    // 装回引擎（同一个引擎，先清空重建）——模拟"打开文件"。
    let (count, _) = scene_io::load_into_engine(&mut engine, &saved.json).expect("导入必须成功");
    assert_eq!(count, before.len(), "物体数量应当一模一样");

    // 1) 逐个物体对账：名字没丢、位置没漂。
    for (name, position) in &before {
        let object = engine
            .scene
            .objects
            .iter()
            .find(|o| &o.name == name)
            .unwrap_or_else(|| panic!("导入后丢了物体「{name}」"));
        let drift = (object.transform.position - *position).length();
        assert!(
            drift < 0.01,
            "「{name}」位置漂移了 {drift:.4}：{position:?} -> {:?}",
            object.transform.position
        );
    }

    // 2) 地面高度：物理半空间在薄板顶面，存读一轮之后必须还在同一个高度。
    let ground_object = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "地面")
        .expect("地面应当在");
    assert!(
        (ground_object.transform.position.y - ground.y).abs() < 1e-4,
        "地面高度对不上：导入后物体 y={}，文档里 y={}",
        ground_object.transform.position.y,
        ground.y
    );
    let ground_body = engine
        .world
        .body(ground_object.body.expect("地面应当有刚体"))
        .expect("地面刚体应当在");
    assert!(ground_body.is_static(), "地面应当是静态体");
    assert!(
        matches!(ground_body.shape, Shape::Plane { .. }),
        "地面的物理形状应当是半空间"
    );

    // 3) 物理世界也照着重建了：刚体数量与非地面物体数对得上。
    assert_eq!(
        engine.world.body_count(),
        parsed.objects.len() + 1,
        "刚体数量 = 物体数 + 地面"
    );
}

#[test]
fn scene_to_json_reports_unrepresentable_objects() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    // 塞一个场景格式表达不了的物体：三角网碰撞体。
    let body = engine
        .world
        .add_builder(BodyBuilder::r#static(Shape::TriMesh {
            vertices: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
            indices: vec![0, 1, 2],
        }));
    let mesh = engine.assets.unit_cube;
    let material = engine.scene.objects[1].material;
    let index = engine
        .scene
        .add_named(mesh, material, Transform::IDENTITY, "三角网");
    engine.scene.objects[index].body = Some(body);

    let saved = scene_io::scene_to_json(&engine, &doc).expect("导出必须成功");
    assert_eq!(
        saved.skipped.len(),
        1,
        "应当只报告一个表达不了的物体：{:?}",
        saved.skipped
    );
    assert!(
        saved.skipped[0].contains("三角网"),
        "报告里要带上物体名，便于用户定位：{:?}",
        saved.skipped
    );
}
