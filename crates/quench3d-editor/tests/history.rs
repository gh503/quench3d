//! 撤销 / 重做的行为验证。
//!
//! 这里钉住的是**用户能感觉到的语义**，不是实现细节：
//! * 编辑之后能撤销回去、能重做回来（数量、名字、位置逐项对账）；
//! * 一次连续编辑（拖拽）算**一步**，不是每帧一步；
//! * 撤销**不动视角**（轨道相机的焦点/距离/朝向保持）；
//! * 撤销后的场景与"当时存的快照"**逐字节相同**（不然每次都变脏）；
//! * 超出上限时丢最旧的（内存有界），新编辑清空重做链。
//!
//! **显式 opt-in**：要真的创建 wgpu 上下文，所以只有设了 `QUENCH3D_GPU_TESTS=1` 才执行
//! （见 `with_gpu_tests`），否则整条测试体不跑并打印跳过原因 —— 在没有 GPU 的 Windows
//! runner 上建上下文是**原生崩溃**（`0xc0000005`），`Result::Err` 接不住。只编译原生目标。
#![cfg(not(target_arch = "wasm32"))]

use quench3d::prelude::*;
use quench3d::render::context::with_gpu_tests;
use quench3d_editor::doc::{Doc, Primitive};
use quench3d_editor::scene_io;

/// 无窗口引擎；没显式打开开关、或本机确实没有 GPU 时返回 `None`。
///
/// **创建上下文那一步在闭包里面**：没设 `QUENCH3D_GPU_TESTS=1` 时它一次都不会被调用。
fn headless_engine() -> Option<Engine> {
    let gpu = with_gpu_tests(
        "撤销 / 重做的行为验证（真的创建 wgpu 上下文）",
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

fn scene_names(engine: &Engine) -> Vec<String> {
    engine
        .scene
        .objects
        .iter()
        .map(|o| o.name.clone())
        .collect()
}

fn snapshot(engine: &Engine, doc: &Doc) -> String {
    scene_io::scene_to_json(engine, doc)
        .expect("导出必须成功")
        .json
}

#[test]
fn add_and_delete_can_be_undone_and_redone() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    doc.reset_history(&engine);

    let base_names = scene_names(&engine);
    assert!(base_names.len() >= 5);
    assert!(!doc.history.can_undo(), "刚建的场景不该有可撤销的步骤");
    assert!(!doc.history.can_redo());

    // ---- 添加 → 撤销 → 重做 ----
    doc.spawn(&mut engine, Primitive::Cube);
    doc.record_edit(&engine);
    let added_names = scene_names(&engine);
    assert_eq!(added_names.len(), base_names.len() + 1);
    assert!(doc.history.can_undo());

    doc.undo(&mut engine);
    assert_eq!(scene_names(&engine), base_names, "撤销添加后应当回到原样");

    doc.redo(&mut engine);
    assert_eq!(
        scene_names(&engine),
        added_names,
        "重做后应当回到加过的状态"
    );

    // ---- 删除 → 撤销：物体要回来 ----
    let index = engine.scene.len() - 1;
    doc.selected = Some(index);
    doc.delete_selected(&mut engine);
    doc.record_edit(&engine);
    assert_eq!(scene_names(&engine).len(), base_names.len());
    doc.undo(&mut engine);
    assert_eq!(
        scene_names(&engine),
        added_names,
        "撤销删除必须把物体还回来"
    );
    assert!(
        doc.selected.is_none() || doc.selected.unwrap() < engine.scene.len(),
        "撤销后选中下标不能越界"
    );
}

#[test]
fn transform_edit_is_restored_exactly() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    doc.reset_history(&engine);
    doc.selected = Some(1);

    let before = engine.scene.objects[1].transform;
    engine.scene.objects[1].transform.position = Vec3::new(3.5, 2.25, -1.75);
    doc.push_transform_to_physics(&mut engine);
    doc.record_edit(&engine);

    doc.undo(&mut engine);
    let after = engine.scene.objects[1].transform;
    assert!(
        (after.position - before.position).length() < 1e-6,
        "撤销后位置应当逐位回到 {:?}，实际 {:?}",
        before.position,
        after.position
    );

    doc.redo(&mut engine);
    let redone = engine.scene.objects[1].transform.position;
    assert!(
        (redone - Vec3::new(3.5, 2.25, -1.75)).length() < 1e-6,
        "重做后应当回到改过的位置，实际 {redone:?}"
    );
}

/// 一次连续编辑（拖拽）必须只留一步历史：中途几十次 `record_edit` 都被折叠。
#[test]
fn transaction_coalesces_a_drag_into_one_step() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    doc.reset_history(&engine);
    doc.selected = Some(1);

    let before = engine.scene.objects[1].transform.position;
    let steps_before = doc.history.len();

    doc.begin_history_transaction();
    for i in 0..40 {
        engine.scene.objects[1].transform.position = before + Vec3::new(i as f32 * 0.1, 0.0, 0.0);
        doc.push_transform_to_physics(&mut engine);
        doc.record_edit(&engine); // 拖拽中的每一帧
    }
    doc.end_history_transaction(&engine);
    assert_eq!(
        doc.history.len(),
        steps_before + 1,
        "40 帧拖拽应当只产生 1 步历史"
    );

    // 撤销一次就回到拖拽前，而不是回到第 39 帧。
    doc.undo(&mut engine);
    let after = engine.scene.objects[1].transform.position;
    assert!(
        (after - before).length() < 1e-6,
        "一次撤销就该回到拖拽前 {before:?}，实际 {after:?}"
    );
}

/// 撤销不改变视角：轨道相机的焦点/距离/朝向都要保持。
#[test]
fn undo_keeps_the_camera_where_the_user_left_it() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    doc.reset_history(&engine);

    doc.spawn(&mut engine, Primitive::Sphere);
    doc.record_edit(&engine);

    engine.orbit.focus = Vec3::new(7.0, 3.0, -2.0);
    engine.orbit.distance = 42.0;
    engine.orbit.yaw = 1.25;
    engine.orbit.pitch = 0.5;
    let kept = engine.orbit;

    doc.undo(&mut engine);
    assert!(
        (engine.orbit.focus - kept.focus).length() < 1e-6
            && (engine.orbit.distance - kept.distance).abs() < 1e-6
            && (engine.orbit.yaw - kept.yaw).abs() < 1e-6
            && (engine.orbit.pitch - kept.pitch).abs() < 1e-6,
        "撤销不该动视角：期望 {:?}，实际 {:?}",
        kept.focus,
        engine.orbit.focus
    );
}

/// 撤销回去的场景必须与"当时存的快照"逐字节相同，否则每次撤销都像"又改了一次"。
#[test]
fn undo_restores_the_exact_serialized_scene() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    doc.reset_history(&engine);

    let snapshot_a = snapshot(&engine, &doc);
    doc.spawn(&mut engine, Primitive::Cylinder);
    doc.record_edit(&engine);
    let snapshot_b = snapshot(&engine, &doc);
    assert_ne!(snapshot_a, snapshot_b);

    doc.undo(&mut engine);
    assert_eq!(
        snapshot(&engine, &doc),
        snapshot_a,
        "撤销后应当与快照 A 逐字节相同"
    );
    doc.redo(&mut engine);
    assert_eq!(
        snapshot(&engine, &doc),
        snapshot_b,
        "重做后应当与快照 B 逐字节相同"
    );
}

/// 默认场景"存盘 → 重新载入"必须**逐字节恒等**。
///
/// 这条钉的是第 53 轮发现的不对称：编辑器默认场景以前直接吃 `Lighting::default()`，
/// 而载入路径（撤销/重做、打开文件都走它）用 CLI 的 `apply_lighting`（没写 `preset`
/// = `Lighting::studio()`，带两盏补光）。两条路径的默认值不同 —— 存盘再载入会凭空
/// 多出那两盏点光源，撤销也因此在快照上显出差异。
///
/// 为什么以前看不见：`scene_to_json` 对局部光"沿用底稿"，把这个不对称藏了起来。
/// 只有"逐字节往返"这种断言能抓住它。
#[test]
fn default_scene_survives_save_and_reload_byte_for_byte() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    let first = snapshot(&engine, &doc);
    // 存盘 → 载入：撤销/重做与"打开文件"走的就是这条路径。
    scene_io::load_into_engine(&mut engine, &first).expect("载入自己存出来的场景必须成功");
    let second = snapshot(&engine, &doc);
    assert_eq!(first, second, "默认场景存盘再载入应当逐字节相同");
    // 再转一圈：恒等要稳定，不能只是"第一次凑巧"。
    scene_io::load_into_engine(&mut engine, &second).expect("第二次载入也必须成功");
    assert_eq!(snapshot(&engine, &doc), second, "第二圈也必须逐字节相同");
}

/// 历史上限：超出后丢最旧的，且新编辑清空重做链。
#[test]
fn history_is_bounded_and_new_edits_drop_the_redo_chain() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    doc.history = quench3d_editor::history::History::new(4);
    doc.reset_history(&engine);

    for _ in 0..6 {
        doc.spawn(&mut engine, Primitive::Cube);
        doc.record_edit(&engine);
    }
    assert!(
        doc.history.len() <= 4,
        "历史上限是 4，实际 {}",
        doc.history.len()
    );

    doc.undo(&mut engine);
    assert!(doc.history.can_redo(), "撤销之后应当能重做");
    doc.spawn(&mut engine, Primitive::Sphere);
    doc.record_edit(&engine);
    assert!(!doc.history.can_redo(), "新编辑必须清空重做链");
}
/// 多选删除：一次删掉所有选中物体，下标的后续物体不受伤。
#[test]
fn delete_selected_removes_every_selected_object() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    let a = doc.spawn(&mut engine, Primitive::Cube);
    let b = doc.spawn(&mut engine, Primitive::Sphere);
    let c = doc.spawn(&mut engine, Primitive::Cylinder);
    let keep = c + 1;
    let a_name = engine.scene.objects[a].name.clone();
    let b_name = engine.scene.objects[b].name.clone();

    doc.select_only(Some(a));
    doc.toggle_selected(b);
    doc.delete_selected(&mut engine);

    assert!(
        !engine
            .scene
            .objects
            .iter()
            .any(|o| o.name == a_name || o.name == b_name),
        "多选删除必须删掉全部选中物体：{:?}",
        scene_names(&engine)
    );
    assert_eq!(engine.scene.objects.len(), keep - 2, "只该删两个");
    assert_eq!(doc.selection().len(), 1, "删完保留一个主选中");
}

/// 多选复制：每个选中物体各复制一份，复制体成为新的多选。
#[test]
fn duplicate_selected_copies_every_member() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    let a = doc.spawn(&mut engine, Primitive::Cube);
    let b = doc.spawn(&mut engine, Primitive::Sphere);
    let before = engine.scene.objects.len();
    doc.select_only(Some(a));
    doc.toggle_selected(b);
    let center = doc.selection_center(&engine).expect("多选必须有中心");
    let expected =
        (engine.scene.world_transform(a).position + engine.scene.world_transform(b).position) * 0.5;
    assert!(
        (center - expected).length() < 1e-5,
        "组轴心应当是选中物体的中心"
    );
    assert_eq!(
        doc.group_selection(&engine).len(),
        1,
        "组变换成员应排除主选中"
    );
    doc.duplicate_selected(&mut engine);
    assert_eq!(engine.scene.objects.len(), before + 2, "应当复制两份");
    assert_eq!(doc.selection().len(), 2, "复制体应当继承多选");
    assert_ne!(doc.selected, Some(a), "主选中应当指向复制体");
    assert_ne!(doc.selected, Some(b), "主选中应当指向复制体");
}

/// 粒子段由编辑器原样保存，并在重新载入时真的建出发射器（不是死字段）。
#[test]
fn particles_survive_editor_roundtrip_and_reload() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    let source = serde_json::json!({
        "particles": [{
            "count": 3,
            "position": [1, 2, 3],
            "lifetime": 5,
            "size": 0.5,
            "color": [1, 0.2, 0.1, 1]
        }]
    })
    .to_string();
    scene_io::load_text_into_doc(&mut engine, &mut doc, &source).expect("先载入带粒子的场景");
    let saved = scene_io::scene_to_json(&engine, &doc).expect("导出");
    assert!(
        saved.json.contains("\"particles\""),
        "编辑器存盘不能丢掉粒子段：{}",
        saved.json
    );
    scene_io::load_into_engine(&mut engine, &saved.json).expect("载入自己存出来的场景");
    assert_eq!(engine.scene.particles.len(), 1, "载入后必须有发射器");
    assert_eq!(engine.scene.particles[0].objects.len(), 3);
    for &index in &engine.scene.particles[0].objects {
        assert!(engine.scene.is_particle_object(index));
    }
}

/// 框选只按屏幕中心命中；Shift/Ctrl 追加、空框清空。
#[test]
fn box_select_uses_screen_centers_and_adds_or_clears() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    engine.scene.clear();
    engine.world.clear();
    let mesh = engine.assets.mesh(
        &mut engine.renderer,
        &engine.gpu,
        "框选测试盒",
        &MeshData::box_sized(Vec3::ONE),
    );
    let mat = engine.assets.solid_material(
        &mut engine.renderer,
        &engine.gpu,
        Vec4::new(0.8, 0.3, 0.2, 1.0),
        0.0,
        0.8,
    );
    for (x, name) in [(-1.2, "左"), (1.2, "右")] {
        engine.scene.add_named(
            mesh,
            mat,
            Transform::new(Vec3::new(x, 1.0, 0.0), Quat::IDENTITY, Vec3::ONE),
            name,
        );
    }
    engine.orbit.focus = Vec3::new(0.0, 1.0, 0.0);
    engine.orbit.distance = 8.0;
    engine.orbit.yaw = 0.0;
    engine.orbit.pitch = 0.0;

    let mut doc = Doc::new();
    let vp = Vec2::splat(64.0);
    let left = engine
        .camera()
        .world_to_screen(Vec3::new(-1.2, 1.0, 0.0), vp)
        .expect("左物体在视口里");
    let right = engine
        .camera()
        .world_to_screen(Vec3::new(1.2, 1.0, 0.0), vp)
        .expect("右物体在视口里");
    assert!(
        left.x < right.x,
        "屏幕 X 应保持左右次序：{left:?} {right:?}"
    );
    let rect = |p: Vec2| (p - Vec2::splat(6.0), p + Vec2::splat(6.0));

    let (min, max) = rect(left);
    assert_eq!(doc.select_in_rect(&engine, vp, min, max, false), 1);
    assert_eq!(doc.selected_object(&engine).unwrap().name, "左");

    let (min, max) = rect(right);
    assert_eq!(doc.select_in_rect(&engine, vp, min, max, true), 1);
    assert_eq!(doc.selection().len(), 2, "Shift 框选应追加而不是替换");
    assert!(doc.is_selected(0) && doc.is_selected(1));

    let empty = Vec2::new(1.0, 1.0);
    assert_eq!(
        doc.select_in_rect(&engine, vp, empty, empty + Vec2::splat(2.0), false),
        0
    );
    assert!(doc.selection().is_empty(), "空框（非追加）要清空选择");
}

/// Ctrl+C / Ctrl+V：复制不改变场景，粘贴重建物体、记一步历史，且粘贴体是根物体。
#[test]
fn copy_and_paste_clipboard_rebuilds_objects_and_is_undoable() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    doc.build_default_scene(&mut engine);
    doc.reset_history(&engine);
    let before = engine.scene.len();
    let names = scene_names(&engine);
    doc.select_only(Some(1));
    doc.toggle_selected(2);
    assert_eq!(doc.selection().len(), 2);

    let copied = doc.copy_selected(&engine);
    assert_eq!(copied, 2);
    assert_eq!(engine.scene.len(), before, "复制不能改变场景");
    assert!(!doc.history.can_undo(), "复制不是编辑，不该进历史");

    let pasted = doc.paste_clipboard(&mut engine);
    assert_eq!(pasted, 2);
    assert_eq!(engine.scene.len(), before + 2);
    assert_eq!(doc.selection().len(), 2, "粘贴后应选中新物体");
    assert!(doc.history.can_undo(), "粘贴要能撤销");
    let pasted_names: Vec<_> = doc
        .selection()
        .iter()
        .map(|&i| engine.scene.objects[i].name.clone())
        .collect();
    for name in &pasted_names {
        assert!(!names.contains(name), "粘贴体不能和原名重复：{name}");
    }
    for &i in &doc.selection() {
        let object = &engine.scene.objects[i];
        assert!(object.parent.is_none(), "粘贴体应是根物体");
        assert!(object.body.is_some(), "有刚体的原物体，粘贴体也要有刚体");
    }

    doc.undo(&mut engine);
    assert_eq!(engine.scene.len(), before, "撤销粘贴要回到原数量");
    doc.redo(&mut engine);
    assert_eq!(engine.scene.len(), before + 2, "重做粘贴要回来");

    // 空选择复制只改提示，不清掉已有剪贴板。
    doc.select_only(None);
    assert_eq!(doc.copy_selected(&engine), 0);
    assert_eq!(doc.paste_clipboard(&mut engine), 2);
}
