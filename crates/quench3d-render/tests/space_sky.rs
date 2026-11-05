//! Space sky is a sparse infinite star field, rather than an atmospheric horizon.
use quench3d_math::{Quat, Vec3};
use quench3d_render::{context::with_gpu_tests, prelude::*, GpuOptions};

#[test]
fn space_sky_is_dark_sparse_and_anchored_to_world_direction() {
    let Some(result) = with_gpu_tests("procedural space sky", || {
        pollster::block_on(GpuContext::headless(
            640,
            360,
            GpuOptions {
                msaa_samples: 1,
                ..Default::default()
            },
        ))
    }) else {
        return;
    };
    // An explicitly requested GPU test must fail on device/shader errors.
    let mut gpu = result.expect("space-sky headless GPU");
    let mut renderer = Renderer::new(
        &gpu,
        RendererOptions {
            grid: false,
            shadows: false,
            sky: true,
            ..Default::default()
        },
    );
    let mut lighting = Lighting::none();
    lighting.sky.enabled = true;
    lighting.sky.space = true;
    lighting.sky.show_grid = false;
    lighting.sky.zenith = Vec3::splat(0.0002);
    lighting.sky.horizon = Vec3::new(1.0, 0.0, 0.0); // Must not draw a horizon.
    lighting.sky.ground = Vec3::splat(0.0001);
    let mut camera = Camera::perspective(640.0 / 360.0);
    camera.position = Vec3::ZERO;
    camera.rotation = Quat::IDENTITY;
    let first = renderer
        .capture(&mut gpu, 640, 360, &camera, &lighting)
        .unwrap();
    let pixels = first.rgba.chunks_exact(4).collect::<Vec<_>>();
    let dark = pixels
        .iter()
        .filter(|p| p[0].max(p[1]).max(p[2]) < 16)
        .count();
    let bright = pixels
        .iter()
        .filter(|p| p[0].max(p[1]).max(p[2]) > 70)
        .count();
    assert!(
        dark * 100 > pixels.len() * 98,
        "space must remain predominantly dark: {dark}/{}",
        pixels.len()
    );
    assert!(
        bright > 3 && bright * 100 < pixels.len(),
        "sparse visible stars, got {bright}"
    );

    camera.position = Vec3::new(4.0, -2.0, 8.0);
    let translated = renderer
        .capture(&mut gpu, 640, 360, &camera, &lighting)
        .unwrap();
    let translation_error: u64 = first
        .rgba
        .iter()
        .zip(&translated.rgba)
        .map(|(a, b)| a.abs_diff(*b) as u64)
        .sum();
    // Inverse matrix reconstruction has floating point error, but no parallax.
    assert!(
        translation_error < first.rgba.len() as u64 / 8,
        "stars must not follow camera translation: {translation_error}"
    );
    camera.rotation = Quat::from_rotation_y(0.6);
    let rotated = renderer
        .capture(&mut gpu, 640, 360, &camera, &lighting)
        .unwrap();
    let rotation_error: u64 = first
        .rgba
        .iter()
        .zip(&rotated.rgba)
        .map(|(a, b)| a.abs_diff(*b) as u64)
        .sum();
    assert!(
        rotation_error > translation_error * 4 + 1000,
        "camera rotation must reveal a different star field"
    );
    let repeated = renderer
        .capture(&mut gpu, 640, 360, &camera, &lighting)
        .unwrap();
    assert_eq!(
        rotated.rgba, repeated.rgba,
        "no animation noise or frame-dependent twinkle"
    );
}
