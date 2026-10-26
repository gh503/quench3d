//! 编辑器回放时间轴的纯 CPU 回归：录制按物理步去重、拖动写回首/末帧。

use quench3d::prelude::*;
use quench3d_editor::doc::Doc;

#[test]
fn replay_recording_captures_steps_and_seeks() {
    let dt = 1.0 / 60.0;
    let mut world = PhysicsWorld::default();
    world.set_fixed_dt(dt);
    let handle = world.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
            .position(vec3(0.0, 5.0, 0.0))
            .build(),
    );
    let start = world.body(handle).unwrap().transform().position.y;

    let mut doc = Doc::new();
    doc.start_replay_recording(&world);
    for _ in 0..3 {
        world.step(dt);
        doc.record_replay_frame(&world);
        // 同一个物理步重复调用不能重复记帧。
        doc.record_replay_frame(&world);
    }
    assert_eq!(doc.replay_frame_count(), 4, "初始帧 + 3 步");
    assert_eq!(doc.stop_replay_recording(), 4);
    assert!(!doc.replay_recording);

    let end = world.body(handle).unwrap().transform().position.y;
    assert!(end < start, "重力应让球下落：{start} → {end}");

    world.body_mut(handle).unwrap().transform.position = vec3(0.0, 99.0, 0.0);
    assert_eq!(doc.apply_replay_at(&mut world, 3.0 * dt), 1);
    let sampled = world.body(handle).unwrap().transform().position.y;
    assert!(
        (sampled - end).abs() < 1e-5,
        "回放末帧应写回 {end}，实际 {sampled}"
    );

    assert_eq!(doc.apply_replay_at(&mut world, 0.0), 1);
    let first = world.body(handle).unwrap().transform().position.y;
    assert!(
        (first - start).abs() < 1e-5,
        "第 0 帧应回到 {start}，实际 {first}"
    );
}
