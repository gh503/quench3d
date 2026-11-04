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
