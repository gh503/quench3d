//! Saved visual environment presets. Geometry and physics remain scene-owned.
use quench3d::prelude::Vec3;
use quench3d::render::{FogParams, Lighting};
use serde_json::{json, Value};

#[derive(Clone, Copy)]
pub(crate) enum Preset {
    Space,
    Studio,
}

impl Preset {
    pub(crate) fn parse(value: &Value) -> Result<Self, &'static str> {
        match value.as_str() {
            Some("space") => Ok(Self::Space),
            Some("studio") => Ok(Self::Studio),
            _ => Err("preset must be space or studio"),
        }
    }
}

pub(crate) fn apply(light: &mut Lighting, preset: Preset) {
    match preset {
        Preset::Space => {
            light.sky.enabled = true;
            light.sky.space = true;
            light.sky.show_grid = false;
            light.sky.zenith = Vec3::new(0.0005, 0.0007, 0.0015);
            light.sky.horizon = Vec3::new(0.0007, 0.0008, 0.0018);
            light.sky.ground = Vec3::new(0.0003, 0.0004, 0.0010);
            light.fog = FogParams::default();
            light.ambient_color = Vec3::new(0.65, 0.72, 1.0);
            light.ambient_intensity = 0.08;
            light.exposure = 1.0;
        }
        Preset::Studio => {
            let defaults = Lighting::default();
            light.sky = defaults.sky;
            light.fog = defaults.fog;
            light.ambient_color = defaults.ambient_color;
            light.ambient_intensity = defaults.ambient_intensity;
            light.exposure = defaults.exposure;
        }
    }
}

pub(crate) fn inspect(light: &Lighting, sky_enabled: bool, grid_enabled: bool) -> Value {
    json!({
        "preset": if light.sky.space { "space" } else { "studio" },
        "space": light.sky.space,
        "sky_enabled": light.sky.enabled && sky_enabled,
        "grid_enabled": light.sky.show_grid && grid_enabled,
        "sky_colors": {"zenith":light.sky.zenith.to_array(),"horizon":light.sky.horizon.to_array(),"ground":light.sky.ground.to_array()},
        "ambient":light.ambient_intensity,
        "ambient_color":light.ambient_color.to_array(),
        "exposure":light.exposure,
        "fog":{"mode":format!("{:?}",light.fog.mode).to_lowercase(),"density":light.fog.density,"start":light.fog.linear_start,"end":light.fog.linear_end}
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn space_is_dark_gridless_fogless_and_keeps_authored_lights() {
        let mut lighting = Lighting::default();
        let sun = lighting.sun;
        lighting.fog = FogParams::default().with_linear(1.0, 2.0);
        apply(&mut lighting, Preset::Space);
        assert!(lighting.sky.enabled && lighting.sky.space);
        assert!(!lighting.sky.show_grid);
        assert!(lighting.sky.zenith.max_element() < 0.002);
        assert_eq!(lighting.fog.density, 0.0);
        assert_eq!(lighting.fog.linear_end, 0.0);
        assert_eq!(lighting.ambient_intensity, 0.08);
        assert_eq!(lighting.sun.map(|s| s.intensity), sun.map(|s| s.intensity));
        assert_eq!(inspect(&lighting, true, true)["preset"], "space");
        assert_eq!(inspect(&lighting, true, true)["grid_enabled"], false);
    }

    #[test]
    fn studio_restores_visual_defaults_and_invalid_preset_is_rejected() {
        let mut lighting = Lighting::default();
        apply(&mut lighting, Preset::Space);
        apply(&mut lighting, Preset::Studio);
        let defaults = Lighting::default();
        assert!(!lighting.sky.space);
        assert_eq!(lighting.sky.zenith, defaults.sky.zenith);
        assert_eq!(lighting.sky.show_grid, defaults.sky.show_grid);
        assert_eq!(lighting.ambient_intensity, defaults.ambient_intensity);
        assert_eq!(lighting.exposure, defaults.exposure);
        for input in [json!(null), json!(true), json!("ocean")] {
            assert!(Preset::parse(&input).is_err());
        }
    }
}
