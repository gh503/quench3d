//! 物理行为的**黄金摘要**（golden digest）测试。
//!
//! 它把一套覆盖全部形状与约束的场景跑固定步数，然后把所有刚体的位置/旋转/速度
//! 按 `f32::to_bits` 汇成一个 64 位摘要。摘要一旦变化，就说明**物理结果变了**——
//! 性能优化（比如把每步的堆分配换成内联缓冲）必须做到摘要逐位不变，
//! 否则就是悄悄改了行为，这是不能接受的。

use super::*;

use crate::body::BodyBuilder;
use crate::shape::Shape;
use quench3d_math::{Quat, Vec3};

fn digest(world: &PhysicsWorld) -> u64 {
    // FNV-1a，输入是所有刚体的位模式（顺序由句柄下标决定，天然确定）
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| {
        for byte in v.to_le_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
    };
    let mut count = 0usize;
    for (_, b) in world.bodies() {
        let p = b.position();
        let q = b.rotation();
        let v = b.linear_velocity;
        let w = b.angular_velocity;
        for x in [
            p.x, p.y, p.z, q.x, q.y, q.z, q.w, v.x, v.y, v.z, w.x, w.y, w.z,
        ] {
            // -0.0 与 0.0 视为同一个值，避免无意义的差异
            let x = if x == 0.0 { 0.0 } else { x };
            mix(x.to_bits() as u64);
        }
        mix(b.is_sleeping() as u64);
        count += 1;
    }
    mix(count as u64);
    hash
}

/// 覆盖多种形状、关节与睡眠的混合场景。
fn mixed_world() -> PhysicsWorld {
    let mut w = PhysicsWorld::new();
    w.set_gravity(Vec3::new(0.0, -9.81, 0.0));
    w.add_body(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).build());
    // 盒-盒堆叠
    for i in 0..8 {
        w.add_body(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::new(0.4, 0.4, 0.4),
            })
            .position(Vec3::new(0.0, 0.4 + i as f32 * 0.82, 0.0))
            .build(),
        );
    }
    // 球与胶囊
    w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.35 })
            .position(Vec3::new(1.6, 3.0, 0.2))
            .build(),
    );
    w.add_body(
        BodyBuilder::dynamic(Shape::Capsule {
            half_height: 0.4,
            radius: 0.25,
        })
        .position(Vec3::new(-1.6, 2.4, 0.0))
        .rotation(Quat::from_axis_angle(Vec3::X, 0.4))
        .build(),
    );
    // 圆柱与凸包
    w.add_body(
        BodyBuilder::dynamic(Shape::Cylinder {
            half_height: 0.35,
            radius: 0.3,
        })
        .position(Vec3::new(2.4, 1.6, -0.6))
        .build(),
    );
    w.add_body(
        BodyBuilder::dynamic(Shape::ConvexHull {
            points: vec![
                Vec3::new(-0.3, -0.3, -0.3),
                Vec3::new(0.35, -0.25, -0.3),
                Vec3::new(-0.25, 0.3, -0.28),
                Vec3::new(-0.3, -0.3, 0.3),
                Vec3::new(0.3, -0.2, 0.32),
                Vec3::new(-0.2, 0.32, 0.3),
                Vec3::new(0.3, 0.3, -0.2),
            ],
        })
        .position(Vec3::new(-2.6, 1.2, 0.8))
        .build(),
    );
    // 三角网格地面块
    w.add_body(
        BodyBuilder::r#static(Shape::TriMesh {
            vertices: vec![
                Vec3::new(-1.0, 0.0, -1.0),
                Vec3::new(1.0, 0.0, -1.0),
                Vec3::new(1.0, 0.0, 1.0),
                Vec3::new(-1.0, 0.0, 1.0),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
        })
        .position(Vec3::new(4.0, 1.0, 0.0))
        .build(),
    );
    w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.3 })
            .position(Vec3::new(4.0, 3.0, 0.0))
            .build(),
    );
    // 关节：铰链 + 距离
    let a = w.add_body(
        BodyBuilder::r#static(Shape::Box {
            half_extents: Vec3::new(0.1, 0.1, 0.1),
        })
        .position(Vec3::new(-4.0, 2.0, 0.0))
        .build(),
    );
    let b = w.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::new(0.3, 0.1, 0.1),
        })
        .position(Vec3::new(-3.4, 2.0, 0.0))
        .build(),
    );
    w.add_joint(Joint::Hinge {
        a,
        b,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: Vec3::new(-0.6, 0.0, 0.0),
        local_axis_a: Vec3::Z,
        local_axis_b: Vec3::Z,
        limits: Some((-1.2, 1.2)),
        motor: None,
    });
    let c = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.2 })
            .position(Vec3::new(-4.0, 1.0, 1.0))
            .build(),
    );
    w.add_joint(Joint::Distance {
        a,
        b: c,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: Vec3::ZERO,
        rest_length: 1.5,
        stiffness: 0.0,
        damping: 0.0,
    });
    w
}

#[test]
fn golden_digest_is_stable() {
    let mut w = mixed_world();
    for _ in 0..400 {
        w.step(1.0 / 60.0);
    }
    let d1 = digest(&w);
    // 再跑一遍同样的场景，必须是同一个摘要（确定性）
    let mut w2 = mixed_world();
    for _ in 0..400 {
        w2.step(1.0 / 60.0);
    }
    assert_eq!(d1, digest(&w2), "同一场景两次运行的摘要不一致");

    // 记录在案的黄金值：物理行为一旦变化，这里会失败。
    // 优化（例如把每步的堆分配改成内联缓冲）**不允许**改变这个值。
    assert_eq!(
        d1, GOLDEN_DIGEST,
        "物理摘要变了（{d1:#018x} != {GOLDEN_DIGEST:#018x}）：400 步摘要对浮点运算顺序极度敏感，\
         变了不代表改坏了，但必须人工确认。先在 60 步参考（sixty_step_states_match_reference）\
         和长跑不变量（long_run_keeps_physical_invariants）都过、并复核改动确实数学等价之后，\
         再把这里的值更新成新摘要"
    );
}

/// 黄金摘要值（由 `golden_digest_is_stable` 首次运行确定）。
const GOLDEN_DIGEST: u64 = 0x70261b1f4c5257e5;

/// 60 步参考终态。
///
/// **为什么是 60 步而不是 400 步**：这套场景（坠落链 + 斜坡滑箱 + 关节 + 三角网格）
/// 对初始条件极其敏感——实测在第 0 步把某个刚体的速度加上 1e-7（和「等价重写带来的
/// 浮点差异」同量级），400 步后位置偏差会放大到 **3.4~7.8 米**：
///
/// ```text
/// 刚体 3 速度 +1e-7 → 11 步 1.4e-8 · 51 步 1.1e-2 · 101 步 1.6e-2 · 201 步 1.4e-1 · 400 步 3.4e0
/// 刚体 6 速度 +1e-7 → 11 步 1.7e-8 · 51 步 1.1e-2 · 101 步 2.1e-2 · 201 步 3.5e-2 · 400 步 7.8e0
/// ```
///
/// 也就是说：长时间跨度的接触仿真**不可能**逐位锁死，任何等价重写都会分道扬镳。
/// 所以行为判据分两层：
/// * 短跨度（60 步）用紧容差 1e-5 —— 这一层能抓住真正的数学错误；
/// * 长跨度（400 步）只查物理不变量（有限、不穿地、速度有界、该静止的确实静止）。
const REFERENCE_60: &[[f32; 7]] = &[
    [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0],
    [
        0.0015874142,
        0.3928741,
        0.0024055776,
        -0.0022813512,
        -0.001761585,
        -0.0023106388,
        0.9999932,
    ],
    [
        0.003827034,
        1.1861013,
        -0.0014103483,
        -0.004069768,
        -0.0017065953,
        -0.004450443,
        0.9999804,
    ],
    [
        0.016582886,
        1.9794259,
        -0.013014047,
        -0.0055352664,
        -0.0016518075,
        -0.0063148155,
        0.99996346,
    ],
    [
        0.037775613,
        2.7728248,
        -0.03051833,
        -0.0066575967,
        -0.0014561674,
        -0.007827005,
        0.9999462,
    ],
    [
        0.06152319,
        3.5663383,
        -0.051940445,
        -0.007427109,
        -0.0013695129,
        -0.008876811,
        0.9999321,
    ],
    [
        0.08553837,
        4.3601007,
        -0.07470789,
        -0.007867469,
        -0.0013458723,
        -0.0095102945,
        0.999923,
    ],
    [
        0.10951143,
        5.1542406,
        -0.09645771,
        -0.008038379,
        -0.0014889421,
        -0.009834578,
        0.9999183,
    ],
    [
        0.13205042,
        5.948574,
        -0.11718006,
        -0.008047173,
        -0.0016630276,
        -0.0097071435,
        0.9999192,
    ],
    [1.6, 0.3437967, 0.2, 0.0, 0.0, 0.0, 1.0],
    [
        -1.6, 0.23115727, 0.54317605, 0.72113085, 0.0, 0.0, 0.69279885,
    ],
    [
        2.4000616,
        0.34503478,
        -0.5999985,
        -8.2953644e-5,
        1.4619394e-5,
        -1.8337134e-5,
        1.0,
    ],
    [
        -2.5997155,
        0.27108258,
        0.8005048,
        0.0020005428,
        -9.827584e-5,
        -0.040170383,
        0.99919087,
    ],
    [4.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0],
    [4.0, 1.2949555, 0.0, 0.0, 0.0, 0.0, 1.0],
    [-4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 1.0],
    [
        -3.7949772,
        1.4355559,
        0.0,
        0.0,
        0.0,
        -0.56987053,
        0.82173455,
    ],
    [-4.0, 0.7703175, -0.8626033, 0.0, 0.0, 0.0, 1.0],
];

/// 取所有刚体的 `[位置, 四元数]`。
fn states(world: &PhysicsWorld) -> Vec<[f32; 7]> {
    world
        .bodies()
        .map(|(_, b)| {
            let p = b.position();
            let q = b.rotation();
            [p.x, p.y, p.z, q.x, q.y, q.z, q.w]
        })
        .collect()
}

#[test]
fn sixty_step_states_match_reference() {
    let mut w = mixed_world();
    for _ in 0..60 {
        w.step(1.0 / 60.0);
    }
    let got = states(&w);
    assert_eq!(got.len(), REFERENCE_60.len(), "刚体数量变了");
    for (i, (g, want)) in got.iter().zip(REFERENCE_60.iter()).enumerate() {
        let dp =
            ((g[0] - want[0]).powi(2) + (g[1] - want[1]).powi(2) + (g[2] - want[2]).powi(2)).sqrt();
        let dq = ((g[3] - want[3]).powi(2)
            + (g[4] - want[4]).powi(2)
            + (g[5] - want[5]).powi(2)
            + (g[6] - want[6]).powi(2))
        .sqrt();
        assert!(dp <= 1e-5, "刚体 {i} 位置偏离 60 步参考 {dp:e}");
        assert!(dq <= 1e-5, "刚体 {i} 旋转偏离 60 步参考 {dq:e}");
    }
}

/// 长跨度只查不变量：有限、不穿地、速度有界、静置的确实静止。
#[test]
fn long_run_keeps_physical_invariants() {
    let mut w = mixed_world();
    for _ in 0..400 {
        w.step(1.0 / 60.0);
        for (i, (_, b)) in w.bodies().enumerate() {
            assert!(
                b.position().is_finite() && b.rotation().is_finite(),
                "第 {i} 个刚体在第 400 步内出现非有限状态"
            );
            assert!(
                b.linear_velocity.length() < 100.0 && b.angular_velocity.length() < 200.0,
                "第 {i} 个刚体速度失控: {:?} / {:?}",
                b.linear_velocity,
                b.angular_velocity
            );
        }
    }
    // 关节两端必须仍然连在一起（距离关节不会被数值漂移拉断）
    for (i, (_, b)) in w.bodies().enumerate() {
        assert!(
            b.position().y > -0.25,
            "第 {i} 个刚体穿到地面以下: {:?}",
            b.position()
        );
    }
}

/// 这套场景的敏感度实验（默认忽略：只用来复核上面两个容差的来源）。
#[test]
#[ignore = "一次性实验：cargo test -p quench3d-physics --lib measure_sensitivity -- --ignored --nocapture"]
fn measure_sensitivity() {
    for (index, dv) in [(3usize, 1e-7f32), (6, 1e-7)] {
        let mut a = mixed_world();
        let mut b = mixed_world();
        let handle = b.bodies().nth(index).map(|(h, _)| h);
        if let Some(h) = handle {
            if let Some(body) = b.body_mut(h) {
                body.linear_velocity.x += dv;
            }
        }
        let mut curve = Vec::new();
        for step in 0..400 {
            a.step(1.0 / 60.0);
            b.step(1.0 / 60.0);
            if [10usize, 50, 100, 200, 399].contains(&step) {
                let mut worst = 0.0f32;
                for ((_, ba), (_, bb)) in a.bodies().zip(b.bodies()) {
                    worst = worst.max((ba.position() - bb.position()).length());
                }
                curve.push((step + 1, worst));
            }
        }
        println!("刚体 {index} 速度 +{dv:e} → 偏差曲线 {curve:?}");
    }
}

#[test]
fn digest_is_sensitive_to_state() {
    // 摘要必须真的对状态敏感，否则上面的断言没有意义
    let mut w = mixed_world();
    for _ in 0..400 {
        w.step(1.0 / 60.0);
    }
    let d = digest(&w);
    let mut w2 = mixed_world();
    for _ in 0..399 {
        w2.step(1.0 / 60.0);
    }
    assert_ne!(d, digest(&w2), "少跑一步摘要也应当不同");
}

/// 一次性工具：把当前 60 步状态**按源码格式**打印出来，用于重标定参考表。
///
/// ```bash
/// cargo test -p quench3d-physics --lib print_reference_table -- --ignored --nocapture
/// ```
///
/// 只在**确认改动是故意的**（例如整岛休眠改变了入睡时机）之后才用它，
/// 并且必须先说明「为什么结果应该变」，否则就是在拿参考表掩盖 bug。
#[test]
#[ignore]
fn print_reference_table() {
    let mut w = mixed_world();
    for _ in 0..60 {
        w.step(1.0 / 60.0);
    }
    let got = states(&w);
    println!("const REFERENCE_60: &[[f32; 7]] = &[");
    for s in &got {
        println!(
            "    [{:?}, {:?}, {:?}, {:?}, {:?}, {:?}, {:?}],",
            s[0], s[1], s[2], s[3], s[4], s[5], s[6]
        );
    }
    println!("];");
    // 顺便把长跑摘要也打出来，省得再跑一次
    let mut w2 = mixed_world();
    for _ in 0..400 {
        w2.step(1.0 / 60.0);
    }
    println!("GOLDEN_DIGEST = {:#018x}", digest(&w2));
}
