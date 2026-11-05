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
    time: f32,
    tracks: Vec<(BodyHandle, quench3d::animation::Animation, f32)>,
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
                (animations.iter().any(|a| a.target == object.name)
                    && engine.world.body(handle).is_some())
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
        self.tracks.clear();
        self.time = 0.0;
        if animations.is_empty() {
            return None;
        }
        // In particular, never replay the saved document's actions or probes.
        let mut animation_doc = SceneDoc::from_json("{}").expect("empty scene document");
        animation_doc.animations = self.animations.clone();
        match Script::new(&animation_doc, world, &self.handles) {
            Ok(script) => {
                // Script has already validated these exact tracks.
                for a in animations {
                    let position = a.position.as_ref().map(|track| {
                        quench3d::animation::Track::new(
                            track.times.clone(),
                            track
                                .values
                                .iter()
                                .map(|v| Vec3::new(v[0], v[1], v[2]))
                                .collect(),
                        )
                        .expect("validated position track")
                    });
                    let rotation = a.rotation.as_ref().map(|track| {
                        quench3d::animation::RotationTrack::new(
                            track.times.clone(),
                            track
                                .values
                                .iter()
                                .map(|v| Vec3::new(v[0], v[1], v[2]))
                                .collect(),
                        )
                        .expect("validated rotation track")
                    });
                    let animation = quench3d::animation::Animation::new(
                        a.name.clone(),
                        a.target.clone(),
                        a.start.unwrap_or(0.0),
                        a.speed.unwrap_or(1.0),
                        a.looping.unwrap_or(false),
                        position,
                        rotation,
                    )
                    .expect("validated animation");
                    self.tracks
                        .push((self.handles[&a.target], animation, a.weight.unwrap_or(1.0)));
                }
                self.script = Some(script);
                None
            }
            Err(error) => Some(format!("动画无法播放：{error}")),
        }
    }

    /// Script owns the physics step too: calling world.step afterwards doubles time.
    pub(crate) fn step(&mut self, world: &mut PhysicsWorld, dt: f32) {
        if let Some(script) = &mut self.script {
            self.time += dt;
            // The shared Script supplies physical velocities for moving contacts.
            // Physics also integrates kinematic velocities; restore the exact
            // authored pose afterwards so it cannot overshoot the sampled path.
            let mut poses: Vec<(BodyHandle, Transform)> = Vec::new();
            for (handle, animation, weight) in &self.tracks {
                let Some(body) = world.body(*handle) else {
                    continue;
                };
                let (position, rotation) = animation.sample(self.time);
                if position.is_none() && rotation.is_none() {
                    continue;
                }
                let index = match poses.iter().position(|(h, _)| h == handle) {
                    Some(index) => index,
                    None => {
                        poses.push((*handle, body.transform()));
                        poses.len() - 1
                    }
                };
                let transform = &mut poses[index].1;
                if let Some(position) = position {
                    transform.position =
                        quench3d::animation::blend_position(transform.position, position, *weight);
                }
                if let Some(rotation) = rotation {
                    transform.rotation =
                        quench3d::animation::blend_rotation(transform.rotation, rotation, *weight);
                }
            }
            script.step(world, dt);
            for (handle, transform) in poses {
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
