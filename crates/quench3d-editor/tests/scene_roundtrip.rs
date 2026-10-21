//! 编辑器存/读场景的往返验证（格式与 CLI 共用 `quench3d.scene`）。
//!
//! 为什么值得单独测：编辑器的核心承诺是「CLI 生成 → 编辑器手工改 → CLI 渲染视频」
//! 这条链能走通，而链上最容易悄悄断掉的一环就是**存一次再读一次之后场景变了**
//! （位置漂移、地面下沉、物体改名、丢物体）。这里把默认场景存成 JSON、
//! 用 CLI 的解析器与校验器读回来、再装回引擎，逐项对账。
//!
//! **显式 opt-in**：要真的创建 wgpu 上下文，所以只有设了 `QUENCH3D_GPU_TESTS=1` 才执行
//! （见 `with_gpu_tests`），否则整条测试体不跑并打印跳过原因 —— 在没有 GPU 的 Windows
//! runner 上建上下文是**原生崩溃**（`0xc0000005`），`Result::Err` 接不住。
//! 整个文件只编译原生目标：wasm 上没有文件系统与 `pollster`，这条链在浏览器里
//! 由 `web_client` 导出的 `scene_roundtrip_probe()` 覆盖（纯 CPU 那半条）。
#![cfg(not(target_arch = "wasm32"))]

use quench3d::prelude::*;
use quench3d::render::context::with_gpu_tests;
use quench3d_cli::scene_doc::{SceneDoc, ShapeKind};
use quench3d_editor::doc::Doc;
use quench3d_editor::scene_io;

/// 无窗口引擎；没显式打开开关、或本机确实没有 GPU 时返回 `None`。
///
/// **创建上下文那一步在闭包里面**：没设 `QUENCH3D_GPU_TESTS=1` 时它一次都不会被调用。
fn headless_engine() -> Option<Engine> {
    let gpu = with_gpu_tests(
        "编辑器场景存 / 读往返（真的创建 wgpu 上下文）",
        || {
            pollster::block_on(quench3d::render::GpuContext::headless(
                64,
                64,
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
    let shape = parsed.objects[0].primary_shape();
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
      "version": 2,
      "name": "preserve",
      "gravity": [0, -9.81, 0],
      "ground": { "size": 24, "friction": 0.7 },
      "physics": { "friction_combine": "min", "restitution_combine": "average",
                   "static_friction_speed": 0.03 },
      "lighting": { "preset": "studio", "sun_direction": [-0.4, -1, -0.35], "sun_intensity": 3.5, "ambient": 0.25, "exposure": 1.4, "shadows": true, "sky": true,
                    "fog": { "color": [0.18, 0.22, 0.3], "density": 0.04 },
                    "shadow_quality": { "strength": 0.65, "radius": 45 },
                    "sky_colors": { "zenith": [0.1, 0.2, 0.6] } },
      "simulate": { "steps": 240, "dt": 0.004 },
      "render": { "output": "out.png", "width": 800, "height": 600, "msaa": 8 },
      "probes": [ { "name": "probe-a", "position": [0, 1, 0], "radius": 2.5 } ],
      "materials": {
        "wood": { "color": [0.78, 0.48, 0.26], "roughness": 0.55, "uv_scale": 3.0, "base_color_texture": "__TEXTURE__", "normal_scale": 1.5, "double_sided": true }
      },
      "objects": [
        { "name": "box", "shape": { "type": "box", "size": [0.8, 0.8, 0.8] },
          "position": [0, 2.0, 0], "material": "wood", "body": "dynamic", "density": 400,
          "friction": 0.35, "static_friction": 0.9, "friction_combine": "max",
          "restitution_combine": "multiply" },
        { "name": "peg", "shape": { "type": "sphere", "radius": 0.25 },
          "position": [0, 4.0, 0], "body": "static" }
      ],
      "joints": [
        { "type": "hinge", "a": "box", "b": "peg", "axis": [0, 1, 0],
          "axis_a": [0, 1, 0], "axis_b": [0, 0, 1],
          "motor_target_deg": 30.0, "motor_stiffness": 10.0, "motor_damping": 2.0,
          "break_force": 500.0, "break_torque": 12.0 },
        { "type": "ball", "a": "box", "b": "peg",
          "cone_limit": { "half_angle_deg": 25.0, "axis_a": [0, 1, 0] },
          "collide_connected": true }
      ]
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
        // 雾 / 阴影质量 / 天空颜色：编辑器能改，导出必须带上（不然"改了存不下来"）
        "\"fog\"",
        "\"shadow_quality\"",
        "\"sky_colors\"",
        // 物理材质（静摩擦 + 合成规则）：编辑器能改，导出必须带上
        "\"static_friction\": 0.9",
        "\"friction_combine\": \"max\"",
        "\"restitution_combine\": \"multiply\"",
        "\"simulate\"",
        "\"steps\": 240",
        "\"render\"",
        "\"msaa\": 8",
        "\"probes\"",
        "probe-a",
        "\"joints\"",
        "\"type\": \"hinge\"",
        // 新增的关节字段：编辑器自己不做这些面板，但**必须原样留着**
        "\"motor_target_deg\": 30.0",
        "\"break_force\": 500.0",
        "\"cone_limit\"",
        "\"collide_connected\": true",
        // 局部轴按端拆分（第 56 轮）：编辑器没有关节面板，但**更不许丢**
        "\"axis_b\"",
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

    // 数值按类型核对（字符串匹配会随着格式化方式变化而假失败/假通过）：
    // 把导出结果交给 CLI 的解析器读回来，逐项对账。
    let reread = SceneDoc::from_json(&saved.json).expect("导出必须能被读回来");
    let fog = reread.lighting.fog.expect("雾被丢了");
    assert!((fog.density.unwrap() - 0.04).abs() < 1e-6, "{fog:?}");
    assert!((fog.color.unwrap()[2] - 0.3).abs() < 1e-6, "{fog:?}");
    let quality = reread.lighting.shadow_quality.expect("阴影质量被丢了");
    assert!(
        (quality.strength.unwrap() - 0.65).abs() < 1e-6,
        "{quality:?}"
    );
    assert!((quality.radius.unwrap() - 45.0).abs() < 1e-6, "{quality:?}");
    let sky = reread.lighting.sky_colors.expect("天空颜色被丢了");
    assert_eq!(sky.zenith, Some([0.1, 0.2, 0.6]), "{sky:?}");
    // 物理材质：世界缺省段要原样保留（编辑器不建模它，但绝不能丢）
    assert_eq!(
        reread.physics.friction_combine,
        Some(quench3d_cli::scene_doc::CombineRuleDoc::Min),
        "physics 段被丢了：{:?}",
        reread.physics
    );
    assert_eq!(reread.physics.static_friction_speed, Some(0.03));
    assert_eq!(
        reread.joints[0].axis_a,
        Some([0.0, 1.0, 0.0]),
        "axis_a 被丢了：{:?}",
        reread.joints
    );
    assert_eq!(
        reread.joints[0].axis_b,
        Some([0.0, 0.0, 1.0]),
        "按端给的 axis_b 被丢了（这正是「编辑器不做面板就顺手清掉」那类静默损坏）：{:?}",
        reread.joints
    );
    let box_obj = reread
        .objects
        .iter()
        .find(|o| o.name.as_deref() == Some("box"))
        .expect("box 被丢了");
    assert_eq!(box_obj.static_friction, Some(0.9), "{box_obj:?}");
    assert_eq!(
        box_obj.friction_combine,
        Some(quench3d_cli::scene_doc::CombineRuleDoc::Max),
        "{box_obj:?}"
    );
    assert_eq!(
        box_obj.restitution_combine,
        Some(quench3d_cli::scene_doc::CombineRuleDoc::Multiply),
        "{box_obj:?}"
    );

    // 物理材质**编辑**也要能存下去（编辑器 UI 里拖静摩擦、选合成规则走的就是这条路）
    let target = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "box")
        .and_then(|o| o.body)
        .expect("box 应当有刚体");
    if let Some(b) = engine.world.body_mut(target) {
        b.static_friction = 1.35;
        b.friction_combine = Some(quench3d::prelude::CombineRule::Multiply);
    }

    // 光照**编辑**必须能存下去（这是"能改光照"这件事的行为证据，不只是"不丢"）。
    engine.scene.lighting.exposure = 2.25;
    engine.scene.lighting.fog.density = 0.09;
    engine.scene.lighting.fog.color = vec3(0.5, 0.25, 0.125);
    engine.scene.lighting.shadow_strength = 0.4;
    engine.scene.lighting.sky.horizon = vec3(0.9, 0.8, 0.7);
    if let Some(sun) = engine.scene.lighting.sun.as_mut() {
        sun.intensity = 7.5;
        sun.shadow_radius = 60.0;
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
    let edited_reread = SceneDoc::from_json(&edited.json).expect("导出必须能被读回来");
    let edit_fog = edited_reread.lighting.fog.expect("雾被丢了");
    assert!(
        (edit_fog.density.unwrap() - 0.09).abs() < 1e-6,
        "{edit_fog:?}"
    );
    assert!(
        (edit_fog.color.unwrap()[0] - 0.5).abs() < 1e-6,
        "{edit_fog:?}"
    );
    let edit_quality = edited_reread
        .lighting
        .shadow_quality
        .expect("阴影质量被丢了");
    assert!(
        (edit_quality.strength.unwrap() - 0.4).abs() < 1e-6,
        "{edit_quality:?}"
    );
    assert!(
        (edit_quality.radius.unwrap() - 60.0).abs() < 1e-6,
        "{edit_quality:?}"
    );
    let edit_sky = edited_reread.lighting.sky_colors.expect("天空颜色被丢了");
    assert_eq!(edit_sky.horizon, Some([0.9, 0.8, 0.7]), "{edit_sky:?}");
    let edit_box = edited_reread
        .objects
        .iter()
        .find(|o| o.name.as_deref() == Some("box"))
        .expect("box 被丢了");
    assert!(
        (edit_box.static_friction.unwrap() - 1.35).abs() < 1e-6,
        "改了静摩擦却没写进文件：{edit_box:?}"
    );
    assert_eq!(
        edit_box.friction_combine,
        Some(quench3d_cli::scene_doc::CombineRuleDoc::Multiply),
        "改了合成规则却没写进文件：{edit_box:?}"
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
      "version": 2,
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
      "version": 2,
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
      "format": "quench3d.scene", "version": 2, "name": "mat",
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
      "format": "quench3d.scene", "version": 2, "name": "texapi",
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

/// 导入模型的**变换**要能写回场景文件（编辑器的"模型"面板改的就是这一处）。
///
/// 编辑器拿不到"哪个物体属于哪个模型"的映射，所以面板改的是底稿里的 `models[i]`；
/// 导出时 `models` 段本来就是从底稿带出来的，于是"改位置 → 存盘 → 重开生效"这条链成立。
#[test]
fn model_transform_edits_are_exported() {
    let obj_path = std::env::temp_dir().join("quench3d-model-panel.obj");
    std::fs::write(&obj_path, "v 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n").expect("写测试 OBJ 必须成功");
    let obj_json = obj_path.to_string_lossy().replace('\\', "\\\\");
    let source = format!(
        r#"{{
          "format": "quench3d.scene", "version": 2, "name": "model-panel",
          "models": [
            {{ "path": "{obj_json}", "position": [0.0, 0.0, 0.0], "scale": 1.0, "collide": false,
               "animation": {{ "name": "摆动", "start": 0.5, "speed": 2.0, "loop": false }} }}
          ],
          "objects": []
        }}"#
    );
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    scene_io::load_text_into_doc(&mut engine, &mut doc, &source).expect("载入必须成功");

    // 没动过：导出里必须是原来的值（不能凭空改数据）。
    let before = scene_io::scene_to_json(&engine, &doc).expect("导出必须成功");
    assert!(
        before.json.contains("quench3d-model-panel.obj"),
        "{}",
        before.json
    );
    // 只查"模型"这一节，别用整篇 JSON 的子串匹配：整篇匹配会撞上别处的合法值
    // （第 53 轮实测：预设补光的位置 `[5, 4, -3.0]` 撞上了 `contains("3.0")`）。
    // 这里直接解析成场景类型再对字段断言，比子串匹配更严也更准。
    let before_parsed = quench3d_cli::scene_doc::SceneDoc::from_json(&before.json)
        .expect("导出的必须是合法场景 JSON");
    assert_eq!(
        before_parsed.models[0].position,
        Some([0.0, 0.0, 0.0]),
        "没动过的模型位置必须原样导出"
    );
    assert_eq!(
        before_parsed.models[0].scale,
        Some(1.0),
        "没动过的模型缩放必须原样导出"
    );
    // 模型动画是**编辑器不改也不该丢**的字段（录制/渲染时由 CLI 消费）：
    // 存一次再读一次必须逐项还在，否则"在编辑器里打开过"就等于悄悄关掉了动画。
    let anim = before_parsed.models[0]
        .animation
        .as_ref()
        .expect("模型动画字段不许在导出时丢掉");
    assert_eq!(anim.name, "摆动");
    assert_eq!(anim.start, Some(0.5));
    assert_eq!(anim.speed, Some(2.0));
    assert_eq!(anim.looping, Some(false));

    // 编辑器"模型"面板做的事：直接改底稿里的模型变换。
    let base = doc.base_doc_mut().expect("应当有底稿");
    base.models[0].position = Some([3.0, 1.5, -2.0]);
    base.models[0].scale = Some(2.5);
    let after = scene_io::scene_to_json(&engine, &doc).expect("导出必须成功");
    // 同样用类型解析来断言"写进去了"：子串匹配在这里是**不健全**的——
    // 预设补光里已有 `-3.0`，就算模型位置根本没写出去，`contains("3.0")` 也会通过。
    let after_parsed = quench3d_cli::scene_doc::SceneDoc::from_json(&after.json)
        .expect("导出的必须是合法场景 JSON");
    assert_eq!(
        after_parsed.models[0].position,
        Some([3.0, 1.5, -2.0]),
        "模型位置没写进场景文件"
    );
    assert_eq!(
        after_parsed.models[0].scale,
        Some(2.5),
        "模型缩放没写进场景文件"
    );

    // 存了再读回来：值必须还在（否则"重开生效"是假的）。
    let mut engine2 = headless_engine().expect("需要 GPU 才能复核");
    let mut doc2 = Doc::new();
    scene_io::load_text_into_doc(&mut engine2, &mut doc2, &after.json).expect("二次载入必须成功");
    let base2 = doc2.base_doc().expect("应当有底稿");
    assert_eq!(
        base2.models[0].position,
        Some([3.0, 1.5, -2.0]),
        "位置没读回来"
    );
    assert_eq!(base2.models[0].scale, Some(2.5), "缩放没读回来");
    assert_eq!(
        base2.models[0].animation.as_ref().map(|a| a.name.as_str()),
        Some("摆动"),
        "动画字段没读回来（编辑器存盘把动画丢了）"
    );
}

/// 复合形状：导出要写 `shapes`，而"部件绘制物体"不许被当成独立物体写出去。
///
/// 这条测试盯的是一个很容易发生、却很难发现的损坏：复合形状在渲染上要拆成多个绘制
/// 物体，如果导出时把它们也写进 `objects`，场景每"存一次"就多出几个物体。
#[test]
fn compound_bodies_export_as_shapes_without_duplicating_parts() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);

    // 顶板 + 一条腿：一个刚体两个部件
    let body = engine.world.add_builder(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::new(0.6, 0.05, 0.4),
        })
        .position(Vec3::new(1.5, 2.0, 0.0))
        .part_at(
            Shape::Box {
                half_extents: Vec3::new(0.05, 0.35, 0.05),
            },
            Vec3::new(0.5, -0.4, 0.0),
        ),
    );
    let mesh = engine.assets.unit_cube;
    let material = engine.scene.objects[1].material;
    let index = engine
        .scene
        .add_with_body(mesh, material, body, &engine.world, "复合桌子");
    // 手工放一个"附加部件伪物体"（CLI 装载场景时就是这么建的）
    let child = engine
        .scene
        .add_named(mesh, material, Transform::IDENTITY, "复合桌子#1");
    engine.scene.objects[child].parent = Some(index);
    engine.scene.objects[child].owner_part = Some(index);

    let saved = scene_io::scene_to_json(&engine, &doc).expect("导出必须成功");
    assert!(
        saved.json.contains("\"shapes\""),
        "复合形状要写成 shapes：\n{}",
        saved.json
    );
    assert_eq!(
        saved.json.matches("\"复合桌子\"").count(),
        1,
        "部件伪物体不该被导出成独立物体：\n{}",
        saved.json
    );

    // 用 CLI 的解析器 + 校验器读回来：必须仍然是 2 个部件、1 个物体
    let parsed = SceneDoc::from_json(&saved.json).expect("解析必须成功");
    let diagnostics = parsed.validate();
    assert!(
        !SceneDoc::has_errors(&diagnostics),
        "编辑器存的复合形状必须过 CLI 校验：{diagnostics:?}"
    );
    let table = parsed
        .objects
        .iter()
        .find(|o| o.name.as_deref() == Some("复合桌子"))
        .expect("应当有复合桌子");
    assert_eq!(table.parts().len(), 2, "两个部件要原样写回");
    assert!(table.shape.is_none(), "复合形状不该同时写 shape");
    assert_eq!(
        table.parts()[1].position,
        Some([0.5, -0.4, 0.0]),
        "部件局部位姿要保住"
    );
    // 再装回引擎一次：物理侧也必须仍然是"一个刚体两个部件"
    let (count, _) = scene_io::load_into_engine(&mut engine, &saved.json).expect("导入必须成功");
    assert!(count >= 1);
    let index = engine.scene.find("复合桌子").expect("导入后该有复合桌子");
    let handle = engine.scene.objects[index].body.expect("该带刚体");
    let body = engine.world.body(handle).expect("刚体该在");
    assert_eq!(body.part_count(), 2, "读回来仍然是两个部件");
    assert!(body.extra_parts().len() == 1);
    // 部件伪物体在导入后**重新**由装载器生成（数量对得上），但文档里只有一个物体
    assert!(
        !parsed
            .objects
            .iter()
            .any(|o| o.name.as_deref().unwrap_or("").contains('#')),
        "文档里不该出现 `名字#序号` 这种伪物体"
    );
}

/// 模板 + 实例：编辑器能装载，但**存回时会摊平成具体物体**。
///
/// 这是有意的取舍：编辑器没有"模板/实例"的界面概念，引擎里那三个副本是三个真物体。
/// 摊平不改变行为（位置、数量、物理都对），只是文件变长——所以这里把"摊平"这件事
/// 钉成一条明确的测试，而不是让它作为"用户某天发现文件变长了"的惊喜。
#[test]
fn templates_and_instances_are_flattened_on_save() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);

    let text = r#"{
      "ground": { "size": 20 },
      "templates": { "块": { "shape": { "type": "box", "size": [1, 0.4, 1] }, "body": "dynamic" } },
      "objects": [
        { "name": "塔", "template": "块",
          "instances": [ { "position": [0, 0.2, 0] },
                         { "position": [0, 0.6, 0] },
                         { "position": [3, 0.2, 0] } ] } ]
    }"#;
    let (count, _) = scene_io::load_into_engine(&mut engine, text).expect("导入必须成功");
    // 三个副本 + 地面（`ground` 字段也会建一个可视平面）
    assert_eq!(count, 4, "三个副本该变成三个物体（外带地面）");
    for name in ["塔#1", "塔#2", "塔#3"] {
        let index = engine
            .scene
            .find(name)
            .unwrap_or_else(|| panic!("该有 {name}"));
        assert!(
            engine.scene.objects[index].body.is_some(),
            "{name} 该带刚体（模板里的 body: dynamic）"
        );
    }

    let saved = scene_io::scene_to_json(&engine, &doc).expect("导出必须成功");
    assert!(
        !saved.json.contains("\"instances\"") && !saved.json.contains("\"templates\""),
        "编辑器存回时摊平 instances（有意取舍，不是丢数据）：\n{}",
        saved.json
    );
    let parsed = SceneDoc::from_json(&saved.json).expect("解析必须成功");
    let names: Vec<String> = parsed
        .objects
        .iter()
        .filter_map(|o| o.name.clone())
        .collect();
    for name in ["塔#1", "塔#2", "塔#3"] {
        assert!(names.contains(&name.to_string()), "{names:?}");
    }
    assert!(
        !SceneDoc::has_errors(&parsed.validate()),
        "{:?}",
        parsed.validate()
    );
}

#[test]
fn waters_survive_editor_roundtrip_and_render_a_surface() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let source = r#"{
      "format": "quench3d.scene", "version": 2, "name": "water-editor",
      "ground": { "size": 20, "y": -2 },
      "waters": [{ "center": [1, -2], "size": [8, 6], "level": 0.4,
                   "density": 900, "drag": 2.5 }],
      "objects": [
        { "name": "浮球", "shape": { "type": "sphere", "radius": 0.5 },
          "position": [0, 2, 0], "body": "dynamic", "density": 400 }
      ]
    }"#;
    let mut doc = Doc::new();
    scene_io::load_text_into_doc(&mut engine, &mut doc, source).expect("载入必须成功");
    assert_eq!(engine.world.water_count(), 1, "物理世界要装上水域");
    assert!(
        engine
            .scene
            .objects
            .iter()
            .any(|o| o.name.starts_with("水域")),
        "水面要有可见物体，否则编辑器里看不到水"
    );

    let out = scene_io::scene_to_json(&engine, &doc).expect("导出必须成功");
    let parsed = SceneDoc::from_json(&out.json).expect("导出的必须是合法场景 JSON");
    assert_eq!(parsed.waters.len(), 1, "存一次不许把水域丢掉");
    let w = &parsed.waters[0];
    assert_eq!(w.center, Some([1.0, -2.0]));
    assert_eq!(w.size, [8.0, 6.0]);
    assert_eq!(w.level, 0.4);
    assert_eq!(w.density, 900.0);
    assert_eq!(w.drag, 2.5);
}

#[test]
fn fluids_survive_editor_roundtrip() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let source = r#"{
      "format": "quench3d.scene", "version": 2, "name": "fluid-editor",
      "ground": { "size": 20, "y": 0 },
      "fluids": [{ "center": [0, 1.2, 0], "size": [1.2, 1.2, 1.2],
                   "spacing": 0.1, "radius": 0.05, "rest_density": 900,
                   "viscosity": 0.08, "drag": 4, "iterations": 5,
                   "max_particles": 1200 }]
    }"#;
    let mut doc = Doc::new();
    scene_io::load_text_into_doc(&mut engine, &mut doc, source).expect("载入必须成功");
    assert_eq!(engine.world.fluid_count(), 1, "物理世界要装上粒子流体");
    assert!(engine.scene.len() > 100, "粒子应当已经进了可视场景");

    let out = scene_io::scene_to_json(&engine, &doc).expect("导出必须成功");
    let parsed = SceneDoc::from_json(&out.json).expect("导出的必须是合法场景 JSON");
    assert_eq!(parsed.fluids.len(), 1, "存一次不许把流体丢掉");
    assert_eq!(
        parsed.objects.len(),
        0,
        "粒子/水面是渲染代理，存回去不许变成几千个球体物体"
    );
    let f = &parsed.fluids[0];
    assert_eq!(f.center, Some([0.0, 1.2, 0.0]));
    assert_eq!(f.size, [1.2, 1.2, 1.2]);
    assert_eq!(f.spacing, 0.1);
    assert_eq!(f.radius, Some(0.05));
    assert_eq!(f.rest_density, 900.0);
    assert_eq!(f.viscosity, 0.08);
    assert_eq!(f.drag, 4.0);
    assert_eq!(f.iterations, 5);
    assert_eq!(f.max_particles, 1200);
}
