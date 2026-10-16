//! 参考姿态（`joints[].frame_a` / `frame_b`）走**真链条**的验证：
//! 场景 JSON → `SceneDoc` → `runner::build_world` → 物理引擎。
//!
//! 为什么光有引擎测试不够：这一段的坑几乎全在**接线上** —— 字段没接进 runner、
//! 度与弧度差 57 倍、`frame_b` 被当成 `frame_a` 用。所以这里从 JSON 出发，断言
//! 跑完之后**世界朝向**落在格式文档承诺的那个角度上，并另钉两条不变式：
//! 「写全 0 == 不写（逐位相同）」与「不给参考姿态时行为与老版本一致」。

use quench3d::physics::fixed_frame_error_deg;
use quench3d::prelude::Quat;
use quench3d_cli::runner::build_world;
use quench3d_cli::scene_doc::SceneDoc;

/// 与引擎缺省子步一致。
const DT: f32 = 1.0 / 60.0;

/// 每个刚体的 (名字, 位置, 朝向)，按名字排序（`handles` 是 BTreeMap，顺序稳定）。
type Snapshot = Vec<(String, [f32; 3], [f32; 4])>;

fn run(scene: &str, steps: usize) -> Snapshot {
    let doc = SceneDoc::from_json(scene).expect("场景必须能解析");
    let diags = doc.validate();
    assert!(!SceneDoc::has_errors(&diags), "场景不该有错误：{diags:?}");
    let mut built = build_world(&doc).expect("必须能建出世界");
    built.step(steps as u32, DT);
    built
        .handles
        .iter()
        .map(|(name, h)| {
            let b = built.world.body(*h).expect("刚体必须还在");
            (
                name.clone(),
                b.position().to_array(),
                b.rotation().to_array(),
            )
        })
        .collect()
}

/// 逐位快照（`f32::to_bits`）：用来证明两条路径在数值上**完全**一致，而不是"差不多"。
fn bits(snapshot: &Snapshot) -> Vec<(String, [u32; 3], [u32; 4])> {
    snapshot
        .iter()
        .map(|(n, p, r)| (n.clone(), p.map(f32::to_bits), r.map(f32::to_bits)))
        .collect()
}

/// 静态底座 + 动态门板，锚点都在原点（角自由度干净）。
fn scene_with_joint(extra: &str) -> String {
    format!(
        r#"{{ "objects": [
               {{ "name": "底座", "body": "static", "shape": {{ "type": "box", "size": [0.2, 0.2, 0.2] }},
                  "position": [0, 2, 0] }},
               {{ "name": "门板", "body": "dynamic", "shape": {{ "type": "box", "size": [0.4, 0.4, 0.4] }},
                  "position": [0, 2, 0], "rotation_deg": [0, 90, 0] }} ],
             "joints": [ {{ "type": "fixed", "a": "底座", "b": "门板"{extra} }} ] }}"#
    )
}

#[test]
fn frame_a_degrees_reach_the_engine_and_twist_the_door() {
    // `frame_a: [0, 90, 0]` = 绕 Y 转 90°（度，XYZ 顺序）→ 门板最终就该停在 90° 上。
    // 门板出生时是 0°，所以这一条同时验证了"真的被扭过去了"。
    let scene = scene_with_joint(r#", "frame_a": [0, 90, 0]"#);
    let doc = SceneDoc::from_json(&scene).unwrap();
    let diags = doc.validate();
    assert!(
        !SceneDoc::has_errors(&diags),
        "参考姿态不该产生错误：{diags:?}"
    );

    let mut built = build_world(&doc).unwrap();
    built.step(180, DT);
    let base = built.world.body(built.handles["底座"]).unwrap();
    let door = built.world.body(built.handles["门板"]).unwrap();

    let target = Quat::from_rotation_y(core::f32::consts::FRAC_PI_2);
    let off_deg = door.rotation().angle_between(target).to_degrees();
    assert!(
        off_deg < 2.0,
        "门板应被扭到 90°（绕 Y），实际差 {off_deg}°（朝向 {:?}）",
        door.rotation().to_array()
    );
    // 用引擎自己的判据再量一次：两端的参考姿态误差应该收敛到 0。
    let e = fixed_frame_error_deg(
        base.rotation(),
        Quat::from_rotation_y(core::f32::consts::FRAC_PI_2),
        door.rotation(),
        Quat::IDENTITY,
    );
    assert!(e < 2.0, "参考姿态误差应 < 2°，实际 {e}°");
    // 点约束还在：门板不许从底座上掉下来（重力拉、关节扛）。
    let gap = (door.position() - base.position()).length();
    assert!(gap < 0.05, "锚点位移 {gap} 过大");
}

#[test]
fn frame_b_alone_also_twists_the_door() {
    // B 端给参考姿态：目标变成 q_b = frame_b 的逆（A、B 的参考姿态要重合）。
    let scene = scene_with_joint(r#", "frame_b": [0, 90, 0]"#);
    let doc = SceneDoc::from_json(&scene).unwrap();
    let mut built = build_world(&doc).unwrap();
    built.step(180, DT);
    let door = built.world.body(built.handles["门板"]).unwrap();
    let target = Quat::from_rotation_y(-core::f32::consts::FRAC_PI_2);
    let off_deg = door.rotation().angle_between(target).to_degrees();
    assert!(
        off_deg < 2.0,
        "frame_b 也应生效（目标 -90°），实际差 {off_deg}°"
    );
}

#[test]
fn omitting_frames_keeps_the_creation_pose() {
    // 不给 frame_*：老行为 —— 锁定创建那一刻的相对姿态（门板出生就是 90°）。
    let snapshot = run(&scene_with_joint(""), 180);
    let door = snapshot.iter().find(|(n, _, _)| n == "门板").unwrap();
    let q = Quat::from_xyzw(door.2[0], door.2[1], door.2[2], door.2[3]);
    let off_deg = q
        .angle_between(Quat::from_rotation_y(core::f32::consts::FRAC_PI_2))
        .to_degrees();
    assert!(
        off_deg < 2.0,
        "缺省固定关节应锁住创建时的 90°，实际差了 {off_deg}°（被扭平了）"
    );
}

#[test]
fn zero_frames_are_bit_identical_to_omitting_them() {
    // **向后兼容的硬证据**：显式写 `"frame_a": [0, 0, 0]` 与完全不写，120 步之后
    // 每个刚体的位置/朝向**逐位相同**（不是"看起来差不多"）。
    // 校验会给一条 ineffective 警告（写了等于没写），这里只要求它不是错误。
    let plain = run(&scene_with_joint(""), 120);
    let zero = run(
        &scene_with_joint(r#", "frame_a": [0, 0, 0], "frame_b": [0, 0, 0]"#),
        120,
    );
    assert_eq!(
        bits(&plain),
        bits(&zero),
        "写全 0 必须与不写逐位相同（老场景的行为不能被改）"
    );
    assert_eq!(plain.len(), 2, "底座 + 门板");
}

#[test]
fn reference_frame_joint_keeps_both_bodies_awake() {
    // 参考姿态是**每步都在修**的约束：睡眠会把求解器里的刚体完全冻结
    // （`BodyView::from_body` 把 inv_m / inv_I 清零），修正就会停在半路。
    // 所以驱动中的两端必须保持清醒 —— 这里用"睡够时间也还醒着"来钉住。
    let scene = scene_with_joint(r#", "frame_a": [0, 90, 0]"#);
    let doc = SceneDoc::from_json(&scene).unwrap();
    let mut built = build_world(&doc).unwrap();
    built.step(600, DT); // 10 秒，远超自动睡眠的判定时间
    for name in ["底座", "门板"] {
        let b = built.world.body(built.handles[name]).unwrap();
        if b.is_dynamic() {
            assert!(
                !b.is_sleeping(),
                "{name} 被参考姿态驱动，不该睡着（睡着就修不动了）"
            );
        }
    }
    // 位置仍然钉住（不是"没睡但也没在修"）。
    let base = built.world.body(built.handles["底座"]).unwrap();
    let door = built.world.body(built.handles["门板"]).unwrap();
    assert!((door.position() - base.position()).length() < 0.05);
    assert!(
        fixed_frame_error_deg(
            base.rotation(),
            Quat::from_rotation_y(core::f32::consts::FRAC_PI_2),
            door.rotation(),
            Quat::IDENTITY,
        ) < 2.0
    );
}
