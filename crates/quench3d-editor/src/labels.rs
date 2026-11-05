//! Non-interactive viewport names, projected from the rendered scene poses.
use egui::{Color32, Pos2, Rect, Stroke, Vec2};
use quench3d::{prelude::*, Engine};

use crate::doc::Doc;

const MAX_LABELS: usize = 256;
const PADDING: egui::Vec2 = egui::vec2(6.0, 3.0);

fn project(
    camera: &quench3d::render::Camera,
    world: Vec3,
    window: quench3d::prelude::Vec2,
    viewport: Rect,
) -> Option<Pos2> {
    let screen = camera.world_to_screen(world, window)?;
    let point = egui::pos2(screen.x, screen.y);
    (point.x.is_finite() && point.y.is_finite() && viewport.contains(point)).then_some(point)
}

fn eligible(visible: bool, requested: bool, generated: bool, particle: bool, fluid: bool) -> bool {
    visible && requested && !generated && !particle && !fluid
}

/// Place the entire box inside the viewport; do not draw unreadable overlaps.
fn place(anchor: Pos2, size: Vec2, viewport: Rect, occupied: &[Rect]) -> Option<Rect> {
    let bounds = viewport.shrink(3.0);
    if size.x > bounds.width() || size.y > bounds.height() || !bounds.is_positive() {
        return None;
    }
    let clamp = |p: Pos2| {
        egui::pos2(
            p.x.clamp(bounds.left(), bounds.right() - size.x),
            p.y.clamp(bounds.top(), bounds.bottom() - size.y),
        )
    };
    let preferred = clamp(anchor + egui::vec2(8.0, -12.0 - size.y));
    // Prefer the nearest available row, trying below and above symmetrically.
    for row in 0..=MAX_LABELS {
        for sign in [1.0, -1.0] {
            if row == 0 && sign < 0.0 {
                continue;
            }
            let min = clamp(preferred + egui::vec2(0.0, sign * row as f32 * (size.y + 4.0)));
            let rect = Rect::from_min_size(min, size);
            if !occupied
                .iter()
                .any(|other| other.expand(2.0).intersects(rect))
            {
                return Some(rect);
            }
        }
    }
    None
}

pub(crate) fn draw(ui: &egui::Ui, engine: &Engine, doc: &Doc, viewport: Rect) {
    if viewport.width() < 20.0 || viewport.height() < 20.0 {
        return;
    }
    let ppp = ui.ctx().pixels_per_point().max(0.01);
    let (width, height) = engine.gpu.size();
    let window = quench3d::prelude::Vec2::new(width as f32 / ppp, height as f32 / ppp);
    let camera = engine.camera();
    let mut candidates = Vec::new();
    for (index, object) in engine.scene.objects.iter().enumerate() {
        if !eligible(
            object.visible,
            doc.label_visible(&object.name),
            object.generated,
            engine.scene.is_particle_object(index),
            object.fluid_particle.is_some()
                || object.fluid_surface.is_some()
                || object.fluid_foam.is_some()
                || object.fluid_bubble.is_some(),
        ) {
            continue;
        }
        // This is the interpolated visual pose, including parent transforms.
        let world = engine.scene.world_transform(index).position;
        if let Some(anchor) = project(&camera, world, window, viewport) {
            let distance = (world - camera.position).length_squared();
            candidates.push((distance, index, anchor));
        }
    }
    candidates.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let painter = ui.painter().with_clip_rect(viewport);
    let mut occupied = Vec::new();
    for (_, index, anchor) in candidates.into_iter().take(MAX_LABELS) {
        let name = &engine.scene.objects[index].name;
        let galley = painter.layout(
            name.clone(),
            egui::FontId::proportional(14.0),
            Color32::WHITE,
            (viewport.width() - 2.0 * PADDING.x - 6.0).max(1.0),
        );
        let Some(rect) = place(anchor, galley.size() + PADDING * 2.0, viewport, &occupied) else {
            continue;
        };
        let end = egui::pos2(
            anchor.x.clamp(rect.left(), rect.right()),
            anchor.y.clamp(rect.top(), rect.bottom()),
        );
        painter.line_segment(
            [anchor, end],
            Stroke::new(1.0, Color32::from_white_alpha(140)),
        );
        painter.rect_filled(rect, 4.0, Color32::from_rgba_unmultiplied(15, 20, 28, 220));
        painter.rect_stroke(
            rect,
            4.0,
            Stroke::new(0.75, Color32::from_white_alpha(120)),
            egui::StrokeKind::Inside,
        );
        // A normal egui text shape also exposes the exact name to shape probes.
        painter.galley(rect.min + PADDING, galley, Color32::WHITE);
        occupied.push(rect);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn chinese_label_is_painted_and_follows_visual_pose() {
        if std::env::var("QUENCH3D_GPU_TESTS").as_deref() != Ok("1") {
            return;
        }
        let gpu = pollster::block_on(quench3d::render::GpuContext::headless(
            800,
            600,
            Default::default(),
        ))
        .unwrap();
        let mut engine = Engine::new(gpu, &EngineConfig::default());
        let mut doc = Doc::new();
        crate::scene_io::load_text_into_doc(&mut engine, &mut doc, r#"{"objects":[{"name":"地球","body":"kinematic","position":[0,0,0],"shape":{"type":"sphere","radius":0.2},"show_label":true}]}"#).unwrap();
        let ctx = egui::Context::default();
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "label-test-cjk".into(),
            std::sync::Arc::new(egui::FontData::from_static(include_bytes!(
                "../assets/ui-font.ttf"
            ))),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "label-test-cjk".into());
        ctx.set_fonts(fonts);
        let render = |engine: &Engine, doc: &Doc| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0))),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        draw(
                            ui,
                            engine,
                            doc,
                            Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0)),
                        )
                    });
                },
            );
            output.textures_delta.clear();
            output.shapes.iter().find_map(|s| match &s.shape {
                egui::Shape::Text(text) if text.galley.job.text == "地球" => Some(text.pos),
                _ => None,
            })
        };
        let first = render(&engine, &doc).expect("Chinese label must emit an actual text shape");
        engine.scene.objects[0].transform.position.x += 1.0;
        let moved = render(&engine, &doc).expect("moving object keeps its text label");
        assert!((moved - first).length() > 1.0);
        doc.set_label_visible("地球", false);
        assert!(render(&engine, &doc).is_none());
    }

    #[test]
    fn projection_uses_full_window_and_clips_to_viewport() {
        let mut camera = quench3d::render::Camera::perspective(2.0);
        camera.position = Vec3::ZERO;
        camera.rotation = Quat::IDENTITY;
        let window = quench3d::prelude::Vec2::new(800.0, 400.0);
        let viewport = Rect::from_min_max(egui::pos2(200.0, 50.0), egui::pos2(650.0, 350.0));
        assert_eq!(
            project(&camera, Vec3::new(0.0, 0.0, -5.0), window, viewport),
            Some(egui::pos2(400.0, 200.0))
        );
        assert!(project(&camera, Vec3::new(0.0, 0.0, 5.0), window, viewport).is_none());
        assert!(project(&camera, Vec3::new(-100.0, 0.0, -5.0), window, viewport).is_none());
        assert!(project(
            &camera,
            Vec3::new(0.0, 0.0, -5.0),
            window,
            Rect::from_min_size(Pos2::ZERO, egui::vec2(100.0, 100.0))
        )
        .is_none());
    }

    #[test]
    fn hidden_generated_particles_and_fluids_have_no_label() {
        assert!(eligible(true, true, false, false, false));
        for flags in [
            (false, true, false, false, false),
            (true, false, false, false, false),
            (true, true, true, false, false),
            (true, true, false, true, false),
            (true, true, false, false, true),
        ] {
            assert!(!eligible(flags.0, flags.1, flags.2, flags.3, flags.4));
        }
    }

    #[test]
    fn coincident_labels_stack_without_overlap_and_stay_inside() {
        let viewport = Rect::from_min_size(egui::pos2(80.0, 30.0), egui::vec2(300.0, 200.0));
        let size = egui::vec2(70.0, 22.0);
        let anchor = viewport.right_bottom();
        let mut occupied = Vec::new();
        for _ in 0..6 {
            let rect = place(anchor, size, viewport, &occupied).unwrap();
            assert!(viewport.contains_rect(rect));
            assert!(!occupied.iter().any(|other: &Rect| other.intersects(rect)));
            occupied.push(rect);
        }
        assert_eq!(
            place(anchor, size, viewport, &occupied),
            place(anchor, size, viewport, &occupied)
        );
        assert!(place(anchor, viewport.size() * 2.0, viewport, &[]).is_none());
    }
}
