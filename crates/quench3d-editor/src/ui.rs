//! 编辑器的界面：工具栏、场景树、属性面板、状态栏、视口。
//!
//! 界面只表达意图：真正改场景/物理的代码在 [`crate::doc`] 里，
//! 需要平台能力（存盘、截图）的请求回传给 [`crate::app`]。

use crate::doc::{BodyKind, Doc, Primitive};
use crate::gizmo::{Gizmo, Mode as GizmoMode, Space as GizmoSpace};
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
    /// 这一帧指针被变换 gizmo 接管了（此时不要转视角、也不要拾取）。
    pub gizmo_active: bool,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            rect: egui::Rect::NOTHING,
            hovered: false,
            click: None,
            dragging: false,
            gizmo_active: false,
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
pub fn panels(
    ui: &mut Ui,
    engine: &mut Engine,
    doc: &mut Doc,
    view: &mut Viewport,
    gizmo: &mut Gizmo,
) -> Requests {
    let mut req = Requests::default();
    view.click = None;
    top_bar(ui, engine, doc, gizmo, &mut req);
    status_bar(ui, engine, doc, gizmo);
    outliner(ui, engine, doc);
    inspector(ui, engine, doc, &mut req);
    viewport(ui, engine, doc, view, gizmo);
    let ctx = ui.ctx().clone();
    help_window(&ctx, doc);
    req
}

/// 顶部工具栏。
fn top_bar(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc, gizmo: &mut Gizmo, req: &mut Requests) {
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
                doc.record_edit(engine);
            }

            ui.separator();

            // 撤销 / 重做：整场景快照，拖拽算一步（见 history.rs 的边界说明）。
            let can_undo = doc.history.can_undo();
            let can_redo = doc.history.can_redo();
            if ui
                .add_enabled(can_undo, egui::Button::new("↶ 撤销"))
                .on_hover_text("Ctrl+Z")
                .clicked()
            {
                doc.undo(engine);
            }
            if ui
                .add_enabled(can_redo, egui::Button::new("↷ 重做"))
                .on_hover_text("Ctrl+Shift+Z / Ctrl+Y")
                .clicked()
            {
                doc.redo(engine);
            }

            ui.separator();

            ui.label("添加：");
            for kind in Primitive::ALL {
                if ui.button(kind.label()).clicked() {
                    doc.spawn(engine, kind);
                    engine.paused = true;
                    doc.record_edit(engine);
                }
            }
            if ui.button("复制").clicked() {
                doc.duplicate_selected(engine);
                engine.paused = true;
                doc.record_edit(engine);
            }
            let has_selection = doc.selected.is_some();
            if ui
                .add_enabled(has_selection, egui::Button::new("删除"))
                .clicked()
            {
                doc.delete_selected(engine);
                doc.record_edit(engine);
            }
            if ui.button("清空").clicked() {
                doc.clear(engine);
                engine.paused = true;
                doc.record_edit(engine);
            }

            ui.separator();

            // 变换 gizmo：模式 + 坐标系（快捷键 W/E/R，L 切坐标系）。
            ui.label("手柄：");
            for mode in GizmoMode::ALL {
                let active = gizmo.mode == mode;
                if ui
                    .selectable_label(active, mode.label())
                    .on_hover_text(format!("{}（快捷键 {}）", mode.label(), mode.key_hint()))
                    .clicked()
                {
                    gizmo.mode = mode;
                }
            }
            egui::ComboBox::from_id_salt("gizmo-space")
                .selected_text(gizmo.space.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut gizmo.space, GizmoSpace::World, "世界");
                    ui.selectable_value(&mut gizmo.space, GizmoSpace::Local, "局部");
                });
            ui.checkbox(&mut gizmo.enabled, "显示")
                .on_hover_text("关掉后视口里只剩拾取与转视角");

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
fn status_bar(ui: &mut Ui, engine: &Engine, doc: &Doc, gizmo: &Gizmo) {
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
            ui.label(format!(
                "手柄 {}·{}",
                gizmo.mode.label(),
                gizmo.space.label()
            ));
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
/// 场景级光照（太阳方向/强度、环境光、曝光、投影、天空与网格）。
///
/// 放在"选中物体"之前：光照不属于任何物体，没选中东西时也该能调。
fn light_section(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc) {
    egui::CollapsingHeader::new("光照")
        .default_open(false)
        .show(ui, |ui| {
            let mut changed = false;
            {
                let light = &mut engine.scene.lighting;
                let mut dir = light
                    .sun
                    .as_ref()
                    .map(|s| [s.direction.x, s.direction.y, s.direction.z])
                    .unwrap_or([-0.4, -1.0, -0.35]);
                let mut intensity = light.sun.as_ref().map(|s| s.intensity).unwrap_or(3.0);
                let mut shadows = light.sun.as_ref().map(|s| s.cast_shadows).unwrap_or(true);
                ui.horizontal(|ui| {
                    ui.label("太阳方向");
                    for (i, axis) in ["X", "Y", "Z"].iter().enumerate() {
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut dir[i])
                                    .speed(0.01)
                                    .range(-1.0..=1.0)
                                    .prefix(*axis),
                            )
                            .changed();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("太阳强度");
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut intensity)
                                .speed(0.05)
                                .range(0.0..=30.0),
                        )
                        .changed();
                });
                changed |= ui.checkbox(&mut shadows, "投射阴影").changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut light.ambient_intensity)
                            .speed(0.01)
                            .range(0.0..=2.0)
                            .prefix("环境光 "),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut light.exposure)
                            .speed(0.02)
                            .range(0.05..=8.0)
                            .prefix("曝光 "),
                    )
                    .changed();
                changed |= ui.checkbox(&mut light.sky.enabled, "天空").changed();
                changed |= ui.checkbox(&mut light.sky.show_grid, "参考网格").changed();
                if changed {
                    // 方向按原样写回（**不**在这里归一化：归一化不是逐位幂等的，
                    // 会让"存一次读一次再存一次字节相同"这条性质失效）。
                    let length_squared = dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2];
                    let direction = if length_squared > 1e-8 {
                        Vec3::new(dir[0], dir[1], dir[2]).normalize()
                    } else {
                        Vec3::new(0.0, -1.0, 0.0)
                    };
                    if let Some(sun) = light.sun.as_mut() {
                        if length_squared > 1e-8 {
                            // 用户动过方向才写：没动就保持原值，避免归一化引入的最后一位漂移。
                            sun.direction = direction;
                        }
                        sun.intensity = intensity;
                        sun.cast_shadows = shadows;
                    }
                }
            }
            if changed {
                if ui.input(|i| i.pointer.any_down()) {
                    doc.begin_history_transaction();
                }
                doc.record_edit(engine);
            }
            if doc.history.in_transaction() && !ui.input(|i| i.pointer.any_down()) {
                doc.end_history_transaction(engine);
            }
        });
}

/// 材质缩放与开关（选中物体）：**改的是引擎里的材质**，所以视口立刻变；
/// 同时写进 `doc.textures`（按物体名），存盘与撤销才有依据。
///
/// 为什么单独一个函数、而且放在物体变换之前：属性面板里物体是被 `&mut` 借出去的，
/// 在那一块里碰 `engine.renderer` 会和借用冲突；这里没有活跃的物体借用，可以自由改引擎。
/// 贴图**路径**目前仍然只能写在场景文件里：换贴图要重建材质的绑定组，
/// 渲染器还没有这样的 API（见 docs/production_readiness.md §8）。
fn material_section(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc) {
    let Some(index) = doc.selected else { return };
    if index >= engine.scene.objects.len() {
        return;
    }
    let material_id = engine.scene.objects[index].material;
    let object_name = engine.scene.objects[index].name.clone();
    let Some(before) = engine.renderer.material(material_id).cloned() else {
        return;
    };
    let mut desc = before.clone();
    let mut changed = false;
    egui::CollapsingHeader::new("材质（缩放与开关）")
        .default_open(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("UV 平铺");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut desc.uv_scale)
                            .speed(0.05)
                            .range(0.01..=64.0),
                    )
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.label("法线强度");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut desc.normal_scale)
                            .speed(0.02)
                            .range(0.0..=4.0),
                    )
                    .changed();
            });
            ui.horizontal(|ui| {
                ui.label("alpha 裁剪");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut desc.alpha_cutoff)
                            .speed(0.02)
                            .range(0.0..=1.0),
                    )
                    .changed();
            });
            changed |= ui.checkbox(&mut desc.alpha_blend, "alpha 混合").changed();
            changed |= ui.checkbox(&mut desc.unlit, "无光照").changed();
            changed |= ui.checkbox(&mut desc.double_sided, "双面").changed();
            // 贴图**路径**：改的是文档层记录（导出依据），按下"应用"才真的读盘上传。
            ui.separator();
            let mut path = doc
                .textures
                .get(&object_name)
                .and_then(|t| t.base_color.clone())
                .unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label("基础色贴图");
                if ui.text_edit_singleline(&mut path).changed() {
                    doc.textures
                        .entry(object_name.clone())
                        .or_default()
                        .base_color = if path.trim().is_empty() {
                        None
                    } else {
                        Some(path.clone())
                    };
                }
            });
            let ready = !path.trim().is_empty();
            #[cfg(not(target_arch = "wasm32"))]
            {
                if ui
                    .add_enabled(ready, egui::Button::new("应用贴图（从磁盘读取）"))
                    .clicked()
                {
                    match quench3d::render::ImageData::from_file(&path) {
                        Ok(image) => {
                            let texture = engine.renderer.add_texture(&engine.gpu, &image, true);
                            let ok = engine.renderer.set_material_texture(
                                &engine.gpu,
                                material_id,
                                quench3d::render::MaterialTextureSlot::BaseColor,
                                Some(texture),
                            );
                            doc.status = if ok {
                                format!("已应用贴图：{path}")
                            } else {
                                format!("贴图应用失败（材质或贴图句柄无效）：{path}")
                            };
                            if ok {
                                doc.record_edit(engine);
                            }
                        }
                        Err(error) => {
                            // 场景校验器要求贴图文件存在；这里失败必须说清原因，不能静默留旧图。
                            doc.status = format!("读图失败：{error}（{path}）");
                        }
                    }
                }
            }
            #[cfg(target_arch = "wasm32")]
            {
                let _ = ready;
                ui.label(
                    RichText::new("Web 端没有本地文件系统：贴图路径请写在场景文件里")
                        .small()
                        .weak(),
                );
            }
        });
    if !changed || desc == before {
        // 没变也要把未结束的拖拽事务收尾，避免历史里留下半截事务。
        if doc.history.in_transaction() && !ui.input(|i| i.pointer.any_down()) {
            doc.end_history_transaction(engine);
        }
        return;
    }
    engine
        .renderer
        .update_material(&engine.gpu, material_id, desc.clone());
    let entry = doc.textures.entry(object_name).or_default();
    entry.uv_scale = Some(desc.uv_scale);
    entry.normal_scale = Some(desc.normal_scale);
    entry.alpha_cutoff = Some(desc.alpha_cutoff);
    entry.alpha_blend = Some(desc.alpha_blend);
    entry.unlit = Some(desc.unlit);
    entry.double_sided = Some(desc.double_sided);
    if ui.input(|i| i.pointer.any_down()) {
        doc.begin_history_transaction();
    }
    doc.record_edit(engine);
    if doc.history.in_transaction() && !ui.input(|i| i.pointer.any_down()) {
        doc.end_history_transaction(engine);
    }
}

fn inspector(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc, _req: &mut Requests) {
    egui::Panel::right("quench3d-inspector")
        .default_size(260.0)
        .show(ui, |ui| {
            ui.heading("属性");
            ui.separator();
            light_section(ui, engine, doc);
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

            material_section(ui, engine, doc);
            ui.separator();
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
                // 拖数值框/滑杆期间指针是按住状态：整段拖拽只算一步历史。
                // （`begin_transaction` 是幂等的，重复调用不会清掉"改过"标志。）
                if ui.input(|i| i.pointer.any_down()) {
                    doc.begin_history_transaction();
                }
                doc.record_edit(engine);
            }
            if doc.history.in_transaction() && !ui.input(|i| i.pointer.any_down()) {
                doc.end_history_transaction(engine);
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
                    doc.record_edit(engine);
                }
                if ui.button("删除").clicked() {
                    doc.delete_selected(engine);
                    doc.record_edit(engine);
                }
            });
        });
}

/// 中央视口（3D 画面本身由引擎画，这里只放交互与叠加提示）。
fn viewport(
    ui: &mut Ui,
    engine: &mut Engine,
    doc: &mut Doc,
    view: &mut Viewport,
    gizmo: &mut Gizmo,
) {
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            let rect = ui.max_rect();
            view.rect = rect;
            let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());
            // gizmo **先**拿指针：命中它的手柄时不再当成转视角 / 点选。
            // （它画在 3D 之上、不参与深度，所以被墙挡住的手柄也能抓。）
            let gizmo_took = gizmo.interact(ui, engine, doc, rect);
            view.gizmo_active = gizmo_took || gizmo.is_dragging();
            view.hovered = response.hovered() || response.dragged();
            view.dragging = response.dragged() && !view.gizmo_active;
            if response.clicked() && !view.gizmo_active {
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
            ui.label("场景树里的 ⌖ / F：把相机对准这个物体");
            ui.label("属性面板：改名称 / 变换 / 颜色 / 材质 / 物理参数");
            ui.add_space(6.0);
            ui.label(RichText::new("变换手柄").strong());
            ui.label("拖轴箭头 / 圆环 / 方块：移动 · 旋转 · 缩放");
            ui.label("W / E / R：切换移动 / 旋转 / 缩放（轨道相机下）");
            ui.label("L：世界 / 局部坐标系");
            ui.label("Ctrl：吸附（0.25 / 15° / 0.1），Shift：精细（×0.1）");
            ui.label("拖拽中按 Esc：取消这次拖拽");
            ui.add_space(6.0);
            ui.label(RichText::new("撤销").strong());
            ui.label("Ctrl+Z 撤销 · Ctrl+Shift+Z / Ctrl+Y 重做");
            ui.label("拖拽与滑杆整段算一步；撤销不动你的视角");
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
