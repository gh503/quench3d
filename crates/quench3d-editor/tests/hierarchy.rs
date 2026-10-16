//! 场景层级（父子）从场景文件到引擎、到编辑器操作的整条链。
//!
//! 为什么值得单独测：层级把 `SceneObject::transform` 的含义从"世界"变成了"局部"，
//! 一旦哪个环节（装载、拖动、改父、删除、存盘）还把它当世界变换用，现象都是
//! "物体跑到别的地方去了" —— 而且只在有父物体的时候才出现，很容易漏。
//! 这里把"子物体跟着父物体走""改父不跳位""删父不跳位""存盘往返一致"各钉一条。
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

/// 一份带层级的场景：`底座`（静态）→ `立柱`（相对底座偏移并旋转）→ `顶球`。
///
/// 三层是刻意的：只测一层的话，"父链拼了几次""祖父有没有算进去"这类错都测不出来。
fn layered_scene() -> &'static str {
    r#"{
      "format": "quench3d.scene", "version": 2, "name": "hierarchy",
      "ground": { "size": 10 },
      "objects": [
        { "name": "底座", "shape": { "type": "box", "size": [2, 0.2, 2] },
          "position": [0, 0.1, 0], "body": "static" },
        { "name": "立柱", "parent": "底座", "shape": { "type": "box", "size": [0.3, 2, 0.3] },
          "position": [1, 1, 0], "rotation_deg": [0, 90, 0] },
        { "name": "顶球", "parent": "立柱", "shape": { "type": "sphere", "radius": 0.3 },
          "position": [0, 1, 0] },
        { "name": "旁边的球", "shape": { "type": "sphere", "radius": 0.3 },
          "position": [4, 0.3, 0] }
      ]
    }"#
}

fn index_of(engine: &Engine, name: &str) -> usize {
    engine
        .scene
        .find(name)
        .unwrap_or_else(|| panic!("场景里没有 {name}"))
}

/// 装载之后：父链被接上，子物体的世界位置 = 父链逐级拼出来的结果。
#[test]
fn install_builds_the_parent_chain() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let doc = Doc::new();
    scene_io::load_into_engine(&mut engine, layered_scene()).expect("场景必须能装载");

    let base = index_of(&engine, "底座");
    let post = index_of(&engine, "立柱");
    let ball = index_of(&engine, "顶球");
    let loose = index_of(&engine, "旁边的球");
    assert_eq!(engine.scene.objects[post].parent, Some(base));
    assert_eq!(engine.scene.objects[ball].parent, Some(post));
    assert_eq!(
        engine.scene.objects[loose].parent, None,
        "没有 parent 的就是根"
    );

    // 立柱：底座在 (0,0.1,0)，立柱局部 (1,1,0) → 世界 (1,1.1,0)。
    let world_post = engine.scene.world_transform(post);
    assert!(
        (world_post.position - Vec3::new(1.0, 1.1, 0.0)).length() < 1e-4,
        "{:?}",
        world_post.position
    );
    // 顶球：立柱绕 Y 转了 90°，所以立柱局部的 (0,1,0) 在世界里仍是 (0,1,0)。
    let world_ball = engine.scene.world_transform(ball);
    assert!(
        (world_ball.position - Vec3::new(1.0, 2.1, 0.0)).length() < 1e-4,
        "{:?}",
        world_ball.position
    );
    // 根物体的世界变换就是它自己的局部变换。
    assert_eq!(
        engine.scene.world_transform(loose).position,
        Vec3::new(4.0, 0.3, 0.0)
    );
    assert_eq!(doc.model_instances.len(), 0);
}

/// 拖动父物体：子物体的**世界**位置跟着走，而它自己存的局部量一个字节都没变。
#[test]
fn moving_the_parent_moves_the_children() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    scene_io::load_into_engine(&mut engine, layered_scene()).expect("场景必须能装载");
    let base = index_of(&engine, "底座");
    let post = index_of(&engine, "立柱");
    let ball = index_of(&engine, "顶球");

    let before_ball = engine.scene.world_transform(ball);
    let before_local = engine.scene.objects[post].transform;
    // 走"视口拖动"那条路：算出来的是世界变换。
    let mut world = engine.scene.world_transform(base);
    world.position += Vec3::new(0.0, 5.0, 0.0);
    assert!(engine.scene.set_world_transform(base, world));

    let after_ball = engine.scene.world_transform(ball);
    assert!(
        (after_ball.position - (before_ball.position + Vec3::new(0.0, 5.0, 0.0))).length() < 1e-4,
        "父物体上移 5，子物体应跟着上移 5：{:?} → {:?}",
        before_ball.position,
        after_ball.position
    );
    assert_eq!(
        engine.scene.objects[post].transform.position, before_local.position,
        "子物体存的是局部量，不该被父物体的移动改掉"
    );
}

/// 改父物体**不跳位**：世界位置在改父前后一致（哪怕新父物体带旋转）。
#[test]
fn reparenting_keeps_the_world_pose() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    scene_io::load_text_into_doc(&mut engine, &mut doc, layered_scene()).expect("场景必须能装载");
    let post = index_of(&engine, "立柱");
    let loose = index_of(&engine, "旁边的球");

    let before = engine.scene.world_transform(loose);
    assert!(
        doc.set_parent(&mut engine, loose, Some(post)),
        "{}",
        doc.status
    );
    assert_eq!(engine.scene.objects[loose].parent, Some(post));
    let after = engine.scene.world_transform(loose);
    assert!(
        (after.position - before.position).length() < 1e-3,
        "改父后世界位置必须不变：{:?} → {:?}（{}）",
        before.position,
        after.position,
        doc.status
    );
    // 提回根物体也不跳位。
    assert!(doc.set_parent(&mut engine, loose, None));
    assert_eq!(engine.scene.objects[loose].parent, None);
    assert!((engine.scene.world_transform(loose).position - before.position).length() < 1e-3);
    // 刚体不能挂父物体：地面是 static 刚体（选的不是"底座"，否则会先撞上成环检查）。
    let ground = index_of(&engine, "地面");
    assert!(!doc.set_parent(&mut engine, ground, Some(post)));
    assert!(doc.status.contains("刚体"), "{}", doc.status);
}

/// 成环会被挡住，并且状态栏说清楚原因（而不是"点了没反应"）。
#[test]
fn reparenting_refuses_a_cycle() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    scene_io::load_text_into_doc(&mut engine, &mut doc, layered_scene()).expect("场景必须能装载");
    let base = index_of(&engine, "底座");
    let post = index_of(&engine, "立柱");
    let ball = index_of(&engine, "顶球");

    // 把"底座"挂到它的孙子"顶球"下面 → 成环。
    assert!(!doc.set_parent(&mut engine, base, Some(ball)));
    assert!(doc.status.contains("成环"), "{}", doc.status);
    assert_eq!(
        engine.scene.objects[base].parent, None,
        "失败时不能改坏层级"
    );
    // 直接调引擎接口也一样。
    assert!(
        engine.scene.set_parent(ball, Some(ball)).is_err(),
        "自己不能当自己的父物体"
    );
    assert!(engine.scene.set_parent(post, Some(ball)).is_err());
    // 祖先关系是"向上走"：立柱是顶球的父物体，所以立柱是顶球的祖先，反过来不是。
    assert!(engine.scene.is_ancestor(post, ball));
    assert!(!engine.scene.is_ancestor(ball, post));
}

/// 删掉父物体：子物体提到上一级、**保持世界位置**，后面物体的父下标整体前移。
#[test]
fn deleting_a_parent_promotes_children_without_jumping() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    scene_io::load_into_engine(&mut engine, layered_scene()).expect("场景必须能装载");
    let post = index_of(&engine, "立柱");
    let ball = index_of(&engine, "顶球");
    let loose = index_of(&engine, "旁边的球");
    let before_ball = engine.scene.world_transform(ball);
    let before_loose = engine.scene.world_transform(loose);

    let removed = engine.scene.remove(post).expect("立柱应当存在");
    assert_eq!(removed.name, "立柱");
    // 顶球被提到立柱的父级（底座）下，世界位置不变。
    let ball_now = engine.scene.find("顶球").expect("顶球还在");
    assert_eq!(
        engine.scene.objects[ball_now].parent,
        Some(engine.scene.find("底座").unwrap())
    );
    assert!(
        (engine.scene.world_transform(ball_now).position - before_ball.position).length() < 1e-3,
        "{:?} → {:?}",
        before_ball.position,
        engine.scene.world_transform(ball_now).position
    );
    // 排在后面的物体下标前移了，但它自己的世界位置没变。
    let loose_now = engine.scene.find("旁边的球").expect("旁边的球还在");
    assert!(loose_now < loose);
    assert!(
        (engine.scene.world_transform(loose_now).position - before_loose.position).length() < 1e-4
    );
}

/// 存盘 → 读回：`parent` 与**局部**变换都原样保留，世界位置也一致。
#[test]
fn export_writes_local_transform_and_parent() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    scene_io::load_text_into_doc(&mut engine, &mut doc, layered_scene()).expect("场景必须能装载");
    let ball = index_of(&engine, "顶球");
    let before = engine.scene.world_transform(ball);

    let saved = scene_io::scene_to_json(&engine, &doc).expect("存盘必须成功");
    // 用装载器同一条解析路径读回来，避免"测试里能读、真装载读不了"。
    let parsed =
        quench3d_cli::scene_doc::SceneDoc::from_json(&saved.json).expect("存出来必须能解析");
    let ball_doc = parsed
        .objects
        .iter()
        .find(|o| o.name.as_deref() == Some("顶球"))
        .expect("顶球应当被写进 objects");
    assert_eq!(ball_doc.parent.as_deref(), Some("立柱"), "父物体按名字写");
    assert_eq!(
        ball_doc.position,
        Some([0.0, 1.0, 0.0]),
        "写的是**局部**位置，不是世界位置"
    );

    // 读回来：世界位置一致（说明局部量 + 父链的语义前后一致）。
    let mut again = headless_engine().expect("引擎应当可用");
    scene_io::load_into_engine(&mut again, &saved.json).expect("存出来的必须能读回");
    let ball_again = index_of(&again, "顶球");
    let after = again.scene.world_transform(ball_again);
    assert!(
        (after.position - before.position).length() < 1e-3,
        "{:?} vs {:?}",
        before.position,
        after.position
    );
}

/// 父物体没被写成物体时（地面 / 模型展开的网格），关系只能丢掉 —— 但不能丢得无声无息。
#[test]
fn dropped_parent_relation_is_reported() {
    let Some(mut engine) = headless_engine() else {
        return;
    };
    let mut doc = Doc::new();
    // 地面物体在场景里叫「地面」；把旁边的球挂到它下面，存盘时父物体名写不出去。
    scene_io::load_text_into_doc(&mut engine, &mut doc, layered_scene()).expect("场景必须能装载");
    let ground = index_of(&engine, "地面");
    let loose = index_of(&engine, "旁边的球");
    assert!(
        doc.set_parent(&mut engine, loose, Some(ground)),
        "{}",
        doc.status
    );
    assert_eq!(engine.scene.objects[loose].parent, Some(ground));

    let saved = scene_io::scene_to_json(&engine, &doc).expect("存盘必须成功");
    let parsed =
        quench3d_cli::scene_doc::SceneDoc::from_json(&saved.json).expect("存出来必须能解析");
    let loose_doc = parsed
        .objects
        .iter()
        .find(|o| o.name.as_deref() == Some("旁边的球"))
        .expect("旁边的球应当被写进 objects");
    assert!(
        loose_doc.parent.is_none(),
        "父物体（地面）没有物体名，不能写出悬空引用: {loose_doc:?}"
    );
    assert!(
        saved.skipped.iter().any(|s| s.contains("父子关系")),
        "丢掉的关系必须上报：{:?}",
        saved.skipped
    );
    // 丢掉父子关系之后，文件仍然读得回来（没有悬空引用）。
    scene_io::load_into_engine(&mut engine, &saved.json).expect("存出来的必须能读回");
}
