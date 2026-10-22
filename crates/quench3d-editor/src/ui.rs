//! 编辑器的界面：工具栏、场景树、属性面板、状态栏、视口。
//!
//! 界面只表达意图：真正改场景/物理的代码在 [`crate::doc`] 里，
//! 需要平台能力（存盘、截图）的请求回传给 [`crate::app`]。

use crate::camera::CameraFeed;
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
    /// 这次点击是否按着 Shift/Ctrl（多选）。
    pub additive: bool,
    /// 框选起点（Shift/Ctrl + 左键拖动）；`None` = 没在框选。
    pub box_select_start: Option<egui::Pos2>,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            rect: egui::Rect::NOTHING,
            hovered: false,
            click: None,
            dragging: false,
            gizmo_active: false,
            additive: false,
            box_select_start: None,
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
    /// 把崩溃恢复文件装回引擎。
    pub restore_recovery: bool,
    /// 丢弃崩溃恢复文件。
    pub discard_recovery: bool,
}

/// 画整个界面。
pub fn panels(
    ui: &mut Ui,
    engine: &mut Engine,
    doc: &mut Doc,
    view: &mut Viewport,
    gizmo: &mut Gizmo,
    camera: &mut CameraFeed,
) -> Requests {
    let mut req = Requests::default();
    view.click = None;
    view.additive = false;
    top_bar(ui, engine, doc, gizmo, &mut req);
    status_bar(ui, engine, doc, gizmo);
    outliner(ui, engine, doc);
    inspector(ui, engine, doc, &mut req);
    viewport(ui, engine, doc, view, gizmo);
    let ctx = ui.ctx().clone();
    help_window(&ctx, doc);
    camera.window(&ctx);
    req
}

/// 合成规则下拉框：返回 `true` 表示用户改了它。
///
/// `None` = "跟随世界缺省"，所以下拉里除了五条规则还有一个"（缺省）"选项——
/// 少了它，用户一旦点过下拉框就再也回不到"跟随世界"的状态。
fn combine_rule_combo(ui: &mut Ui, label: &str, value: &mut Option<CombineRule>) -> bool {
    let mut changed = false;
    let text = value.map(|r| r.name()).unwrap_or("（缺省）");
    egui::ComboBox::from_id_salt(label)
        .selected_text(format!("{label}：{text}"))
        .show_ui(ui, |ui| {
            if ui.selectable_label(value.is_none(), "（缺省）").clicked() {
                *value = None;
                changed = true;
            }
            for rule in CombineRule::ALL {
                if ui
                    .selectable_label(*value == Some(rule), rule.name())
                    .clicked()
                {
                    *value = Some(rule);
                    changed = true;
                }
            }
        });
    changed
}

/// 顶部工具栏。
fn top_bar(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc, gizmo: &mut Gizmo, req: &mut Requests) {
    // `horizontal_wrapped` 的 min-size 只报一行；如果面板按这个最小高度布局，
    // 换行后的控件会被整块裁掉（DoF/SSAO/水的开关曾因此看不见）。显式给一个
    // 足够容纳多行的最小高度。
    egui::Panel::top("quench3d-toolbar")
        .min_size(240.0)
        .show(ui, |ui| {
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
            if ui
                .button("☀ 唤醒全部")
                .on_hover_text(
                    "把所有睡着的刚体叫醒（速度保持为零）：\n                     搬动过一批物体、或从快照恢复之后，\n                     免得有东西以睡着的姿态停在半空",
                )
                .clicked()
            {
                let woken = engine.world.wake_up_all();
                doc.status = format!("已唤醒 {woken} 个睡着的刚体");
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
            ui.horizontal(|ui| {
                ui.label("级联");
                ui.add(
                    egui::DragValue::new(&mut engine.renderer.options.shadow_cascades)
                        .speed(0.05)
                        .range(1..=4),
                )
                .on_hover_text(
                    "阴影级联数：1 = 单级（近处最清晰，远处可能没阴影）；\n                     2~4 = 远处也有阴影，但近处的阴影会糊一些。\n                     与 quench3d render 用同一个值。",
                );
                ui.add(
                    egui::DragValue::new(&mut engine.renderer.options.shadow_split_lambda)
                        .speed(0.01)
                        .range(0.0..=1.0),
                )
                .on_hover_text("级联切分系数：0 = 按距离均匀，1 = 对数分布（近处更细）");
            });
            ui.checkbox(&mut engine.renderer.options.bloom.enabled, "泛光（bloom）")
                .on_hover_text(
                    "让比阈值更亮的部分外溢（太阳、自发光、高光）。\n                     阈值越高越只有真正亮的东西会发光。与 quench3d render 用同一份映射。",
                );
            ui.add_enabled_ui(engine.renderer.options.bloom.enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.label("强度");
                    ui.add(
                        egui::DragValue::new(&mut engine.renderer.options.bloom.intensity)
                            .speed(0.01)
                            .range(0.0..=4.0),
                    );
                    ui.label("阈值");
                    ui.add(
                        egui::DragValue::new(&mut engine.renderer.options.bloom.threshold)
                            .speed(0.01)
                            .range(0.0..=16.0),
                    );
                });
            });
            ui.checkbox(&mut engine.renderer.options.fxaa.enabled, "抗锯齿（FXAA）")
                .on_hover_text(
                    "后处理抗锯齿：在 tonemap 之后的画面上找边缘做定向模糊。\n                     关掉时画面与不开这个功能完全一致；MSAA 与它互不替代，可以同时开。",
                );
            ui.checkbox(&mut engine.renderer.options.ssao.enabled, "环境光遮蔽（SSAO）")
                .on_hover_text(
                    "屏幕空间环境光遮蔽：让物体与地面接触的缝隙、墙角变暗。\n                     深度-only 实现，只影响间接光的近似；半径/强度在场景文件的 lighting.ssao 里调。",
                );
            ui.checkbox(&mut engine.renderer.options.dof.enabled, "景深（DoF）")
                .on_hover_text(
                    "按像素到焦平面的距离做圆盘模糊；天空不参与。\n                     只在能读深度的后端（原生 wgpu）生效，GL/WebGL2 会忽略。",
                );
            ui.add_enabled_ui(engine.renderer.options.dof.enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.label("对焦");
                    ui.add(
                        egui::DragValue::new(&mut engine.renderer.options.dof.focus)
                            .speed(0.05)
                            .range(0.1..=100.0)
                            .suffix("m"),
                    );
                    ui.label("光圈");
                    ui.add(
                        egui::DragValue::new(&mut engine.renderer.options.dof.aperture)
                            .speed(0.01)
                            .range(0.0..=4.0),
                    );
                    ui.label("模糊");
                    ui.add(
                        egui::DragValue::new(&mut engine.renderer.options.dof.max_blur)
                            .speed(0.1)
                            .range(0.0..=32.0)
                            .suffix("px"),
                    );
                });
            });
            water_section(ui, engine);
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
            // 相机投影：视场角 / 裁剪面 / 正交高度。后两项以前只在场景文件里能调，
            // 而且存盘时会被 `..Default::default()` 悄悄丢掉。
            let mut ortho = engine.orbit.orthographic_height.is_some();
            if ui
                .checkbox(&mut ortho, "正交")
                .on_hover_text("关 = 透视（近大远小）；开 = 工程制图式正交投影")
                .changed()
            {
                engine.orbit.orthographic_height = if ortho {
                    Some(engine.orbit.orthographic_height.unwrap_or(12.0))
                } else {
                    None
                };
            }
            let mut fov_deg = engine.orbit.fov_y.to_degrees();
            if ui
                .add_enabled_ui(!ortho, |ui| {
                    ui.label("视场角");
                    ui.add(
                        egui::DragValue::new(&mut fov_deg)
                            .speed(0.5)
                            .range(10.0..=170.0)
                            .suffix("°"),
                    )
                })
                .inner
                .changed()
            {
                engine.orbit.fov_y = fov_deg.to_radians();
            }
            ui.label("近/远");
            ui.add(
                egui::DragValue::new(&mut engine.orbit.near)
                    .speed(0.005)
                    .range(0.001..=10.0),
            );
            ui.add(
                egui::DragValue::new(&mut engine.orbit.far)
                    .speed(5.0)
                    .range(10.0..=100000.0),
            );
            if let Some(height) = engine.orbit.orthographic_height.as_mut() {
                ui.label("高度");
                ui.add(
                    egui::DragValue::new(height)
                        .speed(0.1)
                        .range(0.1..=5000.0)
                        .suffix("m"),
                );
            }

            ui.separator();

            if doc.recovery.is_some() {
                ui.label("发现自动保存");
                if ui
                    .button("恢复")
                    .on_hover_text("把上次崩溃前的自动保存装回引擎（不覆盖原场景文件）；快捷键 F9")
                    .clicked()
                {
                    req.restore_recovery = true;
                }
                if ui
                    .button("丢弃")
                    .on_hover_text("删掉自动保存，保持当前场景；快捷键 F10")
                    .clicked()
                {
                    req.discard_recovery = true;
                }
            }
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

/// 事件总线里一条事件的单行描述（句柄用 `#下标` 显示，名字查询留给场景树）。
fn event_label(e: &PhysicsEvent) -> String {
    let pair = |b: &Option<BodyHandle>| match b {
        Some(b) => format!(" #{}", b.index),
        None => String::new(),
    };
    let main = format!("#{}{}", e.a.index, pair(&e.b));
    match e.kind {
        PhysicsEventKind::JointBroken => format!(
            "{} {main} {:.0}N/{:.0}N·m",
            e.kind.name(),
            e.force,
            e.torque
        ),
        _ => format!("{} {main}", e.kind.name()),
    }
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
            // 事件总线：**只读**显示队列深度与最近一条。
            // 编辑器暂时没有事件面板/时间线，但"总线在动"这件事必须在界面上看得见，
            // 否则它就成了只有 CLI 报告才知道的东西。
            {
                let events = engine.world.events();
                let dropped = engine.world.events_dropped();
                if !events.is_empty() || dropped > 0 {
                    ui.separator();
                    let last = events
                        .last()
                        .map(event_label)
                        .unwrap_or_else(|| "—".to_string());
                    ui.label(format!(
                        "事件 {}（丢 {}）· 最近 {last}",
                        events.len(),
                        dropped
                    ));
                }
            }
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

/// 画一行物体（点击选中 + `⌖` 聚焦），返回这一行的聚焦请求。
fn object_row(ui: &mut Ui, engine: &Engine, doc: &mut Doc, index: usize) -> Option<usize> {
    let object = engine.scene.objects.get(index)?;
    let mut label = object.name.clone();
    if !object.visible {
        label.push_str("（隐藏）");
    }
    if object.body.is_none() {
        label.push_str(" · 显示");
    }
    let mut focus = None;
    ui.horizontal(|ui| {
        let response = ui.selectable_label(doc.is_selected(index), label);
        if response.clicked() {
            let additive =
                ui.input(|i| i.modifiers.shift || i.modifiers.command || i.modifiers.ctrl);
            if additive {
                doc.toggle_selected(index);
            } else {
                doc.select_only(Some(index));
            }
        }
        if ui.small_button("⌖").on_hover_text("聚焦").clicked() {
            focus = Some(index);
        }
    });
    focus
}

/// 左侧场景树。
///
/// 两种"成组"都要在这里表达出来：
///
/// * **父子层级**：子物体缩进画在父物体下面（拖动父物体时子物体跟着走，见 `Scene::world_transform`）。
/// * **导入的模型**：在引擎里它是"一个子网格一个物体"，散在列表里看不出它们是一伙的 ——
///   按 [`Doc::model_instances`] 收进 `模型 #i · 路径` 分组，成员照旧可点选、可 `⌖` 聚焦，
///   分组标题上还有"整组聚焦"。
fn outliner(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc) {
    egui::Panel::left("quench3d-outliner")
        .default_size(190.0)
        .show(ui, |ui| {
            ui.heading("场景");
            let selected = doc.selection().len();
            if selected > 1 {
                ui.label(
                    RichText::new(format!("已选 {selected} 个（Shift 点选增减）"))
                        .small()
                        .color(egui::Color32::from_rgb(240, 200, 120)),
                );
            }
            ui.label(RichText::new(doc.status.clone()).small().weak());
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                let count = engine.scene.objects.len();
                // "分组标题该画在哪个下标上"：模型实例的第一个成员所在的行。
                let mut header_at: Vec<Option<usize>> = vec![None; count];
                for (mi, instance) in doc.model_instances.iter().enumerate() {
                    if let Some(&first) = instance.objects.first() {
                        if let Some(slot) = header_at.get_mut(first) {
                            *slot = Some(mi);
                        }
                    }
                }
                // 父 → 子列表（顺序按场景顺序）。
                let mut children: Vec<Vec<usize>> = vec![Vec::new(); count];
                let mut roots: Vec<usize> = Vec::new();
                for (index, object) in engine.scene.objects.iter().enumerate() {
                    // 流体粒子/水面/公告板粒子是渲染代理：几万个列表项既点不过来，
                    // 也不该被当成场景物体。
                    if object.fluid_particle.is_some()
                        || object.fluid_surface.is_some()
                        || engine.scene.is_particle_object(index)
                    {
                        continue;
                    }
                    match object.parent.filter(|p| *p < count) {
                        Some(p) => children[p].push(index),
                        None => roots.push(index),
                    }
                }

                let mut focus_request: Option<Vec<usize>> = None;
                let mut drawn = vec![false; count];
                for index in roots {
                    draw_subtree(
                        ui,
                        engine,
                        doc,
                        index,
                        &children,
                        &header_at,
                        &mut drawn,
                        &mut focus_request,
                    );
                }
                // 兜底：父链成环或引用越界的物体不会被上面的遍历碰到，
                // 但它们确实还在场景里 —— 照样画出来，藏起来的物体比画错更难查。
                for index in 0..count {
                    let Some(object) = engine.scene.objects.get(index) else {
                        continue;
                    };
                    if object.fluid_particle.is_some()
                        || object.fluid_surface.is_some()
                        || engine.scene.is_particle_object(index)
                    {
                        continue;
                    }
                    draw_subtree(
                        ui,
                        engine,
                        doc,
                        index,
                        &children,
                        &header_at,
                        &mut drawn,
                        &mut focus_request,
                    );
                }
                if let Some(indices) = focus_request {
                    if let Some(bounds) = engine.scene.bounds_of(&engine.renderer, &indices) {
                        engine.orbit.focus_on(&bounds);
                    }
                    doc.select_only(indices.first().copied());
                }
            });
        });
}

/// 画一棵子树（父物体 + 缩进的子物体），并在模型实例的第一个成员处插入分组标题。
///
/// `drawn` 保证每个物体只画一次：模型分组的成员会在这里被"提前画掉"，
/// 父链成环时递归也要能停下来（场景树是每帧都跑的，不能死循环）。
#[allow(clippy::too_many_arguments)]
fn draw_subtree(
    ui: &mut Ui,
    engine: &Engine,
    doc: &mut Doc,
    index: usize,
    children: &[Vec<usize>],
    header_at: &[Option<usize>],
    drawn: &mut [bool],
    focus_request: &mut Option<Vec<usize>>,
) {
    if drawn.get(index).copied().unwrap_or(true) {
        return;
    }
    drawn[index] = true;

    if let Some(mi) = header_at.get(index).copied().flatten() {
        // 标题用 `models` 数组里的下标（`instance.model`），不是实例在表里的位置。
        let model = doc.model_instances[mi].model;
        let path = doc.model_instances[mi].path.display().to_string();
        let members = doc.model_instances[mi].objects.clone();
        egui::CollapsingHeader::new(
            RichText::new(format!("模型 #{model} · {path}"))
                .small()
                .strong(),
        )
        .id_salt(("quench3d-model-group", mi))
        .default_open(true)
        .show(ui, |ui| {
            if ui
                .small_button("⌖ 整组聚焦")
                .on_hover_text("把相机对准这个模型的全部子网格")
                .clicked()
            {
                *focus_request = Some(members.clone());
            }
            for &object in &members {
                draw_subtree(
                    ui,
                    engine,
                    doc,
                    object,
                    children,
                    header_at,
                    drawn,
                    focus_request,
                );
            }
        });
        return;
    }

    if let Some(focus) = object_row(ui, engine, doc, index) {
        *focus_request = Some(vec![focus]);
    }
    let kids = children.get(index).cloned().unwrap_or_default();
    if kids.is_empty() {
        return;
    }
    ui.indent(("quench3d-children", index), |ui| {
        for child in kids {
            draw_subtree(
                ui,
                engine,
                doc,
                child,
                children,
                header_at,
                drawn,
                focus_request,
            );
        }
    });
}

/// 画"父物体"选择器。
///
/// 只发请求、不动场景：调用方要同时改两个物体（子物体的 `parent` 和它的局部变换），
/// 而这里正处在只读借用物体的位置。请求交给调用方在借用结束后统一执行。
///
/// 列表里**不出现自己的子孙**：选中它们必然成环（引擎也会拒绝），
/// 与其让人点一下再看到一条错误，不如干脆不给这个选项。
fn draw_parent_picker(
    ui: &mut Ui,
    engine: &Engine,
    index: usize,
    request: &mut Option<Option<usize>>,
) {
    let Some(object) = engine.scene.objects.get(index) else {
        return;
    };
    let current = object.parent.filter(|p| *p < engine.scene.objects.len());
    if object.body.is_some() {
        ui.label(
            RichText::new("父物体：（带刚体的物体不能有父物体：位姿由物理决定）")
                .small()
                .weak(),
        );
        return;
    }
    let world = engine.scene.world_transform(index);
    ui.horizontal(|ui| {
        ui.label("父物体");
        let current_label = current
            .map(|p| engine.scene.objects[p].name.clone())
            .unwrap_or_else(|| "（无）".to_string());
        egui::ComboBox::from_id_salt("quench3d-parent-picker")
            .selected_text(current_label)
            .show_ui(ui, |ui| {
                if ui.selectable_label(current.is_none(), "（无）").clicked() {
                    *request = Some(None);
                }
                for (i, candidate) in engine.scene.objects.iter().enumerate() {
                    if i == index || engine.scene.is_ancestor(index, i) {
                        continue;
                    }
                    let selected = current == Some(i);
                    if ui.selectable_label(selected, &candidate.name).clicked() && !selected {
                        *request = Some(Some(i));
                    }
                }
            });
    });
    if current.is_some() {
        // 有父物体时输入框里的数字是**局部**量：不写清楚的话，
        // "改了位置但物体会跑到别处"会被当成 bug（父物体带旋转时尤其明显）。
        ui.label(
            RichText::new(format!(
                "变换是相对父物体的；世界位置 ({:.2}, {:.2}, {:.2})",
                world.position.x, world.position.y, world.position.z
            ))
            .small()
            .weak(),
        );
    }
}

/// 右侧属性面板。
/// 场景级光照（太阳方向/强度、环境光、曝光、投影、天空与网格）。
///
/// 放在"选中物体"之前：光照不属于任何物体，没选中东西时也该能调。
/// 局部光（点光源 / 聚光灯）的增删改。
///
/// 方向光只有一盏，局部光是**一组**：这里做列表编辑。落在 `engine.scene.lighting` 上，
/// 存盘由 `scene_io` 从引擎写回，所以编辑器里调的和 `quench3d render` 用的是同一组灯。
/// 点光源与聚光灯**共享**每帧 8 盏的名额（点光源优先），超出的会在 CLI 校验里给警告。
///
/// 投影：点光源（6 面立方体阴影）与聚光灯（一张 2D 图）是**两套独立资源**，
/// 各自整帧只支持一盏 —— 所以勾上一盏点光不会把聚光灯的勾去掉（反之亦然）。
fn local_lights_section(ui: &mut Ui, engine: &mut Engine, changed: &mut bool) {
    let lights = &mut engine.scene.lighting;
    let total = lights.point_lights.len() + lights.spot_lights.len();
    ui.separator();
    ui.label(format!(
        "局部光 {total} / {} 盏（点光源优先，超出名额的会被忽略）",
        quench3d::render::MAX_POINT_LIGHTS
    ));
    let mut remove_point = None;
    // 点光源也是整帧只有一盏投影：勾上某一盏之后把其它点光源的勾取消掉
    // （聚光灯那边同理，但两套阴影图互不影响，所以两个"保留谁"是分开的）。
    let mut keep_point_caster: Option<usize> = None;
    for (i, p) in lights.point_lights.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.label(format!("#{} 点光", i + 1));
            for (axis, value) in [
                ("X", &mut p.position.x),
                ("Y", &mut p.position.y),
                ("Z", &mut p.position.z),
            ] {
                *changed |= ui
                    .add(egui::DragValue::new(value).speed(0.1).prefix(axis))
                    .changed();
            }
            let mut color = p.color.to_array();
            if ui.color_edit_button_rgb(&mut color).changed() {
                p.color = Vec3::new(color[0], color[1], color[2]);
                *changed = true;
            }
            *changed |= ui
                .add(
                    egui::DragValue::new(&mut p.intensity)
                        .speed(0.2)
                        .prefix("强度 "),
                )
                .changed();
            *changed |= ui
                .add(
                    egui::DragValue::new(&mut p.radius)
                        .speed(0.2)
                        .range(0.01..=1000.0)
                        .prefix("半径 "),
                )
                .changed();
            let mut shadows = p.cast_shadows;
            if ui
                .checkbox(&mut shadows, "投影")
                .on_hover_text(
                    "整帧只支持一盏投影点光源：勾上这一盏会自动取消其它点光源的勾选。\
                     用的是 6 个 90° 面的深度数组（立方体阴影），与聚光灯那张 2D 图互不影响。",
                )
                .changed()
            {
                p.cast_shadows = shadows;
                if shadows {
                    keep_point_caster = Some(i);
                }
                *changed = true;
            }
            if ui.small_button("删").clicked() {
                remove_point = Some(i);
            }
        });
    }
    if let Some(keep) = keep_point_caster {
        for (j, other) in lights.point_lights.iter_mut().enumerate() {
            if j != keep && other.cast_shadows {
                other.cast_shadows = false;
                *changed = true;
            }
        }
    }
    if let Some(i) = remove_point {
        lights.point_lights.remove(i);
        *changed = true;
    }
    let mut remove_spot = None;
    // 投影灯**整帧只有一盏**：勾上某一盏之后要把别的都取消掉。
    // 不能在 `iter_mut()` 里边遍历边改整个列表，所以先记下"要保留谁"，循环后再处理。
    let mut keep_caster: Option<usize> = None;
    for (i, s) in lights.spot_lights.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.label(format!("#{} 聚光", i + 1));
            for (axis, value) in [
                ("X", &mut s.position.x),
                ("Y", &mut s.position.y),
                ("Z", &mut s.position.z),
            ] {
                *changed |= ui
                    .add(egui::DragValue::new(value).speed(0.1).prefix(axis))
                    .changed();
            }
            for (axis, value) in [
                ("x", &mut s.direction.x),
                ("y", &mut s.direction.y),
                ("z", &mut s.direction.z),
            ] {
                *changed |= ui
                    .add(egui::DragValue::new(value).speed(0.02).prefix(axis))
                    .changed();
            }
            let mut color = s.color.to_array();
            if ui.color_edit_button_rgb(&mut color).changed() {
                s.color = Vec3::new(color[0], color[1], color[2]);
                *changed = true;
            }
            *changed |= ui
                .add(
                    egui::DragValue::new(&mut s.intensity)
                        .speed(0.2)
                        .prefix("强度 "),
                )
                .changed();
            *changed |= ui
                .add(
                    egui::DragValue::new(&mut s.radius)
                        .speed(0.2)
                        .range(0.01..=1000.0)
                        .prefix("半径 "),
                )
                .changed();
            let mut shadows = s.cast_shadows;
            if ui
                .checkbox(&mut shadows, "投影")
                .on_hover_text(
                    "整帧只支持一盏投影聚光灯：勾上这一盏会自动取消其它聚光灯的勾选。\
                     点光源是另一套阴影图（6 面），两边各自只支持一盏、互不影响。",
                )
                .changed()
            {
                s.cast_shadows = shadows;
                if shadows {
                    keep_caster = Some(i);
                }
                *changed = true;
            }
            if ui.small_button("删").clicked() {
                remove_spot = Some(i);
            }
        });
        // 锥角：引擎只存余弦，所以来回换算一次；内锥不许大于外锥（UI 也不该造出引擎不认的锥）。
        ui.horizontal(|ui| {
            ui.label("锥角（内 / 外，度）");
            let mut inner = s.inner_cos.acos().to_degrees();
            let mut outer = s.outer_cos.acos().to_degrees();
            let a = ui
                .add(
                    egui::DragValue::new(&mut inner)
                        .speed(0.2)
                        .range(0.01..=89.99),
                )
                .changed();
            let b = ui
                .add(
                    egui::DragValue::new(&mut outer)
                        .speed(0.2)
                        .range(0.01..=89.99),
                )
                .changed();
            if a || b {
                let (inner, outer) = (inner.min(outer), outer.max(inner));
                let (position, direction, color, intensity, radius, shadows) = (
                    s.position,
                    s.direction,
                    s.color,
                    s.intensity,
                    s.radius,
                    s.cast_shadows,
                );
                // `SpotLight::new` 缺省不投影，这里必须把开关带回去：
                // 否则"改一下锥角就把投影勾掉了"——而且只在改角度时才复现，极难查
                *s = quench3d::render::SpotLight::new(
                    position, direction, color, intensity, radius, inner, outer,
                )
                .with_cast_shadows(shadows);
                *changed = true;
            }
        });
    }
    if let Some(keep) = keep_caster {
        for (j, other) in lights.spot_lights.iter_mut().enumerate() {
            if j != keep && other.cast_shadows {
                other.cast_shadows = false;
                *changed = true;
            }
        }
    }
    if let Some(i) = remove_spot {
        lights.spot_lights.remove(i);
        *changed = true;
    }
    ui.horizontal(|ui| {
        if ui.button("加点光").clicked() {
            lights.point_lights.push(quench3d::render::PointLight {
                position: Vec3::new(0.0, 4.0, 0.0),
                color: Vec3::new(1.0, 1.0, 1.0),
                intensity: 12.0,
                radius: 18.0,
                // 新加的灯缺省不投影（与 `PointLight::new` 一致）
                cast_shadows: false,
            });
            *changed = true;
        }
        if ui.button("加聚光").clicked() {
            lights.spot_lights.push(quench3d::render::SpotLight::new(
                Vec3::new(0.0, 6.0, 0.0),
                Vec3::new(0.0, -1.0, 0.0),
                Vec3::new(1.0, 1.0, 1.0),
                12.0,
                18.0,
                20.0,
                30.0,
            ));
            *changed = true;
        }
    });
}

/// 屏幕空间水的全局外观（与场景文件顶层 `water` 同一份字段）。
///
/// 这里直接改 `engine.renderer.options.water`：下一帧生效。存盘时
/// `scene_io::scene_to_json` 再从渲染选项写回 `out.water`，所以编辑器里调过的
/// 颜色/波浪在 `quench3d render` 里仍然是同一份——不会只活在预览里。
///
/// `enabled` 不在这里开关：它由场景里有没有 `waters`/`fluids` 决定；
/// 没有水时参数照样能编辑和保存，只是画面不会出现水面。
fn water_section(ui: &mut Ui, engine: &mut Engine) {
    egui::CollapsingHeader::new("屏幕空间水")
        .default_open(false)
        .show(ui, |ui| {
            let water = &mut engine.renderer.options.water;
            if water.enabled {
                ui.label("当前场景的水面后处理：开（由 waters / fluids 自动启用）");
            } else {
                ui.label("当前场景没有 waters / fluids：参数会保存，但不会出现水面");
            }
            ui.horizontal(|ui| {
                ui.label("颜色");
                ui.color_edit_button_rgb(&mut water.color)
                    .on_hover_text("线性 RGB；缺省 [0.06, 0.28, 0.48]");
            });
            ui.add(
                egui::DragValue::new(&mut water.clarity)
                    .speed(0.01)
                    .range(0.0..=1.0)
                    .prefix("清澈度 "),
            )
            .on_hover_text("0 = 浑浊/强吸收，1 = 清澈；只影响画面");
            ui.add(
                egui::DragValue::new(&mut water.strength)
                    .speed(0.02)
                    .range(0.0..=4.0)
                    .prefix("波浪 "),
            )
            .on_hover_text("波面法线扰动幅度；0 = 平静水面");
            ui.add(
                egui::DragValue::new(&mut water.foam)
                    .speed(0.02)
                    .range(0.0..=2.0)
                    .prefix("泡沫 "),
            )
            .on_hover_text("水面边缘/浪尖的泡沫；0 = 无泡沫");
            ui.add(
                egui::DragValue::new(&mut water.refraction)
                    .speed(0.02)
                    .range(0.0..=2.0)
                    .prefix("折射 "),
            )
            .on_hover_text("屏幕空间折射偏移；0 = 不扰动背景");
        });
}

fn light_section(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc) {
    egui::CollapsingHeader::new("光照")
        .default_open(false)
        .show(ui, |ui| {
            let mut changed = false;
            if let Some(map) = doc
                .base_doc
                .as_ref()
                .and_then(|d| d.lighting.environment_map.as_deref())
            {
                let intensity = doc
                    .base_doc
                    .as_ref()
                    .and_then(|d| d.lighting.environment_intensity)
                    .unwrap_or(1.0);
                let rotation = doc
                    .base_doc
                    .as_ref()
                    .and_then(|d| d.lighting.environment_rotation_deg)
                    .unwrap_or(0.0);
                ui.label(
                    RichText::new(format!(
                        "IBL 环境贴图：{map}（强度 {intensity:.2}，旋转 {rotation:.0}°）"
                    ))
                    .small()
                    .color(egui::Color32::from_rgb(150, 210, 240)),
                );
            }
            local_lights_section(ui, engine, &mut changed);
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
                if light.sky.enabled {
                    // 天空/雾的颜色都是**线性空间**（引擎内部就是线性），所以用
                    // `color_edit_button_rgb`（egui 的 `Rgb` 就是线性）而不是 `_srgb`。
                    // 注意：颜色控件改的是**传进去的数组**，所以要像材质颜色那样
                    // 先拷进局部数组、改完再写回引擎（否则改了不生效还留下一笔历史）。
                    let mut zenith = light.sky.zenith.to_array();
                    let mut horizon = light.sky.horizon.to_array();
                    ui.horizontal(|ui| {
                        ui.label("天顶 / 地平线");
                        changed |= ui.color_edit_button_rgb(&mut zenith).changed();
                        changed |= ui.color_edit_button_rgb(&mut horizon).changed();
                    });
                    light.sky.zenith = Vec3::new(zenith[0], zenith[1], zenith[2]);
                    light.sky.horizon = Vec3::new(horizon[0], horizon[1], horizon[2]);
                }
                if let Some(sun) = light.sun.as_mut() {
                    ui.horizontal(|ui| {
                        ui.label("阴影强度/覆盖半径");
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut light.shadow_strength)
                                    .speed(0.02)
                                    .range(0.0..=1.0),
                            )
                            .changed();
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut sun.shadow_radius)
                                    .speed(0.5)
                                    .range(2.0..=4000.0),
                            )
                            .changed();
                    });
                }
                // 雾：密度 0 = 关（与场景格式的 `lighting.fog` 同一语义）
                let mut fog_color = light.fog.color.to_array();
                ui.horizontal(|ui| {
                    ui.label("雾");
                    changed |= ui.color_edit_button_rgb(&mut fog_color).changed();
                    changed |= ui
                        .add(
                            egui::DragValue::new(&mut light.fog.density)
                                .speed(0.002)
                                .range(0.0..=1.0)
                                .prefix("密度 "),
                        )
                        .changed();
                });
                light.fog.color = Vec3::new(fog_color[0], fog_color[1], fog_color[2]);
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
            // 四个槽共用同一套控件；sRGB 按槽位区分（基础色/自发光是颜色贴图，法线/MR 是数据贴图）。
            ui.separator();
            for (slot, label, srgb) in [
                (
                    quench3d::render::MaterialTextureSlot::BaseColor,
                    "基础色贴图",
                    true,
                ),
                (
                    quench3d::render::MaterialTextureSlot::Normal,
                    "法线贴图",
                    false,
                ),
                (
                    quench3d::render::MaterialTextureSlot::MetallicRoughness,
                    "金属度-粗糙度贴图",
                    false,
                ),
                (
                    quench3d::render::MaterialTextureSlot::Emissive,
                    "自发光贴图",
                    true,
                ),
            ] {
                let mut path = doc
                    .textures
                    .get(&object_name)
                    .and_then(|t| match slot {
                        quench3d::render::MaterialTextureSlot::BaseColor => t.base_color.clone(),
                        quench3d::render::MaterialTextureSlot::Normal => t.normal.clone(),
                        quench3d::render::MaterialTextureSlot::MetallicRoughness => {
                            t.metallic_roughness.clone()
                        }
                        quench3d::render::MaterialTextureSlot::Emissive => t.emissive.clone(),
                    })
                    .unwrap_or_default();
                ui.horizontal(|ui| {
                    ui.label(label);
                    if ui.text_edit_singleline(&mut path).changed() {
                        let value = if path.trim().is_empty() {
                            None
                        } else {
                            Some(path.clone())
                        };
                        let entry = doc.textures.entry(object_name.clone()).or_default();
                        match slot {
                            quench3d::render::MaterialTextureSlot::BaseColor => {
                                entry.base_color = value
                            }
                            quench3d::render::MaterialTextureSlot::Normal => entry.normal = value,
                            quench3d::render::MaterialTextureSlot::MetallicRoughness => {
                                entry.metallic_roughness = value
                            }
                            quench3d::render::MaterialTextureSlot::Emissive => {
                                entry.emissive = value
                            }
                        }
                    }
                });
                let ready = !path.trim().is_empty();
                #[cfg(not(target_arch = "wasm32"))]
                {
                    if ui
                        .add_enabled(
                            ready,
                            egui::Button::new(format!("应用{label}（从磁盘读取）")),
                        )
                        .clicked()
                    {
                        match quench3d::render::ImageData::from_file(&path) {
                            Ok(image) => {
                                let texture =
                                    engine.renderer.add_texture(&engine.gpu, &image, srgb);
                                let ok = engine.renderer.set_material_texture(
                                    &engine.gpu,
                                    material_id,
                                    slot,
                                    Some(texture),
                                );
                                doc.status = if ok {
                                    format!("已应用{label}：{path}")
                                } else {
                                    format!("{label}应用失败（材质或贴图句柄无效）：{path}")
                                };
                                if ok {
                                    doc.record_edit(engine);
                                }
                            }
                            Err(error) => {
                                // 场景校验器要求贴图文件存在；失败必须说清原因，不能静默留旧图。
                                doc.status = format!("读图失败：{error}（{path}）");
                            }
                        }
                    }
                }
                #[cfg(target_arch = "wasm32")]
                {
                    // wasm 侧没有文件系统：按钮不出现，这两个变量只在原生分支用到。
                    let _ = (ready, srgb);
                    ui.label(
                        RichText::new("Web 端没有本地文件系统：贴图路径请写在场景文件里")
                            .small()
                            .weak(),
                    );
                }
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

/// 动画面板：拖动当前 clip 的时间，暂停时也能看到蒙皮/morph 当场变化。
///
/// 这里**不**改场景文件里的速度/循环/状态机；那些由 `models[].animation` /
/// `animator` 决定。拖动是"检查器"语义：只改当前会话的 `player.time`，然后
/// 调 [`quench3d::scene::Scene::refresh_animations_with_renderer`] 重算位姿与
/// 蒙皮顶点，不推进播放器。
fn animations_section(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc) {
    let count = engine.scene.animations.len();
    if count == 0 {
        return;
    }
    // 动画实例驱动的物体 → 底稿里的 models 下标；先算好，避免在可变借用时再查 doc。
    let models: Vec<Option<usize>> = engine
        .scene
        .animations
        .iter()
        .map(|a| a.objects.iter().find_map(|&o| doc.model_of_object(o)))
        .collect();
    // 资产里可选的 clip 名字；普通动画可以换，状态机的 clip 由 states 管。
    let clips: Vec<Vec<String>> = engine
        .scene
        .animations
        .iter()
        .map(|a| a.model.animations.iter().map(|c| c.name.clone()).collect())
        .collect();
    // 每个模型最多有多少个 morph target；多网格共用一组覆盖权重（与场景格式一致）。
    let morph_counts: Vec<usize> = engine
        .scene
        .animations
        .iter()
        .map(|a| {
            a.model
                .meshes
                .iter()
                .map(|m| m.morph_targets.len())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut refresh = false;
    let mut step_animator = false;
    let mut changed_model: Option<String> = None;
    let mut begin_edit = false;
    let mut end_edit = false;
    let mut record_edit = false;
    egui::CollapsingHeader::new(format!("动画（{count} 个实例）"))
        .default_open(true)
        .show(ui, |ui| {
            for (index, animation) in engine.scene.animations.iter_mut().enumerate() {
                let name = animation.player.animation.clone();
                let duration = animation.player.duration.max(0.0);
                let objects = animation.objects.clone();
                let state_name = animation
                    .animator
                    .as_ref()
                    .and_then(|a| a.states.get(a.current))
                    .map(|s| s.name.clone());
                ui.label(RichText::new(format!("#{index} · {name}")).small().strong());
                let state = if animation.player.finished {
                    "已播完"
                } else if animation.player.looping {
                    "循环"
                } else {
                    "播放中"
                };
                ui.label(
                    RichText::new(format!(
                        "时长 {duration:.2}s · 倍速 {:.2} · {state}",
                        animation.player.speed
                    ))
                    .small()
                    .weak(),
                );
                if animation.animator.is_none() && !clips[index].is_empty() {
                    let mut selected = animation.player.animation.clone();
                    ui.horizontal(|ui| {
                        ui.label("片段");
                        egui::ComboBox::from_id_salt(format!("animation-clip-{index}"))
                            .selected_text(selected.clone())
                            .show_ui(ui, |ui| {
                                for clip in &clips[index] {
                                    ui.selectable_value(&mut selected, clip.clone(), clip);
                                }
                            });
                    });
                    if selected != animation.player.animation {
                        if let Some(mut player) = animation.model.player(&selected) {
                            player.speed = animation.player.speed;
                            player.looping = animation.player.looping;
                            player.time = 0.0;
                            player.finished = false;
                            player.advance(0.0);
                            animation.player = player;
                            refresh = true;
                            record_edit = true;
                            if models[index].is_some() {
                                if let Some(path) = doc.set_animation_clip(&objects, &selected) {
                                    changed_model = Some(path);
                                }
                            }
                        }
                    }
                }
                if morph_counts[index] > 0 {
                    let mut manual = animation.morph_weights.is_some();
                    if ui.checkbox(&mut manual, "手动 morph 覆盖").changed() {
                        animation.morph_weights = if manual {
                            Some(vec![0.0; morph_counts[index]])
                        } else {
                            None
                        };
                        if models[index].is_some() {
                            if let Some(path) =
                                doc.set_morph_weights(&objects, animation.morph_weights.clone())
                            {
                                changed_model = Some(path);
                            }
                        }
                        refresh = true;
                        record_edit = true;
                    }
                    if let Some(weights) = &mut animation.morph_weights {
                        weights.resize(morph_counts[index], 0.0);
                        let mut changed = false;
                        for (target, weight) in weights.iter_mut().enumerate() {
                            let response = ui.add(
                                egui::Slider::new(weight, 0.0..=1.0)
                                    .text(format!("目标 #{target}"))
                                    .fixed_decimals(3),
                            );
                            if response.drag_started() {
                                begin_edit = true;
                            }
                            if response.changed() {
                                changed = true;
                                if !response.dragged() {
                                    record_edit = true;
                                }
                            }
                            if response.drag_stopped() {
                                end_edit = true;
                            }
                        }
                        if changed {
                            refresh = true;
                            if models[index].is_some() {
                                if let Some(path) =
                                    doc.set_morph_weights(&objects, Some(weights.clone()))
                                {
                                    changed_model = Some(path);
                                }
                            }
                        }
                    } else {
                        // 未启用覆盖时仍把目标画出来（禁用），让用户知道这个模型有
                        // morph target、勾选后会多出哪些滑杆。
                        for target in 0..morph_counts[index] {
                            let mut zero = 0.0;
                            ui.add_enabled(
                                false,
                                egui::Slider::new(&mut zero, 0.0..=1.0)
                                    .text(format!("目标 #{target}"))
                                    .fixed_decimals(3),
                            );
                        }
                    }
                }
                if !animation.ik.is_empty() {
                    ui.label(RichText::new("IK 目标（模型局部空间）").small().strong());
                    for (chain_index, chain) in animation.ik.iter_mut().enumerate() {
                        ui.label(
                            RichText::new(format!(
                                "#{chain_index} {} → {} → {}",
                                chain.root, chain.mid, chain.tip
                            ))
                            .small()
                            .weak(),
                        );
                        let mut target = [chain.target.x, chain.target.y, chain.target.z];
                        let mut changed = false;
                        ui.horizontal(|ui| {
                            for (axis, value) in ["X", "Y", "Z"].iter().zip(target.iter_mut()) {
                                let response = ui.add(
                                    egui::DragValue::new(value)
                                        .speed(0.01)
                                        .prefix(format!("{axis} ")),
                                );
                                if response.drag_started() {
                                    begin_edit = true;
                                }
                                if response.changed() {
                                    changed = true;
                                    if !response.dragged() {
                                        record_edit = true;
                                    }
                                }
                                if response.drag_stopped() {
                                    end_edit = true;
                                }
                            }
                        });
                        if changed {
                            chain.target = Vec3::new(target[0], target[1], target[2]);
                            if models[index].is_some() {
                                if let Some(path) = doc.set_ik_target(&objects, chain_index, target)
                                {
                                    changed_model = Some(path);
                                }
                            }
                            refresh = true;
                        }
                    }
                }
                ui.horizontal(|ui| {
                    let mut speed = animation.player.speed;
                    let response = ui.add(
                        egui::DragValue::new(&mut speed)
                            .range(0.05..=8.0)
                            .speed(0.01)
                            .prefix("倍速 "),
                    );
                    if response.drag_started() {
                        begin_edit = true;
                    }
                    if response.changed() {
                        animation.player.speed = speed;
                        if let Some(animator) = &mut animation.animator {
                            if let Some(state) = animator.states.get_mut(animator.current) {
                                state.speed = speed;
                            }
                        }
                        if models[index].is_some() {
                            if let Some(path) = doc.set_animation_params(
                                &objects,
                                state_name.as_deref(),
                                speed,
                                animation.player.looping,
                            ) {
                                changed_model = Some(path);
                            }
                        }
                        if !response.dragged() {
                            record_edit = true;
                        }
                    }
                    if response.drag_stopped() {
                        end_edit = true;
                    }
                    let mut looping = animation.player.looping;
                    if ui.checkbox(&mut looping, "循环").changed() {
                        animation.player.looping = looping;
                        if looping {
                            animation.player.finished = false;
                        }
                        if let Some(animator) = &mut animation.animator {
                            if let Some(state) = animator.states.get_mut(animator.current) {
                                state.looping = looping;
                            }
                        }
                        if models[index].is_some() {
                            if let Some(path) = doc.set_animation_params(
                                &objects,
                                state_name.as_deref(),
                                animation.player.speed,
                                looping,
                            ) {
                                changed_model = Some(path);
                            }
                        }
                        record_edit = true;
                    }
                });
                if duration > 0.0 {
                    let mut time = animation.player.time.clamp(0.0, duration);
                    if ui
                        .add(
                            egui::Slider::new(&mut time, 0.0..=duration)
                                .text("时间")
                                .fixed_decimals(3),
                        )
                        .changed()
                    {
                        animation.player.time = time;
                        animation.player.finished = false;
                        refresh = true;
                    }
                }
                if let Some(secondary) = &animation.secondary {
                    ui.label(
                        RichText::new(format!(
                            "过渡到「{}」 · 权重 {:.2}",
                            secondary.animation, animation.blend_weight
                        ))
                        .small()
                        .weak(),
                    );
                }
                if let Some(animator) = &mut animation.animator {
                    if !animator.parameters.is_empty() {
                        ui.label(RichText::new("参数（输入 → 过渡条件）").small().strong());
                        let mut reset = false;
                        for param in &mut animator.parameters {
                            ui.horizontal(|ui| {
                                let response = ui.add(
                                    egui::DragValue::new(&mut param.value)
                                        .range(-100.0..=100.0)
                                        .speed(0.01)
                                        .prefix(format!("{} ", param.name)),
                                );
                                if response.changed() {
                                    step_animator = true;
                                }
                                ui.label(
                                    RichText::new(format!("默认 {:.2}", param.default))
                                        .small()
                                        .weak(),
                                );
                            });
                        }
                        if ui.button("重置参数").clicked() {
                            reset = true;
                            step_animator = true;
                        }
                        if reset {
                            for param in &mut animator.parameters {
                                param.value = param.default;
                            }
                        }
                    }
                    let event_names: Vec<String> = animator
                        .transitions
                        .iter()
                        .filter_map(|t| t.on_event.clone())
                        .fold(Vec::new(), |mut names, name| {
                            if !names.contains(&name) {
                                names.push(name);
                            }
                            names
                        });
                    if !event_names.is_empty() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new("事件").small().strong());
                            for name in &event_names {
                                if ui.small_button(format!("发送 {name}")).clicked() {
                                    animator.pending_event = Some(name.clone());
                                    step_animator = true;
                                }
                            }
                        });
                    }
                    let current = animator
                        .states
                        .get(animator.current)
                        .map(|s| s.name.as_str())
                        .unwrap_or("?");
                    let pending = animator
                        .pending
                        .and_then(|i| animator.states.get(i))
                        .map(|s| s.name.as_str());
                    ui.label(
                        RichText::new(match pending {
                            Some(to) => {
                                format!("状态机：{current} → {to}（停留 {:.2}s）", animator.elapsed)
                            }
                            None => format!("状态机：{current}（停留 {:.2}s）", animator.elapsed),
                        })
                        .small()
                        .weak(),
                    );
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(!clips[index].is_empty(), egui::Button::new("加状态"))
                            .clicked()
                        {
                            let mut n = animator.states.len() + 1;
                            let name = loop {
                                let candidate = format!("状态{n}");
                                if !animator.states.iter().any(|s| s.name == candidate) {
                                    break candidate;
                                }
                                n += 1;
                            };
                            animator.states.push(quench3d::scene::ModelAnimatorState {
                                name: name.clone(),
                                animation: clips[index][0].clone(),
                                speed: 1.0,
                                looping: true,
                                start: 0.0,
                            });
                            if models[index].is_some() {
                                if let Some(path) =
                                    doc.add_animator_state(&objects, &name, &clips[index][0])
                                {
                                    changed_model = Some(path);
                                }
                            }
                            record_edit = true;
                        }
                    });
                    let initial = if models[index].is_some() {
                        doc.animator_initial(&objects)
                    } else {
                        None
                    }
                    .or_else(|| {
                        animator
                            .states
                            .get(animator.current)
                            .map(|s| s.name.clone())
                    });
                    if let Some(initial) = initial {
                        let mut selected = initial.clone();
                        let mut changed = false;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("初始（立即切换）").small().weak());
                            egui::ComboBox::from_id_salt(format!("animator-initial-{index}"))
                                .selected_text(selected.clone())
                                .show_ui(ui, |ui| {
                                    for name in animator.states.iter().map(|s| s.name.clone()) {
                                        if ui
                                            .selectable_value(&mut selected, name.clone(), &name)
                                            .changed()
                                        {
                                            changed = true;
                                        }
                                    }
                                });
                        });
                        if changed && selected != initial {
                            if let Some(target) =
                                animator.states.iter().position(|s| s.name == selected)
                            {
                                animator.current = target;
                                animator.pending = None;
                                animator.elapsed = 0.0;
                                let state = &animator.states[target];
                                if let Some(mut player) = animation.model.player(&state.animation) {
                                    player.speed = state.speed;
                                    player.looping = state.looping;
                                    player.time = state.start;
                                    player.finished = false;
                                    player.advance(0.0);
                                    animation.player = player;
                                }
                                animation.secondary = None;
                                animation.blend_weight = 0.0;
                                animation.blend_target = 0.0;
                                animation.blend_fade = 0.0;
                                animation.blend_elapsed = 0.0;
                                refresh = true;
                            }
                            if let Some(path) = doc.set_animator_initial(&objects, &selected) {
                                changed_model = Some(path);
                            }
                            record_edit = true;
                        }
                    }
                    let mut remove_state = None;
                    ui.label(RichText::new("状态").small().strong());
                    for si in 0..animator.states.len() {
                        let name = animator.states[si].name.clone();
                        let old_clip = animator.states[si].animation.clone();
                        let mut selected = old_clip.clone();
                        let mut start = animator.states[si].start;
                        let mut changed = false;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("#{si} {name}")).small());
                            egui::ComboBox::from_id_salt(format!(
                                "animator-state-clip-{index}-{si}"
                            ))
                            .selected_text(selected.clone())
                            .show_ui(ui, |ui| {
                                for clip in &clips[index] {
                                    if ui
                                        .selectable_value(&mut selected, clip.clone(), clip)
                                        .changed()
                                    {
                                        changed = true;
                                    }
                                }
                            });
                            let response = ui.add(
                                egui::DragValue::new(&mut start)
                                    .range(0.0..=60.0)
                                    .speed(0.02)
                                    .prefix("起始 ")
                                    .suffix("s"),
                            );
                            if response.drag_started() {
                                begin_edit = true;
                            }
                            if response.changed() {
                                changed = true;
                                if !response.dragged() {
                                    record_edit = true;
                                }
                            }
                            if response.drag_stopped() {
                                end_edit = true;
                            }
                            if animator.states.len() > 1 && ui.small_button("删").clicked() {
                                remove_state = Some(si);
                            }
                        });
                        if changed {
                            let clip_changed = selected != old_clip;
                            animator.states[si].animation = selected.clone();
                            animator.states[si].start = start;
                            if si == animator.current {
                                if clip_changed {
                                    if let Some(mut player) = animation.model.player(&selected) {
                                        player.speed = animator.states[si].speed;
                                        player.looping = animator.states[si].looping;
                                        player.time = start;
                                        player.finished = false;
                                        player.advance(0.0);
                                        animation.player = player;
                                        animation.secondary = None;
                                        animation.blend_weight = 0.0;
                                        animation.blend_target = 0.0;
                                        animation.blend_fade = 0.0;
                                        animation.blend_elapsed = 0.0;
                                    }
                                } else {
                                    animation.player.time = start;
                                    animation.player.finished = false;
                                }
                                refresh = true;
                            }
                            if models[index].is_some() {
                                if let Some(path) =
                                    doc.set_animator_state(&objects, si, &selected, start)
                                {
                                    changed_model = Some(path);
                                }
                            }
                            record_edit = true;
                        }
                    }
                    if let Some(remove) = remove_state {
                        let removed = animator.states.remove(remove).name;
                        animator
                            .transitions
                            .retain(|t| t.from != removed && t.to != removed);
                        let current_changed = animator.current == remove;
                        if current_changed {
                            animator.current = 0;
                        } else if animator.current > remove {
                            animator.current -= 1;
                        }
                        let pending_removed = animator.pending == Some(remove);
                        if pending_removed {
                            animator.pending = None;
                        } else if let Some(pending) = animator.pending {
                            if pending > remove {
                                animator.pending = Some(pending - 1);
                            }
                        }
                        if current_changed {
                            if let Some(state) = animator.states.get(animator.current) {
                                if let Some(mut player) = animation.model.player(&state.animation) {
                                    player.speed = state.speed;
                                    player.looping = state.looping;
                                    player.time = state.start;
                                    player.finished = false;
                                    player.advance(0.0);
                                    animation.player = player;
                                }
                            }
                            animation.secondary = None;
                            animation.blend_weight = 0.0;
                            animation.blend_target = 0.0;
                            animation.blend_fade = 0.0;
                            animation.blend_elapsed = 0.0;
                        } else if pending_removed {
                            animation.secondary = None;
                            animation.blend_weight = 0.0;
                            animation.blend_target = 0.0;
                            animation.blend_fade = 0.0;
                            animation.blend_elapsed = 0.0;
                        }
                        animator.elapsed = 0.0;
                        if models[index].is_some() {
                            if let Some(path) = doc.remove_animator_state(&objects, remove) {
                                changed_model = Some(path);
                            }
                        }
                        refresh = true;
                        record_edit = true;
                    }
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(animator.states.len() >= 2, egui::Button::new("加过渡"))
                            .clicked()
                        {
                            if let Some(from) = animator
                                .states
                                .get(animator.current)
                                .map(|s| s.name.clone())
                            {
                                if let Some(to) = animator
                                    .states
                                    .iter()
                                    .map(|s| s.name.clone())
                                    .find(|name| *name != from)
                                {
                                    animator.transitions.push(
                                        quench3d::scene::ModelAnimatorTransition {
                                            from: from.clone(),
                                            to: to.clone(),
                                            after: 0.5,
                                            on_finished: false,
                                            fade: 0.0,
                                            conditions: Vec::new(),
                                            on_event: None,
                                        },
                                    );
                                    if models[index].is_some() {
                                        if let Some(path) = doc.add_animator_transition(
                                            &objects, &from, &to, 0.5, 0.0, false,
                                        ) {
                                            changed_model = Some(path);
                                        }
                                    }
                                    record_edit = true;
                                }
                            }
                        }
                    });
                    if !animator.transitions.is_empty() {
                        ui.label(RichText::new("过渡").small().strong());
                    }
                    let state_names: Vec<String> =
                        animator.states.iter().map(|s| s.name.clone()).collect();
                    let mut remove_transition = None;
                    for ti in 0..animator.transitions.len() {
                        let mut from = animator.transitions[ti].from.clone();
                        let mut to = animator.transitions[ti].to.clone();
                        let transition = &mut animator.transitions[ti];
                        let mut changed = false;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{from} → {to}")).small());
                            if let Some(event) = &transition.on_event {
                                ui.label(
                                    RichText::new(format!("事件 {event}"))
                                        .small()
                                        .color(egui::Color32::from_rgb(240, 190, 90)),
                                );
                            }
                            let response = ui.add_enabled(
                                !transition.on_finished,
                                egui::DragValue::new(&mut transition.after)
                                    .range(0.0..=60.0)
                                    .speed(0.05)
                                    .prefix("等待 ")
                                    .suffix("s"),
                            );
                            if response.drag_started() {
                                begin_edit = true;
                            }
                            if response.changed() {
                                changed = true;
                                if !response.dragged() {
                                    record_edit = true;
                                }
                            }
                            if response.drag_stopped() {
                                end_edit = true;
                            }
                            let response = ui.add(
                                egui::DragValue::new(&mut transition.fade)
                                    .range(0.0..=5.0)
                                    .speed(0.02)
                                    .prefix("淡入 ")
                                    .suffix("s"),
                            );
                            if response.drag_started() {
                                begin_edit = true;
                            }
                            if response.changed() {
                                changed = true;
                                if !response.dragged() {
                                    record_edit = true;
                                }
                            }
                            if response.drag_stopped() {
                                end_edit = true;
                            }
                            if ui
                                .checkbox(&mut transition.on_finished, "播完触发")
                                .changed()
                            {
                                changed = true;
                                record_edit = true;
                            }
                            if ui.small_button("删").clicked() {
                                remove_transition = Some(ti);
                            }
                        });
                        let mut ends_changed = false;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("端点").small().weak());
                            egui::ComboBox::from_id_salt(format!(
                                "animator-transition-from-{index}-{ti}"
                            ))
                            .selected_text(from.clone())
                            .show_ui(ui, |ui| {
                                for name in &state_names {
                                    if ui.selectable_value(&mut from, name.clone(), name).changed()
                                    {
                                        ends_changed = true;
                                    }
                                }
                            });
                            ui.label(RichText::new("→").small().weak());
                            egui::ComboBox::from_id_salt(format!(
                                "animator-transition-to-{index}-{ti}"
                            ))
                            .selected_text(to.clone())
                            .show_ui(ui, |ui| {
                                for name in &state_names {
                                    if ui.selectable_value(&mut to, name.clone(), name).changed() {
                                        ends_changed = true;
                                    }
                                }
                            });
                        });
                        if ends_changed && from != to {
                            transition.from = from.clone();
                            transition.to = to.clone();
                            changed = true;
                            if animator.pending.is_some() {
                                animator.pending = None;
                                animation.secondary = None;
                                animation.blend_weight = 0.0;
                                animation.blend_target = 0.0;
                                animation.blend_fade = 0.0;
                                animation.blend_elapsed = 0.0;
                                refresh = true;
                            }
                        }
                        if changed && models[index].is_some() {
                            if from != to {
                                if let Some(path) =
                                    doc.set_animator_transition_ends(&objects, ti, &from, &to)
                                {
                                    changed_model = Some(path);
                                }
                            }
                            if let Some(path) = doc.set_animator_transition(
                                &objects,
                                ti,
                                transition.after,
                                transition.fade,
                                transition.on_finished,
                            ) {
                                changed_model = Some(path);
                            }
                        }
                    }
                    if let Some(remove) = remove_transition {
                        let to = animator.transitions[remove].to.clone();
                        animator.transitions.remove(remove);
                        let active = animator
                            .pending
                            .and_then(|i| animator.states.get(i))
                            .map(|s| s.name.as_str())
                            == Some(to.as_str());
                        if active {
                            animator.pending = None;
                            animation.secondary = None;
                            animation.blend_weight = 0.0;
                            animation.blend_target = 0.0;
                            animation.blend_fade = 0.0;
                            animation.blend_elapsed = 0.0;
                            refresh = true;
                        }
                        if models[index].is_some() {
                            if let Some(path) = doc.remove_animator_transition(&objects, remove) {
                                changed_model = Some(path);
                            }
                        }
                        record_edit = true;
                    }
                }
                ui.add_space(4.0);
            }
            ui.label(
                RichText::new("倍速/循环写回场景文件（可撤销）；时间滑杆只改当前会话。")
                    .small()
                    .weak(),
            );
        });
    if step_animator {
        // dt=0：只按当前参数判一次过渡，不额外推进时间；暂停时也能当场看到状态切换。
        let (scene, renderer, gpu) = (&mut engine.scene, &mut engine.renderer, &engine.gpu);
        scene.advance_animations_with_renderer(0.0, renderer, gpu);
        refresh = true;
    }
    if refresh {
        let (scene, renderer, gpu) = (&mut engine.scene, &mut engine.renderer, &engine.gpu);
        let updated = scene.refresh_animations_with_renderer(renderer, gpu);
        doc.status = format!("动画时间已刷新（{updated} 个片段）");
    }
    if begin_edit {
        doc.begin_history_transaction();
    }
    if end_edit {
        doc.end_history_transaction(engine);
    } else if record_edit {
        doc.record_edit(engine);
    }
    if let Some(path) = changed_model {
        doc.status = format!("动画参数已写回 {path}（存盘生效，可撤销）");
    }
}

/// 模型（glTF/OBJ 引用）的变换面板。
///
/// 改这里会**同时**做两件事：把新变换写进底稿的 `models[i]`（存盘生效），
/// 以及通过 `Doc` 里的"物体↔模型"映射**当场**把引擎里那一组物体挪过去
/// （视口立刻动，不用重开）。反向的那半条在 `gizmo.rs`：在视口里拖模型的
/// 任何一个网格，同样是整组联动并写回 `models[i]`。
fn models_section(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc) {
    let Some(base) = doc.base_doc.as_ref() else {
        return;
    };
    if base.models.is_empty() {
        return;
    }
    // 每个模型在视口里对应几个物体（没有映射时是 None，界面如实不显示）。
    let counts: Vec<Option<usize>> = (0..base.models.len())
        .map(|i| doc.model_objects(i).map(<[usize]>::len))
        .collect();
    let mut rows: Vec<(String, [f32; 3], [f32; 3], f32)> = base
        .models
        .iter()
        .map(|m| {
            (
                m.path.display().to_string(),
                m.position.unwrap_or([0.0, 0.0, 0.0]),
                m.rotation_deg.unwrap_or([0.0, 0.0, 0.0]),
                m.scale.unwrap_or(1.0),
            )
        })
        .collect();
    let lod_rows: Vec<Option<String>> = base
        .models
        .iter()
        .map(|m| {
            m.lods.as_ref().map(|lods| {
                let distances = lods
                    .iter()
                    .map(|l| format!("{:.1}m", l.min_distance))
                    .collect::<Vec<_>>()
                    .join(" / ");
                format!("LOD {} 级（{} 起切换）", lods.len() + 1, distances)
            })
        })
        .collect();
    let mut changed = false;
    egui::CollapsingHeader::new(format!("模型（{} 个引用）", rows.len()))
        .default_open(false)
        .show(ui, |ui| {
            for (index, (path, position, rotation, scale)) in rows.iter_mut().enumerate() {
                ui.label(RichText::new(format!("模型 #{index}")).small().strong());
                ui.label(RichText::new(path.as_str()).small().weak());
                if let Some(count) = counts[index] {
                    ui.label(
                        RichText::new(format!("视口里 {count} 个物体（整组一起动）"))
                            .small()
                            .weak(),
                    );
                }
                if let Some(lod) = lod_rows.get(index).and_then(|l| l.as_ref()) {
                    ui.label(
                        RichText::new(lod)
                            .small()
                            .color(egui::Color32::from_rgb(150, 210, 240)),
                    );
                }
                ui.horizontal(|ui| {
                    ui.label("位置");
                    for (i, axis) in ["X", "Y", "Z"].iter().enumerate() {
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut position[i])
                                    .speed(0.02)
                                    .prefix(*axis),
                            )
                            .changed();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("旋转°");
                    for (i, axis) in ["X", "Y", "Z"].iter().enumerate() {
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut rotation[i])
                                    .speed(0.5)
                                    .prefix(*axis),
                            )
                            .changed();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("缩放");
                    changed |= ui
                        .add(
                            egui::DragValue::new(scale)
                                .speed(0.01)
                                .range(0.001..=1000.0),
                        )
                        .changed();
                });
                ui.add_space(4.0);
            }
        });
    if !changed {
        return;
    }
    let mut dirty: Vec<usize> = Vec::new();
    if let Some(base) = doc.base_doc.as_mut() {
        for (index, (m, (_, position, rotation, scale))) in
            base.models.iter_mut().zip(rows.iter()).enumerate()
        {
            let same_position = m.position.unwrap_or([0.0, 0.0, 0.0]) == *position;
            let same_rotation = m.rotation_deg.unwrap_or([0.0, 0.0, 0.0]) == *rotation;
            let same_scale = m.scale.unwrap_or(1.0) == *scale;
            if same_position && same_rotation && same_scale {
                continue;
            }
            m.position = Some(*position);
            m.rotation_deg = Some(*rotation);
            m.scale = Some(*scale);
            dirty.push(index);
        }
    }
    if dirty.is_empty() {
        return;
    }
    // 当场生效：把底稿里的新变换应用到引擎里那一组物体。
    let moved: usize = dirty
        .iter()
        .map(|index| doc.apply_model_transform(engine, *index))
        .sum();
    doc.status = if moved > 0 {
        format!("模型变换已应用（视口更新 {moved} 个物体；存盘后重开也一致）")
    } else {
        "模型变换已写入场景文件（重新打开后模型会到新位置）".to_string()
    };
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
            models_section(ui, engine, doc);
            animations_section(ui, engine, doc);
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
                doc.select_only(None);
                return;
            }

            material_section(ui, engine, doc);
            ui.separator();
            // 改父物体要在拿到 `objects[index]` 的可变借用之前发出请求，
            // 因为它要同时改场景里的两个物体（还得保持世界位姿）。
            let mut parent_request: Option<Option<usize>> = None;
            draw_parent_picker(ui, engine, index, &mut parent_request);
            let mut changed = false;
            // 变换是否真的被改过（与 `changed` 分开：改名字/颜色不该触发整组联动）。
            let transform_changed;
            let mut body_kind_change: Option<BodyKind> = None;
            let is_model_member = doc.model_of_object(index).is_some();
            {
                let object = &mut engine.scene.objects[index];
                let before_transform = object.transform;
                ui.horizontal(|ui| {
                    ui.label("名称");
                    if ui.text_edit_singleline(&mut object.name).changed() {
                        changed = true;
                    }
                });
                ui.checkbox(&mut object.visible, "可见");
                if is_model_member {
                    ui.label(
                        RichText::new("模型实例：变换整组联动、缩放是均匀的（写回 models）")
                            .small()
                            .weak(),
                    );
                }

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
                transform_changed = object.transform != before_transform;

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
                                // 静摩擦：切向速度很小时用（"推得动但自己不会慢慢滑"）。
                                // 缺省跟随动摩擦，所以这里显示的初值就是动摩擦的值。
                                let mut static_friction = object
                                    .body
                                    .and_then(|h| engine.world.body(h))
                                    .map(|b| b.static_friction)
                                    .unwrap_or(friction);
                                if ui
                                    .add(
                                        egui::Slider::new(&mut static_friction, 0.0..=2.0)
                                            .text("静摩擦"),
                                    )
                                    .on_hover_text("切向速度很小时用的摩擦系数（≥ 动摩擦才「推得动又推不住」）")
                                    .changed()
                                {
                                    if let Some(h) = object.body {
                                        if let Some(b) = engine.world.body_mut(h) {
                                            b.static_friction = static_friction;
                                        }
                                    }
                                }
                                // 合成规则：和别的物体接触时听谁的（缺省 = 世界规则）
                                let mut f_rule = object
                                    .body
                                    .and_then(|h| engine.world.body(h))
                                    .and_then(|b| b.friction_combine);
                                let mut r_rule = object
                                    .body
                                    .and_then(|h| engine.world.body(h))
                                    .and_then(|b| b.restitution_combine);
                                if combine_rule_combo(ui, "摩擦合成", &mut f_rule) {
                                    if let Some(h) = object.body {
                                        if let Some(b) = engine.world.body_mut(h) {
                                            b.friction_combine = f_rule;
                                        }
                                    }
                                }
                                if combine_rule_combo(ui, "弹性合成", &mut r_rule) {
                                    if let Some(h) = object.body {
                                        if let Some(b) = engine.world.body_mut(h) {
                                            b.restitution_combine = r_rule;
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
            // 改父物体：`Doc::set_parent` 会保持世界位姿不变，并自己写状态栏。
            if let Some(parent) = parent_request {
                if doc.set_parent(engine, index, parent) {
                    doc.record_edit(engine);
                }
            }
            // 属性面板里改的也是模型成员时，跟视口拖动走同一条路：整组一起动 + 写回 `models`。
            // 模型变换是**世界**变换（模型网格挂在层级里也是按世界写的）。
            if transform_changed {
                let transform = engine.scene.world_transform(index);
                if doc.apply_transform_to_model_group(engine, index, transform) {
                    doc.status = "模型实例：整组一起变换（已写回 models，存盘生效）".to_string();
                }
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
                    view.additive =
                        ui.input(|i| i.modifiers.shift || i.modifiers.command || i.modifiers.ctrl);
                }
            }
            // Shift/Ctrl + 左键拖动 = 框选（不转视角；app 层在按下时已经让路）。
            let additive_drag = ui.input(|i| {
                i.modifiers.shift || i.modifiers.command || i.modifiers.ctrl
            });
            if response.drag_started() && additive_drag && !gizmo_took {
                view.box_select_start = response.interact_pointer_pos();
            }
            if let Some(start) = view.box_select_start {
                let current = ui
                    .input(|i| i.pointer.interact_pos())
                    .unwrap_or(start);
                let rect_select = egui::Rect::from_two_pos(start, current);
                ui.painter().rect_filled(
                    rect_select,
                    0.0,
                    egui::Color32::from_rgba_unmultiplied(80, 160, 255, 40),
                );
                ui.painter().rect_stroke(
                    rect_select,
                    0.0,
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 160, 255)),
                    egui::StrokeKind::Inside,
                );
                let released = response.drag_stopped()
                    || !ui.input(|i| i.pointer.primary_down());
                if released {
                    if rect_select.width() >= 3.0 || rect_select.height() >= 3.0 {
                        doc.select_in_rect(
                            engine,
                            Vec2::new(rect.width().max(1.0), rect.height().max(1.0)),
                            Vec2::new(rect_select.min.x - rect.left(), rect_select.min.y - rect.top()),
                            Vec2::new(rect_select.max.x - rect.left(), rect_select.max.y - rect.top()),
                            true,
                        );
                    }
                    view.box_select_start = None;
                }
            }

            // 左上角提示：当前相机模式与操作。
            let hint = match engine.camera_mode {
                CameraMode::Orbit => {
                    "轨道相机：左键拖动 / 中键拖动 / Alt+左键 旋转，滚轮缩放，右键拖动平移；Shift+左键拖动框选"
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
            ui.label("左键点击：选中物体；Shift/Ctrl+左键拖动：框选");
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
            ui.label("F3 网格 · F4 阴影 · F5 接触点（传感器是青色） · F6 包围盒 · F7 查询探针（相机中心射线打到什么）");
            ui.label("Esc：退出");
        });
    doc.show_help = open;
}
