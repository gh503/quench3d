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
//! * **接触摩擦**（`contact_friction`，缺省关）：粒子与刚体之间的切向库仑摩擦；
//!   静置接触用重力法向支持力近似载荷。
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

/// Velocity policy for fluid contacts with rigid boundaries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FluidBoundaryVelocityMode {
    /// Preserve historical use of stored rigid-body velocities.
    #[default]
    Legacy,
    /// Static boundaries have zero velocity; other body types retain point velocity.
    BodyMotion,
}

/// 流体参数（场景格式的 `fluids[]` 一项）。
#[derive(Clone, Debug, PartialEq)]
pub struct FluidDesc {
    /// 发射体（长方体）中心。
    pub center: Vec3,
    /// 发射体半尺寸（米）。
    pub half_extents: Vec3,
    /// 可选的初始粒子世界坐标，按输入顺序保留，不应用 center 偏移。
    /// 必须非空、全部有限且不超过 max_particles；None 使用原长方体晶格。
    /// spacing 仍决定核半径和粒子质量，其他参数约束保持不变。
    pub initial_positions: Option<Vec<Vec3>>,
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
    /// 粒子流对刚体的速度耦合（1/s，缺省 0 = 关闭，旧路径逐位不变）。
    ///
    /// 用刚体 AABB 内粒子的平均速度作为局部流速，把浸没部分的速度拖向它；
    /// 这是**局部流场拖曳**，不是粒子对刚体的逐点无滑移边界，也不是两相流。
    pub flow_coupling: f32,
    /// Boundary velocity policy; Legacy preserves existing scenes.
    pub boundary_velocity_mode: FluidBoundaryVelocityMode,
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
    /// 粒子间最小分离（缺省 false = 只靠 PBF 密度约束，旧场景逐位不变）。
    ///
    /// 打开后，每步 PBF 位置迭代结束后把距离小于 `2 * radius` 的邻居沿连线推开
    /// （各一半），再重新投影到刚体边界。PBF 本身已防止压缩，这一层额外保证
    /// 自由表面/低迭代次数下粒子不会互相重叠；代价是每步一次邻居遍历。
    pub particle_collision: bool,
    /// 流体-刚体接触摩擦系数（0~1，缺省 0 = 关闭，旧场景逐位不变）。
    ///
    /// 只作用于粒子碰到动态/静态刚体时的**切向**相对速度；法向冲量仍按原逻辑
    /// 反推。打开后水会拖着刚体走，也会被刚体表面拖慢。
    pub contact_friction: f32,
    /// 是否模拟气泡/气相（缺省 false；旧场景逐位不变）。
    ///
    /// 气泡是**轻质气相粒子**：受反向重力浮升、与刚体/地面碰撞、把液体粒子
    /// 从气泡体积里推开，到液面/寿命结束消失。它把局部气体体积分数耦合进 PBF
    /// 密度约束（`bubble_density_coupling`），并可选地施加气泡尺度的过压
    /// （`bubble_pressure`）；但没有独立气相动量方程/相变，所以仍**不是**
    /// 严格两相流；它会排开液体并对液体做功。
    pub bubbles: bool,
    /// 气泡生成速率（0~5000 颗/秒），缺省 20。
    pub bubble_rate: f32,
    /// 气泡上限（0~4096），缺省 128。
    pub max_bubbles: usize,
    /// 气泡半径（(0, 0.5] 米），缺省 0.03。
    pub bubble_radius: f32,
    /// 浮升加速度相对重力的倍数（0~10），缺省 1.8。
    pub bubble_buoyancy: f32,
    /// 气泡阻尼（0~50，1/s），缺省 2.0。
    pub bubble_drag: f32,
    /// 气泡半径随上升膨胀的速率（0~10，1/s），缺省 0 = 不膨胀。
    ///
    /// 这是简化压力模型：没有真正解气泡内压/表面张力，只让气泡在上升时按指数
    /// 长大（体积分数随之增大）；半径封顶到初始值的 4 倍。
    pub bubble_expansion: f32,
    /// 气泡破碎半径上限（0~1 米，0 = 关闭）。大于它的气泡按体积守恒拆成多颗；
    /// 受 `max_bubbles` 限制，放不下时保留原气泡。缺省 0，旧路径逐位不变。
    pub bubble_max_radius: f32,
    /// 气泡尾流强度（0~2，缺省 0 = 关闭）：上浮气泡把上方液体向上拖，
    /// 形成羽流；关闭时完全走旧路径。
    pub bubble_wake: f32,
    /// 静水压压缩系数（0~10，1/m，0 = 不压缩）。深处气泡按 `r = r_surface / (1 + k·depth)^(1/3)`
    /// 收缩，近似 `pV = const`；缺省 0，旧路径逐位不变。
    pub bubble_compressibility: f32,
    /// 气泡过压（Pa，缺省 0 = 关闭）。按 `p = p0·(r_rest/r)³` 把气泡内压转成
    /// 对周围液体粒子的径向压力加速度 `a = p/(ρ·r)`，压力在气泡边缘最大、
    /// 到 `reach` 处衰减为零。这是**气泡尺度的压力耦合**，不是独立气相动量方程，
    /// 所以仍不等同于严格两相流；0 时完全不执行，旧场景逐位不变。
    pub bubble_pressure: f32,
    /// 气泡-液体动量反冲系数（0~2，缺省 0 = 关闭，旧路径逐位不变）。
    ///
    /// 上面的径向排开/过压/尾流都会给液体注入动量；这个字段把液体获得的
    /// 总动量按 0.5·ρ·V（气泡附加质量）反推给气泡，近似两相动量守恒。不是
    /// 独立气相质量/相变；0 时完全走旧路径。
    pub bubble_momentum_coupling: f32,
    /// 气泡是否参与 PBF 密度约束（缺省开；只在 `bubbles`/手动注入气泡时生效）。
    ///
    /// 开：气泡占据的局部体积把有效静止密度降为 `rho0 * (1 - gas)`，液体在
    /// 气泡周围膨胀；关：只保留原来的径向排开，密度约束视为纯液相。旧场景
    /// （没有气泡）两条路径都逐位不变。
    pub bubble_density_coupling: bool,
}

impl Default for FluidDesc {
    fn default() -> Self {
        Self {
            center: Vec3::ZERO,
            half_extents: Vec3::splat(1.0),
            initial_positions: None,
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
            flow_coupling: 0.0,
            boundary_velocity_mode: FluidBoundaryVelocityMode::Legacy,
            iterations: 4,
            max_particles: 4000,
            foam: false,
            foam_speed: 3.0,
            foam_rate: 120.0,
            max_foam: 256,
            particle_collision: false,
            contact_friction: 0.0,
            bubbles: false,
            bubble_rate: 20.0,
            max_bubbles: 128,
            bubble_radius: 0.03,
            bubble_buoyancy: 1.8,
            bubble_drag: 2.0,
            bubble_expansion: 0.0,
            bubble_max_radius: 0.0,
            bubble_wake: 0.0,
            bubble_compressibility: 0.0,
            bubble_pressure: 0.0,
            bubble_momentum_coupling: 0.0,
            bubble_density_coupling: true,
        }
    }
}

impl FluidDesc {
    /// 参数是否可用（与 `validate` 的口径一致）。
    pub fn is_valid(&self) -> bool {
        let he = self.half_extents;
        self.center.is_finite()
            && self.initial_positions.as_ref().is_none_or(|points| {
                !points.is_empty()
                    && points.len() <= self.max_particles
                    && points.iter().all(|p| p.is_finite())
            })
            && he.is_finite()
            && (self.center - he).is_finite()
            && (self.center + he).is_finite()
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
            && self.flow_coupling.is_finite()
            && self.flow_coupling >= 0.0
            && (1..=8).contains(&self.iterations)
            && (1..=20_000).contains(&self.max_particles)
            && self.foam_speed.is_finite()
            && self.foam_speed >= 0.0
            && self.foam_rate.is_finite()
            && self.foam_rate >= 0.0
            && self.max_foam <= 4096
            && self.contact_friction.is_finite()
            && (0.0..=1.0).contains(&self.contact_friction)
            && self.bubble_rate.is_finite()
            && (0.0..=5000.0).contains(&self.bubble_rate)
            && self.max_bubbles <= 4096
            && self.bubble_radius.is_finite()
            && self.bubble_radius > 0.0
            && self.bubble_radius <= 0.5
            && self.bubble_buoyancy.is_finite()
            && (0.0..=10.0).contains(&self.bubble_buoyancy)
            && self.bubble_drag.is_finite()
            && (0.0..=50.0).contains(&self.bubble_drag)
            && self.bubble_expansion.is_finite()
            && (0.0..=10.0).contains(&self.bubble_expansion)
            && self.bubble_compressibility.is_finite()
            && (0.0..=10.0).contains(&self.bubble_compressibility)
            && self.bubble_max_radius.is_finite()
            && (0.0..=1.0).contains(&self.bubble_max_radius)
            && self.bubble_wake.is_finite()
            && (0.0..=2.0).contains(&self.bubble_wake)
            && self.bubble_pressure.is_finite()
            && (0.0..=1_000_000.0).contains(&self.bubble_pressure)
            && self.bubble_momentum_coupling.is_finite()
            && (0.0..=2.0).contains(&self.bubble_momentum_coupling)
    }

    /// 按晶格估算的粒子数（已按 `max_particles` 截断）。
    pub fn estimated_particles(&self) -> usize {
        if let Some(points) = &self.initial_positions {
            return points.len();
        }
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

/// 流体邻域查询用的稠密网格（每步按预测位置重建一次）。
///
/// 旧实现每颗粒子查 27 个 `HashMap` 格子；对 4000+ 粒子，SipHash 和
/// `Vec` 分配会占掉整步的明显比例。这里把格子映射到扁平 `heads`，链表
/// 节点复用，构建一次、查询零分配；格子坐标与旧的 `p/h` 取整完全一致，
/// 格子内按原始下标升序串起来，因此邻居顺序与旧路径相同。
struct FluidGrid {
    cell: f32,
    origin: [i32; 3],
    dims: [i32; 3],
    heads: Vec<i32>,
    next: Vec<i32>,
}

impl FluidGrid {
    /// 格子总数上限；超过时退回旧哈希路径（极端大跨度点云）。
    const MAX_CELLS: usize = 2_000_000;

    fn new(points: &[Vec3], kernel: f32) -> Option<Self> {
        if !(kernel.is_finite() && kernel > 0.0) || points.is_empty() {
            return None;
        }
        let cell = kernel;
        let mut min = [i32::MAX; 3];
        let mut max = [i32::MIN; 3];
        for p in points {
            let c = fluid_abs_cell(*p, cell);
            for axis in 0..3 {
                min[axis] = min[axis].min(c[axis]);
                max[axis] = max[axis].max(c[axis]);
            }
        }
        let dims = [
            (max[0] - min[0] + 1).max(1),
            (max[1] - min[1] + 1).max(1),
            (max[2] - min[2] + 1).max(1),
        ];
        let cells = dims[0] as usize * dims[1] as usize * dims[2] as usize;
        if cells == 0 || cells > Self::MAX_CELLS {
            return None;
        }
        let mut heads = vec![-1i32; cells];
        let mut next = vec![-1i32; points.len()];
        // 反向插入，让每个格子的链表按原始下标升序；与旧 `Vec` 的追加顺序一致。
        for i in (0..points.len()).rev() {
            let c = fluid_abs_cell(points[i], cell);
            let local = [c[0] - min[0], c[1] - min[1], c[2] - min[2]];
            let slot = (local[0] + dims[0] * (local[1] + dims[1] * local[2])) as usize;
            next[i] = heads[slot];
            heads[slot] = i as i32;
        }
        Some(Self {
            cell,
            origin: min,
            dims,
            heads,
            next,
        })
    }

    /// 向 `out` 追加与 `i` 距离小于核半径的邻居下标（不包含自己）。
    fn neighbors(&self, points: &[Vec3], i: usize, kernel2: f32, out: &mut Vec<u32>) {
        let c = fluid_abs_cell(points[i], self.cell);
        for dz in -1..=1 {
            let z = c[2] + dz - self.origin[2];
            if z < 0 || z >= self.dims[2] {
                continue;
            }
            for dy in -1..=1 {
                let y = c[1] + dy - self.origin[1];
                if y < 0 || y >= self.dims[1] {
                    continue;
                }
                for dx in -1..=1 {
                    let x = c[0] + dx - self.origin[0];
                    if x < 0 || x >= self.dims[0] {
                        continue;
                    }
                    let slot = (x + self.dims[0] * (y + self.dims[1] * z)) as usize;
                    let mut j = self.heads[slot];
                    while j >= 0 {
                        let index = j as usize;
                        if index != i && (points[index] - points[i]).length_squared() < kernel2 {
                            out.push(index as u32);
                        }
                        j = self.next[index];
                    }
                }
            }
        }
    }
}

fn fluid_abs_cell(p: Vec3, cell: f32) -> [i32; 3] {
    [
        (p.x / cell).floor() as i32,
        (p.y / cell).floor() as i32,
        (p.z / cell).floor() as i32,
    ]
}

/// 单个粒子区间的密度与 lambda 计算（并行/串行共用）。
///
/// 每颗粒子只读邻域/预测位置/气体分数，只写自己的 `density` 与 `lambda`；
/// 区间之间没有共享写，所以按区间分给多个线程与串行**逐位相同**。
#[allow(clippy::too_many_arguments)]
fn density_lambda_range(
    predicted: &[Vec3],
    neighbors: &[Vec<u32>],
    gas_fractions: &[f32],
    mass: f32,
    rho0: f32,
    h: f32,
    h2: f32,
    poly6: f32,
    spiky: f32,
    w0: f32,
    start: usize,
    densities: &mut [f32],
    lambdas: &mut [f32],
) {
    for (offset, (density, lambda)) in densities.iter_mut().zip(lambdas.iter_mut()).enumerate() {
        let i = start + offset;
        let mut rho = w0;
        for &j in &neighbors[i] {
            let r2 = (predicted[i] - predicted[j as usize]).length_squared();
            if r2 < h2 {
                let x = h2 - r2;
                rho += poly6 * x * x * x;
            }
        }
        let rho = rho * mass;
        *density = rho;
        let rho0_i = if gas_fractions.is_empty() {
            rho0
        } else {
            rho0 * (1.0 - gas_fractions.get(i).copied().unwrap_or(0.0).clamp(0.0, 0.9))
        };
        let c = (rho / rho0_i - 1.0).clamp(0.0, 10.0);
        if c < 0.01 {
            *lambda = 0.0;
            continue;
        }
        let mut sum_grad2 = 0.0f32;
        for &j in &neighbors[i] {
            let r = predicted[i] - predicted[j as usize];
            let len = r.length();
            if len > EPS && len < h {
                let x = h - len;
                let g = spiky * x * x / len;
                let g = r * g * (mass / rho0_i);
                sum_grad2 += g.length_squared();
            }
        }
        *lambda = -c / (sum_grad2 + 1e-4);
    }
}

/// 极端大跨度时的旧哈希回退；邻居顺序与旧实现一致。
fn build_neighbors_hash(predicted: &[Vec3], h: f32, h2: f32, neighbors: &mut [Vec<u32>]) {
    let mut grid: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::new();
    let cell = |p: Vec3| -> (i32, i32, i32) {
        (
            (p.x / h).floor() as i32,
            (p.y / h).floor() as i32,
            (p.z / h).floor() as i32,
        )
    };
    for (i, p) in predicted.iter().enumerate() {
        grid.entry(cell(*p)).or_default().push(i as u32);
    }
    for (i, out) in neighbors.iter_mut().enumerate() {
        let pi = predicted[i];
        let (cx, cy, cz) = cell(pi);
        for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if let Some(list) = grid.get(&(cx + dx, cy + dy, cz + dz)) {
                        for &j in list {
                            let j = j as usize;
                            if j != i && (pi - predicted[j]).length_squared() < h2 {
                                out.push(j as u32);
                            }
                        }
                    }
                }
            }
        }
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
    /// 当前气泡数（0 时报告省略）。
    pub bubbles: usize,
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

/// 一颗气泡/气相粒子。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bubble {
    /// 世界位置。
    pub position: Vec3,
    /// 速度。
    pub velocity: Vec3,
    /// 当前半径（米，已含静水压压缩）。
    pub radius: f32,
    /// 无压缩时的表面半径（米）；`bubble_expansion` 改这个值，合并时按体积守恒。
    pub rest_radius: f32,
    /// 已存活时间。
    pub age: f32,
    /// 寿命；到点后消失。
    pub life: f32,
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
    /// 气泡/气相粒子。
    bubbles: Vec<Bubble>,
    /// 每个液体粒子所在处的气体体积分数（0 = 纯液体，只在密度耦合开启时非空）。
    gas_fractions: Vec<f32>,
    /// 气泡生成不足一颗的累积量。
    bubble_accum: f32,
    /// 已经生成的气泡序号（确定性轮转起点）。
    bubble_emitted: u64,
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
            bubbles: Vec::new(),
            gas_fractions: Vec::new(),
            bubble_accum: 0.0,
            bubble_emitted: 0,
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

    /// 与 `aabb` 相交粒子的平均速度（局部流场，m/s）。
    ///
    /// 没有粒子时返回 `None`（不能把"没有流体"误当成零流速阻尼）；非有限速度
    /// 被跳过。平均是**粒子数加权**，不是体积加权。
    pub fn flow_velocity_near(&self, aabb: Aabb) -> Option<Vec3> {
        let mut sum = Vec3::ZERO;
        let mut n = 0u32;
        for (p, v) in self.positions.iter().zip(self.velocities.iter()) {
            if aabb.contains_point(*p) && v.is_finite() {
                sum += *v;
                n += 1;
            }
        }
        if n == 0 {
            None
        } else {
            Some(sum / n as f32)
        }
    }

    /// 当前泡沫/飞沫粒子（只读；最多 `desc.max_foam` 颗）。
    pub fn foam(&self) -> &[FoamParticle] {
        &self.foam
    }

    /// 当前气泡/气相粒子（只读；最多 `desc.max_bubbles` 颗）。
    pub fn bubbles(&self) -> &[Bubble] {
        &self.bubbles
    }

    /// 注入一颗气泡（测试/工具用；正常路径是 `bubbles = true` 连续生成）。
    ///
    /// 不检查 `max_bubbles`，方便用一颗气泡做受控实验；正式生成仍受上限约束。
    pub fn add_bubble(&mut self, bubble: Bubble) {
        self.bubbles.push(bubble);
    }

    /// 与包围盒重叠的气泡总体积（m³）。用于把局部气体体积分数折算进浮力。
    pub fn gas_volume_near(&self, aabb: Aabb) -> f32 {
        if !aabb.is_valid() {
            return 0.0;
        }
        let mut volume = 0.0f32;
        for b in &self.bubbles {
            let r = b.radius.max(0.0);
            if r <= 0.0 || !b.radius.is_finite() || !b.position.is_finite() {
                continue;
            }
            // 气泡被液体推到刚体表面时中心正好在 `aabb + r` 上（浮点会差一点），
            // 留 1 cm 余量把“贴着表面”的气泡算进局部含气量。
            if aabb.grown(r + 0.01).contains_point(b.position) {
                volume += 4.0 / 3.0 * std::f32::consts::PI * r * r * r;
            }
        }
        if volume.is_finite() {
            volume.max(0.0)
        } else {
            0.0
        }
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

    /// 按世界坐标点集或长方体晶格初始化粒子（确定性：保留固定输入/晶格顺序）。
    fn spawn(&mut self) {
        let d = &self.desc;
        let positions = if let Some(points) = &d.initial_positions {
            points.clone()
        } else {
            let n = |half: f32| ((half * 2.0 / d.spacing).floor() as usize).saturating_add(1);
            let (nx, ny, nz) = (
                n(d.half_extents.x),
                n(d.half_extents.y),
                n(d.half_extents.z),
            );
            let mut positions = Vec::with_capacity(d.estimated_particles());
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
            positions
        };
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

    /// 粒子间最小分离：把距离小于 `2 * radius` 的邻居沿连线各推开一半。
    ///
    /// 邻居表固定在本步预测位置，这里只做一次确定性遍历；推开后重新投影到
    /// 刚体边界，避免把粒子推进墙里。只在 `particle_collision` 打开时调用。
    fn separate_particles(&mut self, radius: f32, bodies: &[BodySlot]) {
        let min_dist = 2.0 * radius;
        if min_dist <= 0.0 {
            return;
        }
        let n = self.predicted.len();
        let neighbors = &self.neighbors;
        let predicted = &mut self.predicted;
        for i in 0..n {
            for &j in &neighbors[i] {
                let j = j as usize;
                if j <= i || j >= n {
                    continue;
                }
                let delta = predicted[i] - predicted[j];
                let dist = delta.length();
                if !dist.is_finite() || dist >= min_dist {
                    continue;
                }
                // 非零距离保留连线方向；完全重合或长度下溢时用确定性 X，避免 0/0。
                let dir = if dist > 0.0 { delta / dist } else { Vec3::X };
                let push = (min_dist - dist) * 0.5;
                predicted[i] += dir * push;
                predicted[j] -= dir * push;
            }
        }
        // 分离可能把粒子推进墙；重新投影一次（不做速度/摩擦）。
        for p in &mut self.predicted {
            *p = collide_position(*p, *p, bodies, radius, self.desc.boundary_velocity_mode).0;
        }
    }

    /// 推进一步（PBF：预测 → 密度约束 → 边界投影 → 速度更新 → XSPH 粘性）。
    fn step(&mut self, bodies: &mut [BodySlot], gravity: Vec3, dt: f32) {
        if !self.spawned {
            self.spawn();
        }
        // 发射要在 n == 0 的早退之前：水被排空后还能继续补水。
        self.emit(dt);
        self.emit_bubbles(dt);
        let n = self.positions.len();
        if n == 0 {
            // 水排空后泡沫/气泡还要继续飞/淡出，不能在这里把统计和它们一起清掉。
            let foam_radius = (self.desc.radius * 0.5).max(0.005);
            self.update_foam(gravity, dt, bodies, foam_radius);
            self.update_bubbles(gravity, dt, bodies);
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
        self.update_gas_fractions(h);

        // 1. 重力 + 预测
        self.predicted.resize(n, Vec3::ZERO);
        for i in 0..n {
            let v = (self.velocities[i] + gravity * dt).clamp_length(MAX_SPEED);
            self.velocities[i] = v;
            self.predicted[i] = self.positions[i] + v * dt;
        }

        // 2. 稠密网格邻域（固定 27 格顺序；网格每步重建一次）
        self.neighbors.resize_with(n, Vec::new);
        for v in &mut self.neighbors {
            v.clear();
        }
        if let Some(grid) = FluidGrid::new(&self.predicted, h) {
            for i in 0..n {
                grid.neighbors(&self.predicted, i, h2, &mut self.neighbors[i]);
            }
        } else {
            build_neighbors_hash(&self.predicted, h, h2, &mut self.neighbors);
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
            let gas = &self.gas_fractions;
            for (i, d) in self.delta.iter_mut().enumerate() {
                let rho0_i = if gas.is_empty() {
                    rho0
                } else {
                    rho0 * (1.0 - gas.get(i).copied().unwrap_or(0.0).clamp(0.0, 0.9))
                };
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
                *d = (dp / rho0_i).clamp_length(0.1 * h);
            }
            for (i, d) in self.delta.iter().enumerate() {
                let prev = self.predicted[i];
                let (p, contact) = collide_position(
                    prev,
                    prev + *d,
                    &*bodies,
                    radius,
                    self.desc.boundary_velocity_mode,
                );
                self.predicted[i] = p;
                if contact.is_some() {
                    self.contacts[i] = contact;
                }
            }
        }
        if self.desc.particle_collision {
            self.separate_particles(radius, &*bodies);
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
            let vb = body.velocity_at_point(c.point);
            let vn = (*v - vb).dot(c.normal);
            let dynamic = body.is_dynamic();
            if dynamic && vn < 0.0 {
                // 只反推 20% 的法向动量：粒子的"压力"已由浮力项承担，
                // 全额反推会让成千上万颗粒子把刚体当鼓面敲（实测会把浮球砸进池底）。
                let impulse = c.normal * (self.mass * vn * 0.2);
                body.apply_impulse_at_point(impulse, c.point);
                // 粒子也拿到反冲；否则水会"穿过"轻的刚体。
                *v -= impulse / self.mass;
            }
            if self.desc.contact_friction > 0.0 {
                // 切向摩擦：粒子相对刚体表面的切向速度，按库仑上限 μ·|Jn| 衰减。
                // 粒子受 -vt 方向冲量，动态刚体受反作用 +vt 方向（因此会被水拖着走）。
                // vn≈0 的静置接触没有"接近冲量"，用重力在法线方向的支持力近似
                // 法向载荷，否则一池静水会完全没有摩擦。静态刚体不接收冲量。
                let approach = (-self.mass * vn * 0.2).max(0.0);
                let support = (-gravity.dot(c.normal)).max(0.0) * self.mass * dt;
                let jn = approach.max(support);
                let vrel = *v - vb;
                let vt = vrel - c.normal * vrel.dot(c.normal);
                let vt_len = vt.length();
                if jn > 0.0 && vt_len > EPS && vt_len.is_finite() {
                    let jt_mag = (vt_len * self.mass).min(self.desc.contact_friction * jn);
                    if jt_mag > 0.0 {
                        let jt = -vt * (jt_mag / vt_len);
                        if dynamic {
                            body.apply_impulse_at_point(-jt, c.point);
                        }
                        *v += jt / self.mass;
                    }
                }
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
                    self.densities[keep] = self.densities[i];
                    if !self.gas_fractions.is_empty() {
                        self.gas_fractions[keep] = self.gas_fractions[i];
                    }
                    keep += 1;
                }
            }
            self.positions.truncate(keep);
            self.velocities.truncate(keep);
            self.densities.truncate(keep);
            self.gas_fractions.truncate(keep);
        }
        self.spawn_foam(dt);
        let foam_radius = (self.desc.radius * 0.5).max(0.005);
        self.update_foam(gravity, dt, bodies, foam_radius);
        self.update_bubbles(gravity, dt, bodies);
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
        if self.foam.len() >= d.max_foam {
            // Match the already-full path: capacity cannot bank emission for later.
            self.foam_accum = 0.0;
        }
    }

    /// 连续生成气泡：按 `bubble_rate` 累积，位置用黄金角/确定轮转撒在发射盘内。
    fn emit_bubbles(&mut self, dt: f32) {
        let d = &self.desc;
        if !d.bubbles || d.bubble_rate <= 0.0 || d.max_bubbles == 0 || dt <= 0.0 {
            return;
        }
        self.bubble_accum += d.bubble_rate * dt;
        let radius = d.bubble_radius.max(0.005);
        while self.bubble_accum >= 1.0 && self.bubbles.len() < d.max_bubbles {
            let k = self.bubble_emitted;
            let angle = k as f32 * 2.399_963_2;
            let ring = ((k % 8) as f32 + 0.5) / 8.0;
            let r = d.emit_radius.max(0.0) * ring.sqrt();
            let position = d.emit_position + Vec3::new(angle.cos() * r, 0.0, angle.sin() * r);
            let velocity = d.emit_velocity + Vec3::Y * 0.5;
            self.bubbles.push(Bubble {
                position,
                velocity,
                radius,
                rest_radius: radius,
                age: 0.0,
                life: 4.0,
            });
            self.bubble_emitted = self.bubble_emitted.wrapping_add(1);
            self.bubble_accum -= 1.0;
        }
        if self.bubbles.len() >= d.max_bubbles && self.bubble_accum >= 1.0 {
            // 上限已满：丢掉未能发射的整数预算，保留分数相位，避免释放容量后补发。
            self.bubble_accum = self.bubble_accum.fract();
        }
    }

    /// 气泡浮升/阻尼/刚体碰撞/排开液体。
    ///
    /// 气泡不是 PBF 粒子：不参与密度约束，也不会被液体压力推回去。但每步会把
    /// 与气泡体积重叠的液体粒子推开，并给它们一点速度，所以气泡会**排开液体**、
    /// 在液面破裂。`life` 到点或浮出液面就消失。
    fn update_bubbles(&mut self, gravity: Vec3, dt: f32, bodies: &[BodySlot]) {
        if self.bubbles.is_empty() || !dt.is_finite() || dt <= 0.0 {
            return;
        }
        let buoyancy = self.desc.bubble_buoyancy.max(0.0);
        let drag = self.desc.bubble_drag.max(0.0);
        let expansion = self.desc.bubble_expansion.max(0.0);
        let wake = self.desc.bubble_wake.max(0.0);
        let pressure = self.desc.bubble_pressure.max(0.0);
        let momentum_coupling = self.desc.bubble_momentum_coupling.max(0.0);
        let max_bubble_radius = self.desc.bubble_radius.max(0.005) * 4.0;
        let particle_radius = self.desc.radius.max(0.0);
        let foam_enabled = self.desc.foam;
        let max_foam = self.desc.max_foam;
        let up = if gravity.length_squared() > 1e-6 {
            -gravity.normalize()
        } else {
            Vec3::Y
        };
        let accel = up * (9.81 * buoyancy);
        let base_radius = self.desc.bubble_radius.max(1e-4);
        let surface = if !self.positions.is_empty() {
            let mut max_y = f32::NEG_INFINITY;
            for p in &self.positions {
                max_y = max_y.max(p.y);
            }
            if self.desc.initial_positions.is_some() {
                // Explicit world-space particles need not surround the lattice center.
                max_y + self.desc.radius * 0.5
            } else {
                (max_y + self.desc.radius * 0.5).max(self.desc.center.y)
            }
        } else {
            self.desc.center.y + self.desc.half_extents.y
        };
        let mut keep = 0usize;
        for index in 0..self.bubbles.len() {
            let mut b = self.bubbles[index];
            let mut liquid_momentum = Vec3::ZERO;
            b.age += dt;
            if b.age >= b.life {
                continue;
            }
            // 兼容手工构造的旧调用：没有表面半径时用当前半径补上。
            if !(b.rest_radius.is_finite() && b.rest_radius > 0.0) {
                b.rest_radius = b.radius.max(0.005);
            }
            if expansion > 0.0 {
                // 简化压力膨胀：表面半径按指数长大，封顶到初始半径的 4 倍。
                // 不改变已经比上限大的注入气泡。
                let cap = max_bubble_radius.max(b.rest_radius);
                b.rest_radius = (b.rest_radius * (1.0 + expansion * dt)).min(cap);
            }
            // 静水压：pV=const 的等温近似，深处半径按 (1+k·depth)^(-1/3) 收缩。
            // k=0 直接走旧路径，连 cbrt 都不做，旧气泡逐位不变。
            let compression = self.desc.bubble_compressibility.max(0.0);
            b.radius = if compression > 0.0 {
                let depth = (surface - b.position.y).max(0.0);
                let pressure = 1.0 + compression * depth;
                (b.rest_radius / pressure.cbrt()).max(0.005)
            } else {
                b.rest_radius
            };
            let prev = b.position;
            // 气泡浮力 ∝ r³、阻力 ∝ r²，终端速度 ∝ √r；合并/膨胀后的大泡更快。
            // 半径等于 `bubble_radius` 时因子逐位为 1，旧气泡路径不变。
            let size_factor = (b.radius.max(0.0) / base_radius).sqrt().clamp(0.25, 4.0);
            b.velocity =
                (b.velocity + accel * (size_factor * dt)) * (1.0 - drag * dt).clamp(0.0, 1.0);
            b.velocity = b.velocity.clamp_length(10.0);
            b.position += b.velocity * dt;
            let (corrected, contact) = collide_position(
                prev,
                b.position,
                bodies,
                b.radius.max(0.005),
                self.desc.boundary_velocity_mode,
            );
            b.position = corrected;
            if let Some(contact) = contact {
                let vn = b.velocity.dot(contact.normal);
                if vn < 0.0 {
                    b.velocity -= contact.normal * (vn * 1.05);
                }
                let vn_after = b.velocity.dot(contact.normal);
                let tangential = b.velocity - contact.normal * vn_after;
                b.velocity = contact.normal * vn_after + tangential * 0.94;
            }
            // 排开液体：气泡体积内的粒子沿径向推出，并给一点速度；气泡受反作用。
            if !self.positions.is_empty() {
                let reach = b.radius + particle_radius * 0.5;
                for i in 0..self.positions.len() {
                    let d = self.positions[i] - b.position;
                    let dist = d.length();
                    if dist >= reach || dist <= EPS {
                        continue;
                    }
                    let n = d / dist;
                    let push = (reach - dist) * 0.5;
                    self.positions[i] += n * push;
                    let dv = n * (push / dt * 0.2);
                    self.velocities[i] += dv;
                    liquid_momentum += dv * self.mass;
                    b.position -= n * (push * 0.05);
                }
            }
            // 气泡过压：把内压转成对液体的径向加速度。只加在液体上；气泡本身
            // 的动量由上面的径向排开近似处理（真实气体质量太小，按动量守恒反推
            // 会让气泡速度爆掉）。`pressure=0` 时整段不执行。
            if pressure > 0.0 && !self.positions.is_empty() {
                let radius = b.radius.max(0.005);
                let over = (pressure * (b.rest_radius / radius).powi(3)).clamp(0.0, 1.0e7);
                let accel = (over / (self.desc.rest_density.max(1.0) * radius)).min(1.0e4);
                let reach = radius + particle_radius * 0.5;
                for i in 0..self.positions.len() {
                    let d = self.positions[i] - b.position;
                    let dist = d.length();
                    if dist >= reach || dist <= EPS {
                        continue;
                    }
                    let falloff = 1.0 - dist / reach;
                    let dv = (d / dist) * (accel * falloff * dt);
                    self.velocities[i] += dv;
                    liquid_momentum += dv * self.mass;
                }
            }
            // 尾流：气泡上方锥形区域内的液体被向上拖，气泡受到很小的反作用。
            // 这是两相动量耦合的近似，不改变 `bubble_wake = 0` 的旧路径。
            if wake > 0.0 && b.velocity.y > 0.0 && !self.positions.is_empty() {
                let reach_y = b.radius * 3.0;
                let reach_xz = b.radius * 1.5;
                for i in 0..self.positions.len() {
                    let d = self.positions[i] - b.position;
                    if d.y <= 0.0 || d.y >= reach_y {
                        continue;
                    }
                    let horizontal = (d.x * d.x + d.z * d.z).sqrt();
                    if horizontal >= reach_xz {
                        continue;
                    }
                    let weight =
                        (1.0 - d.y / reach_y).max(0.0) * (1.0 - horizontal / reach_xz).max(0.0);
                    let dv = wake * b.velocity.y * weight * dt * 2.0;
                    self.velocities[i].y += dv;
                    liquid_momentum.y += dv * self.mass;
                    b.velocity.y -= dv * 0.01;
                }
            }
            if momentum_coupling > 0.0 && liquid_momentum.length_squared() > 0.0 {
                let volume = 4.0 / 3.0 * std::f32::consts::PI * b.radius.max(0.005).powi(3);
                let added_mass = (0.5 * self.desc.rest_density.max(1.0) * volume).max(1e-6);
                let recoil = liquid_momentum * (momentum_coupling / added_mass);
                b.velocity = (b.velocity - recoil).clamp_length(10.0);
            }
            // 排水优先于液面破裂：已流出的气泡不能留下下一步才被排掉的泡沫。
            if let Some(y) = self.desc.drain_y {
                if b.position.y < y {
                    continue;
                }
            }
            if b.position.y > surface + b.radius {
                // 到达液面：气泡破裂消失，并在液面留下一颗**被动泡沫**。
                // 只有开启 `foam` 时才留；泡沫数量仍受 `max_foam` 限制。
                if foam_enabled && self.foam.len() < max_foam {
                    let mut v = b.velocity * 0.2;
                    v.y = v.y.max(0.25);
                    self.foam.push(FoamParticle {
                        position: b.position,
                        velocity: v,
                        life: 0.8,
                        kind: FoamKind::Foam,
                    });
                }
                continue;
            }
            if !b.position.is_finite() || !b.velocity.is_finite() {
                continue;
            }
            self.bubbles[keep] = b;
            keep += 1;
        }
        self.bubbles.truncate(keep);
        self.coalesce_bubbles();
        self.split_large_bubbles();
    }

    /// 气泡半径超过 `bubble_max_radius` 时按体积守恒拆成多颗。
    ///
    /// 只在 `bubble_max_radius > 0` 时执行； daughters 沿一个固定圆环均匀排开，
    /// 避免下一步立刻又合并。`max_bubbles` 放不下时保留原气泡，不悄悄丢气体。
    fn split_large_bubbles(&mut self) {
        let max_radius = self.desc.bubble_max_radius.max(0.0);
        if max_radius <= 0.0 || self.bubbles.is_empty() {
            return;
        }
        let cap = self.desc.max_bubbles.max(self.bubbles.len());
        let mut out = Vec::with_capacity(self.bubbles.len());
        for (index, b) in self.bubbles.iter().copied().enumerate() {
            if b.radius <= max_radius || out.len() >= cap {
                out.push(b);
                continue;
            }
            let ratio = (b.radius / max_radius).max(1.0);
            let needed = (ratio.powi(3).ceil() as usize).max(2);
            // 为尚未处理的原气泡各保留一个位置，拆分不能挤占它们的容量。
            let remaining = self.bubbles.len() - index - 1;
            let room = cap.saturating_sub(out.len() + remaining);
            if needed > room || room < 2 {
                // 放不下：保留原气泡，下一帧/腾出空间后再拆。
                out.push(b);
                continue;
            }
            let daughter = (b.radius.powi(3) / needed as f32).cbrt();
            let rest = (b.rest_radius.powi(3) / needed as f32).cbrt();
            for k in 0..needed {
                let angle = std::f32::consts::TAU * k as f32 / needed as f32;
                let offset = Vec3::new(angle.cos(), 0.0, angle.sin()) * (daughter * 1.2);
                out.push(Bubble {
                    position: b.position + offset,
                    velocity: b.velocity,
                    radius: daughter,
                    rest_radius: rest,
                    age: b.age,
                    life: b.life,
                });
            }
        }
        self.bubbles = out;
    }

    /// 两颗气泡接触时合并：位置/速度按体积加权，半径按体积守恒
    /// `r = (r1³ + r2³)^(1/3)`；age 取较大、life 取较长。
    fn coalesce_bubbles(&mut self) {
        if self.bubbles.len() < 2 {
            return;
        }
        let mut i = 0;
        while i < self.bubbles.len() {
            let mut j = i + 1;
            while j < self.bubbles.len() {
                let a = self.bubbles[i];
                let b = self.bubbles[j];
                let sr = (a.radius + b.radius).max(1e-4);
                if (a.position - b.position).length_squared() <= sr * sr {
                    let va = a.radius.max(0.0).powi(3);
                    let vb = b.radius.max(0.0).powi(3);
                    let total = va + vb;
                    let ra = a.rest_radius.max(0.0).powi(3);
                    let rb = b.rest_radius.max(0.0).powi(3);
                    let rest_total = ra + rb;
                    if total > 1e-12 {
                        let merged = Bubble {
                            position: (a.position * va + b.position * vb) / total,
                            velocity: (a.velocity * va + b.velocity * vb) / total,
                            radius: total.cbrt(),
                            rest_radius: if rest_total > 1e-12 {
                                rest_total.cbrt()
                            } else {
                                total.cbrt()
                            },
                            age: a.age.max(b.age),
                            life: a.life.max(b.life),
                        };
                        self.bubbles[i] = merged;
                        self.bubbles.swap_remove(j);
                        // swap_remove 把末尾元素放到 j；不递增 j，继续检查它。
                    } else {
                        j += 1;
                    }
                } else {
                    j += 1;
                }
            }
            i += 1;
        }
    }

    /// 被动泡沫的寿命/重力/阻尼 + **与刚体碰撞**。
    ///
    /// 泡沫仍是纯视觉粒子（不产生反作用力），但不应该穿过地板/墙：用与流体粒子
    /// 相同的 `collide_position` 把它推出表面，并消掉朝表面的法向速度（轻微回弹 +
    /// 切向摩擦）。弹道/飞沫都走这条路，所以水花不会掉到地下。
    fn update_foam(&mut self, gravity: Vec3, dt: f32, bodies: &[BodySlot], radius: f32) {
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
            let prev = f.position;
            f.velocity = (f.velocity + g * dt) * (1.0 - 2.2 * dt).clamp(0.0, 1.0);
            f.position += f.velocity * dt;
            let (corrected, contact) = collide_position(
                prev,
                f.position,
                bodies,
                radius,
                self.desc.boundary_velocity_mode,
            );
            f.position = corrected;
            if let Some(contact) = contact {
                let vn = f.velocity.dot(contact.normal);
                if vn < 0.0 {
                    // 5% 回弹，避免泡沫贴着地面时完全失去法向速度而抖动。
                    f.velocity -= contact.normal * (vn * 1.05);
                }
                // 切向摩擦：水花碰到斜面会顺着坡滑，而不是继续原方向钻进去。
                let vn_after = f.velocity.dot(contact.normal);
                let tangential = f.velocity - contact.normal * vn_after;
                f.velocity = contact.normal * vn_after + tangential * 0.94;
            }
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

    /// 用当前气泡位置计算每个液体粒子的局部气体体积分数。
    ///
    /// 这不是严格的两相状态方程：用气泡体积与核支持体积的比值近似空隙率，
    /// 只用于降低局部有效静止密度，让液体在气泡周围膨胀。没有气泡/关闭耦合时
    /// 清空，后续路径逐位回到旧行为。
    fn update_gas_fractions(&mut self, h: f32) {
        self.gas_fractions.clear();
        if !self.desc.bubble_density_coupling
            || self.bubbles.is_empty()
            || self.positions.is_empty()
            || !(h.is_finite() && h > 0.0)
        {
            return;
        }
        let support = 4.0 / 3.0 * PI * h * h * h;
        if support <= 0.0 {
            return;
        }
        let n = self.positions.len();
        self.gas_fractions.resize(n, 0.0);
        for i in 0..n {
            let p = self.positions[i];
            let mut volume = 0.0f32;
            for b in &self.bubbles {
                let d = (p - b.position).length();
                if d >= h {
                    continue;
                }
                let r = b.radius.max(0.0);
                // 气泡体积按核内重叠比例计入：中心全算，离得越远权重越低。
                volume += 4.0 / 3.0 * PI * r * r * r * (1.0 - d / h);
            }
            self.gas_fractions[i] = (volume / support).clamp(0.0, 0.9);
        }
    }

    /// 当前每个粒子的气体体积分数（测试/诊断；无耦合时为空）。
    pub fn gas_fractions(&self) -> &[f32] {
        &self.gas_fractions
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
        // 粒子多时按区间并行：每颗粒子只写自己的密度/lambda，区间互不共享，
        // 与串行逐位相同。少粒子仍走单线程，避免线程创建开销。
        #[cfg(not(target_arch = "wasm32"))]
        let n = self.predicted.len();
        #[cfg(not(target_arch = "wasm32"))]
        let workers = if n >= 1024 {
            std::thread::available_parallelism()
                .map(|v| v.get())
                .unwrap_or(1)
                .min(8)
        } else {
            1
        };
        #[cfg(target_arch = "wasm32")]
        let workers = 1usize;
        if workers <= 1 {
            density_lambda_range(
                &self.predicted,
                &self.neighbors,
                &self.gas_fractions,
                mass,
                rho0,
                h,
                h2,
                poly6,
                spiky,
                w0,
                0,
                &mut self.densities,
                &mut self.lambdas,
            );
        } else {
            #[cfg(not(target_arch = "wasm32"))]
            {
                let predicted = &self.predicted;
                let neighbors = &self.neighbors;
                let gas_fractions = &self.gas_fractions;
                let densities = &mut self.densities;
                let lambdas = &mut self.lambdas;
                let chunk = n.div_ceil(workers);
                std::thread::scope(|scope| {
                    for (chunk_index, (density_chunk, lambda_chunk)) in densities
                        .chunks_mut(chunk)
                        .zip(lambdas.chunks_mut(chunk))
                        .enumerate()
                    {
                        let start = chunk_index * chunk;
                        scope.spawn(move || {
                            density_lambda_range(
                                predicted,
                                neighbors,
                                gas_fractions,
                                mass,
                                rho0,
                                h,
                                h2,
                                poly6,
                                spiky,
                                w0,
                                start,
                                density_chunk,
                                lambda_chunk,
                            );
                        });
                    }
                });
            }
        }
    }

    fn recompute_stats(&mut self) {
        let n = self.positions.len();
        if n == 0 {
            self.stats = FluidStats {
                foam: self.foam.len(),
                bubbles: self.bubbles.len(),
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
            bubbles: self.bubbles.len(),
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
    mode: FluidBoundaryVelocityMode,
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
            let boundary_velocity = || {
                if mode == FluidBoundaryVelocityMode::BodyMotion
                    && body.body_type == crate::body::BodyType::Static
                {
                    Vec3::ZERO
                } else {
                    body.velocity_at_point(surf_world)
                }
            };
            if crossed {
                p = surf_world - n * radius;
                contact = Some(Contact {
                    normal: -n,
                    velocity: boundary_velocity(),
                    point: surf_world,
                    body: body_index as u32,
                });
            } else if signed < radius {
                p = surf_world + n * radius;
                contact = Some(Contact {
                    normal: n,
                    velocity: boundary_velocity(),
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

    #[test]
    fn body_local_rotated_compound_matches_independent_fluid_obstacle() {
        use crate::body::{CompoundTransformMode, ShapePart};

        let body_pose = Transform {
            position: vec3(0.4, 0.2, -0.3),
            rotation: Quat::from_rotation_z(0.6),
            scale: Vec3::ONE,
        };
        let local = Transform {
            position: Vec3::X * 2.0,
            rotation: Quat::from_rotation_x(0.7),
            scale: Vec3::ONE,
        };
        let expected = body_pose.mul_transform(&local);
        let normal = expected.rotation * Vec3::Y;
        let tangent = expected.rotation * Vec3::X;
        let start = expected.position + normal * 0.55;
        let obstacle = Shape::Box {
            half_extents: vec3(0.8, 0.15, 0.8),
        };
        for kinematic in [false, true] {
            let mut trajectories = Vec::new();
            let mut obstacle_poses = Vec::new();
            for variant in 0..5 {
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                let primary = if variant == 1 {
                    obstacle.clone()
                } else {
                    Shape::Sphere { radius: 0.05 }
                };
                let mut builder = if kinematic {
                    BodyBuilder::kinematic(primary)
                } else {
                    BodyBuilder::r#static(primary)
                };
                let pose = if variant == 1 { expected } else { body_pose };
                builder = builder.position(pose.position).rotation(pose.rotation);
                if variant != 1 {
                    builder = builder.part(ShapePart::at(
                        obstacle.clone(),
                        local.position,
                        local.rotation,
                    ));
                }
                if variant <= 2 {
                    builder = builder.compound_transform_mode(CompoundTransformMode::BodyLocal);
                } else if variant == 4 {
                    builder = builder.compound_transform_mode(CompoundTransformMode::Legacy);
                }
                if variant == 2 {
                    builder = builder.sensor(true);
                }
                if kinematic {
                    builder = builder.linear_velocity(Vec3::Z * 0.2);
                }
                let handle = world.add_builder(builder);
                assert_eq!(
                    world.body(handle).unwrap().linear_velocity,
                    if kinematic { Vec3::Z * 0.2 } else { Vec3::ZERO }
                );
                world
                    .add_fluid(FluidDesc {
                        initial_positions: Some(vec![
                            start - tangent * 0.3,
                            start,
                            start + tangent * 0.3,
                        ]),
                        initial_velocity: -normal * 2.0,
                        spacing: 0.1,
                        radius: 0.04,
                        viscosity: 0.0,
                        surface_tension: 0.0,
                        vorticity: 0.0,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                assert_eq!(world.fluids()[0].velocities()[0], -normal * 2.0);
                let mut trajectory = Vec::new();
                let mut poses = Vec::new();
                for _ in 0..24 {
                    world.step(1.0 / 60.0);
                    let body = world.body(handle).unwrap();
                    let (_, part) = body.parts().nth(if variant == 1 { 0 } else { 1 }).unwrap();
                    let pose = body.part_world_transform(&part);
                    poses.push((
                        pose.position.to_array().map(f32::to_bits),
                        pose.rotation.to_array().map(f32::to_bits),
                    ));
                    let fluid = &world.fluids()[0];
                    trajectory.push((fluid.particles().to_vec(), fluid.velocities().to_vec()));
                }
                obstacle_poses.push(poses);
                trajectories.push(trajectory);
            }
            assert_eq!(
                obstacle_poses[0], obstacle_poses[1],
                "equivalent obstacle poses must match every step"
            );
            for (compound, independent) in trajectories[0].iter().zip(&trajectories[1]) {
                for (a, b) in compound.0.iter().zip(&independent.0) {
                    assert!(
                        (*a - *b).length() < 2e-4,
                        "position kine={kinematic}: {a:?} {b:?}"
                    );
                }
                for (a, b) in compound.1.iter().zip(&independent.1) {
                    assert!(
                        (*a - *b).length() < 2e-3,
                        "velocity kine={kinematic}: {a:?} {b:?}"
                    );
                }
            }
            let end = trajectories[0].last().unwrap().0[1];
            let control = trajectories[2].last().unwrap().0[1];
            assert!((end - start).length() > 0.2, "particles must actually move");
            assert!(
                (end - expected.position).dot(normal) >= 0.189,
                "must stop outside box: {end:?}"
            );
            assert!(
                (control - expected.position).dot(normal) < 0.0,
                "sensor control must cross"
            );
            for (default, explicit) in trajectories[3].iter().zip(&trajectories[4]) {
                for (a, b) in default
                    .0
                    .iter()
                    .chain(&default.1)
                    .zip(explicit.0.iter().chain(&explicit.1))
                {
                    assert_eq!(
                        a.to_array().map(f32::to_bits),
                        b.to_array().map(f32::to_bits)
                    );
                }
            }
        }
    }

    #[test]
    fn static_obstacle_residual_velocity_does_not_drive_fluid() {
        let mut trajectories = Vec::new();
        for residual in [Vec3::ZERO, Vec3::Y * 3.0] {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            let handle = world.add_builder(
                BodyBuilder::r#static(Shape::Box {
                    half_extents: vec3(1.0, 0.1, 1.0),
                })
                .linear_velocity(residual),
            );
            assert_eq!(world.body(handle).unwrap().linear_velocity, residual);
            world
                .add_fluid(FluidDesc {
                    boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                    initial_positions: Some(vec![Vec3::Y * 0.16]),
                    initial_velocity: -Vec3::Y * 2.0,
                    radius: 0.04,
                    spacing: 0.1,
                    ..FluidDesc::default()
                })
                .unwrap();
            world.step(1.0 / 60.0);
            assert_eq!(world.body(handle).unwrap().position(), Vec3::ZERO);
            trajectories.push((
                world.fluids()[0].particles()[0],
                world.fluids()[0].velocities()[0],
            ));
        }
        assert_eq!(
            trajectories[0], trajectories[1],
            "static residual must not impart boundary motion"
        );
    }

    #[test]
    fn boundary_velocity_modes_preserve_moving_bodies_and_legacy_bits() {
        use crate::body::BodyType;
        for body_type in [BodyType::Static, BodyType::Kinematic, BodyType::Dynamic] {
            for angular in [false, true] {
                let mut outputs = Vec::new();
                for mode in [
                    None,
                    Some(FluidBoundaryVelocityMode::Legacy),
                    Some(FluidBoundaryVelocityMode::BodyMotion),
                ] {
                    let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                    let shape = Shape::Box {
                        half_extents: vec3(1.0, 0.1, 1.0),
                    };
                    let builder = match body_type {
                        BodyType::Static => BodyBuilder::r#static(shape),
                        BodyType::Kinematic => BodyBuilder::kinematic(shape),
                        BodyType::Dynamic => BodyBuilder::dynamic(shape),
                    };
                    let v = if angular { Vec3::ZERO } else { Vec3::Y * 3.0 };
                    let w = if angular { Vec3::Z * 3.0 } else { Vec3::ZERO };
                    let handle = world.add_builder(builder.linear_velocity(v).angular_velocity(w));
                    assert_eq!(world.body(handle).unwrap().linear_velocity, v);
                    assert_eq!(world.body(handle).unwrap().angular_velocity, w);
                    let mut desc = FluidDesc {
                        initial_positions: Some(vec![vec3(0.3, 0.16, 0.0)]),
                        initial_velocity: -Vec3::Y * 2.0,
                        radius: 0.04,
                        spacing: 0.1,
                        ..FluidDesc::default()
                    };
                    if let Some(mode) = mode {
                        desc.boundary_velocity_mode = mode;
                    }
                    world.add_fluid(desc).unwrap();
                    world.step(1.0 / 60.0);
                    let fluid = &world.fluids()[0];
                    outputs.push((
                        fluid.particles()[0].to_array().map(f32::to_bits),
                        fluid.velocities()[0].to_array().map(f32::to_bits),
                    ));
                    if body_type == BodyType::Static
                        && mode == Some(FluidBoundaryVelocityMode::BodyMotion)
                    {
                        assert_eq!(fluid.velocities()[0], Vec3::ZERO);
                    }
                }
                assert_eq!(
                    outputs[0], outputs[1],
                    "default must preserve explicit legacy"
                );
                if body_type != BodyType::Static {
                    assert_eq!(
                        outputs[0], outputs[2],
                        "moving boundary policy must be unchanged"
                    );
                } else {
                    assert_ne!(
                        outputs[0], outputs[2],
                        "residual fixture must exercise contact"
                    );
                }
            }
        }
    }

    #[test]
    fn boundary_velocity_policy_keeps_passive_spray_response() {
        let mut states = Vec::new();
        for mode in [
            FluidBoundaryVelocityMode::Legacy,
            FluidBoundaryVelocityMode::BodyMotion,
        ] {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            world.add_builder(
                BodyBuilder::r#static(Shape::Box {
                    half_extents: vec3(1.0, 0.1, 1.0),
                })
                .linear_velocity(Vec3::Y * 3.0),
            );
            world
                .add_fluid(FluidDesc {
                    boundary_velocity_mode: mode,
                    radius: 0.04,
                    spacing: 0.1,
                    initial_positions: Some(vec![Vec3::Y * 2.0]),
                    ..FluidDesc::default()
                })
                .unwrap();
            world.fluids_mut()[0].foam.push(FoamParticle {
                position: Vec3::Y * 0.13,
                velocity: -Vec3::Y * 2.0,
                life: 1.0,
                kind: FoamKind::Spray,
            });
            world.step(1.0 / 60.0);
            let particle = world.fluids()[0].foam[0];
            // Passive spray uses half the liquid collision radius.
            assert!((particle.position.y - 0.12).abs() < 1e-6);
            assert!(particle.velocity.y > 0.0);
            states.push(particle);
        }
        assert_eq!(states[0], states[1]);
    }

    #[test]
    fn gas_volume_ignores_nonfinite_radius_without_losing_valid_bubbles() {
        let mut fluid = Fluid::new(FluidDesc::default()).unwrap();
        let bubble = Bubble {
            position: Vec3::ZERO,
            velocity: Vec3::ZERO,
            radius: 0.1,
            rest_radius: 0.1,
            age: 0.0,
            life: 10.0,
        };
        let bounds = Aabb::new(-Vec3::ONE, Vec3::ONE);
        fluid.add_bubble(bubble);
        let expected = fluid.gas_volume_near(bounds);
        assert!(expected > 0.0 && expected.is_finite());
        for radius in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
            fluid.add_bubble(Bubble { radius, ..bubble });
            assert_eq!(fluid.gas_volume_near(bounds).to_bits(), expected.to_bits());
        }
    }

    #[test]
    fn foam_emission_discards_budget_when_it_fills_capacity() {
        let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
        world
            .add_fluid(FluidDesc {
                foam: true,
                foam_rate: 7.0,
                foam_speed: 0.0,
                max_foam: 2,
                max_particles: 8,
                ..FluidDesc::default()
            })
            .unwrap();
        let fluid = &mut world.fluids_mut()[0];
        fluid.spawn_foam(0.5); // 3.5 due; two fit. Full capacity discards all remaining budget.
        assert_eq!(fluid.foam.len(), 2);
        fluid.foam.clear(); // Controlled release before the public fixed step.
        world.step(1.0 / 60.0);
        assert_eq!(
            world.fluids()[0].foam().len(),
            0,
            "released capacity must not replay foam budget discarded while full"
        );
    }

    #[test]
    fn foam_capacity_reset_preserves_uncapped_and_ineligible_behavior() {
        let desc = FluidDesc {
            foam: true,
            foam_rate: 7.0,
            foam_speed: 1.0,
            initial_velocity: Vec3::X * 2.0,
            max_particles: 8,
            ..FluidDesc::default()
        };
        for cap in [0, 2, 8] {
            let mut fluid = Fluid::new(FluidDesc {
                max_foam: cap,
                ..desc.clone()
            })
            .unwrap();
            fluid.spawn_foam(2.5); // Enough budget and eligible particles to fill every cap.
            assert_eq!(fluid.foam.len(), cap);
            assert_eq!(fluid.foam_accum.to_bits(), 0.0f32.to_bits());
            fluid.spawn_foam(0.5); // Already-full path has identical reset semantics.
            assert_eq!(fluid.foam_accum.to_bits(), 0.0f32.to_bits());
            fluid.foam.clear();
            fluid.spawn_foam(0.125); // 0.875 new budget must not replay the discarded fraction.
            assert!(fluid.foam.is_empty());
        }
        let mut fluid = Fluid::new(desc).unwrap();
        fluid.velocities.fill(Vec3::ZERO);
        fluid.spawn_foam(0.5); // No eligible candidates retain the pre-existing budget semantics.
        assert!(fluid.foam.is_empty());
        assert_eq!(fluid.foam_accum, 3.5);
        fluid.velocities.fill(Vec3::X * 2.0);
        fluid.spawn_foam(0.125); // 4.375 total: emit four in the original candidate order.
        assert_eq!(fluid.foam.len(), 4);
        assert_eq!(fluid.foam_accum, 0.375);
        for (i, foam) in fluid.foam.iter().enumerate() {
            assert_eq!(foam.position, fluid.positions[i]);
            assert_eq!(foam.velocity, Vec3::new(2.0 * 0.35, 0.6 + 2.0 * 0.06, 0.0));
            assert_eq!(foam.life, 0.9 + (i % 5) as f32 * 0.1);
        }
        fluid.positions.clear();
        fluid.velocities.clear();
        fluid.spawn_foam(0.5);
        assert_eq!(fluid.foam_accum, 3.875); // Empty candidates also keep their existing semantics.
    }

    #[test]
    fn bubble_emission_discards_capped_whole_budget() {
        let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
        world
            .add_fluid(FluidDesc {
                bubbles: true,
                bubble_rate: 7.0,
                max_bubbles: 2,
                max_particles: 8,
                bubble_buoyancy: 0.0,
                bubble_drag: 0.0,
                emit_position: vec3(0.0, -2.0, 0.0),
                ..FluidDesc::default()
            })
            .unwrap();
        let fluid = &mut world.fluids_mut()[0];
        fluid.emit_bubbles(0.5); // 3.5 due, only two fit: retain fractional phase, not debt.
        assert_eq!(fluid.bubbles.len(), 2);
        fluid.bubbles.clear(); // Controlled capacity release before a public fixed step.
        world.step(1.0 / 60.0);
        assert_eq!(
            world.fluids()[0].bubbles().len(),
            0,
            "released capacity must not replay the discarded whole emission budget"
        );
    }

    #[test]
    fn capped_bubble_emission_preserves_fractional_phase() {
        for cap in [0, 2, 8] {
            let mut fluid = Fluid::new(FluidDesc {
                bubbles: true,
                bubble_rate: 7.0,
                max_bubbles: cap,
                max_particles: 8,
                ..FluidDesc::default()
            })
            .unwrap();
            fluid.emit_bubbles(0.5);
            assert_eq!(fluid.bubbles.len(), cap.min(3));
            assert_eq!(
                fluid.bubble_accum.to_bits(),
                if cap == 0 { 0.0f32 } else { 0.5f32 }.to_bits()
            );
            if cap == 2 {
                fluid.emit_bubbles(0.5); // Exactly four due while full, including fractional carry.
                assert_eq!(fluid.bubble_accum.to_bits(), 0.0f32.to_bits());
                fluid.bubble_accum = 1.0;
                fluid.emit_bubbles(f32::MIN_POSITIVE); // Exact-one debt must also be discarded.
                assert_eq!(fluid.bubble_accum.to_bits(), 0.0f32.to_bits());
                fluid.bubbles.clear();
                fluid.emit_bubbles(0.125);
                assert!(fluid.bubbles.is_empty());
                assert_eq!(fluid.bubble_accum.to_bits(), 0.875f32.to_bits());
                fluid.emit_bubbles(0.125);
                assert_eq!(fluid.bubbles.len(), 1);
                assert_eq!(fluid.bubble_accum.to_bits(), 0.75f32.to_bits());
            } else if cap == 8 {
                let before = fluid.bubbles.clone();
                fluid.emit_bubbles(0.125);
                assert_eq!(fluid.bubbles.len(), 4);
                assert_eq!(fluid.bubbles[..3], before);
                assert_eq!(fluid.bubble_accum.to_bits(), 0.375f32.to_bits());
                assert_eq!(
                    fluid.bubbles[3].velocity,
                    fluid.desc.emit_velocity + Vec3::Y * 0.5
                );
            }
        }
    }

    #[test]
    fn bubble_parameters_are_validated_by_public_fluid_creation() {
        for field in 0..8 {
            let upper = [0.5, 5000.0, 10.0, 50.0, 10.0, 10.0, 1.0, 2.0][field];
            for enabled in [false, true] {
                for value in [
                    f32::NAN,
                    f32::INFINITY,
                    f32::NEG_INFINITY,
                    -1.0,
                    upper + 1.0,
                    0.0,
                    f32::MIN_POSITIVE,
                    upper,
                ] {
                    let mut desc = FluidDesc {
                        bubbles: enabled,
                        max_bubbles: 8,
                        max_particles: 8,
                        ..FluidDesc::default()
                    };
                    match field {
                        0 => desc.bubble_radius = value,
                        1 => desc.bubble_rate = value,
                        2 => desc.bubble_buoyancy = value,
                        3 => desc.bubble_drag = value,
                        4 => desc.bubble_expansion = value,
                        5 => desc.bubble_compressibility = value,
                        6 => desc.bubble_max_radius = value,
                        7 => desc.bubble_wake = value,
                        _ => unreachable!(),
                    }
                    let valid = value.is_finite()
                        && value >= 0.0
                        && value <= upper
                        && (field != 0 || value > 0.0);
                    let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                    assert_eq!(
                        world.add_fluid(desc).is_some(),
                        valid,
                        "field={field}, value={value}, enabled={enabled}"
                    );
                    assert_eq!(world.fluids().len(), usize::from(valid));
                }
            }
        }
        for cap in [0, 8, 4096, 4097, usize::MAX] {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            // Creation only: no emission or cap-sized allocation is requested.
            assert_eq!(
                world
                    .add_fluid(FluidDesc {
                        max_bubbles: cap,
                        max_particles: 8,
                        ..FluidDesc::default()
                    })
                    .is_some(),
                cap <= 4096
            );
        }
    }

    #[test]
    fn custom_initial_positions_preserve_world_order_and_velocity() {
        let points = vec![
            vec3(2.0, 1.0, 0.0),
            vec3(0.0, 2.0, 0.0),
            vec3(-1.0, 1.0, 0.0),
        ];
        let velocity = vec3(0.25, 0.0, 0.0);
        let desc = FluidDesc {
            center: Vec3::splat(10.0),
            initial_positions: Some(points.clone()),
            initial_velocity: velocity,
            max_particles: points.len(),
            ..FluidDesc::default()
        };
        assert_eq!(desc.estimated_particles(), points.len());
        let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
        assert!(world.add_fluid(desc).is_some());
        assert_eq!(world.fluids()[0].particles(), &points);
        assert_eq!(world.fluids()[0].velocities(), &[velocity; 3]);
        world.step(1.0 / 60.0);
        for (actual, start) in world.fluids()[0].particles().iter().zip(points) {
            assert!((*actual - (start + velocity / 60.0)).length() < 1e-6);
        }
    }

    #[test]
    fn custom_initial_positions_reject_empty_nonfinite_and_over_capacity() {
        let mut cases = vec![vec![], vec![Vec3::ZERO; 3]];
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for axis in 0..3 {
                let mut point = Vec3::ZERO;
                point[axis] = bad;
                cases.push(vec![point]);
            }
        }
        for points in cases {
            let desc = FluidDesc {
                initial_positions: Some(points),
                max_particles: 2,
                ..FluidDesc::default()
            };
            assert!(!desc.is_valid());
            assert!(PhysicsWorld::default().add_fluid(desc).is_none());
        }
    }

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

    /// 4000 级粒子的整步预算（默认跳过；显式 `--ignored` 才跑）。
    #[test]
    #[ignore = "慢：4000 级流体粒子，显式 --ignored 运行"]
    fn fluid_4000_particles_perf() {
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.add_builder(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(Vec3::ZERO),
        );
        let mut d = box_desc();
        d.center = vec3(0.0, 1.6, 0.0);
        d.half_extents = vec3(0.8, 0.8, 0.8);
        d.spacing = 0.1;
        d.radius = 0.05;
        d.max_particles = 5000;
        world.add_fluid(d).expect("参数合法");
        let fluid = &world.fluids()[0];
        println!("流体粒子数：{}", fluid.particles().len());
        for _ in 0..10 {
            world.step(1.0 / 60.0);
        }
        let start = std::time::Instant::now();
        for _ in 0..30 {
            world.step(1.0 / 60.0);
        }
        let per_step = start.elapsed().as_secs_f64() * 1000.0 / 30.0;
        println!("4000 级流体：{per_step:.2} ms/步");
        assert!(per_step < 16.0, "流体 {per_step:.2} ms/步，超过 16 ms 目标");
    }

    #[test]
    fn fluid_grid_matches_the_hash_neighbour_order() {
        // 稠密网格必须与旧哈希路径给出完全相同的邻居表（集合和顺序）：
        // 邻居顺序会影响浮点求和，顺序变了就不再是“旧场景逐位不变”。
        let h = 0.2f32;
        let h2 = h * h;
        let mut points = Vec::new();
        for i in 0..240 {
            let x = (i % 12) as f32 * 0.055 - 0.3;
            let y = (i / 12 % 10) as f32 * 0.06 - 0.25;
            let z = (i / 120) as f32 * 0.07 - 0.1;
            points.push(vec3(x, y, z));
        }
        let mut grid_neighbors = vec![Vec::new(); points.len()];
        let grid = FluidGrid::new(&points, h).expect("小点集应能建网格");
        for (i, out) in grid_neighbors.iter_mut().enumerate() {
            grid.neighbors(&points, i, h2, out);
        }
        let mut hash_neighbors = vec![Vec::new(); points.len()];
        build_neighbors_hash(&points, h, h2, &mut hash_neighbors);
        assert_eq!(grid_neighbors, hash_neighbors);
    }

    #[test]
    fn fluid_spawn_capacity_respects_particle_limit() {
        let d = FluidDesc {
            half_extents: Vec3::splat(0.5),
            spacing: 0.1,
            radius: 0.05,
            max_particles: 8,
            ..FluidDesc::default()
        };
        let fluid = Fluid::new(d).unwrap();
        assert_eq!(fluid.positions.len(), 8);
        assert_eq!(fluid.positions.capacity(), 8);
    }

    #[test]
    fn fluid_capped_spawn_matches_original_lattice_bits() {
        for limit in [8, 137, 2000] {
            let d = FluidDesc {
                center: Vec3::new(0.25, 2.0, -0.75),
                half_extents: Vec3::splat(0.5),
                spacing: 0.1,
                radius: 0.05,
                initial_velocity: Vec3::new(-0.0, 0.25, -1.5),
                max_particles: limit,
                ..FluidDesc::default()
            };
            let fluid = Fluid::new(d.clone()).unwrap();
            let mut expected = Vec::new();
            for iz in 0..11 {
                for iy in 0..11 {
                    for ix in 0..11 {
                        expected.push(
                            d.center - d.half_extents
                                + Vec3::new(
                                    ix as f32 * d.spacing,
                                    iy as f32 * d.spacing,
                                    iz as f32 * d.spacing,
                                ),
                        );
                    }
                }
            }
            expected.truncate(limit);
            let bits = |v: &Vec3| [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()];
            assert_eq!(
                fluid.positions.iter().map(bits).collect::<Vec<_>>(),
                expected.iter().map(bits).collect::<Vec<_>>()
            );
            assert!(fluid
                .velocities
                .iter()
                .all(|v| bits(v) == bits(&d.initial_velocity)));
            assert_eq!(fluid.positions.capacity(), expected.len());
        }
    }

    #[test]
    fn fluid_extreme_lattice_dimensions_allocate_only_the_limit() {
        let d = FluidDesc {
            half_extents: Vec3::splat(1.0e20),
            spacing: 0.1,
            radius: 0.05,
            max_particles: 8,
            ..FluidDesc::default()
        };
        assert!(d.is_valid());
        assert_eq!(d.estimated_particles(), 8);
        let fluid = Fluid::new(d).unwrap();
        assert_eq!(fluid.positions.len(), 8);
        assert_eq!(fluid.positions.capacity(), 8);
        assert!(fluid.positions.iter().all(|p| p.is_finite()));
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
    fn fluid_bounds_overflow_is_rejected() {
        for sign in [-1.0, 1.0] {
            for axis in 0..3 {
                let mut center = [0.0; 3];
                center[axis] = sign * f32::MAX;
                let d = FluidDesc {
                    center: Vec3::new(center[0], center[1], center[2]),
                    half_extents: Vec3::splat(f32::MAX * 0.5),
                    max_particles: 8,
                    ..FluidDesc::default()
                };
                let fluid = Fluid::new(d);
                assert!(
                    fluid.is_none(),
                    "axis={axis}, sign={sign}: accepted bounds overflow; finite particles={}",
                    fluid
                        .as_ref()
                        .is_none_or(|f| f.particles().iter().all(|p| p.is_finite()))
                );
            }
        }
    }

    #[test]
    fn fluid_representable_boundary_remains_valid() {
        for sign in [-1.0, 1.0] {
            let d = FluidDesc {
                center: Vec3::splat(sign * f32::MAX * 0.5),
                half_extents: Vec3::splat(f32::MAX * 0.25),
                max_particles: 8,
                ..FluidDesc::default()
            };
            assert!(d.is_valid());
            let fluid = Fluid::new(d).unwrap();
            assert_eq!(fluid.particles().len(), 8);
            assert!(fluid.particles().iter().all(|p| p.is_finite()));
        }
        assert!(FluidDesc::default().is_valid());
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
    fn foam_does_not_fall_through_the_floor() {
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.add_builder(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(Vec3::ZERO),
        );
        let mut d = box_desc();
        d.center = Vec3::new(0.0, 1.2, 0.0);
        d.foam = true;
        d.foam_rate = 2000.0;
        d.foam_speed = 0.2;
        d.max_foam = 128;
        world.add_fluid(d).expect("参数合法");
        for _ in 0..75 {
            world.step(1.0 / 60.0);
        }
        let foam = world.fluids()[0].foam();
        assert!(!foam.is_empty(), "高速粒子应甩出泡沫");
        let min_y = foam
            .iter()
            .map(|f| f.position.y)
            .fold(f32::INFINITY, f32::min);
        assert!(
            min_y > -0.05,
            "泡沫不能穿过地板；min_y = {min_y}（{} 颗）",
            foam.len()
        );
    }

    #[test]
    fn bubbles_rise_and_do_not_fall_through_the_floor() {
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.add_builder(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(Vec3::ZERO),
        );
        let mut d = box_desc();
        d.bubbles = true;
        d.bubble_rate = 30.0;
        d.max_bubbles = 16;
        d.emit_position = Vec3::new(0.0, 0.5, 0.0);
        d.emit_radius = 0.1;
        d.drain_y = Some(-1.0);
        world.add_fluid(d).expect("参数合法");
        for _ in 0..120 {
            world.step(1.0 / 60.0);
        }
        let fluid = &world.fluids()[0];
        let bubbles = fluid.bubbles();
        assert!(!bubbles.is_empty(), "开启气泡后应生成气泡");
        assert!(bubbles.len() <= 16, "气泡不能超过上限：{}", bubbles.len());
        let min_y = bubbles
            .iter()
            .map(|b| b.position.y)
            .fold(f32::INFINITY, f32::min);
        let max_speed = bubbles
            .iter()
            .map(|b| b.velocity.length())
            .fold(0.0f32, f32::max);
        assert!(
            min_y > -0.1,
            "气泡不能穿过地板：min_y = {min_y}（{} 颗）",
            bubbles.len()
        );
        assert!(
            bubbles
                .iter()
                .all(|b| b.position.is_finite() && b.velocity.is_finite()),
            "气泡位置/速度必须有限"
        );
        assert!(max_speed < 20.0, "气泡速度失控：{max_speed}");
        assert_eq!(fluid.stats().bubbles, bubbles.len());
    }

    #[test]
    fn bubbles_expand_as_they_rise() {
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.set_gravity(Vec3::ZERO);
        let mut d = box_desc();
        d.bubbles = true;
        d.bubble_rate = 20.0;
        d.max_bubbles = 8;
        d.bubble_buoyancy = 0.0; // 不浮升，避免到达液面破裂
        d.bubble_expansion = 1.0;
        d.emit_position = Vec3::new(0.0, 0.8, 0.0);
        d.emit_radius = 0.1;
        world.add_fluid(d).expect("参数合法");
        for _ in 0..60 {
            world.step(1.0 / 60.0);
        }
        let bubbles = world.fluids()[0].bubbles();
        assert!(!bubbles.is_empty());
        let max_radius = bubbles.iter().map(|b| b.radius).fold(0.0f32, f32::max);
        assert!(
            max_radius > 0.03 * 1.2,
            "气泡应随上升膨胀：最大半径 {max_radius}（初始 0.03）"
        );
    }

    #[test]
    fn custom_particle_surface_controls_bubble_bursting() {
        for custom in [false, true] {
            for center_y in [0.0, 10.0] {
                for foam in [false, true] {
                    let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                    world
                        .add_fluid(FluidDesc {
                            center: vec3(0.0, center_y, 0.0),
                            initial_positions: custom.then(|| vec![Vec3::ZERO]),
                            max_particles: 8,
                            foam,
                            foam_rate: 0.0,
                            bubble_buoyancy: 0.0,
                            bubble_drag: 0.0,
                            bubble_expansion: 0.0,
                            ..FluidDesc::default()
                        })
                        .unwrap();
                    // Give both paths identical liquid geometry; only custom initialization
                    // opts out of the legacy center-height floor.
                    world.fluids_mut()[0].positions = vec![Vec3::ZERO];
                    world.fluids_mut()[0].velocities = vec![Vec3::ZERO];
                    world.fluids_mut()[0].add_bubble(Bubble {
                        position: vec3(0.0, 0.2, 0.0),
                        velocity: Vec3::ZERO,
                        radius: 0.05,
                        rest_radius: 0.05,
                        age: 0.0,
                        life: 2.0,
                    });
                    world.step(1.0 / 60.0);
                    let fluid = &world.fluids()[0];
                    let burst = custom || center_y == 0.0;
                    assert_eq!(
                        fluid.bubbles().len(),
                        usize::from(!burst),
                        "custom={custom}, center_y={center_y}, foam={foam}"
                    );
                    assert_eq!(fluid.foam().len(), usize::from(burst && foam));
                    assert_eq!(fluid.stats().bubbles, usize::from(!burst));
                    assert_eq!(fluid.stats().foam, usize::from(burst && foam));
                }
            }
        }
    }

    #[test]
    fn drained_bursting_bubbles_do_not_leave_foam() {
        for drain in [None, Some(2.0)] {
            for empty_before_step in [false, true] {
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                world
                    .add_fluid(FluidDesc {
                        center: Vec3::ZERO,
                        half_extents: Vec3::splat(0.2),
                        max_particles: 8,
                        drain_y: drain,
                        foam: true,
                        foam_rate: 0.0,
                        bubble_buoyancy: 0.0,
                        bubble_drag: 0.0,
                        bubble_expansion: 0.0,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                if empty_before_step {
                    // Use public stepping to drain the liquid before injecting the bubble.
                    if drain.is_some() {
                        world.step(1.0 / 60.0);
                        assert!(world.fluids()[0].particles().is_empty());
                    } else {
                        continue;
                    }
                }
                world.fluids_mut()[0].add_bubble(Bubble {
                    position: Vec3::Y,
                    velocity: Vec3::ZERO,
                    radius: 0.05,
                    rest_radius: 0.05,
                    age: 0.0,
                    life: 2.0,
                });
                world.step(1.0 / 60.0);
                let fluid = &world.fluids()[0];
                assert!(fluid.bubbles().is_empty());
                let expected = usize::from(drain.is_none());
                assert_eq!(fluid.foam().len(), expected,
                    "drain={drain:?}, empty_before_step={empty_before_step}: drained bubble must not leave foam");
                assert_eq!(fluid.stats().foam, expected);
                assert_eq!(fluid.stats().bubbles, 0);
            }
        }
    }

    #[test]
    fn bursting_bubbles_leave_foam_at_the_surface() {
        let mut world = PhysicsWorld::new();
        world.set_fixed_dt(1.0 / 60.0);
        world.add_builder(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(Vec3::ZERO),
        );
        let mut d = box_desc();
        d.bubbles = true;
        d.bubble_rate = 60.0;
        d.max_bubbles = 16;
        d.emit_position = Vec3::new(0.0, 0.5, 0.0);
        d.emit_radius = 0.1;
        d.foam = true;
        d.foam_rate = 0.0; // 只允许气泡破裂产生泡沫
        d.max_foam = 8;
        world.add_fluid(d).expect("参数合法");
        for _ in 0..240 {
            world.step(1.0 / 60.0);
        }
        let fluid = &world.fluids()[0];
        let foam = fluid.foam();
        assert!(!foam.is_empty(), "气泡到达液面破裂应留下泡沫；当前泡沫数 0");
        assert!(
            foam.iter().all(|f| f.kind == FoamKind::Foam),
            "气泡破裂产生的是泡沫，不是高速飞沫"
        );
        assert!(
            foam.iter()
                .all(|f| f.position.is_finite() && f.position.y > 0.0),
            "泡沫位置必须有限且在地面以上"
        );
    }

    #[test]
    fn touching_bubbles_coalesce_and_conserve_gas_volume() {
        let mut fluid = Fluid::new(box_desc()).unwrap();
        for x in [-0.05f32, 0.05] {
            fluid.add_bubble(Bubble {
                position: Vec3::new(x, 0.8, 0.0),
                velocity: Vec3::ZERO,
                radius: 0.1,
                rest_radius: 0.1,
                age: 0.0,
                life: 10.0,
            });
        }
        let bounds = Aabb::new(Vec3::splat(-1.0), Vec3::splat(1.0));
        let before = fluid.gas_volume_near(bounds);
        let mut bodies = [];
        fluid.step(&mut bodies, Vec3::ZERO, 1.0 / 60.0);
        assert_eq!(
            fluid.bubbles().len(),
            1,
            "两颗重叠气泡应合并成一颗，实际 {}",
            fluid.bubbles().len()
        );
        let after = fluid.gas_volume_near(bounds);
        assert!(
            (after - before).abs() <= before * 0.02,
            "合并必须近似守恒气体体积：before {before}, after {after}"
        );
    }

    #[test]
    fn bubble_pressure_pushes_liquid_outward_without_the_old_path() {
        fn run(pressure: f32) -> (f32, f32) {
            let mut fluid = Fluid::new(box_desc()).unwrap();
            fluid.desc.bubble_buoyancy = 0.0;
            fluid.desc.bubble_drag = 0.0;
            fluid.desc.viscosity = 0.0;
            fluid.desc.bubble_pressure = pressure;
            fluid.add_bubble(Bubble {
                position: Vec3::new(0.0, 1.0, 0.0),
                velocity: Vec3::ZERO,
                radius: 0.18,
                rest_radius: 0.18,
                age: 0.0,
                life: 10.0,
            });
            let mut bodies = [];
            fluid.step(&mut bodies, Vec3::ZERO, 1.0 / 60.0);
            let mut outward = 0.0f32;
            let mut max_speed = 0.0f32;
            for (p, v) in fluid.positions.iter().zip(fluid.velocities()) {
                let d = *p - Vec3::new(0.0, 1.0, 0.0);
                if d.length() <= 0.18 {
                    outward += v.dot(d.normalize_or_zero());
                }
                max_speed = max_speed.max(v.length());
            }
            (outward, max_speed)
        }
        let (off, off_speed) = run(0.0);
        let (on, on_speed) = run(1000.0);
        assert!(
            on > off + 0.01,
            "过压应让气泡周围液体获得额外径向速度：关 {off}，开 {on}"
        );
        assert!(
            on_speed.is_finite() && on_speed < 100.0,
            "压力速度必须有限：{on_speed}"
        );
        assert!(off_speed.is_finite());
    }

    #[test]
    fn bubble_momentum_recoil_reduces_injected_liquid_momentum() {
        fn added_mass(fluid: &Fluid, radius: f32) -> f32 {
            let volume = 4.0 / 3.0 * std::f32::consts::PI * radius.powi(3);
            0.5 * fluid.desc.rest_density.max(1.0) * volume
        }
        fn run(coupling: f32) -> (f32, f32, Vec3) {
            let mut fluid = Fluid::new(box_desc()).unwrap();
            fluid.desc.bubble_buoyancy = 0.0;
            fluid.desc.bubble_drag = 0.0;
            fluid.desc.viscosity = 0.0;
            fluid.desc.bubble_pressure = 1000.0;
            fluid.desc.bubble_momentum_coupling = coupling;
            fluid.add_bubble(Bubble {
                position: Vec3::new(0.0, 1.0, 0.0),
                velocity: Vec3::ZERO,
                radius: 0.18,
                rest_radius: 0.18,
                age: 0.0,
                life: 10.0,
            });
            let mut bodies = [];
            fluid.step(&mut bodies, Vec3::ZERO, 1.0 / 60.0);
            let liquid: Vec3 = fluid
                .velocities
                .iter()
                .fold(Vec3::ZERO, |sum, v| sum + *v * fluid.mass);
            let bubble = fluid.bubbles()[0];
            let gas = added_mass(&fluid, bubble.radius);
            let total = liquid + bubble.velocity * gas;
            (liquid.length(), total.length(), bubble.velocity)
        }
        let (off_liquid, off_total, _) = run(0.0);
        let (on_liquid, on_total, on_velocity) = run(1.0);
        assert!(off_liquid > 0.0, "过压必须真的把动量注入液体");
        assert!(
            on_total < off_total * 0.98,
            "反冲应让液体+气泡的总动量更接近守恒：关 {off_total}，开 {on_total}"
        );
        assert!(
            on_velocity.is_finite() && on_velocity.length() <= 10.0,
            "反冲不能让气泡速度发散：{on_velocity:?}"
        );
        // 液体本身的动量也应被反冲抽走一部分，而不是只改气泡。
        assert!(
            on_liquid <= off_liquid,
            "开反冲后液体动量不应增加：关 {off_liquid}，开 {on_liquid}"
        );
    }

    #[test]
    fn drain_compacts_particle_gas_and_density_samples() {
        for coupling in [true, false] {
            let mut reference = Fluid::new(FluidDesc {
                max_particles: 8,
                viscosity: 0.0,
                bubble_density_coupling: coupling,
                bubble_buoyancy: 0.0,
                bubble_drag: 0.0,
                ..box_desc()
            })
            .unwrap();
            reference.positions = vec![
                vec3(0.0, -1.0, 0.0),
                vec3(0.0, 1.0, 0.0),
                vec3(0.05, 1.0, 0.0),
            ];
            reference.velocities = vec![Vec3::ZERO; 3];
            reference.add_bubble(Bubble {
                position: vec3(0.0, -1.0, 0.0),
                velocity: Vec3::ZERO,
                radius: 0.05,
                rest_radius: 0.05,
                age: 0.0,
                life: 10.0,
            });
            for drain_y in [0.0, 10.0] {
                let mut full = reference.clone();
                let mut drained = reference.clone();
                drained.desc.drain_y = Some(drain_y);
                full.step(&mut [], Vec3::ZERO, 1.0 / 60.0);
                drained.step(&mut [], Vec3::ZERO, 1.0 / 60.0);
                let survivors: Vec<_> = full
                    .positions
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.y >= drain_y)
                    .map(|(i, _)| i)
                    .collect();
                assert_eq!(
                    drained.positions,
                    survivors
                        .iter()
                        .map(|&i| full.positions[i])
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    drained.velocities,
                    survivors
                        .iter()
                        .map(|&i| full.velocities[i])
                        .collect::<Vec<_>>()
                );
                let expected_gas: Vec<_> = if coupling {
                    survivors.iter().map(|&i| full.gas_fractions[i]).collect()
                } else {
                    vec![]
                };
                assert_eq!(
                    drained.gas_fractions(),
                    expected_gas,
                    "coupling={coupling}, drain_y={drain_y}"
                );
                let expected_density = if survivors.is_empty() {
                    0.0
                } else {
                    survivors
                        .iter()
                        .map(|&i| full.densities[i] / full.desc.rest_density)
                        .sum::<f32>()
                        / survivors.len() as f32
                };
                assert_eq!(
                    drained.stats().avg_density_ratio.to_bits(),
                    expected_density.to_bits(),
                    "survivor density stats"
                );
                assert_eq!(drained.stats().particles, survivors.len());
            }
        }
    }

    #[test]
    fn gas_density_coupling_expands_liquid_around_a_bubble() {
        let mut off = Fluid::new(box_desc()).unwrap();
        let mut on = Fluid::new(box_desc()).unwrap();
        off.desc.bubble_density_coupling = false;
        for f in [&mut off, &mut on] {
            f.desc.bubble_buoyancy = 0.0;
            f.desc.bubble_drag = 0.0;
            f.desc.viscosity = 0.0;
            f.add_bubble(Bubble {
                position: Vec3::new(0.0, 1.0, 0.0),
                velocity: Vec3::ZERO,
                radius: 0.18,
                rest_radius: 0.18,
                age: 0.0,
                life: 10.0,
            });
        }
        let mut bodies = [];
        for _ in 0..2 {
            off.step(&mut bodies, Vec3::ZERO, 1.0 / 60.0);
            on.step(&mut bodies, Vec3::ZERO, 1.0 / 60.0);
        }
        assert!(
            !on.gas_fractions().is_empty(),
            "开启密度耦合后应记录每个粒子的气体分数"
        );
        assert!(
            on.gas_fractions().iter().any(|g| *g > 0.01),
            "气泡附近的气体分数应大于 0：{:?}",
            on.gas_fractions().iter().fold(0.0f32, |a, b| a.max(*b))
        );
        assert!(off.gas_fractions().is_empty(), "关闭耦合时不应写入气体分数");
        let center = Vec3::new(0.0, 1.0, 0.0);
        let mean = |f: &Fluid| {
            f.positions
                .iter()
                .map(|p| (*p - center).length())
                .sum::<f32>()
                / f.positions.len() as f32
        };
        let (off_mean, on_mean) = (mean(&off), mean(&on));
        assert!(
            on_mean > off_mean * 1.005,
            "气体密度耦合应让液体在气泡周围膨胀：off {off_mean:.5}, on {on_mean:.5}"
        );
    }

    #[test]
    fn larger_bubbles_rise_faster() {
        // 浮力 ∝ r³、阻力 ∝ r²，终端速度 ∝ √r；同一时刻大泡应升得更高。
        let mut fluid = Fluid::new(box_desc()).unwrap();
        fluid.positions.clear();
        fluid.velocities.clear();
        fluid.desc.bubble_buoyancy = 1.0;
        fluid.desc.bubble_drag = 0.0;
        fluid.add_bubble(Bubble {
            position: Vec3::new(-0.2, 0.8, 0.0),
            velocity: Vec3::ZERO,
            radius: 0.03,
            rest_radius: 0.03,
            age: 0.0,
            life: 10.0,
        });
        fluid.add_bubble(Bubble {
            position: Vec3::new(0.2, 0.8, 0.0),
            velocity: Vec3::ZERO,
            radius: 0.12,
            rest_radius: 0.12,
            age: 0.0,
            life: 10.0,
        });
        for _ in 0..10 {
            fluid.update_bubbles(Vec3::new(0.0, -9.81, 0.0), 1.0 / 60.0, &[]);
        }
        assert_eq!(fluid.bubbles().len(), 2);
        let mut ys: Vec<f32> = fluid.bubbles().iter().map(|b| b.position.y).collect();
        ys.sort_by(f32::total_cmp);
        assert!(
            ys[1] > ys[0] + 1e-4,
            "大泡应比小泡升得快：small={}, large={}",
            ys[0],
            ys[1]
        );
    }

    #[test]
    fn deeper_bubbles_are_compressed_by_hydrostatic_pressure() {
        let mut fluid = Fluid::new(box_desc()).unwrap();
        fluid.positions.clear();
        fluid.velocities.clear();
        fluid.desc.bubble_compressibility = 2.0;
        fluid.desc.bubble_buoyancy = 0.0;
        fluid.desc.bubble_drag = 0.0;
        fluid.add_bubble(Bubble {
            position: Vec3::new(-0.2, 0.2, 0.0),
            velocity: Vec3::ZERO,
            radius: 0.1,
            rest_radius: 0.1,
            age: 0.0,
            life: 10.0,
        });
        fluid.add_bubble(Bubble {
            position: Vec3::new(0.2, 1.2, 0.0),
            velocity: Vec3::ZERO,
            radius: 0.1,
            rest_radius: 0.1,
            age: 0.0,
            life: 10.0,
        });
        for _ in 0..2 {
            fluid.update_bubbles(Vec3::ZERO, 1.0 / 60.0, &[]);
        }
        let deep = fluid
            .bubbles()
            .iter()
            .find(|b| b.position.x < 0.0)
            .unwrap()
            .radius;
        let shallow = fluid
            .bubbles()
            .iter()
            .find(|b| b.position.x > 0.0)
            .unwrap()
            .radius;
        assert!(
            deep < shallow * 0.95,
            "深处气泡应被压缩得更小：deep={deep:.5}, shallow={shallow:.5}"
        );
    }

    #[test]
    fn bubble_splitting_reserves_capacity_for_remaining_bubbles() {
        for cap in [8, 9] {
            for reverse in [false, true] {
                for enabled in [false, true] {
                    let mut fluid = Fluid::new(box_desc()).unwrap();
                    fluid.desc.max_bubbles = cap;
                    fluid.desc.bubble_max_radius = if enabled { 0.2 } else { 0.0 };
                    let large = Bubble {
                        position: Vec3::new(-2.0, 1.0, 0.0),
                        velocity: Vec3::new(0.25, 0.0, -0.5),
                        radius: 0.4,
                        rest_radius: 0.3,
                        age: 0.25,
                        life: 10.0,
                    };
                    let small = Bubble {
                        position: Vec3::new(2.0, 1.0, 0.0),
                        velocity: Vec3::new(-0.5, 0.0, 0.25),
                        radius: 0.1,
                        rest_radius: 0.08,
                        ..large
                    };
                    let original = if reverse {
                        vec![small, large]
                    } else {
                        vec![large, small]
                    };
                    fluid.bubbles = original.clone();
                    fluid.split_large_bubbles();
                    let split = enabled && cap == 9;
                    assert_eq!(
                        fluid.bubbles.len(),
                        if split { 9 } else { 2 },
                        "cap={cap}, reverse={reverse}, enabled={enabled}"
                    );
                    let volume = |bs: &[Bubble], rest: bool| -> f32 {
                        bs.iter()
                            .map(|b| if rest { b.rest_radius } else { b.radius }.powi(3))
                            .sum()
                    };
                    for rest in [false, true] {
                        let before = volume(&original, rest);
                        assert!((volume(&fluid.bubbles, rest) - before).abs() < before * 1e-5);
                    }
                    let momentum = |bs: &[Bubble]| {
                        bs.iter()
                            .fold(Vec3::ZERO, |sum, b| sum + b.velocity * b.radius.powi(3))
                    };
                    assert!((momentum(&fluid.bubbles) - momentum(&original)).length() < 1e-7);
                    for b in &fluid.bubbles {
                        let source = if b.position.x < 0.0 { large } else { small };
                        assert_eq!(b.velocity, source.velocity);
                        assert_eq!(b.age.to_bits(), source.age.to_bits());
                        assert_eq!(b.life.to_bits(), source.life.to_bits());
                    }
                    if !split {
                        for (a, b) in fluid.bubbles.iter().zip(&original) {
                            assert_eq!(a.position, b.position);
                            assert_eq!(a.radius.to_bits(), b.radius.to_bits());
                            assert_eq!(a.rest_radius.to_bits(), b.rest_radius.to_bits());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn large_bubbles_break_up_and_conserve_gas_volume() {
        let mut fluid = Fluid::new(box_desc()).unwrap();
        fluid.positions.clear();
        fluid.velocities.clear();
        fluid.desc.bubble_max_radius = 0.2;
        fluid.desc.bubble_buoyancy = 0.0;
        fluid.desc.bubble_drag = 0.0;
        fluid.add_bubble(Bubble {
            position: Vec3::new(0.0, 1.0, 0.0),
            velocity: Vec3::ZERO,
            radius: 0.4,
            rest_radius: 0.4,
            age: 0.0,
            life: 10.0,
        });
        let before = 4.0 / 3.0 * std::f32::consts::PI * 0.4f32.powi(3);
        fluid.update_bubbles(Vec3::ZERO, 1.0 / 60.0, &[]);
        assert!(
            fluid.bubbles().len() >= 2,
            "半径 0.4 m 的气泡在 max=0.2 时必须破碎，实际 {} 颗",
            fluid.bubbles().len()
        );
        assert!(
            fluid.bubbles().iter().all(|b| b.radius <= 0.21),
            "破碎后每颗都不应超过上限：{:?}",
            fluid.bubbles().iter().map(|b| b.radius).collect::<Vec<_>>()
        );
        let after: f32 = fluid
            .bubbles()
            .iter()
            .map(|b| 4.0 / 3.0 * std::f32::consts::PI * b.radius.powi(3))
            .sum();
        assert!(
            (after - before).abs() <= before * 0.02,
            "破碎必须守恒气体体积：before={before:.5}, after={after:.5}"
        );
    }

    #[test]
    fn rising_bubbles_drag_a_liquid_wake_upward() {
        let mut off = Fluid::new(box_desc()).unwrap();
        let mut on = Fluid::new(box_desc()).unwrap();
        off.desc.bubble_wake = 0.0;
        on.desc.bubble_wake = 1.0;
        for f in [&mut off, &mut on] {
            f.desc.bubble_buoyancy = 0.0;
            f.desc.bubble_drag = 0.0;
            f.desc.viscosity = 0.0;
            f.add_bubble(Bubble {
                position: Vec3::new(0.0, 0.75, 0.0),
                velocity: Vec3::new(0.0, 0.5, 0.0),
                radius: 0.12,
                rest_radius: 0.12,
                age: 0.0,
                life: 10.0,
            });
        }
        let mut bodies = [];
        for _ in 0..3 {
            off.step(&mut bodies, Vec3::new(0.0, -9.81, 0.0), 1.0 / 60.0);
            on.step(&mut bodies, Vec3::new(0.0, -9.81, 0.0), 1.0 / 60.0);
        }
        let mean_vy =
            |f: &Fluid| f.velocities.iter().map(|v| v.y).sum::<f32>() / f.velocities.len() as f32;
        let (off_vy, on_vy) = (mean_vy(&off), mean_vy(&on));
        assert!(
            on_vy > off_vy + 1e-4,
            "上浮气泡应向上拖动尾流：off={off_vy:.6}, on={on_vy:.6}"
        );
    }

    #[test]
    fn flow_velocity_requires_particles_inside_the_box() {
        let fluid = Fluid::new(box_desc()).unwrap();
        assert!(
            fluid
                .flow_velocity_near(Aabb::new(Vec3::splat(100.0), Vec3::splat(101.0)))
                .is_none(),
            "没有粒子时不能返回零流速（否则会变成额外阻尼）"
        );
        let whole = Aabb::new(-Vec3::splat(10.0), Vec3::splat(10.0));
        assert!(
            fluid.flow_velocity_near(whole).is_some(),
            "初始粒子在盒内，应能取到平均速度"
        );
    }

    #[test]
    fn bubbles_are_off_by_default() {
        let fluid = Fluid::new(box_desc()).unwrap();
        assert!(fluid.bubbles().is_empty());
        assert_eq!(fluid.stats().bubbles, 0);
    }

    #[test]
    fn bubbles_rise_and_displace_liquid_particles() {
        fn run(bubbles: bool) -> (Vec<Vec3>, f32) {
            let mut world = PhysicsWorld::new();
            world.set_fixed_dt(1.0 / 60.0);
            world.add_builder(
                BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(Vec3::ZERO),
            );
            let mut d = box_desc();
            d.bubbles = bubbles;
            d.bubble_rate = 400.0;
            d.max_bubbles = 32;
            d.emit_position = Vec3::new(0.0, 0.8, 0.0);
            d.emit_radius = 0.2;
            world.add_fluid(d).expect("参数合法");
            for _ in 0..60 {
                world.step(1.0 / 60.0);
            }
            let fluid = &world.fluids()[0];
            let max_bubble_y = fluid
                .bubbles()
                .iter()
                .map(|b| b.position.y)
                .fold(f32::NEG_INFINITY, f32::max);
            (fluid.particles().to_vec(), max_bubble_y)
        }
        let (without, _) = run(false);
        let (with, bubble_y) = run(true);
        assert!(bubble_y > 0.8, "气泡必须向上浮：最高 y = {bubble_y}");
        let moved = with
            .iter()
            .zip(&without)
            .map(|(a, b)| (*a - *b).length())
            .fold(0.0f32, f32::max);
        assert!(moved > 1e-3, "气泡体积内的液体会被排开；最大位移 = {moved}");
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
    fn particle_collision_separates_nearly_coincident_pairs_in_their_direction() {
        for delta in [
            Vec3::ZERO,
            vec3(-5e-7, 0.0, 0.0),
            vec3(5e-7, 0.0, 0.0),
            vec3(0.0, 5e-7, 0.0),
            vec3(0.0, -5e-7, 0.0),
            vec3(0.0, 0.0, 5e-7),
            vec3(0.0, 0.0, -5e-7),
        ] {
            for collision in [false, true] {
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                world
                    .add_fluid(FluidDesc {
                        half_extents: Vec3::splat(0.2),
                        spacing: 0.1,
                        radius: 0.05,
                        iterations: 1,
                        max_particles: 8,
                        viscosity: 0.0,
                        particle_collision: collision,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                let fluid = &mut world.fluids_mut()[0];
                fluid.positions = vec![delta, Vec3::ZERO];
                fluid.velocities = vec![Vec3::ZERO; 2];
                world.step(1.0 / 60.0);
                let positions = world.fluids()[0].particles();
                let actual = positions[0] - positions[1];
                if collision {
                    // 2e-8 is several ULPs at 0.1, but far below the 5e-7 gap.
                    assert!(
                        actual.length() >= 0.1 - 2e-8,
                        "delta={delta:?}: separation={}",
                        actual.length()
                    );
                    if delta != Vec3::ZERO {
                        assert!(
                            actual.dot(delta) > 0.0,
                            "separation reversed: {delta:?} -> {actual:?}"
                        );
                        assert!(actual.cross(delta / delta.length()).length() < 2e-8);
                    }
                } else {
                    for (got, expected) in positions.iter().zip([delta, Vec3::ZERO]) {
                        assert_eq!(
                            [got.x.to_bits(), got.y.to_bits(), got.z.to_bits()],
                            [
                                expected.x.to_bits(),
                                expected.y.to_bits(),
                                expected.z.to_bits()
                            ]
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn particle_collision_separates_overlapping_neighbors() {
        fn run(collision: bool) -> f32 {
            let desc = FluidDesc {
                center: vec3(0.0, 1.0, 0.0),
                half_extents: vec3(0.2, 0.2, 0.2),
                spacing: 0.1,
                radius: 0.05,
                iterations: 1,
                max_particles: 8,
                particle_collision: collision,
                ..FluidDesc::default()
            };
            let mut fluid = Fluid::new(desc).expect("参数合法");
            // 只留两颗几乎重合的粒子，绕过晶格初始间距。
            fluid.positions = vec![vec3(0.0, 1.0, 0.0), vec3(0.01, 1.0, 0.0)];
            fluid.velocities = vec![Vec3::ZERO, Vec3::ZERO];
            fluid.contacts = vec![None, None];
            fluid.step(&mut [], Vec3::ZERO, 1.0 / 60.0);
            (fluid.positions[0] - fluid.positions[1]).length()
        }
        let without = run(false);
        let with = run(true);
        assert!(
            with >= 0.099,
            "粒子碰撞应把两颗粒子推到直径以外：without={without}, with={with}"
        );
        assert!(
            with > without,
            "碰撞开关应比纯 PBF 更分离：without={without}, with={with}"
        );
    }

    #[test]
    fn contact_friction_slows_a_sliding_fluid_on_a_static_floor() {
        let make = || FluidDesc {
            center: vec3(0.0, 0.25, 0.0),
            half_extents: vec3(0.2, 0.2, 0.2),
            spacing: 0.1,
            radius: 0.05,
            initial_velocity: vec3(3.0, 0.0, 0.0),
            max_particles: 64,
            ..FluidDesc::default()
        };
        let run = |mu: f32| {
            let mut world = PhysicsWorld::new();
            world.set_fixed_dt(1.0 / 60.0);
            world.add_builder(
                BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(Vec3::ZERO),
            );
            world.add_fluid(FluidDesc {
                contact_friction: mu,
                ..make()
            });
            for _ in 0..180 {
                world.step(1.0 / 60.0);
            }
            let fluid = &world.fluids()[0];
            let mean_x =
                fluid.particles().iter().map(|p| p.x).sum::<f32>() / fluid.particles().len() as f32;
            (mean_x, fluid.stats().max_speed)
        };
        let (off_x, _) = run(0.0);
        let (on_x, _) = run(1.0);
        assert!(
            on_x < off_x - 0.05,
            "摩擦应让水柱少滑一段：off={off_x}, on={on_x}"
        );
    }

    #[test]
    fn contact_friction_drags_a_dynamic_body_with_the_flow() {
        let run = |mu: f32| {
            let mut world = PhysicsWorld::new();
            world.set_fixed_dt(1.0 / 60.0);
            world.add_builder(
                BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }).position(Vec3::ZERO),
            );
            let body = world.add_builder(
                BodyBuilder::dynamic(Shape::Box {
                    half_extents: vec3(0.3, 0.1, 0.3),
                })
                .position(vec3(0.0, 0.11, 0.0))
                .density(500.0)
                .friction(0.0)
                .can_sleep(false),
            );
            world.add_fluid(FluidDesc {
                center: vec3(-1.2, 0.8, 0.0),
                half_extents: vec3(0.2, 0.2, 0.2),
                spacing: 0.1,
                radius: 0.05,
                initial_velocity: vec3(4.0, 0.0, 0.0),
                contact_friction: mu,
                max_particles: 64,
                ..FluidDesc::default()
            });
            for _ in 0..120 {
                world.step(1.0 / 60.0);
            }
            let b = world.body(body).unwrap();
            let x = b.transform().position.x;
            let fluid_x = world.fluids()[0]
                .particles()
                .iter()
                .map(|p| p.x)
                .sum::<f32>()
                / world.fluids()[0].particles().len() as f32;
            (x, fluid_x)
        };
        let (off_body, off_fluid) = run(0.0);
        let (on_body, on_fluid) = run(1.0);
        assert!(
            on_body > off_body + 0.005,
            "水应把动态刚体往流向拖：off={off_body}, on={on_body}"
        );
        assert!(
            off_fluid.is_finite() && on_fluid.is_finite(),
            "摩擦不能把粒子速度变成 NaN"
        );
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
