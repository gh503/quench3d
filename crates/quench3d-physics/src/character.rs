//! 角色控制器：胶囊体的 `move_and_slide`。
//!
//! 做法（迭代式「先走再解穿」）：
//! 1. 用窄相在候选位置求解穿透，把角色沿接触法线推出；
//! 2. 把剩余位移投影到接触平面上；
//! 3. 重复若干轮（处理角落、多面接触）；
//! 4. 可选台阶抬升与贴地吸附。
//!
//! 角色不受动力学驱动：本模块直接写回位姿、清零速度，适合玩家角色。

use quench3d_math::*;

use crate::gjk;
use crate::narrowphase;
use crate::shape::Shape;
use crate::world::PhysicsWorld;

/// 移动结果。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoveResult {
    /// 实际发生的位移。
    pub translation: Vec3,
    /// 是否落地（有向上的接触）。
    pub grounded: bool,
    /// 地面法线（未落地时为 `Vec3::ZERO`）。
    pub ground_normal: Vec3,
}

/// 角色控制器参数。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CharacterController {
    /// 胶囊半径。
    pub radius: f32,
    /// 胶囊总高度（含两端球帽），至少为 `2 * radius`。
    pub height: f32,
    /// 可站立坡度（度）。
    pub slope_limit_deg: f32,
    /// 可自动跨越的台阶高度。
    pub step_height: f32,
    /// 是否在落地后吸附到地面。
    pub snap_to_ground: bool,
}

impl Default for CharacterController {
    fn default() -> Self {
        Self {
            radius: 0.4,
            height: 1.8,
            slope_limit_deg: 45.0,
            step_height: 0.3,
            snap_to_ground: true,
        }
    }
}

/// 一次移动最多切分的子步数。
const MAX_SUBSTEPS_CHAR: usize = 32;
/// 推出穿透的放大系数（略微过冲以避免残留）。
const DEPENETRATION_GAIN: f32 = 1.0;
/// 视为可站立的法线斜率阈值（`cos(slope_limit)`）。
impl CharacterController {
    /// 胶囊形状（沿本地 Y 轴，原点在胶囊中心）。
    pub fn capsule(&self) -> Shape {
        let r = self.radius.abs().max(1e-4);
        let h = self.height.abs().max(2.0 * r);
        Shape::Capsule {
            half_height: (h * 0.5 - r).max(0.0),
            radius: r,
        }
    }

    /// 半高。
    fn half_height(&self) -> f32 {
        self.capsule_half_height()
    }

    /// 胶囊轴半高。
    fn capsule_half_height(&self) -> f32 {
        match self.capsule() {
            Shape::Capsule { half_height, .. } => half_height,
            _ => 0.0,
        }
    }

    /// 可站立坡度阈值（余弦值）。
    fn slope_cos(&self) -> f32 {
        clamp(self.slope_limit_deg, 0.0, 90.0).to_radians().cos()
    }

    /// 移动角色并做滑动。`position` 为胶囊中心位置。
    ///
    /// 位移被切成若干「子步」：每个子步先整体前进，再迭代解穿，因此既有
    /// 滑动效果，也不会因为单帧位移过大而穿过障碍。
    pub fn move_and_slide(
        &self,
        world: &mut PhysicsWorld,
        position: Vec3,
        desired_translation: Vec3,
    ) -> MoveResult {
        let shape = self.capsule();
        let slope_cos = self.slope_cos();
        let translation = if desired_translation.is_finite() {
            desired_translation
        } else {
            Vec3::ZERO
        };
        let start = if position.is_finite() {
            position
        } else {
            Vec3::ZERO
        };
        // 起始解穿：避免一开始就嵌在几何体里。
        let (depenetrated, _initial_normals) = self.resolve(world, &shape, start);
        let (mut pos, mut normals) = self.slide(world, &shape, depenetrated, translation);

        // 台阶：如果被非可行走的障碍挡住，试着抬高一个台阶再走。
        if self.step_height > 0.0 {
            let goal = start + translation;
            let shortfall = (goal - pos).length();
            if shortfall > 1e-3 {
                let lift = vec3(0.0, self.step_height, 0.0);
                let (up, up_normals) = self.slide(world, &shape, pos + lift, translation);
                let up = self.snap(world, &shape, up);
                if (goal - up).length() < shortfall {
                    pos = up;
                    normals = up_normals;
                }
            }
        }

        // 贴地吸附
        if self.snap_to_ground {
            let snapped = self.snap(world, &shape, pos);
            if snapped.y < pos.y {
                pos = snapped;
            }
        }

        let mut ground_normal = Vec3::ZERO;
        for n in &normals {
            if n.y >= slope_cos && n.y > ground_normal.y {
                ground_normal = *n;
            }
        }
        let g = self.ground_info(world, &shape, pos);
        if g.0 && g.1.y > ground_normal.y {
            ground_normal = g.1;
        }
        MoveResult {
            translation: pos - start,
            grounded: ground_normal.y >= slope_cos && ground_normal.length_squared() > 0.5,
            ground_normal: if ground_normal.length_squared() > 0.5 {
                ground_normal
            } else {
                Vec3::ZERO
            },
        }
    }

    /// 按子步推进并解穿。
    fn slide(
        &self,
        world: &PhysicsWorld,
        shape: &Shape,
        mut pos: Vec3,
        translation: Vec3,
    ) -> (Vec3, Vec<Vec3>) {
        let mut normals: Vec<Vec3> = Vec::new();
        let total = translation.length();
        if total < 1e-9 {
            return (pos, normals);
        }
        let sub = (self.radius.abs().max(0.05) * 0.5).max(0.02);
        let count = ((total / sub).ceil() as usize).clamp(1, MAX_SUBSTEPS_CHAR);
        let step = translation / count as f32;
        for _ in 0..count {
            let (resolved, ns) = self.resolve(world, shape, pos + step);
            pos = resolved;
            for n in ns {
                if !normals.contains(&n) {
                    normals.push(n);
                }
            }
        }
        (pos, normals)
    }

    /// 在给定中心位置求解穿透，返回修正后的位置与接触法线（世界 A→角色）。
    fn resolve(&self, world: &PhysicsWorld, shape: &Shape, center: Vec3) -> (Vec3, Vec<Vec3>) {
        let mut pos = center;
        let mut normals: Vec<Vec3> = Vec::new();
        for _ in 0..4 {
            let mut best_depth = 0.0f32;
            let mut best_normal = Vec3::ZERO;
            let t = Transform::from_translation(pos);
            for (_, body) in world.bodies() {
                if let Some(m) =
                    narrowphase::collide_shapes(shape, &t, &body.shape, &body.transform)
                {
                    for cp in &m.points {
                        // n 由角色指向刚体 → 角色应沿 -n 退出。
                        let push = -cp.normal;
                        if cp.penetration > best_depth {
                            best_depth = cp.penetration;
                            best_normal = push;
                        }
                    }
                }
            }
            if best_depth <= 1e-4 || best_normal.length_squared() < 0.5 {
                break;
            }
            let n = best_normal.normalize_or_zero();
            pos += n * (best_depth * DEPENETRATION_GAIN);
            if !normals.contains(&n) {
                normals.push(n);
            }
        }
        (pos, normals)
    }

    /// 向下吸附：最多下探 `step_height + radius`。
    fn snap(&self, world: &PhysicsWorld, shape: &Shape, center: Vec3) -> Vec3 {
        let max_drop = self.step_height + self.radius;
        let mut best = center;
        // 二分之一逐级下探（保守且确定）。
        let steps = 6;
        for i in 1..=steps {
            let d = max_drop * i as f32 / steps as f32;
            let probe = center - vec3(0.0, d, 0.0);
            let (resolved, normals) = self.resolve(world, shape, probe);
            let grounded_here = normals.iter().any(|n| n.y >= self.slope_cos());
            if grounded_here || (resolved - probe).length_squared() > 1e-8 {
                best = resolved;
                break;
            }
            best = probe;
        }
        best
    }

    /// 当前是否落地以及地面法线。
    fn ground_info(&self, world: &PhysicsWorld, shape: &Shape, center: Vec3) -> (bool, Vec3) {
        let test = center - vec3(0.0, 0.02, 0.0);
        let t = Transform::from_translation(test);
        let mut best_y = -1.0f32;
        let mut normal = Vec3::ZERO;
        for (_, body) in world.bodies() {
            if let Some(m) = narrowphase::collide_shapes(shape, &t, &body.shape, &body.transform) {
                for cp in &m.points {
                    let n = -cp.normal;
                    if n.y > best_y && cp.penetration > 0.0 {
                        best_y = n.y;
                        normal = n;
                    }
                }
            }
        }
        (
            normal.length_squared() > 0.5 && normal.y >= self.slope_cos(),
            normal,
        )
    }

    /// 便捷方法：胶囊中心的世界位置（`Transform` 语义下的原点）。
    pub fn center_from_feet(&self, feet: Vec3) -> Vec3 {
        feet + vec3(0.0, self.height * 0.5, 0.0)
    }

    /// 胶囊轴半高与半径（供外部构造查询用）。
    pub fn half_extents(&self) -> (f32, f32) {
        (self.half_height(), self.radius.abs())
    }

    /// 用 GJK 点距离检查某点是否在角色胶囊内部（调试辅助）。
    pub fn contains_point(&self, center: Vec3, p: Vec3) -> bool {
        gjk::distance_point_to_shape(&self.capsule(), &Transform::from_translation(center), p)
            <= 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::BodyBuilder;

    fn flat_world() -> PhysicsWorld {
        let mut w = PhysicsWorld::new();
        w.add_body(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
                .position(Vec3::ZERO)
                .build(),
        );
        w
    }

    #[test]
    fn capsule_matches_dimensions() {
        let c = CharacterController {
            radius: 0.4,
            height: 1.8,
            ..Default::default()
        };
        match c.capsule() {
            Shape::Capsule {
                half_height,
                radius,
            } => {
                assert!((radius - 0.4).abs() < 1e-6);
                assert!((half_height - 0.5).abs() < 1e-6);
            }
            other => panic!("期望胶囊，得到 {other:?}"),
        }
        assert!((c.center_from_feet(Vec3::ZERO).y - 0.9).abs() < 1e-6);
    }

    #[test]
    fn walks_forward_on_flat_ground() {
        let mut w = flat_world();
        let c = CharacterController::default();
        let start = c.center_from_feet(Vec3::ZERO);
        let r = c.move_and_slide(&mut w, start, vec3(1.0, 0.0, 0.0));
        assert!(r.translation.x > 0.95, "应近似自由前进 {:?}", r.translation);
        assert!(r.grounded, "应判定为落地");
        assert!(r.ground_normal.y > 0.9);
    }

    #[test]
    fn blocked_by_wall_and_slides() {
        let mut w = flat_world();
        // 一堵墙：x = 2 处的薄盒。
        w.add_body(
            BodyBuilder::r#static(Shape::Box {
                half_extents: vec3(0.1, 1.0, 5.0),
            })
            .position(vec3(2.0, 0.0, 0.0))
            .build(),
        );
        let c = CharacterController {
            radius: 0.4,
            height: 1.8,
            ..Default::default()
        };
        let start = c.center_from_feet(Vec3::ZERO);
        let r = c.move_and_slide(&mut w, start, vec3(3.0, 0.0, -1.0));
        assert!(r.translation.x < 2.0, "不应穿过墙 {:?}", r.translation);
        assert!(r.translation.z < -0.5, "应沿墙滑动 {:?}", r.translation);
    }

    #[test]
    fn stands_on_ground_without_sinking() {
        let mut w = flat_world();
        let c = CharacterController::default();
        let start = c.center_from_feet(vec3(0.0, 0.0, 0.0));
        let r = c.move_and_slide(&mut w, start, Vec3::ZERO);
        assert!(r.grounded);
        // 不应穿过地面太多。
        assert!(r.translation.y > -0.1, "下沉过多 {:?}", r.translation);
    }

    #[test]
    fn nan_input_is_ignored() {
        let mut w = flat_world();
        let c = CharacterController::default();
        let start = c.center_from_feet(Vec3::ZERO);
        let r = c.move_and_slide(&mut w, start, Vec3::splat(f32::NAN));
        assert!(r.translation.is_finite());
    }
}
