//! 相机、视锥剔除与内置控制器（轨道相机 / 自由飞行相机）。

use quench3d_math::{clamp, Aabb, Mat4, Plane, Quat, Ray, Vec2, Vec3};

/// 投影方式。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Projection {
    /// 透视投影（`fov_y` 为弧度）。
    Perspective,
    /// 正交投影（`height` 为垂直方向可视高度）。
    Orthographic {
        /// 垂直可视高度。
        height: f32,
    },
}

/// 3D 相机。
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    /// 世界坐标位置。
    pub position: Vec3,
    /// 朝向（`-Z` 为前）。
    pub rotation: Quat,
    /// 垂直视场角（弧度），仅透视有效。
    pub fov_y: f32,
    /// 近裁剪面。
    pub near: f32,
    /// 远裁剪面。
    pub far: f32,
    /// 宽高比（宽 / 高）。
    pub aspect: f32,
    /// 投影方式。
    pub projection: Projection,
}

impl Default for Camera {
    fn default() -> Self {
        Self::perspective(16.0 / 9.0)
    }
}

impl Camera {
    /// 透视相机。
    pub fn perspective(aspect: f32) -> Self {
        Self {
            position: Vec3::new(0.0, 1.0, 5.0),
            rotation: Quat::IDENTITY,
            fov_y: 60f32.to_radians(),
            near: 0.1,
            far: 1000.0,
            aspect: aspect.max(1e-4),
            // 非正/非有限的高度一律退回透视，避免把相机搞成"什么都看不见"
            projection: Projection::Perspective,
        }
    }

    /// 正交相机。
    pub fn orthographic(aspect: f32, height: f32) -> Self {
        let mut c = Self::perspective(aspect);
        c.projection = Projection::Orthographic { height };
        c
    }

    /// 观察矩阵（世界 → 观察空间）。
    pub fn view(&self) -> Mat4 {
        let inv_rot = self.rotation.inverse();
        let eye = self.position;
        // 等价于 look_at(pos, pos + forward, up)，但更快且不会退化。
        let m = Mat4::from_quat(inv_rot);
        let t = m.transform_vector3(-eye);
        Mat4::from_cols(m.cols[0], m.cols[1], m.cols[2], t.extend(1.0))
    }

    /// 投影矩阵。
    pub fn projection_matrix(&self) -> Mat4 {
        match self.projection {
            Projection::Perspective => {
                Mat4::perspective_rh(self.fov_y, self.aspect, self.near, self.far)
            }
            Projection::Orthographic { height } => {
                let h = height * 0.5;
                let w = h * self.aspect;
                Mat4::orthographic_rh(-w, w, -h, h, self.near, self.far)
            }
        }
    }

    /// 视图投影矩阵。
    pub fn view_projection(&self) -> Mat4 {
        self.projection_matrix() * self.view()
    }

    /// 前方向。
    #[inline]
    pub fn forward(&self) -> Vec3 {
        self.rotation.forward()
    }

    /// 右方向。
    #[inline]
    pub fn right(&self) -> Vec3 {
        self.rotation.right()
    }

    /// 上方向。
    #[inline]
    pub fn up(&self) -> Vec3 {
        self.rotation.up()
    }

    /// 更新宽高比。
    pub fn set_aspect(&mut self, aspect: f32) {
        self.aspect = aspect.max(1e-4);
    }

    /// 朝向目标点。
    pub fn look_at(&mut self, target: Vec3) {
        self.rotation = Quat::look_to_rh(target - self.position, Vec3::Y);
    }

    /// 由归一化设备坐标 `[-1, 1]`（y 向上）生成世界空间射线。
    pub fn ndc_ray(&self, ndc: Vec2) -> Ray {
        let inv = self.view_projection().inverse();
        let near = inv.project_point3(Vec3::new(ndc.x, ndc.y, 0.0));
        let far = inv.project_point3(Vec3::new(ndc.x, ndc.y, 1.0));
        Ray::new(near, (far - near).normalize_or_zero())
    }

    /// 由屏幕像素坐标生成世界空间射线（`viewport` 为 `(宽, 高)`）。
    pub fn screen_ray(&self, screen: Vec2, viewport: Vec2) -> Ray {
        let ndc = Vec2::new(
            (screen.x / viewport.x) * 2.0 - 1.0,
            1.0 - (screen.y / viewport.y) * 2.0,
        );
        self.ndc_ray(ndc)
    }

    /// 世界点投影到屏幕像素坐标；在相机背后返回 `None`。
    pub fn world_to_screen(&self, world: Vec3, viewport: Vec2) -> Option<Vec2> {
        let clip = self.view_projection() * world.extend(1.0);
        if clip.w <= 1e-6 {
            return None;
        }
        Some(Vec2::new(
            (clip.x / clip.w * 0.5 + 0.5) * viewport.x,
            (0.5 - clip.y / clip.w * 0.5) * viewport.y,
        ))
    }

    /// 提取 6 个视锥平面（用于剔除）。
    pub fn frustum(&self) -> Frustum {
        Frustum::from_view_projection(&self.view_projection())
    }

    /// 根据网格的包围盒确定合适的相机距离（`focus` 用）。
    pub fn fit_to_aabb(&mut self, bounds: &Aabb, margin: f32) {
        let radius = bounds.half_extents().length().max(1e-3);
        let half_fov = (self.fov_y * 0.5).max(0.05);
        let dist = radius / half_fov.sin() * margin.max(1.0);
        self.far = (dist + radius * 4.0).max(self.far.min(100.0));
        self.near = (dist * 0.001).max(0.01);
        self.position = bounds.center() - self.forward() * dist;
    }
}

/// 视锥体（6 个内朝向平面）。
#[derive(Clone, Copy, Debug)]
pub struct Frustum {
    /// 平面顺序：左、右、下、上、近、远。
    pub planes: [Plane; 6],
}

impl Frustum {
    /// 由视图投影矩阵提取平面（Gribb–Hartmann 方法）。
    pub fn from_view_projection(vp: &Mat4) -> Self {
        let rows = |i: usize| {
            quench3d_math::Vec4::new(vp.cols[0][i], vp.cols[1][i], vp.cols[2][i], vp.cols[3][i])
        };
        let (r0, r1, r2, r3) = (rows(0), rows(1), rows(2), rows(3));
        let mk = |v: quench3d_math::Vec4| -> Plane {
            let n = Vec3::new(v.x, v.y, v.z);
            let len = n.length();
            if len > 1e-12 {
                Plane::new(n / len, v.w / len)
            } else {
                Plane::new(Vec3::Y, f32::INFINITY)
            }
        };
        Self {
            planes: [
                mk(r3 + r0), // 左
                mk(r3 - r0), // 右
                mk(r3 + r1), // 下
                mk(r3 - r1), // 上
                mk(r2),      // 近（wgpu 深度区间 [0,1]）
                mk(r3 - r2), // 远
            ],
        }
    }

    /// 包围盒是否与视锥相交（保守判定，可能有假阳性但不会漏剔除可见物体）。
    pub fn intersects_aabb(&self, b: &Aabb) -> bool {
        for p in &self.planes {
            // 取沿法线最远的角点。
            let positive = Vec3::new(
                if p.normal.x >= 0.0 { b.max.x } else { b.min.x },
                if p.normal.y >= 0.0 { b.max.y } else { b.min.y },
                if p.normal.z >= 0.0 { b.max.z } else { b.min.z },
            );
            if p.signed_distance(positive) < 0.0 {
                return false;
            }
        }
        true
    }

    /// 点是否在视锥内。
    pub fn contains_point(&self, p: Vec3) -> bool {
        self.planes.iter().all(|pl| pl.signed_distance(p) >= 0.0)
    }
}

/// 一帧的相机输入（由窗口层填充，与具体平台解耦）。
#[derive(Clone, Copy, Debug, Default)]
pub struct CameraInput {
    /// 前后移动轴 `[-1, 1]`。
    pub forward: f32,
    /// 左右移动轴 `[-1, 1]`。
    pub right: f32,
    /// 上下移动轴 `[-1, 1]`。
    pub up: f32,
    /// 鼠标位移（像素）。
    pub look_delta: Vec2,
    /// 滚轮增量。
    pub scroll: f32,
    /// 本帧时长（秒）。
    pub dt: f32,
    /// 是否按下加速键。
    pub boost: bool,
    /// 是否处于"拖动观察"状态（如按住中键）。
    pub dragging: bool,
}

/// 轨道相机（绕焦点旋转，工程/建模场景的默认操作方式）。
#[derive(Clone, Copy, Debug)]
pub struct OrbitCamera {
    /// 观察焦点。
    pub focus: Vec3,
    /// 到焦点的距离。
    pub distance: f32,
    /// 水平角（弧度）。
    pub yaw: f32,
    /// 俯仰角（弧度，已夹紧）。
    pub pitch: f32,
    /// 最近距离。
    pub min_distance: f32,
    /// 最远距离。
    pub max_distance: f32,
    /// 旋转灵敏度（弧度 / 像素）。
    pub sensitivity: f32,
    /// 缩放速度。
    pub zoom_speed: f32,
    /// 平移速度。
    pub pan_speed: f32,
    /// 视场角。
    pub fov_y: f32,
    /// 近裁剪面。
    pub near: f32,
    /// 远裁剪面。
    pub far: f32,
    /// 正交投影的可视高度（世界单位）。
    ///
    /// `None`（默认）= 透视投影，与以往逐位一致；给了正值就切到正交投影，
    /// 也就是工程制图那种"没有近大远小"的画法。
    pub orthographic_height: Option<f32>,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        Self {
            focus: Vec3::ZERO,
            distance: 12.0,
            yaw: 0.6,
            pitch: -0.4,
            min_distance: 0.2,
            max_distance: 5000.0,
            sensitivity: 0.006,
            zoom_speed: 0.12,
            pan_speed: 1.0,
            fov_y: 60f32.to_radians(),
            near: 0.05,
            far: 5000.0,
            orthographic_height: None,
        }
    }
}

impl OrbitCamera {
    /// 生成当前相机。
    pub fn camera(&self, aspect: f32) -> Camera {
        let dir = self.direction();
        Camera {
            position: self.focus + dir * self.distance,
            rotation: Quat::look_to_rh(-dir, Vec3::Y),
            fov_y: self.fov_y,
            near: self.near,
            far: self.far,
            aspect: aspect.max(1e-4),
            // 非正/非有限的高度一律退回透视，避免相机变成"什么都看不见"
            projection: match self.orthographic_height {
                Some(h) if h.is_finite() && h > 0.0 => Projection::Orthographic { height: h },
                _ => Projection::Perspective,
            },
        }
    }

    /// 由焦点指向相机的单位向量。
    ///
    /// `pitch > 0` 表示相机位于焦点**上方**（俯视），`yaw` 绕 +Y 轴旋转，
    /// `yaw = 0` 时相机在焦点的 +Z 方向。
    pub fn direction(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(cp * sy, sp, cp * cy).normalize_or_zero()
    }

    /// 根据输入更新。
    pub fn update(&mut self, input: &CameraInput) {
        if input.look_delta.length_squared() > 0.0 && input.dragging {
            // 抓取式转动：向右拖 → 相机绕到左侧；向下拖 → 相机升高俯视。
            self.yaw -= input.look_delta.x * self.sensitivity;
            self.pitch = clamp(
                self.pitch + input.look_delta.y * self.sensitivity,
                -core::f32::consts::FRAC_PI_2 + 0.01,
                core::f32::consts::FRAC_PI_2 - 0.01,
            );
        }
        if input.scroll != 0.0 {
            let factor = (1.0 - input.scroll * self.zoom_speed).clamp(0.2, 5.0);
            self.distance = clamp(self.distance * factor, self.min_distance, self.max_distance);
        }
        // 方向键 / WASD → 平移焦点（Q E 升降）。
        let pan = Vec3::new(input.right, input.up, input.forward);
        if pan.length_squared() > 0.0 {
            let view = -self.direction();
            let flat = Vec3::new(view.x, 0.0, view.z).normalize_or_zero();
            let right = flat.cross(Vec3::Y);
            let speed =
                self.pan_speed * self.distance * input.dt * if input.boost { 4.0 } else { 1.0 };
            self.focus += (right * pan.x + Vec3::Y * pan.y + flat * pan.z) * speed;
        }
    }

    /// 让相机框住包围盒。
    pub fn focus_on(&mut self, bounds: &Aabb) {
        self.focus = bounds.center();
        let radius = bounds.half_extents().length().max(1e-3);
        self.distance = clamp(
            radius / (self.fov_y * 0.5).sin() * 1.6,
            self.min_distance,
            self.max_distance,
        );
    }
}

/// 自由飞行相机（第一人称 / 关卡漫游）。
#[derive(Clone, Copy, Debug)]
pub struct FlyCamera {
    /// 位置。
    pub position: Vec3,
    /// 水平角。
    pub yaw: f32,
    /// 俯仰角。
    pub pitch: f32,
    /// 移动速度（单位/秒）。
    pub speed: f32,
    /// 鼠标灵敏度。
    pub sensitivity: f32,
    /// 视场角。
    pub fov_y: f32,
}

impl Default for FlyCamera {
    fn default() -> Self {
        Self {
            position: Vec3::new(0.0, 2.0, 6.0),
            yaw: 0.0,
            pitch: 0.0,
            speed: 6.0,
            sensitivity: 0.0035,
            fov_y: 70f32.to_radians(),
        }
    }
}

impl FlyCamera {
    /// 生成当前相机。
    pub fn camera(&self, aspect: f32) -> Camera {
        Camera {
            position: self.position,
            rotation: Quat::from_euler_yxz(self.yaw, self.pitch, 0.0),
            fov_y: self.fov_y,
            near: 0.05,
            far: 8000.0,
            aspect: aspect.max(1e-4),
            projection: Projection::Perspective,
        }
    }

    /// 根据输入更新（前进沿视线方向，`boost` 加速）。
    pub fn update(&mut self, input: &CameraInput) {
        if input.dragging || input.look_delta.length_squared() > 0.0 {
            self.yaw -= input.look_delta.x * self.sensitivity;
            self.pitch = clamp(
                self.pitch - input.look_delta.y * self.sensitivity,
                -core::f32::consts::FRAC_PI_2 + 0.01,
                core::f32::consts::FRAC_PI_2 - 0.01,
            );
        }
        let rot = Quat::from_euler_yxz(self.yaw, self.pitch, 0.0);
        let dir = rot.rotation_direction(input.right, input.up, input.forward);
        if dir.length_squared() > 0.0 {
            let v = dir.normalize_or_zero()
                * self.speed
                * input.dt
                * if input.boost { 4.0 } else { 1.0 };
            self.position += v;
        }
    }
}

/// `Quat` 的辅助扩展：把本地轴上的输入转换成世界方向。
trait RotationDirection {
    fn rotation_direction(&self, right: f32, up: f32, forward: f32) -> Vec3;
}

impl RotationDirection for Quat {
    fn rotation_direction(&self, right: f32, up: f32, forward: f32) -> Vec3 {
        self.mul_vec3(Vec3::new(right, 0.0, -forward)) + Vec3::Y * up
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quench3d_math::vec3;

    #[test]
    fn view_matrix_moves_world_to_origin() {
        let mut c = Camera::perspective(1.0);
        c.position = vec3(0.0, 0.0, 10.0);
        c.rotation = Quat::IDENTITY;
        let v = c.view().transform_point3(Vec3::ZERO);
        assert!((v - vec3(0.0, 0.0, -10.0)).length() < 1e-5, "实际 {v:?}");
    }

    #[test]
    fn ndc_center_ray_points_forward() {
        let mut c = Camera::perspective(1.0);
        c.position = vec3(0.0, 0.0, 10.0);
        c.rotation = Quat::IDENTITY;
        let r = c.ndc_ray(Vec2::ZERO);
        assert!((r.dir - Vec3::NEG_Z).length() < 1e-4, "方向 {:?}", r.dir);
    }

    #[test]
    fn screen_roundtrip() {
        let mut c = Camera::perspective(16.0 / 9.0);
        c.position = vec3(0.0, 2.0, 8.0);
        c.look_at(vec3(0.0, 1.0, 0.0));
        let vp = Vec2::new(1280.0, 720.0);
        let p = vec3(1.0, 1.0, 0.0);
        let s = c.world_to_screen(p, vp).unwrap();
        let ray = c.screen_ray(s, vp);
        // 射线应穿过原点附近的该点方向
        let to_p = (p - ray.origin).normalize_or_zero();
        assert!(to_p.dot(ray.dir) > 0.999, "射线未对准目标");
    }

    #[test]
    fn frustum_culls_far_away_box() {
        let c = Camera::perspective(1.0);
        let f = c.frustum();
        assert!(f.intersects_aabb(&Aabb::new(vec3(-1.0, -1.0, -11.0), vec3(1.0, 1.0, -9.0))));
        assert!(!f.intersects_aabb(&Aabb::new(vec3(-1.0, -1.0, 9.0), vec3(1.0, 1.0, 11.0))));
        assert!(!f.intersects_aabb(&Aabb::new(vec3(100.0, 0.0, -10.0), vec3(102.0, 2.0, -8.0))));
    }

    #[test]
    fn orbit_camera_looks_at_focus() {
        let mut o = OrbitCamera {
            focus: vec3(1.0, 2.0, 3.0),
            ..Default::default()
        };
        let c = o.camera(1.0);
        let d = (o.focus - c.position).normalize_or_zero();
        assert!(d.dot(c.forward()) > 0.999, "轨道相机未看向焦点");
        o.update(&CameraInput {
            scroll: 1.0,
            dt: 0.016,
            ..Default::default()
        });
        assert!(o.distance < 12.0, "向上滚动应拉近");
    }

    #[test]
    fn fly_camera_moves_forward() {
        let mut f = FlyCamera {
            position: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            ..Default::default()
        };
        f.update(&CameraInput {
            forward: 1.0,
            dt: 1.0,
            ..Default::default()
        });
        assert!(
            (f.position - vec3(0.0, 0.0, -f.speed)).length() < 1e-4,
            "{:?}",
            f.position
        );
    }
}
