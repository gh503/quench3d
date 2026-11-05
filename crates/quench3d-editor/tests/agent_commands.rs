#![cfg(not(target_arch = "wasm32"))]

use quench3d::prelude::*;
use quench3d_editor::{commands, doc::Doc, scene_io};
use serde_json::{json, Value};

fn fixture() -> Option<(Engine, Doc)> {
    if std::env::var("QUENCH3D_GPU_TESTS").as_deref() != Ok("1") {
        eprintln!("SKIPPED agent command GPU fixture: set QUENCH3D_GPU_TESTS=1");
        return None;
    }
    // Opt-in failures are failures: an unavailable GPU must not masquerade as a pass.
    let gpu = pollster::block_on(quench3d::render::GpuContext::headless(
        64,
        64,
        Default::default(),
    ))
    .expect("agent command GPU fixture");
    let mut engine = Engine::new(gpu, &EngineConfig::default());
    let mut doc = Doc::new();
    scene_io::load_text_into_doc(&mut engine,&mut doc,r#"{"gravity":[0,0,0],"objects":[{"name":"target","body":"dynamic","position":[0,1,0],"shape":{"type":"box","size":[1,1,1]}},{"name":"moving bystander","body":"dynamic","position":[4,1,0],"velocity":[2,0,0],"shape":{"type":"sphere","radius":0.3}}]}"#).unwrap();
    engine.paused = true;
    Some((engine, doc))
}

fn call(engine: &mut Engine, doc: &mut Doc, name: &str, args: Value) -> Value {
    let response = commands::dispatch(engine, doc, name, args);
    assert_eq!(response["ok"], true, "{name}: {response}");
    assert!(
        response["revision"].as_str().is_some_and(|r| !r.is_empty()),
        "revision required: {response}"
    );
    response
}

fn revision(engine: &mut Engine, doc: &mut Doc) -> Value {
    call(engine, doc, "inspect_scene", json!({}))["revision"].clone()
}

#[test]
fn repeated_color_edits_reuse_materials_and_release_registered_clone() {
    let Some((mut engine, mut doc)) = fixture() else {
        return;
    };
    let original = engine.scene.objects[0].material;
    let mut colored = engine.renderer.material(original).unwrap().clone();
    colored.base_color = Vec4::new(0.2, 0.4, 0.8, 1.0);
    let shared = engine.assets.material(
        &mut engine.renderer,
        &engine.gpu,
        "agent-test-shared-colored",
        colored.clone(),
    );
    engine.scene.objects[0].material = shared;
    engine.scene.objects[1].material = shared;
    let bystander_color = engine.scene.objects[1].color;
    let baseline = engine.renderer.material_count();
    let mut rev = revision(&mut engine, &mut doc);
    for i in 0..64 {
        let rgba = Vec4::new(i as f32 / 64.0, 0.3, 0.7, 1.0);
        let result = call(
            &mut engine,
            &mut doc,
            "set_color",
            json!({"name":"target","color":rgba.to_array(),"expected_revision":rev}),
        );
        rev = result["revision"].clone();
        assert_eq!(engine.renderer.material_count(), baseline + 1);
        assert_eq!(engine.scene.objects[0].color, rgba);
        assert_eq!(
            engine
                .renderer
                .material(engine.scene.objects[0].material)
                .unwrap()
                .base_color,
            Vec4::ONE
        );
        assert_eq!(engine.scene.objects[1].material, shared);
        assert_eq!(engine.scene.objects[1].color, bystander_color);
        assert_eq!(engine.renderer.material(shared).unwrap(), &colored);
    }
    call(
        &mut engine,
        &mut doc,
        "delete_object",
        json!({"name":"target","expected_revision":rev}),
    );
    assert_eq!(engine.renderer.material_count(), baseline);
    assert_eq!(engine.renderer.material(shared).unwrap(), &colored);

    // Commands spawn white-base primitives: this common path needs no clone.
    let rev = revision(&mut engine, &mut doc);
    let spawned = call(
        &mut engine,
        &mut doc,
        "spawn_primitive",
        json!({"name":"white base","shape":{"type":"sphere","radius":0.4},"expected_revision":rev}),
    );
    let white_count = engine.renderer.material_count();
    let mut rev = spawned["revision"].clone();
    for i in 0..64 {
        rev = call(&mut engine, &mut doc, "set_color",
            json!({"name":"white base","color":[0.2,i as f32 / 64.0,0.8,1],"expected_revision":rev}))["revision"].clone();
        assert_eq!(engine.renderer.material_count(), white_count);
    }
}

#[test]
fn command_catalog_and_live_transform_history_roundtrip() {
    let Some((mut engine, mut doc)) = fixture() else {
        return;
    };
    let catalog = commands::tools();
    assert!(catalog
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["function"]["name"] == "set_transform"));
    let bystander = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "moving bystander")
        .unwrap()
        .body
        .unwrap();
    let before_velocity = engine.world.body(bystander).unwrap().linear_velocity;
    let old = revision(&mut engine, &mut doc);
    let moved = call(
        &mut engine,
        &mut doc,
        "set_transform",
        json!({"name":"target","position":[1,2,3],"expected_revision":old}),
    );
    let target = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "target")
        .unwrap();
    assert_eq!(target.transform.position, Vec3::new(1.0, 2.0, 3.0));
    assert_eq!(
        engine
            .world
            .body(target.body.unwrap())
            .unwrap()
            .transform()
            .position,
        Vec3::new(1.0, 2.0, 3.0)
    );
    assert_eq!(
        engine.world.body(bystander).unwrap().linear_velocity,
        before_velocity,
        "ordinary edit must preserve unrelated live velocity and handle"
    );
    let undone = call(
        &mut engine,
        &mut doc,
        "undo",
        json!({"expected_revision":moved["revision"]}),
    );
    assert_eq!(
        engine
            .scene
            .objects
            .iter()
            .find(|o| o.name == "target")
            .unwrap()
            .transform
            .position,
        Vec3::new(0.0, 1.0, 0.0)
    );
    call(
        &mut engine,
        &mut doc,
        "redo",
        json!({"expected_revision":undone["revision"]}),
    );
    assert_eq!(
        engine
            .scene
            .objects
            .iter()
            .find(|o| o.name == "target")
            .unwrap()
            .transform
            .position,
        Vec3::new(1.0, 2.0, 3.0)
    );
}

#[test]
fn commands_validate_before_mutation_and_reject_stale_or_ambiguous_names() {
    let Some((mut engine, mut doc)) = fixture() else {
        return;
    };
    let rev = revision(&mut engine, &mut doc);
    let before = scene_io::scene_to_json(&engine, &doc).unwrap().json;
    for radius in [1e-40, 1e38] {
        let rejected = commands::dispatch(
            &mut engine,
            &mut doc,
            "spawn_primitive",
            json!({"name":"invalid size","shape":{"type":"sphere","radius":radius},"expected_revision":rev}),
        );
        assert_eq!(rejected["error"]["code"], "invalid_arguments");
        assert_eq!(engine.scene.objects.len(), 2);
        assert_eq!(scene_io::scene_to_json(&engine, &doc).unwrap().json, before);
    }
    let invalid = commands::dispatch(
        &mut engine,
        &mut doc,
        "set_transform",
        json!({"name":"target","position":[1,2],"expected_revision":rev}),
    );
    assert_eq!(invalid["ok"], false);
    assert_eq!(invalid["error"]["code"], "invalid_arguments");
    assert_eq!(scene_io::scene_to_json(&engine, &doc).unwrap().json, before);
    let changed = call(
        &mut engine,
        &mut doc,
        "select_object",
        json!({"name":"target","expected_revision":rev}),
    );
    assert_ne!(changed["revision"], rev);
    let stale = commands::dispatch(
        &mut engine,
        &mut doc,
        "set_color",
        json!({"name":"target","color":[1,0,0,1],"expected_revision":rev}),
    );
    assert_eq!(stale["error"]["code"], "stale_revision");
    engine.scene.objects[1].name = "target".into();
    let fresh = revision(&mut engine, &mut doc);
    let ambiguous = commands::dispatch(
        &mut engine,
        &mut doc,
        "delete_object",
        json!({"name":"target","expected_revision":fresh}),
    );
    assert_eq!(ambiguous["error"]["code"], "ambiguous_name");
    assert_eq!(engine.scene.objects.len(), 2);
}

#[test]
fn commands_spawn_shape_color_delete_and_playback_use_names() {
    let Some((mut engine, mut doc)) = fixture() else {
        return;
    };
    let rev = revision(&mut engine, &mut doc);
    let spawned = call(
        &mut engine,
        &mut doc,
        "spawn_primitive",
        json!({"name":"agent ball","shape":{"type":"sphere","radius":0.4},"position":[0,3,0],"color":[0.2,0.4,0.8,1],"expected_revision":rev}),
    );
    assert_eq!(spawned["result"]["name"], "agent ball");
    let moved = call(
        &mut engine,
        &mut doc,
        "set_transform",
        json!({"name":"agent ball","position":[1,3,2],"rotation":[0,std::f32::consts::FRAC_1_SQRT_2,0,std::f32::consts::FRAC_1_SQRT_2],"expected_revision":spawned["revision"]}),
    );
    let ball = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "agent ball")
        .unwrap();
    let body = engine.world.body(ball.body.unwrap()).unwrap();
    assert_eq!(body.transform().scale, Vec3::ONE);
    assert_eq!(ball.transform.scale, Vec3::splat(0.8));
    assert!(
        matches!(body.shape,quench3d::physics::Shape::Sphere{radius} if (radius-0.4).abs()<1e-6)
    );
    let shaped = call(
        &mut engine,
        &mut doc,
        "set_shape",
        json!({"name":"agent ball","shape":{"type":"sphere","radius":0.6},"expected_revision":moved["revision"]}),
    );
    let object = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "agent ball")
        .unwrap();
    assert!(
        matches!(engine.world.body(object.body.unwrap()).unwrap().shape,quench3d::physics::Shape::Sphere{radius} if (radius-0.6).abs()<1e-6)
    );
    let colored = call(
        &mut engine,
        &mut doc,
        "set_color",
        json!({"name":"agent ball","color":[1,0,0,1],"expected_revision":shaped["revision"]}),
    );
    assert_eq!(
        engine
            .scene
            .objects
            .iter()
            .find(|o| o.name == "agent ball")
            .unwrap()
            .color,
        Vec4::new(1.0, 0.0, 0.0, 1.0)
    );
    let focused = call(
        &mut engine,
        &mut doc,
        "focus_object",
        json!({"name":"agent ball","expected_revision":colored["revision"]}),
    );
    let stepped = call(
        &mut engine,
        &mut doc,
        "step",
        json!({"expected_revision":focused["revision"]}),
    );
    assert!(engine.paused);
    call(
        &mut engine,
        &mut doc,
        "delete_object",
        json!({"name":"agent ball","expected_revision":stepped["revision"]}),
    );
    assert!(!engine.scene.objects.iter().any(|o| o.name == "agent ball"));
    call(
        &mut engine,
        &mut doc,
        "set_playback",
        json!({"paused":false}),
    );
    assert!(!engine.paused);
    call(
        &mut engine,
        &mut doc,
        "set_playback",
        json!({"paused":true}),
    );
    assert!(engine.paused);
}

#[test]
fn orbit_tools_persist_run_continuously_and_clear_to_stationary_kinematic() {
    let Some((mut engine, mut doc)) = fixture() else {
        return;
    };
    let bystander_before = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "moving bystander")
        .unwrap()
        .transform
        .position;
    let mut rev = revision(&mut engine, &mut doc);
    rev = call(
        &mut engine,
        &mut doc,
        "set_body_mode",
        json!({
            "name":"target", "mode":"static", "expected_revision":rev
        }),
    )["revision"]
        .clone();
    let target = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "target")
        .unwrap();
    let body = engine.world.body(target.body.unwrap()).unwrap();
    assert_eq!(body.body_type, quench3d::physics::BodyType::Static);
    assert_eq!(body.inv_mass(), 0.0);
    assert_eq!(body.linear_velocity(), Vec3::ZERO);

    let orbit = call(
        &mut engine,
        &mut doc,
        "set_orbit",
        json!({
            "name":"target", "center":[0,5,0], "radius":3.0, "period_secs":4.0,
            "phase_deg":0.0, "plane":"xz", "expected_revision":rev
        }),
    );
    rev = orbit["revision"].clone();
    assert_eq!(orbit["result"]["body_mode"], "kinematic");
    assert_eq!(orbit["result"]["kinematic_animation"], true);
    let target = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "target")
        .unwrap();
    let body = engine.world.body(target.body.unwrap()).unwrap();
    assert_eq!(body.body_type, quench3d::physics::BodyType::Kinematic);
    assert_eq!(body.inv_mass(), 0.0);
    assert_eq!(body.position(), Vec3::new(3.0, 5.0, 0.0));
    let saved = scene_io::scene_to_json(&engine, &doc).unwrap();
    let parsed: quench3d_cli::scene_doc::SceneDoc = serde_json::from_str(&saved.json).unwrap();
    let track = parsed
        .animations
        .iter()
        .find(|a| a.target == "target")
        .unwrap();
    assert_eq!(track.name.as_deref(), Some("ai-orbit:target"));
    assert_eq!(track.looping, Some(true));
    assert_eq!(track.position.as_ref().unwrap().values.len(), 257);

    // The paused edit has not advanced unrelated bodies. Explicit single steps
    // verify a full quarter turn and more than two complete loop boundaries.
    let initial = body.position();
    assert_eq!(
        engine
            .scene
            .objects
            .iter()
            .find(|o| o.name == "moving bystander")
            .unwrap()
            .transform
            .position,
        bystander_before
    );
    let steps_per_orbit = (4.0 / engine.world.fixed_dt()).round() as usize;
    for step in 1..=steps_per_orbit * 3 {
        rev = call(
            &mut engine,
            &mut doc,
            "step",
            json!({"expected_revision":rev}),
        )["revision"]
            .clone();
        let object = engine
            .scene
            .objects
            .iter()
            .find(|o| o.name == "target")
            .unwrap();
        let p = object.transform.position;
        if step == steps_per_orbit / 4 {
            assert!(
                (p - Vec3::new(0.0, 5.0, 3.0)).length() < 0.03,
                "quarter orbit did not advance: {p:?}"
            );
        }
        let d = p - Vec3::new(0.0, 5.0, 0.0);
        assert!(
            (d.length() - 3.0).abs() < 0.01,
            "orbit radius drifted: {p:?}"
        );
        let body = engine.world.body(object.body.unwrap()).unwrap();
        assert!(
            (body.position() - p).length() < 1e-4,
            "scene/body transforms diverged"
        );
    }
    let after_two = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "target")
        .unwrap()
        .transform
        .position;
    assert!(
        (after_two - initial).length() < 0.03,
        "orbit did not loop: {after_two:?}"
    );
    let bystander = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "moving bystander")
        .unwrap();
    let bystander_body = engine.world.body(bystander.body.unwrap()).unwrap();
    assert!((bystander_body.linear_velocity().x - 2.0).abs() < 0.01);
    assert!(bystander_body.position().x > bystander_before.x + 1.0);

    // Export/import keeps the orbit track and starts from its phase anchor.
    scene_io::load_text_into_doc(&mut engine, &mut doc, &saved.json).unwrap();
    let reloaded = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "target")
        .unwrap();
    let reloaded_body = engine.world.body(reloaded.body.unwrap()).unwrap();
    assert_eq!(
        reloaded_body.body_type,
        quench3d::physics::BodyType::Kinematic
    );
    assert_eq!(reloaded_body.position(), Vec3::new(3.0, 5.0, 0.0));
    for _ in 0..steps_per_orbit / 4 {
        doc.step_once(&mut engine);
    }
    let reloaded = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "target")
        .unwrap();
    assert!(
        (engine
            .world
            .body(reloaded.body.unwrap())
            .unwrap()
            .position()
            - Vec3::new(0.0, 5.0, 3.0))
        .length()
            < 0.03,
        "saved orbit did not resume after reload"
    );

    rev = revision(&mut engine, &mut doc);
    let cleared = call(
        &mut engine,
        &mut doc,
        "clear_orbit",
        json!({
            "name":"target", "expected_revision":rev
        }),
    );
    assert_eq!(cleared["result"]["body_mode"], "kinematic");
    assert_eq!(cleared["result"]["kinematic_animation"], false);
    let target = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "target")
        .unwrap();
    let handle = target.body.unwrap();
    let settled = engine.world.body(handle).unwrap().position();
    assert_eq!(
        engine.world.body(handle).unwrap().body_type,
        quench3d::physics::BodyType::Kinematic
    );
    assert!(!doc
        .base_doc()
        .unwrap()
        .animations
        .iter()
        .any(|a| a.name.as_deref() == Some("ai-orbit:target")));
    let mut rev = cleared["revision"].clone();
    for _ in 0..120 {
        rev = call(
            &mut engine,
            &mut doc,
            "step",
            json!({"expected_revision":rev}),
        )["revision"]
            .clone();
    }
    assert!((engine.world.body(handle).unwrap().position() - settled).length() < 1e-5);
}

#[test]
fn orbit_tools_validate_without_mutation_and_require_fresh_revisions() {
    let Some((mut engine, mut doc)) = fixture() else {
        return;
    };
    let valid = revision(&mut engine, &mut doc);
    let before = scene_io::scene_to_json(&engine, &doc).unwrap().json;
    let before_history = doc.history.len();
    for args in [
        json!({"name":"target","center":[0,0,0],"radius":0,"period_secs":4,"expected_revision":valid}),
        json!({"name":"target","center":[0,0,0],"radius":3,"period_secs":0,"expected_revision":valid}),
        json!({"name":"target","center":[0,0,0],"radius":3,"period_secs":4,"plane":"bad","expected_revision":valid}),
        json!({"name":"target","center":[0,0,0],"radius":3,"period_secs":4}),
    ] {
        let response = commands::dispatch(&mut engine, &mut doc, "set_orbit", args);
        assert_eq!(response["ok"], false);
        assert!(matches!(
            response["error"]["code"].as_str(),
            Some("invalid_arguments")
        ));
        assert_eq!(scene_io::scene_to_json(&engine, &doc).unwrap().json, before);
        assert_eq!(doc.history.len(), before_history);
    }
    // A duplicate target name is rejected without creating any track.
    engine.scene.objects[1].name = "target".into();
    let duplicate_rev = revision(&mut engine, &mut doc);
    let duplicate_before = scene_io::scene_to_json(&engine, &doc).unwrap().json;
    let duplicate = commands::dispatch(
        &mut engine,
        &mut doc,
        "set_orbit",
        json!({
            "name":"target","center":[0,5,0],"radius":3,"period_secs":4,"expected_revision":duplicate_rev
        }),
    );
    assert_eq!(duplicate["error"]["code"], "ambiguous_name");
    assert_eq!(
        scene_io::scene_to_json(&engine, &doc).unwrap().json,
        duplicate_before
    );
    assert_eq!(doc.history.len(), before_history);
    engine.scene.objects[1].name = "moving bystander".into();
    let valid = revision(&mut engine, &mut doc);
    let set = call(
        &mut engine,
        &mut doc,
        "set_orbit",
        json!({
            "name":"target","center":[0,5,0],"radius":3,"period_secs":4,"expected_revision":valid
        }),
    );
    let stale = commands::dispatch(
        &mut engine,
        &mut doc,
        "clear_orbit",
        json!({
            "name":"target","expected_revision":valid
        }),
    );
    assert_eq!(stale["ok"], false);
    assert_eq!(stale["error"]["code"], "stale_revision");
    assert_eq!(set["ok"], true);
    let configured = scene_io::scene_to_json(&engine, &doc).unwrap().json;
    let undone = call(
        &mut engine,
        &mut doc,
        "undo",
        json!({"expected_revision":set["revision"]}),
    );
    assert!(doc.base_doc().unwrap().animations.is_empty());
    let target = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "target")
        .unwrap();
    assert_eq!(
        engine.world.body(target.body.unwrap()).unwrap().body_type,
        quench3d::physics::BodyType::Dynamic
    );
    call(
        &mut engine,
        &mut doc,
        "redo",
        json!({"expected_revision":undone["revision"]}),
    );
    assert_eq!(
        scene_io::scene_to_json(&engine, &doc).unwrap().json,
        configured
    );
}

#[test]
fn body_mode_changes_preserve_material_properties_and_reset_velocity() {
    let Some((mut engine, mut doc)) = fixture() else {
        return;
    };
    let target_index = engine
        .scene
        .objects
        .iter()
        .position(|o| o.name == "target")
        .unwrap();
    let shared_shape = engine
        .world
        .body(engine.scene.objects[target_index].body.unwrap())
        .unwrap()
        .shape
        .clone();
    let target_color = engine.scene.objects[target_index].color;
    let target_handle = engine.scene.objects[target_index].body.unwrap();
    {
        let body = engine.world.body_mut(target_handle).unwrap();
        body.friction = 0.27;
        body.static_friction = 0.63;
        body.restitution = 0.42;
        body.gravity_scale = 0.37;
        body.linear_damping = 0.12;
        body.angular_damping = 0.23;
        body.user_data = 123;
        body.sensor = true;
        body.linear_velocity = Vec3::new(1.0, 2.0, 3.0);
        body.angular_velocity = Vec3::new(0.2, 0.3, 0.4);
    }
    let bystander_index = engine
        .scene
        .objects
        .iter()
        .position(|o| o.name == "moving bystander")
        .unwrap();
    let bystander_handle = engine.scene.objects[bystander_index].body.unwrap();
    let bystander_velocity = engine
        .world
        .body(bystander_handle)
        .unwrap()
        .linear_velocity();
    let bystander_color = engine.scene.objects[bystander_index].color;
    let mut rev = revision(&mut engine, &mut doc);
    for (mode, expected) in [
        ("static", quench3d::physics::BodyType::Static),
        ("kinematic", quench3d::physics::BodyType::Kinematic),
        ("dynamic", quench3d::physics::BodyType::Dynamic),
    ] {
        rev = call(
            &mut engine,
            &mut doc,
            "set_body_mode",
            json!({
                "name":"target","mode":mode,"expected_revision":rev
            }),
        )["revision"]
            .clone();
        let target = engine
            .scene
            .objects
            .iter()
            .find(|o| o.name == "target")
            .unwrap();
        let body = engine.world.body(target.body.unwrap()).unwrap();
        assert_eq!(body.body_type, expected);
        assert_eq!(target.color, target_color);
        assert_eq!(body.shape, shared_shape);
        assert_eq!(body.friction, 0.27);
        assert_eq!(body.static_friction, 0.63);
        assert_eq!(body.restitution, 0.42);
        assert_eq!(body.gravity_scale, 0.37);
        assert_eq!(body.linear_damping, 0.12);
        assert_eq!(body.angular_damping, 0.23);
        assert_eq!(body.user_data, 123);
        assert!(body.sensor);
        if expected == quench3d::physics::BodyType::Dynamic {
            assert!(body.inv_mass() > 0.0);
        } else {
            assert_eq!(body.inv_mass(), 0.0);
        }
        assert_eq!(body.linear_velocity(), Vec3::ZERO);
        assert_eq!(body.angular_velocity(), Vec3::ZERO);
    }
    assert_eq!(
        engine
            .scene
            .objects
            .iter()
            .find(|o| o.name == "moving bystander")
            .unwrap()
            .color,
        bystander_color
    );
    assert_eq!(
        engine
            .world
            .body(bystander_handle)
            .unwrap()
            .linear_velocity(),
        bystander_velocity
    );
}

#[test]
fn object_label_tool_preserves_chinese_names_and_saved_orbit_labels() {
    let Some((mut engine, mut doc)) = fixture() else {
        return;
    };
    assert!(commands::tools()
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["function"]["name"] == "set_object_label"));
    let mut rev = revision(&mut engine, &mut doc);
    for (name, position) in [("太阳", [0, 5, 0]), ("地球", [3, 5, 0])] {
        rev = call(
            &mut engine,
            &mut doc,
            "spawn_primitive",
            json!({
                "name":name,"shape":{"type":"sphere","radius":0.3},
                "position":position,"expected_revision":rev
            }),
        )["revision"]
            .clone();
        assert!(!doc.label_visible(name));
        rev = call(
            &mut engine,
            &mut doc,
            "set_object_label",
            json!({
                "name":name,"visible":true,"expected_revision":rev
            }),
        )["revision"]
            .clone();
        assert!(doc.label_visible(name));
    }
    rev = call(
        &mut engine,
        &mut doc,
        "set_orbit",
        json!({
            "name":"地球","center":[0,5,0],"radius":3,"period_secs":4,
            "expected_revision":rev
        }),
    )["revision"]
        .clone();
    let before = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "地球")
        .unwrap()
        .transform
        .position;
    for _ in 0..60 {
        rev = call(
            &mut engine,
            &mut doc,
            "step",
            json!({"expected_revision":rev}),
        )["revision"]
            .clone();
        assert!(doc.label_visible("地球"));
    }
    let after = engine
        .scene
        .objects
        .iter()
        .find(|o| o.name == "地球")
        .unwrap();
    assert!(
        (after.transform.position - before).length() > 1.0,
        "labeled planet must actually move"
    );
    assert!(
        (after.transform.position - engine.world.body(after.body.unwrap()).unwrap().position())
            .length()
            < 1e-4
    );
    let inspected = call(&mut engine, &mut doc, "inspect_scene", json!({}));
    for name in ["太阳", "地球"] {
        assert_eq!(
            inspected["result"]["objects"]
                .as_array()
                .unwrap()
                .iter()
                .find(|o| o["name"] == name)
                .unwrap()["label_visible"],
            true
        );
    }
    let saved = scene_io::scene_to_json(&engine, &doc).unwrap().json;
    let parsed: quench3d_cli::scene_doc::SceneDoc = serde_json::from_str(&saved).unwrap();
    for name in ["太阳", "地球"] {
        let object = parsed
            .objects
            .iter()
            .find(|o| o.name.as_deref() == Some(name))
            .expect("exact Chinese object name survives serialization");
        assert_eq!(object.show_label, Some(true));
    }
    scene_io::load_text_into_doc(&mut engine, &mut doc, &saved).unwrap();
    for name in ["太阳", "地球"] {
        assert!(doc.label_visible(name));
    }
    let reloaded = call(&mut engine, &mut doc, "inspect_scene", json!({}));
    assert_eq!(
        reloaded["result"]["objects"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == "地球")
            .unwrap()["label_visible"],
        true
    );
}

#[test]
fn object_label_tool_undo_redo_restores_both_visibility_states() {
    let Some((mut engine, mut doc)) = fixture() else {
        return;
    };
    assert!(!doc.label_visible("target"));
    let rev = revision(&mut engine, &mut doc);
    let enabled = call(
        &mut engine,
        &mut doc,
        "set_object_label",
        json!({
            "name":"target","visible":true,"expected_revision":rev
        }),
    );
    assert!(doc.label_visible("target"));
    let undone = call(
        &mut engine,
        &mut doc,
        "undo",
        json!({"expected_revision":enabled["revision"]}),
    );
    assert!(!doc.label_visible("target"));
    let redone = call(
        &mut engine,
        &mut doc,
        "redo",
        json!({"expected_revision":undone["revision"]}),
    );
    assert!(doc.label_visible("target"));
    let disabled = call(
        &mut engine,
        &mut doc,
        "set_object_label",
        json!({
            "name":"target","visible":false,"expected_revision":redone["revision"]
        }),
    );
    assert!(!doc.label_visible("target"));
    call(
        &mut engine,
        &mut doc,
        "undo",
        json!({"expected_revision":disabled["revision"]}),
    );
    assert!(doc.label_visible("target"));
    assert!(!doc.label_visible("moving bystander"));
}

#[test]
fn object_label_tool_rejects_invalid_stale_ambiguous_and_running_edits_without_mutation() {
    let Some((mut engine, mut doc)) = fixture() else {
        return;
    };
    let rev = revision(&mut engine, &mut doc);
    let before = scene_io::scene_to_json(&engine, &doc).unwrap().json;
    let history = doc.history.len();
    for args in [
        json!({"name":"target","visible":"true","expected_revision":rev}),
        json!({"name":"target","visible":1,"expected_revision":rev}),
        json!({"name":"target","expected_revision":rev}),
        json!({"visible":true,"expected_revision":rev}),
        json!({"name":"missing","visible":true,"expected_revision":rev}),
        json!({"name":"target","visible":true}),
    ] {
        let result = commands::dispatch(&mut engine, &mut doc, "set_object_label", args);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(scene_io::scene_to_json(&engine, &doc).unwrap().json, before);
        assert_eq!(doc.history.len(), history);
        assert!(!doc.label_visible("target"));
    }
    let enabled = call(
        &mut engine,
        &mut doc,
        "set_object_label",
        json!({
            "name":"target","visible":true,"expected_revision":rev
        }),
    );
    let after = scene_io::scene_to_json(&engine, &doc).unwrap().json;
    let history = doc.history.len();
    let stale = commands::dispatch(
        &mut engine,
        &mut doc,
        "set_object_label",
        json!({
            "name":"target","visible":false,"expected_revision":rev
        }),
    );
    assert_eq!(stale["error"]["code"], "stale_revision");
    assert_eq!(scene_io::scene_to_json(&engine, &doc).unwrap().json, after);
    assert_eq!(doc.history.len(), history);
    assert!(doc.label_visible("target"));
    engine.scene.objects[1].name = "target".into();
    let duplicate_rev = revision(&mut engine, &mut doc);
    let duplicate_before = scene_io::scene_to_json(&engine, &doc).unwrap().json;
    let ambiguous = commands::dispatch(
        &mut engine,
        &mut doc,
        "set_object_label",
        json!({
            "name":"target","visible":false,"expected_revision":duplicate_rev
        }),
    );
    assert_eq!(ambiguous["error"]["code"], "ambiguous_name");
    assert_eq!(
        scene_io::scene_to_json(&engine, &doc).unwrap().json,
        duplicate_before
    );
    assert_eq!(doc.history.len(), history);
    assert!(doc.label_visible("target"));
    engine.scene.objects[1].name = "moving bystander".into();
    engine.paused = false;
    let running_rev = revision(&mut engine, &mut doc);
    let running_before = scene_io::scene_to_json(&engine, &doc).unwrap().json;
    let running = commands::dispatch(
        &mut engine,
        &mut doc,
        "set_object_label",
        json!({
            "name":"target","visible":false,"expected_revision":running_rev
        }),
    );
    assert_eq!(running["error"]["code"], "simulation_running");
    assert_eq!(
        scene_io::scene_to_json(&engine, &doc).unwrap().json,
        running_before
    );
    assert_eq!(doc.history.len(), history);
    assert!(doc.label_visible("target"));
    assert_eq!(enabled["ok"], true);
}

#[test]
fn object_label_metadata_is_removed_on_delete_clear_and_default_scene_rebuild() {
    let Some((mut engine, mut doc)) = fixture() else {
        return;
    };
    let rev = revision(&mut engine, &mut doc);
    let enabled = call(
        &mut engine,
        &mut doc,
        "set_object_label",
        json!({
            "name":"target","visible":true,"expected_revision":rev
        }),
    );
    let deleted = call(
        &mut engine,
        &mut doc,
        "delete_object",
        json!({
            "name":"target","expected_revision":enabled["revision"]
        }),
    );
    assert!(!doc.label_visible("target"));
    call(
        &mut engine,
        &mut doc,
        "spawn_primitive",
        json!({
            "name":"target","shape":{"type":"sphere","radius":0.2},
            "expected_revision":deleted["revision"]
        }),
    );
    assert!(
        !doc.label_visible("target"),
        "same-name replacement must not inherit label metadata"
    );
    doc.set_label_visible("target", true);
    doc.clear(&mut engine);
    assert!(!doc.label_visible("target"));
    let rev = revision(&mut engine, &mut doc);
    call(
        &mut engine,
        &mut doc,
        "spawn_primitive",
        json!({
            "name":"target","shape":{"type":"sphere","radius":0.2},"expected_revision":rev
        }),
    );
    assert!(!doc.label_visible("target"));
    doc.set_label_visible("target", true);
    doc.build_default_scene(&mut engine);
    assert!(!doc.label_visible("target"));
    for object in &engine.scene.objects {
        assert!(
            !doc.label_visible(&object.name),
            "default scene labels must begin disabled"
        );
    }
}

fn relative_orbit_position(engine: &Engine, name: &str) -> Vec3 {
    let index = engine
        .scene
        .objects
        .iter()
        .position(|o| o.name == name)
        .unwrap();
    let position = engine.scene.world_transform(index).position;
    let body = engine
        .world
        .body(engine.scene.objects[index].body.unwrap())
        .unwrap();
    assert!(
        (body.position() - position).length() < 1e-4,
        "{name} visual/body mismatch"
    );
    position
}

fn relative_orbit_fixture() -> Option<(Engine, Doc, Value)> {
    let (mut engine, mut doc) = fixture()?;
    let mut rev = revision(&mut engine, &mut doc);
    for (name, radius, position) in [
        ("太阳", 0.4, [0.0, 5.0, 0.0]),
        ("地球", 0.2, [5.0, 5.0, 0.0]),
        ("月球", 0.1, [6.0, 5.0, 0.0]),
    ] {
        rev = call(
            &mut engine,
            &mut doc,
            "spawn_primitive",
            json!({
                "name":name,"shape":{"type":"sphere","radius":radius},
                "position":position,"expected_revision":rev
            }),
        )["revision"]
            .clone();
    }
    rev = call(
        &mut engine,
        &mut doc,
        "set_body_mode",
        json!({
            "name":"太阳","mode":"static","expected_revision":rev
        }),
    )["revision"]
        .clone();
    rev = call(
        &mut engine,
        &mut doc,
        "set_orbit",
        json!({
            "name":"地球","center":[0,5,0],"radius":5,"period_secs":4,
            "expected_revision":rev
        }),
    )["revision"]
        .clone();
    rev = call(
        &mut engine,
        &mut doc,
        "set_orbit",
        json!({
            "name":"月球","center_name":"地球","radius":1,"period_secs":1,
            "expected_revision":rev
        }),
    )["revision"]
        .clone();
    rev = call(
        &mut engine,
        &mut doc,
        "set_object_label",
        json!({
            "name":"月球","visible":true,"expected_revision":rev
        }),
    )["revision"]
        .clone();
    Some((engine, doc, rev))
}

#[test]
fn relative_orbit_follows_moving_center_for_multiple_revolutions_and_reload() {
    let Some((mut engine, mut doc, mut rev)) = relative_orbit_fixture() else {
        return;
    };
    let center = Vec3::new(0.0, 5.0, 0.0);
    let initial_relative =
        relative_orbit_position(&engine, "月球") - relative_orbit_position(&engine, "地球");
    assert!((initial_relative.length() - 1.0).abs() < 0.002);
    let dt = engine.world.fixed_dt();
    let steps = (9.0 / dt).ceil() as usize; // More than two four-second Earth orbits.
    let quarter_moon = (0.25 / dt).round() as usize;
    for step in 1..=steps {
        rev = call(
            &mut engine,
            &mut doc,
            "step",
            json!({"expected_revision":rev}),
        )["revision"]
            .clone();
        let earth = relative_orbit_position(&engine, "地球");
        let moon = relative_orbit_position(&engine, "月球");
        let relative = moon - earth;
        assert!(
            (relative.length() - 1.0).abs() < 0.002,
            "relative radius at step {step}: {relative:?}"
        );
        assert!(
            ((earth - center).length() - 5.0).abs() < 0.002,
            "Earth radius at step {step}: {earth:?}"
        );
        assert!((earth.y - 5.0).abs() < 1e-4);
        assert!((relative.y).abs() < 1e-4);
        assert!(doc.label_visible("月球"));
        if step == quarter_moon {
            assert!(
                (relative - initial_relative).length() > 1.0,
                "Moon must move relative to Earth"
            );
            assert!(
                (relative - Vec3::Z).length() < 0.03,
                "quarter relative orbit: {relative:?}"
            );
            assert!(
                (earth - (center + Vec3::X * 5.0)).length() > 0.5,
                "center must move too"
            );
        }
    }
    assert_eq!(relative_orbit_position(&engine, "太阳"), center);
    let saved = scene_io::scene_to_json(&engine, &doc).unwrap();
    let value: Value = serde_json::from_str(&saved.json).unwrap();
    let animation = value["animations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["target"] == "月球")
        .unwrap();
    assert_eq!(
        animation["position_relative_to"], "地球",
        "persist the center reference, not a frozen position"
    );
    scene_io::load_text_into_doc(&mut engine, &mut doc, &saved.json).unwrap();
    assert!(doc.label_visible("月球"));
    rev = revision(&mut engine, &mut doc);
    let before = relative_orbit_position(&engine, "地球");
    for _ in 0..30 {
        rev = call(
            &mut engine,
            &mut doc,
            "step",
            json!({"expected_revision":rev}),
        )["revision"]
            .clone();
        let earth = relative_orbit_position(&engine, "地球");
        let moon = relative_orbit_position(&engine, "月球");
        assert!(((moon - earth).length() - 1.0).abs() < 0.002);
        assert!(((earth - center).length() - 5.0).abs() < 0.002);
    }
    assert!((relative_orbit_position(&engine, "地球") - before).length() > 0.5);
}

#[test]
fn relative_orbit_validates_center_graph_before_mutation_and_protects_dependencies() {
    let Some((mut engine, mut doc, rev)) = relative_orbit_fixture() else {
        return;
    };
    let before = scene_io::scene_to_json(&engine, &doc).unwrap().json;
    let history = doc.history.len();
    for args in [
        json!({"name":"月球","center_name":"月球","radius":1,"period_secs":1,"expected_revision":rev}),
        json!({"name":"地球","center_name":"月球","radius":5,"period_secs":4,"expected_revision":rev}),
        json!({"name":"月球","center_name":"不存在","radius":1,"period_secs":1,"expected_revision":rev}),
        json!({"name":"月球","center_name":"地球","center":[0,5,0],"radius":1,"period_secs":1,"expected_revision":rev}),
        json!({"name":"月球","radius":1,"period_secs":1,"expected_revision":rev}),
        json!({"name":"月球","center_name":3,"radius":1,"period_secs":1,"expected_revision":rev}),
    ] {
        let rejected = commands::dispatch(&mut engine, &mut doc, "set_orbit", args);
        assert_eq!(rejected["ok"], false, "{rejected}");
        assert!(
            rejected["error"]["code"]
                .as_str()
                .is_some_and(|c| !c.is_empty()),
            "explicit error: {rejected}"
        );
        assert_eq!(scene_io::scene_to_json(&engine, &doc).unwrap().json, before);
        assert_eq!(doc.history.len(), history);
    }
    let duplicate = engine
        .scene
        .objects
        .iter()
        .position(|o| o.name == "target")
        .unwrap();
    engine.scene.objects[duplicate].name = "地球".into();
    let duplicate_rev = revision(&mut engine, &mut doc);
    let duplicate_before = scene_io::scene_to_json(&engine, &doc).unwrap().json;
    let rejected = commands::dispatch(
        &mut engine,
        &mut doc,
        "set_orbit",
        json!({
            "name":"月球","center_name":"地球","radius":1,"period_secs":1,"expected_revision":duplicate_rev
        }),
    );
    assert_eq!(
        rejected["ok"], false,
        "duplicate center must fail: {rejected}"
    );
    assert_eq!(
        scene_io::scene_to_json(&engine, &doc).unwrap().json,
        duplicate_before
    );
    assert_eq!(doc.history.len(), history);
    engine.scene.objects[duplicate].name = "target".into();
    let rev = revision(&mut engine, &mut doc);
    let rejected = commands::dispatch(
        &mut engine,
        &mut doc,
        "delete_object",
        json!({
            "name":"地球","expected_revision":rev
        }),
    );
    assert_eq!(
        rejected["ok"], false,
        "referenced center deletion must fail: {rejected}"
    );
    assert!(
        rejected["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("月球")
                || m.to_lowercase().contains("depend")
                || m.to_lowercase().contains("referenc")),
        "explain dependency: {rejected}"
    );
    assert_eq!(scene_io::scene_to_json(&engine, &doc).unwrap().json, before);
    assert_eq!(doc.history.len(), history);
    let cleared = call(
        &mut engine,
        &mut doc,
        "clear_orbit",
        json!({"name":"月球","expected_revision":rejected["revision"]}),
    );
    call(
        &mut engine,
        &mut doc,
        "delete_object",
        json!({"name":"地球","expected_revision":cleared["revision"]}),
    );
    assert!(!engine.scene.objects.iter().any(|o| o.name == "地球"));
    assert!(engine.scene.objects.iter().any(|o| o.name == "月球"));
}

#[test]
fn relative_orbit_undo_redo_restores_reference_and_kinematic_mode() {
    let Some((mut engine, mut doc, rev)) = relative_orbit_fixture() else {
        return;
    };
    let cleared = call(
        &mut engine,
        &mut doc,
        "clear_orbit",
        json!({"name":"月球","expected_revision":rev}),
    );
    let dynamic = call(
        &mut engine,
        &mut doc,
        "set_body_mode",
        json!({"name":"月球","mode":"dynamic","expected_revision":cleared["revision"]}),
    );
    let configured = call(
        &mut engine,
        &mut doc,
        "set_orbit",
        json!({
            "name":"月球","center_name":"地球","radius":1,"period_secs":1,"expected_revision":dynamic["revision"]
        }),
    );
    let moon_mode = |engine: &Engine| {
        let moon = engine
            .scene
            .objects
            .iter()
            .find(|o| o.name == "月球")
            .unwrap();
        engine.world.body(moon.body.unwrap()).unwrap().body_type
    };
    assert_eq!(moon_mode(&engine), quench3d::physics::BodyType::Kinematic);
    let undone = call(
        &mut engine,
        &mut doc,
        "undo",
        json!({"expected_revision":configured["revision"]}),
    );
    assert_eq!(moon_mode(&engine), quench3d::physics::BodyType::Dynamic);
    let undone_json: Value =
        serde_json::from_str(&scene_io::scene_to_json(&engine, &doc).unwrap().json).unwrap();
    assert!(!undone_json["animations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["target"] == "月球"));
    call(
        &mut engine,
        &mut doc,
        "redo",
        json!({"expected_revision":undone["revision"]}),
    );
    assert_eq!(moon_mode(&engine), quench3d::physics::BodyType::Kinematic);
    let redone_json: Value =
        serde_json::from_str(&scene_io::scene_to_json(&engine, &doc).unwrap().json).unwrap();
    assert_eq!(
        redone_json["animations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["target"] == "月球")
            .unwrap()["position_relative_to"],
        "地球"
    );
    assert!(doc.label_visible("月球"));
}

#[test]
fn space_environment_preserves_geometry_ground_and_physics_and_roundtrips() {
    let Some((mut engine, mut doc)) = fixture() else {
        return;
    };
    scene_io::load_text_into_doc(&mut engine, &mut doc, r#"{
        "gravity":[0,-3,0], "ground":{"size":20,"y":-2,"friction":0.7},
        "objects":[{"name":"太阳","body":"static","position":[0,5,0],"shape":{"type":"sphere","radius":1}},
        {"name":"地球","body":"dynamic","position":[4,5,0],"velocity":[0,0,2],"shape":{"type":"sphere","radius":0.3}}]
    }"#).unwrap();
    engine.paused = true;
    let gravity = engine.world.gravity();
    let objects = engine
        .scene
        .objects
        .iter()
        .map(|o| {
            (
                o.name.clone(),
                o.mesh,
                o.material,
                o.transform,
                o.visible,
                o.body,
                o.body.map(|h| engine.world.body(h).unwrap().clone()),
            )
        })
        .collect::<Vec<_>>();
    let before: Value =
        serde_json::from_str(&scene_io::scene_to_json(&engine, &doc).unwrap().json).unwrap();
    let rev = revision(&mut engine, &mut doc);
    let changed = call(
        &mut engine,
        &mut doc,
        "set_environment",
        json!({"preset":"space","expected_revision":rev}),
    );
    assert_eq!(changed["result"]["geometry_preserved"], true);
    assert_eq!(changed["result"]["physics_preserved"], true);
    let inspected = call(&mut engine, &mut doc, "inspect_scene", json!({}));
    let environment = &inspected["result"]["environment"];
    assert_eq!(environment["space"], true);
    assert_eq!(environment["sky_enabled"], true);
    assert_eq!(environment["grid_enabled"], false);
    assert_eq!(environment["fog"]["density"], 0.0);
    assert_eq!(engine.world.gravity(), gravity);
    assert_eq!(objects, engine.scene.objects.iter().map(|o| (
        o.name.clone(), o.mesh, o.material, o.transform, o.visible, o.body,
        o.body.map(|h| engine.world.body(h).unwrap().clone())
    )).collect::<Vec<_>>(), "environment changes must preserve body handles, all velocities, poses, geometry and visible ground");
    let saved = scene_io::scene_to_json(&engine, &doc).unwrap();
    let after: Value = serde_json::from_str(&saved.json).unwrap();
    assert_eq!(after["objects"], before["objects"]);
    assert_eq!(
        after["ground"], before["ground"],
        "the authored ground is not a disposable visual grid"
    );
    let parsed = quench3d_cli::scene_doc::SceneDoc::from_json(&saved.json).unwrap();
    assert_eq!(parsed.lighting.space, Some(true));
    scene_io::load_text_into_doc(&mut engine, &mut doc, &saved.json).unwrap();
    let reloaded = call(&mut engine, &mut doc, "inspect_scene", json!({}));
    assert_eq!(reloaded["result"]["environment"]["space"], true);
    assert_eq!(reloaded["result"]["environment"]["sky_enabled"], true);
    assert_eq!(reloaded["result"]["environment"]["grid_enabled"], false);
    assert_eq!(reloaded["result"]["environment"]["fog"]["density"], 0.0);
    assert_eq!(engine.world.gravity(), gravity);
    let reloaded_json: Value =
        serde_json::from_str(&scene_io::scene_to_json(&engine, &doc).unwrap().json).unwrap();
    assert_eq!(reloaded_json["ground"], after["ground"]);
    assert_eq!(reloaded_json["objects"], after["objects"]);
}

#[test]
fn space_environment_undo_redo_restores_saved_preset() {
    let Some((mut engine, mut doc)) = fixture() else {
        return;
    };
    let rev = revision(&mut engine, &mut doc);
    let studio = call(
        &mut engine,
        &mut doc,
        "set_environment",
        json!({"preset":"studio","expected_revision":rev}),
    );
    let configured = call(
        &mut engine,
        &mut doc,
        "set_environment",
        json!({"preset":"space","expected_revision":studio["revision"]}),
    );
    assert_eq!(configured["result"]["environment"]["space"], true);
    let undone = call(
        &mut engine,
        &mut doc,
        "undo",
        json!({"expected_revision":configured["revision"]}),
    );
    assert_eq!(undone["result"]["environment"]["space"], false);
    let saved = quench3d_cli::scene_doc::SceneDoc::from_json(
        &scene_io::scene_to_json(&engine, &doc).unwrap().json,
    )
    .unwrap();
    assert_ne!(saved.lighting.space, Some(true));
    let redone = call(
        &mut engine,
        &mut doc,
        "redo",
        json!({"expected_revision":undone["revision"]}),
    );
    assert_eq!(redone["result"]["environment"]["space"], true);
    assert_eq!(redone["result"]["environment"]["sky_enabled"], true);
    assert_eq!(redone["result"]["environment"]["grid_enabled"], false);
    assert_eq!(
        quench3d_cli::scene_doc::SceneDoc::from_json(
            &scene_io::scene_to_json(&engine, &doc).unwrap().json
        )
        .unwrap()
        .lighting
        .space,
        Some(true)
    );
}

#[test]
fn space_environment_invalid_preset_and_stale_revision_do_not_edit() {
    let Some((mut engine, mut doc)) = fixture() else {
        return;
    };
    let rev = revision(&mut engine, &mut doc);
    for args in [
        json!({"preset":"galaxy","expected_revision":rev}),
        json!({"preset":true,"expected_revision":rev}),
        json!({"expected_revision":rev}),
        json!({"preset":"space"}),
        json!({"preset":"space","expected_revision":"stale"}),
    ] {
        let before = scene_io::scene_to_json(&engine, &doc).unwrap().json;
        let history = (
            doc.history.len(),
            doc.history.can_undo(),
            doc.history.can_redo(),
        );
        let result = commands::dispatch(&mut engine, &mut doc, "set_environment", args);
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(scene_io::scene_to_json(&engine, &doc).unwrap().json, before);
        assert_eq!(
            (
                doc.history.len(),
                doc.history.can_undo(),
                doc.history.can_redo()
            ),
            history
        );
    }
}
