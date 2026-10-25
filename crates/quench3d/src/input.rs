//! 输入状态：把窗口事件归一化成每帧可查询的状态，并生成相机输入。

use std::collections::HashSet;

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
}
