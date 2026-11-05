//! Cached, animation-only playback of persistent scene keyframes.
//!
//! The runtime owns no scene copy. Only authored animation tracks and resolved
//! target handles are cached; moving bodies never invalidates the playback clock.
use std::collections::BTreeMap;

use quench3d::prelude::*;
use quench3d_cli::{
    runner::Script,
    scene_doc::{AnimationDoc, SceneDoc},
};

#[derive(Default)]
pub(crate) struct Runtime {
    animations: Vec<AnimationDoc>,
    handles: BTreeMap<String, BodyHandle>,
    script: Option<Script>,
    prepared: bool,
    last_step: u64,
}

impl Runtime {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Returns a diagnostic only when a changed configuration fails to compile.
    pub(crate) fn prepare(&mut self, engine: &Engine, base: Option<&SceneDoc>) -> Option<String> {
        let animations = base.map_or(&[][..], |doc| doc.animations.as_slice());
        let handles = engine
            .scene
            .objects
            .iter()
            .filter_map(|object| {
                let handle = object.body?;
                (animations.iter().any(|a| {
                    a.target == object.name
                        || a.position_relative_to.as_deref() == Some(object.name.as_str())
                }) && engine.world.body(handle).is_some())
                .then(|| (object.name.clone(), handle))
            })
            .collect();
        self.prepare_tracks(&engine.world, animations, handles)
    }

    fn prepare_tracks(
        &mut self,
        world: &PhysicsWorld,
        animations: &[AnimationDoc],
        handles: BTreeMap<String, BodyHandle>,
    ) -> Option<String> {
        if self.prepared
            && self.animations == animations
            && self.handles == handles
            && world.step_count >= self.last_step
        {
            self.last_step = world.step_count;
            return None;
        }
        self.animations = animations.to_vec();
        self.handles = handles;
        self.prepared = true;
        self.last_step = world.step_count;
        self.script = None;
        if animations.is_empty() {
            return None;
        }
        // In particular, never replay the saved document's actions or probes.
        let mut animation_doc = SceneDoc::from_json("{}").expect("empty scene document");
        animation_doc.animations = self.animations.clone();
        match Script::new(&animation_doc, world, &self.handles) {
            Ok(script) => {
                self.script = Some(script);
                None
            }
            Err(error) => Some(format!("动画无法播放：{error}")),
        }
    }

    /// Script owns the physics step too: calling world.step afterwards doubles time.
    pub(crate) fn step(&mut self, world: &mut PhysicsWorld, dt: f32) {
        if let Some(script) = &mut self.script {
            script.step(world, dt);
            // Use the exact weighted, dependency-resolved targets sampled by the
            // shared runtime before physics. Do not independently resample tracks.
            for &(handle, transform) in script.last_animation_targets() {
                if let Some(body) = world.body_mut(handle) {
                    body.set_transform(transform);
                }
            }
        } else {
            world.step(dt);
        }
        self.last_step = world.step_count;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quench3d_cli::runner::build_world;

    fn fixture() -> (SceneDoc, quench3d_cli::runner::WorldBuild) {
        let doc = SceneDoc::from_json(r#"{
            "gravity":[0,0,0],
            "objects":[{"name":"orbiter","body":"kinematic","shape":{"type":"sphere","radius":0.1}}],
            "animations":[{"target":"orbiter","loop":true,"position":{
                "times":[0,0.25,0.5,0.75,1],
                "values":[[1,0,0],[0,0,1],[-1,0,0],[0,0,-1],[1,0,0]]}}]
        }"#).unwrap();
        let build = build_world(&doc).unwrap();
        (doc, build)
    }

    #[test]
    fn cached_motion_loops_and_preparing_while_paused_does_not_advance() {
        let (doc, mut build) = fixture();
        let mut runtime = Runtime::default();
        let handle = build.handles["orbiter"];
        for _ in 0..5 {
            assert!(runtime
                .prepare_tracks(&build.world, &doc.animations, build.handles.clone())
                .is_none());
            runtime.step(&mut build.world, 0.25);
        }
        assert_eq!(
            build.world.step_count, 5,
            "exactly one physics step per animation step"
        );
        let position = build.world.body(handle).unwrap().position();
        assert!((position - Vec3::new(0.0, 0.0, 1.0)).length() < 1e-4);
        for _ in 0..10 {
            runtime.prepare_tracks(&build.world, &doc.animations, build.handles.clone());
        }
        assert_eq!(build.world.body(handle).unwrap().position(), position);
        runtime.step(&mut build.world, 0.25);
        assert!(
            (build.world.body(handle).unwrap().position() - Vec3::new(-1.0, 0.0, 0.0)).length()
                < 1e-4
        );
        runtime.reset();
        runtime.prepare_tracks(&build.world, &doc.animations, build.handles.clone());
        runtime.step(&mut build.world, 0.25);
        assert!((build.world.body(handle).unwrap().position() - position).length() < 1e-4);
    }

    #[test]
    fn nested_relative_tracks_follow_centers_in_reverse_authoring_order() {
        let doc = SceneDoc::from_json(r#"{
            "gravity":[0,0,0],
            "objects":[
                {"name":"sun","body":"static","position":[10,4,0],"shape":{"type":"sphere","radius":0.1}},
                {"name":"earth","body":"kinematic","position":[12,4,0],"shape":{"type":"sphere","radius":0.1}},
                {"name":"moon","body":"kinematic","position":[12.5,4,0],"shape":{"type":"sphere","radius":0.1}}],
            "animations":[
                {"target":"moon","position_relative_to":"earth","loop":true,"position":{"times":[0,0.25,0.5,0.75,1],"values":[[0.5,0,0],[0,0,0.5],[-0.5,0,0],[0,0,-0.5],[0.5,0,0]]}},
                {"target":"earth","position_relative_to":"sun","loop":true,"position":{"times":[0,0.25,0.5,0.75,1],"values":[[2,0,0],[0,0,2],[-2,0,0],[0,0,-2],[2,0,0]]}}]
        }"#).unwrap();
        let mut build = build_world(&doc).unwrap();
        let mut runtime = Runtime::default();
        assert!(runtime
            .prepare_tracks(&build.world, &doc.animations, build.handles.clone())
            .is_none());
        for step in 1..=12 {
            runtime.step(&mut build.world, 0.25);
            let earth = build.world.body(build.handles["earth"]).unwrap().position();
            let moon = build.world.body(build.handles["moon"]).unwrap().position();
            assert!(((earth - Vec3::new(10.0, 4.0, 0.0)).length() - 2.0).abs() < 1e-4);
            assert!(((moon - earth).length() - 0.5).abs() < 1e-4, "step {step}");
            if step == 1 {
                assert!((moon - Vec3::new(10.0, 4.0, 2.5)).length() < 1e-4);
            }
        }
    }

    #[test]
    fn relative_tracks_reject_missing_self_and_cycles() {
        let (mut doc, build) = fixture();
        for center in ["missing", "orbiter"] {
            doc.animations[0].position_relative_to = Some(center.into());
            let mut runtime = Runtime::default();
            assert!(runtime
                .prepare_tracks(&build.world, &doc.animations, build.handles.clone())
                .is_some());
        }
        let mut a = doc.animations[0].clone();
        a.target = "a".into();
        a.position_relative_to = Some("b".into());
        let mut b = a.clone();
        b.target = "b".into();
        b.position_relative_to = Some("a".into());
        assert!(crate::orbit::validate_dependencies(&[a, b]).is_err());
    }

    #[test]
    fn invalid_target_reports_once_and_physics_keeps_running() {
        let (doc, mut build) = fixture();
        let mut runtime = Runtime::default();
        assert!(runtime
            .prepare_tracks(&build.world, &doc.animations, BTreeMap::new())
            .is_some());
        assert!(runtime
            .prepare_tracks(&build.world, &doc.animations, BTreeMap::new())
            .is_none());
        runtime.step(&mut build.world, 1.0 / 60.0);
        assert_eq!(build.world.step_count, 1);
    }
}
