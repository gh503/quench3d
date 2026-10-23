//! 视口里的变换 gizmo：直接拖着物体改位置 / 朝向 / 缩放。
//!
//! 这是"模型查看器"和"编辑器"之间那条线：没有它，所有变换都得在右侧面板里敲数字。
//!
//! ## 设计取舍（每条都对应一个具体会踩的坑）
//!
//! * **屏幕空间定长**：手柄长度固定 88 px，世界长度由"投影一个单位轴到屏幕"反推
//!   （`Screen::project`）。于是贴脸和几公里外手柄都是同一个大小，
//!   不存在"近处糊满屏、远处点不到"。命中判定也在屏幕空间（10 px 容差），
//!   不是拿细箭头做几何求交。
//! * **命中优先于拾取**：视口里先问 gizmo 再问物体（见 `crate::ui::viewport`）。
//!   被别的几何挡住的轴照样能抓——它是覆盖层，不参与深度。
//! * **拖拽参照物在按下时定死**：单轴用"屏幕方向上的 1 维投影"（最稳，不会沿视线滑走）；
//!   平面手柄用按下时两条轴的屏幕方向解 2×2 线性系统。全程不再重算，
//!   所以拖出屏幕外也不会漂。
//! * **每帧都从"按下时的变换"重算**，不是逐帧累加——避免长拖拽的浮点漂移。
//! * **吸附作用在"本次拖拽的累计增量"上**，不是每帧增量，否则会抖成绝对坐标。
//!   Ctrl = 吸附（平移 0.25 / 旋转 15° / 缩放 0.1），Shift = 精细（×0.1），两者可叠加。
//! * **一次拖拽 = 一步历史**：按下 `begin_transaction`，抬起 `end_transaction`；
//!   Esc 中途取消会还原到按下前的变换且不留下历史。
//! * **旋转手柄画的是真实投影椭圆，命中判定也用同一条折线**：所见即所抓。
//!   只画屏幕圆会让 Local 模式下"看到的圈"和"抓到的圈"不一致。
//! * **模型成员整组联动**：导入的 glTF/OBJ 在引擎里是"一个子网格一个物体"，
//!   拖其中任何一个 = 这一组一起动，并把新变换写回场景文件的 `models[i]`
//!   （物体↔模型的映射由 `scene_io` 在装载时建立，见 `runner::ModelInstance`）；
//!   缩放手柄对模型成员强制走等比 —— 模型格式里只有一个均匀缩放值。
//!
//! ## 没做的（老实说）
//!
//! * 拖拽中键入数值（Blender 的 `G X 2.5 Enter`）——需要一层文本输入浮层，这版没有。
//! * 3D 光标 / 多点平均轴心——现在提供物体原点、选中中心、世界原点三种。
//! * 屏幕空间旋转（轨迹球）、**多选**（框选 / Shift 加选）成组变换——多选还没做；
//!   现在唯一的"成组"来源是模型实例（见上）。
//! * 手柄是 egui 的 2D 图元，不写深度：被物体完全挡住的轴也看得见（刻意如此）。

use crate::doc::Doc;
use egui::{Color32, Painter, Pos2, Rect, Stroke, Ui, Vec2 as EVec2};
use quench3d::prelude::*;

/// 手柄可抓半径（屏幕像素）。
pub const PICK_RADIUS_PX: f32 = 10.0;
/// 手柄在屏幕上的长度（像素）。
pub const HANDLE_LEN_PX: f32 = 88.0;
/// 平移吸附步长（世界单位）。
pub const SNAP_TRANSLATE: f32 = 0.25;
/// 旋转吸附步长（度）。
pub const SNAP_ROTATE_DEG: f32 = 15.0;
/// 缩放吸附步长。
pub const SNAP_SCALE: f32 = 0.1;
/// 按住 Shift 时的精细倍率。
pub const FINE_FACTOR: f32 = 0.1;

/// gizmo 的三种模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// 平移。
    Translate,
    /// 旋转。
    Rotate,
    /// 缩放。
    Scale,
}

impl Mode {
    /// 全部模式（工具栏按这个顺序排）。
    pub const ALL: [Mode; 3] = [Mode::Translate, Mode::Rotate, Mode::Scale];

    /// 界面标签。
    pub fn label(self) -> &'static str {
        match self {
            Mode::Translate => "移动",
            Mode::Rotate => "旋转",
            Mode::Scale => "缩放",
        }
    }

    /// 快捷键提示。
    pub fn key_hint(self) -> &'static str {
        match self {
            Mode::Translate => "W",
            Mode::Rotate => "E",
            Mode::Scale => "R",
        }
    }
}

/// 坐标系。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Space {
    /// 世界轴。
    World,
    /// 物体自己的轴（跟着朝向转）。
    Local,
}

impl Space {
    /// 界面标签。
    pub fn label(self) -> &'static str {
        match self {
            Space::World => "世界",
            Space::Local => "局部",
        }
    }
}

/// 旋转/缩放的轴心。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pivot {
    /// 活动物体自己的原点。
    Object,
    /// 所有选中物体的包围盒中心（多选时的默认）。
    #[default]
    SelectionCenter,
    /// 世界原点。
    WorldOrigin,
    /// 可放置的 3D 光标（`K` 放到选中中心，或工具栏手输）。
    Cursor,
}

impl Pivot {
    /// 全部轴心（工具栏按这个顺序排）。
    pub const ALL: [Pivot; 4] = [
        Pivot::Object,
        Pivot::SelectionCenter,
        Pivot::WorldOrigin,
        Pivot::Cursor,
    ];

    /// 界面标签。
    pub fn label(self) -> &'static str {
        match self {
            Pivot::Object => "物体",
            Pivot::SelectionCenter => "选中中心",
            Pivot::WorldOrigin => "世界原点",
            Pivot::Cursor => "3D 光标",
        }
    }
}

/// 可抓的手柄。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// 第 `axis` 根轴（0=X, 1=Y, 2=Z）的箭头 / 圆环 / 方块。
    Axis(usize),
    /// 平面手柄：`(a, b)` 两条轴组成的平面。
    Plane(usize, usize),
    /// 中心方块（等比缩放）。
    Uniform,
}

/// 正在进行的拖拽。
#[derive(Clone, Debug)]
struct Drag {
    handle: Handle,
    /// 按下时的物体变换（Esc 取消 / 每帧从它重算）。
    start_transform: Transform,
    /// 按下时的指针位置（视口局部坐标）。
    start_pointer: EVec2,
    /// 按下时的轴（世界空间，已按 World/Local 决定）。
    axes: [Vec3; 3],
    /// 每根轴的单位屏幕方向与"每像素多少世界单位"。
    axis_dir: [EVec2; 3],
    world_per_px: [f32; 3],
    /// 平面拖拽的 2×2 列向量（像素 / 世界单位）。
    plane_cols: Option<(EVec2, EVec2)>,
    /// 旋转用：按下时"轴心 → 指针"的屏幕向量。
    start_vector: EVec2,
    /// 旋转用：轴背对相机时取反，保证"往哪边转都跟着手"。
    sign: f32,
    /// 当前状态文本（画在光标旁边）。
    hud: String,
    /// 组变换的轴心（单选的物体位置；多选时是选中物体的中心）。
    pivot: Vec3,
    /// 多选时一起动的附加物体（按下时的世界变换）。
    group: Vec<(usize, Transform)>,
}

/// gizmo 的全部可变状态。
pub struct Gizmo {
    /// 当前模式。
    pub mode: Mode,
    /// 当前坐标系。
    pub space: Space,
    /// 旋转/缩放的轴心。
    pub pivot: Pivot,
    /// 3D 光标位置（`Pivot::Cursor` 用）。
    pub cursor: Vec3,
    /// 总开关（关掉后视口里只剩拾取与转视角）。
    pub enabled: bool,
    /// 当前悬停的手柄。
    pub hover: Option<Handle>,
    /// 正在拖拽。
    drag: Option<Drag>,
    /// 手柄屏幕长度（留作设置项）。
    pub size_px: f32,
}

impl Default for Gizmo {
    fn default() -> Self {
        Self {
            mode: Mode::Translate,
            space: Space::World,
            pivot: Pivot::SelectionCenter,
            cursor: Vec3::ZERO,
            enabled: true,
            hover: None,
            drag: None,
            size_px: HANDLE_LEN_PX,
        }
    }
}

impl Gizmo {
    /// 是否正握着某个手柄（此时要把左键拖动从"转视角"和"拾取"那里抢过来）。
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// 一帧的交互：先更新已有拖拽，再算悬停，最后把手柄画出来。
    ///
    /// 返回 `true` 表示"这一帧的指针属于 gizmo"：调用方不要把它当成转视角或点选。
    pub fn interact(&mut self, ui: &Ui, engine: &mut Engine, doc: &mut Doc, rect: Rect) -> bool {
        let Some(index) = doc.selected.filter(|i| *i < engine.scene.len()) else {
            self.drag = None;
            self.hover = None;
            return false;
        };
        if !self.enabled {
            self.drag = None;
            self.hover = None;
            return false;
        }
        // 父+子同时选中时，变换写在最上层被选中的祖先上；子物体跟着层级走。
        let driver = doc.drag_driver(engine, index);

        let pointer_abs = ui.input(|i| i.pointer.hover_pos());
        // 统一用**视口局部坐标**：`world_to_screen` 给的就是视口坐标，
        // 命中测试与绘制都 +rect.min 变回 egui 绝对坐标。
        let pointer = pointer_abs.map(|p| p - rect.min);
        let down = ui.input(|i| i.pointer.primary_down());
        let pressed = ui.input(|i| i.pointer.primary_pressed());
        let released = ui.input(|i| i.pointer.primary_released());
        let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let ctrl = ui.input(|i| i.modifiers.ctrl || i.modifiers.command);
        let shift = ui.input(|i| i.modifiers.shift);

        // Esc：取消整段拖拽，还原到按下前的变换，且不留历史。
        if escape && self.drag.is_some() {
            let drag = self.drag.take().expect("刚判断过");
            // `start_transform` 是**世界**变换：写回时层级里要换算成局部（有父物体时）。
            engine
                .scene
                .set_world_transform(driver, drag.start_transform);
            // 拖的是模型成员时，整组一起还原（连同写回 `models[i]` 的那一份）。
            doc.apply_transform_to_model_group(engine, driver, drag.start_transform);
            doc.push_transform_to_physics(engine);
            doc.history.abort_transaction();
            doc.status = "已取消拖拽".to_string();
            return true;
        }

        let pivot = self.pivot_position(engine, doc, driver);
        let screen = Screen::project(self, engine, rect, driver, pivot);

        // ---- 1. 已经在拖：每帧从 start_transform 重算 ----
        if self.drag.is_some() {
            let Some(pointer) = pointer else {
                // 指针跑出窗口：先按当前状态画着，等它回来。
                self.paint(ui, engine, doc, rect, &screen, None);
                return true;
            };
            let (group, drag_pivot) = {
                let drag = self.drag.as_ref().expect("正在拖");
                (drag.group.clone(), drag.pivot)
            };
            let (transform, hud) = {
                let drag = self.drag.as_ref().expect("正在拖");
                let delta_px = pointer - drag.start_pointer;
                match self.mode {
                    Mode::Translate => {
                        let offset = translate_offset(drag, delta_px, ctrl, shift);
                        let hud = format!("Δ {:.3}, {:.3}, {:.3}", offset.x, offset.y, offset.z);
                        (
                            Transform::new(
                                drag.start_transform.position + offset,
                                drag.start_transform.rotation,
                                drag.start_transform.scale,
                            ),
                            hud,
                        )
                    }
                    Mode::Rotate => {
                        let current = pointer - screen.center.to_vec2();
                        let mut angle = (drag.start_vector.angle() - current.angle()) * drag.sign;
                        while angle > std::f32::consts::PI {
                            angle -= std::f32::consts::TAU;
                        }
                        while angle <= -std::f32::consts::PI {
                            angle += std::f32::consts::TAU;
                        }
                        if shift {
                            angle *= FINE_FACTOR;
                        }
                        if ctrl {
                            let step = SNAP_ROTATE_DEG.to_radians();
                            angle = (angle / step).round() * step;
                        }
                        let axis = drag.axes[axis_of(drag.handle)];
                        let q = Quat::from_axis_angle(axis, angle);
                        let rotation = match self.space {
                            // 世界轴：左乘；局部轴：右乘（轴本身已经跟着物体转了）。
                            Space::World => q * drag.start_transform.rotation,
                            Space::Local => drag.start_transform.rotation * q,
                        };
                        let hud = format!("{:.2}°", angle.to_degrees());
                        (
                            Transform::new(
                                drag.start_transform.position,
                                rotation,
                                drag.start_transform.scale,
                            ),
                            hud,
                        )
                    }
                    Mode::Scale => {
                        let i = axis_of(drag.handle);
                        let mut factor =
                            1.0 + delta_px.dot(drag.axis_dir[i]) / (self.size_px * 0.9).max(1.0);
                        if shift {
                            factor = 1.0 + (factor - 1.0) * FINE_FACTOR;
                        }
                        if ctrl {
                            factor = (factor / SNAP_SCALE).round() * SNAP_SCALE;
                        }
                        factor = factor.max(0.02);
                        let mut scale = drag.start_transform.scale;
                        match drag.handle {
                            Handle::Uniform => scale *= factor,
                            _ => match i {
                                0 => scale.x = (scale.x * factor).max(0.02),
                                1 => scale.y = (scale.y * factor).max(0.02),
                                _ => scale.z = (scale.z * factor).max(0.02),
                            },
                        }
                        let hud = format!("×{factor:.3}");
                        (
                            Transform::new(
                                drag.start_transform.position,
                                drag.start_transform.rotation,
                                scale,
                            ),
                            hud,
                        )
                    }
                }
            };
            // 多选时主物体也要绕组中心转/缩放，否则"整组"只有其它人在动。
            // 注意附加物体仍用未绕心的 `transform` 算增量，避免把主物体的绕心位移
            // 又当成平移加给它们（那样会平移两次）。
            let primary_transform = if group.is_empty() {
                transform
            } else {
                let start = self
                    .drag
                    .as_ref()
                    .map(|d| d.start_transform)
                    .unwrap_or(transform);
                group_member_transform(start, start, transform, drag_pivot)
            };
            // 拖拽算出来的是**世界**变换，写回时按层级换算成局部（没有父物体时就是它自己）。
            // 有父物体时这是必须的：把世界位移直接加到局部位置上，父物体一转就会走错方向。
            engine.scene.set_world_transform(driver, primary_transform);
            // 模型（glTF/OBJ 引用）展开出来的物体：**整组一起动**，并把新变换写回
            // `models[i]` —— 这就是"视口里实时拖动模型"，存盘后位置也保得住。
            // 普通物体这一步什么都不做（返回 false）。
            doc.apply_transform_to_model_group(engine, driver, primary_transform);
            // 多选：把同一个"主物体的变化量"应用到其它选中物体，绕组中心旋转/缩放。
            for (member, start) in &group {
                let member_transform = group_member_transform(
                    *start,
                    self.drag
                        .as_ref()
                        .map(|d| d.start_transform)
                        .unwrap_or(transform),
                    transform,
                    drag_pivot,
                );
                engine.scene.set_world_transform(*member, member_transform);
                doc.apply_transform_to_model_group(engine, *member, member_transform);
                freeze_body(engine, *member);
            }
            doc.push_transform_to_physics(engine);
            freeze_body(engine, driver);
            doc.record_edit(engine); // 事务中 → 只标脏，抬起时合成一步
            self.paint(ui, engine, doc, rect, &screen, Some(hud));

            if released || !down {
                let drag = self.drag.take().expect("正在拖");
                doc.end_history_transaction(engine);
                doc.status = format!("{}完成（{}）", self.mode.label(), drag.hud);
            }
            return true;
        }

        // ---- 2. 没在拖：算悬停；按在把手上就开拖 ----
        self.hover = match pointer {
            Some(p) if rect.contains(rect.min + p) => screen.hit(self, p, PICK_RADIUS_PX),
            _ => None,
        };

        let Some(pointer) = pointer else {
            self.paint(ui, engine, doc, rect, &screen, None);
            return false;
        };
        if pressed {
            let mut handle = self.hover;
            // 模型只有**均匀**缩放（场景格式里 `models[i].scale` 是一个数）：
            // 拖模型成员的单轴缩放块时强制走等比手柄，免得拖出格式表达不了的缩放。
            if self.mode == Mode::Scale
                && doc.model_of_object(index).is_some()
                && matches!(handle, Some(Handle::Axis(_)))
            {
                handle = Some(Handle::Uniform);
            }
            if let Some(handle) = handle {
                self.drag = Some(self.begin_drag(engine, doc, driver, &screen, handle, pointer));
                doc.begin_history_transaction();
                doc.status = format!("开始{}…", self.mode.label());
                self.paint(ui, engine, doc, rect, &screen, None);
                return true;
            }
        }

        self.paint(ui, engine, doc, rect, &screen, None);
        self.hover.is_some() && rect.contains(rect.min + pointer)
    }

    /// 按当前轴心设置求旋转/缩放中心。
    fn pivot_position(&self, engine: &Engine, doc: &Doc, driver: usize) -> Vec3 {
        let object = engine.scene.world_transform(driver).position;
        pivot_point(
            self.pivot,
            object,
            doc.selection_center(engine),
            self.cursor,
        )
    }

    /// 按下时把"这次拖拽"的全部参照物固定下来。
    fn begin_drag(
        &self,
        engine: &Engine,
        doc: &Doc,
        index: usize,
        screen: &Screen,
        handle: Handle,
        pointer: EVec2,
    ) -> Drag {
        // 拖拽的参照是物体的**世界**变换（有父物体时 `transform` 是局部的）。
        let start_transform = engine.scene.world_transform(index);
        // 旋转方向：轴指向相机时，屏幕上的顺时针才是绕轴正方向。
        let i = axis_of(handle);
        let sign = if screen.axes[i].normalize().dot(screen.forward) > 0.0 {
            -1.0
        } else {
            1.0
        };
        // 平面拖拽的 2×2 矩阵：列 = 每条轴"每个世界单位在屏幕上走多少像素"。
        let plane_cols = match handle {
            Handle::Plane(a, b) => {
                let col = |i: usize| screen.axis_dir[i] / screen.world_per_px[i].max(1e-6);
                Some((col(a), col(b)))
            }
            _ => None,
        };
        Drag {
            handle,
            start_transform,
            start_pointer: pointer,
            axes: screen.axes,
            axis_dir: screen.axis_dir,
            world_per_px: screen.world_per_px,
            plane_cols,
            start_vector: pointer - screen.center.to_vec2(),
            sign,
            hud: String::new(),
            pivot: screen.pivot,
            group: doc.group_selection(engine),
        }
    }

    /// 画手柄 + 拖拽 HUD。
    fn paint(
        &self,
        ui: &Ui,
        engine: &Engine,
        doc: &Doc,
        rect: Rect,
        screen: &Screen,
        hud: Option<String>,
    ) {
        let painter = ui.painter_at(rect);
        let offset = rect.min.to_vec2();
        let active = self.drag.as_ref().map(|d| d.handle).or(self.hover);
        let axis_hi = |i: usize| active == Some(Handle::Axis(i));
        let name = doc
            .selected_object(engine)
            .map(|o| o.name.clone())
            .unwrap_or_else(|| "选中物体".to_string());

        match self.mode {
            Mode::Translate => {
                for i in 0..3 {
                    if !screen.visible[i] {
                        continue;
                    }
                    let color = axis_color(i, axis_hi(i));
                    let tip = screen.tips[i] + offset;
                    let center = screen.center + offset;
                    let start = center + (tip - center) * 0.12;
                    painter.line_segment([start, tip], Stroke::new(2.5, color));
                    arrow_head(&painter, center, tip, color);
                }
                for (a, b) in [(0usize, 1usize), (1, 2), (2, 0)] {
                    let Some(c) = screen
                        .plane_centers
                        .iter()
                        .find(|(key, _)| *key == (a, b))
                        .map(|(_, pos)| *pos)
                    else {
                        continue;
                    };
                    let color = axis_color(
                        if a == 0 { 2 } else { 0 },
                        active == Some(Handle::Plane(a, b)),
                    );
                    let c = c + offset;
                    let d = 9.0;
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            c + EVec2::new(-d, -d),
                            c + EVec2::new(d, -d),
                            c + EVec2::new(d, d),
                            c + EVec2::new(-d, d),
                        ],
                        color.gamma_multiply(0.35),
                        Stroke::new(1.0, color),
                    ));
                }
            }
            Mode::Rotate => {
                for i in 0..3 {
                    if screen.circles[i].len() < 2 {
                        continue;
                    }
                    let color = axis_color(i, axis_hi(i));
                    let pts: Vec<Pos2> = screen.circles[i].iter().map(|p| *p + offset).collect();
                    painter.add(egui::Shape::line(pts, Stroke::new(2.5, color)));
                }
            }
            Mode::Scale => {
                for i in 0..3 {
                    if !screen.visible[i] {
                        continue;
                    }
                    let color = axis_color(i, axis_hi(i));
                    painter.line_segment(
                        [screen.center + offset, screen.tips[i] + offset],
                        Stroke::new(2.5, color),
                    );
                    painter.rect_filled(
                        egui::Rect::from_center_size(screen.tips[i] + offset, EVec2::splat(10.0)),
                        1.0,
                        color,
                    );
                }
                let color = axis_color(1, active == Some(Handle::Uniform));
                painter.rect_filled(
                    egui::Rect::from_center_size(screen.center + offset, EVec2::splat(12.0)),
                    2.0,
                    color.gamma_multiply(0.85),
                );
            }
        }

        // 名字标在轴心上方：选错/看不清时一眼知道在拖谁。
        painter.text(
            screen.center + offset + EVec2::new(0.0, -14.0),
            egui::Align2::CENTER_BOTTOM,
            name,
            egui::FontId::proportional(11.0),
            Color32::from_rgba_unmultiplied(230, 230, 230, 200),
        );

        if let Some(hud) = hud {
            let at = ui
                .input(|i| i.pointer.hover_pos())
                .unwrap_or(screen.center + offset)
                + EVec2::new(16.0, 16.0);
            painter.text(
                at,
                egui::Align2::LEFT_TOP,
                hud,
                egui::FontId::monospace(12.0),
                Color32::from_rgb(255, 226, 138),
            );
        }
    }
}

/// 轴的颜色（X 红 / Y 绿 / Z 蓝），高亮时提亮。
fn axis_color(index: usize, highlight: bool) -> Color32 {
    let base = match index {
        0 => Color32::from_rgb(226, 84, 92),
        1 => Color32::from_rgb(104, 200, 104),
        _ => Color32::from_rgb(86, 140, 240),
    };
    if highlight {
        base.gamma_multiply(1.6)
    } else {
        base
    }
}

/// 画箭头（两条短线）。
fn arrow_head(painter: &Painter, from: Pos2, tip: Pos2, color: Color32) {
    let dir = (tip - from).normalized();
    let normal = EVec2::new(-dir.y, dir.x);
    let len = 12.0;
    let wide = 5.0;
    painter.line_segment(
        [tip, tip - dir * len + normal * wide],
        Stroke::new(2.5, color),
    );
    painter.line_segment(
        [tip, tip - dir * len - normal * wide],
        Stroke::new(2.5, color),
    );
}

/// 手柄对应的轴下标（平面 / 等比缩放没有单一轴，取 1 作中性值）。
fn axis_of(handle: Handle) -> usize {
    match handle {
        Handle::Axis(i) => i,
        Handle::Plane(a, _) => a,
        Handle::Uniform => 1,
    }
}

/// 拖拽中把刚体的速度清零，免得它在手里继续被物理带着跑。
fn freeze_body(engine: &mut Engine, index: usize) {
    let Some(handle) = engine.scene.objects[index].body else {
        return;
    };
    if let Some(body) = engine.world.body_mut(handle) {
        body.wake_up();
        body.set_linear_velocity(Vec3::ZERO);
        body.set_angular_velocity(Vec3::ZERO);
    }
}

/// 单轴 / 平面平移的偏移量（世界空间）。
fn translate_offset(drag: &Drag, delta_px: EVec2, ctrl: bool, shift: bool) -> Vec3 {
    match drag.handle {
        Handle::Plane(a, b) => {
            let (col_a, col_b) = drag.plane_cols.unwrap_or((EVec2::X, EVec2::Y));
            let det = col_a.x * col_b.y - col_a.y * col_b.x;
            let (mut da, mut db) = if det.abs() > 1e-4 {
                (
                    (delta_px.x * col_b.y - delta_px.y * col_b.x) / det,
                    (col_a.x * delta_px.y - col_a.y * delta_px.x) / det,
                )
            } else {
                // 两条轴在屏幕上几乎重合：退化成各自的一维投影（总比乱动好）。
                (
                    delta_px.dot(drag.axis_dir[a]) * drag.world_per_px[a],
                    delta_px.dot(drag.axis_dir[b]) * drag.world_per_px[b],
                )
            };
            if shift {
                da *= FINE_FACTOR;
                db *= FINE_FACTOR;
            }
            if ctrl {
                da = (da / SNAP_TRANSLATE).round() * SNAP_TRANSLATE;
                db = (db / SNAP_TRANSLATE).round() * SNAP_TRANSLATE;
            }
            drag.axes[a] * da + drag.axes[b] * db
        }
        handle => {
            let i = axis_of(handle);
            let d = axis_translation(
                delta_px,
                drag.axis_dir[i],
                drag.world_per_px[i],
                ctrl,
                shift,
            );
            drag.axes[i] * d
        }
    }
}

/// 一帧的屏幕投影缓存（全部是**视口局部坐标**）。
struct Screen {
    /// 轴心在屏幕上的位置。
    center: Pos2,
    /// 轴心的世界坐标（组变换绕它旋转/缩放）。
    pivot: Vec3,
    /// 三根轴（世界空间）。
    axes: [Vec3; 3],
    /// 三根轴的箭头 / 方块屏幕位置。
    tips: [Pos2; 3],
    /// 三根轴的单位屏幕方向与"像素 → 世界单位"比例。
    axis_dir: [EVec2; 3],
    world_per_px: [f32; 3],
    /// 该轴是否投影成功（在相机前方）。
    visible: [bool; 3],
    /// 平面手柄的屏幕中心。
    plane_centers: Vec<((usize, usize), Pos2)>,
    /// 旋转圆的投影折线（画图与命中都用它）。
    circles: [Vec<Pos2>; 3],
    /// 相机朝向（定旋转符号用）。
    forward: Vec3,
}

impl Screen {
    /// 把轴心、三根轴、旋转圆全部投影到屏幕。
    fn project(gizmo: &Gizmo, engine: &Engine, rect: Rect, index: usize, pivot: Vec3) -> Screen {
        // 手柄的轴方向仍取主选物体的世界朝向（多选时不强行统一各物体的旋转）。
        let world = engine.scene.world_transform(index);
        let q = world.rotation;
        let camera = engine
            .orbit
            .camera(rect.width().max(1.0) / rect.height().max(1.0));
        // 注意：这里的 `Vec2` 是引擎（glam）的，egui 的别名成 `EVec2`。
        // `world_to_screen` 收的是引擎 Vec2，返回的也是引擎 Vec2。
        let vp = Vec2::new(rect.width().max(1.0), rect.height().max(1.0));
        let to_screen = |world: Vec3| camera.world_to_screen(world, vp);

        let axes = match gizmo.space {
            Space::World => [Vec3::X, Vec3::Y, Vec3::Z],
            Space::Local => [q * Vec3::X, q * Vec3::Y, q * Vec3::Z],
        };

        let center_v = to_screen(pivot).unwrap_or(vp * 0.5);
        let center = Pos2::new(center_v.x, center_v.y);

        let mut tips = [center; 3];
        let mut axis_dir = [EVec2::X; 3];
        let mut world_per_px = [1.0; 3];
        let mut visible = [false; 3];

        for i in 0..3 {
            let axis = axes[i].normalize();
            // 先量"一个世界单位在屏幕上多长"，再反推 88 px 对应多少世界长度。
            let Some(unit_px) = to_screen(pivot + axis) else {
                continue;
            };
            let px_per_unit = (unit_px - center_v).length();
            if !px_per_unit.is_finite() || px_per_unit < 1e-3 {
                continue;
            }
            let world_len = gizmo.size_px / px_per_unit;
            let Some(tip_v) = to_screen(pivot + axis * world_len) else {
                continue;
            };
            let dir = tip_v - center_v;
            if dir.length() < 1e-3 {
                continue;
            }
            tips[i] = Pos2::new(tip_v.x, tip_v.y);
            axis_dir[i] = EVec2::new(dir.x, dir.y).normalized();
            world_per_px[i] = world_len / gizmo.size_px;
            visible[i] = true;
        }

        let mut plane_centers = Vec::new();
        for (a, b) in [(0usize, 1usize), (1, 2), (2, 0)] {
            if !(visible[a] && visible[b]) {
                continue;
            }
            let world_len = gizmo.size_px * world_per_px[a];
            if let Some(pos) = to_screen(pivot + (axes[a] + axes[b]) * world_len * 0.3) {
                plane_centers.push(((a, b), Pos2::new(pos.x, pos.y)));
            }
        }

        // 旋转圆：把"垂直于轴、半径 = 手柄长度"的真实圆投影出来，48 段。
        let mut circles: [Vec<Pos2>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for (i, slot) in circles.iter_mut().enumerate() {
            if !visible[i] {
                continue;
            }
            let axis = axes[i].normalize();
            let (u, v) = orthonormal_basis(axis);
            let radius = gizmo.size_px * world_per_px[i];
            let mut pts = Vec::with_capacity(49);
            for k in 0..=48 {
                let t = k as f32 / 48.0 * std::f32::consts::TAU;
                match to_screen(pivot + (u * t.cos() + v * t.sin()) * radius) {
                    Some(p) => pts.push(Pos2::new(p.x, p.y)),
                    None => {
                        pts.clear();
                        break;
                    }
                }
            }
            *slot = pts;
        }

        Screen {
            center,
            pivot,
            axes,
            tips,
            axis_dir,
            world_per_px,
            visible,
            plane_centers,
            circles,
            forward: camera.forward(),
        }
    }

    /// 命中测试：返回离指针最近的手柄（`pointer` 是视口局部坐标）。
    fn hit(&self, gizmo: &Gizmo, pointer: EVec2, radius: f32) -> Option<Handle> {
        let mut best: Option<(f32, Handle)> = None;
        {
            let consider = |dist: f32, handle: Handle, best: &mut Option<(f32, Handle)>| {
                if dist <= radius && best.map(|(d, _)| dist < d).unwrap_or(true) {
                    *best = Some((dist, handle));
                }
            };
            match gizmo.mode {
                Mode::Translate => {
                    for i in 0..3 {
                        if !self.visible[i] {
                            continue;
                        }
                        consider(
                            distance_to_segment(pointer, self.center, self.tips[i]),
                            Handle::Axis(i),
                            &mut best,
                        );
                    }
                    for ((a, b), c) in &self.plane_centers {
                        consider(
                            (pointer - c.to_vec2()).length(),
                            Handle::Plane(*a, *b),
                            &mut best,
                        );
                    }
                }
                Mode::Rotate => {
                    for i in 0..3 {
                        if self.circles[i].len() < 2 {
                            continue;
                        }
                        consider(
                            distance_to_polyline(pointer, &self.circles[i]),
                            Handle::Axis(i),
                            &mut best,
                        );
                    }
                }
                Mode::Scale => {
                    for i in 0..3 {
                        if !self.visible[i] {
                            continue;
                        }
                        consider(
                            distance_to_segment(pointer, self.center, self.tips[i]),
                            Handle::Axis(i),
                            &mut best,
                        );
                    }
                    consider(
                        (pointer - self.center.to_vec2()).length(),
                        Handle::Uniform,
                        &mut best,
                    );
                }
            }
        }
        best.map(|(_, h)| h)
    }
}

/// 点到线段的距离（屏幕空间）。
fn distance_to_segment(p: EVec2, a: Pos2, b: Pos2) -> f32 {
    let a = a.to_vec2();
    let b = b.to_vec2();
    let ab = b - a;
    let len2 = ab.length_sq();
    if len2 < 1e-6 {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

/// 点到折线的最短距离。
fn distance_to_polyline(p: EVec2, pts: &[Pos2]) -> f32 {
    let mut best = f32::INFINITY;
    for pair in pts.windows(2) {
        best = best.min(distance_to_segment(p, pair[0], pair[1]));
    }
    best
}

/// 给一根轴构造两个正交单位向量（画圆用）。
fn orthonormal_basis(axis: Vec3) -> (Vec3, Vec3) {
    let helper = if axis.x.abs() < 0.9 { Vec3::X } else { Vec3::Y };
    let u = axis.cross(helper).normalize();
    (u, axis.cross(u).normalize())
}

/// 一个便于外部单元测试的纯函数：屏幕空间 1 维投影 → 世界增量。
///
/// 抽出来是因为"吸附作用在累计增量上"这条语义最容易写错，
/// 而它又完全不需要 GPU 就能测。
pub fn axis_translation(
    delta_px: EVec2,
    axis_dir: EVec2,
    world_per_px: f32,
    ctrl: bool,
    shift: bool,
) -> f32 {
    let mut d = delta_px.dot(axis_dir) * world_per_px;
    if shift {
        d *= FINE_FACTOR;
    }
    if ctrl {
        d = (d / SNAP_TRANSLATE).round() * SNAP_TRANSLATE;
    }
    d
}

/// 按轴心设置求一个具体的世界坐标点。
fn pivot_point(pivot: Pivot, object: Vec3, center: Option<Vec3>, cursor: Vec3) -> Vec3 {
    match pivot {
        Pivot::Object => object,
        Pivot::SelectionCenter => center.unwrap_or(object),
        Pivot::WorldOrigin => Vec3::ZERO,
        Pivot::Cursor => cursor,
    }
}

/// 把主物体的"本次变化量"套到另一个选中物体上。
///
/// 平移直接加位移；旋转/缩放绕 `pivot`（多选中心）做刚体变换。这样多选时
/// 拖动的是**整组**，而不是各转各的。
fn group_member_transform(
    start: Transform,
    primary_start: Transform,
    primary_now: Transform,
    pivot: Vec3,
) -> Transform {
    let dq = primary_now.rotation * primary_start.rotation.conjugate();
    let ratio = |now: f32, before: f32| {
        if before.abs() > 1e-6 {
            now / before
        } else {
            1.0
        }
    };
    let ds = Vec3::new(
        ratio(primary_now.scale.x, primary_start.scale.x),
        ratio(primary_now.scale.y, primary_start.scale.y),
        ratio(primary_now.scale.z, primary_start.scale.z),
    );
    let scaled = Vec3::new(
        (start.position.x - pivot.x) * ds.x,
        (start.position.y - pivot.y) * ds.y,
        (start.position.z - pivot.z) * ds.z,
    );
    let position = pivot + dq * scaled + (primary_now.position - primary_start.position);
    Transform::new(
        position,
        dq * start.rotation,
        Vec3::new(
            start.scale.x * ds.x,
            start.scale.y * ds.y,
            start.scale.z * ds.z,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个"投影结果已知"的屏幕缓存：轴心在原点，三根轴分别是屏幕右 / 上 / 左。
    /// 这样命中测试完全不依赖 GPU 与相机，纯几何。
    fn fake_screen() -> Screen {
        let center = Pos2::new(100.0, 100.0);
        Screen {
            center,
            pivot: Vec3::ZERO,
            axes: [Vec3::X, Vec3::Y, Vec3::Z],
            tips: [
                Pos2::new(188.0, 100.0),
                Pos2::new(100.0, 12.0),
                Pos2::new(12.0, 100.0),
            ],
            axis_dir: [
                EVec2::new(1.0, 0.0),
                EVec2::new(0.0, -1.0),
                EVec2::new(-1.0, 0.0),
            ],
            world_per_px: [0.01, 0.01, 0.01],
            visible: [true, true, true],
            plane_centers: vec![((0, 1), Pos2::new(126.0, 74.0))],
            circles: [Vec::new(), Vec::new(), Vec::new()],
            forward: -Vec3::Z,
        }
    }

    fn fake_drag(handle: Handle, cols: Option<(EVec2, EVec2)>) -> Drag {
        Drag {
            handle,
            start_transform: Transform::IDENTITY,
            start_pointer: EVec2::ZERO,
            axes: [Vec3::X, Vec3::Y, Vec3::Z],
            axis_dir: [EVec2::X, EVec2::Y, EVec2::new(0.0, 0.0)],
            world_per_px: [1.0, 1.0, 1.0],
            plane_cols: cols,
            start_vector: EVec2::X,
            sign: 1.0,
            hud: String::new(),
            pivot: Vec3::ZERO,
            group: Vec::new(),
        }
    }

    #[test]
    fn hit_test_prefers_the_axis_under_the_pointer() {
        let screen = fake_screen();
        let gizmo = Gizmo::default();
        // 沿着 +X 轴离轴线 3 px：应当抓住 X。
        assert_eq!(
            screen.hit(&gizmo, EVec2::new(140.0, 103.0), PICK_RADIUS_PX),
            Some(Handle::Axis(0))
        );
        // 离任何轴线都超过 10 px：不该抓任何东西。
        assert_eq!(
            screen.hit(&gizmo, EVec2::new(150.0, 150.0), PICK_RADIUS_PX),
            None
        );
        // 平面手柄的方块中心。
        assert_eq!(
            screen.hit(&gizmo, EVec2::new(126.0, 74.0), PICK_RADIUS_PX),
            Some(Handle::Plane(0, 1))
        );
    }

    #[test]
    fn hit_test_ignores_axes_that_are_behind_the_camera() {
        let mut screen = fake_screen();
        screen.visible[0] = false;
        let gizmo = Gizmo::default();
        assert_eq!(
            screen.hit(&gizmo, EVec2::new(140.0, 100.0), PICK_RADIUS_PX),
            None,
            "投影失败的轴不该能被抓住"
        );
    }

    #[test]
    fn rotate_hit_uses_the_projected_ring_not_a_screen_circle() {
        let mut screen = fake_screen();
        // Z 轴的圆：屏幕上是个椭圆（这里用矩形折线代表），只有靠近折线才算命中。
        screen.circles[2] = vec![
            Pos2::new(60.0, 100.0),
            Pos2::new(100.0, 60.0),
            Pos2::new(140.0, 100.0),
            Pos2::new(100.0, 140.0),
            Pos2::new(60.0, 100.0),
        ];
        let gizmo = Gizmo {
            mode: Mode::Rotate,
            ..Default::default()
        };
        assert_eq!(
            screen.hit(&gizmo, EVec2::new(100.0, 63.0), PICK_RADIUS_PX),
            Some(Handle::Axis(2))
        );
        // 圆环内部的中心点（离折线 40 px）不该命中。
        assert_eq!(
            screen.hit(&gizmo, EVec2::new(100.0, 100.0), PICK_RADIUS_PX),
            None
        );
    }

    #[test]
    fn axis_translation_snaps_the_accumulated_delta() {
        // 没按修饰键：跟着走。
        let d = axis_translation(EVec2::new(31.0, 0.0), EVec2::X, 0.01, false, false);
        assert!((d - 0.31).abs() < 1e-5, "实际 {d}");
        // Ctrl：吸附到 0.25 的整数倍（作用在累计量上，不是每帧增量）。
        let d = axis_translation(EVec2::new(31.0, 0.0), EVec2::X, 0.01, true, false);
        assert!((d - 0.25).abs() < 1e-5, "实际 {d}");
        let d = axis_translation(EVec2::new(60.0, 0.0), EVec2::X, 0.01, true, false);
        assert!((d - 0.5).abs() < 1e-5, "实际 {d}");
        // Shift：精细 ×0.1。
        let d = axis_translation(EVec2::new(100.0, 0.0), EVec2::X, 0.01, false, true);
        assert!((d - 0.1).abs() < 1e-5, "实际 {d}");
        // 两者可叠加：先精细再吸附。
        let d = axis_translation(EVec2::new(100.0, 0.0), EVec2::X, 0.01, true, true);
        assert!((d - 0.0).abs() < 1e-5, "实际 {d}");
        // 垂直方向的鼠标位移不产生沿轴位移。
        let d = axis_translation(EVec2::new(0.0, 80.0), EVec2::X, 0.01, false, false);
        assert!(d.abs() < 1e-5, "实际 {d}");
    }

    #[test]
    fn plane_translation_solves_both_axes() {
        let drag = fake_drag(
            Handle::Plane(0, 1),
            Some((EVec2::new(1.0, 0.0), EVec2::new(0.0, 1.0))),
        );
        let offset = translate_offset(&drag, EVec2::new(0.6, -0.3), false, false);
        assert!((offset.x - 0.6).abs() < 1e-5, "实际 {offset:?}");
        assert!((offset.y + 0.3).abs() < 1e-5, "实际 {offset:?}");
        // Ctrl：两条轴各自吸附到 0.25。
        let offset = translate_offset(&drag, EVec2::new(0.6, -0.3), true, false);
        assert!((offset.x - 0.5).abs() < 1e-5, "实际 {offset:?}");
        assert!((offset.y + 0.25).abs() < 1e-5, "实际 {offset:?}");
    }

    #[test]
    fn orthonormal_basis_is_perpendicular() {
        for axis in [
            Vec3::X,
            Vec3::Y,
            Vec3::Z,
            Vec3::new(1.0, 2.0, -3.0).normalize(),
        ] {
            let (u, v) = orthonormal_basis(axis);
            assert!((u.dot(axis)).abs() < 1e-5, "u 该垂直于轴");
            assert!((v.dot(axis)).abs() < 1e-5, "v 该垂直于轴");
            assert!((u.dot(v)).abs() < 1e-5, "u 该垂直于 v");
            assert!((u.length() - 1.0).abs() < 1e-5);
            assert!((v.length() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn distance_to_segment_clamps_to_the_ends() {
        let a = Pos2::new(0.0, 0.0);
        let b = Pos2::new(10.0, 0.0);
        assert!((distance_to_segment(EVec2::new(5.0, 3.0), a, b) - 3.0).abs() < 1e-5);
        // 线段之外：算到端点的距离，而不是到延长线的距离。
        assert!((distance_to_segment(EVec2::new(-4.0, 0.0), a, b) - 4.0).abs() < 1e-5);
        assert!((distance_to_segment(EVec2::new(14.0, 0.0), a, b) - 4.0).abs() < 1e-5);
    }
    #[test]
    fn pivot_labels_cover_all_choices() {
        assert_eq!(Pivot::ALL.len(), 4);
        assert_eq!(Pivot::Object.label(), "物体");
        assert_eq!(Pivot::SelectionCenter.label(), "选中中心");
        assert_eq!(Pivot::WorldOrigin.label(), "世界原点");
        assert_eq!(Pivot::Cursor.label(), "3D 光标");
        assert_eq!(Pivot::default(), Pivot::SelectionCenter);
        let object = Vec3::new(1.0, 2.0, 3.0);
        let center = Vec3::new(10.0, 20.0, 30.0);
        let cursor = Vec3::new(-4.0, 5.0, 6.0);
        assert_eq!(
            pivot_point(Pivot::Cursor, object, Some(center), cursor),
            cursor
        );
        assert_eq!(
            pivot_point(Pivot::Object, object, Some(center), cursor),
            object
        );
        assert_eq!(
            pivot_point(Pivot::SelectionCenter, object, Some(center), cursor),
            center
        );
        assert_eq!(
            pivot_point(Pivot::SelectionCenter, object, None, cursor),
            object,
            "没有多选中心时退回活动物体"
        );
        assert_eq!(
            pivot_point(Pivot::WorldOrigin, object, Some(center), cursor),
            Vec3::ZERO
        );
    }

    #[test]
    fn group_member_follows_translate_rotate_and_scale_around_pivot() {
        let pivot = Vec3::new(0.0, 0.0, 0.0);
        let start = Transform::new(Vec3::new(2.0, 0.0, 0.0), Quat::IDENTITY, Vec3::ONE);
        let primary = Transform::new(Vec3::ZERO, Quat::IDENTITY, Vec3::ONE);

        // 平移 (0,1,0)：成员整体跟着走。
        let moved = Transform::new(Vec3::new(0.0, 1.0, 0.0), Quat::IDENTITY, Vec3::ONE);
        let out = group_member_transform(start, primary, moved, pivot);
        assert!((out.position - Vec3::new(2.0, 1.0, 0.0)).length() < 1e-5);

        // 绕 Y 转 90°：成员从 +X 转到 -Z。
        let turned = Transform::new(
            Vec3::ZERO,
            Quat::from_axis_angle(Vec3::Y, std::f32::consts::FRAC_PI_2),
            Vec3::ONE,
        );
        let out = group_member_transform(start, primary, turned, pivot);
        assert!(
            (out.position - Vec3::new(0.0, 0.0, -2.0)).length() < 1e-5,
            "{:?}",
            out.position
        );

        // 均匀放大 2 倍：成员到轴心的距离也翻倍。
        let scaled = Transform::new(Vec3::ZERO, Quat::IDENTITY, Vec3::splat(2.0));
        let out = group_member_transform(start, primary, scaled, pivot);
        assert!((out.position - Vec3::new(4.0, 0.0, 0.0)).length() < 1e-5);
        assert!((out.scale - Vec3::splat(2.0)).length() < 1e-5);
    }
}
