//! 固定关节的**参考姿态**（`local_frame_a` / `local_frame_b`）端到端数值验证。
//!
//! 这一组测试全部是 CPU 物理（无显卡、无渲染、无文件系统），所以 CI 的每条腿都能跑：
//! 它们回答的是"给了参考姿态之后，N 秒内两端的相对姿态真的收敛到它了吗"，
//! 以及"不给参考姿态的老场景有没有被改坏"。
//!
//! 场景刻意做成最简单的形态：静态基准 + 动态箱体，**两端锚点都在质心**，
//! 于是点约束把箱体钉在原地，只剩角自由度 —— 收敛与否不会被平动耦合掩盖。

use quench3d_math::{vec3, Quat, Vec3};
use quench3d_physics::{fixed_frame_error_deg, BodyBuilder, Joint, PhysicsWorld, Shape};

/// 与引擎缺省子步一致（`PhysicsWorld` 的固定步长）。
const DT: f32 = 1.0 / 60.0;

/// 静态基准 + 动态箱体，用固定关节连起来；锚点都在质心。
fn weld(fa: Quat, fb: Quat, initial_b: Quat) -> (PhysicsWorld, quench3d_physics::BodyHandle) {
    let mut w = PhysicsWorld::new();
    let a = w.add_body(
        BodyBuilder::r#static(Shape::Sphere { radius: 0.1 })
            .position(Vec3::ZERO)
            .build(),
    );
    let b = w.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.2),
        })
        .position(Vec3::ZERO)
        .rotation(initial_b)
        .build(),
    );
    w.add_joint(Joint::Fixed {
        a,
        b,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: Vec3::ZERO,
        local_frame_a: fa,
        local_frame_b: fb,
    });
    (w, b)
}

/// 关节两端在世界系里的参考姿态误差（度）。
fn error_deg(w: &PhysicsWorld, b: quench3d_physics::BodyHandle, fa: Quat, fb: Quat) -> f32 {
    let q_b = w.body(b).expect("刚体必须还在").rotation();
    fixed_frame_error_deg(Quat::IDENTITY, fa, q_b, fb)
}

/// 给了参考姿态：B 应该被**扭到**它（90° 绕 Z），并在 1 秒内基本到位。
///
/// 这条同时钉住三件事：误差确实在收敛、收敛到的是"参考姿态"而不是别的朝向、
/// 收敛过程中点约束没被带歪（箱体仍钉在原点）。
#[test]
fn fixed_reference_frame_twists_b_to_the_pose() {
    let fa = Quat::from_rotation_z(core::f32::consts::FRAC_PI_2);
    let fb = Quat::IDENTITY;
    let (mut w, b) = weld(fa, fb, Quat::IDENTITY);

    let initial = error_deg(&w, b, fa, fb);
    assert!(
        (initial - 90.0).abs() < 1e-3,
        "初始误差应该正好是 90°，实际 {initial}"
    );

    let mut worst_position = 0.0f32;
    let mut error_1s = initial;
    let mut ever_grew = 0.0f32;
    for step in 0..180 {
        w.step(DT);
        let e = error_deg(&w, b, fa, fb);
        ever_grew = ever_grew.max(e - initial);
        if step == 59 {
            error_1s = e;
        }
        worst_position = worst_position.max(w.body(b).unwrap().position().length());
    }
    let final_error = error_deg(&w, b, fa, fb);
    let final_rot = w.body(b).unwrap().rotation();
    let to_target_deg = final_rot.angle_between(fa).to_degrees();

    assert!(
        error_1s < 5.0,
        "1 秒后参考姿态误差应 < 5°，实际 {error_1s}°（伺服没收住）"
    );
    assert!(
        final_error < 2.0,
        "3 秒后参考姿态误差应 < 2°，实际 {final_error}°"
    );
    assert!(
        to_target_deg < 2.0,
        "B 的世界朝向应转到参考姿态 {fa:?}，实际差 {to_target_deg}°"
    );
    // 修正过程不该过冲（一阶伺服的特征）：误差不允许超过初始值。
    assert!(ever_grew < 1.0, "误差中途比初始还大 {ever_grew}°（过冲）");
    // 点约束仍然把箱体钉在原点（参考姿态只管角度，不接管位置）。
    assert!(
        worst_position < 0.05,
        "平动不该被角修正带跑，最大位移 {worst_position}"
    );
}

/// **老行为不变**：两端参考姿态都是单位四元数时，固定关节锁的是**创建那一刻**的
/// 相对姿态（这里就是 90°），不会把 B 扭到与 A 同朝向。
///
/// 反过来说：如果谁把"单位参考姿态"也当成"扭到重合"，这条测试会立刻从 87° 掉到 0°。
#[test]
fn fixed_without_reference_frames_keeps_the_creation_pose() {
    let spin = Quat::from_rotation_z(core::f32::consts::FRAC_PI_2);
    let (mut w, b) = weld(Quat::IDENTITY, Quat::IDENTITY, spin);
    for _ in 0..180 {
        w.step(DT);
    }
    let final_rot = w.body(b).unwrap().rotation();
    let kept = final_rot.angle_between(spin).to_degrees();
    assert!(
        kept < 2.0,
        "缺省固定关节应锁定创建时的相对姿态（{spin:?}），实际差了 {kept}°"
    );
    // 这个场景是**有判别力**的：按新语义它本该收敛到 0°，两种解释相差 90°。
    let legacy_error = error_deg(&w, b, Quat::IDENTITY, Quat::IDENTITY);
    assert!(
        legacy_error > 85.0,
        "缺省参考姿态的误差不该被求解器消掉，实际 {legacy_error}°"
    );
}

/// 两端都是动态刚体时也要收敛（冲量必须同时作用在两侧，不能只在 B 上）。
#[test]
fn fixed_reference_frame_works_with_two_dynamic_bodies() {
    let mut w = PhysicsWorld::new();
    let fa = Quat::from_rotation_x(core::f32::consts::FRAC_PI_3);
    let fb = Quat::IDENTITY;
    let a = w.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.25),
        })
        .position(Vec3::ZERO)
        .build(),
    );
    let b = w.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.25),
        })
        .position(Vec3::ZERO)
        .rotation(Quat::from_rotation_y(core::f32::consts::FRAC_PI_2))
        .build(),
    );
    w.add_joint(Joint::Fixed {
        a,
        b,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: Vec3::ZERO,
        local_frame_a: fa,
        local_frame_b: fb,
    });
    for _ in 0..240 {
        w.step(DT);
    }
    let q_a = w.body(a).unwrap().rotation();
    let q_b = w.body(b).unwrap().rotation();
    let e = fixed_frame_error_deg(q_a, fa, q_b, fb);
    assert!(e < 2.0, "两个动态刚体的参考姿态误差应 < 2°，实际 {e}°");
    // 两端都在动（不是"A 不动、只有 B 转"）——否则冲量只作用在一侧。
    let w_a = w.body(a).unwrap().angular_velocity().length();
    let w_b = w.body(b).unwrap().angular_velocity().length();
    assert!(w_a < 1.0 && w_b < 1.0, "收敛后两端应都停下：{w_a} / {w_b}");
    // 对称场景里两端各转一半：A 转了约 30°、B 转了约 -30°（相对差 60°）。
    let a_deg = q_a.angle_between(Quat::IDENTITY).to_degrees();
    assert!(
        (5.0..55.0).contains(&a_deg),
        "A 也该跟着转（一半误差），实际 {a_deg}°"
    );
}

/// 参考姿态是"每步都在修"的持续约束，不是一次性初始化：中途被外力推歪，
/// 松手之后要能自己回来。这一条也顺带证明睡眠没有把修正冻住。
#[test]
fn fixed_reference_frame_re_corrects_after_being_disturbed() {
    let fa = Quat::from_rotation_z(0.5);
    let fb = Quat::IDENTITY;
    let (mut w, b) = weld(fa, fb, Quat::IDENTITY);
    for _ in 0..120 {
        w.step(DT);
    }
    assert!(error_deg(&w, b, fa, fb) < 2.0, "先收敛到位");
    // 推歪 3.0 rad/s 的角速度，并清掉睡眠计时（等价于"撞了一下"）。
    {
        let body = w.body_mut(b).expect("刚体必须还在");
        body.set_angular_velocity(vec3(0.0, 0.0, 3.0));
        body.wake_up();
    }
    for _ in 0..180 {
        w.step(DT);
    }
    let e = error_deg(&w, b, fa, fb);
    assert!(e < 2.0, "被推歪之后应该自己扭回来，实际误差 {e}°");
}
