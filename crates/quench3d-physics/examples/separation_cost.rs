//! Fixed full-step production-library measurements, without cfg(test) probes.
//! No latency budget or universal performance claim. Distinct policies can evolve
//! distinct states; only the free-space control is expected to agree exactly.
use quench3d_math::{vec3, Vec3};
use quench3d_physics::{
    BodyBuilder, FluidCollisionMode, FluidDesc, FluidSeparationMode, PhysicsWorld, Shape,
};
use std::time::Instant;

fn fixture(side: usize, geometry: usize, mode: FluidSeparationMode) -> PhysicsWorld {
    let mut world = PhysicsWorld::with_gravity(if geometry == 0 {
        Vec3::ZERO
    } else {
        vec3(0.0, -9.81, 0.0)
    });
    world.set_fixed_dt(1.0 / 60.0);
    // Exact initial counts 5^3/10^3; same spacing/radius, scaled room for large cloud.
    let half = side as f32 * 0.05 + 0.25;
    let height = 2.0 * half;
    if geometry != 0 {
        let mut walls = vec![(vec3(0.0, -0.05, 0.0), vec3(half + 0.1, 0.05, half + 0.1))];
        if geometry == 2 {
            walls.extend([
                (
                    vec3(0.0, height + 0.05, 0.0),
                    vec3(half + 0.1, 0.05, half + 0.1),
                ),
                (
                    vec3(-half - 0.05, height * 0.5, 0.0),
                    vec3(0.05, height * 0.5 + 0.1, half + 0.1),
                ),
                (
                    vec3(half + 0.05, height * 0.5, 0.0),
                    vec3(0.05, height * 0.5 + 0.1, half + 0.1),
                ),
                (
                    vec3(0.0, height * 0.5, -half - 0.05),
                    vec3(half + 0.1, height * 0.5 + 0.1, 0.05),
                ),
                (
                    vec3(0.0, height * 0.5, half + 0.05),
                    vec3(half + 0.1, height * 0.5 + 0.1, 0.05),
                ),
            ]);
        }
        for (p, h) in walls {
            world.add_builder(BodyBuilder::r#static(Shape::Box { half_extents: h }).position(p));
        }
    }
    let mut positions = Vec::new();
    for z in 0..side {
        for y in 0..side {
            for x in 0..side {
                positions.push(vec3(
                    (x as f32 - (side - 1) as f32 * 0.5) * 0.1,
                    0.04 + y as f32 * 0.1,
                    (z as f32 - (side - 1) as f32 * 0.5) * 0.1,
                ));
            }
        }
    }
    world
        .add_fluid(FluidDesc {
            initial_positions: Some(positions),
            radius: 0.04,
            spacing: 0.1,
            max_particles: side.pow(3),
            initial_velocity: if geometry == 2 {
                vec3(0.15, 0.0, -0.1)
            } else {
                Vec3::ZERO
            },
            particle_collision: true,
            collision_mode: FluidCollisionMode::SweptMeshes,
            separation_mode: mode,
            ..FluidDesc::default()
        })
        .unwrap();
    world
}

fn main() {
    if cfg!(debug_assertions) {
        panic!("run this fixed plan with --release");
    }
    println!("# production-library full public world.step timing; 8 warmup +32 measured; independent world/trial; 5 samples; LN/NL alternation; no clock budget; different modes may evolve different states");
    println!("kind,particles,geometry,trial,order,mode,us_per_step,final_particles,checksum");
    for side in [5usize, 10] {
        for geometry in 0..3 {
            let mut samples = [Vec::new(), Vec::new()];
            for trial in 0..5 {
                let order = if trial % 2 == 0 { [0, 1] } else { [1, 0] };
                for slot in order {
                    let mode = if slot == 0 {
                        FluidSeparationMode::Legacy
                    } else {
                        FluidSeparationMode::BoundaryConstrained
                    };
                    let mut world = fixture(side, geometry, mode);
                    for _ in 0..8 {
                        world.step(1.0 / 60.0);
                    }
                    let start = Instant::now();
                    for _ in 0..32 {
                        world.step(1.0 / 60.0);
                    }
                    let us = start.elapsed().as_secs_f64() * 1e6 / 32.0;
                    let f = &world.fluids()[0];
                    assert_eq!(f.particles().len(), side.pow(3));
                    assert!(f
                        .particles()
                        .iter()
                        .chain(f.velocities())
                        .all(|p| p.is_finite()));
                    let checksum = f
                        .particles()
                        .iter()
                        .flat_map(|p| p.to_array())
                        .fold(0u64, |a, v| {
                            a.wrapping_mul(31).wrapping_add(u64::from(v.to_bits()))
                        });
                    samples[slot].push(us);
                    println!(
                        "sample,{},{geometry},{trial},{}{},{},{us:.6},{},{checksum}",
                        side.pow(3),
                        if order[0] == 0 { "L" } else { "N" },
                        if order[1] == 0 { "L" } else { "N" },
                        if slot == 0 {
                            "Legacy"
                        } else {
                            "BoundaryConstrained"
                        },
                        f.particles().len()
                    );
                    std::hint::black_box(world);
                }
            }
            for s in &mut samples {
                s.sort_by(f64::total_cmp);
            }
            println!(
                "median,{},{geometry},-,-,Legacy,{:.6},-,-",
                side.pow(3),
                samples[0][2]
            );
            println!(
                "median,{},{geometry},-,-,BoundaryConstrained,{:.6},-,-",
                side.pow(3),
                samples[1][2]
            );
            println!(
                "ratio,{},{geometry},-,-,new_over_legacy,{:.6},-,-",
                side.pow(3),
                samples[1][2] / samples[0][2]
            );
        }
    }
}
