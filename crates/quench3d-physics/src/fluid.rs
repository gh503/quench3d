//! CPU 粒子流体：Position Based Fluids（PBF，Macklin & Müller 2013）的最小可用实现。
//!
//! 设计边界（与 M1 的解析水域是两套东西，互不替换）：
//! * **CPU + 确定性**：所有容器按粒子下标顺序遍历，邻域用固定顺序的 27 个格子查找；
//!   同一输入在原生与 wasm 上都走同一段代码（浮点结果与平台有关，这是整个引擎的既有口径）。
//! * **双向耦合（M3）**：刚体是流体的边界，流体会被刚体推开；粒子撞到**动态**刚体时
//!   把法向冲量按动量守恒反推给刚体（静态刚体只挡粒子，不会动）。
//! * **边界**：对所有非传感器刚体做最近表面投影（球/盒/胶囊/圆柱/圆锥/圆角盒/半空间），
//!   `TriMesh` 逐三角形最近点；凸包用 GJK/EPA，圆锥用底面圆盘 + 侧面线段投影，
//!   都是精确最近表面（见 [`crate::Shape::closest_surface`]）。
//! * 没有表面张力、涡量约束、粘性应力张量；粘性用 XSPH 近似。
//! * **泡沫/飞沫是被动视觉粒子**（`FluidDesc.foam`，缺省关）：不参与 PBF 密度，
//!   不与刚体碰撞；只从高速度粒子按确定顺序甩出，受寿命/重力/阻尼。

use core::f32::consts::PI;
use std::collections::HashMap;

use quench3d_math::*;

use crate::world::BodySlot;

/// 浮点比较的兜底 epsilon。
const EPS: f32 = 1e-6;
/// 单个粒子的速度上限（m/s）：防止参数失控时飞出宇宙。
const MAX_SPEED: f32 = 40.0;
/// 估算自由表面至少需要的粒子数（少于此数就当作"这里没有水"）。
const MIN_SURFACE_PARTICLES: usize = 8;

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
    /// 表面张力/内聚力系数（0~5，0 = 关闭）。
    ///
    /// 用邻居方向加权把表面粒子往内拉，近似"水不想摊成薄膜"；不是 Young-Laplace
    /// 曲率模型，不能标定真实表面张力系数（mN/m）。
    pub surface_tension: f32,
    /// 涡量约束强度（0~5，0 = 关闭）。保留小尺度涡，减少 PBF 的过度耗散。
    pub vorticity: f32,
    /// 浸没刚体的线性阻尼（1/s），只对浮力部分生效；0 = 无阻尼。
    pub drag: f32,
    /// 初始速度（米/秒），缺省 0；给水柱一个喷射方向。
    pub initial_velocity: Vec3,
    /// 排水面高度：粒子 y 低于它就被移除（`None` = 不排水）。
    ///
    /// 用来做"无底水池"/开放边界，避免水一直掉到 -1e6 还在算。
    pub drain_y: Option<f32>,
    /// 连续发射速率（粒子/秒，0 = 不发射）。
    pub emit_rate: f32,
    /// 发射器中心。
    pub emit_position: Vec3,
    /// 发射器半径（米，XZ 圆盘内均匀撒点）。
    pub emit_radius: f32,
    /// 发射初速度（米/秒）。
    pub emit_velocity: Vec3,
    /// 发射速度随机扰动幅度（米/秒）。
    pub emit_spread: f32,
    /// 密度约束迭代次数（1~8）。
    pub iterations: u32,
    /// 粒子上限（超过就按晶格顺序截断）。
    pub max_particles: usize,
    /// 是否生成泡沫/飞沫（缺省 false；旧场景逐位不变）。
    ///
    /// 这是**视觉白水**，不是两相流：高速度粒子按确定顺序甩出被动泡沫粒子，
    /// 泡沫受重力/阻尼/寿命影响，不参与 PBF 密度，也不会反过来推水。
    pub foam: bool,
    /// 起泡速度阈值（m/s），缺省 3.0。
    pub foam_speed: f32,
    /// 泡沫生成速率（颗/秒），缺省 120。
    pub foam_rate: f32,
    /// 泡沫上限，缺省 256。
    pub max_foam: usize,
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
            surface_tension: 0.0,
            vorticity: 0.0,
            initial_velocity: Vec3::ZERO,
            drain_y: None,
            emit_rate: 0.0,
            emit_position: Vec3::ZERO,
            emit_radius: 0.1,
            emit_velocity: Vec3::ZERO,
            emit_spread: 0.0,
            drag: 6.0,
            iterations: 4,
            max_particles: 4000,
            foam: false,
            foam_speed: 3.0,
            foam_rate: 120.0,
            max_foam: 256,
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
            && self.surface_tension.is_finite()
            && (0.0..=5.0).contains(&self.surface_tension)
            && self.vorticity.is_finite()
            && (0.0..=5.0).contains(&self.vorticity)
            && self.initial_velocity.is_finite()
            && self.drain_y.is_none_or(|y| y.is_finite())
            && self.emit_rate.is_finite()
            && self.emit_rate >= 0.0
            && self.emit_position.is_finite()
            && self.emit_radius.is_finite()
            && self.emit_radius >= 0.0
            && self.emit_velocity.is_finite()
            && self.emit_spread.is_finite()
            && self.emit_spread >= 0.0
            && self.drag.is_finite()
            && self.drag >= 0.0
            && (1..=8).contains(&self.iterations)
            && (1..=20_000).contains(&self.max_particles)
            && self.foam_speed.is_finite()
            && self.foam_speed >= 0.0
            && self.foam_rate.is_finite()
            && self.foam_rate >= 0.0
            && self.max_foam <= 4096
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
    /// 本步与刚体的接触数（报告用，0 时序列化会省略）。
    pub contacts: usize,
    /// 当前泡沫/飞沫粒子数（0 时报告省略）。
    pub foam: usize,
}

/// 粒子与刚体的一个接触（求解最后一步记录，用于两-way 反冲）。
#[derive(Clone, Copy, Debug)]
struct Contact {
    /// 从刚体指向粒子的单位法线。
    normal: Vec3,
    /// 接触点的刚体速度（本步碰撞时采样）。
    velocity: Vec3,
    /// 世界坐标接触点。
    point: Vec3,
    /// 刚体在 `bodies` 里的下标。
    body: u32,
}

/// 泡沫/飞沫类型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoamKind {
    /// 飞沫：高速甩出，受完整重力。
    Spray,
    /// 泡沫：贴着水面，轻微上浮。
    Foam,
}

/// 一颗被动泡沫/飞沫粒子（只用于渲染，不参与 PBF 求解）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FoamParticle {
    /// 世界位置。
    pub position: Vec3,
    /// 世界速度。
    pub velocity: Vec3,
    /// 剩余寿命（秒）。
    pub life: f32,
    /// 类型。
    pub kind: FoamKind,
}

/// 一个粒子流体。
#[derive(Clone, Debug)]
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
    /// 每个粒子在本步撞到的最后一个边界（用于摩擦与两-way 反冲）。
    contacts: Vec<Option<Contact>>,
    /// PBF 位置修正的临时缓冲（跨步复用，热路径零分配）。
    delta: Vec<Vec3>,
    /// 速度更新的临时缓冲（跨步复用，热路径零分配）。
    new_vel: Vec<Vec3>,
    /// 涡量临时缓冲（只在场强 > 0 时用）。
    omega: Vec<Vec3>,
    /// 涡量梯度临时缓冲。
    eta: Vec<Vec3>,
    /// 位置/速度版本号：每步 +1；渲染端用它跳过未变化流体的表面重建。
    revision: u64,
    /// 粒子质量：按**晶格静止密度**校准（`rho0 / ΣW`），不是随便取 1。
    mass: f32,
    stats_contacts: usize,
    stats: FluidStats,
    spawned: bool,
    /// 连续发射的不足一颗的累积量。
    emit_accum: f32,
    /// 已经发射的粒子序号（用于确定性撒点）。
    emitted: u64,
    /// 被动泡沫/飞沫（视觉白水）。
    foam: Vec<FoamParticle>,
    /// 泡沫生成不足一颗的累积量。
    foam_accum: f32,
    /// 已经生成的泡沫序号（确定性轮转起点）。
    foam_emitted: u64,
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
            contacts: Vec::new(),
            delta: Vec::new(),
            new_vel: Vec::new(),
            omega: Vec::new(),
            eta: Vec::new(),
            revision: 0,
            mass: 1.0,
            stats_contacts: 0,
            stats: FluidStats::default(),
            spawned: false,
            emit_accum: 0.0,
            emitted: 0,
            foam: Vec::new(),
            foam_accum: 0.0,
            foam_emitted: 0,
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

    /// 当前泡沫/飞沫粒子（只读；最多 `desc.max_foam` 颗）。
    pub fn foam(&self) -> &[FoamParticle] {
        &self.foam
    }

    /// 用当前粒子提取连续表面（Surface Nets）。
    ///
    /// `resolution` 是最长轴的目标格数，`iso` 是等值面阈值；默认 `32 / 0.45`
    /// 适合 `spacing ≈ 0.1 m` 的水。返回网格在**世界坐标**，可直接上传渲染。
    pub fn surface_mesh(&self, resolution: u32, iso: f32) -> crate::FluidSurfaceMesh {
        crate::extract_surface(
            &self.positions,
            self.desc.smoothing_radius(),
            resolution,
            iso,
        )
    }

    /// 当前版本号（每次 `step` 自增）。
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// 外部（快照恢复）让缓存失效。
    pub(crate) fn bump_revision(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// 当前统计。
    pub fn stats(&self) -> FluidStats {
        self.stats
    }

    /// 刚体 XZ 范围附近的粒子表面高度（取最高粒子 y）。
    ///
    /// 这是 M3 给刚体算浮力用的**近似自由表面**：不是水平集，也不平滑，
    /// 飞溅的单个粒子会把高度抬高（所以要求至少 `MIN_SURFACE_PARTICLES` 颗）。
    pub fn surface_near(&self, aabb: Aabb, margin: f32) -> Option<f32> {
        if !aabb.is_valid() {
            return None;
        }
        let (min_x, max_x) = (aabb.min.x - margin, aabb.max.x + margin);
        let (min_z, max_z) = (aabb.min.z - margin, aabb.max.z + margin);
        let mut max_y = f32::NEG_INFINITY;
        let mut count = 0usize;
        for p in &self.positions {
            if p.x >= min_x && p.x <= max_x && p.z >= min_z && p.z <= max_z {
                max_y = max_y.max(p.y);
                count += 1;
            }
        }
        if count >= MIN_SURFACE_PARTICLES && max_y.is_finite() {
            Some(max_y)
        } else {
            None
        }
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
        self.velocities = vec![d.initial_velocity; self.positions.len()];
        self.mass = calibrated_mass(d);
        self.recompute_stats();
        self.spawned = true;
    }

    /// 连续发射：按 `emit_rate` 累积，最多补到 `max_particles` 上限。
    ///
    /// 撒点不用随机数：第 `k` 颗的半径/角度由黄金角与 k 的确定函数给出，
    /// 同一初始状态 + 同样步数逐位可复现。
    fn emit(&mut self, dt: f32) {
        let d = &self.desc;
        if d.emit_rate <= 0.0 || !dt.is_finite() || dt <= 0.0 {
            return;
        }
        if self.positions.len() >= d.max_particles {
            self.emit_accum = 0.0;
            return;
        }
        self.emit_accum += d.emit_rate * dt;
        let available = d.max_particles - self.positions.len();
        let n = (self.emit_accum.floor() as usize).min(available);
        if n == 0 {
            return;
        }
        self.emit_accum -= n as f32;
        for _ in 0..n {
            let k = self.emitted;
            self.emitted = self.emitted.wrapping_add(1);
            let phase = k as f32 * 2.399_963_1;
            // 半径用 sqrt 让圆盘面积均匀，而不是全挤在中心。
            let frac = ((k as f32 * 0.618_034).fract() + 1.0).fract();
            let r = d.emit_radius * frac.sqrt();
            let offset = Vec3::new(phase.cos() * r, 0.0, phase.sin() * r);
            let wobble = d.emit_spread
                * Vec3::new(
                    (phase * 1.7).sin(),
                    (phase * 2.3).cos() * 0.5,
                    (phase * 3.1).cos(),
                );
            self.positions.push(d.emit_position + offset);
            self.velocities.push(d.emit_velocity + wobble);
        }
        if self.positions.len() >= d.max_particles {
            self.emit_accum = 0.0;
        }
    }

    /// 推进一步（PBF：预测 → 密度约束 → 边界投影 → 速度更新 → XSPH 粘性）。
    fn step(&mut self, bodies: &mut [BodySlot], gravity: Vec3, dt: f32) {
        if !self.spawned {
            self.spawn();
        }
        // 发射要在 n == 0 的早退之前：水被排空后还能继续补水。
        self.emit(dt);
        let n = self.positions.len();
        if n == 0 {
            // 水排空后泡沫还要继续飞/淡出，不能在这里把统计和泡沫一起清掉。
            self.update_foam(gravity, dt);
            self.recompute_stats();
            self.revision = self.revision.wrapping_add(1);
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
        self.contacts.resize(n, None);
        self.contacts.fill(None);
        // 每次迭代后都做边界投影：只压不拉 + 碰撞放在迭代里面，
        // 粒子不会先陷进地面再被一次性弹出来（那是"炸开"的主要来源）。
        let iters = self.desc.iterations.max(1);
        for _ in 0..iters {
            self.update_density_lambda(mass, rho0, h, h2, poly6, spiky, w0);
            self.delta.clear();
            self.delta.resize(n, Vec3::ZERO);
            for (i, d) in self.delta.iter_mut().enumerate() {
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
            for (i, d) in self.delta.iter().enumerate() {
                let prev = self.predicted[i];
                let (p, contact) = collide_position(prev, prev + *d, &*bodies, radius);
                self.predicted[i] = p;
                if contact.is_some() {
                    self.contacts[i] = contact;
                }
            }
        }
        // 统计用的是**求解后**的密度（报告里"密度比"才有意义）。
        self.update_density_lambda(mass, rho0, h, h2, poly6, spiky, w0);

        // 6. 速度 = 位置差 / dt，然后处理边界相对速度
        let inv_dt = 1.0 / dt;
        self.new_vel.clear();
        self.new_vel.resize(n, Vec3::ZERO);
        for (i, out) in self.new_vel.iter_mut().enumerate() {
            let mut v = (self.predicted[i] - self.positions[i]) * inv_dt;
            if let Some(c) = self.contacts[i] {
                // 简单摩擦：法向速度清零，切向留 80%。
                let rel = v - c.velocity;
                let vn = rel.dot(c.normal);
                if vn < 0.0 {
                    v -= c.normal * vn;
                }
                let tangent = rel - c.normal * rel.dot(c.normal);
                v -= tangent * 0.2;
            }
            *out = v.clamp_length(MAX_SPEED);
        }

        // 7. XSPH 粘性（用邻居密度做归一化；没有邻居时保持原速度）
        if self.desc.viscosity > 0.0 {
            self.delta.clear();
            self.delta.resize(n, Vec3::ZERO);
            for (i, out) in self.delta.iter_mut().enumerate() {
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
                    sum += (self.new_vel[j] - self.new_vel[i]) * w;
                    wsum += w;
                }
                if wsum > 0.0 {
                    *out = sum * (self.desc.viscosity * mass / wsum.max(1e-9));
                }
            }
            for (i, d) in self.delta.iter().enumerate() {
                self.new_vel[i] = (self.new_vel[i] + *d).clamp_length(MAX_SPEED);
            }
        }

        // 7b. 表面张力（内聚力）：把粒子拉向邻居重心；只在显式开启时执行。
        if self.desc.surface_tension > 0.0 && dt > 0.0 {
            self.delta.clear();
            self.delta.resize(n, Vec3::ZERO);
            for (i, out) in self.delta.iter_mut().enumerate() {
                let mut sum = Vec3::ZERO;
                let mut count = 0.0f32;
                for &j in &self.neighbors[i] {
                    let j = j as usize;
                    let d = self.predicted[j] - self.predicted[i];
                    let r = d.length();
                    if r <= EPS || r >= h {
                        continue;
                    }
                    let w = 1.0 - r / h;
                    sum += d / r * (w * w);
                    count += 1.0;
                }
                if count > 0.0 {
                    *out = sum / count;
                }
            }
            let k = self.desc.surface_tension * 40.0 * dt;
            for (i, d) in self.delta.iter().enumerate() {
                self.new_vel[i] = (self.new_vel[i] + *d * k).clamp_length(MAX_SPEED);
            }
        }

        // 7c. 涡量约束：f = ε h (N × ω)，N = ∇|ω|；只在显式开启时执行。
        if self.desc.vorticity > 0.0 && dt > 0.0 {
            let inv_pi_h6 = 45.0 / (std::f32::consts::PI * h * h * h * h * h * h);
            self.omega.clear();
            self.omega.resize(n, Vec3::ZERO);
            for (i, out) in self.omega.iter_mut().enumerate() {
                let mut curl = Vec3::ZERO;
                for &j in &self.neighbors[i] {
                    let j = j as usize;
                    let d = self.predicted[i] - self.predicted[j];
                    let r = d.length();
                    if r <= EPS || r >= h {
                        continue;
                    }
                    let x = h - r;
                    let grad = d * (-inv_pi_h6 * x * x / r);
                    curl += (self.new_vel[j] - self.new_vel[i]).cross(grad);
                }
                *out = curl;
            }
            self.eta.clear();
            self.eta.resize(n, Vec3::ZERO);
            for (i, out) in self.eta.iter_mut().enumerate() {
                let mut eta = Vec3::ZERO;
                for &j in &self.neighbors[i] {
                    let j = j as usize;
                    let d = self.predicted[i] - self.predicted[j];
                    let r = d.length();
                    if r <= EPS || r >= h {
                        continue;
                    }
                    let x = h - r;
                    let grad = d * (-inv_pi_h6 * x * x / r);
                    eta += grad * (self.omega[j].length() - self.omega[i].length());
                }
                *out = eta.normalize_or_zero();
            }
            let scale = self.desc.vorticity * h;
            for (i, e) in self.eta.iter().enumerate() {
                // 约束力是**加速度**：先限幅再乘 dt。单帧相邻速度差很大时 `ω`
                // 可以到 1e3/s，不限幅会一帧把粒子打到 MAX_SPEED（实测飞出 70 m）。
                let accel = (e.cross(self.omega[i]) * scale).clamp_length(10.0);
                self.new_vel[i] = (self.new_vel[i] + accel * dt).clamp_length(MAX_SPEED);
            }
        }

        // 8. 两-way：把粒子撞到**动态**刚体上的法向冲量反推给刚体。
        //    冲量按动量守恒：粒子减 `J`、刚体加 `J`；`J = m_p · vn · n`
        //    （vn < 0 表示还在靠近，n 从刚体指向粒子，所以刚体被推向 -n）。
        let mut contact_count = 0usize;
        for (i, v) in self.new_vel.iter_mut().enumerate() {
            let Some(c) = self.contacts[i] else {
                continue;
            };
            contact_count += 1;
            let Some(slot) = bodies.get_mut(c.body as usize) else {
                continue;
            };
            let Some(body) = slot.body.as_mut() else {
                continue;
            };
            if !body.is_dynamic() {
                continue;
            }
            let vb = body.velocity_at_point(c.point);
            let vn = (*v - vb).dot(c.normal);
            if vn < 0.0 {
                // 只反推 20% 的法向动量：粒子的"压力"已由浮力项承担，
                // 全额反推会让成千上万颗粒子把刚体当鼓面敲（实测会把浮球砸进池底）。
                let impulse = c.normal * (self.mass * vn * 0.2);
                body.apply_impulse_at_point(impulse, c.point);
                // 粒子也拿到反冲；否则水会"穿过"轻的刚体。
                *v -= impulse / self.mass;
            }
        }
        self.stats_contacts = contact_count;

        std::mem::swap(&mut self.positions, &mut self.predicted);
        std::mem::swap(&mut self.velocities, &mut self.new_vel);
        // 开放排水边界：粒子穿过 `drain_y` 后移除（紧凑写回，保持顺序）。
        if let Some(y) = self.desc.drain_y {
            let mut keep = 0usize;
            for i in 0..self.positions.len() {
                if self.positions[i].y >= y {
                    self.positions[keep] = self.positions[i];
                    self.velocities[keep] = self.velocities[i];
                    keep += 1;
                }
            }
            self.positions.truncate(keep);
            self.velocities.truncate(keep);
        }
        self.spawn_foam(dt);
        self.update_foam(gravity, dt);
        self.recompute_stats();
        self.revision = self.revision.wrapping_add(1);
    }

    /// 从高速粒子甩出被动泡沫/飞沫；顺序由 `foam_emitted` 轮转，确定性。
    fn spawn_foam(&mut self, dt: f32) {
        let d = &self.desc;
        if !d.foam || d.foam_rate <= 0.0 || d.max_foam == 0 || !dt.is_finite() || dt <= 0.0 {
            self.foam_accum = 0.0;
            return;
        }
        if self.foam.len() >= d.max_foam {
            self.foam_accum = 0.0;
            return;
        }
        self.foam_accum += d.foam_rate * dt;
        let budget = (self.foam_accum.floor() as usize).min(d.max_foam - self.foam.len());
        if budget == 0 {
            return;
        }
        let count = self.positions.len();
        if count == 0 {
            return;
        }
        let start = (self.foam_emitted as usize) % count;
        let mut spawned = 0usize;
        for k in 0..count {
            if spawned >= budget {
                break;
            }
            let i = (start + k) % count;
            let speed = self.velocities[i].length();
            if !speed.is_finite() || speed < d.foam_speed {
                continue;
            }
            let mut v = self.velocities[i] * 0.35;
            v.y += 0.6 + speed * 0.06;
            if v.length() > MAX_SPEED {
                v = v.clamp_length(MAX_SPEED);
            }
            let life = 0.9 + (i % 5) as f32 * 0.1;
            let kind = if speed >= d.foam_speed * 2.0 {
                FoamKind::Spray
            } else {
                FoamKind::Foam
            };
            self.foam.push(FoamParticle {
                position: self.positions[i],
                velocity: v,
                life,
                kind,
            });
            self.foam_emitted = self.foam_emitted.wrapping_add(1);
            spawned += 1;
        }
        self.foam_accum -= spawned as f32;
    }

    /// 被动泡沫的寿命/重力/阻尼；不做碰撞，纯视觉白水。
    fn update_foam(&mut self, gravity: Vec3, dt: f32) {
        if self.foam.is_empty() || !dt.is_finite() || dt <= 0.0 {
            return;
        }
        let mut keep = 0usize;
        for index in 0..self.foam.len() {
            let mut f = self.foam[index];
            f.life -= dt;
            if f.life <= 0.0 {
                continue;
            }
            let g = match f.kind {
                FoamKind::Spray => gravity,
                FoamKind::Foam => gravity * 0.2 + Vec3::Y * 0.8,
            };
            f.velocity = (f.velocity + g * dt) * (1.0 - 2.2 * dt).clamp(0.0, 1.0);
            f.position += f.velocity * dt;
            if let Some(y) = self.desc.drain_y {
                if f.position.y < y {
                    continue;
                }
            }
            if !f.position.is_finite() || !f.velocity.is_finite() {
                continue;
            }
            self.foam[keep] = f;
            keep += 1;
        }
        self.foam.truncate(keep);
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
            self.stats = FluidStats {
                foam: self.foam.len(),
                ..FluidStats::default()
            };
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
            contacts: self.stats_contacts,
            foam: self.foam.len(),
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

/// 把粒子投影到所有刚体边界之外；返回修正后的位置与最后一个接触。
fn collide_position(
    prev: Vec3,
    mut p: Vec3,
    bodies: &[BodySlot],
    radius: f32,
) -> (Vec3, Option<Contact>) {
    let mut contact = None;
    // 位移可能比半径大（快速下落/薄网格）：AABB 拒绝测试要把整段位移算进去，
    // 否则一帧穿过 0 厚度的三角网时根本进不了碰撞循环。
    let swept = (p - prev).length();
    for (body_index, slot) in bodies.iter().enumerate() {
        let Some(body) = &slot.body else {
            continue;
        };
        if body.sensor {
            continue;
        }
        let aabb = body.world_aabb();
        // `Plane` 的 AABB 是无穷（无效值），不能拿它做拒绝测试。
        if aabb.is_valid()
            && !aabb.grown(radius + swept).contains_point(prev)
            && !aabb.grown(radius + swept).contains_point(p)
        {
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
            // `n` 由当前点所在的侧面决定。若上一步在另一侧（signed_prev < 0），
            // 而这一步已经过面到当前侧，就是扫掠穿透：把粒子推回**来的一侧**，
            // 而不是顺着 n 推到穿过去的那一侧（那会让它卡在网格背面/穿到地下）。
            let signed_prev = (prev - world.transform_point(surf)).dot(n);
            let crossed = signed_prev < -1e-5 && signed > -radius;
            if crossed {
                p = surf_world - n * radius;
                contact = Some(Contact {
                    normal: -n,
                    velocity: body.velocity_at_point(surf_world),
                    point: surf_world,
                    body: body_index as u32,
                });
            } else if signed < radius {
                p = surf_world + n * radius;
                contact = Some(Contact {
                    normal: n,
                    velocity: body.velocity_at_point(surf_world),
                    point: surf_world,
                    body: body_index as u32,
                });
            }
        }
    }
    (p, contact)
}

/// 推进所有流体。`bodies` 是当前子步的刚体（流体与动态刚体双向耦合）。
pub(crate) fn step_all(fluids: &mut [Fluid], bodies: &mut [BodySlot], gravity: Vec3, dt: f32) {
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

    fn forced_world(surface_tension: f32, vorticity: f32) -> PhysicsWorld {
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.add_builder(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(Vec3::ZERO),
        );
        world
            .add_fluid(FluidDesc {
                center: vec3(0.0, 1.2, 0.0),
                half_extents: vec3(0.3, 0.3, 0.3),
                spacing: 0.1,
                radius: 0.05,
                surface_tension,
                vorticity,
                ..FluidDesc::default()
            })
            .expect("参数合法");
        world
    }

    #[test]
    fn surface_tension_and_vorticity_are_finite_and_bounded() {
        for (st, vort) in [(1.0, 0.0), (0.0, 1.0), (0.8, 0.8)] {
            let mut world = forced_world(st, vort);
            for _ in 0..180 {
                world.step(1.0 / 60.0);
            }
            let s = world.fluids()[0].stats();
            assert!(s.particles > 0);
            assert!(s.avg_density_ratio.is_finite());
            assert!(
                s.max_speed.is_finite() && s.max_speed < 20.0,
                "最大速度 {}",
                s.max_speed
            );
            assert!(s.min_y > -0.1, "穿地：min_y = {}", s.min_y);
            assert!(s.max_y.is_finite());
        }
    }

    #[test]
    fn surface_tension_changes_a_non_symmetric_drop() {
        // 把最外层的一颗粒子推开，破坏对称性；开表面张力后它应被邻居拉回，
        // 至少一步后的位置/速度必须不同（关闭时逐位保持旧行为）。
        let mut a = Fluid::new(FluidDesc {
            center: vec3(0.0, 1.2, 0.0),
            half_extents: vec3(0.3, 0.3, 0.3),
            spacing: 0.1,
            radius: 0.05,
            ..FluidDesc::default()
        })
        .expect("参数合法");
        let mut b = Fluid::new(FluidDesc {
            center: vec3(0.0, 1.2, 0.0),
            half_extents: vec3(0.3, 0.3, 0.3),
            spacing: 0.1,
            radius: 0.05,
            surface_tension: 1.0,
            ..FluidDesc::default()
        })
        .expect("参数合法");
        a.positions[0] += Vec3::X * 0.05;
        b.positions[0] += Vec3::X * 0.05;
        a.step(&mut [], Vec3::NEG_Y * 9.81, 1.0 / 60.0);
        b.step(&mut [], Vec3::NEG_Y * 9.81, 1.0 / 60.0);
        // 表面张力改的是速度；位置要在**下一步**的预测里才体现。
        let changed_v = a
            .velocities()
            .iter()
            .zip(b.velocities())
            .any(|(p, q)| p != q);
        assert!(changed_v, "表面张力必须实际改变至少一个速度");
        a.step(&mut [], Vec3::NEG_Y * 9.81, 1.0 / 60.0);
        b.step(&mut [], Vec3::NEG_Y * 9.81, 1.0 / 60.0);
        let changed = a.particles().iter().zip(b.particles()).any(|(p, q)| p != q);
        assert!(changed, "表面张力必须实际改变下一步的位置");
    }

    #[test]
    fn initial_velocity_moves_the_jet_and_drain_removes_particles() {
        let mut jet = Fluid::new(FluidDesc {
            center: vec3(0.0, 2.0, 0.0),
            half_extents: vec3(0.2, 0.2, 0.2),
            spacing: 0.1,
            radius: 0.05,
            initial_velocity: vec3(2.0, 0.0, 0.0),
            ..FluidDesc::default()
        })
        .expect("参数合法");

        for _ in 0..30 {
            jet.step(&mut [], Vec3::NEG_Y * 9.81, 1.0 / 60.0);
        }
        let mean_x: f32 =
            jet.particles().iter().map(|p| p.x).sum::<f32>() / jet.particles().len() as f32;
        assert!(mean_x > 0.4, "初始速度必须推着水柱走，实际 mean_x={mean_x}");

        let mut drained = Fluid::new(FluidDesc {
            center: vec3(0.0, 1.0, 0.0),
            half_extents: vec3(0.2, 0.2, 0.2),
            spacing: 0.1,
            radius: 0.05,
            drain_y: Some(0.5),
            ..FluidDesc::default()
        })
        .expect("参数合法");
        let before = drained.particles().len();
        assert!(before > 0);
        for _ in 0..90 {
            drained.step(&mut [], Vec3::NEG_Y * 9.81, 1.0 / 60.0);
        }
        assert!(
            drained.particles().is_empty(),
            "排水面以下应清空，还剩 {} 颗",
            drained.particles().len()
        );
        assert_eq!(drained.particles().len(), drained.velocities().len());
    }

    #[test]
    fn emitter_adds_particles_deterministically_and_respects_cap() {
        let mut f = Fluid::new(FluidDesc {
            center: vec3(0.0, 1.0, 0.0),
            half_extents: vec3(0.1, 0.1, 0.1),
            spacing: 0.1,
            radius: 0.05,
            emit_rate: 120.0,
            emit_position: vec3(2.0, 1.0, 0.0),
            emit_radius: 0.2,
            emit_velocity: vec3(0.0, 1.0, 0.0),
            max_particles: 80,
            ..FluidDesc::default()
        })
        .expect("参数合法");
        let start = f.particles().len();
        for _ in 0..120 {
            f.step(&mut [], Vec3::NEG_Y * 9.81, 1.0 / 60.0);
        }
        assert!(
            f.particles().len() > start,
            "发射器必须补粒子：{} → {}",
            start,
            f.particles().len()
        );
        assert!(
            f.particles().len() <= 80,
            "不能超过 max_particles：{}",
            f.particles().len()
        );
        // 新粒子应出现在发射器附近（X 正方向），不是初始方块的位置。
        let emitted = f.particles().iter().filter(|p| p.x > 1.0).count();
        assert!(emitted > 20, "发射位置不对：只有 {emitted} 颗在 x>1");
        // 同样的参数再跑一次，粒子数必须逐位一致。
        let mut g = Fluid::new(FluidDesc {
            center: vec3(0.0, 1.0, 0.0),
            half_extents: vec3(0.1, 0.1, 0.1),
            spacing: 0.1,
            radius: 0.05,
            emit_rate: 120.0,
            emit_position: vec3(2.0, 1.0, 0.0),
            emit_radius: 0.2,
            emit_velocity: vec3(0.0, 1.0, 0.0),
            max_particles: 80,
            ..FluidDesc::default()
        })
        .expect("参数合法");
        for _ in 0..120 {
            g.step(&mut [], Vec3::NEG_Y * 9.81, 1.0 / 60.0);
        }
        assert_eq!(f.particles().len(), g.particles().len());
        assert_eq!(f.particles(), g.particles());
    }

    #[test]
    fn particles_slide_down_a_cone_side_not_its_aabb_top() {
        // 圆锥 h=1,r=1：x=0.8 处侧面在 y≈-0.6，而 AABB 顶面在 y=1。
        // 旧 AABB 近似会把粒子托在 y≈1；精确侧面应让它们沿锥面滑下。
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.add_builder(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(vec3(0.0, -1.2, 0.0)),
        );
        world.add_builder(BodyBuilder::r#static(Shape::Cone {
            half_height: 1.0,
            radius: 1.0,
        }));
        world.add_fluid(FluidDesc {
            center: vec3(0.8, 2.0, 0.0),
            half_extents: vec3(0.1, 0.1, 0.1),
            spacing: 0.1,
            radius: 0.05,
            max_particles: 64,
            ..FluidDesc::default()
        });
        for _ in 0..180 {
            world.step(1.0 / 60.0);
        }
        let s = world.fluids()[0].stats();
        assert!(
            s.min_y < 0.0,
            "粒子应沿锥面滑到 y<0，实际 min_y={}（AABB 会托在 1.0 附近）",
            s.min_y
        );
        assert!(s.max_speed < 4.0, "max_speed={}", s.max_speed);
    }

    #[test]
    fn particles_fall_past_a_convex_hull_aabb_corner() {
        // 四面体的 AABB 是 1×1×1，但在 (0.8, *, 0.8) 这条竖直线上四面体只到 y≈0。
        // 旧实现把凸包当 AABB，粒子会停在天花板 y≈1；精确最近表面应让它们落到地面。
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.add_builder(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(Vec3::ZERO),
        );
        world.add_builder(BodyBuilder::r#static(Shape::ConvexHull {
            points: vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z],
        }));
        world.add_fluid(FluidDesc {
            center: vec3(0.8, 2.0, 0.8),
            half_extents: vec3(0.1, 0.1, 0.1),
            spacing: 0.1,
            radius: 0.05,
            max_particles: 64,
            ..FluidDesc::default()
        });
        for _ in 0..180 {
            world.step(1.0 / 60.0);
        }
        let s = world.fluids()[0].stats();
        assert!(
            s.min_y < 0.3,
            "粒子应绕过凸包落到地面，实际 min_y={}（AABB 近似会停在 1.0 附近）",
            s.min_y
        );
        assert!(s.max_speed < 4.0, "max_speed={}", s.max_speed);
    }

    #[test]
    fn foam_spawns_from_a_fast_jet_and_expires_deterministically() {
        let desc = FluidDesc {
            center: vec3(0.0, 2.0, 0.0),
            half_extents: vec3(0.2, 0.2, 0.2),
            spacing: 0.1,
            radius: 0.05,
            initial_velocity: vec3(0.0, -12.0, 0.0),
            drain_y: Some(-2.0),
            foam: true,
            foam_speed: 2.0,
            foam_rate: 120.0,
            max_foam: 64,
            max_particles: 64,
            ..FluidDesc::default()
        };
        let mut a = Fluid::new(desc.clone()).expect("参数合法");
        let mut b = Fluid::new(desc).expect("参数合法");
        for _ in 0..20 {
            a.step(&mut [], Vec3::NEG_Y * 9.81, 1.0 / 60.0);
            b.step(&mut [], Vec3::NEG_Y * 9.81, 1.0 / 60.0);
        }
        assert!(!a.foam().is_empty(), "高速水流必须甩出泡沫");
        assert!(a.foam().len() <= 64, "泡沫超过上限：{}", a.foam().len());
        assert!(
            a.foam()
                .iter()
                .all(|f| f.position.is_finite() && f.velocity.is_finite() && f.life > 0.0),
            "泡沫位置/速度/寿命必须有限且为正"
        );
        assert_eq!(a.foam(), b.foam(), "泡沫生成/更新必须逐位确定");
        let spawned = a.foam().len();
        // 水从开放底排空后不再产生新泡沫；旧泡沫在约 1.5 秒内全部过期。
        for _ in 0..240 {
            a.step(&mut [], Vec3::NEG_Y * 9.81, 1.0 / 60.0);
        }
        assert!(
            a.foam().is_empty(),
            "水排空后泡沫应过期，仍有 {} 颗（曾生成 {spawned}）",
            a.foam().len()
        );
    }

    #[test]
    fn foam_flag_does_not_change_the_particle_solver() {
        let base = FluidDesc {
            center: vec3(0.0, 2.0, 0.0),
            half_extents: vec3(0.2, 0.2, 0.2),
            spacing: 0.1,
            radius: 0.05,
            initial_velocity: vec3(3.0, -8.0, 0.0),
            max_particles: 64,
            ..FluidDesc::default()
        };
        let mut off = Fluid::new(base.clone()).expect("参数合法");
        let mut on = Fluid::new(FluidDesc {
            foam: true,
            foam_speed: 1.0,
            foam_rate: 120.0,
            max_foam: 64,
            ..base
        })
        .expect("参数合法");
        for _ in 0..60 {
            off.step(&mut [], Vec3::NEG_Y * 9.81, 1.0 / 60.0);
            on.step(&mut [], Vec3::NEG_Y * 9.81, 1.0 / 60.0);
        }
        assert!(!on.foam().is_empty());
        assert_eq!(off.stats().foam, 0);
        assert_eq!(off.particles(), on.particles(), "泡沫不能改变 PBF 粒子位置");
        assert_eq!(
            off.velocities(),
            on.velocities(),
            "泡沫不能改变 PBF 粒子速度"
        );
    }

    #[test]
    fn particles_stop_on_a_static_trimesh_floor() {
        // 以前 `TriMesh::closest_surface` 返回 None，粒子会直接穿过网格；
        // 场景里用 `--collide` 导入的模型因此对流体完全透明。这里锁住薄壳碰撞。
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        let floor = vec![
            vec3(-4.0, 0.0, -4.0),
            vec3(4.0, 0.0, -4.0),
            vec3(4.0, 0.0, 4.0),
            vec3(-4.0, 0.0, 4.0),
        ];
        world.add_builder(BodyBuilder::r#static(Shape::TriMesh {
            vertices: floor,
            indices: vec![0, 1, 2, 0, 2, 3],
        }));
        world.add_fluid(box_desc()).expect("参数合法");
        for _ in 0..240 {
            world.step(1.0 / 60.0);
        }
        let s = world.fluids()[0].stats();
        assert!(s.min_y > -0.06, "流体穿过了静态三角网：min_y = {}", s.min_y);
        assert!(s.max_speed < 5.0, "max_speed = {}", s.max_speed);
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
    fn fluid_pushes_a_heavy_dynamic_body_downward() {
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.add_builder(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(Vec3::ZERO),
        );
        let handle = world.add_builder(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::splat(0.25),
            })
            .position(vec3(0.0, 1.0, 0.0))
            .density(3000.0),
        );
        world.add_fluid(FluidDesc {
            center: vec3(0.0, 2.2, 0.0),
            half_extents: Vec3::splat(0.35),
            spacing: 0.1,
            radius: 0.05,
            ..FluidDesc::default()
        });
        let start = world.body(handle).unwrap().transform.position;
        for _ in 0..120 {
            world.step(1.0 / 60.0);
        }
        let end = world.body(handle).unwrap().transform.position;
        assert!(
            end.y < start.y - 0.02,
            "水应该把刚体往下压：{start:?} -> {end:?}"
        );
        assert!(!world.body(handle).unwrap().is_sleeping());
    }

    #[test]
    fn fluid_wakes_a_sleeping_body_on_impact() {
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.add_builder(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(Vec3::ZERO),
        );
        let handle = world.add_builder(
            BodyBuilder::dynamic(Shape::Box {
                half_extents: Vec3::splat(0.25),
            })
            .position(vec3(0.0, 1.0, 0.0))
            .density(200.0)
            .gravity_scale(0.0),
        );
        world.body_mut(handle).unwrap().sleep();
        assert!(world.body(handle).unwrap().is_sleeping());
        world.add_fluid(FluidDesc {
            center: vec3(0.0, 2.0, 0.0),
            half_extents: Vec3::splat(0.3),
            spacing: 0.1,
            radius: 0.05,
            ..FluidDesc::default()
        });
        for _ in 0..90 {
            world.step(1.0 / 60.0);
        }
        assert!(
            !world.body(handle).unwrap().is_sleeping(),
            "粒子撞上来必须把睡眠体叫醒，否则水会穿过它"
        );
    }

    #[test]
    fn fluid_buoyancy_keeps_a_light_body_afloat() {
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.add_builder(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(Vec3::ZERO),
        );
        let handle = world.add_builder(
            BodyBuilder::dynamic(Shape::Sphere { radius: 0.25 })
                .position(vec3(0.0, 1.2, 0.0))
                .density(300.0),
        );
        world.add_fluid(FluidDesc {
            center: vec3(0.0, 1.2, 0.0),
            half_extents: Vec3::splat(0.6),
            spacing: 0.1,
            radius: 0.05,
            max_particles: 2000,
            ..FluidDesc::default()
        });
        for _ in 0..240 {
            world.step(1.0 / 60.0);
        }
        let b = world.body(handle).unwrap();
        assert!(
            b.transform.position.y > 0.5,
            "300 kg/m³ 的球应该浮着，实际 {:?}",
            b.transform.position
        );
        assert!(!b.is_sleeping());
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
