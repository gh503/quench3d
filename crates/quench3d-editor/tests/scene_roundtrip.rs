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

/// 编辑器只"懂"物体/地面/材质标量/相机 —— 但它**不能顺手把别的段丢掉**。
///
/// 这条测试钉的是一个真实存在过的静默数据损坏：`scene_to_json` 曾经从
/// `SceneDoc::default()` 起头，于是「CLI 生成一份带光照/渲染设置/模型的场景
/// → 编辑器改一改 → 存盘」之后，`lighting`、`simulate`、`render`、`models`、
/// 材质的四个贴图槽会被**悄悄清零**，而文件看上去完全正常。
///
/// 判据（全部是"可复核的字符串/语义"）：
/// 1. 编辑器不建模的段必须原样出现在导出结果里；
/// 2. 贴图引用与 UV 参数不能丢（材质按同名物体继承）；
/// 3. 存 → 读 → 再存 必须**字节相同**（存储是固定点）；
/// 4. 引用了已删除物体的关节/动作要被清掉（否则文件自己读不回来）。
#[test]
fn export_preserves_sections_the_editor_does_not_model() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();

    // 场景格式的校验器要求贴图**真实存在**（这是好事：错的路径早点报，别到渲染时才发现）。
    // 所以这里先造一张 8×8 棋盘 PNG 当贴图。
    let texture_path = std::env::temp_dir().join("quench3d-editor-test-texture.png");
    let texture =
        quench3d::render::ImageData::checker(8, 2, [255, 255, 255, 255], [40, 40, 40, 255]);
    quench3d::write_png(&texture, &texture_path).expect("造测试贴图必须成功");
    // JSON 里的反斜杠要转义（Windows 上路径带 `\`）。
    let texture_json = texture_path.to_string_lossy().replace('\\', "\\\\");

    // 手写一份"CLI 生成"的场景：带光照、仿真与渲染设置、探针、贴图材质、关节。
    let source = r#"{
      "format": "quench3d.scene",
      "version": 1,
      "name": "preserve",
      "gravity": [0, -9.81, 0],
      "ground": { "size": 24, "friction": 0.7 },
      "lighting": { "preset": "studio", "sun_direction": [-0.4, -1, -0.35], "sun_intensity": 3.5, "ambient": 0.25, "exposure": 1.4, "shadows": true, "sky": true },
      "simulate": { "steps": 240, "dt": 0.004 },
      "render": { "output": "out.png", "width": 800, "height": 600, "msaa": 8 },
      "probes": [ { "name": "probe-a", "position": [0, 1, 0], "radius": 2.5 } ],
      "materials": {
        "wood": { "color": [0.78, 0.48, 0.26], "roughness": 0.55, "uv_scale": 3.0, "base_color_texture": "__TEXTURE__", "normal_scale": 1.5, "double_sided": true }
      },
      "objects": [
        { "name": "box", "shape": { "type": "box", "size": [0.8, 0.8, 0.8] },
          "position": [0, 2.0, 0], "material": "wood", "body": "dynamic", "density": 400 },
        { "name": "peg", "shape": { "type": "sphere", "radius": 0.25 },
          "position": [0, 4.0, 0], "body": "static" }
      ],
      "joints": [ { "type": "hinge", "a": "box", "b": "peg", "axis": [0, 1, 0] } ]
    }"#
    .replace("__TEXTURE__", &texture_json);

    let (count, _) =
        scene_io::load_text_into_doc(&mut engine, &mut doc, &source).expect("载入必须成功");
    assert!(count >= 2, "至少应当装出 box / peg（实际 {count}）");

    let saved = scene_io::scene_to_json(&engine, &doc).expect("导出必须成功");
    for needle in [
        "\"lighting\"",
        // 注意：`preset` 会被**故意**丢掉 —— 编辑器现在自己负责太阳/环境光/曝光这几个字段，
        // 留着预设会在载入时把它们再覆盖一遍（"存了但不算数"）。
        "\"sun_intensity\": 3.5",
        "\"exposure\": 1.4",
        "\"simulate\"",
        "\"steps\": 240",
        "\"render\"",
        "\"msaa\": 8",
        "\"probes\"",
        "probe-a",
        "\"joints\"",
        "\"type\": \"hinge\"",
    ] {
        assert!(
            saved.json.contains(needle),
            "导出丢了「{needle}」——这正是那类静默损坏。导出内容：\n{}",
            saved.json
        );
    }
    // 贴图引用与 UV 参数：材质按同名物体继承，不许被"没有界面"当借口清掉。
    assert!(
        saved.json.contains("quench3d-editor-test-texture.png"),
        "贴图引用被丢了：\n{}",
        saved.json
    );
    assert!(saved.json.contains("\"uv_scale\": 3.0"), "UV 被丢了");
    assert!(
        saved.json.contains("\"normal_scale\": 1.5"),
        "法线强度被丢了"
    );
    // 物体本身还在。
    assert!(saved.json.contains("\"box\"") && saved.json.contains("\"peg\""));

    // 光照**编辑**必须能存下去（这是"能改光照"这件事的行为证据，不只是"不丢"）。
    engine.scene.lighting.exposure = 2.25;
    if let Some(sun) = engine.scene.lighting.sun.as_mut() {
        sun.intensity = 7.5;
    }
    let edited = scene_io::scene_to_json(&engine, &doc).expect("导出必须成功");
    assert!(
        edited.json.contains("\"exposure\": 2.25"),
        "改了曝光却没写进文件：\n{}",
        edited.json
    );
    assert!(
        edited.json.contains("\"sun_intensity\": 7.5"),
        "改了太阳强度却没写进文件：\n{}",
        edited.json
    );

    // 存储必须是固定点：存 → 读 → 再存，字节相同。
    let Some(mut engine2) = headless_engine() else {
        return;
    };
    let mut doc2 = Doc::new();
    scene_io::load_text_into_doc(&mut engine2, &mut doc2, &edited.json).expect("二次载入必须成功");
    let saved2 = scene_io::scene_to_json(&engine2, &doc2).expect("二次导出必须成功");
    assert_eq!(
        edited.json, saved2.json,
        "「存一次读一次再存一次」应当字节不变，否则每次保存都会漂移"
    );
}

/// 编辑器删掉物体后，引用它的关节/动作不能让文件变成"自己都读不回来"。
#[test]
fn export_drops_joints_that_reference_deleted_objects() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    let source = r#"{
      "format": "quench3d.scene",
      "version": 1,
      "name": "dangling",
      "objects": [
        { "name": "box", "shape": { "type": "box", "size": [1, 1, 1] },
          "position": [0, 2, 0], "body": "dynamic" },
        { "name": "peg", "shape": { "type": "sphere", "radius": 0.25 },
          "position": [0, 4, 0], "body": "static" }
      ],
      "joints": [ { "type": "hinge", "a": "box", "b": "peg", "axis": [0, 1, 0] } ],
      "actions": [ { "type": "impulse", "target": "box", "at_step": 10, "value": [0, 5, 0] } ]
    }"#;
    scene_io::load_text_into_doc(&mut engine, &mut doc, source).expect("载入必须成功");

    // 删掉 peg：引用它的铰链必须一起消失，否则存出来的文件读不回来。
    let peg = engine
        .scene
        .objects
        .iter()
        .position(|o| o.name == "peg")
        .expect("应当有 peg");
    doc.selected = Some(peg);
    doc.delete_selected(&mut engine);

    let saved = scene_io::scene_to_json(&engine, &doc).expect("导出必须成功");
    assert!(
        !saved.json.contains("\"type\": \"hinge\""),
        "引用了已删除物体的关节必须被清掉：\n{}",
        saved.json
    );
    assert!(
        saved.skipped.iter().any(|s| s.contains("关节")),
        "被清掉的关节要如实上报，实际：{:?}",
        saved.skipped
    );
    // 二次装载必须还能读（这是"不能让文件坏掉"的最终判据）。
    let Some(mut engine2) = headless_engine() else {
        return;
    };
    let mut doc2 = Doc::new();
    scene_io::load_text_into_doc(&mut engine2, &mut doc2, &saved.json)
        .expect("清掉悬空引用之后，导出结果必须还能读回来");
}

/// 编辑器里显式设置的材质贴图/开关，必须能写进场景文件（并遵守"同名物体"的键）。
///
/// 这是"材质贴图编辑"这条链**数据侧**的钉子：界面还没有，但底下的存储、导出、
/// 以及与"继承原文档"的优先级关系必须先是确定的 —— 否则界面一接上就会出错。
#[test]
fn explicit_material_textures_are_exported() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    let source = r#"{
      "format": "quench3d.scene",
      "version": 1,
      "name": "tex",
      "objects": [
        { "name": "box", "shape": { "type": "box", "size": [1, 1, 1] },
          "position": [0, 2, 0], "body": "dynamic" },
        { "name": "ball", "shape": { "type": "sphere", "radius": 0.5 },
          "position": [2, 2, 0], "body": "dynamic" }
      ]
    }"#;
    scene_io::load_text_into_doc(&mut engine, &mut doc, source).expect("载入必须成功");

    // 造一张真贴图：校验器要求被引用的贴图**确实存在**（这正是它该做的事）。
    let texture_path = std::env::temp_dir().join("quench3d-tex-explicit.png");
    let texture =
        quench3d::render::ImageData::checker(8, 2, [255, 255, 255, 255], [40, 40, 40, 255]);
    quench3d::write_png(&texture, &texture_path).expect("造测试贴图必须成功");
    let texture_json = texture_path.to_string_lossy().replace('\\', "\\\\");

    // box 显式指定：基础色贴图 + UV + 双面 + 无光照；ball 什么都不设。
    let paths = quench3d_editor::doc::TexturePaths {
        base_color: Some(texture_path.display().to_string()),
        uv_scale: Some(2.5),
        double_sided: Some(true),
        unlit: Some(false),
        ..Default::default()
    };
    doc.textures.insert("box".to_string(), paths);

    let saved = scene_io::scene_to_json(&engine, &doc).expect("导出必须成功");
    for needle in [
        format!("\"base_color_texture\": \"{texture_json}\"",),
        "\"uv_scale\": 2.5".to_string(),
        "\"double_sided\": true".to_string(),
    ] {
        assert!(
            saved.json.contains(&needle),
            "显式设置的材质项没写进文件：「{needle}」\n{}",
            saved.json
        );
    }

    // 再读一次：显式设置必须真的生效（按物体名匹配，而不是靠索引）。
    let mut engine2 = headless_engine().expect("需要 GPU 才能复核材质");
    let mut doc2 = Doc::new();
    scene_io::load_text_into_doc(&mut engine2, &mut doc2, &saved.json).expect("二次载入必须成功");
    let box_index = engine2
        .scene
        .objects
        .iter()
        .position(|o| o.name == "box")
        .expect("应当有 box");
    let ball_index = engine2
        .scene
        .objects
        .iter()
        .position(|o| o.name == "ball")
        .expect("应当有 ball");
    let box_material = engine2
        .renderer
        .material(engine2.scene.objects[box_index].material)
        .expect("box 应当有材质");
    assert_eq!(box_material.uv_scale, 2.5, "UV 平铺没有生效");
    assert!(box_material.double_sided, "双面没有生效");
    let ball_material = engine2
        .renderer
        .material(engine2.scene.objects[ball_index].material)
        .expect("ball 应当有材质");
    assert_eq!(ball_material.uv_scale, 1.0, "ball 不该被 box 的设置串到");
    assert!(!ball_material.double_sided, "ball 不该被 box 的设置串到");
}

/// 编辑器"材质（缩放与开关）"面板走的就是这条调用：`update_material` 改引擎里的材质描述。
///
/// 面板本身是 UI 代码，没法在无头测试里点；但把它真正依赖的那一步（写进引擎、
/// 再读回来确实变了）钉住，面板里"视口立刻变"这句话才有依据。
#[test]
fn update_material_changes_the_engine_material() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    let source = r#"{
      "format": "quench3d.scene", "version": 1, "name": "mat",
      "objects": [ { "name": "box", "shape": { "type": "box", "size": [1, 1, 1] },
                     "position": [0, 2, 0], "body": "dynamic" } ]
    }"#;
    scene_io::load_text_into_doc(&mut engine, &mut doc, source).expect("载入必须成功");
    let material_id = engine.scene.objects[0].material;
    let mut desc = engine
        .renderer
        .material(material_id)
        .cloned()
        .expect("应当有材质");
    assert_eq!(desc.uv_scale, 1.0, "初始 UV 平铺应当是 1");
    desc.uv_scale = 3.5;
    desc.unlit = true;
    desc.double_sided = true;
    engine
        .renderer
        .update_material(&engine.gpu, material_id, desc);
    let after = engine
        .renderer
        .material(material_id)
        .cloned()
        .expect("应当有材质");
    assert_eq!(after.uv_scale, 3.5, "UV 平铺没写进引擎");
    assert!(after.unlit, "无光照没写进引擎");
    assert!(after.double_sided, "双面没写进引擎");
}

/// 运行时换贴图：贴图句柄烘在材质绑定组里，所以必须重建绑定组才真的换得掉。
///
/// 这是"界面里选一张贴图文件"所依赖的最后一块地基；顺带钉住"贴图 id 无效必须被拒绝"，
/// 避免以后静默用错图或直接 panic。
#[test]
fn set_material_texture_swaps_and_rejects_bad_ids() {
    use quench3d::render::MaterialTextureSlot;
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    let source = r#"{
      "format": "quench3d.scene", "version": 1, "name": "texapi",
      "objects": [ { "name": "box", "shape": { "type": "box", "size": [1, 1, 1] },
                     "position": [0, 2, 0], "body": "dynamic" } ]
    }"#;
    scene_io::load_text_into_doc(&mut engine, &mut doc, source).expect("载入必须成功");
    let material_id = engine.scene.objects[0].material;
    assert!(
        engine
            .renderer
            .material(material_id)
            .expect("应当有材质")
            .base_color_texture
            .is_none(),
        "初始不该有基础色贴图"
    );

    let image = quench3d::render::ImageData::checker(4, 4, [255, 0, 0, 255], [0, 255, 0, 255]);
    let texture = engine.renderer.add_texture(&engine.gpu, &image, true);
    assert!(
        engine.renderer.set_material_texture(
            &engine.gpu,
            material_id,
            MaterialTextureSlot::BaseColor,
            Some(texture)
        ),
        "换贴图应当成功"
    );
    assert_eq!(
        engine
            .renderer
            .material(material_id)
            .expect("应当有材质")
            .base_color_texture,
        Some(texture),
        "贴图句柄没写进材质"
    );

    // 无效贴图 id：必须被拒绝，且不能改动原来的材质。
    assert!(
        !engine.renderer.set_material_texture(
            &engine.gpu,
            material_id,
            MaterialTextureSlot::Normal,
            Some(9999)
        ),
        "无效贴图 id 应当被拒绝"
    );
    assert!(
        engine
            .renderer
            .material(material_id)
            .expect("应当有材质")
            .normal_texture
            .is_none(),
        "被拒绝的调用不该改动材质"
    );

    // `None` = 还原成内置默认贴图。
    assert!(engine.renderer.set_material_texture(
        &engine.gpu,
        material_id,
        MaterialTextureSlot::BaseColor,
        None
    ));
    assert!(
        engine
            .renderer
            .material(material_id)
            .expect("应当有材质")
            .base_color_texture
            .is_none(),
        "还原默认贴图失败"
    );
}
