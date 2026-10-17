//! 端到端仿真验收测试。
//!
//! 覆盖需求中要求的全部场景：自由落体、静止接触、堆叠、恢复系数、
//! 斜面摩擦、射线检测、铰链摆、确定性与宽相规模测试。

use quench3d_math::*;

use crate::body::{BodyBuilder, CombineRule, InteractionGroups};
use crate::contact::ContactPair;
use crate::joints::{BallConeLimit, HingeMotor, Joint};
use crate::narrowphase::collide_shapes;
use crate::query::ray_shape;
use crate::query::QueryFilter;
use crate::shape::Shape;
use crate::world::PhysicsWorld;

/// 一帧固定步长（与默认 `fixed_dt` 完全一致，因此每次调用恰好推进一个子步）。
const DT: f32 = 1.0 / 60.0;

/// 添加一个静态地面（法线 +Y，过原点）。
fn add_ground(world: &mut PhysicsWorld, friction: f32, restitution: f32) -> crate::BodyHandle {
    world.add_body(
        BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
            .position(Vec3::ZERO)
            .friction(friction)
            .restitution(restitution)
            .build(),
    )
}

/// 添加一个动态盒子。
fn add_box(
    world: &mut PhysicsWorld,
    half: f32,
    pos: Vec3,
    friction: f32,
    restitution: f32,
) -> crate::BodyHandle {
    world.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(half),
        })
        .position(pos)
        .friction(friction)
        .restitution(restitution)
        .build(),
    )
}

/// 断言所有刚体的状态都是有限的。
fn assert_all_finite(world: &PhysicsWorld, tag: &str) {
    for (h, b) in world.bodies() {
        assert!(
            b.position().is_finite() && b.rotation().is_finite(),
            "{tag}: 句柄 {h:?} 位姿出现 NaN/Inf"
        );
        assert!(
            b.linear_velocity().is_finite() && b.angular_velocity().is_finite(),
            "{tag}: 句柄 {h:?} 速度出现 NaN/Inf"
        );
    }
}

// ---------------------------------------------------------------------------
// 1. 自由落体
// ---------------------------------------------------------------------------

#[test]
fn free_fall_matches_analytic_then_lands() {
    // 无地面的纯自由落体：与解析解对比（一阶积分器，1 秒内偏差应很小）。
    let mut w = PhysicsWorld::new();
    let h = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
            .position(vec3(0.0, 10.0, 0.0))
            .build(),
    );
    let steps = 60;
    for _ in 0..steps {
        w.step(DT);
    }
    let y = w.body(h).unwrap().position().y;
    let analytic = 10.0 - 0.5 * 9.81 * 1.0 * 1.0;
    assert!(
        (y - analytic).abs() < 0.1,
        "1 秒后高度 {y}，解析解 {analytic}（半隐式欧拉有一阶偏差）"
    );
    let vy = w.body(h).unwrap().linear_velocity().y;
    assert!(
        (vy + 9.81).abs() < 0.05,
        "1 秒后竖直速度应为 -9.81，实际 {vy}"
    );

    // 加地面后继续下落，最终静止在 y ≈ 半径处。
    add_ground(&mut w, 0.5, 0.0);
    for _ in 0..300 {
        w.step(DT);
    }
    let y = w.body(h).unwrap().position().y;
    assert!(
        (y - 0.5).abs() < 0.5 * 0.05,
        "落地静止后高度应约等于半径，实际 {y}"
    );
    assert_all_finite(&w, "自由落体");
}

// ---------------------------------------------------------------------------
// 2. 静止接触
// ---------------------------------------------------------------------------

#[test]
fn resting_contact_does_not_sink() {
    let mut w = PhysicsWorld::new();
    add_ground(&mut w, 0.5, 0.0);
    let b = add_box(&mut w, 0.5, vec3(0.0, 0.5, 0.0), 0.5, 0.0);
    for _ in 0..180 {
        w.step(DT);
    }
    let body = w.body(b).unwrap();
    assert!(
        (body.position().y - 0.5).abs() < 0.02,
        "静止盒高度应保持 0.5，实际 {}",
        body.position().y
    );
    assert!(
        body.linear_velocity().length() < 0.05,
        "静止后速度应接近 0，实际 {:?}",
        body.linear_velocity()
    );
    assert_all_finite(&w, "静止接触");
}

// ---------------------------------------------------------------------------
// 3. 堆叠
// ---------------------------------------------------------------------------

#[test]
fn three_box_stack_stays_stacked() {
    let mut w = PhysicsWorld::new();
    add_ground(&mut w, 0.8, 0.0);
    let handles: Vec<_> = (0..3)
        .map(|i| add_box(&mut w, 0.5, vec3(0.0, 0.5 + i as f32 * 1.0, 0.0), 0.8, 0.0))
        .collect();
    // 5 秒
    for _ in 0..300 {
        w.step(DT);
    }
    assert_all_finite(&w, "堆叠");
    let mut ys = Vec::new();
    for (i, h) in handles.iter().enumerate() {
        let p = w.body(*h).unwrap().position();
        ys.push(p.y);
        assert!(
            (p.x.abs() < 0.2) && (p.z.abs() < 0.2),
            "第 {i} 层水平漂移过大：{p:?}"
        );
        let expected = 0.5 + i as f32 * 1.0;
        assert!(
            (p.y - expected).abs() < 0.1,
            "第 {i} 层高度应约为 {expected}，实际 {}",
            p.y
        );
    }
    assert!(ys[0] < ys[1] && ys[1] < ys[2], "堆叠顺序应保持：{ys:?}");
}

// ---------------------------------------------------------------------------
// 4. 恢复系数
// ---------------------------------------------------------------------------

#[test]
fn restitution_08_returns_expected_height() {
    let mut w = PhysicsWorld::new();
    add_ground(&mut w, 0.2, 0.8);
    let r = 0.5;
    let drop = 5.0;
    let h = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: r })
            .position(vec3(0.0, drop + r, 0.0))
            .restitution(0.8)
            .friction(0.2)
            .build(),
    );
    let mut impacted = false;
    let mut peak = 0.0f32;
    for _ in 0..300 {
        w.step(DT);
        let y = w.body(h).unwrap().position().y;
        if !impacted && y <= r + 0.05 {
            impacted = true;
        }
        if impacted {
            peak = peak.max(y - r);
        }
    }
    assert!(impacted, "小球应落到地面");
    // 理论回弹高度 = e² · h = 0.64 · 5 = 3.2，允许 20% 误差。
    let expected = 0.64 * drop;
    assert!(
        (peak - expected).abs() <= 0.2 * expected,
        "回弹高度应约 {expected}，实际 {peak}"
    );
    assert_all_finite(&w, "恢复系数");
}

// ---------------------------------------------------------------------------
// 5. 斜面摩擦
// ---------------------------------------------------------------------------

/// 构造 30° 斜面与静置于其上的盒子，返回（世界, 盒子, 斜面法线, 初始位置）。
fn incline_world(mu: f32) -> (PhysicsWorld, crate::BodyHandle, Vec3, Vec3) {
    let theta = 30f32.to_radians();
    let rot = Quat::from_rotation_z(theta);
    let n = rot.mul_vec3(Vec3::Y);
    let mut w = PhysicsWorld::new();
    w.add_body(
        BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
            .position(Vec3::ZERO)
            .rotation(rot)
            .friction(mu)
            .build(),
    );
    let pos = n * 0.5;
    let b = w.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.5),
        })
        .position(pos)
        .rotation(rot)
        .friction(mu)
        .build(),
    );
    (w, b, n, pos)
}

/// 沿斜面方向的滑动距离。
fn slide_distance(w: &PhysicsWorld, b: crate::BodyHandle, n: Vec3, start: Vec3) -> f32 {
    let d = w.body(b).unwrap().position() - start;
    (d - n * d.dot(n)).length()
}

#[test]
fn incline_high_friction_does_not_slide() {
    let (mut w, b, n, start) = incline_world(0.9);
    for _ in 0..120 {
        w.step(DT);
    }
    let d = slide_distance(&w, b, n, start);
    assert!(d < 0.1, "μ=0.9 时不应滑动，实际滑了 {d} 米");
    assert_all_finite(&w, "高摩擦斜面");
}

#[test]
fn incline_low_friction_slides() {
    let (mut w, b, n, start) = incline_world(0.05);
    for _ in 0..120 {
        w.step(DT);
    }
    let d = slide_distance(&w, b, n, start);
    assert!(d > 0.5, "μ=0.05 时应明显下滑，实际只滑了 {d} 米");
    assert_all_finite(&w, "低摩擦斜面");
}

// ---------------------------------------------------------------------------
// 6. 射线检测
// ---------------------------------------------------------------------------

#[test]
fn raycast_finds_nearest_body() {
    let mut w = PhysicsWorld::new();
    let near = add_box(&mut w, 0.5, vec3(2.0, 0.0, 0.0), 0.5, 0.0);
    let _far = add_box(&mut w, 0.5, vec3(5.0, 0.0, 0.0), 0.5, 0.0);
    let hit = w
        .raycast(vec3(-5.0, 0.0, 0.0), Vec3::X, 100.0)
        .expect("应命中近处的盒");
    assert_eq!(hit.handle, near);
    assert!(
        (hit.distance - 6.5).abs() < 1e-3,
        "命中距离应为 6.5，实际 {}",
        hit.distance
    );
    assert!((hit.point - vec3(1.5, 0.0, 0.0)).length() < 1e-3);
    assert!(
        hit.normal.dot(Vec3::X) < -0.9,
        "法线应朝向射线：{:?}",
        hit.normal
    );
    // 反方向应打不到。
    assert!(w
        .raycast(vec3(-5.0, 0.0, 0.0), Vec3::NEG_X, 100.0)
        .is_none());
    // 过滤掉近处的盒后应命中远处。
    let hit2 = w
        .raycast_filtered(vec3(-5.0, 0.0, 0.0), Vec3::X, 100.0, |h| h != near)
        .expect("应命中远处的盒");
    assert!((hit2.distance - 9.5).abs() < 1e-3, "距离 {}", hit2.distance);
}

// ---------------------------------------------------------------------------
// 7. 铰链摆
// ---------------------------------------------------------------------------

#[test]
fn hinge_pendulum_preserves_distance() {
    let mut w = PhysicsWorld::new();
    let anchor = w.add_body(
        BodyBuilder::r#static(Shape::Sphere { radius: 0.1 })
            .position(Vec3::ZERO)
            .build(),
    );
    let length = 2.0f32;
    let bob = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.2 })
            .position(vec3(length, 0.0, 0.0))
            .build(),
    );
    // 铰链位于锚点体中心，在摆球本地坐标下位于 -X 方向 length 处。
    w.add_joint(Joint::Hinge {
        a: anchor,
        b: bob,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: vec3(-length, 0.0, 0.0),
        local_axis_a: Vec3::Z,
        local_axis_b: Vec3::Z,
        limits: None,
        motor: None,
    });
    let mut max_err = 0.0f32;
    for _ in 0..120 {
        w.step(DT);
        let d = (w.body(bob).unwrap().position() - w.body(anchor).unwrap().position()).length();
        max_err = max_err.max((d - length).abs());
    }
    assert!(
        max_err <= 0.01 * length,
        "摆长误差 {max_err} 超过 1%（{length} 米的 {}%）",
        1.0
    );
    // 摆应确实摆动过（y 明显下降）。
    assert!(
        w.body(bob).unwrap().position().y < -0.5,
        "摆应向下摆动，实际 {:?}",
        w.body(bob).unwrap().position()
    );
    assert_all_finite(&w, "铰链摆");
}

#[test]
fn hinge_motor_drives_rotation() {
    let mut w = PhysicsWorld::new();
    let anchor = w.add_body(
        BodyBuilder::r#static(Shape::Sphere { radius: 0.1 })
            .position(Vec3::ZERO)
            .build(),
    );
    let bob = w.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.2),
        })
        .position(vec3(1.0, 0.0, 0.0))
        .build(),
    );
    w.add_joint(Joint::Hinge {
        a: anchor,
        b: bob,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: vec3(-1.0, 0.0, 0.0),
        local_axis_a: Vec3::Y,
        local_axis_b: Vec3::Y,
        limits: None,
        motor: Some(crate::joints::HingeMotor::velocity(2.0, 100.0)),
    });
    for _ in 0..30 {
        w.step(DT);
    }
    // 角速度应趋向目标（绕 Y 轴）。
    let w_bob = w.body(bob).unwrap().angular_velocity();
    assert!(w_bob.y > 0.5, "马达应驱动绕轴旋转，实际角速度 {w_bob:?}");
}

// ---------------------------------------------------------------------------
// 8. 确定性
// ---------------------------------------------------------------------------

/// 构造一个包含多种形状的确定性场景。
fn build_deterministic_scene() -> PhysicsWorld {
    let mut w = PhysicsWorld::new();
    add_ground(&mut w, 0.6, 0.1);
    add_box(&mut w, 0.5, vec3(0.0, 0.6, 0.0), 0.6, 0.1);
    add_box(&mut w, 0.5, vec3(0.05, 1.7, 0.02), 0.6, 0.1);
    w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.4 })
            .position(vec3(1.5, 2.5, -0.3))
            .linear_velocity(vec3(-0.5, 0.0, 0.25))
            .angular_velocity(vec3(0.3, 1.1, -0.7))
            .restitution(0.3)
            .friction(0.4)
            .build(),
    );
    w.add_body(
        BodyBuilder::dynamic(Shape::Capsule {
            half_height: 0.4,
            radius: 0.3,
        })
        .position(vec3(-1.2, 1.2, 0.4))
        .rotation(Quat::from_rotation_x(0.7))
        .friction(0.5)
        .build(),
    );
    w.add_body(
        BodyBuilder::dynamic(Shape::Cylinder {
            half_height: 0.3,
            radius: 0.35,
        })
        .position(vec3(0.8, 0.4, 1.4))
        .rotation(Quat::from_rotation_z(0.4))
        .friction(0.5)
        .build(),
    );
    w.add_body(
        BodyBuilder::dynamic(Shape::ConvexHull {
            points: vec![
                vec3(-0.3, -0.3, -0.3),
                vec3(0.3, -0.3, -0.3),
                vec3(0.3, 0.3, -0.3),
                vec3(-0.3, 0.3, -0.3),
                vec3(-0.3, -0.3, 0.3),
                vec3(0.3, -0.3, 0.3),
                vec3(0.3, 0.3, 0.3),
                vec3(-0.3, 0.3, 0.3),
            ],
        })
        .position(vec3(-0.6, 3.0, -1.1))
        .build(),
    );
    w
}

/// 抓取全部刚体状态用于逐位比较。
fn snapshot(w: &PhysicsWorld) -> Vec<(Vec3, Quat, Vec3, Vec3)> {
    w.bodies()
        .map(|(_, b)| {
            (
                b.position(),
                b.rotation(),
                b.linear_velocity(),
                b.angular_velocity(),
            )
        })
        .collect()
}

#[test]
fn determinism_two_worlds_are_bit_identical() {
    let mut a = build_deterministic_scene();
    let mut b = build_deterministic_scene();
    for _ in 0..120 {
        a.step(DT);
        b.step(DT);
    }
    let sa = snapshot(&a);
    let sb = snapshot(&b);
    assert_eq!(sa.len(), sb.len());
    for (i, (x, y)) in sa.iter().zip(sb.iter()).enumerate() {
        assert_eq!(x, y, "第 {i} 个刚体在第 120 步后与对照世界不一致");
    }
    assert_all_finite(&a, "确定性 A");
}

#[test]
fn determinism_survives_body_recycling() {
    // 删除并重加刚体后，两个世界仍应完全一致。
    let mut a = build_deterministic_scene();
    let mut b = build_deterministic_scene();
    let victims: Vec<_> = a.bodies().map(|(h, _)| h).collect();
    let victims_b: Vec<_> = b.bodies().map(|(h, _)| h).collect();
    a.remove_body(victims[3]);
    b.remove_body(victims_b[3]);
    let pa = vec3(0.0, 4.0, 0.0);
    let ha = a.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.25 })
            .position(pa)
            .build(),
    );
    let hb = b.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.25 })
            .position(pa)
            .build(),
    );
    assert_eq!(ha, hb, "槽位复用后的句柄应一致");
    for _ in 0..60 {
        a.step(DT);
        b.step(DT);
    }
    assert_eq!(snapshot(&a), snapshot(&b));
}

// ---------------------------------------------------------------------------
// 9. 宽相规模测试
// ---------------------------------------------------------------------------

#[test]
fn broadphase_two_hundred_bodies() {
    let mut w = PhysicsWorld::new();
    add_ground(&mut w, 0.6, 0.0);
    // 10 × 10 × 2 的网格，间距 0.9（半径 0.5，邻居轻微重叠）→ 大量接触。
    let mut count = 0;
    for x in 0..10 {
        for z in 0..10 {
            for y in 0..2 {
                add_box(
                    &mut w,
                    0.5,
                    vec3(
                        x as f32 * 0.9 - 4.0,
                        0.5 + y as f32 * 0.9,
                        z as f32 * 0.9 - 4.0,
                    ),
                    0.6,
                    0.0,
                );
                count += 1;
            }
        }
    }
    assert_eq!(count, 200);
    // 200 个盒子 + 1 个地面。
    assert_eq!(w.body_count(), 201);
    let mut max_contacts = 0usize;
    for _ in 0..30 {
        w.step(DT);
        max_contacts = max_contacts.max(w.contacts().len());
    }
    assert_all_finite(&w, "宽相 200 体");
    // 26 邻域是物理上限；同时必须远小于全配对（19900），说明宽相确实在剔除。
    assert!(
        max_contacts > 0 && max_contacts <= 200 * 27,
        "接触数量应在合理范围内，实际 {max_contacts}"
    );
    // 每个刚体不应沉到地面以下。
    for (_, b) in w.bodies() {
        assert!(b.position().y > -1.0, "刚体穿地：{:?}", b.position());
    }
}

#[test]
fn broadphase_far_apart_bodies_have_no_contacts() {
    let mut w = PhysicsWorld::new();
    for i in 0..200 {
        add_box(&mut w, 0.4, vec3(i as f32 * 3.0, 0.0, 0.0), 0.5, 0.0);
    }
    w.set_gravity(Vec3::ZERO);
    w.step(DT);
    assert!(
        w.contacts().is_empty(),
        "相距 3 米的 200 个盒不应有任何接触，实际 {}",
        w.contacts().len()
    );
}

#[test]
fn contacts_report_impulse() {
    let mut w = PhysicsWorld::new();
    add_ground(&mut w, 0.5, 0.0);
    add_box(&mut w, 0.5, vec3(0.0, 0.5, 0.0), 0.5, 0.0);
    for _ in 0..30 {
        w.step(DT);
    }
    let ContactPair {
        normal, impulse, ..
    } = w.contacts()[0];
    assert!(impulse > 0.0, "支撑冲量应为正，实际 {impulse}");
    assert!(normal.is_finite());
}

// ---------------------------------------------------------------------------
// 10. 睡眠与唤醒
// ---------------------------------------------------------------------------

#[test]
fn body_sleeps_then_wakes_on_impact() {
    let mut w = PhysicsWorld::new();
    add_ground(&mut w, 0.5, 0.0);
    let sleeper = add_box(&mut w, 0.5, vec3(0.0, 0.5, 0.0), 0.5, 0.0);
    for _ in 0..120 {
        w.step(DT);
    }
    assert!(
        w.body(sleeper).unwrap().is_sleeping(),
        "静止 2 秒后应进入睡眠"
    );
    // 从上方丢一个球砸它。
    let ball = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.4 })
            .position(vec3(0.0, 4.0, 0.0))
            .restitution(0.0)
            .build(),
    );
    let mut woke = false;
    for _ in 0..120 {
        w.step(DT);
        if !w.body(sleeper).unwrap().is_sleeping() {
            woke = true;
            break;
        }
    }
    assert!(woke, "被落体撞击后应唤醒");
    assert!(w.body(ball).unwrap().position().y > -0.5);
    assert_all_finite(&w, "睡眠唤醒");
}

// ---------------------------------------------------------------------------
// 11. 三角网格与凸包
// ---------------------------------------------------------------------------

/// 生成一个封闭长方体三角网格（薄板），缠绕与 `MeshData::box_sized` 一致。
fn box_trimesh(center: Vec3, half: Vec3) -> (Vec<Vec3>, Vec<u32>) {
    let faces = [
        (Vec3::X, Vec3::Y, Vec3::Z),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::Z, Vec3::X),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::Y, Vec3::X),
    ];
    let extent = |axis: Vec3| -> f32 {
        if axis.x.abs() > 0.5 {
            half.x
        } else if axis.y.abs() > 0.5 {
            half.y
        } else {
            half.z
        }
    };
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (n, u, v) in faces {
        let (su, sv) = (extent(u), extent(v));
        let fc = center + n * extent(n);
        let base = vertices.len() as u32;
        for (du, dv) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            vertices.push(fc + u * (du * su) + v * (dv * sv));
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    (vertices, indices)
}

#[test]
fn sphere_rests_on_thin_trimesh_slab() {
    // 薄板（厚 0.15）是「场景用模型网格当碰撞体」的典型情况：
    // 球必须在顶面上稳定停住，既不穿透也不被侧面推着横移。
    let mut w = PhysicsWorld::new();
    let (vertices, indices) = box_trimesh(vec3(0.0, 1.475, 0.0), vec3(1.2, 0.075, 1.2));
    w.add_body(
        BodyBuilder::r#static(Shape::TriMesh { vertices, indices })
            .friction(0.85)
            .build(),
    );
    let radius = 0.28f32;
    let ball = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius })
            .position(vec3(0.5, 2.55, 0.0))
            .friction(0.6)
            .restitution(0.05)
            .build(),
    );
    for _ in 0..300 {
        w.step(DT);
    }
    let p = w.body(ball).unwrap().position();
    let expected = 1.55 + radius; // 顶面 + 半径
    assert!(
        (p.y - expected).abs() < 0.03,
        "球应停在顶面 y ≈ {expected:.3}，实际 {p:?}（穿透 {:.3}）",
        expected - p.y
    );
    assert!(
        (p.x - 0.5).hypot(p.z) < 0.15,
        "球不应被侧面推着横移，实际水平位移 {:.3}",
        (p.x - 0.5).hypot(p.z)
    );
    assert_all_finite(&w, "薄板三角网格");
}

#[test]
fn sphere_rests_on_stacked_trimesh_boxes() {
    // 与示例 `model_viewer` 一致：墙 + 屋顶 + 烟囱三个长方体拼成的模型网格，
    // 球从上方落下后应停在屋顶上（高恢复系数也不应被侧面推走）。
    let mut w = PhysicsWorld::new();
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    for (center, half) in [
        (vec3(0.0, 0.7, 0.0), vec3(1.0, 0.7, 1.0)),
        (vec3(0.0, 1.475, 0.0), vec3(1.2, 0.075, 1.2)),
        (vec3(-0.7, 1.9, -0.7), vec3(0.18, 0.35, 0.18)),
    ] {
        let (v, i) = box_trimesh(center, half);
        let base = vertices.len() as u32;
        vertices.extend(v);
        indices.extend(i.into_iter().map(|x| x + base));
    }
    w.add_body(
        BodyBuilder::r#static(Shape::TriMesh { vertices, indices })
            .friction(0.85)
            .build(),
    );
    let radius = 0.28f32;
    let ball = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius })
            .position(vec3(0.54, 3.25, 0.0))
            .friction(0.6)
            .restitution(0.35)
            .build(),
    );
    for _ in 0..300 {
        w.step(DT);
    }
    let p = w.body(ball).unwrap().position();
    let expected = 1.55 + radius;
    assert!(
        (p.y - expected).abs() < 0.05,
        "球应停在屋顶 y ≈ {expected:.3}，实际 {p:?}"
    );
    assert!(
        (p.x - 0.54).hypot(p.z) < 0.2,
        "球不应被侧面推着横移，实际水平位移 {:.3}（位置 {p:?}）",
        (p.x - 0.54).hypot(p.z)
    );
    assert_all_finite(&w, "堆叠三角网格");
}

#[test]
fn trimesh_floor_and_convex_hull_rest() {
    let mut w = PhysicsWorld::new();
    // 两块三角形组成的 20×20 地板。
    w.add_body(
        BodyBuilder::r#static(Shape::TriMesh {
            vertices: vec![
                vec3(-10.0, 0.0, -10.0),
                vec3(10.0, 0.0, -10.0),
                vec3(10.0, 0.0, 10.0),
                vec3(-10.0, 0.0, 10.0),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
        })
        .build(),
    );
    let hull = w.add_body(
        BodyBuilder::dynamic(Shape::ConvexHull {
            points: vec![
                vec3(-0.4, -0.4, -0.4),
                vec3(0.4, -0.4, -0.4),
                vec3(0.4, 0.4, -0.4),
                vec3(-0.4, 0.4, -0.4),
                vec3(-0.4, -0.4, 0.4),
                vec3(0.4, -0.4, 0.4),
                vec3(0.4, 0.4, 0.4),
                vec3(-0.4, 0.4, 0.4),
            ],
        })
        .position(vec3(3.0, 2.0, 3.0))
        .friction(0.5)
        .build(),
    );
    let sphere = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
            .position(vec3(-3.0, 2.0, -3.0))
            .friction(0.5)
            .build(),
    );
    for _ in 0..300 {
        w.step(DT);
    }
    assert_all_finite(&w, "网格与凸包");
    let hy = w.body(hull).unwrap().position().y;
    assert!(hy > -0.2 && hy < 1.0, "凸包应停在地板上，实际高度 {hy}");
    let sy = w.body(sphere).unwrap().position().y;
    assert!(
        (sy - 0.5).abs() < 0.1,
        "球应停在地板上高度 ≈ 0.5，实际 {sy}"
    );
}

// ---------------------------------------------------------------------------
// 12. 防御性
// ---------------------------------------------------------------------------

#[test]
fn pathological_inputs_do_not_produce_nan() {
    let mut w = PhysicsWorld::new();
    add_ground(&mut w, 0.5, 0.0);
    // 零尺寸 / 零密度 / 巨大速度
    w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.0 })
            .position(vec3(0.0, 1.0, 0.0))
            .linear_velocity(vec3(1e6, -1e6, 0.0))
            .density(0.0)
            .build(),
    );
    w.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::ZERO,
        })
        .position(vec3(1.0, 1.0, 1.0))
        .build(),
    );
    w.add_body(
        BodyBuilder::dynamic(Shape::Capsule {
            half_height: 0.0,
            radius: 0.0,
        })
        .position(vec3(-1.0, 1.0, 0.0))
        .build(),
    );
    for _ in 0..300 {
        w.step(DT);
    }
    assert_all_finite(&w, "异常输入");
    // 极小/极大 dt 也不能破坏状态。
    w.step(0.0);
    w.step(-1.0);
    w.step(f32::NAN);
    w.step(1e-6);
    w.step(1e6);
    assert_all_finite(&w, "异常 dt");
}

// ---------------------------------------------------------------------------
// 13. 其余关节类型
// ---------------------------------------------------------------------------

/// 静态锚点体（原点处的小球）。
fn add_anchor(w: &mut PhysicsWorld) -> crate::BodyHandle {
    w.add_body(
        BodyBuilder::r#static(Shape::Sphere { radius: 0.1 })
            .position(Vec3::ZERO)
            .build(),
    )
}

#[test]
fn ball_joint_keeps_anchor_points_together() {
    let mut w = PhysicsWorld::new();
    let anchor = add_anchor(&mut w);
    let length = 1.5f32;
    // 摆球中心在 (0,-length,0)；其本地锚点指向世界原点。
    let bob = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.2 })
            .position(vec3(0.0, -length, 0.0))
            .build(),
    );
    w.add_joint(Joint::Ball {
        a: anchor,
        b: bob,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: vec3(0.0, length, 0.0),
        cone_limit: None,
    });
    let mut max_gap = 0.0f32;
    for _ in 0..180 {
        w.step(DT);
        let p = w.body(bob).unwrap();
        let world_anchor = p.position() + p.rotation().mul_vec3(vec3(0.0, length, 0.0));
        max_gap = max_gap.max(world_anchor.length());
    }
    assert!(max_gap < 0.02, "球窝关节锚点最大偏差 {max_gap} 过大");
    assert_all_finite(&w, "球窝关节");
}

#[test]
fn fixed_joint_welds_bodies() {
    let mut w = PhysicsWorld::new();
    let base = add_anchor(&mut w);
    let offset = vec3(1.0, 0.0, 0.0);
    let body = w.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.2),
        })
        .position(offset)
        .build(),
    );
    w.add_joint(Joint::Fixed {
        a: base,
        b: body,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: -offset,
        local_frame_a: Quat::IDENTITY,
        local_frame_b: Quat::IDENTITY,
    });
    for _ in 0..180 {
        w.step(DT);
    }
    let p = w.body(body).unwrap().position();
    assert!(
        (p - offset).length() < 0.05,
        "固定关节应保持相对位置，实际漂移到 {p:?}"
    );
    assert_all_finite(&w, "固定关节");
}

#[test]
fn distance_joint_holds_rest_length() {
    let mut w = PhysicsWorld::new();
    let anchor = add_anchor(&mut w);
    let rest = 2.0f32;
    let body = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.2 })
            .position(vec3(rest, 0.0, 0.0))
            .build(),
    );
    w.add_joint(Joint::Distance {
        a: anchor,
        b: body,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: Vec3::ZERO,
        rest_length: rest,
        stiffness: 0.0,
        damping: 0.0,
    });
    let mut max_err = 0.0f32;
    for _ in 0..180 {
        w.step(DT);
        let d = (w.body(body).unwrap().position() - w.body(anchor).unwrap().position()).length();
        max_err = max_err.max((d - rest).abs());
    }
    assert!(max_err < 0.05 * rest, "距离关节长度误差 {max_err} 超过 5%");
    assert_all_finite(&w, "距离关节");
}

#[test]
fn slider_joint_confines_motion_to_axis() {
    let mut w = PhysicsWorld::new();
    let anchor = add_anchor(&mut w);
    // 滑轴为 X：把滑块沿轴放到 x = 2，本地锚点指回原点，避免与锚点体几何重叠。
    let start = 2.0f32;
    let body = w.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.2),
        })
        .position(vec3(start, 0.0, 0.0))
        .linear_velocity(vec3(1.0, 3.0, -2.0))
        .build(),
    );
    w.add_joint(Joint::Slider {
        a: anchor,
        b: body,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: vec3(-start, 0.0, 0.0),
        local_axis_a: Vec3::X,
        local_axis_b: Vec3::X,
        limits: None,
    });
    for _ in 0..120 {
        w.step(DT);
    }
    let p = w.body(body).unwrap().position();
    assert!(
        p.y.abs() < 0.02 && p.z.abs() < 0.02,
        "滑块关节应约束垂直方向，实际 {p:?}"
    );
    // 120 步 = 2 秒，轴向初速度 1 m/s → 应自由滑动约 2 米。
    assert!(
        (p.x - (start + 2.0)).abs() < 0.1,
        "沿轴方向应自由滑动到 ≈{}，实际 {}",
        start + 2.0,
        p.x
    );
    assert_all_finite(&w, "滑块关节");
}

#[test]
fn slider_limits_stop_axial_travel() {
    let mut w = PhysicsWorld::new();
    let anchor = add_anchor(&mut w);
    // 滑块初始位于锚点正上方 1 米（本地锚点指回锚点原点），轴向限位 [-0.5, 0.5]。
    // 重力会把它往下拉，限位应当把它卡在 0.5 米处，而不是任其自由落体。
    let start = vec3(0.0, 1.0, 0.0);
    let body = w.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.2),
        })
        .position(start)
        .build(),
    );
    w.add_joint(Joint::Slider {
        a: anchor,
        b: body,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: -start,
        local_axis_a: Vec3::Y,
        local_axis_b: Vec3::Y,
        limits: Some((-0.5, 0.5)),
    });
    for _ in 0..240 {
        w.step(DT);
    }
    let p = w.body(body).unwrap().position();
    // 下界 0.5 → 世界高度不应低于 0.5（留一点数值余量）
    assert!(
        p.y > 0.45 && p.y < 0.75,
        "轴向限位应把滑块卡在 y ≈ 0.5，实际 {p:?}"
    );
    assert!(
        p.x.abs() < 0.02 && p.z.abs() < 0.02,
        "垂直轴向应被锁住: {p:?}"
    );
    assert_all_finite(&w, "滑块限位");
}

#[test]
fn hinge_limits_restrict_angle() {
    let mut w = PhysicsWorld::new();
    let anchor = add_anchor(&mut w);
    let length = 1.0f32;
    let bob = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.2 })
            .position(vec3(length, 0.0, 0.0))
            .build(),
    );
    // 限位 [-0.2, 0.2] 弧度：摆应被卡在很小的角度内。
    w.add_joint(Joint::Hinge {
        a: anchor,
        b: bob,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: vec3(-length, 0.0, 0.0),
        local_axis_a: Vec3::Z,
        local_axis_b: Vec3::Z,
        limits: Some((-0.2, 0.2)),
        motor: None,
    });
    let mut min_angle = 0.0f32;
    for _ in 0..120 {
        w.step(DT);
        let p = w.body(bob).unwrap().position();
        min_angle = min_angle.min(p.y.atan2(p.x));
    }
    assert!(
        min_angle > -0.35,
        "限位应把摆角限制在 -0.2 附近，实际最小角 {min_angle}"
    );
    assert_all_finite(&w, "铰链限位");
}

// ---------------------------------------------------------------------------
// 14. 长时间运行稳定性（75 个盒子 5 秒）
// ---------------------------------------------------------------------------

#[test]
fn long_run_settles_and_stays_stable() {
    let mut w = PhysicsWorld::new();
    add_ground(&mut w, 0.7, 0.0);
    let mut count = 0;
    // 5 × 5 × 3 塔群，间距略大于边长，初始无穿透。
    for x in 0..5 {
        for z in 0..5 {
            for y in 0..3 {
                add_box(
                    &mut w,
                    0.5,
                    vec3(
                        x as f32 * 1.02 - 2.0,
                        0.5 + y as f32 * 1.02,
                        z as f32 * 1.02 - 2.0,
                    ),
                    0.7,
                    0.0,
                );
                count += 1;
            }
        }
    }
    for _ in 0..300 {
        w.step(DT);
    }
    assert_all_finite(&w, "长时间运行");
    let mut sleeping = 0;
    for (_, b) in w.bodies() {
        if b.is_sleeping() {
            sleeping += 1;
        }
        // 地面本体位于 y = 0；其余刚体不应沉入地面。
        if b.is_dynamic() {
            assert!(b.position().y > 0.3, "刚体沉入地面：{:?}", b.position());
        }
    }
    assert!(
        sleeping * 10 >= count * 9,
        "5 秒后应几乎全部入睡，实际 {sleeping}/{count}"
    );
}

#[test]
fn moving_kinematic_body_wakes_and_pushes_a_sleeping_body() {
    // 回归：宽相筛选曾经写成 `dynamic = body_type == Dynamic && !sleeping`，
    // 于是「运动学体 × 睡眠刚体」被当成两个不可动体、连候选对都不生成 →
    // 接触与唤醒逻辑永远走不到，睡眠的箱子会被推板无声穿过。
    let mut w = PhysicsWorld::new();
    w.set_gravity(vec3(0.0, -9.81, 0.0));
    add_ground(&mut w, 0.9, 0.0);
    let target = add_box(&mut w, 0.5, vec3(0.0, 0.5, 0.0), 0.9, 0.0);

    // 先站着不动，让它进入睡眠
    for _ in 0..600 {
        w.step(DT);
    }
    assert!(
        w.body(target).unwrap().is_sleeping(),
        "箱子应当先睡着（前置条件），否则这个用例证明不了唤醒"
    );
    let before = w.body(target).unwrap().position();

    // 运动学推板从左往右扫过（运动学体靠速度推进，质量无穷大）
    w.add_body(
        BodyBuilder::kinematic(Shape::Box {
            half_extents: vec3(0.2, 0.5, 0.5),
        })
        .position(vec3(-2.5, 0.5, 0.0))
        .friction(0.8)
        .linear_velocity(vec3(3.0, 0.0, 0.0))
        .build(),
    );
    for _ in 0..180 {
        w.step(DT);
    }

    let body = w.body(target).unwrap();
    let after = body.position();
    assert!(
        !body.is_sleeping() || after.x > before.x + 0.2,
        "睡眠体必须被唤醒或被推动（before={before:?} after={after:?}）"
    );
    assert!(
        after.x > before.x + 0.2,
        "睡眠的箱子应当被运动学推板推动：{before:?} → {after:?}"
    );
    assert_all_finite(&w, "运动学推动");
}

#[test]
fn kinematic_body_at_rest_does_not_wake_sleepers() {
    // 反向约束：静止的运动学体不该把整片睡眠世界唤醒（否则睡眠优化形同虚设）
    let mut w = PhysicsWorld::new();
    w.set_gravity(vec3(0.0, -9.81, 0.0));
    add_ground(&mut w, 0.9, 0.0);
    let target = add_box(&mut w, 0.5, vec3(0.0, 0.5, 0.0), 0.9, 0.0);
    // 远处一个不动的运动学体
    w.add_body(
        BodyBuilder::kinematic(Shape::Box {
            half_extents: Vec3::splat(0.5),
        })
        .position(vec3(20.0, 0.5, 0.0))
        .build(),
    );
    for _ in 0..600 {
        w.step(DT);
    }
    assert!(
        w.body(target).unwrap().is_sleeping(),
        "不动的运动学体不该阻止旁边的箱子入睡"
    );
}

// ---------------------------------------------------------------------------
// 13. 碰撞过滤、传感器、CCD
// ---------------------------------------------------------------------------

/// 分组不匹配的一对刚体**连接触都不产生**，动了的那一个会穿过去。
#[test]
fn groups_filter_contacts_between_layers() {
    // 地面只在第 0 层、且只碰第 2 层；盒子在第 1 层、愿意碰所有层。
    // 盒子愿意没用——地面不愿意，这一对就是不碰（对称匹配）。
    let mut w2 = PhysicsWorld::new();
    let _ground = w2.add_body(
        BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
            .layer(0)
            .collides_with(1 << 2) // 只和第 2 层碰
            .build(),
    );
    let b2 = w2.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.5),
        })
        .position(vec3(0.0, 0.5, 0.0))
        .layer(1)
        .collides_with(InteractionGroups::ALL)
        .build(),
    );
    for _ in 0..60 {
        w2.step(DT);
    }
    let y = w2.body(b2).unwrap().position().y;
    assert!(
        y < -1.0,
        "分组不匹配时盒子应当穿过地面掉下去，实际停在 y = {y}"
    );
    assert!(
        w2.contacts().is_empty(),
        "分组不匹配的一对不该产生接触：{:?}",
        w2.contacts().len()
    );

    // 对照组：同层且都愿意 → 停在半高上（回归：过滤不能把正常碰撞也滤掉）
    let mut w3 = PhysicsWorld::new();
    let _ground = w3.add_body(
        BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
            .layer(0)
            .build(),
    );
    let b3 = w3.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.5),
        })
        .position(vec3(0.0, 0.5, 0.0))
        .layer(0)
        .build(),
    );
    for _ in 0..30 {
        w3.step(DT);
    }
    let y = w3.body(b3).unwrap().position().y;
    assert!((y - 0.5).abs() < 0.05, "对照组应当停在 y ≈ 0.5，实际 {y}");
    assert_all_finite(&w3, "分组对照组");
}

#[test]
fn interaction_groups_are_symmetric() {
    let a = InteractionGroups::new(1 << 0, 1 << 1);
    let b = InteractionGroups::new(1 << 1, 1 << 0);
    assert!(a.interacts(b) && b.interacts(a));
    // 单边愿意 → 不碰：c（第 2 层）愿意碰所有层，但 a 的 filter 里没有第 2 层
    let c = InteractionGroups::new(1 << 2, InteractionGroups::ALL);
    assert!(!a.interacts(c) && !c.interacts(a));
    // 缺省 = 全都在同一层且都愿意
    assert!(InteractionGroups::default().interacts(InteractionGroups::default()));
    // filter = 0 什么都不碰
    assert!(!InteractionGroups::NONE.interacts(InteractionGroups::default()));
}

/// 传感器：只报"进入/离开"，不产生任何冲量，物体直接穿过去。
#[test]
fn sensor_reports_enter_and_leave_without_impulse() {
    let mut w = PhysicsWorld::with_gravity(vec3(0.0, -9.81, 0.0));
    // 传感器是静态的薄盒子（触发区），摆在 y = 0 处
    let sensor = w.add_body(
        BodyBuilder::r#static(Shape::Box {
            half_extents: vec3(1.0, 0.1, 1.0),
        })
        .position(vec3(0.0, 0.0, 0.0))
        .sensor(true)
        .build(),
    );
    // 从上方自由落体穿过它
    let faller = w.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.2),
        })
        .position(vec3(0.0, 1.5, 0.0))
        .build(),
    );
    let mut entered = false;
    let mut left = false;
    let mut max_impulse = 0.0f32;
    for _ in 0..120 {
        w.step(DT);
        for e in w.sensor_events() {
            assert_eq!(e.sensor, sensor, "事件里的传感器句柄不对");
            assert_eq!(e.other, faller);
            if e.started {
                entered = true;
            } else {
                left = true;
            }
        }
        for c in w.contacts() {
            if c.sensor {
                max_impulse = max_impulse.max(c.impulse);
            }
        }
    }
    assert!(entered, "物体穿过传感器时应当报一次进入");
    assert!(left, "物体离开传感器时应当报一次离开");
    assert_eq!(max_impulse, 0.0, "传感器不该产生任何冲量");
    let y = w.body(faller).unwrap().position().y;
    assert!(y < -2.0, "物体应当毫无阻碍地穿过传感器，实际停在 y = {y}");
    // 进去过又出来了，稳态下不该再有事件
    assert!(
        w.sensor_events().is_empty(),
        "物体已经离开，不该还在报事件：{:?}",
        w.sensor_events()
    );
    assert!(w.sensor_pairs().is_empty());
    assert_all_finite(&w, "传感器");
}

/// 传感器不唤醒睡眠体（一个巨大的触发区不该把整个世界叫醒）。
#[test]
fn sensor_does_not_wake_sleeping_bodies() {
    let mut w = PhysicsWorld::new();
    // 地面上放一个盒子，等它睡着
    let _ground = add_ground(&mut w, 0.5, 0.0);
    let box_h = add_box(&mut w, 0.5, vec3(0.0, 0.5, 0.0), 0.5, 0.0);
    for _ in 0..600 {
        w.step(DT);
    }
    assert!(w.body(box_h).unwrap().is_sleeping(), "前提：盒子先得睡着");
    // 运动学传感器扫过它
    let sensor = w.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: vec3(0.5, 0.5, 0.5),
        })
        .position(vec3(-3.0, 0.5, 0.0))
        .linear_velocity(vec3(2.0, 0.0, 0.0))
        .sensor(true)
        .build(),
    );
    for _ in 0..60 {
        w.step(DT);
    }
    let _ = sensor;
    assert!(
        w.body(box_h).unwrap().is_sleeping(),
        "传感器经过时不该把睡着的盒子叫醒"
    );
}

/// CCD 贴着地面滑行时也得挡住薄墙（回归：保守推进扫掠会被脚下的地面"拖住"）。
///
/// 这个用例是真正的坑：没有地面时扫掠能找到墙，加了地面之后——地面永远在正下方
/// 0.1 米、"最近物体"永远是地面，扫掠每步只推进 0.1 米，而且返回的"命中"是地面
/// （法线与运动垂直 → 不该拦），于是子弹把墙穿了过去。现在用中心射线，只报
/// **沿运动方向**的命中，一次查询就够。
#[test]
fn ccd_works_while_skimming_a_ground_plane() {
    let mut w = PhysicsWorld::with_gravity(Vec3::ZERO);
    // 地面：无限半空间，就在子弹正下方 0.2 米
    w.add_body(
        BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
            .position(Vec3::ZERO)
            .build(),
    );
    w.add_body(
        BodyBuilder::r#static(Shape::Box {
            half_extents: vec3(0.05, 2.0, 2.0),
        })
        .position(vec3(-5.0, 0.2, 0.0))
        .build(),
    );
    let ball = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.1 })
            .position(vec3(-10.0, 0.2, 0.0))
            .linear_velocity(vec3(600.0, 0.0, 0.0))
            .gravity_scale(0.0)
            .ccd(true)
            .build(),
    );
    for _ in 0..30 {
        w.step(DT);
    }
    let x = w.body(ball).unwrap().position().x;
    assert!(
        x <= -5.0,
        "贴着地面滑行的 CCD 子弹必须被薄墙拦住，实际 x = {x}"
    );
}

/// CCD 也要遵守碰撞分组：分组不匹配的墙，扫掠时就得当作不存在。
#[test]
fn ccd_respects_collision_groups() {
    let mut w = PhysicsWorld::with_gravity(Vec3::ZERO);
    // 墙只在第 5 层、且只碰第 5 层；子弹在第 0 层 → 互相不碰
    let _wall = w.add_body(
        BodyBuilder::r#static(Shape::Box {
            half_extents: vec3(0.05, 2.0, 2.0),
        })
        .position(vec3(-9.0, 0.0, 0.0))
        .layer(5)
        .collides_with(1 << 5)
        .build(),
    );
    let ball = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.1 })
            .position(vec3(-14.0, 0.0, 0.0))
            .linear_velocity(vec3(600.0, 0.0, 0.0))
            .gravity_scale(0.0)
            .ccd(true)
            .build(),
    );
    for _ in 0..30 {
        w.step(DT);
    }
    let x = w.body(ball).unwrap().position().x;
    assert!(
        x > 0.0,
        "CCD 不该被分组不匹配的墙拦住（说明扫掠没查分组），实际 x = {x}"
    );
}

/// CCD：高速小球撞薄墙——不开会穿过去，开了会停在墙前（并且之后不穿模）。
#[test]
fn ccd_stops_a_fast_body_at_a_thin_wall() {
    // 薄墙：0.1 厚（半厚 0.05），立在 x = 0
    let make = |ccd: bool| {
        let mut w = PhysicsWorld::with_gravity(Vec3::ZERO);
        let wall = w.add_body(
            BodyBuilder::r#static(Shape::Box {
                half_extents: vec3(0.05, 2.0, 2.0),
            })
            .position(vec3(0.0, 0.0, 0.0))
            .build(),
        );
        let ball = w.add_body(
            BodyBuilder::dynamic(Shape::Sphere { radius: 0.1 })
                .position(vec3(-5.0, 0.0, 0.0))
                .linear_velocity(vec3(600.0, 0.0, 0.0))
                .ccd(ccd)
                .build(),
        );
        (w, wall, ball)
    };

    // ① 不开 CCD：一步 10 米，直接跳到墙的另一边（墙只有 0.1 厚）
    let (mut w, _wall, ball) = make(false);
    w.step(DT);
    let x_no_ccd = w.body(ball).unwrap().position().x;
    assert!(
        x_no_ccd > 0.15,
        "前提：不开 CCD 时这一步应当已经穿过薄墙（一步 10 米），实际 x = {x_no_ccd}"
    );

    // ② 开 CCD：停在墙的 -x 一侧（球心 ≤ 墙面 - 半径 = -0.15）
    let (mut w2, _wall2, ball2) = make(true);
    for _ in 0..10 {
        w2.step(DT);
    }
    let x_ccd = w2.body(ball2).unwrap().position().x;
    assert!(
        x_ccd <= -0.14,
        "开了 CCD 不该穿墙，实际 x = {x_ccd}（墙面在 -0.05，球半径 0.1）"
    );
    assert!(
        x_ccd >= -0.6,
        "开了 CCD 也不该在离墙半米外就停下（说明扫掠把速度截得太狠），实际 x = {x_ccd}"
    );
    assert_all_finite(&w2, "CCD");

    // ③ 长时间跑下去也不能穿过去（每一步都扫）
    for _ in 0..240 {
        w2.step(DT);
    }
    let x_late = w2.body(ball2).unwrap().position().x;
    assert!(x_late <= -0.14, "跑 4 秒后仍然不该穿墙，实际 x = {x_late}");
}

// ---------------------------------------------------------------------------
// 6b. 查询补全（所有命中 / 任意形状重叠 / 层过滤）与查询加速结构
// ---------------------------------------------------------------------------

/// 沿 +X 摆三个盒子：`raycast_all` 要按距离升序给全，`raycast` 只给最近的。
#[test]
fn raycast_all_reports_every_hit_in_order() {
    let mut w = PhysicsWorld::new();
    let a = add_box(&mut w, 0.5, vec3(2.0, 0.0, 0.0), 0.5, 0.0);
    let b = add_box(&mut w, 0.5, vec3(5.0, 0.0, 0.0), 0.5, 0.0);
    let c = add_box(&mut w, 0.5, vec3(8.0, 0.0, 0.0), 0.5, 0.0);
    let origin = vec3(-5.0, 0.0, 0.0);

    let all = w.raycast_all(origin, Vec3::X, 100.0);
    assert_eq!(all.len(), 3, "三个盒子都该被命中：{all:?}");
    assert_eq!(
        all.iter().map(|h| h.handle).collect::<Vec<_>>(),
        vec![a, b, c],
        "必须按距离升序（也就是摆放顺序）"
    );
    for (i, hit) in all.iter().enumerate() {
        let expected = 6.5 + 3.0 * i as f32;
        assert!(
            (hit.distance - expected).abs() < 1e-3,
            "第 {i} 个命中距离应为 {expected}，实际 {}",
            hit.distance
        );
    }
    // `raycast` 与 `raycast_all().first()` 必须一致
    let nearest = w.raycast(origin, Vec3::X, 100.0).expect("应命中");
    assert_eq!(nearest.handle, a);
    assert!((nearest.distance - all[0].distance).abs() < 1e-6);

    // 距离上限要截断
    let near_only = w.raycast_all(origin, Vec3::X, 7.0);
    assert_eq!(near_only.len(), 1, "7 米内只有第一个盒子：{near_only:?}");
    // 反方向什么都没有
    assert!(w.raycast_all(origin, Vec3::NEG_X, 100.0).is_empty());
    // 退化输入不 panic
    assert!(w.raycast_all(origin, Vec3::ZERO, 100.0).is_empty());
    assert!(w.raycast_all(origin, Vec3::X, -1.0).is_empty());
    assert!(w
        .raycast_all(Vec3::splat(f32::NAN), Vec3::X, 10.0)
        .is_empty());
}

/// 任意形状重叠：不只是球；结果按槽位升序（与线性遍历一致）。
#[test]
fn overlap_shape_supports_boxes_and_ordering() {
    let mut w = PhysicsWorld::new();
    let a = add_box(&mut w, 0.5, vec3(0.0, 0.0, 0.0), 0.5, 0.0);
    let b = add_box(&mut w, 0.5, vec3(1.5, 0.0, 0.0), 0.5, 0.0);
    let c = add_box(&mut w, 0.5, vec3(9.0, 0.0, 0.0), 0.5, 0.0);

    // 一个 2.2 米宽的探针盒：盖住 a 与 b，碰不到 c
    let probe = Shape::Box {
        half_extents: Vec3::new(1.1, 0.6, 0.6),
    };
    let hits = w.overlap_shape(&probe, &Transform::from_translation(vec3(0.7, 0.0, 0.0)));
    assert_eq!(hits, vec![a, b], "应命中前两个盒子且按槽位升序");
    assert!(!hits.contains(&c));

    // 球探针与 `overlap_sphere`（老 API）结果一致——两者现在走同一条实现
    let probe = Shape::Sphere { radius: 1.0 };
    let t = Transform::from_translation(vec3(0.0, 0.0, 0.0));
    assert_eq!(
        w.overlap_shape(&probe, &t),
        crate::overlap_sphere(&w, vec3(0.0, 0.0, 0.0), 1.0)
    );

    // 非法输入
    assert!(w
        .overlap_shape(&probe, &Transform::from_translation(Vec3::splat(f32::NAN)))
        .is_empty());
}

/// 层掩码是**单向**的：只看被查方是不是掩码里的层；传感器可以按需排除。
#[test]
fn query_filter_masks_by_layer_and_sensors() {
    let mut w = PhysicsWorld::new();
    let layer0 = add_box(&mut w, 0.5, vec3(2.0, 0.0, 0.0), 0.5, 0.0);
    let layer1 = w.add_body(
        BodyBuilder::r#static(Shape::Box {
            half_extents: Vec3::splat(0.5),
        })
        .position(vec3(5.0, 0.0, 0.0))
        .layer(1)
        .build(),
    );
    let trigger = w.add_body(
        BodyBuilder::r#static(Shape::Box {
            half_extents: Vec3::splat(0.5),
        })
        .position(vec3(8.0, 0.0, 0.0))
        .layer(2)
        .sensor(true)
        .build(),
    );
    let origin = vec3(-5.0, 0.0, 0.0);

    let all: Vec<_> = w
        .raycast_all(origin, Vec3::X, 100.0)
        .into_iter()
        .map(|h| h.handle)
        .collect();
    assert_eq!(all, vec![layer0, layer1, trigger], "缺省不过滤");

    let only1: Vec<_> = w
        .raycast_all_filtered(origin, Vec3::X, 100.0, QueryFilter::default().mask(1 << 1))
        .into_iter()
        .map(|h| h.handle)
        .collect();
    assert_eq!(only1, vec![layer1], "只查第 1 层");

    // 单向：掩码里给"两个层"，两个都算；不像物理那样要双方都愿意
    let both: Vec<_> = w
        .raycast_all_filtered(origin, Vec3::X, 100.0, QueryFilter::default().mask(0b11))
        .into_iter()
        .map(|h| h.handle)
        .collect();
    assert_eq!(both, vec![layer0, layer1]);

    // 排除传感器
    let no_sensor: Vec<_> = w
        .raycast_all_filtered(
            origin,
            Vec3::X,
            100.0,
            QueryFilter::default().sensors(false),
        )
        .into_iter()
        .map(|h| h.handle)
        .collect();
    assert_eq!(no_sensor, vec![layer0, layer1]);

    // 掩码为 0：什么都不查
    assert!(w
        .raycast_all_filtered(origin, Vec3::X, 100.0, QueryFilter::default().mask(0))
        .is_empty());
    // `raycast_in` 与 `raycast_all_filtered` 的第一个一致
    let hit = w
        .raycast_in(origin, Vec3::X, 100.0, QueryFilter::default().mask(1 << 1))
        .expect("应命中第 1 层");
    assert_eq!(hit.handle, layer1);
}

/// 加速结构：结果与线性扫描**逐条一致**，而且确实少测了刚体。
#[test]
fn query_index_matches_linear_scan() {
    let mut w = PhysicsWorld::new();
    // 6×6×3 的静态盒阵（留出空隙），外加一个半空间地面与一个大地板盒
    let mut expected = Vec::new();
    for ix in 0..6 {
        for iz in 0..6 {
            for iy in 0..3 {
                let pos = vec3(
                    -7.5 + ix as f32 * 3.0,
                    1.0 + iy as f32 * 2.5,
                    -7.5 + iz as f32 * 3.0,
                );
                expected.push(
                    w.add_body(
                        BodyBuilder::r#static(Shape::Box {
                            half_extents: Vec3::splat(0.6),
                        })
                        .position(pos)
                        .build(),
                    ),
                );
            }
        }
    }
    add_ground(&mut w, 0.5, 0.0);
    // 一个横跨很多单元的大盒子（会自动升级成"全局代理"）
    w.add_body(
        BodyBuilder::r#static(Shape::Box {
            half_extents: vec3(40.0, 0.05, 40.0),
        })
        .position(vec3(0.0, -0.5, 0.0))
        .build(),
    );

    let index = w.build_query_index();
    assert!(!index.is_stale(&w), "刚建好的索引不该是过期的");
    assert_eq!(index.len(), w.body_count());
    assert!(index.cells() > 0, "静态场景也要铺出单元来");

    let filter = QueryFilter::default();
    let rays = [
        (vec3(0.0, 5.0, 0.0), vec3(0.0, -1.0, 0.0)),
        (vec3(0.0, 1.0, -20.0), vec3(0.0, 0.0, 1.0)),
        (vec3(-20.0, 3.0, 0.0), Vec3::X),
        (vec3(0.0, 0.3, 0.0), Vec3::X),
        (vec3(3.0, 1.0, 3.0), vec3(1.0, 1.0, 1.0)),
    ];
    let mut pruned_somewhere = false;
    for (origin, dir) in rays {
        let linear_all = w.raycast_all(origin, dir, 60.0);
        let fast = index.raycast_detailed(&w, origin, dir, 60.0, filter, true);
        assert!(!fast.fallback, "索引是新的，不该回退");
        assert_eq!(
            fast.hits.len(),
            linear_all.len(),
            "命中数不一致：origin={origin:?} dir={dir:?}\n线性={linear_all:?}\n索引={:?}",
            fast.hits
        );
        for (a, b) in fast.hits.iter().zip(linear_all.iter()) {
            assert_eq!(a.handle, b.handle, "句柄顺序不一致");
            assert!((a.distance - b.distance).abs() < 1e-4);
            assert!((a.point - b.point).length() < 1e-3);
        }
        // 最近命中也得一致
        let linear_one = w.raycast(origin, dir, 60.0);
        let fast_one = index.raycast(&w, origin, dir, 60.0, filter);
        assert_eq!(
            linear_one.map(|h| h.handle),
            fast_one.map(|h| h.handle),
            "最近命中不一致"
        );
        if fast.tested < w.body_count() {
            pruned_somewhere = true;
        }
    }
    assert!(
        pruned_somewhere,
        "至少有一条射线该少测几个刚体，否则说明索引根本没起作用"
    );

    // 任意形状重叠也要一致（球 + 盒两种探针）
    for (shape, at) in [
        (Shape::Sphere { radius: 2.0 }, vec3(0.0, 1.0, 0.0)),
        (
            Shape::Box {
                half_extents: vec3(3.0, 0.5, 3.0),
            },
            vec3(-3.0, 1.2, 0.0),
        ),
    ] {
        let t = Transform::from_translation(at);
        let linear = w.overlap_shape(&shape, &t);
        let fast = index.overlap_shape_detailed(&w, &shape, &t, filter);
        assert!(!fast.fallback);
        assert_eq!(fast.handles, linear, "重叠结果不一致（at={at:?}）");
        assert!(!linear.is_empty(), "这个探针本来就该命中东西");
    }
}

/// 索引是快照：世界动过它就过期，此时**自动回退**到线性扫描（结果仍然正确）。
#[test]
fn query_index_falls_back_when_stale() {
    let mut w = PhysicsWorld::new();
    let ball = add_box(&mut w, 0.5, vec3(2.0, 0.0, 0.0), 0.5, 0.0);
    let index = w.build_query_index();
    assert!(index
        .raycast(
            &w,
            vec3(-5.0, 0.0, 0.0),
            Vec3::X,
            100.0,
            QueryFilter::default()
        )
        .is_some());

    // 把球挪走（`body_mut` 会让版本号 +1）
    w.body_mut(ball).unwrap().transform.position = vec3(2.0, 50.0, 0.0);
    assert!(index.is_stale(&w), "动过之后索引必须自知过期");
    let stale = index.raycast_detailed(
        &w,
        vec3(-5.0, 0.0, 0.0),
        Vec3::X,
        100.0,
        QueryFilter::default(),
        true,
    );
    assert!(stale.fallback, "过期索引必须回退到线性扫描");
    assert!(
        stale.hits.is_empty(),
        "球已经不在射线上了：{:?}",
        stale.hits
    );

    // 重建之后又走索引路径，结果与线性一致
    let index = w.build_query_index();
    assert!(!index.is_stale(&w));
    let fresh = index.raycast_detailed(
        &w,
        vec3(-5.0, 0.0, 0.0),
        Vec3::X,
        100.0,
        QueryFilter::default(),
        true,
    );
    assert!(!fresh.fallback);
    assert_eq!(
        fresh.hits,
        w.raycast_all(vec3(-5.0, 0.0, 0.0), Vec3::X, 100.0)
    );

    // `step` 也会让索引过期（子步改了变换）
    let index = w.build_query_index();
    w.step(DT);
    assert!(index.is_stale(&w), "步进之后索引必须过期");
}

/// 索引也要处理"单个刚体横跨很多单元"与掩码过滤。
#[test]
fn query_index_handles_multi_cell_bodies_and_masks() {
    let mut w = PhysicsWorld::new();
    // 一块很长的地板：横跨无数单元（超过单个代理的单元上限 → 全局代理）
    w.add_body(
        BodyBuilder::r#static(Shape::Box {
            half_extents: vec3(30.0, 0.1, 30.0),
        })
        .position(vec3(0.0, -1.0, 0.0))
        .build(),
    );
    let target = w.add_body(
        BodyBuilder::r#static(Shape::Box {
            half_extents: Vec3::splat(0.5),
        })
        .position(vec3(4.0, 0.4, 0.0))
        .layer(3)
        .build(),
    );
    let index = w.build_query_index();
    assert!(index.globals() >= 1, "超大地板该升级成全局代理");
    assert_eq!(index.len(), 2);

    // 从上往下打：先撞到 layer 3 的盒子，再撞地板
    let origin = vec3(4.0, 20.0, 0.0);
    let down = vec3(0.0, -1.0, 0.0);
    let all = index.raycast_all(&w, origin, down, 100.0, QueryFilter::default());
    assert_eq!(all.len(), 2, "地板与盒子都该命中：{all:?}");
    assert_eq!(all[0].handle, target, "最近的应是盒子");
    assert_eq!(all, w.raycast_all(origin, down, 100.0));

    // 只看第 3 层：地板（第 0 层）被过滤掉
    let filtered = index.raycast_all(&w, origin, down, 100.0, QueryFilter::default().mask(1 << 3));
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].handle, target);
    assert_eq!(
        filtered,
        w.raycast_all_filtered(origin, down, 100.0, QueryFilter::default().mask(1 << 3))
    );
}

#[test]
fn wake_up_all_wakes_every_sleeper_and_manual_sleep_clears_velocity() {
    // **零重力** + 静止：不碰任何东西，30 步（0.5 秒）后就该睡着。
    let mut w = PhysicsWorld::with_gravity(Vec3::ZERO);
    let a = add_box(&mut w, 0.5, vec3(0.0, 0.0, 0.0), 0.5, 0.0);
    let b = add_box(&mut w, 0.5, vec3(4.0, 0.0, 0.0), 0.5, 0.0);
    for _ in 0..60 {
        w.step(DT);
    }
    assert!(w.body(a).unwrap().is_sleeping(), "前提：a 先睡着");
    assert!(w.body(b).unwrap().is_sleeping(), "前提：b 先睡着");

    // 全醒：返回"唤醒了几个"，而不是默默什么都不做
    let woken = w.wake_up_all();
    assert_eq!(woken, 2, "两个都睡着，就该报 2");
    assert!(!w.body(a).unwrap().is_sleeping());
    assert!(!w.body(b).unwrap().is_sleeping());
    // 已经醒着的再唤一次：0（幂等，可观测）
    assert_eq!(w.wake_up_all(), 0, "没人睡着时应当返回 0");

    // 公开的 sleep()：立即入睡 + **速度清零**
    for _ in 0..60 {
        w.step(DT);
    }
    let handle = a;
    let body = w.body_mut(handle).unwrap();
    body.set_linear_velocity(vec3(3.0, 0.0, 0.0));
    body.set_angular_velocity(vec3(0.0, 2.0, 0.0));
    body.sleep();
    let body = w.body(handle).unwrap();
    assert!(body.is_sleeping());
    assert_eq!(body.linear_velocity(), Vec3::ZERO, "入睡会清零线速度");
    assert_eq!(body.angular_velocity(), Vec3::ZERO, "入睡会清零角速度");

    // 睡着之后不被积分：位置不会漂
    let before = w.body(handle).unwrap().position();
    for _ in 0..30 {
        w.step(DT);
    }
    let after = w.body(handle).unwrap().position();
    assert!(
        (after - before).length() < 1e-5,
        "睡着的刚体不该被积分：{before:?} → {after:?}"
    );
    assert_all_finite(&w, "唤醒全部");

    // allow_sleep = false 的刚体：手动 sleep 之后下一步就会被自动睡眠逻辑纠正回来
    let awake = add_box(&mut w, 0.5, vec3(0.0, 8.0, 0.0), 0.5, 0.0);
    w.body_mut(awake).unwrap().set_can_sleep(false);
    w.body_mut(awake).unwrap().sleep();
    assert!(w.body(awake).unwrap().is_sleeping(), "sleep() 是立即的");
    w.step(DT);
    assert!(
        !w.body(awake).unwrap().is_sleeping(),
        "allow_sleep = false 的刚体不该停在睡眠状态"
    );
}

#[test]
fn static_friction_holds_a_box_that_low_static_friction_lets_slide() {
    // 同一个推力、同一个动摩擦，只有静摩擦不同：一个被按住，一个滑出去。
    //
    // 水平推力折合 **5 m/s²**（箱子 1 m³、密度 1000 → 1000 kg，力就是 5000 N）：
    // * 静摩擦 1.0：μ_s = sqrt(1.0 × 0.5 地面) ≈ 0.707 → 阈值 6.9 m/s² > 5，按住；
    // * 静摩擦 0.05：μ_s ≈ 0.158 → 阈值 1.6 m/s² < 5，滑走。
    // （动摩擦两例都是 sqrt(0.3 × 0.5) ≈ 0.387，所以差异只来自静摩擦这一项。）
    fn displacement(static_friction: f32) -> f32 {
        let mut w = PhysicsWorld::new();
        add_ground(&mut w, 0.5, 0.0);
        let mass = 1000.0;
        let h = w.add_body(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::splat(0.5),
            })
            .position(vec3(0.0, 0.5, 0.0))
            .friction(0.3)
            .static_friction(static_friction)
            .build(),
        );
        let start = w.body(h).unwrap().position();
        for _ in 0..120 {
            if let Some(b) = w.body_mut(h) {
                b.apply_force(Vec3::X * (5.0 * mass));
            }
            w.step(DT);
        }
        (w.body(h).unwrap().position() - start).length()
    }

    let held = displacement(1.0);
    let slid = displacement(0.05);
    assert!(held < 0.05, "静摩擦 1.0 该把箱子按住，实际位移 {held:.4} m");
    assert!(slid > 0.5, "静摩擦 0.05 该滑出去，实际位移 {slid:.4} m");
}

#[test]
fn friction_combine_rule_changes_how_far_a_box_slides() {
    // 箱子与地面各写自己的摩擦，`friction_combine` 决定听谁的：
    // `min` → 0.3，`max` → 0.5（地面 0.5）。摩擦越大滑得越近。
    fn travelled(rule: CombineRule) -> f32 {
        let mut w = PhysicsWorld::new();
        add_ground(&mut w, 0.5, 0.0);
        let h = w.add_body(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::splat(0.5),
            })
            .position(vec3(0.0, 0.5, 0.0))
            .friction(0.3)
            // 静摩擦给 0：箱子一开始就在滑动，测到的就是动摩擦合成规则本身
            .static_friction(0.0)
            .friction_combine(rule)
            .build(),
        );
        w.body_mut(h)
            .unwrap()
            .set_linear_velocity(vec3(4.0, 0.0, 0.0));
        let start = w.body(h).unwrap().position();
        for _ in 0..180 {
            w.step(DT);
        }
        w.body(h).unwrap().position().x - start.x
    }

    let soft = travelled(CombineRule::Min);
    let hard = travelled(CombineRule::Max);
    assert!(
        soft > hard + 0.3,
        "取小的摩擦该滑得更远：min → {soft:.3} m，max → {hard:.3} m"
    );
}

#[test]
fn combine_rule_math_and_names() {
    // 五条规则的数学定义
    assert!((CombineRule::GeometricMean.combine(0.5, 0.5) - 0.5).abs() < 1e-6);
    assert!((CombineRule::GeometricMean.combine(0.0, 1.0)).abs() < 1e-6);
    assert!((CombineRule::Average.combine(0.2, 0.8) - 0.5).abs() < 1e-6);
    assert!((CombineRule::Min.combine(0.2, 0.8) - 0.2).abs() < 1e-6);
    assert!((CombineRule::Max.combine(0.2, 0.8) - 0.8).abs() < 1e-6);
    assert!((CombineRule::Multiply.combine(0.5, 0.5) - 0.25).abs() < 1e-6);
    // 负数当 0、非有限当 0（一个 NaN 材质不该把整个世界传染成 NaN）
    assert_eq!(CombineRule::Average.combine(-1.0, 0.4), 0.2);
    assert_eq!(CombineRule::Max.combine(f32::NAN, 0.4), 0.4);
    assert_eq!(CombineRule::Min.combine(f32::INFINITY, 0.4), 0.0);
    // 名字与解析互逆（场景文件、报告、编辑器下拉框都用这些名字）
    for rule in CombineRule::ALL {
        assert_eq!(CombineRule::from_name(rule.name()), Some(rule));
    }
    assert_eq!(CombineRule::from_name("geometric"), None);
    // 缺省规则必须与历史行为一致：摩擦 = 几何平均，恢复 = 取大
    let b = BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
        .friction(0.4)
        .restitution(0.6)
        .build();
    assert_eq!(b.friction_combine, None, "不指定就是 None（用世界缺省）");
    assert_eq!(b.restitution_combine, None);
    // 静摩擦缺省跟随动摩擦（只用 friction 的老代码行为不变）
    assert_eq!(b.static_friction, 0.4);
    let c = BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
        .friction(0.4)
        .static_friction(0.9)
        .build();
    assert_eq!(c.friction, 0.4);
    assert_eq!(c.static_friction, 0.9);
}

// ---------------------------------------------------------------------------
// 12. 关节补全：锥形限位 / 伺服马达 / 断裂
// ---------------------------------------------------------------------------

/// 摆杆局部 Y 轴与竖直方向的夹角（度）。
fn swing_angle(world: &PhysicsWorld, handle: crate::BodyHandle) -> f32 {
    let rot = world.body(handle).unwrap().rotation();
    let axis = rot.mul_vec3(Vec3::Y);
    axis.dot(Vec3::Y).clamp(-1.0, 1.0).acos().to_degrees()
}

#[test]
fn ball_cone_limit_pulls_the_swing_back_into_the_cone() {
    // 零重力 + 初始 60° 摆角：球窝本身不产生任何恢复力矩，所以
    // * 没有锥形限位 → 一直停在 60°；
    // * 有 25° 限位 → 被推回锥面内（25° 上下，允许求解器余量）。
    fn setup(limit_deg: Option<f32>) -> (PhysicsWorld, crate::BodyHandle) {
        let mut w = PhysicsWorld::with_gravity(Vec3::ZERO);
        let anchor = w.add_body(
            BodyBuilder::r#static(Shape::Sphere { radius: 0.05 })
                .position(vec3(0.0, 2.0, 0.0))
                .build(),
        );
        let arm = w.add_body(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: vec3(0.05, 0.5, 0.05),
            })
            // 位置算好，让**初始状态就满足**锚点重合：旋转 60° 之后，本地锚点
            // (0, 0.5, 0) 的世界位置是 0.5·(-sin60, cos60, 0)，把它从锚点位置减掉。
            // 否则第一步的位置修正会顺手改掉初始摆角，测出来的"自由摆角"就不是 60°。
            .position(vec3(0.4330127, 1.75, 0.0))
            .rotation(Quat::from_axis_angle(Vec3::Z, 60f32.to_radians()))
            .build(),
        );
        w.add_joint(Joint::Ball {
            a: anchor,
            b: arm,
            local_anchor_a: Vec3::ZERO,
            local_anchor_b: vec3(0.0, 0.5, 0.0),
            cone_limit: limit_deg.map(|d| BallConeLimit {
                half_angle: d.to_radians(),
                local_axis_a: Vec3::Y,
                local_axis_b: Vec3::Y,
            }),
        });
        (w, arm)
    }

    let (mut free, free_arm) = setup(None);
    let (mut limited, limited_arm) = setup(Some(25.0));
    let mut free_max: f32 = 0.0;
    let mut limited_max: f32 = 0.0;
    let mut limited_at_half_second = 0.0;
    for k in 0..240 {
        free.step(DT);
        limited.step(DT);
        free_max = free_max.max(swing_angle(&free, free_arm));
        limited_max = limited_max.max(swing_angle(&limited, limited_arm));
        if k == 29 {
            limited_at_half_second = swing_angle(&limited, limited_arm);
        }
    }
    let free_end = swing_angle(&free, free_arm);
    let limited_end = swing_angle(&limited, limited_arm);
    assert!(
        (free_max - 60.0).abs() < 1.0,
        "没有限位时该停在 60°：max {free_max:.2}，末尾 {free_end:.2}"
    );
    // 限位是**速度级**约束（和铰链限位同一行），所以不是瞬间夹住：
    // 第一两帧还会冲到 50° 上下，但半秒内必须被压回锥面附近。
    assert!(
        limited_max < 60.0,
        "锥形限位至少不该让摆角超过初始的 60°：{limited_max:.2}°"
    );
    assert!(
        limited_at_half_second < 32.0,
        "半秒内该压回锥面附近（25° + 余量），实际 {limited_at_half_second:.2}°"
    );
    assert!(
        limited_end < 32.0,
        "锥形限位该把摆角压回锥面附近（25° + 余量）：{limited_end:.2}°"
    );
    assert_all_finite(&limited, "球窝锥形限位");
}

#[test]
fn ball_cone_limit_allows_free_spin_about_the_axis() {
    // 锥形限位约束的是"方向"，绕轴自转必须仍然自由：给一个绕 Y 的初始角速度，
    // 轴方向不变（夹角恒 0），物体应当照转不误。
    let mut w = PhysicsWorld::with_gravity(Vec3::ZERO);
    let anchor = w.add_body(BodyBuilder::r#static(Shape::Sphere { radius: 0.05 }).build());
    let spinner = w.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.2),
        })
        .build(),
    );
    w.add_joint(Joint::Ball {
        a: anchor,
        b: spinner,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: Vec3::ZERO,
        cone_limit: Some(BallConeLimit {
            half_angle: 5f32.to_radians(),
            local_axis_a: Vec3::Y,
            local_axis_b: Vec3::Y,
        }),
    });
    w.body_mut(spinner)
        .unwrap()
        .set_angular_velocity(Vec3::Y * 4.0);
    for _ in 0..120 {
        w.step(DT);
    }
    let body = w.body(spinner).unwrap();
    assert!(
        body.angular_velocity().dot(Vec3::Y).abs() > 2.0,
        "绕轴自转被限位拦住了：{:?}",
        body.angular_velocity()
    );
    assert!(
        swing_angle(&w, spinner) < 5.0,
        "轴方向不该变：{:.2}°",
        swing_angle(&w, spinner)
    );
}

#[test]
fn hinge_servo_motor_drives_to_the_target_angle_and_holds() {
    fn build(target_deg: f32) -> (PhysicsWorld, crate::BodyHandle) {
        let mut w = PhysicsWorld::with_gravity(Vec3::ZERO);
        let anchor = w.add_body(
            BodyBuilder::r#static(Shape::Box {
                half_extents: Vec3::splat(0.1),
            })
            .build(),
        );
        let door = w.add_body(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: vec3(0.5, 0.5, 0.05),
            })
            .position(vec3(0.5, 0.0, 0.0))
            .density(500.0)
            .build(),
        );
        w.add_joint(Joint::Hinge {
            a: anchor,
            b: door,
            local_anchor_a: Vec3::ZERO,
            local_anchor_b: vec3(-0.5, 0.0, 0.0),
            local_axis_a: Vec3::Y,
            local_axis_b: Vec3::Y,
            limits: None,
            motor: Some(HingeMotor::servo(target_deg.to_radians(), 500.0)),
        });
        (w, door)
    }

    let angle = |w: &PhysicsWorld, h| {
        let rot = w.body(h).unwrap().rotation();
        rot.mul_vec3(Vec3::X)
            .dot(Vec3::X)
            .clamp(-1.0, 1.0)
            .acos()
            .to_degrees()
    };

    // 90°：转到并停住；再跑 5 秒不能抖出去（伺服是 PD，不是"到位就锁死"）
    let (mut w, door) = build(90.0);
    for _ in 0..300 {
        w.step(DT);
    }
    let reached = angle(&w, door);
    assert!(
        (reached - 90.0).abs() < 3.0,
        "伺服该转到 90°，实际 {reached:.2}°"
    );
    for _ in 0..300 {
        w.step(DT);
    }
    let held = angle(&w, door);
    assert!((held - 90.0).abs() < 3.0, "停住之后不该漂：{held:.2}°");

    // -45°：反向也能到（符号不能写反）
    let (mut w2, door2) = build(-45.0);
    for _ in 0..300 {
        w2.step(DT);
    }
    let back = angle(&w2, door2);
    assert!(
        (back - 45.0).abs() < 3.0,
        "反向目标 -45° 应当转到 45°（角度取绝对值），实际 {back:.2}°"
    );
    assert_all_finite(&w, "伺服马达");
}

#[test]
fn joint_breaks_when_the_constraint_force_exceeds_the_threshold() {
    // 吊一个 64 kg 的箱子（重力 ≈ 628 N）：
    // * 断裂力 500 N → 断，箱子掉下去；
    // * 不设阈值 / 给 1e9 N → 不断，吊在原处。
    fn drop_y(break_force: Option<f32>) -> (f32, usize, bool) {
        let mut w = PhysicsWorld::new();
        let anchor = w.add_body(
            BodyBuilder::r#static(Shape::Sphere { radius: 0.05 })
                .position(vec3(0.0, 3.0, 0.0))
                .build(),
        );
        let load = w.add_body(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::splat(0.2),
            })
            .position(vec3(0.0, 1.0, 0.0))
            .build(),
        );
        let handle = w.add_joint(Joint::Distance {
            a: anchor,
            b: load,
            local_anchor_a: Vec3::ZERO,
            local_anchor_b: Vec3::ZERO,
            rest_length: 2.0,
            stiffness: 0.0,
            damping: 0.0,
        });
        if let Some(f) = break_force {
            assert!(w.set_joint_break(handle, Some(f), None), "设阈值必须成功");
        }
        for _ in 0..180 {
            w.step(DT);
        }
        (
            w.body(load).unwrap().position().y,
            w.broken_joints().len(),
            w.joint(handle).is_some(),
        )
    }

    let (y_broken, broken, still_there) = drop_y(Some(500.0));
    assert_eq!(broken, 1, "500 N 阈值下 628 N 的载荷该把关节拉断");
    assert!(!still_there, "断掉的关节不该还能查到");
    assert!(y_broken < 0.5, "断了就该掉下去，实际 y = {y_broken:.2}");

    let (y_held, none_broken, held_there) = drop_y(None);
    assert_eq!(none_broken, 0, "没设阈值就不该断");
    assert!(held_there);
    assert!((y_held - 1.0).abs() < 0.05, "该吊在原处：y = {y_held:.3}");

    let (_, strong_broken, _) = drop_y(Some(1.0e9));
    assert_eq!(strong_broken, 0, "阈值给得极大就不该断");

    // 0 / 负 / NaN 阈值一律"等于没设"，而不是"立刻断"（静默的 NaN 阈值最难查）
    let mut w = PhysicsWorld::new();
    let a = w.add_body(BodyBuilder::r#static(Shape::Sphere { radius: 0.05 }).build());
    let b = w.add_body(BodyBuilder::dynamic(Shape::Sphere { radius: 0.2 }).build());
    let h = w.add_joint(Joint::Ball {
        a,
        b,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: Vec3::ZERO,
        cone_limit: None,
    });
    assert!(w.set_joint_break(h, Some(0.0), Some(f32::NAN)));
    assert_eq!(
        w.joint_break(h),
        Some((None, None)),
        "非正/非有限阈值等于不设"
    );
    for _ in 0..120 {
        w.step(DT);
    }
    assert_eq!(w.broken_joints().len(), 0);
    assert!(w.joint(h).is_some());
    assert_all_finite(&w, "关节断裂");
}

#[test]
fn jointed_bodies_stop_colliding_but_can_be_told_to_collide() {
    // 两个球叠在一起、用固定关节绑住：这是"门和门轴""球窝和球头"的典型几何。
    //
    // * 缺省（`collide_connected = false`，与 Bullet/PhysX 一致）：不产生接触，
    //   关节说什么就是什么；
    // * 打开之后：恢复"物理上确实叠在一起"的旧行为——会有接触、互相挤。
    fn run(collide: bool) -> (usize, bool, f32) {
        let mut w = PhysicsWorld::with_gravity(Vec3::ZERO);
        let a = w.add_body(BodyBuilder::r#static(Shape::Sphere { radius: 0.5 }).build());
        let b = w.add_body(
            BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
                .position(vec3(0.2, 0.0, 0.0))
                .build(),
        );
        // 锚点 A 偏 0.2：静止球（半径 0.5）与动态球（半径 0.5）的球心相距 0.2，
        // 也就是说两个球**深叠**在一起——正是"门和门轴""球头和球窝"的几何。
        let handle = w.add_joint(Joint::Fixed {
            a,
            b,
            local_anchor_a: vec3(0.2, 0.0, 0.0),
            local_anchor_b: Vec3::ZERO,
            local_frame_a: Quat::IDENTITY,
            local_frame_b: Quat::IDENTITY,
        });
        assert!(w.set_joint_collide_connected(handle, collide));
        assert_eq!(w.joint_collide_connected(handle), Some(collide));
        let mut max_contacts = 0usize;
        for _ in 0..120 {
            w.step(DT);
            max_contacts = max_contacts.max(w.contacts().len());
        }
        let resting = w.body(b).unwrap().is_sleeping();
        (max_contacts, resting, w.body(b).unwrap().position().x)
    }

    let (off_contacts, _, off_x) = run(false);
    let (on_contacts, _, on_x) = run(true);
    assert_eq!(off_contacts, 0, "缺省不该有接触（门和门轴不该互相挤）");
    assert!(on_contacts > 0, "打开 collide_connected 后该有接触");
    assert!(
        (off_x - 0.2).abs() < 1e-3,
        "关掉碰撞时固定关节该把球按在 0.2 m 处：{off_x}"
    );
    assert!(
        (on_x - 0.2).abs() > (off_x - 0.2).abs(),
        "打开碰撞后接触会把球挤歪：{on_x} vs {off_x}"
    );

    // 关节删掉之后阈值/开关都不该残留（槽位是复用的）
    let mut w = PhysicsWorld::with_gravity(Vec3::ZERO);
    let a = w.add_body(BodyBuilder::r#static(Shape::Sphere { radius: 0.5 }).build());
    let b = w.add_body(BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 }).build());
    let h = w.add_joint(Joint::Ball {
        a,
        b,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: Vec3::ZERO,
        cone_limit: None,
    });
    assert!(w.set_joint_collide_connected(h, true));
    assert!(w.remove_joint(h));
    let reuse_a = w.add_body(BodyBuilder::r#static(Shape::Sphere { radius: 0.1 }).build());
    let reuse_b = w.add_body(BodyBuilder::dynamic(Shape::Sphere { radius: 0.1 }).build());
    let h2 = w.add_joint(Joint::Ball {
        a: reuse_a,
        b: reuse_b,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: Vec3::ZERO,
        cone_limit: None,
    });
    assert_eq!(
        w.joint_collide_connected(h2),
        Some(false),
        "复用的槽位不该继承上一个关节的碰撞开关"
    );
}

#[test]
fn servo_keeps_its_body_awake() {
    // 伺服到位之后速度正好是 0：只看速度的睡眠判定会把它判成静止并冻结，
    // 于是停在"还没到位"的角度上再也回不来。被马达驱动的刚体必须保持清醒。
    let mut w = PhysicsWorld::with_gravity(Vec3::ZERO);
    let anchor = w.add_body(
        BodyBuilder::r#static(Shape::Box {
            half_extents: Vec3::splat(0.05),
        })
        .build(),
    );
    let door = w.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: vec3(0.35, 0.5, 0.05),
        })
        .position(vec3(0.4, 0.0, 0.0))
        .density(500.0)
        .build(),
    );
    w.add_joint(Joint::Hinge {
        a: anchor,
        b: door,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: vec3(-0.4, 0.0, 0.0),
        local_axis_a: Vec3::Y,
        local_axis_b: Vec3::Y,
        limits: None,
        motor: Some(HingeMotor::servo(30f32.to_radians(), 20.0)),
    });
    for _ in 0..600 {
        w.step(DT);
    }
    let body = w.body(door).unwrap();
    assert!(!body.is_sleeping(), "被马达驱动的刚体不该睡着");
    let angled = body
        .rotation()
        .mul_vec3(Vec3::X)
        .dot(Vec3::X)
        .clamp(-1.0, 1.0)
        .acos()
        .to_degrees();
    assert!(
        (angled - 30.0).abs() < 2.0,
        "10 秒后仍该停在 30°：{angled:.2}°"
    );
}

// ---------------------------------------------------------------------------
// 13. 事件总线
// ---------------------------------------------------------------------------

/// 把事件压成 `(种类, 步号, a, b)` 便于比较（确定性断言用）。
fn event_summary(events: &[crate::PhysicsEvent]) -> Vec<(&'static str, u64, u32, u32)> {
    events
        .iter()
        .map(|e| {
            (
                e.kind.name(),
                e.step,
                e.a.index,
                e.b.map(|b| b.index).unwrap_or(u32::MAX),
            )
        })
        .collect()
}

#[test]
fn event_bus_reports_contact_begin_and_end() {
    // 一个球掉到静态地面上弹起来再离开：应当先 contact_begin、后 contact_end。
    let mut w = PhysicsWorld::new();
    let ground = w.add_body(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).build());
    let ball = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.3 })
            .position(vec3(0.0, 2.0, 0.0))
            .restitution(0.9)
            .build(),
    );
    let mut all = Vec::new();
    let mut frame = Vec::new();
    for _ in 0..180 {
        w.step(DT);
        w.drain_events(&mut frame);
        all.append(&mut frame);
    }
    let kinds: Vec<&str> = all.iter().map(|e| e.kind.name()).collect();
    assert!(
        kinds.contains(&"contact_begin"),
        "球落到地面上应当报 contact_begin：{kinds:?}"
    );
    assert!(
        kinds.contains(&"contact_end"),
        "弹起来离开地面应当报 contact_end：{kinds:?}"
    );
    let begin = all
        .iter()
        .find(|e| e.kind == crate::PhysicsEventKind::ContactBegin)
        .unwrap();
    let end = all
        .iter()
        .find(|e| e.kind == crate::PhysicsEventKind::ContactEnd)
        .unwrap();
    assert!(begin.step <= end.step, "开始不可能晚于结束");
    assert!(
        begin.b.unwrap().index == ground.index || begin.a.index == ground.index,
        "接触事件的两端该是地面和球"
    );
    assert!(begin.a.index == ball.index || begin.b.unwrap().index == ball.index);
    // 不重复报"开始"：同一次接触只报一次（除非真的分开又碰上）
    assert!(
        all.iter()
            .filter(|e| e.kind == crate::PhysicsEventKind::ContactBegin)
            .count()
            <= 4,
        "反复报 begin 说明差分写错了：{:?}",
        event_summary(&all)
    );
}

#[test]
fn event_bus_reports_sensor_enter_and_exit_and_sleep_and_wake() {
    let mut w = PhysicsWorld::new();
    let zone = w.add_body(
        BodyBuilder::r#static(Shape::Box {
            half_extents: vec3(0.5, 0.5, 0.5),
        })
        .position(vec3(0.0, 1.0, 0.0))
        .sensor(true)
        .build(),
    );
    let faller = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.2 })
            .position(vec3(0.0, 4.0, 0.0))
            .restitution(0.0)
            .build(),
    );
    // 传感器不该唤醒/挡住它：球穿过触发区继续往地面走（这里没有地面，所以一直落）
    let mut all = Vec::new();
    let mut frame = Vec::new();
    for _ in 0..240 {
        w.step(DT);
        w.drain_events(&mut frame);
        all.append(&mut frame);
    }
    let enter = all
        .iter()
        .find(|e| e.kind == crate::PhysicsEventKind::SensorEnter)
        .expect("球穿过触发区该报 sensor_enter");
    assert_eq!(enter.a.index, zone.index, "sensor 事件的主体是传感器本体");
    assert_eq!(enter.b.unwrap().index, faller.index);
    // 传感器不产生接触
    assert!(
        !all.iter()
            .any(|e| e.kind == crate::PhysicsEventKind::ContactBegin),
        "触发区不该报接触开始"
    );

    // 睡眠/唤醒：静止 1 秒后入睡 → 报 sleep；给个冲量 → 报 wake
    let mut w = PhysicsWorld::new();
    let box_body = w.add_body(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).build());
    let sleeper = w.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.3),
        })
        .position(vec3(0.0, 0.3, 0.0))
        .build(),
    );
    let _ = box_body;
    let mut all = Vec::new();
    let mut frame = Vec::new();
    for _ in 0..120 {
        w.step(DT);
        w.drain_events(&mut frame);
        all.append(&mut frame);
    }
    assert!(
        all.iter()
            .any(|e| e.kind == crate::PhysicsEventKind::Sleep && e.a.index == sleeper.index),
        "静止够久该报 sleep：{:?}",
        event_summary(&all)
    );
    all.clear();
    w.body_mut(sleeper).unwrap().apply_impulse(Vec3::Y * 50.0);
    w.step(DT);
    w.drain_events(&mut frame);
    all.append(&mut frame);
    assert!(
        all.iter()
            .any(|e| e.kind == crate::PhysicsEventKind::Wake && e.a.index == sleeper.index),
        "冲量唤醒该报 wake：{:?}",
        event_summary(&all)
    );
}

#[test]
fn event_bus_reports_joint_break_and_is_bounded() {
    // 断裂事件带量级
    let mut w = PhysicsWorld::new();
    let anchor = w.add_body(
        BodyBuilder::r#static(Shape::Sphere { radius: 0.05 })
            .position(vec3(0.0, 3.0, 0.0))
            .build(),
    );
    let load = w.add_body(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.2),
        })
        .position(vec3(0.0, 1.0, 0.0))
        .build(),
    );
    let rope = w.add_joint(Joint::Distance {
        a: anchor,
        b: load,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: Vec3::ZERO,
        rest_length: 2.0,
        stiffness: 0.0,
        damping: 0.0,
    });
    w.set_joint_break(rope, Some(500.0), None);
    let mut events = Vec::new();
    for _ in 0..120 {
        w.step(DT);
        if w.events().is_empty() {
            continue;
        }
        w.drain_events(&mut events);
        if events
            .iter()
            .any(|e| e.kind == crate::PhysicsEventKind::JointBroken)
        {
            break;
        }
    }
    let broken = events
        .iter()
        .find(|e| e.kind == crate::PhysicsEventKind::JointBroken)
        .expect("关节断了该报 joint_broken");
    assert_eq!(broken.joint.map(|j| j.index), Some(rope.index));
    assert!(
        broken.force > 500.0,
        "事件里的力量级该超过阈值：{}",
        broken.force
    );

    // 队列有上限：塞不满就会无限涨内存
    let mut w = PhysicsWorld::new();
    w.set_event_capacity(4);
    assert_eq!(w.event_capacity(), 4);
    let ground = w.add_body(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).build());
    assert!(matches!(ground.index, _));
    for i in 0..6 {
        let b = w.add_body(
            BodyBuilder::dynamic(Shape::Sphere { radius: 0.2 })
                .position(vec3(i as f32 * 1.5, 1.0, 0.0))
                .build(),
        );
        assert!(w.body(b).is_some());
    }
    for _ in 0..120 {
        w.step(DT);
    }
    assert!(w.events().len() <= 4, "队列不该超过上限");
    assert!(w.events_dropped() > 0, "超上限的该被记账，而不是静默消失");
    // 取走之后队列不该再"记账"（丢的是新事件，不是旧账）
    w.clear_events();
    for _ in 0..30 {
        w.step(DT);
    }
    assert!(w.events().len() <= 4);
    w.reset_events_dropped();
    assert_eq!(w.events_dropped(), 0);
    // 上限 0 会退化成默认值：一个"永远丢事件"的总线没有意义
    w.set_event_capacity(0);
    assert_eq!(w.event_capacity(), crate::DEFAULT_EVENT_CAPACITY);

    // 确定性：同场景两次跑，事件序列（种类 + 步号 + 两端）逐条相同
    fn run() -> Vec<(&'static str, u64, u32, u32)> {
        let mut w = PhysicsWorld::new();
        w.add_body(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).build());
        for i in 0..5 {
            w.add_body(
                BodyBuilder::dynamic(Shape::Box {
                    half_extents: Vec3::splat(0.3),
                })
                .position(vec3(i as f32 * 0.01, 0.3 + i as f32 * 0.7, 0.0))
                .build(),
            );
        }
        let mut all = Vec::new();
        let mut frame = Vec::new();
        for _ in 0..180 {
            w.step(DT);
            w.drain_events(&mut frame);
            all.append(&mut frame);
        }
        event_summary(&all)
    }
    assert_eq!(run(), run(), "事件序列必须确定性");
}

// ---------------------------------------------------------------------------
// 14. 复合形状（一个刚体多个 Shape）
// ---------------------------------------------------------------------------

#[test]
fn compound_mass_properties_add_up() {
    // 两个 1×1×1 的盒子放在 x = ±1：质量翻倍、质心仍在原点、
    // 绕 X 的惯量不变（离转轴没距离），绕 Y/Z 的惯量加上平行轴项。
    let single = PhysicsWorld::new();
    let _ = single;
    let cube = Shape::Box {
        half_extents: Vec3::splat(0.5),
    };
    let one = BodyBuilder::dynamic(cube.clone()).build();
    let m = one.mass;
    assert!(m > 0.0);

    let two = BodyBuilder::dynamic(cube.clone())
        .part_at(cube.clone(), vec3(1.0, 0.0, 0.0))
        .part_at(cube.clone(), vec3(-1.0, 0.0, 0.0))
        .build();
    assert_eq!(two.part_count(), 3);
    assert!((two.mass - 3.0 * m).abs() < 1e-3 * m, "质量该是 3 倍");
    assert!(
        two.local_center.length() < 1e-4,
        "对称摆放的质心该在原点：{:?}",
        two.local_center
    );
    // 单盒绕自己质心的惯量（对角全相同）：m/6
    let i_box = m / 6.0;
    // 绕 X：两个偏心盒都在 y=z=0，平行轴项为 0
    let expect_x = 3.0 * i_box;
    // 绕 Y/Z：平行轴项 m*(1²)
    let expect_yz = 3.0 * i_box + 2.0 * m * 1.0;
    assert!(
        (two.inertia_local.x - expect_x).abs() < 1e-3 * expect_x.max(1.0),
        "Ix {:?} 期望 {expect_x}",
        two.inertia_local
    );
    assert!(
        (two.inertia_local.y - expect_yz).abs() < 1e-3 * expect_yz.max(1.0),
        "Iy {:?} 期望 {expect_yz}",
        two.inertia_local
    );
    assert!((two.inertia_local.z - expect_yz).abs() < 1e-3 * expect_yz.max(1.0));
    // 绕各轴都为正、且不是 NaN
    assert!(two.inertia_local.x > 0.0 && two.inertia_local.y > 0.0 && two.inertia_local.z > 0.0);

    // AABB 是并集：x 方向从 -1.5 到 1.5
    let aabb = two.world_aabb();
    assert!((aabb.min.x + 1.5).abs() < 1e-4, "{:?}", aabb);
    assert!((aabb.max.x - 1.5).abs() < 1e-4, "{:?}", aabb);
    assert!((aabb.max.y - 0.5).abs() < 1e-4, "{:?}", aabb);
}

/// 接触余量对**复合刚体**同样生效：部件离地面还剩 2 cm 时就该有负穿透的接触点。
///
/// 这条盯的是"余量只作用于单形状刚体"那个旧边界（第 61 轮补上）：复合刚体过去不膨胀，
/// 于是"高速复合体贴上墙前没有早接触"——表现和没开余量一模一样，但没人会想到是复合形状的问题。
#[test]
fn speculative_margin_applies_to_compound_bodies() {
    let build = |margin: f32| {
        let mut w = PhysicsWorld::with_gravity(Vec3::ZERO);
        w.config.speculative_margin = margin;
        w.add_body(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).build());
        // 复合刚体：主形状在原点（小球），附加盒子在下方 0.5 处，盒子半高 0.25
        // ⇒ 刚体原点在 y = 0.75 + 0.02 时，盒子底面离地 0.02
        let body = w.add_body(
            BodyBuilder::dynamic(Shape::Sphere { radius: 0.1 })
                .position(vec3(0.0, 0.75 + 0.02, 0.0))
                .part_at(
                    Shape::Box {
                        half_extents: Vec3::splat(0.25),
                    },
                    vec3(0.0, -0.5, 0.0),
                )
                .gravity_scale(0.0)
                .build(),
        );
        w.step(DT);
        let hit = w
            .contacts()
            .iter()
            .find(|p| p.a == body || p.b == body)
            .cloned();
        (hit, w.body(body).unwrap().position().y)
    };

    // 没有余量：还差 2 cm，不该有接触
    let (none, _) = build(0.0);
    assert!(none.is_none(), "余量 0 时 2 cm 的空气不该算接触：{none:?}");

    // 有余量：该生成接触对（复合刚体也要膨胀），而且接触点在**地面上**（不是部件中心）
    let (some, y) = build(0.05);
    let pair = some.expect("余量 0.05 时 2 cm 的空气必须算接触（复合刚体也要膨胀）");
    assert!(
        pair.point.y.abs() < 0.05,
        "接触点该贴在地面上，实际 {:?}",
        pair.point
    );
    // 零重力 + 还没真碰上 ⇒ 不该被推开（余量只"提前算接触"，不制造冲量）
    assert!(
        (y - 0.77).abs() < 0.02,
        "余量不该把没碰上的物体推开：y = {y}"
    );
}

#[test]
fn compound_extra_parts_really_collide() {
    // 主形状（小球）挂在高处，附加部件（盒子）在下方：落地的是**附加部件**。
    // 如果窄相只看主形状，刚体会一路掉到 y≈0.15（小球半径）——这就是回归判据。
    let mut w = PhysicsWorld::new();
    w.add_body(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).build());
    let body = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.15 })
            .position(vec3(0.0, 2.0, 0.0))
            .part_at(
                Shape::Box {
                    half_extents: Vec3::splat(0.25),
                },
                vec3(0.0, -0.75, 0.0),
            )
            .build(),
    );
    for _ in 0..240 {
        w.step(DT);
    }
    let y = w.body(body).unwrap().position().y;
    assert!(
        (y - 1.0).abs() < 0.05,
        "该停在 1.0 附近（附加盒子的底面贴地），实际 {y}"
    );
    // 睡着之后窄相不再为它生成接触（睡眠体整岛跳过）——所以这里看的是"停在哪"，
    // 而不是"还有没有接触"（后者是睡眠的语义，不是复合形状的）。
    assert!(w.body(body).unwrap().sleeping, "静止够久该睡着");

    // 射线：小球的顶面在 body_y + 0.15 ≈ 1.15；附加盒子的顶面在 body_y - 0.5 ≈ 0.5。
    // 两条不同 x 的竖直线分别打中**两个不同的部件**——这是"查询也看得见部件"的判据。
    // 射线只打 1.0 米（起点 1.4 → 最低到 0.4），免得打到无限大的地面半空间
    let sphere_top = w
        .raycast(vec3(0.0, 1.4, 0.0), Vec3::NEG_Y, 1.0)
        .expect("x=0 该打中小球");
    assert!(
        (sphere_top.point.y - (y + 0.15)).abs() < 0.05,
        "x=0 该打中小球顶面（{}），实际 {}",
        y + 0.15,
        sphere_top.point.y
    );
    let box_top = w
        .raycast(vec3(0.2, 1.4, 0.0), Vec3::NEG_Y, 1.0)
        .expect("x=0.2 该打中附加盒子");
    assert!(
        (box_top.point.y - (y - 0.5)).abs() < 0.05,
        "x=0.2 该打中附加盒子顶面（{}），实际 {}",
        y - 0.5,
        box_top.point.y
    );
    // 再偏到 x = 0.6：小球半径 0.15、盒子半宽 0.25 → 两个部件都打不中
    assert!(
        w.raycast(vec3(0.6, 1.4, 0.0), Vec3::NEG_Y, 1.0).is_none(),
        "x=0.6 处不该命中任何部件"
    );
}

#[test]
fn compound_body_is_one_rigid_body_and_the_old_shapes_still_match() {
    // 部件不影响"一个刚体"这件事：一个刚体、质量是两部分之和、
    // 而且**单形状刚体的质量属性与以前逐位一致**（同一分支）。
    let cube = Shape::Box {
        half_extents: Vec3::splat(0.5),
    };
    let plain = BodyBuilder::dynamic(cube.clone()).build();
    let mp = cube.mass_properties(1000.0);
    assert_eq!(plain.mass, mp.mass, "单形状必须走老路径（逐位相等）");
    assert_eq!(plain.inertia_local, mp.inertia);
    assert_eq!(plain.local_center, mp.center);

    let mut w = PhysicsWorld::new();
    let h = w.add_body(
        BodyBuilder::dynamic(cube.clone())
            .part_at(cube.clone(), vec3(0.0, 1.0, 0.0))
            .build(),
    );
    assert_eq!(w.body_count(), 1, "复合形状仍然是**一个**刚体");
    let body = w.body(h).unwrap();
    assert_eq!(body.part_count(), 2);
    // 质心在两部分中间（y = 0.5）
    assert!(
        (body.local_center.y - 0.5).abs() < 1e-4,
        "{:?}",
        body.local_center
    );
    assert!(w.raycast(vec3(0.0, 5.0, 0.0), Vec3::NEG_Y, 20.0).is_some());
}

// ---------------------------------------------------------------------------
// 圆锥（第 61 轮补的形状）
// ---------------------------------------------------------------------------

/// 底面朝下的圆锥落在平面上：底面停在 y=0，刚体原点（几何中心）停在 `half_height`。
///
/// 这条同时钉住三件事：圆锥真的参与了半空间接触（`shape_plane_candidates` 的底圈点），
/// 底面没有陷进地面，以及**质心偏移** `(0, -half_height/2, 0)` 被真的算进去了
/// （否则重力作用点错、静置高度也会漂）。
/// 圆角盒：平放稳稳停在 y = 半长（底面是平面面片），不会因为"只有一个接触点"摇晃。
#[test]
fn rounded_box_rests_flat_on_ground() {
    let he = vec3(0.5, 0.3, 0.4);
    let r = 0.05;
    let mut w = PhysicsWorld::new();
    add_ground(&mut w, 0.6, 0.0);
    let handle = w.add_body(
        BodyBuilder::dynamic(Shape::RoundedBox {
            half_extents: he,
            radius: r,
        })
        .position(vec3(0.0, 1.0, 0.0))
        .friction(0.6)
        .build(),
    );
    for _ in 0..400 {
        w.step(DT);
    }
    assert_all_finite(&w, "圆角盒静置");
    let body = w.body(handle).unwrap();
    assert!(
        (body.position().y - he.y).abs() < 0.02,
        "底面应停在 y=0（原点在 {}），实际 {}",
        he.y,
        body.position().y
    );
    // 没有翻倒：绕 X/Z 的倾斜很小（平放 vs 侧翻）
    let up = body.rotation().mul_vec3(Vec3::Y);
    assert!(up.y > 0.98, "不该翻倒：up = {up:?}");
}

/// 圆角盒 45° 斜着落下：圆角让它能滚过去而不是"卡在棱上"，最后必须躺平。
#[test]
fn rounded_box_settles_after_tilted_drop() {
    let he = Vec3::splat(0.4);
    let mut w = PhysicsWorld::new();
    add_ground(&mut w, 0.9, 0.0);
    let handle = w.add_body(
        BodyBuilder::dynamic(Shape::RoundedBox {
            half_extents: he,
            radius: 0.12,
        })
        .position(vec3(0.0, 0.8, 0.0))
        .rotation(Quat::from_rotation_z(0.7))
        .friction(0.9)
        .build(),
    );
    for _ in 0..600 {
        w.step(DT);
    }
    assert_all_finite(&w, "圆角盒斜落");
    let body = w.body(handle).unwrap();
    // 落地后原点高度在 [最薄半长, 外接半长] 之间（躺平取最小值），且没有穿地
    assert!(
        body.position().y > he.y - 0.05 && body.position().y < he.length() + 0.1,
        "落地高度异常：{}",
        body.position().y
    );
    assert!(
        body.linear_velocity.length() < 0.2,
        "该停下来了：{:?}",
        body.linear_velocity
    );
}

/// 圆角盒 vs 球/盒/平面/三角网的窄相都走通用凸体路径：只要不 NaN、能分开就算通。
#[test]
fn rounded_box_pairs_produce_finite_contacts() {
    let rb = Shape::RoundedBox {
        half_extents: Vec3::splat(0.5),
        radius: 0.1,
    };
    let others = [
        Shape::Sphere { radius: 0.4 },
        Shape::Box {
            half_extents: Vec3::splat(0.3),
        },
        Shape::Cylinder {
            half_height: 0.3,
            radius: 0.3,
        },
        Shape::Cone {
            half_height: 0.3,
            radius: 0.3,
        },
        Shape::RoundedBox {
            half_extents: Vec3::splat(0.4),
            radius: 0.05,
        },
    ];
    let ta = Transform::from_translation(Vec3::ZERO);
    for other in &others {
        let tb = Transform::from_translation(vec3(0.55, 0.0, 0.0));
        if let Some(m) = collide_shapes(&rb, &ta, other, &tb) {
            assert!(m.points.len() <= 4, "流形点数 {}", m.points.len());
            for p in &m.points {
                assert!(p.position.is_finite() && p.normal.is_finite());
                assert!(p.penetration.is_finite());
            }
        }
        // 分离时必须没有接触
        let far = Transform::from_translation(vec3(5.0, 0.0, 0.0));
        assert!(collide_shapes(&rb, &ta, other, &far).is_none(), "{other:?}");
    }
    // 射线打在圆角盒上：沿 -Y 打到顶面 (y = 0.5)
    let hit = ray_shape(&rb, &ta, vec3(0.0, 3.0, 0.0), -Vec3::Y, 10.0).expect("射线该命中圆角盒");
    assert!((hit.0 - 2.5).abs() < 1e-3, "距离 {}", hit.0);
    assert!(hit.1.y > 0.99, "法线该朝上：{:?}", hit.1);
}

#[test]
fn cone_base_down_rests_on_ground() {
    let h = 0.5f32;
    let r = 0.4f32;
    let mut w = PhysicsWorld::new();
    add_ground(&mut w, 0.6, 0.0);
    let handle = w.add_body(
        BodyBuilder::dynamic(Shape::Cone {
            half_height: h,
            radius: r,
        })
        .position(vec3(0.0, 1.0, 0.0))
        .friction(0.6)
        .build(),
    );
    // 刚体建好后质心就该在原点下方 h/2（`MassProperties::center` 生效）
    assert!(
        (w.body(handle).unwrap().local_center.y + h * 0.5).abs() < 1e-5,
        "质心 {:?}",
        w.body(handle).unwrap().local_center
    );
    for _ in 0..300 {
        w.step(DT);
    }
    assert_all_finite(&w, "圆锥静置");
    let body = w.body(handle).unwrap();
    assert!(
        (body.position().y - h).abs() < 0.05,
        "底面应停在 y=0（原点在 {h}），实际 {}",
        body.position().y
    );
    assert!(
        body.linear_velocity().length() < 0.1,
        "静置后速度应接近 0，实际 {:?}",
        body.linear_velocity()
    );
    // 立着没倒：局部 Y 轴仍接近世界 +Y
    let up = body.rotation().mul_vec3(Vec3::Y);
    assert!(up.y > 0.95, "圆锥倒了：{up:?}");
    // 质心在世界里也在原点下方 h/2
    assert!(
        (body.com_world().y - (body.position().y - h * 0.5)).abs() < 1e-4,
        "com {:?} 位姿 {:?}",
        body.com_world(),
        body.position()
    );
}

/// 顶点朝下（还带一点倾斜）的圆锥会倒下来：它的侧面对地面产生接触，
/// 最终躺倒（局部 Y 轴偏离世界 +Y 很多）但不会穿地、不出 NaN。
#[test]
fn cone_apex_down_tips_over() {
    let h = 0.5f32;
    let r = 0.4f32;
    let mut w = PhysicsWorld::new();
    add_ground(&mut w, 0.4, 0.0);
    let handle = w.add_body(
        BodyBuilder::dynamic(Shape::Cone {
            half_height: h,
            radius: r,
        })
        // 顶点朝下：绕 Z 转 180°，再给 0.15 rad 的倾斜让它别停在对称的刀刃上
        .position(vec3(0.0, 0.9, 0.0))
        .rotation(Quat::from_rotation_z(core::f32::consts::PI + 0.15))
        .friction(0.4)
        .build(),
    );
    for _ in 0..420 {
        w.step(DT);
    }
    assert_all_finite(&w, "圆锥翻倒");
    let body = w.body(handle).unwrap();
    let up = body.rotation().mul_vec3(Vec3::Y);
    assert!(
        up.y < 0.5,
        "顶点朝下应翻倒（局部 Y 轴该偏开世界 Y），实际 {up:?}"
    );
    // 不穿地、也没浮着：用支持函数精确算「世界 -Y 方向上最低的那个点」。
    let shape = Shape::Cone {
        half_height: h,
        radius: r,
    };
    let local_down = body.rotation().inverse().mul_vec3(Vec3::NEG_Y);
    let lowest = body.position() + body.rotation().mul_vec3(shape.support(local_down));
    assert!(
        lowest.y.abs() < 0.1,
        "躺倒后最低点应贴着地面 y≈0，实际 {lowest:?}"
    );
    assert!(
        body.linear_velocity().length() < 0.4,
        "翻倒后应基本停下，实际 {:?}",
        body.linear_velocity()
    );
}
