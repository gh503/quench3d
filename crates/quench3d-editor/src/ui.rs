//! 编辑器的界面：工具栏、场景树、属性面板、状态栏、视口。
//!
//! 界面只表达意图：真正改场景/物理的代码在 [`crate::doc`] 里，
//! 需要平台能力（存盘、截图）的请求回传给 [`crate::app`]。

use crate::doc::{BodyKind, Doc, Primitive};
use egui::{RichText, Ui};
use quench3d::prelude::*;

/// 视口每帧回填的信息。
#[derive(Clone, Copy, Debug)]
pub struct Viewport {
    /// 视口在窗口里的矩形（egui 点坐标）。
    pub rect: egui::Rect,
    /// 指针是否悬停在视口上。
    pub hovered: bool,
    /// 本帧内是否发生了"点击"（按下并抬起且几乎没移动）。
    pub click: Option<Vec2>,
    /// 左键是否正在视口里拖动（用来把左键拖动当转视角）。
    pub dragging: bool,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            rect: egui::Rect::NOTHING,
            hovered: false,
            click: None,
            dragging: false,
        }
    }
}

impl Viewport {
    /// 指针是否在视口里（面板遮挡判断用）。
    pub fn contains_pointer(&self) -> bool {
        self.hovered
    }
}

/// 需要 app 层代劳的动作（涉及平台能力）。
#[derive(Default, Clone, Copy)]
pub struct Requests {
    /// 保存场景文件。
    pub save: bool,
    /// 打开场景文件。
    pub load: bool,
    /// 截图（当前窗口内容 → PNG）。
    pub screenshot: bool,
}

/// 画整个界面。
pub fn panels(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc, view: &mut Viewport) -> Requests {
    let mut req = Requests::default();
    view.click = None;
    top_bar(ui, engine, doc, &mut req);
    status_bar(ui, engine, doc);
    outliner(ui, engine, doc);
    inspector(ui, engine, doc, &mut req);
    viewport(ui, engine, doc, view);
    let ctx = ui.ctx().clone();
    help_window(&ctx, doc);
    req
}

/// 顶部工具栏。
fn top_bar(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc, req: &mut Requests) {
    egui::Panel::top("quench3d-toolbar").show(ui, |ui| {
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            let (play_label, play_hint) = if engine.paused {
                ("▶ 播放", "继续推进物理")
            } else {
                ("⏸ 暂停", "冻结物理与仿真时间")
            };
            if ui.button(play_label).on_hover_text(play_hint).clicked() {
                engine.paused = !engine.paused;
                doc.status = if engine.paused {
                    "已暂停".to_string()
                } else {
                    "继续运行".to_string()
                };
            }
            if ui
                .button("⏭ 单步")
                .on_hover_text("推进一个固定物理步长（暂停时也能用）")
                .clicked()
            {
                doc.step_once(engine);
            }
            if ui
                .button("⟲ 重置")
                .on_hover_text("所有物体回到基线状态（位置 / 朝向 / 速度）")
                .clicked()
            {
                doc.reset(engine);
                engine.paused = true;
            }

            ui.separator();

            ui.label("添加：");
            for kind in Primitive::ALL {
                if ui.button(kind.label()).clicked() {
                    doc.spawn(engine, kind);
                    engine.paused = true;
                }
            }
            if ui.button("复制").clicked() {
                doc.duplicate_selected(engine);
                engine.paused = true;
            }
            let has_selection = doc.selected.is_some();
            if ui
                .add_enabled(has_selection, egui::Button::new("删除"))
                .clicked()
            {
                doc.delete_selected(engine);
            }
            if ui.button("清空").clicked() {
                doc.clear(engine);
                engine.paused = true;
            }

            ui.separator();

            ui.checkbox(&mut engine.renderer.options.grid, "网格");
            ui.checkbox(&mut engine.renderer.options.shadows, "阴影");
            ui.checkbox(&mut engine.debug.axes, "坐标轴");

            let mode_label = match engine.camera_mode {
                CameraMode::Orbit => "轨道相机",
                CameraMode::Fly => "飞行相机",
            };
            egui::ComboBox::from_id_salt("camera-mode")
                .selected_text(mode_label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut engine.camera_mode, CameraMode::Orbit, "轨道相机");
                    ui.selectable_value(&mut engine.camera_mode, CameraMode::Fly, "飞行相机");
                });

            ui.separator();

            if ui.button("打开").on_hover_text("读入 scene.json").clicked() {
                req.load = true;
            }
            if ui.button("保存").on_hover_text("写出 scene.json").clicked() {
                req.save = true;
            }
            if ui
                .button("截图")
                .on_hover_text("把当前画面存成 PNG")
                .clicked()
            {
                req.screenshot = true;
            }
            if ui.button("？").on_hover_text("快捷键与操作说明").clicked() {
                doc.show_help = !doc.show_help;
            }
        });
        ui.add_space(2.0);
    });
}

/// 底部状态栏。
fn status_bar(ui: &mut Ui, engine: &Engine, doc: &Doc) {
    egui::Panel::bottom("quench3d-status").show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("{:.0} FPS", engine.fps));
            ui.separator();
            ui.label(format!("{:.1} ms", engine.last_dt * 1000.0));
            ui.separator();
            ui.label(format!(
                "物体 {} · 刚体 {}",
                engine.scene.len(),
                engine.world.body_count()
            ));
            ui.separator();
            ui.label(format!(
                "绘制 {} · 三角形 {}",
                engine.stats.draw_calls, engine.stats.triangles
            ));
            ui.separator();
            ui.label(format!("仿真 {:.2} s", engine.time));
            ui.separator();
            ui.label(format!("后端 {}", engine.gpu.adapter_info.name));
            if engine.paused {
                ui.separator();
                ui.label(RichText::new("已暂停").strong());
            }
            ui.separator();
            let selected = doc
                .selected_object(engine)
                .map(|o| o.name.clone())
                .unwrap_or_else(|| "（未选中）".to_string());
            ui.label(format!("选中：{selected}"));
        });
        ui.add_space(1.0);
    });
}

/// 左侧场景树。
fn outliner(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc) {
    egui::Panel::left("quench3d-outliner")
        .default_size(190.0)
        .show(ui, |ui| {
            ui.heading("场景");
            ui.label(RichText::new(doc.status.clone()).small().weak());
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                let mut focus_request: Option<usize> = None;
                for (index, object) in engine.scene.objects.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let selected = doc.selected == Some(index);
                        let mut label = object.name.clone();
                        if !object.visible {
                            label.push_str("（隐藏）");
                        }
                        if object.body.is_none() {
                            label.push_str(" · 显示");
                        }
                        if ui.selectable_label(selected, label).clicked() {
                            doc.selected = Some(index);
                        }
                        if ui.small_button("⌖").on_hover_text("聚焦").clicked() {
                            focus_request = Some(index);
                        }
                    });
                }
                if let Some(index) = focus_request {
                    if let Some(bounds) = engine.scene.bounds_of(&engine.renderer, &[index]) {
                        engine.orbit.focus_on(&bounds);
                    }
                    doc.selected = Some(index);
                }
            });
        });
}

/// 右侧属性面板。
fn inspector(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc, _req: &mut Requests) {
    egui::Panel::right("quench3d-inspector")
        .default_size(260.0)
        .show(ui, |ui| {
            ui.heading("属性");
            ui.separator();
            let Some(index) = doc.selected else {
                ui.label("在场景树或视口里点一个物体。");
                ui.add_space(8.0);
                ui.label(
                    RichText::new("提示：左键点击选中，左键拖动转视角，滚轮缩放。")
                        .small()
                        .weak(),
                );
                return;
            };
            if index >= engine.scene.objects.len() {
                doc.selected = None;
                return;
            }

            let mut changed = false;
            let mut body_kind_change: Option<BodyKind> = None;
            {
                let object = &mut engine.scene.objects[index];
                ui.horizontal(|ui| {
                    ui.label("名称");
                    if ui.text_edit_singleline(&mut object.name).changed() {
                        changed = true;
                    }
                });
                ui.checkbox(&mut object.visible, "可见");

                ui.add_space(4.0);
                egui::CollapsingHeader::new("变换")
                    .default_open(true)
                    .show(ui, |ui| {
                        let mut p = object.transform.position;
                        ui.horizontal(|ui| {
                            ui.label("位置");
                            changed |= ui.add(egui::DragValue::new(&mut p.x).speed(0.02)).changed();
                            changed |= ui.add(egui::DragValue::new(&mut p.y).speed(0.02)).changed();
                            changed |= ui.add(egui::DragValue::new(&mut p.z).speed(0.02)).changed();
                        });
                        // 存的是四元数，界面按「偏航 / 俯仰 / 翻滚」的度数给人改。
                        let (yaw, pitch, roll) = object.transform.rotation.to_euler_yxz();
                        let (mut yaw_d, mut pitch_d, mut roll_d) =
                            (yaw.to_degrees(), pitch.to_degrees(), roll.to_degrees());
                        let before = (yaw_d, pitch_d, roll_d);
                        ui.horizontal(|ui| {
                            ui.label("旋转°");
                            changed |= ui
                                .add(egui::DragValue::new(&mut yaw_d).speed(0.5))
                                .changed();
                            changed |= ui
                                .add(egui::DragValue::new(&mut pitch_d).speed(0.5))
                                .changed();
                            changed |= ui
                                .add(egui::DragValue::new(&mut roll_d).speed(0.5))
                                .changed();
                        });
                        if (yaw_d, pitch_d, roll_d) != before {
                            object.transform.rotation = Quat::from_euler_yxz(
                                yaw_d.to_radians(),
                                pitch_d.to_radians(),
                                roll_d.to_radians(),
                            );
                            changed = true;
                        }
                        let mut s = object.transform.scale;
                        ui.horizontal(|ui| {
                            ui.label("缩放");
                            changed |= ui.add(egui::DragValue::new(&mut s.x).speed(0.02)).changed();
                            changed |= ui.add(egui::DragValue::new(&mut s.y).speed(0.02)).changed();
                            changed |= ui.add(egui::DragValue::new(&mut s.z).speed(0.02)).changed();
                        });
                        if changed {
                            object.transform.position = p;
                            object.transform.scale = s;
                        }
                    });

                ui.add_space(4.0);
                egui::CollapsingHeader::new("外观")
                    .default_open(true)
                    .show(ui, |ui| {
                        let mut rgba = [
                            object.color.x,
                            object.color.y,
                            object.color.z,
                            object.color.w,
                        ];
                        ui.horizontal(|ui| {
                            ui.label("颜色倍乘");
                            if ui.color_edit_button_rgba_unmultiplied(&mut rgba).changed() {
                                object.color = Vec4::new(rgba[0], rgba[1], rgba[2], rgba[3]);
                                changed = true;
                            }
                        });
                        let mut override_pbr =
                            object.metallic.is_some() || object.roughness.is_some();
                        if ui.checkbox(&mut override_pbr, "覆盖材质参数").changed() {
                            if override_pbr {
                                object.metallic = Some(object.metallic.unwrap_or(0.0));
                                object.roughness = Some(object.roughness.unwrap_or(0.5));
                            } else {
                                object.metallic = None;
                                object.roughness = None;
                            }
                        }
                        if override_pbr {
                            let mut metallic = object.metallic.unwrap_or(0.0);
                            let mut roughness = object.roughness.unwrap_or(0.5);
                            changed |= ui
                                .add(egui::Slider::new(&mut metallic, 0.0..=1.0).text("金属度"))
                                .changed();
                            changed |= ui
                                .add(egui::Slider::new(&mut roughness, 0.0..=1.0).text("粗糙度"))
                                .changed();
                            object.metallic = Some(metallic);
                            object.roughness = Some(roughness);
                        }
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut object.emissive_strength, 0.0..=4.0)
                                    .text("自发光"),
                            )
                            .changed();
                    });

                ui.add_space(4.0);
                egui::CollapsingHeader::new("物理")
                    .default_open(true)
                    .show(ui, |ui| {
                        let current = match object.body {
                            None => BodyKind::Visual,
                            Some(handle) => match engine.world.body(handle) {
                                Some(b) if b.is_dynamic() => BodyKind::Dynamic,
                                _ => BodyKind::Static,
                            },
                        };
                        ui.horizontal(|ui| {
                            ui.label("类型");
                            for kind in [BodyKind::Dynamic, BodyKind::Static, BodyKind::Visual] {
                                if ui.selectable_label(current == kind, kind.label()).clicked() {
                                    body_kind_change = Some(kind);
                                }
                            }
                        });
                        match object.body.and_then(|h| engine.world.body(h)) {
                            Some(body) => {
                                ui.label(
                                    RichText::new(format!(
                                        "质量 {:.2} kg · 摩擦 {:.2} · 弹性 {:.2} · 速度 {:.2} m/s",
                                        body.mass(),
                                        body.friction,
                                        body.restitution,
                                        body.linear_velocity().length()
                                    ))
                                    .small()
                                    .weak(),
                                );
                                let mut friction = body.friction;
                                let mut restitution = body.restitution;
                                if ui
                                    .add(egui::Slider::new(&mut friction, 0.0..=1.5).text("摩擦"))
                                    .changed()
                                {
                                    if let Some(h) = object.body {
                                        if let Some(b) = engine.world.body_mut(h) {
                                            b.friction = friction;
                                        }
                                    }
                                }
                                if ui
                                    .add(
                                        egui::Slider::new(&mut restitution, 0.0..=1.0).text("弹性"),
                                    )
                                    .changed()
                                {
                                    if let Some(h) = object.body {
                                        if let Some(b) = engine.world.body_mut(h) {
                                            b.restitution = restitution;
                                        }
                                    }
                                }
                                ui.horizontal(|ui| {
                                    if ui.button("停下").clicked() {
                                        if let Some(h) = object.body {
                                            if let Some(b) = engine.world.body_mut(h) {
                                                b.set_linear_velocity(Vec3::ZERO);
                                                b.set_angular_velocity(Vec3::ZERO);
                                            }
                                        }
                                    }
                                    if ui.button("踢一脚").clicked() {
                                        if let Some(h) = object.body {
                                            if let Some(b) = engine.world.body_mut(h) {
                                                b.wake_up();
                                                b.apply_impulse(Vec3::new(0.0, 6.0, 0.0));
                                            }
                                        }
                                    }
                                });
                            }
                            None => {
                                ui.label(RichText::new("这个物体不参与物理。").small().weak());
                            }
                        }
                    });
            }

            if let Some(kind) = body_kind_change {
                doc.set_body_kind(engine, kind);
                changed = true;
            }
            if changed {
                doc.push_transform_to_physics(engine);
            }

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("聚焦").clicked() {
                    if let Some(bounds) = engine.scene.bounds_of(&engine.renderer, &[index]) {
                        engine.orbit.focus_on(&bounds);
                    }
                }
                if ui.button("复制").clicked() {
                    doc.duplicate_selected(engine);
                }
                if ui.button("删除").clicked() {
                    doc.delete_selected(engine);
                }
            });
        });
}

/// 中央视口（3D 画面本身由引擎画，这里只放交互与叠加提示）。
fn viewport(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc, view: &mut Viewport) {
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            let rect = ui.max_rect();
            view.rect = rect;
            let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
            view.hovered = response.hovered() || response.dragged();
            view.dragging = response.dragged();
            if response.clicked() {
                if let Some(pos) = response.interact_pointer_pos() {
                    let local = Vec2::new(pos.x - rect.left(), pos.y - rect.top());
                    view.click = Some(local);
                }
            }

            // 左上角提示：当前相机模式与操作。
            let hint = match engine.camera_mode {
                CameraMode::Orbit => {
                    "轨道相机：左键拖动 / 中键拖动 / Alt+左键 旋转，滚轮缩放，右键拖动平移"
                }
                CameraMode::Fly => "飞行相机：右键拖动看向，WASD 移动，Q/E 升降",
            };
            let painter = ui.painter();
            let pos = rect.left_top() + egui::vec2(10.0, 8.0);
            painter.text(
                pos,
                egui::Align2::LEFT_TOP,
                hint,
                egui::FontId::proportional(12.0),
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 190),
            );
            if engine.paused {
                painter.text(
                    rect.left_top() + egui::vec2(10.0, 26.0),
                    egui::Align2::LEFT_TOP,
                    "已暂停（P 继续）",
                    egui::FontId::proportional(12.0),
                    egui::Color32::from_rgb(255, 196, 0),
                );
            }
            let _ = doc;
        });
}

/// 帮助窗口。
fn help_window(ctx: &egui::Context, doc: &mut Doc) {
    let mut open = doc.show_help;
    egui::Window::new("快捷键与操作")
        .open(&mut open)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(RichText::new("相机").strong());
            ui.label("左键拖动 / 中键拖动 / Alt+左键：旋转视角");
            ui.label("右键拖动：平移（飞行模式下是转头）");
            ui.label("滚轮：推近拉远");
            ui.label("Tab：切换轨道 / 飞行相机");
            ui.label("R：聚焦到整个场景");
            ui.add_space(6.0);
            ui.label(RichText::new("编辑").strong());
            ui.label("左键点击：选中物体");
            ui.label("场景树里的 ⌖：把相机对准这个物体");
            ui.label("属性面板：改名称 / 变换 / 颜色 / 材质 / 物理参数");
            ui.add_space(6.0);
            ui.label(RichText::new("仿真").strong());
            ui.label("P 或工具栏：播放 / 暂停");
            ui.label("单步：推进一个固定步长");
            ui.label("重置：回到基线（位置 / 朝向 / 速度）");
            ui.add_space(6.0);
            ui.label(RichText::new("显示与其它").strong());
            ui.label("F3 网格 · F4 阴影 · F5 接触点 · F6 包围盒");
            ui.label("Esc：退出");
        });
    doc.show_help = open;
}
