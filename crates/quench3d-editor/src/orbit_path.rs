//! Opt-in orbit guides drawn as transient depth-tested lines, never scene objects.
use crate::doc::Doc;
use quench3d::{
    prelude::{Vec3, Vec4},
    Engine,
};
use quench3d_cli::scene_doc::AnimationDoc;

const MAX_PATHS: usize = 256;
const MAX_SEGMENTS: usize = 65_536;

/// Draw saved AI-owned paths after the scene has synchronized its render poses.
pub(crate) fn draw(engine: &mut Engine, doc: &Doc) {
    let Some(base) = doc.base_doc() else {
        return;
    };
    let segments = collect_segments(&base.animations, |name, require_visible| {
        let mut matches = engine
            .scene
            .objects
            .iter()
            .enumerate()
            .filter(|(_, object)| object.name == name);
        let (index, object) = matches.next()?;
        if matches.next().is_some() || (require_visible && !object.visible) {
            return None;
        }
        Some(engine.scene.world_transform(index).position)
    });
    let color = Vec4::new(0.3, 0.65, 1.0, 0.65);
    for (start, end) in segments {
        engine.renderer.lines.line(start, end, color);
    }
}

/// Resolve the center once per path so every vertex follows the same rendered pose.
/// The resolver also validates a unique visible target, independently of its center.
fn collect_segments(
    animations: &[AnimationDoc],
    mut resolve: impl FnMut(&str, bool) -> Option<Vec3>,
) -> Vec<(Vec3, Vec3)> {
    let mut segments = Vec::new();
    let mut paths = 0;
    for animation in animations {
        if !animation.show_path.unwrap_or(false)
            || animation.name.as_deref()
                != Some(crate::orbit::owned_name(&animation.target).as_str())
        {
            continue;
        }
        if paths == MAX_PATHS || segments.len() == MAX_SEGMENTS {
            break;
        }
        if resolve(&animation.target, true).is_none() {
            continue;
        }
        let Some(track) = &animation.position else {
            continue;
        };
        let center = match animation.position_relative_to.as_deref() {
            Some(name) => match resolve(name, false) {
                Some(position) => position,
                None => continue,
            },
            None => Vec3::ZERO,
        };
        if !finite(center) {
            continue;
        }
        paths += 1;
        for pair in track.values.windows(2) {
            if segments.len() == MAX_SEGMENTS {
                break;
            }
            let start = Vec3::from_array(pair[0]) + center;
            let end = Vec3::from_array(pair[1]) + center;
            if finite(start) && finite(end) {
                segments.push((start, end));
            }
        }
    }
    segments
}

fn finite(point: Vec3) -> bool {
    point.x.is_finite() && point.y.is_finite() && point.z.is_finite()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn orbit(target: &str, center: Option<&str>) -> AnimationDoc {
        let mut animation = crate::orbit::prepare(target, [0.0; 3], 2.0, 4.0, 0.0, "xz").unwrap();
        animation.show_path = Some(true);
        animation.position_relative_to = center.map(str::to_owned);
        animation
    }

    #[test]
    fn reversed_earth_moon_paths_follow_current_center_and_stay_closed() {
        let animations = [orbit("月球", Some("地球")), orbit("地球", None)];
        let center = Vec3::new(8.0, 5.0, -3.0);
        let first = collect_segments(&animations, |name, _| {
            Some(if name == "地球" { center } else { Vec3::ZERO })
        });
        assert_eq!(first.len(), 512);
        assert!((first[0].0 - (center + Vec3::new(2.0, 0.0, 0.0))).length() < 1e-5);
        assert!((first[0].0 - first[255].1).length() < 1e-5);
        assert!((first[256].0 - first[511].1).length() < 1e-5);
        let delta = Vec3::new(-4.0, 0.0, 7.0);
        let moved = collect_segments(&animations, |name, _| {
            Some(if name == "地球" {
                center + delta
            } else {
                Vec3::ZERO
            })
        });
        for (old, new) in first[..256].iter().zip(&moved[..256]) {
            assert!((new.0 - old.0 - delta).length() < 1e-5);
            assert!((new.1 - old.1 - delta).length() < 1e-5);
        }
        assert_eq!(&first[256..], &moved[256..]);
    }

    #[test]
    fn hidden_unowned_missing_and_nonfinite_paths_are_skipped() {
        let mut animation = orbit("月球", Some("地球"));
        for flag in [None, Some(false)] {
            animation.show_path = flag;
            assert!(collect_segments(&[animation.clone()], |_, _| Some(Vec3::ZERO)).is_empty());
        }
        animation.show_path = Some(true);
        assert!(
            collect_segments(&[animation.clone()], |name, _| (name != "地球")
                .then_some(Vec3::ZERO))
            .is_empty()
        );
        assert!(collect_segments(&[animation.clone()], |_, target| (!target)
            .then_some(Vec3::ZERO))
        .is_empty());
        animation.name = Some("authored-animation".into());
        assert!(collect_segments(&[animation.clone()], |_, _| Some(Vec3::ZERO)).is_empty());
        animation.name = Some(crate::orbit::owned_name("月球"));
        animation.position.as_mut().unwrap().values =
            vec![[0.0; 3], [f32::NAN, 0.0, 0.0], [1.0; 3], [2.0; 3]];
        assert_eq!(
            collect_segments(&[animation.clone()], |_, _| Some(Vec3::ZERO)).len(),
            1
        );
        assert!(collect_segments(&[animation], |_, _| Some(Vec3::new(
            f32::INFINITY,
            0.0,
            0.0
        )))
        .is_empty());
    }

    #[test]
    fn line_generation_is_bounded_without_changing_animation_data() {
        let animation = orbit("地球", None);
        let animations = vec![animation; MAX_PATHS + 10];
        let saved = serde_json::to_value(&animations).unwrap();
        assert_eq!(
            collect_segments(&animations, |_, _| Some(Vec3::ZERO)).len(),
            MAX_SEGMENTS
        );
        assert_eq!(serde_json::to_value(&animations).unwrap(), saved);
        let mut long: AnimationDoc = serde_json::from_value(json!({
            "name":crate::orbit::owned_name("地球"), "target":"地球", "show_path":true,
            "position":{"times":[0.0,1.0],"values":[[0.0,0.0,0.0],[1.0,0.0,0.0]]}
        }))
        .unwrap();
        long.position.as_mut().unwrap().values = vec![[0.0; 3]; MAX_SEGMENTS + 20];
        assert_eq!(
            collect_segments(&[long], |_, _| Some(Vec3::ZERO)).len(),
            MAX_SEGMENTS
        );
    }
}
