//! 公开 API 契约测试。
//!
//! 这里逐条调用需求中约定的类型、字段与方法签名：编译通过即代表 API 面
//! 与约定一致，运行通过代表最短路径可用。

use crate::*;
use quench3d_math::*;

#[test]
fn gravity_constant() {
    let g: Vec3 = GRAVITY;
    assert_eq!(g, vec3(0.0, -9.81, 0.0));
}

#[test]
fn shape_api() {
    // 全部 7 种形状。
    let shapes = [
        Shape::Sphere { radius: 0.5 },
        Shape::Box {
            half_extents: Vec3::splat(0.5),
        },
        Shape::Capsule {
            half_height: 0.5,
            radius: 0.25,
        },
        Shape::Cylinder {
            half_height: 0.5,
            radius: 0.25,
        },
        Shape::ConvexHull {
            points: vec![
                vec3(-0.5, -0.5, -0.5),
                vec3(0.5, -0.5, -0.5),
                vec3(0.0, 0.5, -0.5),
                vec3(0.0, 0.0, 0.5),
            ],
        },
        Shape::Plane { normal: Vec3::Y },
        Shape::TriMesh {
            vertices: vec![
                vec3(-1.0, 0.0, -1.0),
                vec3(1.0, 0.0, -1.0),
                vec3(0.0, 0.0, 1.0),
            ],
            indices: vec![0, 1, 2],
        },
    ];
    for s in &shapes {
        let mp: MassProperties = s.mass_properties(1000.0);
        let _mass: f32 = mp.mass;
        let _inertia: Vec3 = mp.inertia;
        let _center: Vec3 = mp.center;
        let aabb: Aabb = s.local_aabb();
        let _static_only: bool = s.is_static_only();
        let _support: Vec3 = s.support(Vec3::X);
        if !s.is_static_only() {
            assert!(aabb.is_valid(), "有限形状应有合法 AABB: {s:?}");
        }
    }
    assert!(Shape::Plane { normal: Vec3::Y }.is_static_only());
}

#[test]
fn body_api() {
    let _t0 = BodyType::Static;
    let _t1 = BodyType::Dynamic;
    let _t2 = BodyType::Kinematic;

    let builder = BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
        .position(vec3(0.0, 1.0, 0.0))
        .rotation(Quat::from_rotation_y(0.3))
        .transform(Transform::from_translation(vec3(0.0, 1.0, 0.0)))
        .linear_velocity(vec3(1.0, 0.0, 0.0))
        .angular_velocity(vec3(0.0, 1.0, 0.0))
        .density(900.0)
        .restitution(0.4)
        .friction(0.7)
        .linear_damping(0.05)
        .angular_damping(0.05)
        .gravity_scale(0.5)
        .user_data(42)
        .build();
    let _ = builder;

    let _ = BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).build();
    let _ = BodyBuilder::kinematic(Shape::Box {
        half_extents: Vec3::splat(0.5),
    })
    .mass(10.0)
    .build();

    let b = BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
        .position(vec3(0.0, 2.0, 0.0))
        .build();
    // 公开字段
    assert_eq!(b.body_type, BodyType::Dynamic);
    let _: &Shape = &b.shape;
    let _: Transform = b.transform;
    let _: f32 = b.restitution;
    let _: f32 = b.friction;
    let _: f32 = b.linear_damping;
    let _: f32 = b.angular_damping;
    let _: f32 = b.gravity_scale;
    let _: u64 = b.user_data;
    // 方法
    let mut b = b;
    let _: Transform = b.transform();
    b.set_transform(Transform::IDENTITY);
    let _: Vec3 = b.position();
    let _: Quat = b.rotation();
    let _: Vec3 = b.linear_velocity();
    let _: Vec3 = b.angular_velocity();
    b.set_linear_velocity(vec3(1.0, 0.0, 0.0));
    b.set_angular_velocity(vec3(0.0, 1.0, 0.0));
    b.apply_force(vec3(0.0, 1.0, 0.0));
    b.apply_impulse(vec3(0.0, 1.0, 0.0));
    b.apply_impulse_at_point(vec3(0.0, 1.0, 0.0), vec3(0.0, 0.5, 0.0));
    b.apply_torque_impulse(vec3(0.0, 0.1, 0.0));
    let _: f32 = b.mass();
    let _: f32 = b.inv_mass();
    let _: bool = b.is_dynamic();
    let _: bool = b.is_sleeping();
    b.wake_up();
    let _: Aabb = b.world_aabb();
    let _: Vec3 = b.velocity_at_point(vec3(0.0, 0.5, 0.0));
}

/// `BodyHandle` 的派生 trait 与字段。
#[test]
fn body_handle_api() {
    let mut w = PhysicsWorld::new();
    let h: BodyHandle = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
            .position(vec3(0.0, 1.0, 0.0))
            .build(),
    );
    let _idx: u32 = h.index;
    let _gen: u32 = h.generation;
    let copy: BodyHandle = h;
    assert_eq!(copy, h);
    assert!(!format!("{h:?}").is_empty());
    let mut set = std::collections::HashSet::new();
    set.insert(h);
    assert!(set.contains(&copy));
}

#[test]
fn contact_api() {
    let mut w = PhysicsWorld::new();
    let ground: BodyHandle = w.add_body(
        BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
            .position(Vec3::ZERO)
            .build(),
    );
    let ball: BodyHandle = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
            .position(vec3(0.0, 0.4, 0.0))
            .build(),
    );
    w.step(1.0 / 60.0);
    let pair: ContactPair = w.contacts()[0];
    let _a: BodyHandle = pair.a;
    let _b: BodyHandle = pair.b;
    let _n: Vec3 = pair.normal;
    let _p: Vec3 = pair.point;
    let _i: f32 = pair.impulse;
    assert!(pair.a == ground || pair.a == ball);

    // 流形与接触点的直接构造。
    let point = ContactPoint {
        position: Vec3::ZERO,
        normal: Vec3::Y,
        penetration: 0.01,
        feature_id: 0,
    };
    let manifold = ContactManifold {
        points: vec![point].into(),
    };
    assert_eq!(manifold.points.len(), 1);
    assert_eq!(manifold.points[0].position, Vec3::ZERO);
    assert_eq!(manifold.points[0].normal, Vec3::Y);
    assert_eq!(manifold.points[0].penetration, 0.01);
}

#[test]
fn world_api() {
    let mut w = PhysicsWorld::new();
    let _ = PhysicsWorld::with_gravity(vec3(0.0, -1.0, 0.0));
    w.set_gravity(vec3(0.0, -9.81, 0.0));
    let _: Vec3 = w.gravity();
    let _: f32 = w.fixed_dt();
    w.set_fixed_dt(1.0 / 120.0);
    assert!((w.fixed_dt() - 1.0 / 120.0).abs() < 1e-9);

    let ground = w.add_body(
        BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
            .position(Vec3::ZERO)
            .build(),
    );
    let ball = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.4 })
            .position(vec3(0.0, 2.0, 0.0))
            .build(),
    );
    assert_eq!(w.body_count(), 2);
    let _: Option<&RigidBody> = w.body(ball);
    let _: Option<&mut RigidBody> = w.body_mut(ball);
    assert_eq!(w.bodies().count(), 2);

    let joint: JointHandle = w.add_joint(Joint::Ball {
        a: ground,
        b: ball,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: vec3(0.0, 2.0, 0.0),
        cone_limit: None,
    });
    let _idx: u32 = joint.index;
    let _gen: u32 = joint.generation;
    assert!(w.remove_joint(joint));

    let _: &[ContactPair] = w.contacts();
    let _: usize = w.step_with_accumulator(1.0 / 60.0);
    w.step(1.0 / 60.0);

    let hit: Option<RaycastHit> = w.raycast(vec3(0.0, 5.0, 0.0), Vec3::NEG_Y, 100.0);
    if let Some(h) = hit {
        let _: BodyHandle = h.handle;
        let _: Vec3 = h.point;
        let _: Vec3 = h.normal;
        let _: f32 = h.distance;
    }
    let filtered = w.raycast_filtered(vec3(0.0, 5.0, 0.0), Vec3::NEG_Y, 100.0, |h| h == ball);
    let _ = filtered;

    let cast = w.shape_cast(
        &Shape::Sphere { radius: 0.25 },
        &Transform::from_translation(vec3(0.0, 5.0, 0.0)),
        vec3(0.0, -10.0, 0.0),
    );
    let _: Option<RaycastHit> = cast;

    assert!(w.remove_body(ball).is_some());
    assert_eq!(w.body_count(), 1);
}

#[test]
fn joints_api() {
    let mut w = PhysicsWorld::new();
    let a = w.add_body(
        BodyBuilder::r#static(Shape::Sphere { radius: 0.1 })
            .position(Vec3::ZERO)
            .build(),
    );
    let b = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.2 })
            .position(vec3(1.0, 0.0, 0.0))
            .build(),
    );
    let ball = Joint::Ball {
        a,
        b,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: vec3(-1.0, 0.0, 0.0),
        cone_limit: None,
    };
    assert_eq!(ball.bodies(), (a, b));
    let _ = w.add_joint(ball);
    let _ = w.add_joint(Joint::Fixed {
        a,
        b,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: vec3(-1.0, 0.0, 0.0),
        local_frame_a: Quat::IDENTITY,
        local_frame_b: Quat::IDENTITY,
    });
    let _ = w.add_joint(Joint::Hinge {
        a,
        b,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: vec3(-1.0, 0.0, 0.0),
        local_axis_a: Vec3::Z,
        local_axis_b: Vec3::Z,
        limits: Some((-1.0, 1.0)),
        motor: Some(HingeMotor::velocity(1.0, 10.0)),
    });
    let _ = w.add_joint(Joint::Distance {
        a,
        b,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: Vec3::ZERO,
        rest_length: 1.0,
        stiffness: 0.0,
        damping: 0.0,
    });
    let _ = w.add_joint(Joint::Slider {
        a,
        b,
        local_anchor_a: Vec3::ZERO,
        local_anchor_b: vec3(-1.0, 0.0, 0.0),
        local_axis_a: Vec3::X,
        local_axis_b: Vec3::X,
        limits: None,
    });
    assert_eq!(w.joint_count(), 5);
}

#[test]
fn character_api() {
    let c = CharacterController {
        radius: 0.4,
        height: 1.8,
        slope_limit_deg: 45.0,
        step_height: 0.3,
        snap_to_ground: true,
    };
    let _shape: Shape = c.capsule();

    let mut w = PhysicsWorld::new();
    let _ = w.add_body(
        BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
            .position(Vec3::ZERO)
            .build(),
    );
    let start = c.center_from_feet(vec3(0.0, 0.0, 0.0));
    let res: MoveResult = c.move_and_slide(&mut w, start, vec3(1.0, -0.5, 0.0));
    let _t: Vec3 = res.translation;
    let _g: bool = res.grounded;
    let _n: Vec3 = res.ground_normal;
    assert!(res.grounded, "站在地面上应报告 grounded");
}

#[test]
fn query_api() {
    let mut w = PhysicsWorld::new();
    let _ = w.add_body(
        BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
            .position(Vec3::ZERO)
            .build(),
    );
    let ball = w.add_body(
        BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
            .position(vec3(2.0, 0.0, 0.0))
            .build(),
    );
    let hits: Vec<BodyHandle> = overlap_sphere(&w, vec3(2.0, 0.0, 0.0), 0.6);
    assert!(hits.contains(&ball), "重叠查询应包含球体");

    let hit: Option<RaycastHit> = shape_cast(
        &w,
        &Shape::Sphere { radius: 0.25 },
        &Transform::from_translation(vec3(-5.0, 0.0, 0.0)),
        vec3(10.0, 0.0, 0.0),
    );
    assert!(hit.is_some(), "形状投射应命中球体");
}

#[test]
fn island_partition_is_exposed_and_sane() {
    // 两个互不相干的球 + 一块静态地板：应该是两个岛、各一个刚体，
    // 且判定为「并行没意义」（没有多体岛）。
    let mut w = PhysicsWorld::new();
    w.add_body(
        BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
            .position(Vec3::ZERO)
            .build(),
    );
    for x in [-4.0f32, 4.0] {
        w.add_body(
            BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
                .position(vec3(x, 0.4, 0.0))
                .build(),
        );
    }
    for _ in 0..10 {
        w.step(1.0 / 60.0);
    }
    let stats: IslandStats = w.island_stats();
    assert_eq!(stats.islands, 2, "两个独立的球应该是两个岛");
    assert_eq!(stats.largest, 1);
    assert!(!stats.parallelizable(), "全是单体岛时不该走并行");
    assert_eq!(w.island_count(), 2);
    assert_eq!(w.island_bodies(0).len(), 1);
    assert!(w.island_of_body(0).is_none(), "静态地板不属于任何岛");

    // 竖直叠三个箱子：一个岛、三个刚体。
    let mut w = PhysicsWorld::new();
    w.add_body(
        BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
            .position(Vec3::ZERO)
            .build(),
    );
    for layer in 0..3 {
        w.add_body(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::splat(0.4),
            })
            .position(vec3(0.0, 0.4 + layer as f32 * 0.81, 0.0))
            .build(),
        );
    }
    for _ in 0..10 {
        w.step(1.0 / 60.0);
    }
    let stats = w.island_stats();
    assert_eq!(stats.islands, 1, "叠起来的箱子应该连成一个岛");
    assert_eq!(stats.largest, 3);
    assert_eq!(stats.awake_bodies, 3);
    assert_eq!(w.island_bodies(0).len(), 3);
}

/// 并行求解与串行求解必须**逐位相同**。
///
/// 这是 island 并行的核心契约：岛与岛不共享清醒刚体、岛内接触保持全局顺序、
/// 关节两趟本来就是「先接触后关节」，所以并行不改变任何刚体经历的运算顺序。
/// 反过来，如果这里失败，说明岛划分或顺序保持出了问题——不能靠放宽容差蒙过去。
#[test]
fn parallel_solver_matches_sequential_bit_for_bit() {
    // 6 根互不相干的柱子，每根 6 个箱子：岛数 6、接触数远超阈值，必然走并行路径。
    let build = |parallel: bool| {
        let mut w = PhysicsWorld::new();
        w.config.parallel = parallel;
        // 考的是求解器数学，不是睡眠：整岛休眠会让堆叠真的睡过去，
        // 那时没有清醒岛、并行不启用，测试就证明不了任何事。
        w.config.sleep_enabled = false;
        w.add_body(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
                .position(Vec3::ZERO)
                .build(),
        );
        for column in 0..6 {
            for layer in 0..6 {
                w.add_body(
                    BodyBuilder::dynamic(Shape::Box {
                        half_extents: Vec3::splat(0.4),
                    })
                    .position(vec3(column as f32 * 3.0, 0.4 + layer as f32 * 0.81, 0.0))
                    .build(),
                );
            }
        }
        for _ in 0..240 {
            w.step(1.0 / 60.0);
        }
        w
    };
    let sequential = build(false);
    let parallel = build(true);
    assert!(!sequential.uses_parallel_solver());
    assert!(
        parallel.uses_parallel_solver(),
        "并行路径没有被启用，这个测试就证明不了任何事"
    );
    assert!(parallel.solver_threads() >= 2, "至少要真的用上两个线程");
    assert!(
        parallel.island_stats().multi_body_islands >= 6,
        "场景应该被切成多根柱子"
    );

    let a = snapshot_bits(&sequential);
    let b = snapshot_bits(&parallel);
    assert_eq!(a.len(), b.len());
    for (i, (sa, sb)) in a.iter().zip(b.iter()).enumerate() {
        assert_eq!(sa, sb, "第 {i} 个刚体的状态在并行/串行下不一致（逐位比较）");
    }
}

/// 刚体状态的逐位快照（位置、旋转、线速度、角速度、是否睡着）。
fn snapshot_bits(world: &PhysicsWorld) -> Vec<[u32; 14]> {
    world
        .bodies()
        .map(|(_, b)| {
            let p = b.position();
            let q = b.rotation();
            let v = b.linear_velocity;
            let a = b.angular_velocity;
            [
                p.x.to_bits(),
                p.y.to_bits(),
                p.z.to_bits(),
                q.x.to_bits(),
                q.y.to_bits(),
                q.z.to_bits(),
                q.w.to_bits(),
                v.x.to_bits(),
                v.y.to_bits(),
                v.z.to_bits(),
                a.x.to_bits(),
                a.y.to_bits(),
                a.z.to_bits(),
                if b.is_sleeping() { 1 } else { 0 },
            ]
        })
        .collect()
}

/// 有关节时走的是「每个迭代：并行接触 + 串行关节」那条路径，
/// 同样必须逐位相同——这条路径专门验证关节与接触的交错顺序没有被打乱。
#[test]
fn parallel_solver_with_joints_is_also_bit_exact() {
    let build = |parallel: bool| {
        let mut w = PhysicsWorld::new();
        w.config.parallel = parallel;
        // 考的是求解器数学，不是睡眠：整岛休眠会让堆叠真的睡过去，
        // 那时没有清醒岛、并行不启用，测试就证明不了任何事。
        w.config.sleep_enabled = false;
        w.add_body(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
                .position(Vec3::ZERO)
                .build(),
        );
        let mut boxes = Vec::new();
        for pile in 0..20 {
            let mut last = None;
            for layer in 0..5 {
                let h = w.add_body(
                    BodyBuilder::dynamic(Shape::Box {
                        half_extents: Vec3::splat(0.4),
                    })
                    .position(vec3(pile as f32 * 3.0, 0.4 + layer as f32 * 0.81, 0.0))
                    .build(),
                );
                boxes.push(h);
                let _ = last;
                last = Some(h);
            }
            // 每个堆里加一根距离关节：它在同一个 island 内部，
            // 但会让求解器走「接触与关节交错」的那条路径。
            if let (Some(a), Some(b)) = (boxes.get(pile * 5), boxes.get(pile * 5 + 2)) {
                w.add_joint(Joint::Distance {
                    a: *a,
                    b: *b,
                    local_anchor_a: Vec3::ZERO,
                    local_anchor_b: Vec3::ZERO,
                    rest_length: 1.62,
                    stiffness: 0.0,
                    damping: 0.0,
                });
            }
        }
        for _ in 0..180 {
            w.step(1.0 / 60.0);
        }
        w
    };
    let sequential = build(false);
    let parallel = build(true);
    assert!(parallel.uses_parallel_solver(), "这条路径必须真的并行");
    assert!(parallel.joint_count() > 0, "场景里应该有关节");
    let a = snapshot_bits(&sequential);
    let b = snapshot_bits(&parallel);
    for (i, (sa, sb)) in a.iter().zip(b.iter()).enumerate() {
        assert_eq!(sa, sb, "有关节时第 {i} 个刚体逐位不一致");
    }
}

/// 线程数**不影响结果**：2 / 3 / 4 线程（以及自动）都得到逐位相同的状态。
///
/// 这是「可复现」的完整含义——不只是「并行 == 串行」，还包括
/// 「换个核数（或换台机器）跑，结果不会变」。
#[test]
fn parallel_results_do_not_depend_on_thread_count() {
    // 线程数会被机器核数夹住：`max_threads = 4` 在 3 核的机器上只能开 3 个线程
    //（macOS CI runner 就是 3 核，这条断言因此在那边变红过一次）。
    let hw = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    if hw < 2 {
        eprintln!("只有 {hw} 个逻辑核，跳过并行确定性测试");
        return;
    }
    let build = |threads: usize| {
        let mut w = PhysicsWorld::new();
        w.config.max_threads = threads;
        w.config.sleep_enabled = false;
        w.add_body(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
                .position(Vec3::ZERO)
                .build(),
        );
        for pile in 0..24 {
            for layer in 0..6 {
                w.add_body(
                    BodyBuilder::dynamic(Shape::Box {
                        half_extents: Vec3::splat(0.4),
                    })
                    .position(vec3(pile as f32 * 3.0, 0.4 + layer as f32 * 0.81, 0.0))
                    .build(),
                );
            }
        }
        for _ in 0..150 {
            w.step(1.0 / 60.0);
        }
        w
    };
    let base = build(2);
    assert!(base.uses_parallel_solver());
    assert_eq!(base.solver_threads(), 2);
    let want = snapshot_bits(&base);
    for threads in [3usize, 4] {
        let other = build(threads);
        assert!(other.uses_parallel_solver());
        assert_eq!(
            other.solver_threads(),
            threads.min(hw),
            "线程数 = min(max_threads, 机器逻辑核数 = {hw})"
        );
        let got = snapshot_bits(&other);
        for (i, (a, b)) in want.iter().zip(got.iter()).enumerate() {
            assert_eq!(a, b, "{threads} 线程与 2 线程下第 {i} 个刚体逐位不一致");
        }
    }
}

/// **整岛休眠**：一摞箱子必须**同一步**入睡，而不是一个一个睡。
///
/// 这是逐体计时的老毛病：下面几个先睡，被当成无限质量冻住，接触力传不下去，
/// 上面还在抖的继续渗进下面。整岛计时把这种「半睡半醒的堆」直接消灭。
#[test]
fn stacked_bodies_fall_asleep_in_the_same_step() {
    let mut world = PhysicsWorld::new();
    world.add_body(
        BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
            .position(Vec3::ZERO)
            .build(),
    );
    for layer in 0..5 {
        world.add_body(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::splat(0.5),
            })
            .position(vec3(0.0, 0.5 + layer as f32 * 1.02, 0.0))
            .build(),
        );
    }
    let mut sleep_step: Vec<Option<u32>> = vec![None; 6];
    for step in 0..900u32 {
        world.step(1.0 / 60.0);
        for (i, (_, b)) in world.bodies().enumerate() {
            if b.is_sleeping() && sleep_step[i].is_none() {
                sleep_step[i] = Some(step);
            }
        }
    }
    // 地面是静态体，永远不会「睡」；4 个箱子（下标 1..=4）必须同一步入睡。
    assert!(
        sleep_step[1].is_some(),
        "整摞箱子应当在 900 步内睡着（实际睡眠情况 {sleep_step:?}）"
    );
    for i in 2..=4 {
        assert_eq!(
            sleep_step[i], sleep_step[1],
            "第 {i} 个箱子在第 {:?} 步入睡，而第 1 个是 {:?}——整岛必须同时入睡",
            sleep_step[i], sleep_step[1]
        );
    }
    // 睡着了就不该再被无谓地唤醒（没有新的扰动）
    for _ in 0..60 {
        world.step(1.0 / 60.0);
    }
    for i in 1..=4 {
        assert!(
            world
                .bodies()
                .nth(i)
                .map(|(_, b)| b.is_sleeping())
                .unwrap_or(false),
            "第 {i} 个箱子睡着后又被唤醒了"
        );
    }
}

/// **整岛唤醒**：睡着的一摞箱子，一旦有一个被推动，整摞要一起醒过来。
#[test]
fn sleeping_island_wakes_as_a_unit() {
    let mut world = PhysicsWorld::new();
    world.add_body(
        BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
            .position(Vec3::ZERO)
            .build(),
    );
    let mut handles = Vec::new();
    for layer in 0..4 {
        handles.push(
            world.add_body(
                BodyBuilder::dynamic(Shape::Box {
                    half_extents: Vec3::splat(0.5),
                })
                .position(vec3(0.0, 0.5 + layer as f32 * 1.02, 0.0))
                .build(),
            ),
        );
    }
    for _ in 0..900 {
        world.step(1.0 / 60.0);
    }
    let asleep = handles
        .iter()
        .filter(|h| world.body(**h).map(|b| b.is_sleeping()).unwrap_or(false))
        .count();
    assert_eq!(asleep, handles.len(), "整摞箱子应该都睡着了");

    // 推最下面那个：整岛都该醒（醒来是靠接触传播的，给几步时间）
    if let Some(b) = world.body_mut(handles[0]) {
        b.apply_impulse(vec3(3.0, 0.0, 0.0));
    }
    let mut woke = 0usize;
    for _ in 0..5 {
        world.step(1.0 / 60.0);
        woke = handles
            .iter()
            .filter(|h| world.body(**h).map(|b| !b.is_sleeping()).unwrap_or(false))
            .count();
        if woke == handles.len() {
            break;
        }
    }
    assert_eq!(
        woke,
        handles.len(),
        "整岛唤醒失败：只有 {woke}/{} 个醒了",
        handles.len()
    );
}

/// 时钟 API：原生默认就有可用时钟，阶段计时确实走它。
///
/// 这条测试守的是 wasm 那个坑：`std::time::Instant::now()` 在
/// `wasm32-unknown-unknown` 上直接 panic，所以阶段计时改成走可插拔时钟
/// （原生默认 `Instant`，wasm 由宿主装 `performance.now()`，不装就是 0、不 panic）。
/// 这里验证「装/不装都不影响能不能计时」这件事在原生侧仍然成立。
#[test]
fn phase_timing_uses_the_pluggable_clock() {
    assert!(crate::has_time_source(), "原生应当默认就有可用时钟");
    let mut world = PhysicsWorld::new();
    world.set_phase_timing(true);
    for _ in 0..10 {
        world.step(1.0 / 60.0);
    }
    let t = world.phase_timings();
    assert_eq!(t.steps, 10, "阶段计时的步数应当与调用次数一致");
    assert!(
        t.total_us > 0.0,
        "装了时钟之后整步耗时不应当是 0（实际 {})",
        t.total_us
    );
    // 关掉计时后不再累计（同一个世界继续跑）
    world.set_phase_timing(false);
    for _ in 0..10 {
        world.step(1.0 / 60.0);
    }
    assert_eq!(world.phase_timings().steps, 10, "关掉计时后不应再累计");
}

/// 接触特征 ID（gap #20 的第一条腿）：同特征稳定、异特征可分、坏输入不 panic。
///
/// 它是**热启动匹配的键**：不稳定 = 冲量每步丢失（表现为"箱子忽软忽硬"）；
/// 不可分 = 把相邻特征点的冲量错误沿用（表现为"多余的黏滞/抖动"）。
#[test]
fn contact_feature_ids_are_stable_and_discriminating() {
    use crate::contact::feature_id_from_local;
    let a = Vec3::new(0.25, -0.5, 1.0);
    let b = Vec3::new(-0.125, 0.75, 0.0);
    let id = feature_id_from_local(a, b);
    assert_ne!(id, 0, "正常输入不该是「未知」");
    assert_eq!(id, feature_id_from_local(a, b), "同输入必须同 ID");

    // 同一个特征在连续帧里会有浮点噪声（1e-4 m 量级）→ 必须落进同一格
    let jitter = Vec3::new(1.0e-4, -2.0e-4, 3.0e-5);
    assert_eq!(
        id,
        feature_id_from_local(a + jitter, b - jitter),
        "0.5 mm 以内的抖动必须得到同一个特征 ID"
    );

    // 相邻但不同的特征点（差 5 mm）必须区分开
    let other = feature_id_from_local(a + Vec3::new(0.005, 0.0, 0.0), b);
    assert_ne!(id, other, "相距 5 mm 的不同特征不该共用一个 ID");
    // 任何一端变了都算不同特征
    assert_ne!(id, feature_id_from_local(a, b + Vec3::new(0.0, 0.005, 0.0)));

    // 非有限值退化为"未知"，绝不 panic、也不产生随机 ID
    assert_eq!(feature_id_from_local(Vec3::new(f32::NAN, 0.0, 0.0), b), 0);
    assert_eq!(
        feature_id_from_local(a, Vec3::new(0.0, f32::INFINITY, 0.0)),
        0
    );
}

/// 接触余量（gap #20 第二条腿）：缺省关闭 = 历史行为逐位不变；形状按余量长大。
#[test]
fn speculative_margin_defaults_off_and_inflates_shapes() {
    use crate::shape::Shape;
    assert_eq!(
        SolverConfig::default().speculative_margin,
        0.0,
        "缺省必须是 0：没写这一项的场景结果要与历史完全一致"
    );
    let b = Shape::Box {
        half_extents: Vec3::new(0.5, 0.5, 0.5),
    };
    if let Shape::Box { half_extents } = b.inflated(0.02) {
        assert!((half_extents.x - 0.52).abs() < 1e-6, "盒子按余量长大");
        assert!((half_extents.y - 0.52).abs() < 1e-6);
    } else {
        panic!("膨胀不该改变形状类型");
    }
    let sphere = Shape::Sphere { radius: 1.0 };
    if let Shape::Sphere { radius } = sphere.inflated(0.1) {
        assert!((radius - 1.1).abs() < 1e-6, "球按半径膨胀");
    } else {
        panic!("膨胀不该改变形状类型");
    }
    // 余量 <= 0 时原样返回；平面没有"长胖"的概念，也原样返回
    let plane = Shape::Plane { normal: Vec3::Y };
    if let Shape::Plane { normal } = plane.inflated(0.1) {
        assert_eq!(normal, Vec3::Y, "平面余量没有意义，原样返回");
    }
}
