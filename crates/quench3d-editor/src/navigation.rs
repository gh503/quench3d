//! Editor camera mode transitions; engine defaults remain unchanged.
use quench3d::prelude::*;

pub(crate) fn fly_from_orbit(engine: &mut Engine) {
    let camera = engine.orbit.camera(1.0);
    let forward = camera.forward();
    engine.fly.position = camera.position;
    engine.fly.yaw = (-forward.x).atan2(-forward.z);
    engine.fly.pitch = forward.y.clamp(-1.0, 1.0).asin();
    engine.fly.fov_y = camera.fov_y;
    engine.set_camera_clip_override(Some((camera.near, camera.far)));
}

pub(crate) fn initialize(engine: &mut Engine) {
    if engine.orbit.orthographic_height.is_none() {
        fly_from_orbit(engine);
        engine.camera_mode = CameraMode::Fly;
    }
}

pub(crate) fn switch(engine: &mut Engine, mode: CameraMode, center: Option<Vec3>) {
    if engine.camera_mode == mode {
        return;
    }
    match mode {
        CameraMode::Fly => fly_from_orbit(engine),
        CameraMode::Orbit => {
            let camera = engine.fly.camera(1.0);
            let fallback = camera.position + camera.forward() * engine.orbit.distance.max(0.2);
            let center = center
                .filter(|c| c.is_finite() && (*c - camera.position).length() > 0.001)
                .unwrap_or(fallback);
            let offset = camera.position - center;
            engine.orbit.focus = center;
            engine.orbit.distance = offset.length();
            let dir = offset.normalize_or_zero();
            engine.orbit.yaw = dir.x.atan2(dir.z);
            engine.orbit.pitch = dir.y.clamp(-1.0, 1.0).asin();
            engine.orbit.fov_y = camera.fov_y;
            engine.orbit.orthographic_height = None;
        }
    }
    engine.camera_mode = mode;
}

pub(crate) fn focus_bounds(engine: &mut Engine, bounds: &Aabb) {
    let mode = engine.camera_mode;
    if mode == CameraMode::Fly {
        switch(engine, CameraMode::Orbit, None);
    }
    engine.orbit.focus_on(bounds);
    if mode == CameraMode::Fly {
        switch(engine, CameraMode::Fly, None);
    }
}
