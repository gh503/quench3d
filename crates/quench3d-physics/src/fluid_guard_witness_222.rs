//! Diagnostic only: one precomputed frozen witness through the existing guard.
use super::*;
use crate::{BodyBuilder, PhysicsWorld, Shape};
use std::fmt::Write;

fn integer(text: &str) -> u64 {
    text.parse().expect("valid integer bits")
}

fn number(text: &str) -> f64 {
    let value = f64::from_bits(integer(text));
    assert!(value.is_finite(), "finite captured f64");
    value
}

fn narrow(text: &str) -> f32 {
    let value = f32::from_bits(u32::try_from(integer(text)).expect("u32 bits"));
    assert!(value.is_finite(), "finite captured f32");
    value
}

#[test]
#[ignore = "Round222 frozen witness diagnosis; not trajectory acceptance"]
fn frozen_witness_through_existing_guard() {
    let snapshot = std::fs::read_to_string(std::env::var("QUENCH222_SNAPSHOT").unwrap()).unwrap();
    let witness = std::fs::read_to_string(std::env::var("QUENCH222_WITNESS").unwrap()).unwrap();
    let mut points = Vec::new();
    let mut inverse = Vec::new();
    let mut rows: Vec<(f64, Vec<(usize, f64)>)> = Vec::new();
    let mut coefficients = Vec::new();
    let mut radius = None;
    for line in snapshot.lines() {
        let f: Vec<_> = line.split('\t').collect();
        match f[0] {
            "ENTRY" => {
                assert_eq!(f.len(), 5);
                radius = Some(narrow(f[4]));
            }
            "PREDICTED" => {
                assert_eq!(f.len(), 5);
                assert_eq!(integer(f[1]) as usize, points.len());
                points.push(Vec3::new(narrow(f[2]), narrow(f[3]), narrow(f[4])));
            }
            "Y" => {
                assert_eq!(f.len(), 6);
                assert_eq!(integer(f[1]) as usize, inverse.len());
                let value = number(f[4]);
                assert!(value > 0.0);
                inverse.push(value);
            }
            "ROW" => {
                assert_eq!(f.len(), 6);
                assert_eq!(integer(f[1]) as usize, rows.len());
                rows.push((number(f[2]), Vec::new()));
            }
            "COEFF" => {
                assert_eq!(f.len(), 4);
                coefficients.push((integer(f[1]) as usize, integer(f[2]) as usize, number(f[3])));
            }
            _ => {}
        }
    }
    assert_eq!(points.len(), 125);
    assert_eq!(inverse.len(), 375);
    assert_eq!(rows.len(), 8500);
    for (row, coordinate, value) in coefficients {
        assert!(row < rows.len() && coordinate < inverse.len());
        rows[row].1.push((coordinate, value));
    }
    for (_, row) in &rows {
        assert!(!row.is_empty());
        let aa: f64 = row.iter().map(|(_, a)| a * a).sum();
        assert!(aa.is_finite() && aa > 0.0);
    }
    let mut y = Vec::new();
    for line in witness.lines() {
        let f: Vec<_> = line.split('\t').collect();
        assert_eq!(f.len(), 3);
        assert_eq!(f[0], "WITNESS");
        assert_eq!(integer(f[1]) as usize, y.len());
        y.push(number(f[2]));
    }
    assert_eq!(y.len(), 375);
    let radius = radius.unwrap();
    assert_eq!(radius.to_bits(), 0.04f32.to_bits());
    let mut world = PhysicsWorld::with_gravity(Vec3::new(0.0, -9.81, 0.0));
    for (position, half_extents) in [
        (Vec3::new(0.0, -0.05, 0.0), Vec3::new(0.6, 0.05, 0.6)),
        (Vec3::new(0.0, 1.05, 0.0), Vec3::new(0.6, 0.05, 0.6)),
        (Vec3::new(-0.55, 0.5, 0.0), Vec3::new(0.05, 0.6, 0.6)),
        (Vec3::new(0.55, 0.5, 0.0), Vec3::new(0.05, 0.6, 0.6)),
        (Vec3::new(0.0, 0.5, -0.55), Vec3::new(0.6, 0.6, 0.05)),
        (Vec3::new(0.0, 0.5, 0.55), Vec3::new(0.6, 0.6, 0.05)),
    ] {
        world.add_builder(BodyBuilder::r#static(Shape::Box { half_extents }).position(position));
    }
    let desc = FluidDesc {
        center: Vec3::new(0.0, 0.5, 0.0),
        half_extents: Vec3::splat(0.2),
        spacing: 0.1,
        radius,
        max_particles: 2000,
        initial_velocity: Vec3::new(0.15, 0.0, -0.1),
        particle_collision: true,
        collision_mode: FluidCollisionMode::SweptMeshes,
        separation_mode: FluidSeparationMode::GlobalHalfspaceExperimental,
        ..FluidDesc::default()
    };
    assert_eq!(desc.iterations, 4);
    let context = SweepQueryContext {
        mesh_cache: None,
        bounds: None,
        index: None,
        meshes: true,
    };
    let mut output = String::from("IDENTITY\tRound222\t6c0f4dca0a1691423dfe62d783b52c8fc856be1ffbc87fad4a82eab7068c982e\tfa8e7d886a5936a4bdb34d7f9eb763485c7ddb2f6e89258712aa46d4bada83d4\n");
    let source_hash =
        std::env::var("QUENCH222_SOURCE_HASH").expect("frozen test source hash required");
    assert_eq!(source_hash.len(), 64);
    writeln!(output,"PROVENANCE\tparent_command_hashes_verified_inputs\t9892918ecdd23c0b67387aeb0d3270472aa176853ef95bd2bed935bbe94a523a\t{source_hash}").unwrap();
    writeln!(output, "RADIUS\t{}", radius.to_bits()).unwrap();
    for coordinate in 0..375 {
        writeln!(
            output,
            "WITNESS\t{coordinate}\t{}\t{}",
            y[coordinate].to_bits(),
            inverse[coordinate].to_bits()
        )
        .unwrap();
    }
    let mut guarded = Vec::new();
    let mut max_change = (0.0f64, 0usize);
    for (i, &point) in points.iter().enumerate() {
        let delta = Vec3::new(
            (y[3 * i] * inverse[3 * i]) as f32,
            (y[3 * i + 1] * inverse[3 * i + 1]) as f32,
            (y[3 * i + 2] * inverse[3 * i + 2]) as f32,
        );
        assert!(delta.is_finite());
        let simple = point + delta;
        let (swept, blocked) = sweep_separation_slide(
            point,
            delta,
            world.fluid_test_body_slots(),
            radius,
            &desc,
            context,
        );
        let final_point = collide_position(
            swept,
            swept,
            world.fluid_test_body_slots(),
            radius,
            desc.boundary_velocity_mode,
            desc.boundary_motion_mode,
        )
        .0;
        assert!(final_point.is_finite());
        let distance = distance64(simple, final_point);
        if distance > max_change.0 {
            max_change = (distance, i);
        }
        for (tag, p) in [
            ("INPUT", point),
            ("DELTA", delta),
            ("SIMPLE", simple),
            ("GUARDED", final_point),
        ] {
            let b = p.to_array().map(f32::to_bits);
            writeln!(output, "{tag}\t{i}\t{}\t{}\t{}", b[0], b[1], b[2]).unwrap();
        }
        writeln!(output, "BLOCKED\t{i}\t{blocked}").unwrap();
        guarded.push(final_point);
    }
    let mut actual_y = Vec::with_capacity(375);
    for i in 0..125 {
        for axis in 0..3 {
            actual_y.push(
                (f64::from(guarded[i].to_array()[axis]) - f64::from(points[i].to_array()[axis]))
                    / inverse[3 * i + axis],
            );
        }
    }
    let mut worst_row = (f64::NEG_INFINITY, 0usize);
    let mut pair_rows_bad = 0;
    let mut wall_rows_bad = 0;
    for (r, (rhs, cs)) in rows.iter().enumerate() {
        let residual = rhs - cs.iter().map(|&(i, a)| a * actual_y[i]).sum::<f64>();
        assert!(residual.is_finite());
        if residual > worst_row.0 {
            worst_row = (residual, r);
        }
        if residual > 1e-7 {
            if r < 7750 {
                pair_rows_bad += 1;
            } else {
                wall_rows_bad += 1;
            }
        }
        writeln!(output, "RESIDUAL\t{r}\t{}", residual.to_bits()).unwrap();
    }
    let mut worst_pair = (f64::INFINITY, 0usize, 1usize);
    let mut pair_bad = 0;
    for i in 0..125 {
        for j in i + 1..125 {
            let distance = distance64(guarded[i], guarded[j]);
            if distance < worst_pair.0 {
                worst_pair = (distance, i, j);
            }
            if distance < 2.0 * f64::from(radius) - 1e-4 {
                pair_bad += 1;
            }
        }
    }
    let mut worst_face = (f64::INFINITY, 0usize, 0usize);
    let mut face_bad = 0;
    let mut wall_particles_bad = 0;
    for (i, p) in guarded.iter().enumerate() {
        let [x, py, z] = p.to_array().map(f64::from);
        let mut particle_bad = false;
        for (face, clearance) in [x + 0.5, 0.5 - x, py, 1.0 - py, z + 0.5, 0.5 - z]
            .into_iter()
            .enumerate()
        {
            let clearance = clearance - f64::from(radius);
            if clearance < worst_face.0 {
                worst_face = (clearance, i, face);
            }
            if clearance < -1e-4 {
                face_bad += 1;
                particle_bad = true;
            }
        }
        wall_particles_bad += usize::from(particle_bad);
    }
    writeln!(
        output,
        "ROW_SUMMARY\t{}\t{}\t{pair_rows_bad}\t{wall_rows_bad}",
        worst_row.0.to_bits(),
        worst_row.1
    )
    .unwrap();
    writeln!(
        output,
        "PAIR_SUMMARY\t{}\t{}\t{}\t{pair_bad}",
        worst_pair.0.to_bits(),
        worst_pair.1,
        worst_pair.2
    )
    .unwrap();
    writeln!(
        output,
        "FACE_SUMMARY\t{}\t{}\t{}\t{face_bad}\t{wall_particles_bad}",
        worst_face.0.to_bits(),
        worst_face.1,
        worst_face.2
    )
    .unwrap();
    writeln!(
        output,
        "GUARD_CHANGE\t{}\t{}",
        max_change.0.to_bits(),
        max_change.1
    )
    .unwrap();
    writeln!(
        output,
        "DIAGNOSIS\t125\t8500\t7750\t750\tfinite\tno_world_step\tno_optimizer"
    )
    .unwrap();
    std::fs::write(std::env::var("QUENCH222_OUTPUT").unwrap(), output)
        .expect("write actual guard evidence");
    println!("Round222 diagnostic completed: rowmax={} pairrowsbad={pair_rows_bad} wallrowsbad={wall_rows_bad} minpair={} pairbad={pair_bad} minclearance={} facebad={face_bad} maxguardchange={}; guard certificate failures are diagnostic results, not trajectory acceptance",worst_row.0,worst_pair.0,worst_face.0,max_change.0);
}

fn distance64(a: Vec3, b: Vec3) -> f64 {
    let a = a.to_array().map(f64::from);
    let b = b.to_array().map(f64::from);
    (0..3)
        .map(|axis| (a[axis] - b[axis]).powi(2))
        .sum::<f64>()
        .sqrt()
}
