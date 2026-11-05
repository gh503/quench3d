//! Shared editor tool contract for native and Web agent sessions.
//!
//! `inspect_scene` and `set_playback` are control tools and do not require an
//! expected revision. All other commands require the revision returned by the
//! latest successful tool. Scene edits should follow pause, inspect, then edit;
//! a running simulation can invalidate a scene fingerprint.
use quench3d::Engine;
use serde_json::{json, Value};

use crate::doc::Doc;

/// OpenAI-compatible function catalog, identical on native and wasm targets.
/// Numeric and target validation is repeated by the live executor.
pub fn tools() -> Value {
    let definitions = [
        (
            "inspect_scene",
            "Inspect named scene objects and obtain the current revision.",
            json!({}),
            vec![],
        ),
        (
            "spawn_primitive",
            "Create one independently editable primitive.",
            json!({"name":{"type":"string"},"shape":{"type":"object"},"position":{"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3},"rotation":{"type":"array","items":{"type":"number"},"minItems":4,"maxItems":4},"color":{"type":"array","items":{"type":"number"},"minItems":4,"maxItems":4}}),
            vec!["name", "shape"],
        ),
        (
            "set_transform",
            "Set the named object's position or quaternion without changing unrelated bodies. Existing animations override position during playback; clear_orbit first to stop an orbit.",
            json!({"name":{"type":"string"},"position":{"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3},"rotation":{"type":"array","items":{"type":"number"},"minItems":4,"maxItems":4}}),
            vec!["name"],
        ),
        (
            "set_shape",
            "Set a supported standalone primitive's physical dimensions.",
            json!({"name":{"type":"string"},"shape":{"type":"object"}}),
            vec!["name", "shape"],
        ),
        (
            "set_color",
            "Set the named object's RGBA color.",
            json!({"name":{"type":"string"},"color":{"type":"array","items":{"type":"number"},"minItems":4,"maxItems":4}}),
            vec!["name", "color"],
        ),
        (
            "set_object_label",
            "Show or hide the named object's visible name label in the viewport. Saved with the scene and follows moving objects; Chinese names are supported.",
            json!({"name":{"type":"string"},"visible":{"type":"boolean"}}),
            vec!["name", "visible"],
        ),
        (
            "set_orbit",
            "Animate a standalone primitive in a stable circular kinematic orbit (not gravity). Resume playback to animate. Rejects conflicting non-orbit tracks.",
            json!({"name":{"type":"string"},"center":{"type":"array","items":{"type":"number"},"minItems":3,"maxItems":3},"radius":{"type":"number","minimum":0.001,"maximum":10000},"period_secs":{"type":"number","minimum":0.1,"maximum":86400},"phase_deg":{"type":"number","minimum":-360000,"maximum":360000},"plane":{"type":"string","enum":["xz","xy","yz"]}}),
            vec!["name","center","radius","period_secs"],
        ),
        (
            "clear_orbit",
            "Remove the AI-owned orbit track. The object remains stationary kinematic; use set_body_mode to restore dynamics if desired.",
            json!({"name":{"type":"string"}}), vec!["name"],
        ),
        (
            "set_body_mode",
            "Set a standalone primitive to static (fixed anchor), dynamic (gravity), or kinematic. Resets velocities; clear conflicting animations before static/dynamic mode.",
            json!({"name":{"type":"string"},"mode":{"type":"string","enum":["static","dynamic","kinematic"]}}), vec!["name","mode"],
        ),
        (
            "delete_object",
            "Delete one supported standalone named object.",
            json!({"name":{"type":"string"}}),
            vec!["name"],
        ),
        (
            "select_object",
            "Select the uniquely named object.",
            json!({"name":{"type":"string"}}),
            vec!["name"],
        ),
        (
            "focus_object",
            "Focus the camera on the uniquely named object.",
            json!({"name":{"type":"string"}}),
            vec!["name"],
        ),
        (
            "set_playback",
            "Pause before inspecting and editing; this control intentionally requires no revision.",
            json!({"paused":{"type":"boolean"}}),
            vec!["paused"],
        ),
        (
            "step",
            "Advance exactly one paused simulation step.",
            json!({}),
            vec![],
        ),
        (
            "undo",
            "Undo one supported scene edit using editor history.",
            json!({}),
            vec![],
        ),
        (
            "redo",
            "Redo one supported scene edit using editor history.",
            json!({}),
            vec![],
        ),
    ];
    Value::Array(definitions.into_iter().map(|(name,description,mut properties,required)| {
        if properties.get("shape").is_some() {
            properties["shape"] = json!({"oneOf":[
                {"type":"object","properties":{"type":{"const":"box"},"size":{"type":"array","items":{"type":"number","minimum":0.0001,"maximum":10000},"minItems":3,"maxItems":3}},"required":["type","size"],"additionalProperties":false},
                {"type":"object","properties":{"type":{"const":"sphere"},"radius":{"type":"number","minimum":0.0001,"maximum":10000}},"required":["type","radius"],"additionalProperties":false},
                {"type":"object","properties":{"type":{"enum":["cylinder","cone"]},"radius":{"type":"number","minimum":0.0001,"maximum":10000},"height":{"type":"number","minimum":0.0001,"maximum":10000}},"required":["type","radius","height"],"additionalProperties":false}
            ]});
        }
        let mut required:Vec<Value>=required.into_iter().map(Value::from).collect();
        if !matches!(name,"inspect_scene"|"set_playback") {
            properties["expected_revision"]=json!({"type":"string"});
            required.push(json!("expected_revision"));
        }
        json!({"type":"function","function":{"name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}}})
    }).collect())
}

type Failure = (&'static str, String);
fn invalid(message: impl Into<String>) -> Failure {
    ("invalid_arguments", message.into())
}

/// Execute a validated tool against the live scene. Revisions are state
/// fingerprints, not monotonic epochs or security credentials.
pub fn dispatch(engine: &mut Engine, doc: &mut Doc, name: &str, args: Value) -> Value {
    let result = execute(engine, doc, name, args);
    let revision = fingerprint(engine, doc);
    match result {
        Ok(result) => json!({"ok":true,"result":result,"revision":revision}),
        Err((code, message)) => {
            json!({"ok":false,"error":{"code":code,"message":message},"revision":revision})
        }
    }
}

fn fingerprint(engine: &Engine, doc: &Doc) -> String {
    let objects:Vec<_>=engine.scene.objects.iter().enumerate().map(|(i,o)| {
        let t=engine.scene.world_transform(i);
        json!({"name":o.name,"position":t.position.to_array(),"rotation":[t.rotation.x,t.rotation.y,t.rotation.z,t.rotation.w],"scale":t.scale.to_array(),"color":o.color.to_array(),"body":format!("{:?}",o.body)})
    }).collect();
    let state = json!({"scene":crate::scene_io::scene_to_json(engine,doc).ok().map(|s|s.json),"objects":objects,"paused":engine.paused,"selection":doc.selection(),"orbit":format!("{:?}",engine.orbit),"fly":format!("{:?}",engine.fly),"mode":format!("{:?}",engine.camera_mode),"history":[doc.history.len(),usize::from(doc.history.can_undo()),usize::from(doc.history.can_redo())]});
    let mut hash = 0xcbf29ce484222325u64;
    for byte in state.to_string().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn array<const N: usize>(value: &Value, label: &str) -> Result<[f32; N], Failure> {
    let values = value
        .as_array()
        .filter(|a| a.len() == N)
        .ok_or_else(|| invalid(format!("{label} must contain {N} numbers")))?;
    let mut out = [0.0; N];
    for (slot, value) in out.iter_mut().zip(values) {
        *slot = value
            .as_f64()
            .ok_or_else(|| invalid(format!("{label} must be numeric")))? as f32;
        if !slot.is_finite() {
            return Err(invalid(format!("{label} must be finite")));
        }
    }
    Ok(out)
}
fn object_name(value: &Value) -> Result<&str, Failure> {
    value
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.chars().count() <= 128)
        .ok_or_else(|| invalid("name must contain 1..128 characters"))
}
fn resolve(engine: &Engine, name: &str) -> Result<usize, Failure> {
    let mut matches = engine
        .scene
        .objects
        .iter()
        .enumerate()
        .filter(|(_, o)| o.name == name);
    let index = matches
        .next()
        .map(|(i, _)| i)
        .ok_or_else(|| ("not_found", format!("No object named {name}")))?;
    if matches.next().is_some() {
        return Err((
            "ambiguous_name",
            format!("Object name {name} is not unique"),
        ));
    }
    Ok(index)
}
fn shape(value: &Value) -> Result<crate::doc::PrimitiveShape, Failure> {
    use crate::doc::PrimitiveShape;
    let map = value
        .as_object()
        .ok_or_else(|| invalid("shape must be an object"))?;
    let kind = map
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("shape.type required"))?;
    let allowed: &[&str] = match kind {
        "box" => &["type", "size"],
        "sphere" => &["type", "radius"],
        "cylinder" | "cone" => &["type", "radius", "height"],
        _ => return Err(invalid("unsupported primitive type")),
    };
    if map.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(invalid("unknown shape field"));
    }
    let positive = |key: &str| -> Result<f32, Failure> {
        let n = map
            .get(key)
            .and_then(Value::as_f64)
            .ok_or_else(|| invalid(format!("shape.{key} required")))? as f32;
        if !n.is_finite() || !(1e-4..=1e4).contains(&n) {
            return Err(invalid(
                "shape dimensions must be within 0.0001..10000 meters",
            ));
        }
        Ok(n)
    };
    Ok(match kind {
        "box" => {
            let size = quench3d::prelude::Vec3::from_array(array(&value["size"], "shape.size")?);
            if size.min_element() < 1e-4 || size.max_element() > 1e4 {
                return Err(invalid("box size must be within 0.0001..10000 meters"));
            }
            PrimitiveShape::Box { size }
        }
        "sphere" => PrimitiveShape::Sphere {
            radius: positive("radius")?,
        },
        "cylinder" => PrimitiveShape::Cylinder {
            radius: positive("radius")?,
            height: positive("height")?,
        },
        _ => PrimitiveShape::Cone {
            radius: positive("radius")?,
            height: positive("height")?,
        },
    })
}
fn shape_json(spec: crate::doc::PrimitiveShape) -> Value {
    use crate::doc::PrimitiveShape::*;
    match spec {
        Box { size } => json!({"type":"box","size":size.to_array()}),
        Sphere { radius } => json!({"type":"sphere","radius":radius}),
        Cylinder { radius, height } => json!({"type":"cylinder","radius":radius,"height":height}),
        Cone { radius, height } => json!({"type":"cone","radius":radius,"height":height}),
    }
}
fn editable(engine: &Engine, doc: &Doc, index: usize) -> Result<(), Failure> {
    let o = &engine.scene.objects[index];
    if doc.model_of_object(index).is_some()
        || o.owner_part.is_some()
        || o.generated
        || o.fluid_surface.is_some()
        || o.fluid_particle.is_some()
        || engine.scene.is_particle_object(index)
        || o.parent.is_some()
        || !engine.scene.children_of(index).is_empty()
        || o.body
            .and_then(|h| engine.world.body(h))
            .is_some_and(|b| !b.extra_parts().is_empty())
    {
        return Err(("unsupported_target","Model members, generated objects, compounds and object hierarchies require dedicated tools".into()));
    }
    Ok(())
}
fn history_ready(engine: &Engine, doc: &Doc) -> Result<(), Failure> {
    if doc.history.in_transaction() {
        return Err((
            "history_busy",
            "Finish the current editor transaction first".into(),
        ));
    }
    let saved =
        crate::scene_io::scene_to_json(engine, doc).map_err(|e| ("unsupported_history", e))?;
    if !saved.skipped.is_empty() {
        return Err((
            "unsupported_history",
            format!(
                "Scene history cannot represent: {}",
                saved.skipped.join(", ")
            ),
        ));
    }
    Ok(())
}
fn inspect(engine: &Engine, doc: &Doc) -> Value {
    let objects:Vec<_>=engine.scene.objects.iter().enumerate().map(|(i,o)|{
        let t=engine.scene.world_transform(i);
        let body=o.body.and_then(|h|engine.world.body(h));
        let orbit=doc.base_doc().and_then(|d|d.animations.iter().find(|a|a.target==o.name && a.name.as_deref()==Some(crate::orbit::owned_name(&o.name).as_str())));
        json!({"name":o.name,"position":t.position.to_array(),"rotation":[t.rotation.x,t.rotation.y,t.rotation.z,t.rotation.w],"color":o.color.to_array(),"label_visible":doc.label_visible(&o.name),"body_position":body.map(|b|b.position().to_array()),"body_type":body.map(|b|format!("{:?}",b.body_type).to_lowercase()),"velocity":body.map(|b|b.linear_velocity.to_array()),"gravity_scale":body.map(|b|b.gravity_scale),"orbit":orbit.map(|a|json!({"name":a.name,"period_secs":a.position.as_ref().and_then(|p|p.times.last()).copied(),"loop":a.looping,"motion":"kinematic animation, not gravity"})),"shape":doc.editable_shape(engine,i).ok().map(shape_json),"capabilities":{"edit":editable(engine,doc,i).is_ok(),"set_shape":doc.editable_shape(engine,i).is_ok(),"set_orbit":editable(engine,doc,i).is_ok() && doc.editable_shape(engine,i).is_ok() && body.is_some(),"set_body_mode":editable(engine,doc,i).is_ok() && doc.editable_shape(engine,i).is_ok() && body.is_some()}})
    }).collect();
    json!({"objects":objects,"paused":engine.paused,"selection":doc.selection().into_iter().filter_map(|i|engine.scene.objects.get(i).map(|o|o.name.clone())).collect::<Vec<_>>(),"history":{"len":doc.history.len(),"can_undo":doc.history.can_undo(),"can_redo":doc.history.can_redo()}})
}
fn execute(
    engine: &mut Engine,
    doc: &mut Doc,
    command: &str,
    args: Value,
) -> Result<Value, Failure> {
    use quench3d::prelude::*;
    let fields: &[&str] = match command {
        "inspect_scene" => &[],
        "set_playback" => &["paused"],
        "spawn_primitive" => &[
            "name",
            "shape",
            "position",
            "rotation",
            "color",
            "expected_revision",
        ],
        "set_transform" => &["name", "position", "rotation", "expected_revision"],
        "set_shape" => &["name", "shape", "expected_revision"],
        "set_color" => &["name", "color", "expected_revision"],
        "set_object_label" => &["name", "visible", "expected_revision"],
        "set_orbit" => &[
            "name",
            "center",
            "radius",
            "period_secs",
            "phase_deg",
            "plane",
            "expected_revision",
        ],
        "clear_orbit" => &["name", "expected_revision"],
        "set_body_mode" => &["name", "mode", "expected_revision"],
        "delete_object" | "select_object" | "focus_object" => &["name", "expected_revision"],
        "step" | "undo" | "redo" => &["expected_revision"],
        _ => return Err(("unknown_tool", format!("Unknown editor command {command}"))),
    };
    let map = args
        .as_object()
        .ok_or_else(|| invalid("arguments must be an object"))?;
    if map.keys().any(|k| !fields.contains(&k.as_str())) {
        return Err(invalid("unknown argument field"));
    }
    if command == "inspect_scene" {
        return Ok(inspect(engine, doc));
    }
    if command == "set_playback" {
        let paused = args["paused"]
            .as_bool()
            .ok_or_else(|| invalid("paused boolean required"))?;
        engine.paused = paused;
        return Ok(json!({"paused":paused}));
    }
    let expected = args["expected_revision"]
        .as_str()
        .ok_or_else(|| invalid("expected_revision required"))?;
    if expected != fingerprint(engine, doc) {
        return Err((
            "stale_revision",
            "Scene state changed; inspect again before retrying".into(),
        ));
    }
    if !engine.paused {
        return Err((
            "simulation_running",
            "Pause with set_playback before editing".into(),
        ));
    }
    if doc.history.in_transaction() {
        return Err((
            "history_busy",
            "Finish the current editor transaction first".into(),
        ));
    }
    let position = args
        .get("position")
        .map(|v| array::<3>(v, "position").map(Vec3::from_array))
        .transpose()?;
    let rotation = args
        .get("rotation")
        .map(|v| {
            let [x, y, z, w] = array::<4>(v, "rotation")?;
            let length = x * x + y * y + z * z + w * w;
            if !length.is_finite() || length < 1e-12 {
                return Err(invalid(
                    "rotation quaternion must have nonzero finite length",
                ));
            }
            Ok(Quat::from_xyzw(x, y, z, w).normalize_or_identity())
        })
        .transpose()?;
    let color = args
        .get("color")
        .map(|v| {
            let rgba = array::<4>(v, "color")?;
            if rgba.iter().any(|c| !(0.0..=1.0).contains(c)) {
                return Err(invalid("color components must be in 0..1"));
            }
            Ok(Vec4::from_array(rgba))
        })
        .transpose()?;
    let spec = args.get("shape").map(shape).transpose()?;
    if command == "spawn_primitive" {
        let name = object_name(&args["name"])?;
        if engine.scene.objects.iter().any(|o| o.name == name) {
            return Err(("ambiguous_name", "Spawn name already exists".into()));
        }
        let spec = spec.ok_or_else(|| invalid("shape required"))?;
        history_ready(engine, doc)?;
        doc.record_edit(engine);
        let primitive = match spec {
            crate::doc::PrimitiveShape::Box { .. } => crate::doc::Primitive::Cube,
            crate::doc::PrimitiveShape::Sphere { .. } => crate::doc::Primitive::Sphere,
            crate::doc::PrimitiveShape::Cylinder { .. } => crate::doc::Primitive::Cylinder,
            crate::doc::PrimitiveShape::Cone { .. } => crate::doc::Primitive::Cone,
        };
        let index = doc.spawn_colored(
            engine,
            primitive,
            position.unwrap_or(Vec3::new(0.0, 1.0, 0.0)),
            Vec4::ONE,
        );
        engine.scene.objects[index].name = name.to_owned();
        doc.set_selected_shape(engine, spec)
            .map_err(|e| ("edit_failed", e))?;
        if let Some(rotation) = rotation {
            engine.scene.objects[index].transform.rotation = rotation;
            let t = engine.scene.world_transform(index);
            if let Some(h) = engine.scene.objects[index].body {
                let body = engine.world.body_mut(h).unwrap();
                let mut pose = body.transform();
                pose.position = t.position;
                pose.rotation = t.rotation;
                body.set_transform(pose);
            }
        }
        engine.scene.objects[index].color = color.unwrap_or(Vec4::ONE);
        doc.status = format!("Agent: spawn {name}");
        doc.record_edit(engine);
        return Ok(json!({"name":name}));
    }
    if matches!(command, "undo" | "redo") {
        history_ready(engine, doc)?;
        if command == "undo" {
            if !doc.history.can_undo() {
                return Err(("no_history", "Nothing to undo".into()));
            }
            doc.undo(engine);
        } else {
            if !doc.history.can_redo() {
                return Err(("no_history", "Nothing to redo".into()));
            }
            doc.redo(engine);
        }
        return Ok(inspect(engine, doc));
    }
    if command == "step" {
        doc.step_once(engine);
        return Ok(json!({"paused":engine.paused}));
    }
    let name = object_name(&args["name"])?;
    let index = resolve(engine, name)?;
    if command == "select_object" {
        doc.select_only(Some(index));
        return Ok(json!({"name":name}));
    }
    if command == "focus_object" {
        doc.select_only(Some(index));
        doc.focus_selected(engine);
        return Ok(json!({"name":name}));
    }
    if command == "set_object_label" {
        let visible = args["visible"]
            .as_bool()
            .ok_or_else(|| invalid("visible boolean required"))?;
        let object = &engine.scene.objects[index];
        if object.generated
            || object.fluid_particle.is_some()
            || object.fluid_surface.is_some()
            || engine.scene.is_particle_object(index)
        {
            return Err((
                "unsupported_target",
                "Generated particles and fluid surfaces do not support labels".into(),
            ));
        }
        history_ready(engine, doc)?;
        doc.record_edit(engine);
        doc.set_label_visible(name, visible);
        doc.record_edit(engine);
        doc.status = format!("Agent: label {name} {visible}");
        return Ok(json!({"name":name,"label_visible":visible,"text":name}));
    }
    editable(engine, doc, index)?;
    if matches!(command, "set_orbit" | "clear_orbit" | "set_body_mode") {
        doc.editable_shape(engine, index)
            .map_err(|e| ("unsupported_target", e))?;
        let handle = engine.scene.objects[index]
            .body
            .ok_or_else(|| ("unsupported_target", "Target must have a rigid body".into()))?;
        let old = engine
            .world
            .body(handle)
            .ok_or_else(|| {
                (
                    "unsupported_target",
                    "Target rigid body is unavailable".into(),
                )
            })?
            .clone();
        let owned = crate::orbit::owned_name(name);
        let animations = doc
            .base_doc()
            .map(|d| d.animations.as_slice())
            .unwrap_or(&[]);
        let has_animation = animations.iter().any(|a| a.target == name);
        if command == "set_orbit"
            && animations
                .iter()
                .any(|a| a.target == name && a.name.as_deref() != Some(owned.as_str()))
        {
            return Err((
                "animation_conflict",
                "Target already has a non-orbit animation; remove it before assigning an orbit"
                    .into(),
            ));
        }
        let mut prepared = None;
        let mode = if command == "set_orbit" {
            let numeric = |key: &str| {
                args[key]
                    .as_f64()
                    .map(|n| n as f32)
                    .ok_or_else(|| invalid(format!("{key} must be a number")))
            };
            let center = array::<3>(&args["center"], "center")?;
            let phase = if args.get("phase_deg").is_some() {
                numeric("phase_deg")?
            } else {
                0.0
            };
            let plane = match args.get("plane") {
                None => "xz",
                Some(v) => v
                    .as_str()
                    .ok_or_else(|| invalid("plane must be a string"))?,
            };
            prepared = Some(
                crate::orbit::prepare(
                    name,
                    center,
                    numeric("radius")?,
                    numeric("period_secs")?,
                    phase,
                    plane,
                )
                .map_err(|e| invalid(&e))?,
            );
            BodyType::Kinematic
        } else if command == "clear_orbit" {
            if !animations
                .iter()
                .any(|a| a.target == name && a.name.as_deref() == Some(owned.as_str()))
            {
                return Err(("no_orbit", "Target has no AI-owned orbit".into()));
            }
            BodyType::Kinematic
        } else {
            let mode = match args["mode"].as_str() {
                Some("static") => BodyType::Static,
                Some("dynamic") => BodyType::Dynamic,
                Some("kinematic") => BodyType::Kinematic,
                _ => return Err(invalid("mode must be static, dynamic or kinematic")),
            };
            if mode != BodyType::Kinematic && has_animation {
                return Err((
                    "animation_conflict",
                    "Clear target animations before changing to static or dynamic".into(),
                ));
            }
            mode
        };
        history_ready(engine, doc)?;
        doc.record_edit(engine);
        if prepared.is_some() && doc.base_doc.is_none() {
            doc.base_doc = Some(
                quench3d_cli::scene_doc::SceneDoc::from_json("{}")
                    .map_err(|e| ("edit_failed", e.to_string()))?,
            );
        }
        let mut replacement = crate::orbit::convert_body(&old, mode);
        if let Some(animation) = prepared {
            let first = animation.position.as_ref().unwrap().values[0];
            let mut pose = replacement.transform();
            pose.position = Vec3::from_array(first);
            replacement.set_transform(pose);
            let mut t = engine.scene.world_transform(index);
            t.position = pose.position;
            engine.scene.set_world_transform(index, t);
            let base = doc.base_doc_mut().unwrap();
            base.animations
                .retain(|a| a.target != name || a.name.as_deref() != Some(owned.as_str()));
            base.animations.push(animation);
        } else if command == "clear_orbit" {
            if let Some(base) = doc.base_doc_mut() {
                base.animations
                    .retain(|a| a.target != name || a.name.as_deref() != Some(owned.as_str()));
            }
        }
        assert!(
            engine.world.replace_body(handle, replacement),
            "validated body"
        );
        doc.status = format!("Agent: {command} {name}");
        doc.record_edit(engine);
        return Ok(
            json!({"name":name,"body_mode":format!("{mode:?}").to_lowercase(),"kinematic_animation":command=="set_orbit","message":if command=="clear_orbit"{"Orbit removed; body remains stationary kinematic"}else{"Body mode and animation updated"}}),
        );
    }
    if command == "set_transform"
        && doc
            .base_doc()
            .is_some_and(|d| d.animations.iter().any(|a| a.target == name))
    {
        return Err((
            "animation_conflict",
            "Clear the object's orbit or animation before editing its transform".into(),
        ));
    }
    if command == "set_transform" && position.is_none() && rotation.is_none() {
        return Err(invalid("position or rotation required"));
    }
    if command == "set_color" {
        if color.is_none() {
            return Err(invalid("color required"));
        }
        if engine
            .renderer
            .material(engine.scene.objects[index].material)
            .is_none()
        {
            return Err((
                "unsupported_target",
                "Target material is unavailable".into(),
            ));
        }
    }
    if command == "set_shape" {
        if spec.is_none() {
            return Err(invalid("shape required"));
        }
        doc.editable_shape(engine, index)
            .map_err(|e| ("unsupported_target", e))?;
    }
    history_ready(engine, doc)?;
    doc.record_edit(engine);
    match command {
        "set_transform" => {
            let mut t = engine.scene.world_transform(index);
            if let Some(p) = position {
                t.position = p;
            }
            if let Some(r) = rotation {
                t.rotation = r;
            }
            engine.scene.set_world_transform(index, t);
            if let Some(h) = engine.scene.objects[index].body {
                if let Some(body) = engine.world.body_mut(h) {
                    let mut pose = body.transform();
                    pose.position = t.position;
                    pose.rotation = t.rotation;
                    body.set_transform(pose);
                }
            }
        }
        "set_color" => {
            let old = engine.scene.objects[index].material;
            let mut material = engine
                .renderer
                .material(old)
                .cloned()
                .expect("validated target material");
            // A white base already supports arbitrary object tint without an
            // allocation or any change to shared material state.
            if material.base_color != Vec4::ONE {
                material.base_color = Vec4::ONE;
                // Register the clone so ordinary object deletion also releases
                // it and its texture references. Debug includes the generation.
                let key = format!("agent:white-base:{old:?}");
                let replacement =
                    engine
                        .assets
                        .material(&mut engine.renderer, &engine.gpu, &key, material);
                engine.scene.objects[index].material = replacement;
                let still_used = engine.scene.objects.iter().any(|o| o.material == old)
                    || engine.scene.particles.iter().any(|p| p.material == old);
                if !still_used && !engine.assets.release_material(&mut engine.renderer, old) {
                    // Materials created directly in the renderer have no cache
                    // entry. The new cached clone owns its texture references.
                    engine.renderer.remove_material(old);
                }
            }
            engine.scene.objects[index].color = color.unwrap();
        }
        "set_shape" => {
            let selected = doc.selected;
            let extras = doc.extra_selected.clone();
            doc.select_only(Some(index));
            let result = doc.set_selected_shape(engine, spec.unwrap());
            doc.selected = selected;
            doc.extra_selected = extras;
            result.map_err(|e| ("edit_failed", e))?;
        }
        "delete_object" => {
            if let Some(base) = doc.base_doc_mut() {
                base.animations.retain(|a| a.target != name);
            }
            doc.select_only(Some(index));
            doc.delete_selected(engine);
        }
        _ => unreachable!(),
    }
    doc.status = format!("Agent: {command} {name}");
    doc.record_edit(engine);
    Ok(json!({"name":name}))
}
