use quench3d_math::{vec3, Vec3};
use quench3d_physics::{BodyBuilder, PhysicsWorld, Shape};
fn sample(layers: usize, global: bool) -> f64 {
    let mut w = PhysicsWorld::new();
    w.set_fixed_dt(1.0 / 60.0);
    w.config.global_normal_projection = global;
    w.config.speculative_margin = 0.005;
    w.config.velocity_iterations = 20;
    w.config.position_iterations = 8;
    w.add_builder(
        BodyBuilder::r#static(Shape::Box {
            half_extents: vec3(4.0, 0.25, 4.0),
        })
        .position(vec3(0.0, -0.25, 0.0)),
    );
    for i in 0..layers {
        w.add_builder(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::splat(0.25),
            })
            .position(vec3(0.0, 0.25 + i as f32 * 0.502, 0.0))
            .mass(1.0),
        );
    }
    for _ in 0..8 {
        w.step(1.0 / 60.0);
    }
    let start = std::time::Instant::now();
    for _ in 0..120 {
        w.step(1.0 / 60.0);
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert_eq!(w.step_count, 128);
    std::hint::black_box(
        w.bodies()
            .map(|(_, b)| (b.position(), b.linear_velocity))
            .collect::<Vec<_>>(),
    );
    elapsed
}
fn main() {
    let mut passed = true;
    for layers in [12, 32] {
        let mut old = Vec::new();
        let mut new = Vec::new();
        for trial in 0..5 {
            let order = if trial % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            };
            for global in order {
                let seconds = sample(layers, global);
                println!("ROUND195_COST layers={layers} trial={trial} global={global} warm=8 measured=120 seconds={seconds:.9}");
                if global {
                    new.push(seconds);
                } else {
                    old.push(seconds);
                }
            }
        }
        old.sort_by(f64::total_cmp);
        new.sort_by(f64::total_cmp);
        let ratio = new[2] / old[2];
        println!("ROUND195_COST_RESULT layers={layers} baseline_median={:.9} candidate_median={:.9} ratio={ratio:.6} limit=1.15 pass={}",old[2],new[2],ratio<=1.15);
        passed &= ratio <= 1.15;
    }
    if !passed {
        std::process::exit(1);
    }
}
