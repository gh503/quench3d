//! 编辑器的界面：工具栏、场景树、属性面板、状态栏、视口。
//!
//! 界面只表达意图：真正改场景/物理的代码在 [`crate::doc`] 里，
//! 需要平台能力（存盘、截图）的请求回传给 [`crate::app`]。

use crate::camera::CameraFeed;
use crate::doc::{BodyKind, Doc, Primitive};
use crate::gizmo::{Gizmo, Mode as GizmoMode, Pivot as GizmoPivot, Space as GizmoSpace};
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
    /// 把当前场景导出成单文件 glTF 2.0。
    pub export_gltf: bool,
    /// 顶部「导出 GLB」：写/下载二进制 GLB。
    pub export_glb: bool,
    /// 本帧左栏实际宽度（测试/探针用；不是布局输入）。
    pub outliner_width: f32,
    /// 本帧右栏实际宽度。
    pub inspector_width: f32,
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
    top_bar(ui, engine, doc, gizmo, &mut req, camera);
    status_bar(ui, engine, doc, gizmo);
    outliner(ui, engine, doc, &mut req);
    inspector(ui, engine, doc, &mut req);
    viewport(ui, engine, doc, view, gizmo);
    let ctx = ui.ctx().clone();
    help_window(&ctx, doc);
    camera.window(&ctx);
    // 桌面文件选择器：选完把路径交给 app，复用现有的 load/save 请求。
    #[cfg(not(target_arch = "wasm32"))]
    {
        let outcome = doc.picker.as_mut().map(|picker| picker.show(&ctx));
        if let Some(outcome) = outcome {
            match outcome {
                crate::picker::Outcome::Picked(picked) => apply_pick(doc, &mut req, picked),
                crate::picker::Outcome::Cancelled => doc.picker = None,
                crate::picker::Outcome::Pending => {}
            }
        }
    }
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

/// 把选择器结果落到文档路径，并转成原有的 load/save 请求。抽出来是为了能单测
/// 「选完文件后路径和请求真的同步变化」，而不是只测窗口画没画出来。
#[cfg(not(target_arch = "wasm32"))]
fn apply_pick(doc: &mut Doc, req: &mut Requests, picked: crate::picker::PickResult) {
    doc.picker = None;
    match (picked.mode, doc.picker_action) {
        (crate::picker::PickMode::Open, _) => {
            doc.file_path = picked.path.display().to_string();
            req.load = true;
        }
        (crate::picker::PickMode::Save, crate::picker::PickAction::ExportGltf) => {
            doc.export_path = Some(picked.path);
            req.export_gltf = true;
        }
        (crate::picker::PickMode::Save, crate::picker::PickAction::ExportGlb) => {
            doc.export_path = Some(picked.path);
            req.export_glb = true;
        }
        (crate::picker::PickMode::Save, _) => {
            doc.file_path = picked.path.display().to_string();
            req.save = true;
        }
    }
}

/// 顶部工具栏。
fn top_bar(
    ui: &mut Ui,
    engine: &mut Engine,
    doc: &mut Doc,
    gizmo: &mut Gizmo,
    req: &mut Requests,
    camera: &mut CameraFeed,
) {
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
            // 历史列表：点任意一步直接跳过去；被跳过的步骤进入重做链。
            let history_labels: Vec<String> = (0..doc.history.len())
                .map(|i| {
                    format!(
                        "{}. {}",
                        i + 1,
                        doc.history.label(i).unwrap_or("编辑")
                    )
                })
                .collect();
            let current_step = doc.history.len().saturating_sub(1);
            ui.menu_button("历史", |ui| {
                ui.set_min_width(260.0);
                if history_labels.len() <= 1 {
                    ui.label(egui::RichText::new("还没有可跳转的步骤").weak());
                }
                for (i, label) in history_labels.iter().enumerate() {
                    if ui
                        .selectable_label(i == current_step, label)
                        .clicked()
                        && i != current_step
                    {
                        doc.jump_history(engine, i);
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text("撤销历史：点一步直接跳回去");

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
            egui::ComboBox::from_id_salt("gizmo-pivot")
                .selected_text(gizmo.pivot.label())
                .show_ui(ui, |ui| {
                    for pivot in GizmoPivot::ALL {
                        ui.selectable_value(&mut gizmo.pivot, pivot, pivot.label());
                    }
                })
                .response
                .on_hover_text("旋转/缩放的轴心");
            if gizmo.pivot == GizmoPivot::Cursor {
                let mut cursor = gizmo.cursor.to_array();
                ui.horizontal(|ui| {
                    ui.label("光标");
                    for (i, axis) in ["X", "Y", "Z"].into_iter().enumerate() {
                        ui.label(axis);
                        ui.add(
                            egui::DragValue::new(&mut cursor[i])
                                .speed(0.05)
                                .range(-10_000.0..=10_000.0),
                        );
                    }
                    if ui
                        .button("到选中中心")
                        .on_hover_text("把 3D 光标放到当前选中物体的中心（快捷键 K）")
                        .clicked()
                    {
                        if let Some(center) = doc.selection_center(engine) {
                            gizmo.cursor = center;
                        }
                    }
                });
                gizmo.cursor = Vec3::new(cursor[0], cursor[1], cursor[2]);
            }
            ui.checkbox(&mut gizmo.enabled, "显示")
                .on_hover_text("关掉后视口里只剩拾取与转视角");

            ui.separator();

            ui.checkbox(&mut engine.renderer.options.grid, "网格");
            ui.checkbox(&mut engine.renderer.options.shadows, "阴影");
            ui.checkbox(&mut engine.renderer.options.auto_lod, "自动 LOD")
                .on_hover_text("高面数网格在载入时生成 1/2、1/4 两档；打开后需重新载入场景才作用于已有网格");
            ui.checkbox(&mut engine.debug.joints, "关节")
                .on_hover_text("画出物理关节的锚点、连线与铰链/滑块轴");
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
            ui.checkbox(&mut engine.renderer.options.taa.enabled, "时域抗锯齿（TAA）")
                .on_hover_text(
                    "用上一帧历史 + 深度重投影做亚像素累积；相机每帧轻微抖动。\n                     比 FXAA 更平滑，但运动物体会有一点拖影；需要可读深度且未开 MSAA。",
                );
            ui.add_enabled_ui(engine.renderer.options.taa.enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.label("历史权重");
                    ui.add(
                        egui::DragValue::new(&mut engine.renderer.options.taa.feedback)
                            .speed(0.01)
                            .range(0.0..=0.98),
                    )
                    .on_hover_text("越大越平滑、越容易拖影；0 = 不累积");
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
            ui.add_enabled_ui(engine.renderer.options.ssao.enabled, |ui| {
                ui.checkbox(
                    &mut engine.renderer.options.ssao.denoise,
                    "SSAO 去噪（4 邻域双边，5× 采样）",
                )
                .on_hover_text("压掉接触处的颗粒；边缘按深度差保护，不会糊墙。");
            });
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

            // 场景 `cameras` 段的命名视角：只切主相机（不做分屏/多视口）。
            let named_cameras: Vec<(String, quench3d_cli::scene_doc::CameraDoc)> = doc
                .base_doc()
                .map(|base| {
                    base.cameras
                        .iter()
                        .map(|(name, camera)| (name.clone(), camera.clone()))
                        .collect()
                })
                .unwrap_or_default();
            if !named_cameras.is_empty() {
                let current = doc
                    .selected_camera
                    .clone()
                    .unwrap_or_else(|| "编辑器视角".to_string());
                ui.horizontal(|ui| {
                if ui.button("◀").on_hover_text("上一台命名相机").clicked() {
                    doc.cycle_named_camera(engine, -1);
                }
                if ui.button("▶").on_hover_text("下一台命名相机").clicked() {
                    doc.cycle_named_camera(engine, 1);
                }
                egui::ComboBox::from_id_salt("named-camera")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        if ui
                            .selectable_label(doc.selected_camera.is_none(), "编辑器视角")
                            .clicked()
                        {
                            doc.selected_camera = None;
                        }
                        for (name, _camera) in &named_cameras {
                            let selected = doc.selected_camera.as_deref() == Some(name.as_str());
                            if ui.selectable_label(selected, name).clicked() {
                                doc.select_named_camera(engine, name);
                            }
                        }
                    })
                    .response
                    .on_hover_text("场景 `cameras` 段里的命名视角；只切换主相机");
                });
                // 画中画：右下角用第二个命名相机做 CPU 回读预览（桌面端）。
                // 它不是实时 GPU 分屏，频率被限制在约 5 Hz。
                #[cfg(not(target_arch = "wasm32"))]
                ui.horizontal(|ui| {
                    ui.label("画中画");
                    let current = doc.pip_camera.clone().unwrap_or_else(|| "关闭".into());
                    egui::ComboBox::from_id_salt("pip-camera")
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_label(doc.pip_camera.is_none(), "关闭")
                                .clicked()
                            {
                                doc.pip_camera = None;
                                doc.pip_texture = None;
                            }
                            for (name, _camera) in &named_cameras {
                                let selected = doc.pip_camera.as_deref() == Some(name.as_str());
                                if ui.selectable_label(selected, name).clicked() {
                                    doc.pip_camera = Some(name.clone());
                                    doc.pip_texture = None;
                                    doc.pip_last_frame = 0;
                                }
                            }
                        })
                        .response
                        .on_hover_text("小窗预览第二台命名相机（约 5 Hz，CPU 回读）");
                    egui::ComboBox::from_id_salt("pip-corner")
                        .selected_text(match doc.pip_corner {
                            1 => "左下",
                            2 => "右上",
                            3 => "左上",
                            _ => "右下",
                        })
                        .show_ui(ui, |ui| {
                            for (value, label) in
                                [(0, "右下"), (1, "左下"), (2, "右上"), (3, "左上")]
                            {
                                ui.selectable_value(&mut doc.pip_corner, value, label);
                            }
                        });
                });
            }

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
            camera.toolbar(ui);
            if ui.button("打开").on_hover_text("选择并读入 scene.json").clicked() {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    doc.picker_action = crate::picker::PickAction::OpenScene;
                    doc.picker = Some(crate::picker::FilePicker::for_open(
                        std::path::Path::new(&doc.file_path),
                        &["json"],
                        "打开场景",
                    ));
                }
                #[cfg(target_arch = "wasm32")]
                {
                    req.load = true;
                }
            }
            let save = ui
                .button("保存")
                .on_hover_text("写出 scene.json（右键：另存为）");
            if save.clicked() {
                req.save = true;
            }
            // 「另存为」放在保存按钮的右键菜单里：主工具栏已经排得很满，再加一个
            // 按钮会把「导出 GLB」挤到 min_size 之外被裁掉（UI 冒烟测试抓过）。
            #[cfg(not(target_arch = "wasm32"))]
            save.context_menu(|ui| {
                if ui.button("另存为…").clicked() {
                    let default = std::path::Path::new(&doc.file_path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("scene.json")
                        .to_string();
                    doc.picker_action = crate::picker::PickAction::SaveScene;
                    doc.picker = Some(crate::picker::FilePicker::for_save(
                        std::path::Path::new(&doc.file_path),
                        &default,
                        &["json"],
                        "场景另存为",
                    ));
                    ui.close();
                }
            });
            if ui
                .button("导出 glTF")
                .on_hover_text("把当前场景的图元导出成单文件 glTF 2.0（base64 几何/贴图）")
                .clicked()
            {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    doc.picker_action = crate::picker::PickAction::ExportGltf;
                    doc.picker = Some(crate::picker::FilePicker::for_save(
                        &std::path::Path::new(&doc.file_path).with_extension("gltf"),
                        "scene.gltf",
                        &["gltf"],
                        "导出 glTF",
                    ));
                }
                #[cfg(target_arch = "wasm32")]
                {
                    req.export_gltf = true;
                }
            }
            if ui
                .button("导出 GLB")
                .on_hover_text("把当前场景的图元导出成二进制 GLB（JSON + BIN chunk）")
                .clicked()
            {
                #[cfg(not(target_arch = "wasm32"))]
                {
                    doc.picker_action = crate::picker::PickAction::ExportGlb;
                    doc.picker = Some(crate::picker::FilePicker::for_save(
                        &std::path::Path::new(&doc.file_path).with_extension("glb"),
                        "scene.glb",
                        &["glb"],
                        "导出 GLB",
                    ));
                }
                #[cfg(target_arch = "wasm32")]
                {
                    req.export_glb = true;
                }
            }
            if ui
                .button("截图")
                .on_hover_text("把当前画面存成 PNG")
                .clicked()
            {
                req.screenshot = true;
            }
            ui.menu_button("布局", |ui| {
                ui.set_min_width(220.0);
                ui.label(RichText::new("面板宽度（逻辑像素）").small().weak());
                let mut changed = false;
                changed |= ui
                    .add(
                        egui::Slider::new(
                            &mut doc.layout.outliner_width,
                            crate::layout::OUTLINER_MIN..=crate::layout::OUTLINER_MAX,
                        )
                        .text("左栏"),
                    )
                    .changed();
                changed |= ui
                    .add(
                        egui::Slider::new(
                            &mut doc.layout.inspector_width,
                            crate::layout::INSPECTOR_MIN..=crate::layout::INSPECTOR_MAX,
                        )
                        .text("右栏"),
                    )
                    .changed();
                if ui.button("恢复默认").clicked() {
                    doc.layout = crate::layout::Layout::default();
                    changed = true;
                }
                if changed {
                    doc.layout.clamp();
                    if let Err(e) = doc.layout.save() {
                        doc.status = format!("布局保存失败：{e}");
                    }
                }
            });
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
        let response = ui
            .selectable_label(doc.is_selected(index), label)
            .interact(egui::Sense::click_and_drag());
        if response.clicked() {
            let additive =
                ui.input(|i| i.modifiers.shift || i.modifiers.command || i.modifiers.ctrl);
            if additive {
                doc.toggle_selected(index);
            } else {
                doc.select_only(Some(index));
            }
        }
        // 拖动行改父子：模型分组成员不参与（改一半会破坏模型实例的整组变换）。
        if response.drag_started() && doc.model_of_object(index).is_none() {
            doc.drag_source = Some(index);
        }
        if let Some(source) = doc.drag_source {
            if source != index
                && doc.model_of_object(index).is_none()
                && ui.input(|i| i.pointer.any_released())
                && response
                    .rect
                    .contains(ui.input(|i| i.pointer.interact_pos()).unwrap_or_default())
            {
                doc.reparent_request = Some((source, Some(index)));
                doc.drag_source = None;
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
fn outliner(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc, req: &mut Requests) {
    let width = doc
        .layout
        .outliner_width
        .clamp(crate::layout::OUTLINER_MIN, crate::layout::OUTLINER_MAX);
    let response = egui::Panel::left("quench3d-outliner")
        .default_size(width)
        .min_size(width)
        .max_size(width)
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
            // 拖到列表空白处 = 解除父子（回到根）。只在正在拖动时占位，平时不挡点击。
            if doc.drag_source.is_some() {
                let (rect, drop) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 24.0),
                    egui::Sense::hover(),
                );
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "拖到这里解除父子",
                    egui::FontId::proportional(12.0),
                    egui::Color32::from_rgb(180, 180, 180),
                );
                if drop.hovered() && ui.input(|i| i.pointer.any_released()) {
                    if let Some(source) = doc.drag_source.take() {
                        doc.reparent_request = Some((source, None));
                    }
                }
            }
            // 拖放只发请求：等遍历结束（只读借用结束）再改两个物体并写历史。
            if let Some((source, parent)) = doc.reparent_request.take() {
                if doc.set_parent(engine, source, parent) {
                    doc.record_edit(engine);
                }
            }
            if ui.input(|i| i.pointer.any_released()) {
                doc.drag_source = None;
            }
        });
    req.outliner_width = response.response.rect.width();
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
                let mut tonemap = engine.renderer.options.tonemap;
                egui::ComboBox::from_id_salt("tonemap")
                    .selected_text(tonemap.label())
                    .show_ui(ui, |ui| {
                        for candidate in quench3d::prelude::Tonemap::ALL {
                            ui.selectable_value(&mut tonemap, candidate, candidate.label());
                        }
                    });
                if tonemap != engine.renderer.options.tonemap {
                    engine.renderer.options.tonemap = tonemap;
                    changed = true;
                }
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
                        ui.label("阴影强度/半径/偏移");
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
                        changed |= ui
                            .add(
                                egui::DragValue::new(&mut sun.shadow_bias)
                                    .speed(0.05)
                                    .range(0.0..=4.0)
                                    .prefix("偏移 "),
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
    // 有显式贴图设置时自动展开：否则用户设置了贴图却看不到入口。
    let has_texture_settings = doc.textures.contains_key(&object_name);
    egui::CollapsingHeader::new("材质（缩放与开关）")
        .default_open(has_texture_settings)
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
                        let resolved = doc.resolve_texture_path(&path);
                        match quench3d::render::ImageData::from_file(&resolved) {
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
                                    match slot {
                                        quench3d::render::MaterialTextureSlot::BaseColor => {
                                            desc.base_color_texture = Some(texture)
                                        }
                                        quench3d::render::MaterialTextureSlot::Normal => {
                                            desc.normal_texture = Some(texture)
                                        }
                                        quench3d::render::MaterialTextureSlot::MetallicRoughness => {
                                            desc.metallic_roughness_texture = Some(texture)
                                        }
                                        quench3d::render::MaterialTextureSlot::Emissive => {
                                            desc.emissive_texture = Some(texture)
                                        }
                                    }
                                    doc.record_edit(engine);
                                }
                            }
                            Err(error) => {
                                // 场景校验器要求贴图文件存在；失败必须说清原因，不能静默留旧图。
                                doc.status = format!("读图失败：{error}（{path}）");
                            }
                        }
                    }
                    if ui.button(format!("清除{label}")).clicked() {
                        let ok = engine.renderer.set_material_texture(
                            &engine.gpu,
                            material_id,
                            slot,
                            None,
                        );
                        if ok {
                            match slot {
                                quench3d::render::MaterialTextureSlot::BaseColor => {
                                    desc.base_color_texture = None
                                }
                                quench3d::render::MaterialTextureSlot::Normal => {
                                    desc.normal_texture = None
                                }
                                quench3d::render::MaterialTextureSlot::MetallicRoughness => {
                                    desc.metallic_roughness_texture = None
                                }
                                quench3d::render::MaterialTextureSlot::Emissive => {
                                    desc.emissive_texture = None
                                }
                            }
                            let entry = doc.textures.entry(object_name.clone()).or_default();
                            match slot {
                                quench3d::render::MaterialTextureSlot::BaseColor => {
                                    entry.base_color = None
                                }
                                quench3d::render::MaterialTextureSlot::Normal => {
                                    entry.normal = None
                                }
                                quench3d::render::MaterialTextureSlot::MetallicRoughness => {
                                    entry.metallic_roughness = None
                                }
                                quench3d::render::MaterialTextureSlot::Emissive => {
                                    entry.emissive = None
                                }
                            }
                            doc.status = format!("已清除{label}");
                            doc.record_edit(engine);
                        } else {
                            doc.status = format!("{label}清除失败（材质句柄无效）");
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

/// 把时间轴上的像素 X 映射成 clip 时间；范围外夹到 `[0, duration]`。
fn timeline_time_at(rect: egui::Rect, x: f32, duration: f32) -> f32 {
    if !duration.is_finite() || duration <= 0.0 || rect.width() <= 0.0 {
        return 0.0;
    }
    let t = (x - rect.left()) / rect.width();
    t.clamp(0.0, 1.0) * duration
}

/// 单 clip 的可视化时间轴：点击/拖动定位，返回新的时间（没变返回 `None`）。
fn timeline_bar(ui: &mut Ui, time: f32, duration: f32, id: usize) -> Option<f32> {
    if !duration.is_finite() || duration <= 0.0 {
        return None;
    }
    let width = ui.available_width().max(120.0);
    let (rect, response) = ui
        .push_id(("quench3d-timeline", id), |ui| {
            ui.allocate_exact_size(egui::vec2(width, 26.0), egui::Sense::click_and_drag())
        })
        .inner;
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, ui.visuals().extreme_bg_color);
    let fraction = (time / duration).clamp(0.0, 1.0);
    let mut progress = rect;
    progress.set_width(rect.width() * fraction);
    painter.rect_filled(progress, 3.0, ui.visuals().selection.bg_fill);
    // 0 / 25 / 50 / 75 / 100% 刻度：没有文字也能看出这是时间，不是进度条。
    for i in 0..=4 {
        let x = rect.left() + rect.width() * (i as f32 / 4.0);
        let color = if i == 0 || i == 4 {
            ui.visuals().weak_text_color()
        } else {
            ui.visuals().weak_text_color().gamma_multiply(0.5)
        };
        painter.line_segment(
            [
                egui::pos2(x, rect.top() + 4.0),
                egui::pos2(x, rect.bottom() - 4.0),
            ],
            egui::Stroke::new(1.0, color),
        );
    }
    let playhead = rect.left() + rect.width() * fraction;
    painter.line_segment(
        [
            egui::pos2(playhead, rect.top()),
            egui::pos2(playhead, rect.bottom()),
        ],
        egui::Stroke::new(2.0, ui.visuals().strong_text_color()),
    );
    let label = format!("{time:.3}s / {duration:.3}s");
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::monospace(11.0),
        ui.visuals().text_color(),
    );
    let response = response.on_hover_text("点击或拖动定位动画时间");
    let mut changed = None;
    if response.clicked() || response.dragged() {
        if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
            let next = timeline_time_at(rect, pos.x, duration);
            if (next - time).abs() > 1e-6 {
                changed = Some(next);
            }
        }
    }
    if response.double_clicked() && time.abs() > 1e-6 {
        changed = Some(0.0);
    }
    changed
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
    let mut event_updates: Vec<(usize, Vec<SceneAnimationEvent>)> = Vec::new();
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
                        "时长 {duration:.2}s · 倍速 {:.2} · {state} · 事件 {} 条",
                        animation.player.speed,
                        animation.events.len()
                    ))
                    .small()
                    .weak(),
                );
                if let Some(root) = animation.root_motion {
                    let node = animation
                        .model
                        .nodes
                        .get(root.node)
                        .map(|n| n.name.as_str())
                        .unwrap_or("?");
                    ui.label(
                        RichText::new(format!("根运动 · {node}（位移 + 偏航）"))
                            .small()
                            .weak(),
                    );
                }
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
                        let mut weight = chain.weight;
                        let response = ui.add(
                            egui::DragValue::new(&mut weight)
                                .range(0.0..=1.0)
                                .speed(0.01)
                                .prefix("权重 "),
                        );
                        if response.drag_started() {
                            begin_edit = true;
                        }
                        if response.changed() {
                            chain.weight = weight;
                            if models[index].is_some() {
                                if let Some(path) = doc.set_ik_weight(&objects, chain_index, weight)
                                {
                                    changed_model = Some(path);
                                }
                            }
                            refresh = true;
                        }
                        if response.drag_stopped() {
                            end_edit = true;
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
                    // 可视化时间轴：和滑杆共享同一个 `player.time`，点/拖都能定位。
                    // 这是单 clip 的检查器，不是多轨混合器（混合仍靠 secondary + weight）。
                    ui.label(RichText::new("时间轴").small().strong());
                    if let Some(scrubbed) = timeline_bar(ui, animation.player.time, duration, index)
                    {
                        animation.player.time = scrubbed;
                        animation.player.finished = false;
                        refresh = true;
                    }
                }
                ui.label(
                    RichText::new("事件标记（播放头跨过时触发）")
                        .small()
                        .strong(),
                );
                let mut events_changed = false;
                let mut remove_event = None;
                for (event_index, event) in animation.events.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        let mut name = event.name.clone();
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut name)
                                    .desired_width(76.0)
                                    .hint_text("名字"),
                            )
                            .changed()
                        {
                            event.name = name;
                            events_changed = true;
                        }
                        let mut time = event.time;
                        if ui
                            .add(
                                egui::DragValue::new(&mut time)
                                    .range(0.0..=duration)
                                    .speed(0.005)
                                    .prefix("t "),
                            )
                            .changed()
                        {
                            event.time = time;
                            events_changed = true;
                        }
                        let mut scope = event.clip.clone();
                        egui::ComboBox::from_id_salt(format!("event-clip-{index}-{event_index}"))
                            .selected_text(scope.clone().unwrap_or_else(|| "主 clip".to_string()))
                            .width(78.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut scope, None, "主 clip");
                                for clip in &clips[index] {
                                    ui.selectable_value(&mut scope, Some(clip.clone()), clip);
                                }
                            });
                        if scope != event.clip {
                            event.clip = scope;
                            events_changed = true;
                        }
                        ui.label(RichText::new("负载").small().weak());
                        let mut payload = event.payload.clone().unwrap_or_default();
                        if ui
                            .add(
                                egui::TextEdit::singleline(&mut payload)
                                    .desired_width(70.0)
                                    .hint_text("可选"),
                            )
                            .changed()
                        {
                            event.payload = if payload.is_empty() {
                                None
                            } else {
                                Some(payload)
                            };
                            events_changed = true;
                        }
                        if ui.small_button("删").clicked() {
                            remove_event = Some(event_index);
                        }
                    });
                }
                if let Some(remove) = remove_event {
                    animation.events.remove(remove);
                    events_changed = true;
                }
                ui.horizontal(|ui| {
                    if ui.button("在播放头加事件").clicked() {
                        let name = {
                            let mut n = animation.events.len() + 1;
                            loop {
                                let candidate = format!("event{n}");
                                if !animation.events.iter().any(|e| e.name == candidate) {
                                    break candidate;
                                }
                                n += 1;
                            }
                        };
                        animation.events.push(SceneAnimationEvent {
                            name,
                            time: animation.player.time.clamp(0.0, duration),
                            clip: None,
                            payload: None,
                        });
                        events_changed = true;
                    }
                    if animation.events.is_empty() {
                        ui.label(RichText::new("（无）").small().weak());
                    }
                });
                if events_changed {
                    if animation.events.iter().any(|e| e.name.trim().is_empty()) {
                        doc.status = "事件名不能为空（未写回场景；修好后才会保存）".to_string();
                    } else {
                        if models[index].is_some() {
                            let docs: Vec<quench3d_cli::scene_doc::ModelEventDoc> = animation
                                .events
                                .iter()
                                .map(|e| quench3d_cli::scene_doc::ModelEventDoc {
                                    name: e.name.trim().to_string(),
                                    time: e.time,
                                    clip: e.clip.clone(),
                                    payload: e.payload.clone(),
                                })
                                .collect();
                            if let Some(path) = doc.set_animation_events(&objects, docs) {
                                changed_model = Some(path);
                            }
                        }
                        event_updates.push((index, animation.events.clone()));
                        record_edit = true;
                    }
                }

                if let Some(secondary) = &animation.secondary {
                    ui.label(
                        RichText::new(format!(
                            "过渡到「{}」· 权重 {:.2}",
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
                                    animator.queue_event(name.clone());
                                    step_animator = true;
                                }
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("事件窗口").small().weak())
                            .on_hover_text(
                                "输入事件在这么多秒内没被接住就丢弃；0 = 不过期。\n只改本次会话，场景默认值写 models[i].animator.event_window。",
                            );
                        ui.add(
                            egui::DragValue::new(&mut animator.event_window)
                                .speed(0.01)
                                .range(0.0..=10.0)
                                .suffix("s"),
                        );
                    });
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
                                format!(
                                    "状态机：{current} → {to}（停留 {:.2}s · 待处理事件 {}）",
                                    animator.elapsed,
                                    animator.queued_events()
                                )
                            }
                            None => format!(
                                "状态机：{current}（停留 {:.2}s · 待处理事件 {}）",
                                animator.elapsed,
                                animator.queued_events()
                            ),
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
    for (index, events) in event_updates {
        engine.scene.set_model_events(index, events);
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

/// 粒子发射器面板。粒子代理在场景树里是隐藏的，没有这块面板就完全看不见；
/// 参数可直接改，并写回底稿的 `particles[i]`，保存后保持。
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn replace_decal_texture(
    engine: &mut Engine,
    doc: &mut Doc,
    index: usize,
    path: &str,
) -> Result<(), String> {
    let object_index = engine
        .scene
        .objects
        .iter()
        .position(|o| o.name == format!("贴花{index}"))
        .ok_or_else(|| format!("找不到贴花{index}"))?;
    let path = path.trim();
    let texture_id = if path.is_empty() {
        None
    } else {
        let resolved = doc.resolve_texture_path(path);
        let image = quench3d::render::ImageData::from_file(&resolved)
            .map_err(|e| format!("贴花贴图读图失败：{e}（{}）", resolved.display()))?;
        Some(engine.renderer.add_texture(&engine.gpu, &image, true))
    };
    let old = engine.scene.objects[object_index].material;
    let mut desc = engine.renderer.material(old).cloned().unwrap_or_default();
    desc.base_color_texture = None;
    let name = format!("$decal_ui_{index}_{}", engine.renderer.material_count());
    let material = engine
        .assets
        .material(&mut engine.renderer, &engine.gpu, &name, desc);
    if let Some(texture_id) = texture_id {
        if !engine.renderer.set_material_texture(
            &engine.gpu,
            material,
            quench3d::render::MaterialTextureSlot::BaseColor,
            Some(texture_id),
        ) {
            return Err("贴花贴图应用失败（材质或贴图句柄无效）".into());
        }
    }
    engine.scene.objects[object_index].material = material;
    if let Some(base) = doc.base_doc_mut() {
        if let Some(decal) = base.decals.get_mut(index) {
            decal.texture = if path.is_empty() {
                None
            } else {
                Some(path.to_string())
            };
        }
    }
    doc.record_edit(engine);
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn replace_particle_texture(
    engine: &mut Engine,
    doc: &mut Doc,
    index: usize,
    path: &str,
) -> Result<(), String> {
    if index >= engine.scene.particles.len() {
        return Err(format!("找不到粒子发射器 #{index}"));
    }
    let path = path.trim();
    let texture_id = if path.is_empty() {
        None
    } else {
        let resolved = doc.resolve_texture_path(path);
        let image = quench3d::render::ImageData::from_file(&resolved)
            .map_err(|e| format!("粒子贴图读图失败：{e}（{}）", resolved.display()))?;
        Some(engine.renderer.add_texture(&engine.gpu, &image, true))
    };
    let old_material = engine.scene.particles[index].material;
    let objects = engine.scene.particles[index].objects.clone();
    let mut desc = engine
        .renderer
        .material(old_material)
        .cloned()
        .unwrap_or_default();
    desc.base_color_texture = None;
    let name = format!("$particle_ui_{index}_{}", engine.renderer.material_count());
    let material = engine
        .assets
        .material(&mut engine.renderer, &engine.gpu, &name, desc);
    if let Some(texture_id) = texture_id {
        if !engine.renderer.set_material_texture(
            &engine.gpu,
            material,
            quench3d::render::MaterialTextureSlot::BaseColor,
            Some(texture_id),
        ) {
            return Err("粒子贴图应用失败（材质或贴图句柄无效）".into());
        }
    }
    engine.scene.particles[index].material = material;
    for object in objects {
        if let Some(obj) = engine.scene.objects.get_mut(object) {
            obj.material = material;
        }
    }
    if let Some(base) = doc.base_doc_mut() {
        if let Some(particle) = base.particles.get_mut(index) {
            particle.texture = if path.is_empty() {
                None
            } else {
                Some(path.to_string())
            };
        }
    }
    doc.record_edit(engine);
    Ok(())
}

fn particles_section(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc) {
    if engine.scene.particles.is_empty() {
        return;
    }
    ui.separator();
    ui.label(RichText::new(format!(
        "粒子发射器（{} 组）",
        engine.scene.particles.len()
    )));
    for i in 0..engine.scene.particles.len() {
        // 先取快照，避免在画控件时长期借用 engine.scene。
        let (
            objects,
            lifetime,
            size,
            size_end,
            drag,
            rotation,
            rotation_end,
            enabled,
            shape,
            radius,
            box_size,
            alignment,
            color,
            color_end,
        ) = {
            let system = &engine.scene.particles[i];
            (
                system.objects.len(),
                system.lifetime,
                system.size,
                system.size_end,
                system.drag,
                system.rotation,
                system.rotation_end,
                system.enabled,
                system.shape,
                system.radius,
                system.box_size,
                system.alignment,
                system.color,
                system.color_end,
            )
        };
        ui.label(RichText::new(format!("#{i} · {objects} 颗")).small());
        let shape_text = match shape {
            quench3d::scene::ParticleShape::Sphere => format!("球 r={radius:.2}"),
            quench3d::scene::ParticleShape::Box => {
                format!("盒 {:.1}×{:.1}×{:.1}", box_size.x, box_size.y, box_size.z)
            }
            quench3d::scene::ParticleShape::Disc => format!("圆盘 r={radius:.2}"),
            quench3d::scene::ParticleShape::Cone => {
                format!("锥 h={:.1} r={radius:.2}", box_size.y)
            }
        };
        let mut suffix = Vec::new();
        if alignment == quench3d::scene::ParticleAlignment::Velocity {
            suffix.push("速度对齐".to_string());
        }
        if drag > 0.0 {
            suffix.push(format!("阻尼 {drag:.2}/s"));
        }
        if rotation.abs() > 1e-6 || rotation_end.abs() > 1e-6 {
            suffix.push(format!(
                "自转 {:.0}°→{:.0}°",
                rotation.to_degrees(),
                rotation_end.to_degrees()
            ));
        }
        if !suffix.is_empty() {
            ui.label(RichText::new(format!("{shape_text} · {}", suffix.join(" · "))).small());
        } else {
            ui.label(RichText::new(shape_text).small());
        }

        let mut new_enabled = enabled;
        let mut new_count = objects;
        let mut new_lifetime = lifetime;
        let mut new_size = size;
        let mut new_size_end = size_end;
        let mut new_drag = drag;
        let mut new_rotation = rotation.to_degrees();
        let mut new_rotation_end = rotation_end.to_degrees();
        let mut new_radius = radius;
        let mut new_box_y = box_size.y;
        let mut new_shape = shape;
        let mut new_alignment = alignment;
        let mut new_color = [color.x, color.y, color.z, color.w];
        let mut new_color_end = [color_end.x, color_end.y, color_end.z, color_end.w];
        let mut changed = false;
        changed |= ui.checkbox(&mut new_enabled, "启用").changed();
        ui.horizontal(|ui| {
            ui.label("数量");
            changed |= ui
                .add(egui::DragValue::new(&mut new_count).range(1..=256).speed(1))
                .changed();
            ui.label("形状");
            egui::ComboBox::from_id_salt(("particle-shape", i))
                .selected_text(particle_shape_name(new_shape))
                .show_ui(ui, |ui| {
                    for shape in [
                        quench3d::scene::ParticleShape::Sphere,
                        quench3d::scene::ParticleShape::Box,
                        quench3d::scene::ParticleShape::Disc,
                        quench3d::scene::ParticleShape::Cone,
                    ] {
                        changed |= ui
                            .selectable_value(&mut new_shape, shape, particle_shape_name(shape))
                            .changed();
                    }
                });
            ui.label("朝向");
            egui::ComboBox::from_id_salt(("particle-alignment", i))
                .selected_text(particle_alignment_name(new_alignment))
                .show_ui(ui, |ui| {
                    for alignment in [
                        quench3d::scene::ParticleAlignment::Billboard,
                        quench3d::scene::ParticleAlignment::Velocity,
                    ] {
                        changed |= ui
                            .selectable_value(
                                &mut new_alignment,
                                alignment,
                                particle_alignment_name(alignment),
                            )
                            .changed();
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label("颜色");
            changed |= ui
                .color_edit_button_rgba_unmultiplied(&mut new_color)
                .changed();
        });
        ui.horizontal(|ui| {
            ui.label("终点色");
            changed |= ui
                .color_edit_button_rgba_unmultiplied(&mut new_color_end)
                .changed();
        });
        ui.horizontal(|ui| {
            ui.label("寿命");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut new_lifetime)
                        .range(0.05..=60.0)
                        .speed(0.1),
                )
                .changed();
            ui.label("尺寸");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut new_size)
                        .range(0.001..=10.0)
                        .speed(0.01),
                )
                .changed();
        });
        ui.horizontal(|ui| {
            ui.label("收束");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut new_size_end)
                        .range(0.001..=10.0)
                        .speed(0.01),
                )
                .changed();
            ui.label("阻尼");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut new_drag)
                        .range(0.0..=20.0)
                        .speed(0.05),
                )
                .changed();
        });
        ui.horizontal(|ui| {
            ui.label("自转");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut new_rotation)
                        .range(-360.0..=360.0)
                        .speed(1.0)
                        .suffix("°"),
                )
                .changed();
            ui.label("到");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut new_rotation_end)
                        .range(-360.0..=360.0)
                        .speed(1.0)
                        .suffix("°"),
                )
                .changed();
        });
        ui.horizontal(|ui| {
            ui.label("半径");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut new_radius)
                        .range(0.0..=20.0)
                        .speed(0.05),
                )
                .changed();
            if matches!(
                shape,
                quench3d::scene::ParticleShape::Box | quench3d::scene::ParticleShape::Cone
            ) {
                ui.label("高");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut new_box_y)
                            .range(0.01..=20.0)
                            .speed(0.05),
                    )
                    .changed();
            }
        });
        let mut texture = doc
            .base_doc()
            .and_then(|base| base.particles.get(i))
            .and_then(|p| p.texture.clone())
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label("发射器贴图");
            ui.text_edit_singleline(&mut texture);
        });
        #[cfg(not(target_arch = "wasm32"))]
        {
            if ui
                .add_enabled(
                    !texture.trim().is_empty(),
                    egui::Button::new("应用粒子贴图（从磁盘读取）"),
                )
                .clicked()
            {
                match replace_particle_texture(engine, doc, i, &texture) {
                    Ok(()) => doc.status = format!("粒子贴图已应用：{}", texture.trim()),
                    Err(e) => doc.status = e,
                }
            }
            if ui
                .add_enabled(
                    doc.base_doc()
                        .and_then(|base| base.particles.get(i))
                        .and_then(|p| p.texture.as_ref())
                        .is_some(),
                    egui::Button::new("清除粒子贴图"),
                )
                .clicked()
            {
                match replace_particle_texture(engine, doc, i, "") {
                    Ok(()) => doc.status = "粒子贴图已清除".into(),
                    Err(e) => doc.status = e,
                }
            }
        }
        if changed {
            engine.scene.set_particle_enabled(i, new_enabled);
            if new_count != objects {
                engine.scene.set_particle_count(i, new_count);
            }
            let system = &mut engine.scene.particles[i];
            system.lifetime = new_lifetime.max(0.05);
            system.size = new_size.max(0.001);
            system.size_end = new_size_end.max(0.001);
            system.drag = new_drag.max(0.0);
            system.rotation = new_rotation.to_radians();
            system.rotation_end = new_rotation_end.to_radians();
            system.radius = new_radius.max(0.0);
            system.box_size.y = new_box_y.max(0.01);
            system.shape = new_shape;
            system.alignment = new_alignment;
            system.color = Vec4::new(new_color[0], new_color[1], new_color[2], new_color[3]);
            system.color_end = Vec4::new(
                new_color_end[0],
                new_color_end[1],
                new_color_end[2],
                new_color_end[3],
            );
            if let Some(base) = doc.base_doc_mut() {
                if let Some(particle) = base.particles.get_mut(i) {
                    particle.enabled = Some(new_enabled);
                    particle.count = Some(system.objects.len());
                    particle.lifetime = Some(system.lifetime);
                    particle.size = Some(system.size);
                    particle.size_end = Some(system.size_end);
                    particle.drag = Some(system.drag);
                    particle.rotation = Some(system.rotation.to_degrees());
                    particle.rotation_end = Some(system.rotation_end.to_degrees());
                    particle.radius = Some(system.radius);
                    particle.box_size = Some(resolve_box_size(system.box_size));
                    particle.shape = Some(particle_shape_json(new_shape).to_string());
                    particle.alignment = Some(particle_alignment_json(new_alignment).to_string());
                    particle.color = Some(new_color);
                    particle.color_end = Some(new_color_end);
                }
            }
            if doc.base_doc().is_some() {
                doc.record_edit(engine);
            }
        }
    }
}

/// 形状在界面/JSON 里的名字。
fn particle_shape_json(shape: quench3d::scene::ParticleShape) -> &'static str {
    match shape {
        quench3d::scene::ParticleShape::Sphere => "sphere",
        quench3d::scene::ParticleShape::Box => "box",
        quench3d::scene::ParticleShape::Disc => "disc",
        quench3d::scene::ParticleShape::Cone => "cone",
    }
}

/// 形状的界面标签。
fn particle_shape_name(shape: quench3d::scene::ParticleShape) -> &'static str {
    match shape {
        quench3d::scene::ParticleShape::Sphere => "球",
        quench3d::scene::ParticleShape::Box => "盒",
        quench3d::scene::ParticleShape::Disc => "圆盘",
        quench3d::scene::ParticleShape::Cone => "锥",
    }
}

/// 朝向模式在界面/JSON 里的名字。
fn particle_alignment_json(alignment: quench3d::scene::ParticleAlignment) -> &'static str {
    match alignment {
        quench3d::scene::ParticleAlignment::Billboard => "billboard",
        quench3d::scene::ParticleAlignment::Velocity => "velocity",
    }
}

/// 朝向的界面标签。
fn particle_alignment_name(alignment: quench3d::scene::ParticleAlignment) -> &'static str {
    match alignment {
        quench3d::scene::ParticleAlignment::Billboard => "公告板",
        quench3d::scene::ParticleAlignment::Velocity => "速度",
    }
}

/// 保证写回的 box_size 每轴都是正数（UI 只改 Y，X/Z 保持原值）。
fn resolve_box_size(value: Vec3) -> [f32; 3] {
    [value.x.max(0.01), value.y.max(0.01), value.z.max(0.01)]
}

/// 底稿里的贴花列表；贴花在场景树里显示成普通物体，这里汇总并可直接改尺寸/旋转。
fn decals_section(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc) {
    let Some(decals) = doc.base_doc().map(|base| {
        base.decals
            .iter()
            .enumerate()
            .map(|(i, d)| (i, d.clone()))
            .collect::<Vec<_>>()
    }) else {
        return;
    };
    if decals.is_empty() {
        return;
    }
    ui.separator();
    ui.label(RichText::new(format!("贴花（{} 个）", decals.len())));
    for (i, decal) in decals {
        let mut position = decal.position.unwrap_or([0.0, 0.0, 0.0]);
        let mut normal = decal.normal.unwrap_or([0.0, 1.0, 0.0]);
        let mut size = decal.size.unwrap_or([1.0, 1.0]);
        let mut rotation = decal.rotation.unwrap_or(0.0);
        let mut color = decal.color.unwrap_or([1.0, 1.0, 1.0, 1.0]);
        let mut changed = false;
        ui.label(
            RichText::new(format!(
                "#{i} · {} · {:.0}°",
                decal.texture.as_deref().unwrap_or("纯色"),
                rotation
            ))
            .small(),
        );
        ui.horizontal(|ui| {
            ui.label("位置");
            for value in &mut position {
                changed |= ui.add(egui::DragValue::new(value).speed(0.05)).changed();
            }
        });
        ui.horizontal(|ui| {
            ui.label("法线");
            for value in &mut normal {
                changed |= ui.add(egui::DragValue::new(value).speed(0.05)).changed();
            }
        });
        ui.horizontal(|ui| {
            ui.label("尺寸");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut size[0])
                        .range(0.01..=100.0)
                        .speed(0.05),
                )
                .changed();
            changed |= ui
                .add(
                    egui::DragValue::new(&mut size[1])
                        .range(0.01..=100.0)
                        .speed(0.05),
                )
                .changed();
            ui.label("旋转");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut rotation)
                        .range(-360.0..=360.0)
                        .speed(1.0)
                        .suffix("°"),
                )
                .changed();
            ui.label("颜色");
            changed |= ui.color_edit_button_rgba_unmultiplied(&mut color).changed();
        });
        let mut texture = decal.texture.clone().unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label("贴图");
            ui.text_edit_singleline(&mut texture);
        });
        #[cfg(not(target_arch = "wasm32"))]
        {
            if ui
                .add_enabled(
                    !texture.trim().is_empty(),
                    egui::Button::new("应用贴图（从磁盘读取）"),
                )
                .clicked()
            {
                match replace_decal_texture(engine, doc, i, &texture) {
                    Ok(()) => doc.status = format!("贴花贴图已应用：{}", texture.trim()),
                    Err(e) => doc.status = e,
                }
            }
            if ui
                .add_enabled(decal.texture.is_some(), egui::Button::new("清除贴图"))
                .clicked()
            {
                match replace_decal_texture(engine, doc, i, "") {
                    Ok(()) => doc.status = "贴花贴图已清除".into(),
                    Err(e) => doc.status = e,
                }
            }
        }
        if !changed {
            continue;
        }
        let size = [size[0].max(0.01), size[1].max(0.01)];
        let rotation = rotation.clamp(-360.0, 360.0);
        color = color.map(|c| c.clamp(0.0, 1.0));
        if let Some(base) = doc.base_doc_mut() {
            if let Some(decal) = base.decals.get_mut(i) {
                decal.position = Some(position);
                decal.normal = Some(normal);
                decal.size = Some(size);
                decal.rotation = Some(rotation);
                decal.color = Some(color);
            }
        }
        let name = format!("贴花{i}");
        if let Some(index) = engine.scene.objects.iter().position(|o| o.name == name) {
            let mut normal = Vec3::from_array(normal);
            if normal.length_squared() <= 1e-12 {
                normal = Vec3::Y;
            }
            let normal = normal.normalize_or_zero();
            let position = Vec3::from_array(position);
            let roll = rotation.to_radians();
            let quat =
                Quat::from_rotation_arc(Vec3::Z, normal) * Quat::from_axis_angle(Vec3::Z, roll);
            engine.scene.objects[index].transform = Transform::from_trs(
                position + normal * 0.01,
                quat,
                Vec3::new(size[0], size[1], 1.0),
            );
            engine.scene.objects[index].color = Vec4::from_array(color);
        }
        doc.record_edit(engine);
    }
}

fn inspector(ui: &mut Ui, engine: &mut Engine, doc: &mut Doc, req: &mut Requests) {
    let width = doc
        .layout
        .inspector_width
        .clamp(crate::layout::INSPECTOR_MIN, crate::layout::INSPECTOR_MAX);
    let response = egui::Panel::right("quench3d-inspector")
        .default_size(width)
        .min_size(width)
        .max_size(width)
        .show(ui, |ui| {
            ui.heading("属性");
            ui.separator();
            light_section(ui, engine, doc);
            models_section(ui, engine, doc);
            animations_section(ui, engine, doc);
            particles_section(ui, engine, doc);
            decals_section(ui, engine, doc);
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
    req.inspector_width = response.response.rect.width();
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
            #[cfg(not(target_arch = "wasm32"))]
            if let Some(texture) = doc.pip_texture.as_ref() {
                let width = (rect.width() * 0.28).clamp(160.0, 320.0);
                let height = width * 9.0 / 16.0;
                let size = egui::vec2(width, height);
                let margin = egui::vec2(12.0, 12.0);
                let min = match doc.pip_corner {
                    1 => egui::pos2(rect.left() + margin.x, rect.bottom() - size.y - margin.y),
                    2 => egui::pos2(rect.right() - size.x - margin.x, rect.top() + margin.y),
                    3 => egui::pos2(rect.left() + margin.x, rect.top() + margin.y),
                    _ => egui::pos2(rect.right() - size.x - margin.x, rect.bottom() - size.y - margin.y),
                };
                let pip_rect = egui::Rect::from_min_size(min, size);
                painter.image(
                    texture.id(),
                    pip_rect,
                    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                painter.rect_stroke(
                    pip_rect,
                    2.0,
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 160, 255)),
                    egui::StrokeKind::Inside,
                );
                if let Some(name) = doc.pip_camera.as_deref() {
                    painter.text(
                        pip_rect.left_top() + egui::vec2(6.0, 4.0),
                        egui::Align2::LEFT_TOP,
                        name,
                        egui::FontId::proportional(12.0),
                        egui::Color32::from_rgb(255, 255, 255),
                    );
                }
            }
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
            ui.label("画中画：相机面板选第二台命名相机，并可选右下/左下/右上/左上（桌面端约 5 Hz 预览）");
            ui.label("R：聚焦到整个场景");
            ui.add_space(6.0);
            ui.label(RichText::new("编辑").strong());
            ui.label("左键点击：选中物体；Shift/Ctrl+左键拖动：框选");
            ui.label("Ctrl+C / Ctrl+V：复制 / 粘贴（编辑器内部剪贴板）");
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

#[cfg(test)]
mod timeline_tests {
    use super::*;

    #[test]
    fn particle_json_names_match_scene_schema() {
        use quench3d::scene::{ParticleAlignment, ParticleShape};
        assert_eq!(particle_shape_json(ParticleShape::Sphere), "sphere");
        assert_eq!(particle_shape_json(ParticleShape::Box), "box");
        assert_eq!(particle_shape_json(ParticleShape::Disc), "disc");
        assert_eq!(particle_shape_json(ParticleShape::Cone), "cone");
        assert_eq!(
            particle_alignment_json(ParticleAlignment::Billboard),
            "billboard"
        );
        assert_eq!(
            particle_alignment_json(ParticleAlignment::Velocity),
            "velocity"
        );
        assert_eq!(particle_shape_name(ParticleShape::Cone), "锥");
        assert_eq!(particle_alignment_name(ParticleAlignment::Velocity), "速度");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn picker_result_updates_path_and_request() {
        let mut doc = Doc::new();
        let mut req = Requests::default();
        apply_pick(
            &mut doc,
            &mut req,
            crate::picker::PickResult {
                mode: crate::picker::PickMode::Open,
                path: std::path::PathBuf::from("/tmp/a.json"),
            },
        );
        assert_eq!(doc.file_path, "/tmp/a.json");
        assert!(req.load && !req.save);
        assert!(doc.picker.is_none());

        let mut req = Requests::default();
        apply_pick(
            &mut doc,
            &mut req,
            crate::picker::PickResult {
                mode: crate::picker::PickMode::Save,
                path: std::path::PathBuf::from("/tmp/b.json"),
            },
        );
        assert_eq!(doc.file_path, "/tmp/b.json");
        assert!(req.save && !req.load);

        doc.picker_action = crate::picker::PickAction::ExportGltf;
        let mut req = Requests::default();
        apply_pick(
            &mut doc,
            &mut req,
            crate::picker::PickResult {
                mode: crate::picker::PickMode::Save,
                path: std::path::PathBuf::from("/tmp/c.gltf"),
            },
        );
        assert_eq!(doc.file_path, "/tmp/b.json", "导出不能改场景路径");
        assert_eq!(
            doc.export_path.as_deref(),
            Some(std::path::Path::new("/tmp/c.gltf"))
        );
        assert!(req.export_gltf && !req.export_glb && !req.save && !req.load);
    }

    #[test]
    fn timeline_scrub_maps_and_clamps() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(100.0, 20.0));
        assert_eq!(timeline_time_at(rect, 0.0, 2.0), 0.0);
        assert_eq!(timeline_time_at(rect, 50.0, 2.0), 1.0);
        assert_eq!(timeline_time_at(rect, 100.0, 2.0), 2.0);
        assert_eq!(timeline_time_at(rect, -30.0, 2.0), 0.0);
        assert_eq!(timeline_time_at(rect, 230.0, 2.0), 2.0);
        assert_eq!(timeline_time_at(rect, 50.0, 0.0), 0.0);
        assert_eq!(timeline_time_at(rect, 50.0, f32::NAN), 0.0);
    }
}
