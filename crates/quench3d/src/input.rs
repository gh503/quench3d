//! 输入状态：把窗口事件归一化成每帧可查询的状态，并生成相机输入。

use std::collections::{HashMap, HashSet};

use quench3d_math::{Vec2, Vec3};
use quench3d_render::CameraInput;
use winit::keyboard::KeyCode;

/// 鼠标按键。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// 左键。
    Left,
    /// 右键。
    Right,
    /// 中键。
    Middle,
}

/// 触屏点的阶段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchPhase {
    /// 手指按下。
    Started,
    /// 手指移动。
    Moved,
    /// 手指抬起/取消。
    Ended,
}

/// 一个触屏点（逻辑像素，左上角为原点）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TouchPoint {
    /// 平台给的触点 id（同一根手指在生命周期内不变）。
    pub id: u64,
    /// 当前位置。
    pub position: Vec2,
    /// 本帧阶段。
    pub phase: TouchPhase,
}

/// 一个手柄的快照（由宿主按平台事件注入；引擎不依赖具体手柄 API）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GamepadState {
    /// 是否连接。
    pub connected: bool,
    /// 左/右摇杆 xy + 扳机（顺序由宿主约定，缺省 6 轴）。
    pub axes: [f32; 6],
    /// 按键位图（宿主约定；引擎只做透传）。
    pub buttons: u32,
}

impl Default for GamepadState {
    fn default() -> Self {
        Self {
            connected: false,
            axes: [0.0; 6],
            buttons: 0,
        }
    }
}

/// 一条动作绑定：键盘键、鼠标键、手柄按键或手柄轴方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ActionBinding {
    /// 键盘键。
    Key(KeyCode),
    /// 鼠标键。
    Mouse(MouseButton),
    /// 手柄按键：`pad` 是 0..4 的槽位，`button` 是位图里的位号（0..32）。
    GamepadButton {
        /// 手柄槽位。
        pad: u8,
        /// 位号。
        button: u8,
    },
    /// 手柄轴方向：轴值超过 `+0.5`（`positive`）或低于 `-0.5` 时算按下。
    GamepadAxis {
        /// 手柄槽位。
        pad: u8,
        /// 轴号。
        axis: u8,
        /// 是否取正方向。
        positive: bool,
    },
}

/// 一帧的输入状态。
#[derive(Clone, Debug, Default)]
pub struct Input {
    keys_down: HashSet<KeyCode>,
    keys_pressed: HashSet<KeyCode>,
    keys_released: HashSet<KeyCode>,
    mouse_down: [bool; 3],
    mouse_pressed: [bool; 3],
    mouse_released: [bool; 3],
    /// 光标位置（逻辑像素）。
    pub mouse_position: Vec2,
    /// 本帧光标位移。
    pub mouse_delta: Vec2,
    /// 本帧滚轮增量。
    pub scroll_delta: f32,
    /// 修饰键。
    pub shift: bool,
    /// 修饰键。
    pub ctrl: bool,
    /// 修饰键。
    pub alt: bool,
    /// 窗口是否聚焦。
    pub focused: bool,
    /// 4 个手柄的当前状态（宿主注入；没有手柄时 `connected = false`）。
    gamepads: [GamepadState; 4],
    /// 本帧的触屏点（`Started/Moved` 保留，`Ended` 只保留这一帧）。
    touches: Vec<TouchPoint>,
    /// 上一帧的触屏点（用来判断“这一帧还按着”）。
    touches_prev: Vec<TouchPoint>,
    /// 上一帧手柄状态（动作的 pressed/released 边界）。
    gamepads_prev: [GamepadState; 4],
    /// 动作名 → 绑定列表。宿主可运行时重绑定。
    bindings: HashMap<String, Vec<ActionBinding>>,
}

impl Input {
    /// 新建。
    pub fn new() -> Self {
        Self {
            focused: true,
            ..Default::default()
        }
    }

    /// 每帧开始时清空瞬时状态。
    pub fn begin_frame(&mut self) {
        self.keys_pressed.clear();
        self.keys_released.clear();
        self.mouse_pressed = [false; 3];
        self.mouse_released = [false; 3];
        self.mouse_delta = Vec2::ZERO;
        self.scroll_delta = 0.0;
        self.touches_prev = std::mem::take(&mut self.touches);
        self.gamepads_prev = self.gamepads;
    }

    /// 按键是否按下。
    #[inline]
    pub fn key_down(&self, key: KeyCode) -> bool {
        self.keys_down.contains(&key)
    }

    /// 按键本帧是否刚按下。
    #[inline]
    pub fn key_pressed(&self, key: KeyCode) -> bool {
        self.keys_pressed.contains(&key)
    }

    /// 按键本帧是否刚抬起。
    #[inline]
    pub fn key_released(&self, key: KeyCode) -> bool {
        self.keys_released.contains(&key)
    }

    /// 鼠标键是否按下。
    #[inline]
    pub fn mouse_button_down(&self, button: MouseButton) -> bool {
        self.mouse_down[button as usize]
    }

    /// 鼠标键本帧是否刚按下。
    #[inline]
    pub fn mouse_button_pressed(&self, button: MouseButton) -> bool {
        self.mouse_pressed[button as usize]
    }

    /// 鼠标键本帧是否刚抬起。
    #[inline]
    pub fn mouse_button_released(&self, button: MouseButton) -> bool {
        self.mouse_released[button as usize]
    }

    /// 任意方向键（WASD / 方向键）构成的移动轴。
    /// 光标位置（逻辑像素，左上角为原点）。
    pub fn mouse_pos(&self) -> Vec2 {
        self.mouse_position
    }

    /// 本帧光标位移。
    pub fn mouse_motion(&self) -> Vec2 {
        self.mouse_delta
    }

    /// 本帧滚轮增量。
    pub fn scroll(&self) -> f32 {
        self.scroll_delta
    }

    /// 由 WASD / 方向键合成的移动轴（已归一化）。
    pub fn movement_axes(&self) -> Vec3 {
        let mut f = 0.0;
        let mut r = 0.0;
        let mut u = 0.0;
        if self.key_down(KeyCode::KeyW) || self.key_down(KeyCode::ArrowUp) {
            f += 1.0;
        }
        if self.key_down(KeyCode::KeyS) || self.key_down(KeyCode::ArrowDown) {
            f -= 1.0;
        }
        if self.key_down(KeyCode::KeyD) || self.key_down(KeyCode::ArrowRight) {
            r += 1.0;
        }
        if self.key_down(KeyCode::KeyA) || self.key_down(KeyCode::ArrowLeft) {
            r -= 1.0;
        }
        if self.key_down(KeyCode::KeyE) || self.key_down(KeyCode::Space) {
            u += 1.0;
        }
        if self.key_down(KeyCode::KeyQ) || self.key_down(KeyCode::ControlLeft) {
            u -= 1.0;
        }
        Vec3::new(r, u, f)
    }

    /// 生成相机控制输入。`orbit_drag` 表示当前是否处于轨道旋转手势。
    pub fn camera_input(&self, dt: f32, orbit_drag: bool) -> CameraInput {
        let axes = self.movement_axes();
        CameraInput {
            forward: axes.z,
            right: axes.x,
            up: axes.y,
            look_delta: self.mouse_delta,
            scroll: self.scroll_delta,
            dt,
            boost: self.shift,
            dragging: orbit_drag,
        }
    }

    /// 给动作追加一条绑定（同一个动作可以有多条）。
    pub fn bind_action(&mut self, action: impl Into<String>, binding: ActionBinding) {
        self.bindings
            .entry(action.into())
            .or_default()
            .push(binding);
    }

    /// 清空一个动作的全部绑定。
    pub fn clear_action(&mut self, action: &str) {
        self.bindings.remove(action);
    }

    /// 把动作的 `old` 绑定换成 `new`；`old` 不存在时返回 `false` 且不改动。
    pub fn rebind_action(&mut self, action: &str, old: ActionBinding, new: ActionBinding) -> bool {
        let Some(list) = self.bindings.get_mut(action) else {
            return false;
        };
        let Some(slot) = list.iter().position(|b| *b == old) else {
            return false;
        };
        list[slot] = new;
        true
    }

    /// 一个动作的绑定列表（调试/设置界面用）。
    pub fn action_bindings(&self, action: &str) -> Option<&[ActionBinding]> {
        self.bindings.get(action).map(Vec::as_slice)
    }

    /// 动作是否按住。
    pub fn action_down(&self, action: &str) -> bool {
        self.bindings
            .get(action)
            .is_some_and(|list| list.iter().any(|b| self.binding_active(*b, false)))
    }

    /// 动作本帧是否刚按下（键盘/鼠标/手柄都按上一帧状态判边界）。
    pub fn action_pressed(&self, action: &str) -> bool {
        self.bindings
            .get(action)
            .is_some_and(|list| list.iter().any(|b| self.binding_active(*b, true)))
    }

    /// 动作本帧是否刚抬起。
    pub fn action_released(&self, action: &str) -> bool {
        self.bindings.get(action).is_some_and(|list| {
            list.iter()
                .any(|b| self.binding_was_active(*b) && !self.binding_active(*b, false))
        })
    }

    fn binding_active(&self, binding: ActionBinding, pressed: bool) -> bool {
        match binding {
            ActionBinding::Key(key) => {
                if pressed {
                    self.key_pressed(key)
                } else {
                    self.key_down(key)
                }
            }
            ActionBinding::Mouse(button) => {
                if pressed {
                    self.mouse_button_pressed(button)
                } else {
                    self.mouse_button_down(button)
                }
            }
            ActionBinding::GamepadButton { pad, button } => {
                if button >= 32 {
                    return false;
                }
                let mask = 1u32 << button;
                let now = self.gamepad_buttons(pad as usize) & mask != 0;
                if pressed {
                    let before = self
                        .gamepads_prev
                        .get(pad as usize)
                        .filter(|g| g.connected)
                        .map_or(0, |g| g.buttons)
                        & mask
                        != 0;
                    now && !before
                } else {
                    now
                }
            }
            ActionBinding::GamepadAxis {
                pad,
                axis,
                positive,
            } => {
                let axis_value = |gamepad: &GamepadState| -> f32 {
                    gamepad.axes.get(axis as usize).copied().unwrap_or(0.0)
                        * if positive { 1.0 } else { -1.0 }
                };
                let now = self
                    .gamepads
                    .get(pad as usize)
                    .filter(|g| g.connected)
                    .is_some_and(|g| axis_value(g) >= 0.5);
                if pressed {
                    let before = self
                        .gamepads_prev
                        .get(pad as usize)
                        .filter(|g| g.connected)
                        .is_some_and(|g| axis_value(g) >= 0.5);
                    now && !before
                } else {
                    now
                }
            }
        }
    }

    fn binding_was_active(&self, binding: ActionBinding) -> bool {
        match binding {
            ActionBinding::GamepadButton { pad, button } => {
                button < 32
                    && self
                        .gamepads_prev
                        .get(pad as usize)
                        .filter(|g| g.connected)
                        .is_some_and(|g| g.buttons & (1u32 << button) != 0)
            }
            ActionBinding::Key(key) => self.key_released(key),
            ActionBinding::Mouse(button) => self.mouse_button_released(button),
            ActionBinding::GamepadAxis {
                pad,
                axis,
                positive,
            } => self
                .gamepads_prev
                .get(pad as usize)
                .filter(|g| g.connected)
                .is_some_and(|g| {
                    let value = g.axes.get(axis as usize).copied().unwrap_or(0.0)
                        * if positive { 1.0 } else { -1.0 };
                    value >= 0.5
                }),
        }
    }

    /// 双指捏合的帧间距离变化（当前两指距离 − 上一帧同两指距离）。
    pub fn pinch_delta(&self) -> f32 {
        let active: Vec<&TouchPoint> = self
            .touches
            .iter()
            .filter(|t| t.phase != TouchPhase::Ended)
            .collect();
        let Some((first, second)) = active.first().zip(active.get(1)) else {
            return 0.0;
        };
        let Some(prev_first) = self.touches_prev.iter().find(|t| t.id == first.id) else {
            return 0.0;
        };
        let Some(prev_second) = self.touches_prev.iter().find(|t| t.id == second.id) else {
            return 0.0;
        };
        (first.position - second.position).length()
            - (prev_first.position - prev_second.position).length()
    }

    /// 单指/多指拖动的帧间平均位移（逻辑像素）。
    pub fn swipe_delta(&self) -> Vec2 {
        let mut sum = Vec2::ZERO;
        let mut count = 0usize;
        for touch in &self.touches {
            if touch.phase == TouchPhase::Ended {
                continue;
            }
            if let Some(prev) = self.touches_prev.iter().find(|t| t.id == touch.id) {
                sum += touch.position - prev.position;
                count += 1;
            }
        }
        if count == 0 {
            Vec2::ZERO
        } else {
            sum / count as f32
        }
    }

    /// 手柄是否连接。
    pub fn gamepad_connected(&self, pad: usize) -> bool {
        self.gamepads.get(pad).is_some_and(|g| g.connected)
    }

    /// 手柄轴值；未连接或越界返回 0。
    pub fn gamepad_axis(&self, pad: usize, axis: usize) -> f32 {
        self.gamepads
            .get(pad)
            .filter(|g| g.connected)
            .and_then(|g| g.axes.get(axis))
            .copied()
            .unwrap_or(0.0)
    }

    /// 手柄按键位图；未连接或越界返回 0。
    pub fn gamepad_buttons(&self, pad: usize) -> u32 {
        self.gamepads
            .get(pad)
            .filter(|g| g.connected)
            .map_or(0, |g| g.buttons)
    }

    /// 当前触屏点（不含上一帧）。
    pub fn touches(&self) -> &[TouchPoint] {
        &self.touches
    }

    /// 某一帧的触屏点是否还在按着（`Started/Moved` 或本帧 `Ended` 之前）。
    pub fn touch_down(&self, id: u64) -> bool {
        if let Some(t) = self.touches.iter().find(|t| t.id == id) {
            return t.phase != TouchPhase::Ended;
        }
        self.touches_prev.iter().any(|t| t.id == id)
    }

    /// 注入手柄连接状态。
    pub fn on_gamepad_connected(&mut self, pad: usize, connected: bool) {
        if let Some(g) = self.gamepads.get_mut(pad) {
            g.connected = connected;
            if !connected {
                *g = GamepadState::default();
            }
        }
    }

    /// 注入手柄轴值（非有限值按 0 处理）。
    pub fn on_gamepad_axis(&mut self, pad: usize, axis: usize, value: f32) {
        if let Some(g) = self.gamepads.get_mut(pad) {
            if let Some(slot) = g.axes.get_mut(axis) {
                *slot = if value.is_finite() {
                    value.clamp(-1.0, 1.0)
                } else {
                    0.0
                };
            }
        }
    }

    /// 注入手柄按键位图。
    pub fn on_gamepad_buttons(&mut self, pad: usize, buttons: u32) {
        if let Some(g) = self.gamepads.get_mut(pad) {
            g.buttons = buttons;
        }
    }

    /// 注入触屏开始/移动。
    pub fn on_touch(&mut self, id: u64, position: Vec2, phase: TouchPhase) {
        self.touches.retain(|t| t.id != id);
        self.touches.push(TouchPoint {
            id,
            position,
            phase,
        });
    }

    /// 注入触屏结束。
    pub fn on_touch_end(&mut self, id: u64, position: Vec2) {
        self.on_touch(id, position, TouchPhase::Ended);
    }

    // ---- 由窗口层调用的事件注入 ----

    /// 注入键盘按下。
    pub fn on_key_down(&mut self, key: KeyCode) {
        if self.keys_down.insert(key) {
            self.keys_pressed.insert(key);
        }
    }

    /// 注入键盘抬起。
    pub fn on_key_up(&mut self, key: KeyCode) {
        self.keys_down.remove(&key);
        self.keys_released.insert(key);
    }

    /// 注入鼠标按下。
    pub fn on_mouse_down(&mut self, button: MouseButton) {
        let i = button as usize;
        if !self.mouse_down[i] {
            self.mouse_down[i] = true;
            self.mouse_pressed[i] = true;
        }
    }

    /// 注入鼠标抬起。
    pub fn on_mouse_up(&mut self, button: MouseButton) {
        let i = button as usize;
        self.mouse_down[i] = false;
        self.mouse_released[i] = true;
    }

    /// 注入光标移动。
    pub fn on_cursor_moved(&mut self, position: Vec2) {
        self.mouse_delta += position - self.mouse_position;
        self.mouse_position = position;
    }

    /// 注入滚轮。
    pub fn on_scroll(&mut self, delta: f32) {
        self.scroll_delta += delta;
    }

    /// 窗口失焦时清理按键状态，避免"按键卡住"。
    pub fn on_focus_lost(&mut self) {
        self.keys_down.clear();
        self.mouse_down = [false; 3];
        self.focused = false;
    }

    /// 所有按下的键（调试用）。
    pub fn pressed_keys(&self) -> impl Iterator<Item = &KeyCode> {
        self.keys_pressed.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_and_release_lifecycle() {
        let mut input = Input::new();
        input.on_key_down(KeyCode::KeyW);
        assert!(input.key_down(KeyCode::KeyW));
        assert!(input.key_pressed(KeyCode::KeyW));
        input.begin_frame();
        assert!(input.key_down(KeyCode::KeyW));
        assert!(!input.key_pressed(KeyCode::KeyW), "瞬时状态应被清空");
        input.on_key_up(KeyCode::KeyW);
        assert!(!input.key_down(KeyCode::KeyW));
        assert!(input.key_released(KeyCode::KeyW));
    }

    #[test]
    fn movement_axes() {
        let mut input = Input::new();
        input.on_key_down(KeyCode::KeyW);
        input.on_key_down(KeyCode::KeyD);
        let a = input.movement_axes();
        assert_eq!(a.z, 1.0);
        assert_eq!(a.x, 1.0);
        assert_eq!(a.y, 0.0);
    }

    #[test]
    fn cursor_delta_accumulates() {
        let mut input = Input::new();
        input.on_cursor_moved(Vec2::new(10.0, 10.0));
        let d1 = input.mouse_delta;
        assert_eq!(d1, Vec2::new(10.0, 10.0));
        input.begin_frame();
        input.on_cursor_moved(Vec2::new(15.0, 12.0));
        assert_eq!(input.mouse_delta, Vec2::new(5.0, 2.0));
    }

    #[test]
    fn focus_lost_clears_keys() {
        let mut input = Input::new();
        input.on_key_down(KeyCode::Space);
        input.on_mouse_down(MouseButton::Left);
        input.on_focus_lost();
        assert!(!input.key_down(KeyCode::Space));
        assert!(!input.mouse_button_down(MouseButton::Left));
    }

    #[test]
    fn gamepad_state_is_injected_and_bounded() {
        let mut input = Input::new();
        assert!(!input.gamepad_connected(0));
        assert_eq!(input.gamepad_axis(0, 0), 0.0);
        input.on_gamepad_connected(0, true);
        input.on_gamepad_axis(0, 0, 0.5);
        input.on_gamepad_axis(0, 1, 3.0);
        input.on_gamepad_axis(0, 2, f32::NAN);
        input.on_gamepad_buttons(0, 0b101);
        assert!(input.gamepad_connected(0));
        assert_eq!(input.gamepad_axis(0, 0), 0.5);
        assert_eq!(input.gamepad_axis(0, 1), 1.0);
        assert_eq!(input.gamepad_axis(0, 2), 0.0);
        assert_eq!(input.gamepad_buttons(0), 0b101);
        input.on_gamepad_connected(0, false);
        assert_eq!(input.gamepad_buttons(0), 0);
    }

    #[test]
    fn touch_keeps_lifecycle_and_ends_only_for_one_frame() {
        let mut input = Input::new();
        input.on_touch(7, Vec2::new(1.0, 2.0), TouchPhase::Started);
        assert!(input.touch_down(7));
        input.begin_frame();
        // 上一帧按着：begin_frame 后仍算 down，直到显式 End。
        assert!(input.touch_down(7));
        input.on_touch(7, Vec2::new(3.0, 4.0), TouchPhase::Moved);
        assert_eq!(input.touches()[0].position, Vec2::new(3.0, 4.0));
        input.on_touch_end(7, Vec2::new(3.0, 4.0));
        assert!(!input.touch_down(7));
        input.begin_frame();
        assert!(input.touches().is_empty(), "Ended 只保留一帧");
    }

    #[test]
    fn action_rebinding_stops_the_old_binding() {
        let mut input = Input::new();
        input.bind_action("jump", ActionBinding::Key(KeyCode::Space));
        input.on_key_down(KeyCode::Space);
        assert!(input.action_down("jump"));
        assert!(input.action_pressed("jump"));
        input.begin_frame();
        assert!(!input.action_pressed("jump"), "按住不算刚按下");
        assert!(input.rebind_action(
            "jump",
            ActionBinding::Key(KeyCode::Space),
            ActionBinding::Key(KeyCode::KeyJ)
        ));
        input.on_key_up(KeyCode::Space);
        assert!(!input.action_down("jump"), "重绑定后旧键必须失效");
        input.on_key_down(KeyCode::KeyJ);
        assert!(input.action_down("jump"));
        assert!(input.action_pressed("jump"));
        assert!(
            !input.rebind_action(
                "jump",
                ActionBinding::Key(KeyCode::KeyQ),
                ActionBinding::Key(KeyCode::KeyK)
            ),
            "不存在的旧绑定不能假装成功"
        );
    }

    #[test]
    fn gamepad_bindings_report_pressed_and_released_edges() {
        let mut input = Input::new();
        input.bind_action("fire", ActionBinding::GamepadButton { pad: 0, button: 1 });
        input.bind_action(
            "forward",
            ActionBinding::GamepadAxis {
                pad: 0,
                axis: 1,
                positive: true,
            },
        );
        input.on_gamepad_connected(0, true);
        input.on_gamepad_buttons(0, 0b10);
        input.on_gamepad_axis(0, 1, 0.75);
        assert!(input.action_down("fire"));
        assert!(input.action_pressed("fire"));
        assert!(input.action_down("forward"));
        input.begin_frame();
        assert!(!input.action_pressed("fire"), "按住不算刚按下");
        assert!(!input.action_pressed("forward"));
        input.on_gamepad_buttons(0, 0);
        input.on_gamepad_axis(0, 1, 0.0);
        assert!(input.action_released("fire"));
        assert!(input.action_released("forward"));
        assert!(!input.action_down("fire"));
        // 未连接的手柄不触发。
        input.on_gamepad_connected(0, false);
        input.on_gamepad_buttons(0, 0b10);
        assert!(!input.action_down("fire"));
    }

    #[test]
    fn pinch_and_swipe_follow_two_fingers() {
        let mut input = Input::new();
        input.on_touch(1, Vec2::new(0.0, 0.0), TouchPhase::Started);
        input.on_touch(2, Vec2::new(10.0, 0.0), TouchPhase::Started);
        input.begin_frame();
        input.on_touch(1, Vec2::new(0.0, 0.0), TouchPhase::Moved);
        input.on_touch(2, Vec2::new(15.0, 0.0), TouchPhase::Moved);
        assert!((input.pinch_delta() - 5.0).abs() < 1e-6);
        let swipe = input.swipe_delta();
        assert!((swipe.x - 2.5).abs() < 1e-6);
        assert_eq!(swipe.y, 0.0);
        // 少于两指没有捏合。
        input.on_touch_end(2, Vec2::new(15.0, 0.0));
        assert_eq!(input.pinch_delta(), 0.0);
    }
}
