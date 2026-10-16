//! 模型引用（glTF/OBJ）↔ 引擎物体的映射，以及"在视口里拖模型"这条链。
//!
//! 为什么值得单独测：导入的模型在引擎里被展开成"一个子网格一个物体"，
//! 编辑器如果不知道"哪几个物体是同一个模型"，就只能改文件里的 `models[i]` 等重开生效
//! （这是 §8.1 记下的卡点）。这里把映射本身、面板改变换的**当场生效**、
//! 拖动整组联动并写回、删除物体后的下标维护、撤销后的刷新各钉一条。
//!
//! 没有可用 GPU 时跳过（CI 机器通常没有软件光栅驱动）。
#![cfg(not(target_arch = "wasm32"))]

use quench3d::prelude::*;
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

/// 造一个**两网格**的 OBJ，以及一份引用它的场景（另有一个普通盒子，用来验证"不串物体"）。
///
/// 返回 `(场景 JSON, OBJ 路径)`；OBJ 落在临时目录，测试结束不清理（与其它往返测试一致）。
fn two_mesh_scene(tag: &str) -> (String, std::path::PathBuf) {
    let obj_path = std::env::temp_dir().join(format!("quench3d-model-{tag}.obj"));
    let obj = "o left\nv 0 0 0\nv 1 0 0\nv 0 1 0\nf 1 2 3\n\
               o right\nv 2 0 0\nv 3 0 0\nv 2 1 0\nf 4 5 6\n";
    std::fs::write(&obj_path, obj).expect("写测试 OBJ 必须成功");
    let path = obj_path.to_string_lossy().replace('\\', "\\\\");
    let json = format!(
        r#"{{
          "format": "quench3d.scene", "version": 1, "name": "model-map",
          "ground": {{ "size": 10 }},
          "objects": [
            {{ "name": "盒子", "shape": {{ "type": "box", "size": [1, 1, 1] }},
               "position": [0, 3, 0], "body": "dynamic" }}
          ],
          "models": [
            {{ "path": "{path}", "position": [0.0, 0.0, 0.0], "scale": 1.0, "collide": false }}
          ]
        }}"#
    );
    (json, obj_path)
}

/// 场景装载之后：`models[0]` 展开出了两个物体，映射指得准，普通物体不被卷进来。
#[test]
fn model_instance_maps_every_submesh() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let (json, _obj) = two_mesh_scene("map");
    let mut doc = Doc::new();
    scene_io::load_text_into_doc(&mut engine, &mut doc, &json).expect("载入必须成功");

    assert_eq!(doc.model_instances.len(), 1, "一条 models 引用 → 一个实例");
    let instance = &doc.model_instances[0];
    assert_eq!(instance.model, 0);
    assert_eq!(instance.objects.len(), 2, "两网格 OBJ 应当展开成两个物体");

    let objects = instance.objects.clone();
    for &object in &objects {
        assert!(object < engine.scene.objects.len(), "映射下标越界");
        assert_eq!(doc.model_of_object(object), Some(0), "物体没被认成模型成员");
    }
    assert_eq!(doc.model_objects(0).map(<[usize]>::len), Some(2));
    // 地面与盒子不是模型展开出来的。
    assert_eq!(doc.model_of_object(0), None);
    assert_eq!(doc.model_of_object(1), None);
}

/// 面板改模型变换 → **当场**作用到引擎里那一组物体（不再需要"重开后生效"）。
#[test]
fn model_panel_change_applies_to_engine_immediately() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let (json, _obj) = two_mesh_scene("panel");
    let mut doc = Doc::new();
    scene_io::load_text_into_doc(&mut engine, &mut doc, &json).expect("载入必须成功");
    let objects = doc.model_instances[0].objects.clone();

    let before = engine.scene.objects[objects[0]].transform.position;
    assert!(before.length() < 1e-6, "初始应当就在原点，实际 {before:?}");

    // 编辑器"模型"面板做的事：把值写进底稿，然后应用到引擎。
    {
        let base = doc.base_doc_mut().expect("应当有底稿");
        base.models[0].position = Some([3.0, 1.5, -2.0]);
        base.models[0].scale = Some(2.5);
    }
    let moved = doc.apply_model_transform(&mut engine, 0);
    assert_eq!(moved, objects.len(), "整组都该被挪动");

    for &object in &objects {
        let transform = engine.scene.objects[object].transform;
        assert!(
            (transform.position - Vec3::new(3.0, 1.5, -2.0)).length() < 1e-5,
            "位置没当场生效：{:?}",
            transform.position
        );
        assert!(
            (transform.scale - Vec3::splat(2.5)).length() < 1e-5,
            "缩放没当场生效：{:?}",
            transform.scale
        );
    }
    // 普通物体一动不动。
    assert!(
        (engine.scene.objects[1].transform.position - Vec3::new(0.0, 3.0, 0.0)).length() < 1e-6,
        "盒子不该被模型面板影响"
    );
}

/// 视口里拖模型的一个网格 = 整组联动 + 写回 `models[i]`，存盘重开后一致。
#[test]
fn dragging_a_model_mesh_moves_the_group_and_writes_back() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let (json, _obj) = two_mesh_scene("drag");
    let mut doc = Doc::new();
    scene_io::load_text_into_doc(&mut engine, &mut doc, &json).expect("载入必须成功");
    let objects = doc.model_instances[0].objects.clone();

    let target = Transform::new(
        Vec3::new(-4.0, 0.5, 1.0),
        Quat::from_rotation_y(0.5),
        Vec3::splat(1.75),
    );
    assert!(
        doc.apply_transform_to_model_group(&mut engine, objects[0], target),
        "模型成员应当走整组联动"
    );
    for &object in &objects {
        let transform = engine.scene.objects[object].transform;
        assert!(
            (transform.position - target.position).length() < 1e-5,
            "同组的另一个网格没跟上：{:?}",
            transform.position
        );
        assert!((transform.scale - Vec3::splat(1.75)).length() < 1e-5);
    }

    let model = doc.base_doc().expect("应当有底稿").models[0].clone();
    assert_eq!(model.scale, Some(1.75), "缩放没写回 models");
    let position = model.position.expect("位置没写回 models");
    assert!(
        (position[0] + 4.0).abs() < 1e-5 && (position[1] - 0.5).abs() < 1e-5,
        "位置写回得不对：{position:?}"
    );
    let yaw = model.rotation_deg.expect("旋转没写回 models")[1];
    assert!(
        (yaw - 0.5f32.to_degrees()).abs() < 1e-2,
        "旋转写回得不对：{yaw}°"
    );

    // 存盘 → 重开：位置与缩放必须还在（与 `model_transform_edits_are_exported` 互补）。
    let saved = scene_io::scene_to_json(&engine, &doc).expect("导出必须成功");
    let mut engine2 = headless_engine().expect("需要 GPU 才能复核");
    let mut doc2 = Doc::new();
    scene_io::load_text_into_doc(&mut engine2, &mut doc2, &saved.json).expect("二次载入必须成功");
    let reloaded = doc2.model_instances[0].objects.clone();
    assert_eq!(reloaded.len(), 2, "重开后映射应当重新建立");
    let transform = engine2.scene.objects[reloaded[0]].transform;
    assert!(
        (transform.position - target.position).length() < 1e-4,
        "重开后模型位置不一致：{:?}",
        transform.position
    );
    assert!((transform.scale - Vec3::splat(1.75)).length() < 1e-4);

    // 普通物体不走这条路：返回 false，也不会动 `models`。
    assert!(
        !doc.apply_transform_to_model_group(&mut engine, 1, target),
        "非模型物体不该被当成模型成员"
    );
}

/// 删掉模型组**之前**的物体：映射里剩下的下标必须整体前移，否则会拖到别人身上。
#[test]
fn deleting_an_object_keeps_the_mapping_pointing_at_the_model() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let (json, _obj) = two_mesh_scene("delete");
    let mut doc = Doc::new();
    scene_io::load_text_into_doc(&mut engine, &mut doc, &json).expect("载入必须成功");
    let before = doc.model_instances[0].objects.clone();

    // 刻意删掉下标 0（地面），它排在模型物体前面，后面所有下标都要 -1。
    doc.selected = Some(0);
    doc.delete_selected(&mut engine);

    let after = doc.model_instances[0].objects.clone();
    assert_eq!(
        after.len(),
        before.len(),
        "删的不是模型网格，映射长度不该变"
    );
    for (old, new) in before.iter().zip(after.iter()) {
        assert_eq!(
            *new + 1,
            *old,
            "映射下标应当整体前移一位（{before:?} → {after:?}）"
        );
    }
    for &object in &after {
        assert!(
            object < engine.scene.objects.len(),
            "前移之后越界：{after:?}"
        );
        assert_eq!(doc.model_of_object(object), Some(0));
    }
}

/// 撤销一步之后：模型变换回到快照里的值，映射也跟着重新建立（不是陈旧的）。
#[test]
fn undo_restores_the_model_transform_and_refreshes_the_mapping() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let (json, _obj) = two_mesh_scene("undo");
    let mut doc = Doc::new();
    scene_io::load_text_into_doc(&mut engine, &mut doc, &json).expect("载入必须成功");
    doc.capture_baseline(&engine);
    doc.reset_history(&engine);

    let objects = doc.model_instances[0].objects.clone();
    let target = Transform::new(Vec3::new(5.0, 0.0, 0.0), Quat::IDENTITY, Vec3::splat(1.0));
    assert!(doc.apply_transform_to_model_group(&mut engine, objects[0], target));
    doc.record_edit(&engine);
    assert!(doc.history.can_undo(), "拖过之后应当能撤销");

    doc.undo(&mut engine);

    let restored = doc.model_instances[0].objects.clone();
    assert_eq!(restored.len(), 2, "撤销后映射应当重新建立");
    for &object in &restored {
        assert_eq!(doc.model_of_object(object), Some(0));
        let position = engine.scene.objects[object].transform.position;
        assert!(
            position.length() < 1e-5,
            "撤销之后模型应当回到原点，实际 {position:?}"
        );
    }
    assert_eq!(
        doc.base_doc().expect("应当有底稿").models[0].position,
        Some([0.0, 0.0, 0.0]),
        "底稿里的模型位置没被撤销回去"
    );
}
