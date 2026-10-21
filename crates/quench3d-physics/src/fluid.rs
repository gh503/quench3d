//! CPU 粒子流体：Position Based Fluids（PBF，Macklin & Müller 2013）的最小可用实现。
//!
//! 设计边界（与 M1 的解析水域是两套东西，互不替换）：
//! * **CPU + 确定性**：所有容器按粒子下标顺序遍历，邻域用固定顺序的 27 个格子查找；
//!   同一输入在原生与 wasm 上都走同一段代码（浮点结果与平台有关，这是整个引擎的既有口径）。
//! * **单向耦合（M2）**：刚体是流体的边界，流体会被刚体推开；流体**还不会**反推刚体。
//!   双向耦合是 M3。
//! * **边界**：对所有非传感器刚体做最近表面投影（球/盒/胶囊/圆柱/圆锥/圆角盒/半空间），
//!   `TriMesh` 与凸包按 AABB 近似（见 [`crate::Shape::closest_surface`]）。
//! * 没有表面张力、涡量约束、粘性应力张量；粘性用 XSPH 近似。

use core::f32::consts::PI;
use std::collections::HashMap;

use quench3d_math::*;

use crate::world::BodySlot;

/// 浮点比较的兜底 epsilon。
const EPS: f32 = 1e-6;
/// 单个粒子的速度上限（m/s）：防止参数失控时飞出宇宙。
const MAX_SPEED: f32 = 40.0;

/// 流体参数（场景格式的 `fluids[]` 一项）。
#[derive(Clone, Debug, PartialEq)]
pub struct FluidDesc {
    /// 发射体（长方体）中心。
    pub center: Vec3,
    /// 发射体半尺寸（米）。
    pub half_extents: Vec3,
    /// 粒子初始间距（米）。
    pub spacing: f32,
    /// 粒子碰撞半径（米）；缺省是 `spacing * 0.5`。
    pub radius: f32,
    /// 静止密度（kg/m³）。
    pub rest_density: f32,
    /// XSPH 粘性系数（0~1）。
    pub viscosity: f32,
    /// 密度约束迭代次数（1~8）。
    pub iterations: u32,
    /// 粒子上限（超过就按晶格顺序截断）。
    pub max_particles: usize,
}

impl Default for FluidDesc {
    fn default() -> Self {
        Self {
            center: Vec3::ZERO,
            half_extents: Vec3::splat(1.0),
            spacing: 0.1,
            radius: 0.05,
            rest_density: 1000.0,
            viscosity: 0.05,
            iterations: 4,
            max_particles: 4000,
        }
    }
}

impl FluidDesc {
    /// 参数是否可用（与 `validate` 的口径一致）。
    pub fn is_valid(&self) -> bool {
        let he = self.half_extents;
        self.center.is_finite()
            && he.is_finite()
            && he.x > 0.0
            && he.y > 0.0
            && he.z > 0.0
            && self.spacing.is_finite()
            && self.spacing > 0.0
            && self.spacing <= he.min_element() * 2.0
            && self.radius.is_finite()
            && self.radius > 0.0
            && self.radius <= self.spacing
            && self.rest_density.is_finite()
            && self.rest_density > 0.0
            && self.viscosity.is_finite()
            && (0.0..=1.0).contains(&self.viscosity)
            && (1..=8).contains(&self.iterations)
            && (1..=20_000).contains(&self.max_particles)
    }

    /// 按晶格估算的粒子数（已按 `max_particles` 截断）。
    pub fn estimated_particles(&self) -> usize {
        let n = |half: f32| ((half * 2.0 / self.spacing).floor() as usize).saturating_add(1);
        n(self.half_extents.x)
            .saturating_mul(n(self.half_extents.y))
            .saturating_mul(n(self.half_extents.z))
            .min(self.max_particles)
    }

    /// 平滑核半径：两倍间距（粒子直径的一倍）。
    pub fn smoothing_radius(&self) -> f32 {
        self.spacing * 2.0
    }
}

/// 单个流体的只读统计（报告用）。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FluidStats {
    /// 粒子数。
    pub particles: usize,
    /// 最大速度（m/s）。
    pub max_speed: f32,
    /// 平均密度比 `rho / rho0`（1 = 静止密度）。
    pub avg_density_ratio: f32,
    /// 最低粒子 y。
    pub min_y: f32,
    /// 最高粒子 y。
    pub max_y: f32,
}

/// 一个粒子流体。
pub struct Fluid {
    desc: FluidDesc,
    positions: Vec<Vec3>,
    velocities: Vec<Vec3>,
    /// 本步预测位置（PBF 的位置迭代变量）。
    predicted: Vec<Vec3>,
    densities: Vec<f32>,
    lambdas: Vec<f32>,
    /// 每个粒子的邻居下标（按粒子顺序；跨步复用，避免每步分配）。
    neighbors: Vec<Vec<u32>>,
    /// 每个粒子在本步撞到的最后一个边界的速度（用于相对速度摩擦）。
    contact_velocity: Vec<Vec3>,
    /// 每个粒子在本步撞到的最后一个边界的法线。
    contact_normal: Vec<Vec3>,
    /// 粒子质量：按**晶格静止密度**校准（`rho0 / ΣW`），不是随便取 1。
    mass: f32,
    stats: FluidStats,
    spawned: bool,
}

impl Fluid {
    /// 用参数创建一个**已经发射**的流体（粒子按晶格排布）。
    pub fn new(desc: FluidDesc) -> Option<Self> {
        if !desc.is_valid() {
            return None;
        }
        let mut fluid = Self {
            desc,
            positions: Vec::new(),
            velocities: Vec::new(),
            predicted: Vec::new(),
            densities: Vec::new(),
            lambdas: Vec::new(),
            neighbors: Vec::new(),
            contact_velocity: Vec::new(),
            contact_normal: Vec::new(),
            mass: 1.0,
            stats: FluidStats::default(),
            spawned: false,
        };
        fluid.spawn();
        Some(fluid)
    }

    /// 参数（只读）。
    pub fn desc(&self) -> &FluidDesc {
        &self.desc
    }

    /// 粒子位置（只读）。
    pub fn particles(&self) -> &[Vec3] {
        &self.positions
    }

    /// 粒子速度（只读）。
    pub fn velocities(&self) -> &[Vec3] {
        &self.velocities
    }

    /// 当前统计。
    pub fn stats(&self) -> FluidStats {
        self.stats
    }

    /// 按参数里的长方体晶格发射粒子（确定性：固定下标顺序 + 固定方向）。
    fn spawn(&mut self) {
        let d = &self.desc;
        let n = |half: f32| ((half * 2.0 / d.spacing).floor() as usize).saturating_add(1);
        let (nx, ny, nz) = (
            n(d.half_extents.x),
            n(d.half_extents.y),
            n(d.half_extents.z),
        );
        let mut positions = Vec::with_capacity(nx * ny * nz);
        'outer: for iz in 0..nz {
            for iy in 0..ny {
                for ix in 0..nx {
                    if positions.len() >= d.max_particles {
                        break 'outer;
                    }
                    let p = d.center - d.half_extents
                        + Vec3::new(
                            ix as f32 * d.spacing,
                            iy as f32 * d.spacing,
                            iz as f32 * d.spacing,
                        );
                    positions.push(p);
                }
            }
        }
        self.positions = positions;
        self.velocities = vec![Vec3::ZERO; self.positions.len()];
        self.mass = calibrated_mass(d);
        self.recompute_stats();
        self.spawned = true;
    }

    /// 推进一步（PBF：预测 → 密度约束 → 边界投影 → 速度更新 → XSPH 粘性）。
    fn step(&mut self, bodies: &[BodySlot], gravity: Vec3, dt: f32) {
        if !self.spawned {
            self.spawn();
        }
        let n = self.positions.len();
        if n == 0 {
            self.stats = FluidStats::default();
            return;
        }
        let h = self.desc.smoothing_radius();
        let h2 = h * h;
        let mass = self.mass;
        let rho0 = self.desc.rest_density;
        let poly6 = 315.0 / (64.0 * PI * h.powi(9));
        let spiky = -45.0 / (PI * h.powi(6));
        let w0 = poly6 * h2 * h2 * h2;

        // 1. 重力 + 预测
        self.predicted.resize(n, Vec3::ZERO);
        for i in 0..n {
            let v = (self.velocities[i] + gravity * dt).clamp_length(MAX_SPEED);
            self.velocities[i] = v;
            self.predicted[i] = self.positions[i] + v * dt;
        }

        // 2. 空间哈希（固定 27 格顺序；map 只用于查找，不遍历）
        let mut grid: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::new();
        let cell = |p: Vec3| -> (i32, i32, i32) {
            (
                (p.x / h).floor() as i32,
                (p.y / h).floor() as i32,
                (p.z / h).floor() as i32,
            )
        };
        for (i, p) in self.predicted.iter().enumerate() {
            grid.entry(cell(*p)).or_default().push(i as u32);
        }
        self.neighbors.resize_with(n, Vec::new);
        for v in &mut self.neighbors {
            v.clear();
        }
        for i in 0..n {
            let pi = self.predicted[i];
            let (cx, cy, cz) = cell(pi);
            for dz in -1..=1 {
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        if let Some(list) = grid.get(&(cx + dx, cy + dy, cz + dz)) {
                            for &j in list {
                                let j = j as usize;
                                if j == i {
                                    continue;
                                }
                                if (pi - self.predicted[j]).length_squared() < h2 {
                                    self.neighbors[i].push(j as u32);
                                }
                            }
                        }
                    }
                }
            }
        }

        // 3. 密度与 lambda：PBF 的 lambda 必须**每次迭代重算**。
        //    只算一次然后反复加同一个 delta 会过冲（实测：无重力也会自己炸开）。
        self.densities.resize(n, 0.0);
        self.lambdas.resize(n, 0.0);

        // 4. 位置修正（迭代；每次用当前 predicted 重算邻域？邻域固定在预测位置，
        //    这是 PBF 的标准做法：迭代内只更新位置/密度，不重建邻居表）。
        let radius = self.desc.radius;
        self.contact_velocity.resize(n, Vec3::ZERO);
        self.contact_normal.resize(n, Vec3::ZERO);
        self.contact_velocity.fill(Vec3::ZERO);
        self.contact_normal.fill(Vec3::ZERO);
        // 每次迭代后都做边界投影：只压不拉 + 碰撞放在迭代里面，
        // 粒子不会先陷进地面再被一次性弹出来（那是"炸开"的主要来源）。
        let iters = self.desc.iterations.max(1);
        for _ in 0..iters {
            self.update_density_lambda(mass, rho0, h, h2, poly6, spiky, w0);
            let mut delta = vec![Vec3::ZERO; n];
            for (i, d) in delta.iter_mut().enumerate() {
                let mut dp = Vec3::ZERO;
                for &j in &self.neighbors[i] {
                    let j = j as usize;
                    let r = self.predicted[i] - self.predicted[j];
                    let len = r.length();
                    if len <= EPS || len >= h {
                        continue;
                    }
                    let x = h - len;
                    let w = spiky * x * x / len;
                    dp += r * (w * (self.lambdas[i] + self.lambdas[j]));
                }
                *d = (dp / rho0).clamp_length(0.1 * h);
            }
            for (i, d) in delta.iter().enumerate() {
                let (p, contact) = collide_position(self.predicted[i] + *d, bodies, radius);
                self.predicted[i] = p;
                if let Some((n, cv)) = contact {
                    self.contact_normal[i] = n;
                    self.contact_velocity[i] = cv;
                }
            }
        }
        // 统计用的是**求解后**的密度（报告里"密度比"才有意义）。
        self.update_density_lambda(mass, rho0, h, h2, poly6, spiky, w0);

        // 6. 速度 = 位置差 / dt，然后处理边界相对速度
        let inv_dt = 1.0 / dt;
        let mut new_vel = vec![Vec3::ZERO; n];
        for (i, out) in new_vel.iter_mut().enumerate() {
            let mut v = (self.predicted[i] - self.positions[i]) * inv_dt;
            let n = self.contact_normal[i];
            if n.length_squared() > 0.5 {
                // 简单摩擦：法向速度清零，切向留 80%。
                let rel = v - self.contact_velocity[i];
                let vn = rel.dot(n);
                if vn < 0.0 {
                    v -= n * vn;
                }
                let tangent = rel - n * rel.dot(n);
                v -= tangent * 0.2;
            }
            *out = v.clamp_length(MAX_SPEED);
        }

        // 7. XSPH 粘性（用邻居密度做归一化；没有邻居时保持原速度）
        if self.desc.viscosity > 0.0 {
            let mut delta = vec![Vec3::ZERO; n];
            for i in 0..n {
                let mut sum = Vec3::ZERO;
                let mut wsum = 0.0f32;
                for &j in &self.neighbors[i] {
                    let j = j as usize;
                    let r2 = (self.predicted[i] - self.predicted[j]).length_squared();
                    if r2 >= h2 {
                        continue;
                    }
                    let x = h2 - r2;
                    let w = poly6 * x * x * x;
                    sum += (new_vel[j] - new_vel[i]) * w;
                    wsum += w;
                }
                if wsum > 0.0 {
                    delta[i] = sum * (self.desc.viscosity * mass / wsum.max(1e-9));
                }
            }
            for i in 0..n {
                new_vel[i] = (new_vel[i] + delta[i]).clamp_length(MAX_SPEED);
            }
        }

        std::mem::swap(&mut self.positions, &mut self.predicted);
        self.velocities = new_vel;
        self.recompute_stats();
    }

    /// 用当前 `predicted` 位置重算密度与 lambda（每次 PBF 迭代前调用）。
    #[allow(clippy::too_many_arguments)]
    fn update_density_lambda(
        &mut self,
        mass: f32,
        rho0: f32,
        h: f32,
        h2: f32,
        poly6: f32,
        spiky: f32,
        w0: f32,
    ) {
        let n = self.predicted.len();
        for i in 0..n {
            let mut rho = w0;
            for &j in &self.neighbors[i] {
                let r2 = (self.predicted[i] - self.predicted[j as usize]).length_squared();
                if r2 < h2 {
                    let x = h2 - r2;
                    rho += poly6 * x * x * x;
                }
            }
            let rho = rho * mass;
            self.densities[i] = rho;
            // 只压不拉：负压（吸引）会引发张力不稳定（实测：无重力也会自己"沸腾"）。
            // 初始质量已按晶格静止密度校准，所以静止时 C≈0，不会被压塌。
            let c = (rho / rho0 - 1.0).clamp(0.0, 10.0);
            if c < 0.01 {
                self.lambdas[i] = 0.0;
                continue;
            }
            let mut sum_grad2 = 0.0f32;
            for &j in &self.neighbors[i] {
                let r = self.predicted[i] - self.predicted[j as usize];
                let len = r.length();
                if len > EPS && len < h {
                    let x = h - len;
                    let g = spiky * x * x / len;
                    let g = r * g * (mass / rho0);
                    sum_grad2 += g.length_squared();
                }
            }
            self.lambdas[i] = -c / (sum_grad2 + 1e-4);
        }
    }

    fn recompute_stats(&mut self) {
        let n = self.positions.len();
        if n == 0 {
            self.stats = FluidStats::default();
            return;
        }
        let mut max_speed = 0.0f32;
        let mut density_sum = 0.0f32;
        let mut min_y = f32::INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for i in 0..n {
            let p = self.positions[i];
            max_speed = max_speed.max(self.velocities[i].length());
            density_sum += if self.densities.len() > i {
                self.densities[i] / self.desc.rest_density
            } else {
                1.0
            };
            min_y = min_y.min(p.y);
            max_y = max_y.max(p.y);
        }
        self.stats = FluidStats {
            particles: n,
            max_speed,
            avg_density_ratio: density_sum / n as f32,
            min_y,
            max_y,
        };
    }
}

/// 按晶格静止密度校准粒子质量：`m = rho0 / ΣW`。
///
/// 不校准的话，`m = rho0·spacing³` 会得到 0.5~0.9 倍的静止密度，粒子在
/// "只压不拉"的 PBF 里没有压力、直接互相穿过，最后摊成一层。
fn calibrated_mass(desc: &FluidDesc) -> f32 {
    let h = desc.smoothing_radius();
    let h2 = h * h;
    let poly6 = 315.0 / (64.0 * PI * h.powi(9));
    let w0 = poly6 * h2 * h2 * h2;
    let k = (h / desc.spacing).floor() as i32;
    let mut sum = w0;
    for iz in -k..=k {
        for iy in -k..=k {
            for ix in -k..=k {
                if ix == 0 && iy == 0 && iz == 0 {
                    continue;
                }
                let r2 = (ix * ix + iy * iy + iz * iz) as f32 * desc.spacing * desc.spacing;
                if r2 < h2 {
                    let x = h2 - r2;
                    sum += poly6 * x * x * x;
                }
            }
        }
    }
    if sum.is_finite() && sum > 0.0 {
        desc.rest_density / sum
    } else {
        desc.rest_density * desc.spacing.powi(3)
    }
}

/// 把粒子投影到所有刚体边界之外；返回修正后的位置与最后一个接触（法线, 边界速度）。
fn collide_position(mut p: Vec3, bodies: &[BodySlot], radius: f32) -> (Vec3, Option<(Vec3, Vec3)>) {
    let mut contact = None;
    for slot in bodies {
        let Some(body) = &slot.body else {
            continue;
        };
        if body.sensor {
            continue;
        }
        let aabb = body.world_aabb();
        // `Plane` 的 AABB 是无穷（无效值），不能拿它做拒绝测试。
        if aabb.is_valid() && !aabb.grown(radius).contains_point(p) {
            continue;
        }
        for (shape, local) in body.parts() {
            let world = body.part_world_transform(&local);
            let lp = world.inverse_transform_point(p);
            let Some((surf, n_local)) = shape.closest_surface(lp) else {
                continue;
            };
            let n = world.rotation.mul_vec3(n_local).normalize_or_zero();
            if n.length_squared() < 0.5 {
                continue;
            }
            let surf_world = world.transform_point(surf);
            let signed = (p - surf_world).dot(n);
            if signed < radius {
                p = surf_world + n * radius;
                contact = Some((n, body.velocity_at_point(surf_world)));
            }
        }
    }
    (p, contact)
}

/// 推进所有流体。`bodies` 是当前子步的刚体（流体单向受它们约束）。
pub(crate) fn step_all(fluids: &mut [Fluid], bodies: &[BodySlot], gravity: Vec3, dt: f32) {
    if dt <= 0.0 || !dt.is_finite() {
        return;
    }
    for fluid in fluids {
        fluid.step(bodies, gravity, dt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::BodyBuilder;
    use crate::shape::Shape;
    use crate::world::PhysicsWorld;

    fn box_desc() -> FluidDesc {
        FluidDesc {
            center: vec3(0.0, 1.0, 0.0),
            half_extents: vec3(0.3, 0.3, 0.3),
            spacing: 0.1,
            radius: 0.05,
            ..FluidDesc::default()
        }
    }

    fn settled_world() -> PhysicsWorld {
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.add_builder(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(Vec3::ZERO),
        );
        world.add_fluid(box_desc()).expect("参数合法");
        world
    }

    #[test]
    fn fluid_spawns_a_deterministic_lattice() {
        let d = box_desc();
        assert_eq!(d.estimated_particles(), 7 * 7 * 7);
        let a = Fluid::new(d.clone()).unwrap();
        let b = Fluid::new(d).unwrap();
        assert_eq!(a.particles(), b.particles());
        assert_eq!(a.particles().len(), 343);
        assert!(a.particles().iter().all(|p| p.y > 0.0));
    }

    #[test]
    fn invalid_parameters_are_rejected() {
        let mut d = box_desc();
        d.spacing = 0.0;
        assert!(Fluid::new(d).is_none());
        let mut d = box_desc();
        d.radius = 1.0; // 比间距大
        assert!(Fluid::new(d).is_none());
        let mut d = box_desc();
        d.iterations = 0;
        assert!(Fluid::new(d).is_none());
        let mut d = box_desc();
        d.max_particles = 0;
        assert!(Fluid::new(d).is_none());
    }

    #[test]
    fn particles_fall_and_stop_on_the_ground() {
        let mut world = settled_world();
        for _ in 0..240 {
            world.step(1.0 / 60.0);
        }
        let fluid = &world.fluids()[0];
        let s = fluid.stats();
        assert_eq!(s.particles, 343);
        assert!(s.min_y.is_finite() && s.max_y.is_finite());
        // 地面在 y=0，粒子半径 0.05：最低点不应该穿到地面以下太多。
        assert!(s.min_y > -0.06, "min_y = {}", s.min_y);
        assert!(s.max_speed < 5.0, "max_speed = {}", s.max_speed);
        assert!(s.avg_density_ratio.is_finite());
    }

    #[test]
    fn pool_holds_a_three_dimensional_pile() {
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.add_builder(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(Vec3::ZERO),
        );
        for (x, z, hx, hz) in [
            (0.9, 0.0, 0.1, 0.9),
            (-0.9, 0.0, 0.1, 0.9),
            (0.0, 0.9, 0.9, 0.1),
            (0.0, -0.9, 0.9, 0.1),
        ] {
            world.add_builder(
                BodyBuilder::r#static(Shape::Box {
                    half_extents: vec3(hx, 0.5, hz),
                })
                .position(vec3(x, 0.5, z)),
            );
        }
        world.add_fluid(FluidDesc {
            center: vec3(0.0, 1.2, 0.0),
            half_extents: vec3(0.6, 0.6, 0.6),
            spacing: 0.1,
            radius: 0.05,
            ..FluidDesc::default()
        });
        for _ in 0..360 {
            world.step(1.0 / 60.0);
        }
        let s = world.fluids()[0].stats();
        assert!(s.min_y > 0.03, "min_y = {}", s.min_y);
        assert!(
            s.max_y > 0.25,
            "max_y = {}（应该堆起来，不是摊成一层）",
            s.max_y
        );
        assert!(s.max_speed < 3.0, "max_speed = {}", s.max_speed);
    }

    #[test]
    fn fluid_is_deterministic() {
        let mut a = settled_world();
        let mut b = settled_world();
        for _ in 0..120 {
            a.step(1.0 / 60.0);
            b.step(1.0 / 60.0);
        }
        let pa = a.fluids()[0].particles();
        let pb = b.fluids()[0].particles();
        assert_eq!(pa.len(), pb.len());
        assert_eq!(pa, pb, "同输入必须逐位相同");
    }

    #[test]
    fn wall_and_ground_keep_particles_out() {
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.add_builder(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(Vec3::ZERO),
        );
        world.add_builder(
            BodyBuilder::r#static(Shape::Box {
                half_extents: Vec3::splat(0.5),
            })
            .position(vec3(0.0, 0.5, 0.0)),
        );
        world.add_fluid(FluidDesc {
            center: vec3(0.0, 2.0, 0.0),
            half_extents: vec3(0.2, 0.2, 0.2),
            ..box_desc()
        });
        for _ in 0..180 {
            world.step(1.0 / 60.0);
        }
        let s = world.fluids()[0].stats();
        assert!(s.max_speed.is_finite());
        // 地面在 y=0、粒子半径 0.05：最低点不应穿到地面以下太多。
        assert!(s.min_y > -0.06, "min_y = {}", s.min_y);
    }
}
