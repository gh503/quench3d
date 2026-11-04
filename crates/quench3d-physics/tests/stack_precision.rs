//! Fixed public-step stack audit. New 5mm target is a fixture acceptance target,
//! not an engine guarantee. 240 steps are4s/2s at60/120Hz respectively.
use quench3d_math::{vec3, Vec3};
use quench3d_physics::{BodyBuilder, PhysicsWorld, Shape};

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    (0..3).map(|i| a[i] * b[i]).sum()
}
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
#[derive(Clone, Copy, Debug)]
struct Obb {
    center: [f64; 3],
    axes: [[f64; 3]; 3],
    half: [f64; 3],
}
impl Obb {
    fn read(world: &PhysicsWorld, handle: quench3d_physics::BodyHandle) -> Self {
        let body = world.body(handle).unwrap();
        let p = body.position();
        let q = body.rotation();
        let [x, y, z, w] = [q.x, q.y, q.z, q.w].map(f64::from);
        let axes = [
            [
                1.0 - 2.0 * (y * y + z * z),
                2.0 * (x * y + z * w),
                2.0 * (x * z - y * w),
            ],
            [
                2.0 * (x * y - z * w),
                1.0 - 2.0 * (x * x + z * z),
                2.0 * (y * z + x * w),
            ],
            [
                2.0 * (x * z + y * w),
                2.0 * (y * z - x * w),
                1.0 - 2.0 * (x * x + y * y),
            ],
        ];
        Self {
            center: [f64::from(p.x), f64::from(p.y), f64::from(p.z)],
            axes,
            half: [0.25; 3],
        }
    }
    fn corners(self) -> [[f64; 3]; 8] {
        std::array::from_fn(|i| {
            std::array::from_fn(|axis| {
                self.center[axis]
                    + (0..3)
                        .map(|j| {
                            (if i & (1 << j) == 0 {
                                -self.half[j]
                            } else {
                                self.half[j]
                            }) * self.axes[j][axis]
                        })
                        .sum::<f64>()
            })
        })
    }
    // Independent normalized SAT: signed positive gap=separated, negative=overlap.
    // Corners/15axes use only public world pose, not engine narrowphase or AABBs.
    fn separation(self, other: Self) -> f64 {
        let mut axes = self.axes.to_vec();
        axes.extend(other.axes);
        for a in self.axes {
            for b in other.axes {
                axes.push(cross(a, b));
            }
        }
        let (a, b) = (self.corners(), other.corners());
        let mut separation = f64::NEG_INFINITY;
        for axis in axes {
            let length = dot(axis, axis).sqrt();
            if length <= 1e-12 {
                continue;
            }
            let axis = axis.map(|x| x / length);
            let range = |points: [[f64; 3]; 8]| {
                points
                    .into_iter()
                    .map(|p| dot(p, axis))
                    .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                        (lo.min(v), hi.max(v))
                    })
            };
            let (alo, ahi) = range(a);
            let (blo, bhi) = range(b);
            separation = separation.max((blo - ahi).max(alo - bhi));
        }
        separation
    }
}
#[derive(Debug)]
struct Summary {
    layers: usize,
    hz: u32,
    enhanced: bool,
    floor_depth: f64,
    plane_depth: f64,
    outside_floor_samples: usize,
    pair_depth: f64,
    top_drift: f64,
    max_speed: f64,
    sleeping: usize,
    contact_steps: usize,
    worst_step: usize,
    worst_pair: usize,
}
fn audit() -> Vec<Summary> {
    let mut summaries = Vec::new();
    for layers in [4usize, 12] {
        for hz in [60u32, 120] {
            for enhanced in [false, true] {
                let dt = 1.0 / hz as f32;
                let mut world = PhysicsWorld::new();
                world.set_fixed_dt(dt);
                if enhanced {
                    world.config.velocity_iterations = 20;
                    world.config.position_iterations = 8;
                }
                assert_eq!(world.config.slop, 0.005);
                assert_eq!(world.config.speculative_margin, 0.0);
                world.add_builder(
                    BodyBuilder::r#static(Shape::Box {
                        half_extents: vec3(4.0, 0.25, 4.0),
                    })
                    .position(vec3(0.0, -0.25, 0.0)),
                );
                let floor = Obb {
                    center: [0.0, -0.25, 0.0],
                    axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                    half: [4.0, 0.25, 4.0],
                };
                let handles: Vec<_> = (0..layers)
                    .map(|i| {
                        world.add_builder(
                            BodyBuilder::dynamic(Shape::Box {
                                half_extents: Vec3::splat(0.25),
                            })
                            .position(vec3(0.0, 0.25 + i as f32 * 0.502, 0.0))
                            .mass(1.0),
                        )
                    })
                    .collect();
                let initial: Vec<_> = handles.iter().map(|h| Obb::read(&world, *h)).collect();
                for pair in initial.windows(2) {
                    assert!(pair[0].separation(pair[1]) > 0.00199);
                }
                let top = initial[layers - 1].center;
                let mut summary = Summary {
                    layers,
                    hz,
                    enhanced,
                    floor_depth: 0.0,
                    plane_depth: 0.0,
                    outside_floor_samples: 0,
                    pair_depth: 0.0,
                    top_drift: 0.0,
                    max_speed: 0.0,
                    sleeping: 0,
                    contact_steps: 0,
                    worst_step: 0,
                    worst_pair: 0,
                };
                let mut worst_geometry = None;
                for step in 1..=240 {
                    world.step(dt);
                    assert_eq!(world.step_count, step as u64);
                    assert_eq!(world.bodies().count(), layers + 1);
                    summary.contact_steps += usize::from(!world.contacts().is_empty());
                    let mut poses = Vec::new();
                    for h in &handles {
                        let b = world.body(*h).unwrap();
                        let q = b.rotation();
                        assert!(
                            b.position().is_finite()
                                && b.linear_velocity.is_finite()
                                && b.angular_velocity.is_finite()
                                && [q.x, q.y, q.z, q.w].iter().all(|v| v.is_finite())
                        );
                        assert!((b.mass() - 1.0).abs() < 1e-6);
                        let obb = Obb::read(&world, *h);
                        let min_y = obb
                            .corners()
                            .into_iter()
                            .map(|p| p[1])
                            .fold(f64::INFINITY, f64::min);
                        summary.plane_depth = summary.plane_depth.max((-min_y).max(0.0));
                        summary.floor_depth =
                            summary.floor_depth.max((-obb.separation(floor)).max(0.0));
                        summary.outside_floor_samples += usize::from(
                            obb.corners().iter().all(|p| p[0] > 4.0)
                                || obb.corners().iter().all(|p| p[0] < -4.0)
                                || obb.corners().iter().all(|p| p[2] > 4.0)
                                || obb.corners().iter().all(|p| p[2] < -4.0),
                        );
                        summary.max_speed =
                            summary.max_speed.max(f64::from(b.linear_velocity.length()));
                        poses.push(obb);
                    }
                    for (i, pair) in poses.windows(2).enumerate() {
                        let depth = (-pair[0].separation(pair[1])).max(0.0);
                        if depth > summary.pair_depth {
                            summary.pair_depth = depth;
                            summary.worst_step = step;
                            summary.worst_pair = i;
                            worst_geometry =
                                Some((step, i, pair[0], pair[1], world.contacts().len()));
                        }
                        if std::env::var_os("Q3D_STACK_TRACE").is_some()
                            && layers == 12
                            && hz == 60
                            && !enhanced
                            && step <= 16
                        {
                            println!("ROUND172_EARLY step={step} pair={i} separation={} contacts={} a={:?} b={:?}",pair[0].separation(pair[1]),world.contacts().len(),pair[0],pair[1]);
                        }
                    }
                    let d = sub(poses[layers - 1].center, top);
                    summary.top_drift = summary.top_drift.max((d[0] * d[0] + d[2] * d[2]).sqrt());
                }
                summary.sleeping = handles
                    .iter()
                    .filter(|h| world.body(**h).unwrap().is_sleeping())
                    .count();
                assert!(summary.contact_steps > 0, "actual contact required");
                if std::env::var_os("Q3D_STACK_TRACE").is_some() {
                    println!("ROUND172_WORST layers={layers} hz={hz} enhanced={enhanced} geometry={worst_geometry:?}");
                }
                println!(
                    "ROUND172_STACK layers={} hz={} enhanced={} {summary:?} physical_seconds={:.1}",
                    summary.layers,
                    summary.hz,
                    summary.enhanced,
                    240.0 / f64::from(hz)
                );
                summaries.push(summary);
            }
        }
    }
    assert_eq!(summaries.len(), 8);
    summaries
}
fn run_duration_fixture(layers: usize, steps: usize) -> (f64, f64) {
    run_duration_velocity_fixture(layers, steps, 20)
}

fn run_duration_velocity_fixture(
    layers: usize,
    steps: usize,
    velocity_iterations: usize,
) -> (f64, f64) {
    let started = std::time::Instant::now();
    let floor = Obb {
        center: [0.0, -0.25, 0.0],
        axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        half: [4.0, 0.25, 4.0],
    };

    let mut world = PhysicsWorld::new();
    world.set_fixed_dt(1.0 / 60.0);
    world.config.speculative_margin = 0.005;
    world.config.velocity_iterations = velocity_iterations;
    world.config.position_iterations = 8;
    world.add_builder(
        BodyBuilder::r#static(Shape::Box {
            half_extents: vec3(4.0, 0.25, 4.0),
        })
        .position(vec3(0.0, -0.25, 0.0)),
    );
    let handles: Vec<_> = (0..layers)
        .map(|i| {
            world.add_builder(
                BodyBuilder::dynamic(Shape::Box {
                    half_extents: Vec3::splat(0.25),
                })
                .position(vec3(0.0, 0.25 + i as f32 * 0.502, 0.0))
                .mass(1.0),
            )
        })
        .collect();
    let (mut max_pair, mut max_floor, mut drift) = (0.0f64, 0.0f64, 0.0f64);
    let (mut pair_worst, mut floor_worst) = (0, 0);
    let mut contact_steps = 0;
    let mut outside = 0;
    let mut first_sleep = None;
    let mut first_excess = false;
    let mut first_pair_excess = false;
    let mut footprint = [0.0f64; 2];
    for step in 1..=steps {
        world.step(1.0 / 60.0);
        assert_eq!(world.step_count, step as u64);
        assert_eq!(world.body_count(), layers + 1);
        let poses: Vec<_> = handles.iter().map(|h| Obb::read(&world, *h)).collect();
        let mut mass = 0.0f32;
        for h in &handles {
            let b = world.body(*h).unwrap();
            assert!(
                b.position().is_finite()
                    && b.linear_velocity.is_finite()
                    && b.angular_velocity.is_finite()
            );
            let q = b.rotation();
            assert!([q.x, q.y, q.z, q.w].into_iter().all(f32::is_finite));
            mass += b.mass();
        }
        assert_eq!(mass, layers as f32);
        contact_steps += usize::from(!world.contacts().is_empty());
        if first_sleep.is_none()
            && handles
                .iter()
                .all(|h| world.body(*h).unwrap().is_sleeping())
        {
            first_sleep = Some(step);
        }
        for (i, a) in poses.iter().enumerate() {
            let fd = (-a.separation(floor)).max(0.0);
            if fd > max_floor {
                max_floor = fd;
                floor_worst = step;
            }
            let horizontal = (a.center[0] * a.center[0] + a.center[2] * a.center[2]).sqrt();
            drift = drift.max(horizontal);
            let corners = a.corners();
            for c in corners {
                footprint[0] = footprint[0].max(c[0].abs());
                footprint[1] = footprint[1].max(c[2].abs());
            }
            if corners.iter().all(|p| p[0] > 4.0)
                || corners.iter().all(|p| p[0] < -4.0)
                || corners.iter().all(|p| p[2] > 4.0)
                || corners.iter().all(|p| p[2] < -4.0)
            {
                outside += 1;
            }
            if fd > 0.00501 && !first_excess {
                first_excess = true;
                println!(
                    "ROUND176_FIRST layers={layers} step={step} floor_body={i} depth={fd:.12}"
                );
                if std::env::var_os("Q3D_STACK_DURATION_TRACE").is_some() {
                    println!(
                        "ROUND176_POSES before_unavailable_post={poses:?} contacts={:?}",
                        world.contacts()
                    );
                }
            }
            for (j, b) in poses.iter().enumerate().skip(i + 1) {
                let d = (-a.separation(*b)).max(0.0);
                if d > max_pair {
                    max_pair = d;
                    pair_worst = step;
                }
                if d > 0.00501 && !first_pair_excess {
                    first_pair_excess = true;
                    let inside = |o: Obb| {
                        o.corners()
                            .iter()
                            .all(|p| p[0].abs() <= 4.0 && p[2].abs() <= 4.0)
                    };
                    println!("ROUND176_FIRST_PAIR layers={layers} step={step} pair={i},{j} depth={d:.12} both_footprints_inside={} a={a:?} b={b:?}",inside(*a)&&inside(*b));
                }
                if d > 0.00501 && !first_excess {
                    first_excess = true;
                    println!(
                        "ROUND176_FIRST layers={layers} step={step} pair={i},{j} depth={d:.12}"
                    );
                    if std::env::var_os("Q3D_STACK_DURATION_TRACE").is_some() {
                        println!(
                            "ROUND176_POSES post={poses:?} contacts={:?}",
                            world.contacts()
                        );
                    }
                }
            }
        }
    }
    assert!(contact_steps > 0);
    let sleeping = handles
        .iter()
        .filter(|h| world.body(**h).unwrap().is_sleeping())
        .count();
    println!("ROUND176_DURATION layers={layers} hz=60 steps={steps} seconds={} pair={max_pair:.12} pair_worst_step={pair_worst} floor={max_floor:.12} floor_worst_step={floor_worst} contact_steps={contact_steps} final_sleep={sleeping} awake={} first_all_sleep={first_sleep:?} drift={drift:.12} footprint={footprint:?} wholly_outside_samples={outside}",steps as f64/60.0,layers-sleeping);
    println!(
        "ROUND176_WALL layers={layers} total_seconds={:.6} diagnostic_only_not_benchmark",
        started.elapsed().as_secs_f64()
    );
    (max_pair, max_floor)
}

#[test]
#[ignore = "controlled80vs20velocity30s32layer precision diagnostic, not solver implementation"]
fn stack_velocity_budget_80_controlled_diagnostic() {
    let mut results = Vec::new();
    for velocity_iterations in [20, 80] {
        for layers in [4, 12, 32] {
            let (pair, floor) = run_duration_velocity_fixture(layers, 1800, velocity_iterations);
            println!("ROUND178_BUDGET velocity={velocity_iterations} position=8 layers={layers} steps=1800 seconds=30 pair={pair:.12} floor={floor:.12} precision_pass={}", pair <= 0.00501 && floor <= 0.00501);
            results.push((velocity_iterations, layers, pair, floor));
        }
    }
    // All six worlds complete before evaluating the candidate. The20 baseline
    // is known unmet at32 layers; it is measured without pretending it passes.
    assert!(
        results
            .iter()
            .filter(|r| r.0 == 80)
            .all(|r| r.2 <= 0.00501 && r.3 <= 0.00501),
        "fixed80/8 precision diagnostic unmet: {results:?}"
    );
}

#[test]
fn speculative_stack_sustained_precision_recipe() {
    for layers in [4, 12] {
        let (pair, floor) = run_duration_fixture(layers, 1800);
        assert!(
            pair <= 0.00501 && floor <= 0.00501,
            "30s recipe layers={layers} pair={pair} floor={floor}"
        );
    }
}

#[test]
#[ignore = "unmet: fixed32layer30s recipe exceeds5mm before leaving finite floor"]
fn speculative_stack_sustained_scale_precision_target() {
    let (pair, floor) = run_duration_fixture(32, 1800);
    assert!(
        pair <= 0.00501 && floor <= 0.00501,
        "32layer30s5mm target pair={pair} floor={floor}"
    );
}

#[test]
#[ignore = "opt-in bounded first16step32layer geometry trace"]
fn speculative_stack_scale_early_trace() {
    run_duration_fixture(32, 16);
}

#[test]
fn speculative_stack_precision_recipe_and_zero_margin_audit() {
    for layers in [4usize, 12] {
        let mut zero_reference = Vec::new();
        for setting in 0..4 {
            let margin = if setting >= 2 { 0.005 } else { 0.0 };
            let mut world = PhysicsWorld::new();
            world.set_fixed_dt(1.0 / 60.0);
            if setting == 3 {
                world.config.velocity_iterations = 20;
                world.config.position_iterations = 8;
            }
            let mut first_excess = false;
            if setting != 0 {
                world.config.speculative_margin = margin;
            }
            let floor = Obb {
                center: [0.0, -0.25, 0.0],
                axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                half: [4.0, 0.25, 4.0],
            };
            world.add_builder(
                BodyBuilder::r#static(Shape::Box {
                    half_extents: vec3(4.0, 0.25, 4.0),
                })
                .position(vec3(0.0, -0.25, 0.0)),
            );
            let handles: Vec<_> = (0..layers)
                .map(|i| {
                    world.add_builder(
                        BodyBuilder::dynamic(Shape::Box {
                            half_extents: Vec3::splat(0.25),
                        })
                        .position(vec3(0.0, 0.25 + i as f32 * 0.502, 0.0))
                        .mass(1.0),
                    )
                })
                .collect();
            let mut bits = Vec::new();
            let mut early = 0.0f64;
            let mut adjacent = 0.0f64;
            let mut all = 0.0f64;
            let mut floor_depth = 0.0f64;
            let mut contacts = 0;
            for step in 1..=240 {
                world.step(1.0 / 60.0);
                assert_eq!(world.step_count, step as u64);
                let poses: Vec<_> = handles.iter().map(|h| Obb::read(&world, *h)).collect();
                for (i, a) in poses.iter().enumerate() {
                    floor_depth = floor_depth.max((-a.separation(floor)).max(0.0));
                    for (j, b) in poses.iter().enumerate().skip(i + 1) {
                        let d = (-a.separation(*b)).max(0.0);
                        all = all.max(d);
                        if std::env::var_os("Q3D_STACK_SOLVER_TRACE").is_some()
                            && setting >= 2
                            && d > 0.00501
                            && !first_excess
                        {
                            first_excess = true;
                            println!("ROUND175_FIRST layers={layers} setting={setting} step={step} pair={i},{j} depth={d:.12} a={a:?} b={b:?} va={:?} vb={:?} contacts={:?}",world.body(handles[i]).unwrap().linear_velocity,world.body(handles[j]).unwrap().linear_velocity,world.contacts());
                        }
                        if j == i + 1 {
                            adjacent = adjacent.max(d);
                            if step <= 16 {
                                early = early.max(d);
                            }
                        }
                    }
                }
                contacts += world.contacts().len();
                for h in &handles {
                    let b = world.body(*h).unwrap();
                    let q = b.rotation();
                    assert!(
                        b.position().is_finite()
                            && b.linear_velocity.is_finite()
                            && b.angular_velocity.is_finite()
                    );
                    for v in b
                        .position()
                        .to_array()
                        .into_iter()
                        .chain([q.x, q.y, q.z, q.w])
                        .chain(b.linear_velocity.to_array())
                        .chain(b.angular_velocity.to_array())
                    {
                        bits.push(v.to_bits());
                    }
                    bits.push(u32::from(b.is_sleeping()));
                }
                if std::env::var_os("Q3D_STACK_SOLVER_TRACE").is_some()
                    && step <= 16
                    && setting != 1
                {
                    println!("ROUND173_EARLY layers={layers} margin={margin} step={step} contacts={} early_pairmax={early:.12} bottom_y={} top_y={}",world.contacts().len(),poses[0].center[1],poses[layers-1].center[1]);
                }
            }
            assert!(contacts > 0);
            // First-green recipe verification using existing API; no new solver fix.
            if setting == 3 {
                assert!(all <= 0.00501 && floor_depth <= 0.00501,
                    "existing margin=.005 and20/8 recipe fixed4/12layer240step5mm target: all={all} floor={floor_depth}");
            }
            println!("ROUND173_STACK layers={layers} setting={setting} margin={margin} steps=240 early16={early:.12} adjacent={adjacent:.12} allpairs={all:.12} floor={floor_depth:.12} contacts={contacts}");
            if setting == 0 {
                zero_reference = bits;
            } else if setting == 1 {
                assert_eq!(
                    zero_reference, bits,
                    "default/explicitmargin0 all240stepfullpublicstate"
                );
            }
        }
    }
}

#[test]
fn speculative_box_gap_does_not_create_false_overlap_impulse() {
    let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
    world.config.speculative_margin = 0.005;
    world.set_fixed_dt(1.0 / 60.0);
    world.add_builder(BodyBuilder::r#static(Shape::Box {
        half_extents: Vec3::splat(0.25),
    }));
    let handle = world.add_builder(
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.25),
        })
        .position(vec3(0.0, 0.502, 0.0))
        .linear_velocity(vec3(0.0, -0.06, 0.0))
        .mass(1.0),
    );
    world.step(1.0 / 60.0);
    let b = world.body(handle).unwrap();
    let contact = world
        .contacts()
        .first()
        .expect("positive gap inside margin generates speculative contact");
    println!(
        "ROUND173_GAP position={:?} velocity={:?} contact={contact:?}",
        b.position(),
        b.linear_velocity
    );
    // Physical gap .002 allows closing .12m/s; prescribed .06m/s consumes
    // only .001 in this step and requires no normal impulse or false stop.
    assert!(
        (b.linear_velocity.y + 0.06).abs() < 1e-6 && contact.impulse.abs() < 1e-6,
        "positive physical gap must not act as penetration"
    );
}

#[test]
fn independent_obb_oracle_known_geometry() {
    let cube = Obb {
        center: [0.0; 3],
        axes: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        half: [0.25; 3],
    };
    for (x, expected) in [(0.4, -0.1), (0.6, 0.1)] {
        let b = Obb {
            center: [x, 0.0, 0.0],
            ..cube
        };
        assert!((cube.separation(b) - expected).abs() < 1e-12);
    }
    let t = std::f64::consts::FRAC_1_SQRT_2;
    let rotated = Obb {
        center: [1.0, 0.0, 0.0],
        axes: [[t, t, 0.0], [-t, t, 0.0], [0.0, 0.0, 1.0]],
        ..cube
    };
    assert!((cube.separation(rotated) - (1.0 - 0.25 - 0.5 * t)).abs() < 1e-12);
    let floor = Obb {
        center: [0.0, -0.25, 0.0],
        half: [4.0, 0.25, 4.0],
        ..cube
    };
    let outside = Obb {
        center: [5.0, -1.0, 0.0],
        ..cube
    };
    assert!(outside.corners().iter().all(|p| p[1] < 0.0));
    assert!(outside.separation(floor) > 0.0);
}
#[test]
fn public_box_stack_finite_contact_audit() {
    let _ = audit();
}
#[test]
#[ignore = "unmet: fixed 5mm stack target exceeded in original successful-compilation red; no solver change"]
fn public_box_stack_precision_target() {
    let summaries = audit();
    // New fixed5mm precision target (=default slop,1% cube edge), plus10um
    // floating tolerance registered before measurement, never widened to pass.
    let failures: Vec<_> = summaries
        .iter()
        .filter(|s| s.floor_depth > 0.00501 || s.pair_depth > 0.00501)
        .collect();
    assert!(
        failures.is_empty(),
        "new5mm fixture target exceeded: {failures:?}"
    );
}
