use quench3d_math::Vec3;
use quench3d_physics::{FluidDesc, PhysicsWorld};

#[test]
fn removing_middle_fluid_preserves_survivors_and_invalid_index_is_noop() {
    let mut world = PhysicsWorld::new();
    for x in [-3.0, 0.0, 3.0] {
        world
            .add_fluid(FluidDesc {
                center: Vec3::new(x, 2.0, 0.0),
                half_extents: Vec3::splat(0.15),
                spacing: 0.15,
                initial_velocity: Vec3::new(x * 0.1, 0.2, 0.0),
                ..Default::default()
            })
            .unwrap();
    }
    world.step(1.0 / 60.0);
    let before: Vec<_> = world
        .fluids()
        .iter()
        .map(|f| {
            (
                f.desc().center,
                f.particles().to_vec(),
                f.velocities().to_vec(),
                f.revision(),
            )
        })
        .collect();
    assert!(world.remove_fluid(usize::MAX).is_none());
    assert_eq!(world.fluid_count(), 3);
    let removed = world.remove_fluid(1).expect("middle fluid must be removed");
    assert_eq!(removed.particles(), before[1].1);
    assert_eq!(world.fluid_count(), 2);
    for (fluid, old) in world.fluids().iter().zip([&before[0], &before[2]]) {
        assert_eq!(fluid.desc().center, old.0);
        assert_eq!(fluid.particles(), old.1);
        assert_eq!(fluid.velocities(), old.2);
        assert_eq!(fluid.revision(), old.3);
    }
    world.step(1.0 / 60.0);
    assert!(world.fluids().iter().all(|f| f
        .particles()
        .iter()
        .chain(f.velocities())
        .all(|p| p.is_finite())));
}
