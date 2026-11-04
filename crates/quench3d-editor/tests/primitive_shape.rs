//! 参数图元必须同步物理、实际渲染边界、标准保存与历史。
#![cfg(not(target_arch = "wasm32"))]
use quench3d::prelude::*;
use quench3d::render::context::with_gpu_tests;
use quench3d_editor::{
    doc::{Doc, PrimitiveShape},
    scene_io,
};

#[test]
fn primitive_edits_preserve_body_and_roundtrip_render_bounds() {
    let Some(gpu) = with_gpu_tests("参数图元实际GPU网格与标准保存", || {
        pollster::block_on(quench3d::render::GpuContext::headless(
            64,
            64,
            Default::default(),
        ))
    }) else {
        return;
    };
    let mut engine = Engine::new(
        gpu.expect("GPU必须可用"),
        &EngineConfig {
            width: 64,
            height: 64,
            ..Default::default()
        },
    );
    let mut doc = Doc::new();
    scene_io::load_text_into_doc(&mut engine,&mut doc,r#"{"objects":[{"name":"edit","body":"dynamic","shape":{"type":"sphere","radius":0.5},"position":[0,2,0],"mass":7,"friction":0.8,"velocity":[1,0,0]}]}"#).unwrap();
    doc.selected = Some(0);
    doc.reset_history(&engine);
    let mut handle = engine.scene.objects[0].body.unwrap();
    let edits = [
        PrimitiveShape::Box {
            size: Vec3::new(2.0, 3.0, 4.0),
        },
        PrimitiveShape::Sphere { radius: 1.2 },
        PrimitiveShape::Cylinder {
            radius: 0.8,
            height: 2.4,
        },
        PrimitiveShape::Cone {
            radius: 1.0,
            height: 3.0,
        },
    ];
    for edit in edits {
        assert!(doc.set_selected_shape(&mut engine, edit).unwrap());
        assert_eq!(engine.scene.objects[0].body, Some(handle));
        let body = engine.world.body(handle).unwrap();
        assert_eq!(body.mass(), 7.0);
        assert_eq!(body.friction, 0.8);
        assert_eq!(body.linear_velocity(), Vec3::X);
        let local = body.shape.local_aabb();
        let bounds = engine.scene.bounds_of(&engine.renderer, &[0]).unwrap();
        let position = engine.scene.objects[0].transform.position;
        assert!((bounds.min - position - local.min).length() < 1e-4);
        assert!((bounds.max - position - local.max).length() < 1e-4);
        assert_eq!(doc.editable_shape(&engine, 0).unwrap(), edit);
        let saved = scene_io::scene_to_json(&engine, &doc).unwrap();
        let mut restored = Doc::new();
        scene_io::load_text_into_doc(&mut engine, &mut restored, &saved.json).unwrap();
        assert_eq!(restored.editable_shape(&engine, 0).unwrap(), edit);
        assert_eq!(
            engine
                .world
                .body(engine.scene.objects[0].body.unwrap())
                .unwrap()
                .mass(),
            7.0
        );
        // load refreshes engine handles; continue with the loaded document and its actual handle.
        doc = restored;
        doc.selected = Some(0);
        handle = engine.scene.objects[0].body.unwrap();
    }
    doc.reset_history(&engine);
    doc.set_selected_shape(&mut engine, PrimitiveShape::Sphere { radius: 0.7 })
        .unwrap();
    doc.record_edit(&engine);
    doc.undo(&mut engine);
    assert_eq!(doc.editable_shape(&engine, 0).unwrap(), edits[3]);
    doc.redo(&mut engine);
    assert_eq!(
        doc.editable_shape(&engine, 0).unwrap(),
        PrimitiveShape::Sphere { radius: 0.7 }
    );
    doc.selected = Some(0);
    let before = scene_io::scene_to_json(&engine, &doc).unwrap().json;
    assert!(doc
        .set_selected_shape(&mut engine, PrimitiveShape::Sphere { radius: f32::NAN })
        .is_err());
    assert_eq!(scene_io::scene_to_json(&engine, &doc).unwrap().json, before);
    engine.scene.objects[0].transform.scale *= 2.0;
    assert!(doc.editable_shape(&engine, 0).is_err());
}
