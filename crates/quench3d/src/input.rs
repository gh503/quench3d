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
}
