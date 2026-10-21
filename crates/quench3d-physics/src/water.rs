//! 水域与浮力（M1：解析水域 + 采样浸没体积）。
//!
//! 这里**不是**流体求解器：没有粒子、没有对流、没有自由表面。它只回答一个问题：
//! 「一个刚体此刻排开了多少水、形心在哪」。浮力/阻尼由 [`crate::PhysicsWorld`]
//! 在每个子步施加，因此窗口、CLI、编辑器、Web 走的是同一套代码。
//!
//! ## 为什么用采样
//!
//! 任意凸形状 + 任意朝向 + 水平面，闭式解要按形状分十几种情况（球冠、圆柱斜切、
//! 圆锥斜切……），每一种都容易写错，而且复合刚体还要把各部件的结果拼起来。
//! 这里在**部件本地 AABB 的均匀网格**上采样，再变换到世界判断是否在水下：
//!
//! * 对每个形状只用 [`Shape::contains_local`] 一个谓词，新增形状自动支持；
//! * 体积是 `AABB 体积 × 命中比例`，是无偏估计（不是拿形状体积去乘比例）；
//! * 形心是命中样本的世界坐标均值，浮力施力点用它，所以有扶正力矩。
//!
//! 代价是精度与采样数：每轴 [`GRID`] 个样本，误差约几个百分点（球体实测
//! < 5%），对"船浮起来""球落水"足够；对需要精确排水量的场景，后续可换解析式。
//! 采样是**确定性**的（固定网格、固定顺序），不引入随机数。

use quench3d_math::{vec3, Vec2, Vec3};

use crate::body::RigidBody;
use crate::shape::Shape;

/// 每个轴上的采样数（总共 `GRID³` 个样本 / 部件）。
pub(crate) const GRID: usize = 10;

/// 一块水域：水平面 `level`、XZ 范围 `center ± half_size` 的**半空间**。
///
/// 没有池底：`y <= level` 且落在 XZ 范围内就算浸没。要表达"池底"就把地面
/// 放在水下——刚体会先被地面挡住，浮力不会把它拽穿。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaterVolume {
    /// XZ 中心 `(x, z)`。
    pub center: Vec2,
    /// XZ 半尺寸。
    pub half_size: Vec2,
    /// 水面高度（世界 Y）。
    pub level: f32,
    /// 密度（kg/m³）。水 1000。
    pub density: f32,
    /// 线性阻尼系数（1/s），只对浸没部分生效。0 = 无阻尼。
    pub drag: f32,
}

impl WaterVolume {
    /// 按中心/半尺寸/水面高度构造，密度 1000、阻尼 6。
    pub fn new(center: Vec2, half_size: Vec2, level: f32) -> Self {
        Self {
            center,
            half_size,
            level,
            density: 1000.0,
            drag: 6.0,
        }
    }

    /// 点是否浸没（水面以下 + XZ 范围内）。
    #[inline]
    pub fn contains(&self, p: Vec3) -> bool {
        p.y <= self.level
            && (p.x - self.center.x).abs() <= self.half_size.x
            && (p.z - self.center.y).abs() <= self.half_size.y
    }

    /// 参数是否可用（构建器/校验器之外的最后一道闸）。
    pub fn is_valid(&self) -> bool {
        self.center.is_finite()
            && self.half_size.is_finite()
            && self.half_size.x > 0.0
            && self.half_size.y > 0.0
            && self.level.is_finite()
            && self.density.is_finite()
            && self.density > 0.0
            && self.drag.is_finite()
            && self.drag >= 0.0
    }
}

/// 一个刚体在**一块**水域里的浸没结果。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SubmergedVolume {
    /// 排开体积（m³）。
    pub volume: f32,
    /// 排开体积的形心（世界坐标）；`volume == 0` 时无意义。
    pub centroid: Vec3,
}

impl SubmergedVolume {
    /// 是否浸没了一点点（浮点容差）。
    #[inline]
    pub fn is_submerged(&self) -> bool {
        self.volume > 1e-9
    }
}

/// 刚体在单块水域中的浸没体积与形心。
///
/// 只对动态体调用；静态/运动学体没有浮力（运动学体的位移由上层决定，
/// 给它浮力会与"按位置驱动"打架）。
pub(crate) fn submerged_in(body: &RigidBody, water: &WaterVolume) -> SubmergedVolume {
    let whole = body.world_aabb();
    if !whole.is_valid() {
        return SubmergedVolume::default();
    }
    // 快路径：整体在水面以上 / 水域 XZ 之外。
    if whole.min.y > water.level
        || whole.max.x < water.center.x - water.half_size.x
        || whole.min.x > water.center.x + water.half_size.x
        || whole.max.z < water.center.y - water.half_size.y
        || whole.min.z > water.center.y + water.half_size.y
    {
        return SubmergedVolume::default();
    }

    let mut volume = 0.0f32;
    let mut moment = Vec3::ZERO;
    let samples = (GRID * GRID * GRID) as f32;

    for (shape, local) in body.parts() {
        let aabb = shape.local_aabb();
        if !aabb.is_valid() {
            continue;
        }
        let ext = aabb.max - aabb.min;
        if !ext.is_finite() || ext.min_element() <= 0.0 {
            continue;
        }
        // 采样网格对**整个 AABB** 均匀，因此每格体积就是这个数；
        // 体积 = 每格体积 × 命中格数（无偏），而不是"形状体积 × 比例"。
        let cell = aabb.volume() / samples;
        if !cell.is_finite() || cell <= 0.0 {
            continue;
        }
        let world = body.part_world_transform(&local);
        let step = ext / GRID as f32;
        let mut hits = 0u32;
        let mut sum = Vec3::ZERO;
        for i in 0..GRID {
            let x = aabb.min.x + (i as f32 + 0.5) * step.x;
            for j in 0..GRID {
                let y = aabb.min.y + (j as f32 + 0.5) * step.y;
                for k in 0..GRID {
                    let z = aabb.min.z + (k as f32 + 0.5) * step.z;
                    let p = vec3(x, y, z);
                    if !shape.contains_local(p) {
                        continue;
                    }
                    let w = world.transform_point(p);
                    if water.contains(w) {
                        hits += 1;
                        sum += w;
                    }
                }
            }
        }
        if hits == 0 {
            continue;
        }
        let v = cell * hits as f32;
        volume += v;
        moment += (sum / hits as f32) * v;
    }

    if volume <= 0.0 || !volume.is_finite() {
        return SubmergedVolume::default();
    }
    SubmergedVolume {
        volume,
        centroid: moment / volume,
    }
}

/// 刚体的总（解析）体积，用于算浸没占比。
pub(crate) fn body_volume(body: &RigidBody) -> f32 {
    body.parts()
        .map(|(shape, _)| match shape {
            Shape::Plane { .. } | Shape::TriMesh { .. } => 0.0,
            other => other.volume(),
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BodyBuilder, PhysicsWorld, Shape};
    use quench3d_math::vec3;

    fn sphere_body(world: &mut PhysicsWorld, y: f32) -> crate::BodyHandle {
        world.add_builder(
            BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
                .position(vec3(0.0, y, 0.0))
                .density(500.0),
        )
    }

    #[test]
    fn water_contains_respects_bounds_and_level() {
        let w = WaterVolume::new(Vec2::new(0.0, 0.0), Vec2::new(2.0, 1.0), 0.5);
        assert!(w.contains(vec3(0.0, 0.5, 0.0)));
        assert!(w.contains(vec3(1.9, -3.0, 0.9)));
        assert!(!w.contains(vec3(0.0, 0.51, 0.0)));
        assert!(!w.contains(vec3(2.1, 0.0, 0.0)));
        assert!(!w.contains(vec3(0.0, 0.0, 1.1)));
    }

    #[test]
    fn half_submerged_sphere_reports_half_its_volume() {
        let mut world = PhysicsWorld::new();
        // 球心刚好在水面上：正好排开半个球。
        let h = sphere_body(&mut world, 0.0);
        let body = world.body(h).unwrap();
        let water = WaterVolume::new(Vec2::ZERO, Vec2::new(4.0, 4.0), 0.0);
        let s = submerged_in(body, &water);
        let full = 4.0 / 3.0 * std::f32::consts::PI * 0.5f32.powi(3);
        assert!(
            (s.volume / full - 0.5).abs() < 0.06,
            "命中比例应接近 0.5，实际 {}",
            s.volume / full
        );
        // 形心在水面以下：球心是 0，半球的形心约在 -0.25。
        assert!(
            s.centroid.y < 0.0 && s.centroid.y > -0.4,
            "{}",
            s.centroid.y
        );
    }

    #[test]
    fn fully_above_water_has_no_buoyancy() {
        let mut world = PhysicsWorld::new();
        let h = sphere_body(&mut world, 5.0);
        let body = world.body(h).unwrap();
        let water = WaterVolume::new(Vec2::ZERO, Vec2::new(4.0, 4.0), 0.0);
        assert_eq!(submerged_in(body, &water), SubmergedVolume::default());
    }

    #[test]
    fn fully_submerged_box_reports_its_volume() {
        let mut world = PhysicsWorld::new();
        let h = world.add_builder(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: vec3(0.5, 0.5, 0.5),
            })
            .position(vec3(0.0, -2.0, 0.0)),
        );
        let body = world.body(h).unwrap();
        let water = WaterVolume::new(Vec2::ZERO, Vec2::new(4.0, 4.0), 0.0);
        let s = submerged_in(body, &water);
        assert!((s.volume - 1.0).abs() < 1e-4, "{}", s.volume);
        assert!((s.centroid - vec3(0.0, -2.0, 0.0)).length() < 1e-4);
    }

    #[test]
    fn half_density_sphere_floats_at_the_surface() {
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.add_water(WaterVolume::new(Vec2::ZERO, Vec2::new(4.0, 4.0), 0.0));
        let h = world.add_builder(
            BodyBuilder::dynamic(Shape::Sphere { radius: 0.5 })
                .position(vec3(0.0, 2.0, 0.0))
                // 密度是水的一半 → 平衡时排开一半体积，球心停在水面。
                .density(500.0)
                .linear_damping(0.0)
                .angular_damping(0.0),
        );
        for _ in 0..900 {
            world.step(1.0 / 60.0);
        }
        let body = world.body(h).unwrap();
        let y = body.transform.position.y;
        assert!(y.abs() < 0.12, "球心应停在水面附近，实际 y = {y}");
        assert!(
            (body.submerged_fraction() - 0.5).abs() < 0.1,
            "浸没占比应接近 0.5，实际 {}",
            body.submerged_fraction()
        );
        assert!(
            body.is_sleeping(),
            "漂浮物应该能睡着：浮力是环境力，不该每一步都把它唤醒"
        );
    }

    #[test]
    fn dense_body_sinks_and_rests_on_the_ground() {
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.add_water(WaterVolume::new(Vec2::ZERO, Vec2::new(4.0, 4.0), 0.0));
        world.add_builder(
            BodyBuilder::r#static(Shape::Plane {
                normal: vec3(0.0, 1.0, 0.0),
            })
            .position(vec3(0.0, -2.0, 0.0)),
        );
        let h = world.add_builder(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: vec3(0.25, 0.25, 0.25),
            })
            .position(vec3(0.0, 1.0, 0.0))
            .density(2000.0),
        );
        for _ in 0..600 {
            world.step(1.0 / 60.0);
        }
        let body = world.body(h).unwrap();
        assert!(
            body.transform.position.y < -0.3,
            "{}",
            body.transform.position.y
        );
        assert!((body.submerged_fraction() - 1.0).abs() < 1e-3);
    }

    #[test]
    fn buoyancy_is_deterministic() {
        let run = || {
            let mut world = PhysicsWorld::new();
            world.set_fixed_dt(1.0 / 120.0);
            world.add_water(WaterVolume::new(Vec2::ZERO, Vec2::new(3.0, 3.0), 0.0));
            let h = world.add_builder(
                BodyBuilder::dynamic(Shape::Box {
                    half_extents: vec3(0.4, 0.2, 0.3),
                })
                .position(vec3(0.3, 1.0, -0.2))
                .rotation(quench3d_math::Quat::from_rotation_y(0.4))
                .density(600.0),
            );
            for _ in 0..240 {
                world.step(1.0 / 120.0);
            }
            let b = world.body(h).unwrap();
            (
                b.transform.position,
                b.transform.rotation,
                b.linear_velocity,
            )
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn body_volume_sums_parts() {
        let mut world = PhysicsWorld::new();
        let h = sphere_body(&mut world, 0.0);
        let body = world.body(h).unwrap();
        let v = body_volume(body);
        let expect = 4.0 / 3.0 * std::f32::consts::PI * 0.125;
        assert!((v - expect).abs() < 1e-5);
    }
}
