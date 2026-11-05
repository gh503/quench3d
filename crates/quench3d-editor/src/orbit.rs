//! AI-authored, deterministic kinematic circles, not a gravitational solver.
use quench3d::prelude::*;
use quench3d_cli::scene_doc::{AnimationDoc, TrackDoc};

pub(crate) fn owned_name(target: &str) -> String {
    format!("ai-orbit:{target}")
}

pub(crate) fn prepare(
    target: &str,
    center: [f32; 3],
    radius: f32,
    period: f32,
    phase: f32,
    plane: &str,
) -> Result<AnimationDoc, String> {
    if center
        .iter()
        .any(|v| !v.is_finite() || v.abs() > 1_000_000.0)
        || !radius.is_finite()
        || !(0.001..=10_000.0).contains(&radius)
        || !period.is_finite()
        || !(0.1..=86_400.0).contains(&period)
        || !phase.is_finite()
        || phase.abs() > 360_000.0
    {
        return Err("center must be finite within +/-1000000m; radius 0.001..10000m, period_secs 0.1..86400 and phase_deg +/-360000".into());
    }
    if !matches!(plane, "xz" | "xy" | "yz") {
        return Err("plane must be xz, xy or yz".into());
    }
    let mut values = Vec::with_capacity(257);
    let mut times = Vec::with_capacity(257);
    for i in 0..=256 {
        let angle = phase.rem_euclid(360.0).to_radians() + std::f32::consts::TAU * i as f32 / 256.0;
        let (sin, cos) = angle.sin_cos();
        let mut p = center;
        let (a, b) = match plane {
            "xy" => (0, 1),
            "yz" => (1, 2),
            _ => (0, 2),
        };
        p[a] += radius * cos;
        p[b] += radius * sin;
        times.push(period * i as f32 / 256.0);
        values.push(p);
    }
    values[256] = values[0];
    Ok(AnimationDoc {
        name: Some(owned_name(target)),
        target: target.into(),
        start: Some(0.0),
        speed: Some(1.0),
        looping: Some(true),
        weight: Some(1.0),
        position: Some(TrackDoc { times, values }),
        rotation: None,
        position_relative_to: None,
        show_path: None,
    })
}

/// Reject any cycle in the authored relative-position graph, including when
/// multiple tracks for a target introduce more than one dependency.
pub(crate) fn validate_dependencies(animations: &[AnimationDoc]) -> Result<(), String> {
    fn visit<'a>(
        name: &'a str,
        animations: &'a [AnimationDoc],
        visiting: &mut std::collections::HashSet<&'a str>,
        done: &mut std::collections::HashSet<&'a str>,
    ) -> Result<(), String> {
        if done.contains(name) {
            return Ok(());
        }
        if !visiting.insert(name) {
            return Err(format!("Relative orbit cycle includes {name}"));
        }
        for center in animations
            .iter()
            .filter(|a| a.target == name)
            .filter_map(|a| a.position_relative_to.as_deref())
        {
            visit(center, animations, visiting, done)?;
        }
        visiting.remove(name);
        done.insert(name);
        Ok(())
    }
    let mut visiting = std::collections::HashSet::new();
    let mut done = std::collections::HashSet::new();
    for animation in animations {
        visit(&animation.target, animations, &mut visiting, &mut done)?;
    }
    Ok(())
}

/// Rebuild mass properties through BodyBuilder, preserving public material/filter data.
/// Never just change body_type: that leaves stale inverse mass/inertia behind.
pub(crate) fn convert_body(old: &RigidBody, mode: BodyType) -> RigidBody {
    let builder = match mode {
        BodyType::Dynamic => BodyBuilder::dynamic(old.shape.clone()),
        BodyType::Static => BodyBuilder::r#static(old.shape.clone()),
        BodyType::Kinematic => BodyBuilder::kinematic(old.shape.clone()),
    };
    let mut body = builder
        .transform(old.transform())
        .mass(old.mass())
        .can_sleep(old.can_sleep())
        .build();
    body.restitution = old.restitution;
    body.friction = old.friction;
    body.static_friction = old.static_friction;
    body.rolling_friction = old.rolling_friction;
    body.friction_axis = old.friction_axis;
    body.friction_anisotropy = old.friction_anisotropy;
    body.friction_combine = old.friction_combine;
    body.restitution_combine = old.restitution_combine;
    body.linear_damping = old.linear_damping;
    body.angular_damping = old.angular_damping;
    body.gravity_scale = old.gravity_scale;
    body.user_data = old.user_data;
    body.groups = old.groups;
    body.sensor = old.sensor;
    body.ccd = old.ccd;
    body.ccd_precise = old.ccd_precise;
    // A body mode switch is a stationary edit, not an impulse. Both velocities zero.
    body.linear_velocity = Vec3::ZERO;
    body.angular_velocity = Vec3::ZERO;
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn circle_has_closed_loop_and_phase() {
        let a = prepare("earth", [1.0, 2.0, 3.0], 5.0, 20.0, 90.0, "xz").unwrap();
        let track = a.position.unwrap();
        assert_eq!(track.values.len(), 257);
        assert_eq!(track.times[256], 20.0);
        assert_eq!(track.values[0], track.values[256]);
        assert!((track.values[0][2] - 8.0).abs() < 1e-5);
        for p in track.values {
            let d = Vec3::from_array(p) - Vec3::new(1.0, 2.0, 3.0);
            assert!((d.length() - 5.0).abs() < 1e-5);
            assert_eq!(p[1], 2.0);
        }
        let outer = prepare("outer", [0.0; 3], 10.0, 80.0, 0.0, "xy").unwrap();
        assert_eq!(outer.position.unwrap().times[256], 80.0);
    }
    #[test]
    fn rejects_bad_orbits() {
        for (r, p, phase, plane) in [
            (0.0, 1.0, 0.0, "xz"),
            (1.0, 0.0, 0.0, "xz"),
            (1.0, 1.0, f32::NAN, "xz"),
            (1.0, 1.0, 0.0, "bad"),
        ] {
            assert!(prepare("x", [0.0; 3], r, p, phase, plane).is_err());
        }
    }
    #[test]
    fn mode_rebuild_zeroes_inverse_mass_and_preserves_material() {
        let mut old = BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
            .mass(3.0)
            .build();
        old.friction = 0.23;
        old.restitution = 0.81;
        old.gravity_scale = 0.4;
        old.user_data = 19;
        old.sensor = true;
        let kine = convert_body(&old, BodyType::Kinematic);
        assert_eq!(kine.inv_mass(), 0.0);
        assert_eq!(kine.friction, old.friction);
        assert_eq!(kine.restitution, old.restitution);
        assert_eq!(kine.gravity_scale, old.gravity_scale);
        assert_eq!(kine.user_data, 19);
        assert!(kine.sensor);
        let fixed = convert_body(&old, BodyType::Static);
        assert_eq!(fixed.inv_mass(), 0.0);
        let dynamic = convert_body(&old, BodyType::Dynamic);
        assert!((dynamic.mass() - 3.0).abs() < 1e-6);
        assert!(dynamic.inv_mass() > 0.0);
    }
}
