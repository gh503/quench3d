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

#[cfg(test)]
type DensityTraceEntry = (usize, Vec3, Vec3, Vec3);
#[cfg(test)]
type SeparationSegmentEntry = (Vec3, Vec3, Vec3);

// Round165 attribution is deliberately test-only: no release fields, branches or math.
#[cfg(test)]
#[derive(Debug)]
struct OverlapPhase {
    step: usize,
    phase: &'static str,
    positions: [Vec3; 2],
    minimum: (f64, usize, usize),
    candidate: bool,
}

#[cfg(test)]
#[derive(Debug)]
struct OverlapPair {
    step: usize,
    pair: [usize; 2],
    before: [Vec3; 2],
    intended: [Vec3; 2],
    after: [Vec3; 2],
    target_after: [Vec3; 2],
    boundary_limited: bool,
}

#[cfg(test)]
#[derive(Debug)]
struct OverlapTrace {
    step: usize,
    target: [usize; 2],
    phases: Vec<OverlapPhase>,
    pairs: Vec<OverlapPair>,
}

#[cfg(test)]
fn overlap_distance(a: Vec3, b: Vec3) -> f64 {
    ((f64::from(a.x) - f64::from(b.x)).powi(2)
        + (f64::from(a.y) - f64::from(b.y)).powi(2)
        + (f64::from(a.z) - f64::from(b.z)).powi(2))
    .sqrt()
}

#[cfg(test)]
fn overlap_minimum(positions: &[Vec3]) -> (f64, usize, usize) {
    let mut result = (f64::INFINITY, 0, 0);
    for (i, &p) in positions.iter().enumerate() {
        for (j, &q) in positions.iter().enumerate().skip(i + 1) {
            let distance = overlap_distance(p, q);
            if distance < result.0 {
                result = (distance, i, j);
            }
        }
    }
    result
}

#[cfg(test)]
fn trace_overlap_phase(phase: &'static str, positions: &[Vec3], neighbors: &[Vec<u32>]) {
    OVERLAP_TRACE.with(|trace| {
        if let Some(trace) = trace.borrow_mut().as_mut() {
            let [i, j] = trace.target;
            assert!(trace.phases.len() < 2048, "bounded phase trace");
            trace.phases.push(OverlapPhase {
                step: trace.step,
                phase,
                positions: [positions[i], positions[j]],
                minimum: overlap_minimum(positions),
                candidate: neighbors
                    .get(i)
                    .is_some_and(|row| row.contains(&(j as u32))),
            });
        }
    });
}

#[cfg(test)]
fn separation_cost_add(site: usize, amount: u64) {
    SEPARATION_COST.with(|cost| {
        if let Some(mut counts) = cost.get() {
            counts[site] += amount;
            cost.set(Some(counts));
        }
    });
}

#[cfg(test)]
thread_local! {
    static OVERLAP_TRACE: std::cell::RefCell<Option<OverlapTrace>> = const { std::cell::RefCell::new(None) };
    static SEPARATION_SEGMENT_TRACE: std::cell::RefCell<Option<Vec<SeparationSegmentEntry>>> = const { std::cell::RefCell::new(None) };
    static MESH_PROJECTION_WORK: std::cell::Cell<[usize; 4]> = const { std::cell::Cell::new([0; 4]) };
    static MOVING_MESH_REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static MOVING_MESH_BOUNDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static MOVING_MESH_WORK: std::cell::Cell<[usize; 2]> = const { std::cell::Cell::new([0; 2]) };
    static MOVING_SCAN_WORK: std::cell::Cell<[usize; 4]> = const { std::cell::Cell::new([0; 4]) };
    static MOVING_SHELL_CARRY: std::cell::Cell<Option<(Vec3, Vec3)>> = const { std::cell::Cell::new(None) };
    static SEPARATION_COST: std::cell::Cell<Option<[u64; 4]>> = const { std::cell::Cell::new(None) };
    static SEPARATION_REDUNDANCY: std::cell::Cell<Option<[u64; 6]>> = const { std::cell::Cell::new(None) };
    static SEPARATION_PUSHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static MESH_CONTINUITY_TRACE: std::cell::RefCell<Option<Vec<DensityTraceEntry>>> = const { std::cell::RefCell::new(None) };
    static MESH_DENSITY_TRACE: std::cell::RefCell<Option<Vec<DensityTraceEntry>>> = const { std::cell::RefCell::new(None) };
    static MESH_CORRECTION_TRACE: std::cell::RefCell<Option<Vec<(usize, Vec3, Vec3)>>> = const { std::cell::RefCell::new(None) };
    static SWEEP_CACHE_REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static SWEEP_BOUNDS_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SWEEP_INDEX_REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static SWEEP_INDEX_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static CANDIDATE_HEAP_SITES: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    static SWEEP_BOUNDS_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SWEEP_WORK: std::cell::Cell<[usize; 3]> = const { std::cell::Cell::new([0; 3]) };
    static MESH_SWEEP_WORK: std::cell::Cell<[usize; 2]> = const { std::cell::Cell::new([0; 2]) };
    static MESH_SWEEP_REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static MESH_SWEEP_BOUNDS_CHECKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static SWEEP_REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
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

/// Liquid-particle movement collision policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FluidCollisionMode {
    /// Preserve historical discrete projection.
    #[default]
    Legacy,
    /// Sweep initial particle motion against static sphere parts.
    SweptSpheres,
    /// Sweep initial motion, PBF density corrections and pair separation against static spheres and boxes.
    /// Inward motion from the particle-clearance shell is held at contact.
    SweptPrimitives,
    /// Sweep liquid segments against static spheres, boxes and triangle meshes.
    SweptMeshes,
}

/// Particle-pair separation policy, independent of density iteration count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FluidSeparationMode {
    /// Preserve the historical equal-half, single-pass correction arithmetic.
    #[default]
    Legacy,
    /// Opt in to bounded boundary-constrained pair corrections.
    /// Requires particle collision and swept primitive or mesh collision.
    BoundaryConstrained,
    #[cfg(test)]
    GlobalHalfspaceExperimental,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ExperimentalSeparationStatus {
    #[default]
    NotRun,
    StubUncertified,
    Certified,
}

/// Temporal motion policy for rigid liquid boundaries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FluidBoundaryMotionMode {
    /// Preserve historical static-only sweeps.
    #[default]
    Legacy,
    /// Carry the first contact with a translating kinematic primary or BodyLocal extra sphere.
    /// Requires SweptMeshes for both static safety segments.
    TranslatingKinematicSpheres,
    /// Carry translating kinematic spheres and boxes, including BodyLocal parts.
    /// Requires SweptMeshes; rotation and repeated moving impacts are excluded.
    TranslatingKinematicPrimitives,
    /// Carry translating kinematic spheres, boxes and triangle meshes.
    /// Requires SweptMeshes; only the first moving impact is handled.
    TranslatingKinematicMeshes,
}

struct CachedSweepTriangle {
    points: [Vec3; 3],
    bounds: Aabb,
}

struct MovingMeshPart {
    delta: Vec3,
    triangles: Vec<CachedSweepTriangle>,
}

struct MovingMeshCache {
    bodies: Vec<Vec<Option<MovingMeshPart>>>,
}

impl MovingMeshCache {
    fn build(bodies: &[BodySlot]) -> Self {
        Self {
            bodies: bodies
                .iter()
                .map(|slot| {
                    let Some(body) = &slot.body else {
                        return Vec::new();
                    };
                    if body.sensor
                        || body.body_type != crate::body::BodyType::Kinematic
                        || body.angular_velocity != Vec3::ZERO
                        || body.prev_transform.rotation != body.transform.rotation
                    {
                        return Vec::new();
                    }
                    body.parts()
                        .enumerate()
                        .map(|(part, (shape, local))| {
                            if part != 0
                                && body.compound_transform_mode()
                                    != crate::body::CompoundTransformMode::BodyLocal
                            {
                                return None;
                            }
                            let crate::shape::Shape::TriMesh { vertices, indices } = shape else {
                                return None;
                            };
                            let (previous, current) = if part == 0 {
                                (body.prev_transform, body.transform)
                            } else {
                                (
                                    body.prev_transform.mul_transform(&local),
                                    body.transform.mul_transform(&local),
                                )
                            };
                            let delta = current.position - previous.position;
                            if !delta.is_finite() || delta == Vec3::ZERO {
                                return None;
                            }
                            let triangles = indices
                                .as_chunks::<3>()
                                .0
                                .iter()
                                .filter_map(|tri| {
                                    let points = [
                                        *vertices.get(tri[0] as usize)?,
                                        *vertices.get(tri[1] as usize)?,
                                        *vertices.get(tri[2] as usize)?,
                                    ]
                                    .map(|p| previous.transform_point(p));
                                    #[cfg(test)]
                                    MOVING_MESH_WORK.with(|n| {
                                        let mut w = n.get();
                                        w[0] += 1;
                                        n.set(w);
                                    });
                                    points.iter().all(|p| p.is_finite()).then(|| {
                                        CachedSweepTriangle {
                                            points,
                                            bounds: Aabb::from_points(&points),
                                        }
                                    })
                                })
                                .collect();
                            Some(MovingMeshPart { delta, triangles })
                        })
                        .collect()
                })
                .collect(),
        }
    }
}

struct MeshSweepCache {
    bodies: Vec<Vec<Option<Vec<CachedSweepTriangle>>>>,
}

impl MeshSweepCache {
    fn build(bodies: &[BodySlot]) -> Self {
        Self {
            bodies: bodies
                .iter()
                .map(|slot| {
                    let Some(body) = &slot.body else {
                        return Vec::new();
                    };
                    if body.sensor || body.body_type != crate::body::BodyType::Static {
                        return Vec::new();
                    }
                    body.parts()
                        .map(|(shape, local)| {
                            let crate::shape::Shape::TriMesh { vertices, indices } = shape else {
                                return None;
                            };
                            let world = body.part_world_transform(&local);
                            Some(
                                indices
                                    .as_chunks::<3>()
                                    .0
                                    .iter()
                                    .filter_map(|tri| {
                                        let points = [
                                            *vertices.get(tri[0] as usize)?,
                                            *vertices.get(tri[1] as usize)?,
                                            *vertices.get(tri[2] as usize)?,
                                        ]
                                        .map(|p| world.transform_point(p));
                                        #[cfg(test)]
                                        MESH_SWEEP_WORK.with(|n| {
                                            let mut work = n.get();
                                            work[0] += 1;
                                            n.set(work);
                                        });
                                        points.iter().all(|p| p.is_finite()).then(|| {
                                            CachedSweepTriangle {
                                                bounds: Aabb::from_points(&points),
                                                points,
                                            }
                                        })
                                    })
                                    .collect(),
                            )
                        })
                        .collect()
                })
                .collect(),
        }
    }
}

#[derive(Clone, Copy)]
struct SweepQueryContext<'a> {
    meshes: bool,
    mesh_cache: Option<&'a MeshSweepCache>,
    bounds: Option<&'a [Option<Aabb>]>,
    index: Option<&'a SweepBodyIndex>,
}

/// Per-step sorted interval prefix; queries preserve original body encounter order.
struct SweepBodyIndex {
    ordered: Vec<(usize, Aabb)>,
    fallback: Vec<usize>,
    scale: f32,
}
impl SweepBodyIndex {
    fn build(bodies: &[BodySlot], bounds: &[Option<Aabb>]) -> Self {
        let mut index = Self {
            ordered: Vec::new(),
            fallback: Vec::new(),
            scale: 1.0,
        };
        for (slot, entry) in bodies.iter().enumerate() {
            let Some(body) = &entry.body else { continue };
            if body.sensor || body.body_type != crate::body::BodyType::Static {
                continue;
            }
            if let Some(aabb) = bounds[slot] {
                index.scale = index
                    .scale
                    .max(aabb.min.abs().max(aabb.max.abs()).max_element());
                index.ordered.push((slot, aabb));
            } else {
                index.fallback.push(slot);
            }
        }
        index
            .ordered
            .sort_by(|a, b| a.1.min.x.total_cmp(&b.1.min.x).then(a.0.cmp(&b.0)));
        index
    }
    fn candidates(&self, start: Vec3, end: Vec3, radius: f32) -> Option<Vec<usize>> {
        if !start.is_finite() || !end.is_finite() {
            return None;
        }
        let scale = self.scale.max(start.abs().max(end.abs()).max_element());
        let margin = radius + 64.0 * f32::EPSILON * scale;
        let segment = Aabb {
            min: start.min(end) - Vec3::splat(margin),
            max: start.max(end) + Vec3::splat(margin),
        };
        if !margin.is_finite() || margin < 0.0 || !segment.is_valid() {
            return None;
        }
        let count = self.ordered.partition_point(|(_, bounds)| {
            #[cfg(test)]
            SWEEP_INDEX_VISITS.with(|n| n.set(n.get() + 1));
            bounds.min.x <= segment.max.x
        });
        #[cfg(test)]
        CANDIDATE_HEAP_SITES.with(|site| {
            if let Some(n) = site.get() {
                site.set(Some(n + 1));
            }
        });
        let mut slots = self.fallback.clone();
        #[cfg(test)]
        SWEEP_INDEX_VISITS.with(|n| n.set(n.get() + slots.len()));
        for &(slot, bounds) in &self.ordered[..count] {
            #[cfg(test)]
            SWEEP_INDEX_VISITS.with(|n| n.set(n.get() + 1));
            if bounds.intersects(&segment) {
                slots.push(slot);
            }
        }
        slots.sort_unstable();
        Some(slots)
    }
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
    /// Liquid-particle sweep policy; defaults to historical projection.
    pub collision_mode: FluidCollisionMode,
    /// Temporal boundary motion policy; defaults to historical behavior.
    pub boundary_motion_mode: FluidBoundaryMotionMode,
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
    /// （各一半），再重新投影到刚体边界。默认每步一次确定邻居遍历，
    /// 后续粒子对及边界限制可能留下重叠，不保证完全分离或不可压缩。
    pub particle_collision: bool,
    /// Rust API opt-in policy for particle separation; defaults to Legacy.
    /// Scene serialization currently exposes only the historical policy.
    pub separation_mode: FluidSeparationMode,
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
            collision_mode: FluidCollisionMode::Legacy,
            boundary_motion_mode: FluidBoundaryMotionMode::Legacy,
            iterations: 4,
            max_particles: 4000,
            foam: false,
            foam_speed: 3.0,
            foam_rate: 120.0,
            max_foam: 256,
            particle_collision: false,
            separation_mode: FluidSeparationMode::Legacy,
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
        (self.boundary_motion_mode == FluidBoundaryMotionMode::Legacy
            || self.collision_mode == FluidCollisionMode::SweptMeshes)
            && (self.separation_mode == FluidSeparationMode::Legacy
                || (self.particle_collision
                    && matches!(
                        self.collision_mode,
                        FluidCollisionMode::SweptPrimitives | FluidCollisionMode::SweptMeshes
                    )))
            && self.center.is_finite()
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
    #[cfg(test)]
    experimental_separation_status: ExperimentalSeparationStatus,
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
            #[cfg(test)]
            experimental_separation_status: ExperimentalSeparationStatus::NotRun,
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
    fn separate_particles(
        &mut self,
        radius: f32,
        bodies: &[BodySlot],
        bounds: Option<&[Option<Aabb>]>,
        index: Option<&SweepBodyIndex>,
        mesh_cache: Option<&MeshSweepCache>,
    ) {
        #[cfg(test)]
        if self.desc.separation_mode == FluidSeparationMode::GlobalHalfspaceExperimental {
            // Stage-one stub: execute the actual old operator, not a second algorithm.
            // Construction/copying is deliberately excluded from performance claims.
            let mut baseline = Fluid::new(FluidDesc {
                separation_mode: FluidSeparationMode::BoundaryConstrained,
                ..self.desc.clone()
            })
            .expect("validated experimental fixture");
            baseline.predicted = self.predicted.clone();
            baseline.neighbors = self.neighbors.clone();
            baseline.separate_particles(radius, bodies, bounds, index, mesh_cache);
            self.predicted = baseline.predicted;
            self.experimental_separation_status = ExperimentalSeparationStatus::StubUncertified;
            return;
        }
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
                #[cfg(test)]
                if OVERLAP_TRACE
                    .with(|trace| trace.borrow().as_ref().is_some_and(|t| t.target == [i, j]))
                {
                    trace_overlap_phase("target_pair_visit", predicted, neighbors);
                }
                let delta = predicted[i] - predicted[j];
                let dist = delta.length();
                if !dist.is_finite() || dist >= min_dist {
                    continue;
                }
                // 非零距离保留连线方向；完全重合或长度下溢时用确定性 X，避免 0/0。
                let dir = if dist > 0.0 { delta / dist } else { Vec3::X };
                let push = (min_dist - dist) * 0.5;
                #[cfg(test)]
                separation_cost_add(0, 1);
                #[cfg(test)]
                SEPARATION_PUSHES.with(|count| count.set(count.get() + 1));
                #[cfg(test)]
                MESH_CORRECTION_TRACE.with(|trace| {
                    if let Some(trace) = trace.borrow_mut().as_mut() {
                        trace.push((i, predicted[i], predicted[i] + dir * push));
                        trace.push((j, predicted[j], predicted[j] - dir * push));
                    }
                });
                #[cfg(test)]
                let continuity_before = (predicted[i], predicted[j]);
                if self.desc.separation_mode == FluidSeparationMode::BoundaryConstrained {
                    let context = SweepQueryContext {
                        mesh_cache,
                        bounds,
                        index,
                        meshes: self.desc.collision_mode == FluidCollisionMode::SweptMeshes,
                    };
                    let (a, blocked_a) = sweep_separation_slide(
                        predicted[i],
                        dir * push,
                        bodies,
                        radius,
                        &self.desc,
                        context,
                    );
                    let (b, blocked_b) = sweep_separation_slide(
                        predicted[j],
                        -dir * push,
                        bodies,
                        radius,
                        &self.desc,
                        context,
                    );
                    predicted[i] = a;
                    predicted[j] = b;
                    // One bounded residual correction only when a boundary blocked
                    // the first correction. Free-space arithmetic stays identical.
                    if blocked_a || blocked_b {
                        let delta = a - b;
                        let distance = delta.length();
                        if distance.is_finite() && distance > 0.0 && distance < min_dist {
                            #[cfg(test)]
                            separation_cost_add(3, 1);
                            let correction = delta / distance * ((min_dist - distance) * 0.5);
                            predicted[i] = sweep_separation_slide(
                                a, correction, bodies, radius, &self.desc, context,
                            )
                            .0;
                            predicted[j] = sweep_separation_slide(
                                b,
                                -correction,
                                bodies,
                                radius,
                                &self.desc,
                                context,
                            )
                            .0;
                        }
                    }
                } else if matches!(
                    self.desc.collision_mode,
                    FluidCollisionMode::SweptPrimitives | FluidCollisionMode::SweptMeshes
                ) {
                    #[cfg(test)]
                    separation_cost_add(1, 2);
                    predicted[i] = sweep_liquid_correction(
                        predicted[i],
                        predicted[i] + dir * push,
                        bodies,
                        radius,
                        &self.desc,
                        SweepQueryContext {
                            mesh_cache,
                            bounds,
                            index,
                            meshes: self.desc.collision_mode == FluidCollisionMode::SweptMeshes,
                        },
                    )
                    .0;
                    predicted[j] = sweep_liquid_correction(
                        predicted[j],
                        predicted[j] - dir * push,
                        bodies,
                        radius,
                        &self.desc,
                        SweepQueryContext {
                            mesh_cache,
                            bounds,
                            index,
                            meshes: self.desc.collision_mode == FluidCollisionMode::SweptMeshes,
                        },
                    )
                    .0;
                } else {
                    predicted[i] += dir * push;
                    predicted[j] -= dir * push;
                }
                #[cfg(test)]
                OVERLAP_TRACE.with(|trace| {
                    if let Some(trace) = trace.borrow_mut().as_mut() {
                        if trace.target.contains(&i) || trace.target.contains(&j) {
                            let intended = [
                                continuity_before.0 + dir * push,
                                continuity_before.1 - dir * push,
                            ];
                            let after = [predicted[i], predicted[j]];
                            assert!(trace.pairs.len() < 60_000, "bounded target pair trace");
                            trace.pairs.push(OverlapPair {
                                step: trace.step,
                                pair: [i, j],
                                before: [continuity_before.0, continuity_before.1],
                                intended,
                                after,
                                target_after: [
                                    predicted[trace.target[0]],
                                    predicted[trace.target[1]],
                                ],
                                boundary_limited: intended != after,
                            });
                        }
                    }
                });
                #[cfg(test)]
                MESH_CONTINUITY_TRACE.with(|trace| {
                    if let Some(trace) = trace.borrow_mut().as_mut() {
                        trace.push((
                            i,
                            continuity_before.0,
                            continuity_before.0 + dir * push,
                            predicted[i],
                        ));
                        trace.push((
                            j,
                            continuity_before.1,
                            continuity_before.1 - dir * push,
                            predicted[j],
                        ));
                    }
                });
            }
        }
        #[cfg(test)]
        trace_overlap_phase(
            "separation_post_before_final_wall",
            &self.predicted,
            &self.neighbors,
        );
        // 分离可能把粒子推进墙；重新投影一次（不做速度/摩擦）。
        #[cfg(test)]
        let continuity_before = MESH_CONTINUITY_TRACE
            .with(|trace| trace.borrow().as_ref().map(|_| self.predicted.clone()));
        for p in &mut self.predicted {
            *p = collide_position(
                *p,
                *p,
                bodies,
                radius,
                self.desc.boundary_velocity_mode,
                self.desc.boundary_motion_mode,
            )
            .0;
        }
        #[cfg(test)]
        trace_overlap_phase("final_wall_post", &self.predicted, &self.neighbors);
        #[cfg(test)]
        if let Some(before) = continuity_before {
            MESH_CONTINUITY_TRACE.with(|trace| {
                if let Some(trace) = trace.borrow_mut().as_mut() {
                    for (i, (a, b)) in before.iter().zip(&self.predicted).enumerate() {
                        trace.push((i, *a, *a, *b));
                    }
                }
            });
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

        #[cfg(test)]
        let cache_enabled = !SWEEP_CACHE_REFERENCE.with(|flag| flag.get());
        #[cfg(not(test))]
        let cache_enabled = true;
        // Owned, slot-aligned bounds live only for this fluid step. All sweeps
        // precede body coupling, and none of those phases mutate body transforms.
        let sweep_bounds = if cache_enabled
            && matches!(
                self.desc.collision_mode,
                FluidCollisionMode::SweptPrimitives | FluidCollisionMode::SweptMeshes
            ) {
            Some(
                bodies
                    .iter()
                    .map(|slot| {
                        let body = slot.body.as_ref()?;
                        if body.sensor
                            || body.body_type != crate::body::BodyType::Static
                            || !(body.part_count() == 1
                                || body.compound_transform_mode()
                                    == crate::body::CompoundTransformMode::BodyLocal)
                        {
                            return None;
                        }
                        #[cfg(test)]
                        SWEEP_BOUNDS_BUILDS.with(|count| count.set(count.get() + 1));
                        let bounds = body.world_aabb();
                        bounds.is_valid().then_some(bounds)
                    })
                    .collect::<Vec<_>>(),
            )
        } else {
            None
        };

        #[cfg(test)]
        let index_enabled = !SWEEP_INDEX_REFERENCE.with(|flag| flag.get());
        #[cfg(not(test))]
        let index_enabled = true;
        // Zero/one static candidate cannot benefit from an interval index.
        let sweep_index = if index_enabled
            && sweep_bounds.is_some()
            && bodies
                .iter()
                .filter(|slot| {
                    slot.body.as_ref().is_some_and(|body| {
                        !body.sensor && body.body_type == crate::body::BodyType::Static
                    })
                })
                .take(2)
                .count()
                > 1
        {
            sweep_bounds
                .as_ref()
                .map(|bounds| SweepBodyIndex::build(bodies, bounds))
        } else {
            None
        };

        #[cfg(test)]
        let mesh_cache_enabled = !MESH_SWEEP_REFERENCE.with(|flag| flag.get());
        #[cfg(not(test))]
        let mesh_cache_enabled = true;
        let mesh_cache =
            if mesh_cache_enabled && self.desc.collision_mode == FluidCollisionMode::SweptMeshes {
                Some(MeshSweepCache::build(bodies))
            } else {
                None
            };

        #[cfg(test)]
        let moving_cache_enabled = !MOVING_MESH_REFERENCE.with(|flag| flag.get());
        #[cfg(not(test))]
        let moving_cache_enabled = true;
        let moving_mesh_cache = if moving_cache_enabled
            && self.desc.boundary_motion_mode == FluidBoundaryMotionMode::TranslatingKinematicMeshes
        {
            Some(MovingMeshCache::build(bodies))
        } else {
            None
        };

        #[cfg(test)]
        trace_overlap_phase("prediction_pre_sweep", &self.predicted, &self.neighbors);
        // Only the explicit mode captures the actual temporal segment, before
        // neighbor construction and later density corrections change its endpoint.
        let swept_contacts = if self.desc.collision_mode != FluidCollisionMode::Legacy {
            Some(
                self.predicted
                    .iter_mut()
                    .zip(&self.positions)
                    .map(|(p, start)| {
                        let (end, contact) =
                            if self.desc.boundary_motion_mode != FluidBoundaryMotionMode::Legacy {
                                sweep_translating_spheres(
                                    *start,
                                    *p,
                                    bodies,
                                    dt,
                                    &self.desc,
                                    moving_mesh_cache.as_ref(),
                                    SweepQueryContext {
                                        mesh_cache: mesh_cache.as_ref(),
                                        meshes: true,
                                        bounds: sweep_bounds.as_deref(),
                                        index: sweep_index.as_ref(),
                                    },
                                )
                            } else {
                                sweep_static_spheres(
                                    *start,
                                    *p,
                                    bodies,
                                    self.desc.radius,
                                    self.desc.boundary_velocity_mode,
                                    matches!(
                                        self.desc.collision_mode,
                                        FluidCollisionMode::SweptPrimitives
                                            | FluidCollisionMode::SweptMeshes
                                    ),
                                    SweepQueryContext {
                                        mesh_cache: mesh_cache.as_ref(),
                                        meshes: self.desc.collision_mode
                                            == FluidCollisionMode::SweptMeshes,
                                        bounds: sweep_bounds.as_deref(),
                                        index: sweep_index.as_ref(),
                                    },
                                )
                            };
                        *p = end;
                        contact
                    })
                    .collect::<Vec<_>>(),
            )
        } else {
            None
        };

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
        if let Some(contacts) = swept_contacts {
            self.contacts.copy_from_slice(&contacts);
        }
        // 每次迭代后都做边界投影：只压不拉 + 碰撞放在迭代里面，
        // 粒子不会先陷进地面再被一次性弹出来（那是"炸开"的主要来源）。
        let iters = self.desc.iterations.max(1);
        for _ in 0..iters {
            #[cfg(test)]
            trace_overlap_phase("density_pre", &self.predicted, &self.neighbors);
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
                let (p, contact) = if matches!(
                    self.desc.collision_mode,
                    FluidCollisionMode::SweptPrimitives | FluidCollisionMode::SweptMeshes
                ) {
                    let (end, swept) = sweep_liquid_correction(
                        prev,
                        prev + *d,
                        &*bodies,
                        radius,
                        &self.desc,
                        SweepQueryContext {
                            mesh_cache: mesh_cache.as_ref(),
                            meshes: self.desc.collision_mode == FluidCollisionMode::SweptMeshes,
                            bounds: sweep_bounds.as_deref(),
                            index: sweep_index.as_ref(),
                        },
                    );
                    let (p, projected) = collide_position(
                        prev,
                        end,
                        &*bodies,
                        radius,
                        self.desc.boundary_velocity_mode,
                        self.desc.boundary_motion_mode,
                    );
                    (p, projected.or(swept))
                } else {
                    collide_position(
                        prev,
                        prev + *d,
                        &*bodies,
                        radius,
                        self.desc.boundary_velocity_mode,
                        self.desc.boundary_motion_mode,
                    )
                };
                #[cfg(test)]
                MESH_DENSITY_TRACE.with(|trace| {
                    if let Some(trace) = trace.borrow_mut().as_mut() {
                        trace.push((i, prev, prev + *d, p));
                    }
                });
                #[cfg(test)]
                MESH_CONTINUITY_TRACE.with(|trace| {
                    if let Some(trace) = trace.borrow_mut().as_mut() {
                        trace.push((i, prev, prev + *d, p));
                    }
                });
                self.predicted[i] = p;
                if contact.is_some() {
                    self.contacts[i] = contact;
                }
            }
            #[cfg(test)]
            trace_overlap_phase("density_post", &self.predicted, &self.neighbors);
        }
        #[cfg(test)]
        trace_overlap_phase("separation_pre", &self.predicted, &self.neighbors);
        if self.desc.particle_collision {
            self.separate_particles(
                radius,
                &*bodies,
                sweep_bounds.as_deref(),
                sweep_index.as_ref(),
                mesh_cache.as_ref(),
            );
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
                FluidBoundaryMotionMode::Legacy,
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
                FluidBoundaryMotionMode::Legacy,
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

/// Sphere/triangle entry from face, edge-cylinder and vertex-sphere features.
/// Widen before arithmetic; this is finite precision, not an exact predicate.
fn sweep_triangle(
    start: Vec3,
    end: Vec3,
    tri: [Vec3; 3],
    radius: f32,
) -> Option<(f64, Vec3, Vec3)> {
    type V = [f64; 3];
    fn sub(a: V, b: V) -> V {
        [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
    }
    fn add(a: V, b: V) -> V {
        [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
    }
    fn mul(a: V, t: f64) -> V {
        a.map(|x| x * t)
    }
    fn dot(a: V, b: V) -> f64 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }
    fn cross(a: V, b: V) -> V {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }
    fn vec(a: V) -> Vec3 {
        Vec3::new(a[0] as f32, a[1] as f32, a[2] as f32)
    }
    fn entry(a: f64, b: f64, c: f64) -> Option<f64> {
        let disc = b * b - a * c;
        if a <= 0.0 || !disc.is_finite() || disc <= 0.0 {
            return None;
        }
        let q = -b - disc.sqrt().copysign(b);
        let mut roots = [q / a, if q != 0.0 { c / q } else { 0.0 }];
        roots.sort_by(f64::total_cmp);
        roots
            .into_iter()
            .find(|t| t.is_finite() && (0.0..=1.0).contains(t) && b + a * t < 0.0)
    }
    if !start.is_finite()
        || !end.is_finite()
        || !tri.iter().all(|p| p.is_finite())
        || !(radius > 0.0 && radius.is_finite())
    {
        return None;
    }
    let s = start.to_array().map(f64::from);
    let d = sub(end.to_array().map(f64::from), s);
    let pts = tri.map(|p| p.to_array().map(f64::from));
    let r = f64::from(radius);
    let raw = cross(sub(pts[1], pts[0]), sub(pts[2], pts[0]));
    let area2 = dot(raw, raw);
    let inside = |p: V| {
        (0..3).all(|i| dot(cross(sub(pts[(i + 1) % 3], pts[i]), sub(p, pts[i])), raw) >= 0.0)
    };
    let mut nearest = pts[0];
    let mut nearest2 = dot(sub(s, nearest), sub(s, nearest));
    let mut consider_nearest = |p: V| {
        let d2 = dot(sub(s, p), sub(s, p));
        if d2 < nearest2 {
            nearest2 = d2;
            nearest = p;
        }
    };
    if area2 > 0.0 {
        let projected = sub(s, mul(raw, dot(sub(s, pts[0]), raw) / area2));
        if inside(projected) {
            consider_nearest(projected);
        }
    }
    for i in 0..3 {
        let a = pts[i];
        let edge = sub(pts[(i + 1) % 3], a);
        let len2 = dot(edge, edge);
        let u = if len2 > 0.0 {
            (dot(sub(s, a), edge) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        consider_nearest(add(a, mul(edge, u)));
    }
    if nearest2 < r * r {
        let gap = sub(s, nearest);
        return if nearest2 > 0.0 && dot(gap, d) < 0.0 {
            Some((0.0, vec(nearest), vec(mul(gap, 1.0 / nearest2.sqrt()))))
        } else {
            None
        };
    }
    let mut result = None;
    let mut earliest = f64::INFINITY;
    let mut candidate = |t: f64, point: V| {
        if !t.is_finite() || !(0.0..=1.0).contains(&t) || t >= earliest {
            return;
        }
        let gap = sub(add(s, mul(d, t)), point);
        let len = dot(gap, gap).sqrt();
        if len > 0.0 && len.is_finite() && dot(gap, d) < 0.0 {
            earliest = t;
            result = Some((t, vec(point), vec(mul(gap, 1.0 / len))));
        }
    };
    if area2 > 0.0 {
        let n = mul(raw, 1.0 / area2.sqrt());
        for sign in [-1.0, 1.0] {
            let n = mul(n, sign);
            let speed = dot(d, n);
            if speed < 0.0 {
                let t = (r - dot(sub(s, pts[0]), n)) / speed;
                let point = sub(add(s, mul(d, t)), mul(n, r));
                if inside(point) {
                    candidate(t, point);
                }
            }
        }
    }
    for i in 0..3 {
        let a = pts[i];
        let edge = sub(pts[(i + 1) % 3], a);
        let len2 = dot(edge, edge);
        let offset = sub(s, a);
        if len2 > 0.0 {
            let along = dot(offset, edge) / len2;
            let speed = dot(d, edge) / len2;
            let p = sub(offset, mul(edge, along));
            let v = sub(d, mul(edge, speed));
            if let Some(t) = entry(dot(v, v), dot(p, v), dot(p, p) - r * r) {
                let u = along + speed * t;
                if (0.0..=1.0).contains(&u) {
                    candidate(t, add(a, mul(edge, u)));
                }
            }
        }
        if let Some(t) = entry(dot(d, d), dot(offset, d), dot(offset, offset) - r * r) {
            candidate(t, a);
        }
    }
    result
}

/// Earliest entry into the exact rounded Minkowski sum of a box and a particle.
/// Coordinate-plane breakpoints make squared point-to-box distance quadratic.
fn sweep_rounded_box(start: Vec3, end: Vec3, half: Vec3, radius: f32) -> Option<(f64, Vec3, Vec3)> {
    let s = start.to_array().map(f64::from);
    let e = end.to_array().map(f64::from);
    let h = half.to_array().map(f64::from);
    let d = [e[0] - s[0], e[1] - s[1], e[2] - s[2]];
    let r = f64::from(radius);
    if !s
        .iter()
        .chain(e.iter())
        .chain(h.iter())
        .all(|v| v.is_finite())
        || !(r > 0.0 && r.is_finite())
    {
        return None;
    }
    let initial: f64 = (0..3).map(|i| (s[i].abs() - h[i]).max(0.0).powi(2)).sum();
    if initial < r * r {
        return None;
    }
    let mut cuts = vec![0.0, 1.0];
    for i in 0..3 {
        if d[i] != 0.0 {
            for face in [-h[i], h[i]] {
                let t = (face - s[i]) / d[i];
                if t > 0.0 && t < 1.0 {
                    cuts.push(t);
                }
            }
        }
    }
    cuts.sort_by(f64::total_cmp);
    cuts.dedup();
    for range in cuts.windows(2) {
        let left = range[0];
        let width = range[1] - left;
        let mid = left + width * 0.5;
        let (mut a, mut b, mut c) = (0.0, 0.0, -r * r);
        for i in 0..3 {
            let value = s[i] + d[i] * mid;
            let bound = if value < -h[i] {
                -h[i]
            } else if value > h[i] {
                h[i]
            } else {
                continue;
            };
            let offset = s[i] + d[i] * left - bound;
            a += d[i] * d[i];
            b += offset * d[i];
            c += offset * offset;
        }
        if a <= 0.0 {
            continue;
        }
        let discriminant = b * b - a * c;
        if !discriminant.is_finite() || discriminant <= 0.0 {
            continue;
        }
        let q = -b - discriminant.sqrt().copysign(b);
        let mut roots = [q / a, if q != 0.0 { c / q } else { 0.0 }];
        roots.sort_by(f64::total_cmp);
        for u in roots {
            if !u.is_finite() || u < 0.0 || u > width || b + a * u >= 0.0 {
                continue;
            }
            let t = left + u;
            let p = [s[0] + d[0] * t, s[1] + d[1] * t, s[2] + d[2] * t];
            let closest = [
                p[0].clamp(-h[0], h[0]),
                p[1].clamp(-h[1], h[1]),
                p[2].clamp(-h[2], h[2]),
            ];
            let gap = [p[0] - closest[0], p[1] - closest[1], p[2] - closest[2]];
            let length = (gap[0] * gap[0] + gap[1] * gap[1] + gap[2] * gap[2]).sqrt();
            if !(length > 0.0 && length.is_finite()) {
                continue;
            }
            return Some((
                t,
                Vec3::new(closest[0] as f32, closest[1] as f32, closest[2] as f32),
                Vec3::new(
                    (gap[0] / length) as f32,
                    (gap[1] / length) as f32,
                    (gap[2] / length) as f32,
                ),
            ));
        }
    }
    None
}

/// First moving-sphere impact in the initial temporal segment only.
/// Carry with remaining boundary translation, clipping approach and carry against
/// static geometry. Does not recurse through subsequent moving impacts.
fn sweep_translating_spheres(
    start: Vec3,
    end: Vec3,
    bodies: &[BodySlot],
    dt: f32,
    desc: &FluidDesc,
    moving_cache: Option<&MovingMeshCache>,
    context: SweepQueryContext<'_>,
) -> (Vec3, Option<Contact>) {
    let static_sweep = |a, b| {
        sweep_static_spheres(
            a,
            b,
            bodies,
            desc.radius,
            desc.boundary_velocity_mode,
            true,
            context,
        )
    };
    let mut earliest = f64::INFINITY;
    let mut moving = None;
    for (index, slot) in bodies.iter().enumerate() {
        #[cfg(test)]
        MOVING_SCAN_WORK.with(|n| {
            let mut w = n.get();
            w[0] += 1;
            n.set(w);
        });
        let Some(body) = &slot.body else { continue };
        if body.sensor
            || body.body_type != crate::body::BodyType::Kinematic
            || body.angular_velocity != Vec3::ZERO
            || body.prev_transform.rotation != body.transform.rotation
        {
            continue;
        }
        for (part, (shape, local)) in body.parts().enumerate() {
            if part != 0
                && body.compound_transform_mode() != crate::body::CompoundTransformMode::BodyLocal
            {
                continue;
            }
            if desc.boundary_motion_mode == FluidBoundaryMotionMode::TranslatingKinematicMeshes {
                if let crate::shape::Shape::TriMesh { vertices, indices } = shape {
                    let mut visit = |points, delta| {
                        #[cfg(test)]
                        MOVING_MESH_WORK.with(|n| {
                            let mut w = n.get();
                            w[1] += 1;
                            n.set(w);
                        });
                        if let Some((t, point, normal)) =
                            sweep_triangle(start, end - delta, points, desc.radius)
                        {
                            if t < earliest {
                                let s = start.to_array().map(f64::from);
                                let e = end.to_array().map(f64::from);
                                let hit = Vec3::from_array(std::array::from_fn(|i| {
                                    (s[i] + (e[i] - s[i]) * t) as f32
                                }));
                                earliest = t;
                                moving = Some((
                                    hit,
                                    hit + delta * (1.0 - t) as f32,
                                    Contact {
                                        point: point + delta,
                                        normal,
                                        body: index as u32,
                                        velocity: delta / dt,
                                    },
                                ));
                            }
                        }
                    };
                    if let Some(cache) = moving_cache {
                        if let Some(part) = &cache.bodies[index][part] {
                            let relative_end = end - part.delta;
                            for triangle in &part.triangles {
                                #[cfg(test)]
                                MOVING_MESH_BOUNDS.with(|n| n.set(n.get() + 1));
                                let bounds = triangle.bounds;
                                let scale = bounds
                                    .min
                                    .abs()
                                    .max(bounds.max.abs())
                                    .max(start.abs())
                                    .max(relative_end.abs())
                                    .max_element()
                                    .max(1.0);
                                let margin = desc.radius + 64.0 * f32::EPSILON * scale;
                                let segment = Aabb {
                                    min: start.min(relative_end) - Vec3::splat(margin),
                                    max: start.max(relative_end) + Vec3::splat(margin),
                                };
                                if start.is_finite()
                                    && relative_end.is_finite()
                                    && bounds.is_valid()
                                    && segment.is_valid()
                                    && margin.is_finite()
                                    && margin >= 0.0
                                    && !bounds.intersects(&segment)
                                {
                                    continue;
                                }
                                visit(triangle.points, part.delta);
                            }
                        }
                    } else {
                        let (previous, current) = if part == 0 {
                            (body.prev_transform, body.transform)
                        } else {
                            (
                                body.prev_transform.mul_transform(&local),
                                body.transform.mul_transform(&local),
                            )
                        };
                        let delta = current.position - previous.position;
                        if !delta.is_finite() || delta == Vec3::ZERO {
                            continue;
                        }
                        for triangle in indices.as_chunks::<3>().0 {
                            let (Some(a), Some(b), Some(c)) = (
                                vertices.get(triangle[0] as usize),
                                vertices.get(triangle[1] as usize),
                                vertices.get(triangle[2] as usize),
                            ) else {
                                continue;
                            };
                            let points = [*a, *b, *c].map(|p| previous.transform_point(p));
                            #[cfg(test)]
                            MOVING_MESH_WORK.with(|n| {
                                let mut w = n.get();
                                w[0] += 1;
                                n.set(w);
                            });
                            visit(points, delta);
                        }
                    }
                    continue;
                }
            }
            if matches!(
                desc.boundary_motion_mode,
                FluidBoundaryMotionMode::TranslatingKinematicPrimitives
                    | FluidBoundaryMotionMode::TranslatingKinematicMeshes
            ) {
                if let crate::shape::Shape::Box { half_extents } = shape {
                    let half = half_extents.abs();
                    let (previous, current) = if part == 0 {
                        (body.prev_transform, body.transform)
                    } else {
                        (
                            body.prev_transform.mul_transform(&local),
                            body.transform.mul_transform(&local),
                        )
                    };
                    let delta = current.position - previous.position;
                    if !half.is_finite() || !delta.is_finite() || delta == Vec3::ZERO {
                        continue;
                    }
                    let a = previous.inverse_transform_point(start);
                    let b = previous.inverse_transform_point(end - delta);
                    if !a.is_finite() || !b.is_finite() {
                        continue;
                    }
                    let closest = a.clamp(-half, half);
                    let gap = a - closest;
                    let gap64 = gap.to_array().map(f64::from);
                    let motion = b.to_array().map(f64::from);
                    let origin = a.to_array().map(f64::from);
                    let distance2 = gap64.iter().map(|x| x * x).sum::<f64>();
                    let inward = (0..3)
                        .map(|i| gap64[i] * (motion[i] - origin[i]))
                        .sum::<f64>()
                        < 0.0;
                    let candidate = if distance2 > 0.0 && distance2 < f64::from(desc.radius).powi(2)
                    {
                        inward.then(|| (0.0, closest, gap.normalize_or_zero()))
                    } else {
                        sweep_rounded_box(a, b, half, desc.radius)
                    };
                    if let Some((t, point, normal)) = candidate {
                        if t < earliest {
                            let s = start.to_array().map(f64::from);
                            let e = end.to_array().map(f64::from);
                            let hit = Vec3::from_array(std::array::from_fn(|i| {
                                (s[i] + (e[i] - s[i]) * t) as f32
                            }));
                            earliest = t;
                            moving = Some((
                                hit,
                                hit + delta * (1.0 - t) as f32,
                                Contact {
                                    point: current.transform_point(point),
                                    normal: previous.rotation.mul_vec3(normal).normalize_or_zero(),
                                    body: index as u32,
                                    velocity: delta / dt,
                                },
                            ));
                        }
                    }
                    continue;
                }
            }
            let crate::shape::Shape::Sphere { radius } = shape else {
                continue;
            };
            let radius = radius.abs();
            if !radius.is_finite() {
                continue;
            }
            #[cfg(test)]
            MOVING_SCAN_WORK.with(|n| {
                let mut w = n.get();
                w[1] += 1;
                n.set(w);
            });
            let (c0, c1) = if part == 0 {
                (body.prev_transform.position, body.transform.position)
            } else {
                (
                    body.prev_transform.mul_transform(&local).position,
                    body.transform.mul_transform(&local).position,
                )
            };
            if c0 == c1 || !c0.is_finite() || !c1.is_finite() {
                continue;
            }
            #[cfg(test)]
            MOVING_SCAN_WORK.with(|n| {
                let mut w = n.get();
                w[2] += 1;
                n.set(w);
            });
            let s = start.to_array().map(f64::from);
            let e = end.to_array().map(f64::from);
            let a = c0.to_array().map(f64::from);
            let b = c1.to_array().map(f64::from);
            let p: [f64; 3] = std::array::from_fn(|i| s[i] - a[i]);
            let d: [f64; 3] = std::array::from_fn(|i| e[i] - s[i] - (b[i] - a[i]));
            let dot = |u: [f64; 3], v: [f64; 3]| u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
            let r = f64::from(radius) + f64::from(desc.radius);
            let aa = dot(d, d);
            let bb = dot(p, d);
            let cc = dot(p, p) - r * r;
            if !aa.is_finite() || aa <= 0.0 || bb >= 0.0 {
                continue;
            }
            let t = if cc < 0.0 {
                // Only the strict exterior clearance shell gets an immediate
                // contact. Solid-interior and surface starts retain projection.
                if dot(p, p) <= f64::from(radius) * f64::from(radius) {
                    continue;
                }
                0.0
            } else {
                let disc = bb * bb - aa * cc;
                if !disc.is_finite() || disc <= 0.0 {
                    continue;
                }
                let q = -bb + disc.sqrt();
                cc / q
            };
            if !t.is_finite() || !(0.0..=1.0).contains(&t) || t >= earliest {
                continue;
            }
            let hit = Vec3::from_array(std::array::from_fn(|i| (s[i] + (e[i] - s[i]) * t) as f32));
            let center =
                Vec3::from_array(std::array::from_fn(|i| (a[i] + (b[i] - a[i]) * t) as f32));
            let normal = (hit - center).normalize_or_zero();
            if normal == Vec3::ZERO {
                continue;
            }
            #[cfg(test)]
            if cc < 0.0 {
                MOVING_SHELL_CARRY
                    .with(|value| value.set(Some((hit + (c1 - center), start + (c1 - c0)))));
            }
            earliest = t;
            moving = Some((
                hit,
                hit + (c1 - center),
                Contact {
                    point: c1 + normal * radius,
                    normal,
                    body: index as u32,
                    velocity: (c1 - c0) / dt,
                },
            ));
        }
    }
    let Some((hit, carry, contact)) = moving else {
        return static_sweep(start, end);
    };
    #[cfg(test)]
    MOVING_SCAN_WORK.with(|n| {
        let mut w = n.get();
        w[3] += 1;
        n.set(w);
    });
    let first = static_sweep(start, hit);
    if first.1.is_some() {
        return first;
    }
    let second = static_sweep(hit, carry);
    if second.1.is_some() {
        second
    } else {
        (carry, Some(contact))
    }
}

/// Opt-in pair correction only: initial sweep plus at most one sliding sweep.
/// Discard inward normal motion and re-sweep every tangential residual, so this
/// cannot teleport through another wall. At the second contact, stop conservatively.
fn sweep_separation_slide(
    start: Vec3,
    correction: Vec3,
    bodies: &[BodySlot],
    radius: f32,
    desc: &FluidDesc,
    context: SweepQueryContext<'_>,
) -> (Vec3, bool) {
    let end = start + correction;
    #[cfg(test)]
    SEPARATION_REDUNDANCY.with(|work| {
        if let Some(mut n) = work.get() {
            n[0] += 1;
            n[1] += u64::from(end == start);
            work.set(Some(n));
        }
    });
    #[cfg(test)]
    separation_cost_add(1, 1);
    let (hit, contact) = sweep_liquid_correction(start, end, bodies, radius, desc, context);
    #[cfg(test)]
    SEPARATION_SEGMENT_TRACE.with(|trace| {
        if let Some(trace) = trace.borrow_mut().as_mut() {
            assert!(trace.len() < 4096);
            trace.push((start, end, hit));
        }
    });
    let Some(contact) = contact else {
        return (hit, false);
    };
    let remaining = end - hit;
    let inward = remaining.dot(contact.normal).min(0.0);
    let tangent = remaining - contact.normal * inward;
    #[cfg(test)]
    SEPARATION_REDUNDANCY.with(|work| {
        if let Some(mut n) = work.get() {
            n[2] += u64::from(tangent.length_squared() == 0.0);
            n[3] += u64::from(hit + tangent == hit);
            work.set(Some(n));
        }
    });
    if !tangent.is_finite() || tangent.length_squared() == 0.0 {
        return (hit, true);
    }
    #[cfg(test)]
    separation_cost_add(2, 1);
    let result = sweep_liquid_correction(hit, hit + tangent, bodies, radius, desc, context).0;
    #[cfg(test)]
    SEPARATION_SEGMENT_TRACE.with(|trace| {
        if let Some(trace) = trace.borrow_mut().as_mut() {
            assert!(trace.len() < 4096);
            trace.push((hit, hit + tangent, result));
        }
    });
    (result, true)
}

/// Sweep a liquid movement segment against static spheres, and optionally boxes.
/// The primitive mode also holds inward clearance-shell motion at the start;
/// centers already inside solids retain discrete projection.
fn sweep_liquid_correction(
    start: Vec3,
    end: Vec3,
    bodies: &[BodySlot],
    radius: f32,
    desc: &FluidDesc,
    context: SweepQueryContext<'_>,
) -> (Vec3, Option<Contact>) {
    if desc.boundary_motion_mode != FluidBoundaryMotionMode::TranslatingKinematicMeshes {
        return sweep_static_spheres(
            start,
            end,
            bodies,
            radius,
            desc.boundary_velocity_mode,
            true,
            context,
        );
    }
    let (mut earliest, mut result) = sweep_static_spheres_timed(
        start,
        end,
        bodies,
        radius,
        desc.boundary_velocity_mode,
        true,
        context,
    );
    // Current geometry only: density and separation must not replay boundary time.
    // Equal times retain original body order; within a body, part/triangle order.
    for (index, slot) in bodies.iter().enumerate() {
        let Some(body) = &slot.body else { continue };
        if body.sensor
            || body.body_type != crate::body::BodyType::Kinematic
            || body.angular_velocity != Vec3::ZERO
            || body.prev_transform.rotation != body.transform.rotation
        {
            continue;
        }
        for (part, (shape, local)) in body.parts().enumerate() {
            if part != 0
                && body.compound_transform_mode() != crate::body::CompoundTransformMode::BodyLocal
            {
                continue;
            }
            let crate::shape::Shape::TriMesh { vertices, indices } = shape else {
                continue;
            };
            let world = body.part_world_transform(&local);
            for tri in indices.as_chunks::<3>().0 {
                let (Some(a), Some(b), Some(c)) = (
                    vertices.get(tri[0] as usize),
                    vertices.get(tri[1] as usize),
                    vertices.get(tri[2] as usize),
                ) else {
                    continue;
                };
                let points = [*a, *b, *c].map(|p| world.transform_point(p));
                if let Some((t, point, normal)) = sweep_triangle(start, end, points, radius) {
                    if t < earliest
                        || (t == earliest
                            && result.1.as_ref().is_some_and(|c| index < c.body as usize))
                    {
                        earliest = t;
                        result = (
                            if t == 0.0 {
                                start
                            } else {
                                point + normal * radius
                            },
                            Some(Contact {
                                point,
                                normal,
                                body: index as u32,
                                velocity: body.velocity_at_point(point),
                            }),
                        );
                    }
                }
            }
        }
    }
    result
}

fn sweep_static_spheres(
    start: Vec3,
    end: Vec3,
    bodies: &[BodySlot],
    radius: f32,
    mode: FluidBoundaryVelocityMode,
    boxes: bool,
    context: SweepQueryContext<'_>,
) -> (Vec3, Option<Contact>) {
    sweep_static_spheres_timed(start, end, bodies, radius, mode, boxes, context).1
}

fn sweep_static_spheres_timed(
    start: Vec3,
    end: Vec3,
    bodies: &[BodySlot],
    radius: f32,
    mode: FluidBoundaryVelocityMode,
    boxes: bool,
    context: SweepQueryContext<'_>,
) -> (f64, (Vec3, Option<Contact>)) {
    let SweepQueryContext {
        mesh_cache,
        meshes,
        bounds: cached_bounds,
        index,
    } = context;
    let wide = |v: Vec3| [f64::from(v.x), f64::from(v.y), f64::from(v.z)];
    let sub = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let delta = sub(wide(end), wide(start));
    let aa = dot(delta, delta);
    if !aa.is_finite() || aa <= 0.0 {
        return (f64::INFINITY, (end, None));
    }
    let mut earliest = f64::INFINITY;
    let mut result = (end, None);
    let candidates = index.and_then(|index| index.candidates(start, end, radius));
    let count = candidates.as_ref().map_or(bodies.len(), Vec::len);
    #[cfg(test)]
    SEPARATION_REDUNDANCY.with(|work| {
        if let Some(mut n) = work.get() {
            // All static sweep sites, including prediction/density/final contacts:
            // broad-phase slots enumerated, not proven redundant candidates.
            n[4] += 1;
            n[5] += count as u64;
            work.set(Some(n));
        }
    });
    for ordinal in 0..count {
        let index = candidates.as_ref().map_or(ordinal, |slots| slots[ordinal]);
        let slot = &bodies[index];
        let Some(body) = &slot.body else {
            continue;
        };
        if body.sensor || body.body_type != crate::body::BodyType::Static {
            continue;
        }
        #[cfg(test)]
        let prune = !SWEEP_REFERENCE.with(|flag| flag.get());
        #[cfg(not(test))]
        let prune = true;
        // Legacy compound placement differs from its world AABB: retain full scans.
        if boxes
            && prune
            && (body.part_count() == 1
                || body.compound_transform_mode() == crate::body::CompoundTransformMode::BodyLocal)
        {
            #[cfg(test)]
            SWEEP_BOUNDS_CHECKS.with(|count| count.set(count.get() + 1));
            let bounds = if let Some(cache) = cached_bounds {
                cache[index]
            } else {
                #[cfg(test)]
                SWEEP_BOUNDS_BUILDS.with(|count| count.set(count.get() + 1));
                let bounds = body.world_aabb();
                bounds.is_valid().then_some(bounds)
            };
            if let Some(bounds) = bounds.filter(|_| start.is_finite() && end.is_finite()) {
                let scale = bounds
                    .min
                    .abs()
                    .max(bounds.max.abs())
                    .max(start.abs())
                    .max(end.abs())
                    .max_element()
                    .max(1.0);
                let margin = radius + 64.0 * f32::EPSILON * scale;
                let segment = Aabb {
                    min: start.min(end) - Vec3::splat(margin),
                    max: start.max(end) + Vec3::splat(margin),
                };
                if margin.is_finite()
                    && margin >= 0.0
                    && segment.is_valid()
                    && !bounds.intersects(&segment)
                {
                    continue;
                }
            }
        }
        for (part_index, (shape, local)) in body.parts().enumerate() {
            if meshes {
                if let crate::shape::Shape::TriMesh { vertices, indices } = shape {
                    let cached =
                        mesh_cache.and_then(|cache| cache.bodies[index][part_index].as_ref());
                    let world = body.part_world_transform(&local);
                    let mut uncached = indices.as_chunks::<3>().0.iter().filter_map(|tri| {
                        let points = [
                            *vertices.get(tri[0] as usize)?,
                            *vertices.get(tri[1] as usize)?,
                            *vertices.get(tri[2] as usize)?,
                        ]
                        .map(|p| world.transform_point(p));
                        #[cfg(test)]
                        MESH_SWEEP_WORK.with(|n| {
                            let mut work = n.get();
                            work[0] += 1;
                            n.set(work);
                        });
                        Some(points)
                    });
                    let mut cached_iter = cached.into_iter().flatten().filter_map(|triangle| {
                        #[cfg(test)]
                        MESH_SWEEP_BOUNDS_CHECKS.with(|n| n.set(n.get() + 1));
                        let bounds = triangle.bounds;
                        let scale = bounds
                            .min
                            .abs()
                            .max(bounds.max.abs())
                            .max(start.abs())
                            .max(end.abs())
                            .max_element()
                            .max(1.0);
                        let margin = radius + 64.0 * f32::EPSILON * scale;
                        let segment = Aabb {
                            min: start.min(end) - Vec3::splat(margin),
                            max: start.max(end) + Vec3::splat(margin),
                        };
                        if bounds.is_valid()
                            && margin.is_finite()
                            && margin >= 0.0
                            && segment.is_valid()
                            && !bounds.intersects(&segment)
                        {
                            None
                        } else {
                            Some(triangle.points)
                        }
                    });
                    let points: &mut dyn Iterator<Item = [Vec3; 3]> = if cached.is_some() {
                        &mut cached_iter
                    } else {
                        &mut uncached
                    };
                    for points in points {
                        #[cfg(test)]
                        MESH_SWEEP_WORK.with(|n| {
                            let mut work = n.get();
                            work[1] += 1;
                            n.set(work);
                        });
                        if let Some((t, point, normal)) = sweep_triangle(start, end, points, radius)
                        {
                            if t < earliest {
                                earliest = t;
                                result = (
                                    if t == 0.0 {
                                        start
                                    } else {
                                        point + normal * radius
                                    },
                                    Some(Contact {
                                        normal,
                                        point,
                                        body: index as u32,
                                        velocity: if mode == FluidBoundaryVelocityMode::BodyMotion {
                                            Vec3::ZERO
                                        } else {
                                            body.velocity_at_point(point)
                                        },
                                    }),
                                );
                            }
                        }
                    }
                    continue;
                }
            }
            #[cfg(test)]
            SWEEP_WORK.with(|counter| {
                let mut n = counter.get();
                n[0] += 1;
                counter.set(n);
            });
            if boxes {
                // A rounded contact can lie a fraction of an ulp inside the
                // particle-clearance shell. Block only inward motion while the
                // center remains outside the solid; do not add a numerical skin.
                let world = body.part_world_transform(&local);
                let local_start = world.inverse_transform_point(start);
                let local_end = world.inverse_transform_point(end);
                let local_point = match shape {
                    crate::shape::Shape::Box { half_extents } => {
                        Some(local_start.clamp(-half_extents.abs(), half_extents.abs()))
                    }
                    crate::shape::Shape::Sphere { radius: r } if local_start.length() > r.abs() => {
                        Some(local_start.normalize_or_zero() * r.abs())
                    }
                    _ => None,
                };
                if let Some(point) = local_point {
                    let gap = sub(wide(local_start), wide(point));
                    let distance_squared = dot(gap, gap);
                    if distance_squared > 0.0
                        && distance_squared < f64::from(radius).powi(2)
                        && dot(gap, sub(wide(local_end), wide(local_start))) < 0.0
                        && 0.0 < earliest
                    {
                        let normal = world
                            .rotation
                            .mul_vec3(Vec3::new(
                                (gap[0] / distance_squared.sqrt()) as f32,
                                (gap[1] / distance_squared.sqrt()) as f32,
                                (gap[2] / distance_squared.sqrt()) as f32,
                            ))
                            .normalize_or_zero();
                        let point = world.transform_point(point);
                        earliest = 0.0;
                        result = (
                            start,
                            Some(Contact {
                                normal,
                                point,
                                body: index as u32,
                                velocity: if mode == FluidBoundaryVelocityMode::BodyMotion {
                                    Vec3::ZERO
                                } else {
                                    body.velocity_at_point(point)
                                },
                            }),
                        );
                        continue;
                    }
                }
                if let crate::shape::Shape::Box { half_extents } = shape {
                    let world = body.part_world_transform(&local);
                    #[cfg(test)]
                    SWEEP_WORK.with(|counter| {
                        let mut n = counter.get();
                        n[1] += 1;
                        counter.set(n);
                    });
                    if let Some((t, point, normal)) = sweep_rounded_box(
                        world.inverse_transform_point(start),
                        world.inverse_transform_point(end),
                        half_extents.abs(),
                        radius,
                    ) {
                        if t < earliest {
                            let point = world.transform_point(point);
                            let normal = world.rotation.mul_vec3(normal).normalize_or_zero();
                            earliest = t;
                            result = (
                                point + normal * radius,
                                Some(Contact {
                                    normal,
                                    point,
                                    body: index as u32,
                                    velocity: if mode == FluidBoundaryVelocityMode::BodyMotion {
                                        Vec3::ZERO
                                    } else {
                                        body.velocity_at_point(point)
                                    },
                                }),
                            );
                        }
                    }
                    continue;
                }
            }
            let crate::shape::Shape::Sphere {
                radius: sphere_radius,
            } = shape
            else {
                continue;
            };
            #[cfg(test)]
            SWEEP_WORK.with(|counter| {
                let mut n = counter.get();
                n[2] += 1;
                counter.set(n);
            });
            let center = body.part_world_transform(&local).position;
            let offset = sub(wide(start), wide(center));
            let sum = f64::from(sphere_radius.abs()) + f64::from(radius);
            let bb = dot(offset, delta);
            let cc = dot(offset, offset) - sum * sum;
            // Initially overlapping points are handled by existing projection.
            if cc < 0.0 || bb >= 0.0 {
                continue;
            }
            let discriminant = bb * bb - aa * cc;
            // A tangent has no inward crossing and must not acquire friction.
            if !discriminant.is_finite() || discriminant <= 0.0 {
                continue;
            }
            let q = -bb + discriminant.sqrt();
            let t = if cc == 0.0 { 0.0 } else { cc / q };
            if !t.is_finite() || !(0.0..=1.0).contains(&t) || t >= earliest {
                continue;
            }
            let hit = [
                offset[0] + delta[0] * t,
                offset[1] + delta[1] * t,
                offset[2] + delta[2] * t,
            ];
            let length = dot(hit, hit).sqrt();
            if !(length > 0.0 && length.is_finite()) {
                continue;
            }
            let n = Vec3::new(
                (hit[0] / length) as f32,
                (hit[1] / length) as f32,
                (hit[2] / length) as f32,
            );
            let particle = Vec3::new(
                (f64::from(center.x) + hit[0] / length * sum) as f32,
                (f64::from(center.y) + hit[1] / length * sum) as f32,
                (f64::from(center.z) + hit[2] / length * sum) as f32,
            );
            let point = center + n * sphere_radius.abs();
            let velocity = if mode == FluidBoundaryVelocityMode::BodyMotion {
                Vec3::ZERO
            } else {
                body.velocity_at_point(point)
            };
            earliest = t;
            result = (
                particle,
                Some(Contact {
                    normal: n,
                    velocity,
                    point,
                    body: index as u32,
                }),
            );
        }
    }
    (earliest, result)
}

// Closest finite feature in world space. Strict face interiors retain the
// historical face crossing rule; edges, vertices and degenerate triangles do not.
fn closest_mesh_feature(
    p: Vec3,
    vertices: &[Vec3],
    indices: &[u32],
    world: quench3d_math::Transform,
) -> Option<(Vec3, Vec3, f64, bool)> {
    type V = [f64; 3];
    fn sub(a: V, b: V) -> V {
        std::array::from_fn(|i| a[i] - b[i])
    }
    fn add(a: V, b: V) -> V {
        std::array::from_fn(|i| a[i] + b[i])
    }
    fn mul(a: V, t: f64) -> V {
        a.map(|x| x * t)
    }
    fn dot(a: V, b: V) -> f64 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }
    fn cross(a: V, b: V) -> V {
        [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ]
    }
    if !p.is_finite() {
        return None;
    }
    let p = p.to_array().map(f64::from);
    let mut best: Option<(V, f64, bool)> = None;
    let mut consider = |q: V, face: bool| {
        let gap = sub(p, q);
        let d2 = dot(gap, gap);
        if d2.is_finite() && best.is_none_or(|(_, old, _)| d2 < old) {
            best = Some((q, d2, face));
        }
    };
    for tri in indices.as_chunks::<3>().0 {
        #[cfg(test)]
        MESH_PROJECTION_WORK.with(|n| {
            let mut w = n.get();
            w[0] += 1;
            n.set(w);
        });
        let (Some(a), Some(b), Some(c)) = (
            vertices.get(tri[0] as usize),
            vertices.get(tri[1] as usize),
            vertices.get(tri[2] as usize),
        ) else {
            continue;
        };
        let points = [*a, *b, *c].map(|v| world.transform_point(v));
        #[cfg(test)]
        MESH_PROJECTION_WORK.with(|n| {
            let mut w = n.get();
            w[1] += 1;
            n.set(w);
        });
        if !points.iter().all(|v| v.is_finite()) {
            continue;
        }
        #[cfg(test)]
        MESH_PROJECTION_WORK.with(|n| {
            let mut w = n.get();
            w[2] += 1;
            n.set(w);
        });
        let v = points.map(|v| v.to_array().map(f64::from));
        let n = cross(sub(v[1], v[0]), sub(v[2], v[0]));
        let n2 = dot(n, n);
        if n2 > 0.0 {
            let q = sub(p, mul(n, dot(sub(p, v[0]), n) / n2));
            if (0..3).all(|i| dot(cross(sub(v[(i + 1) % 3], v[i]), sub(q, v[i])), n) > 0.0) {
                consider(q, true);
            }
        }
        for i in 0..3 {
            let edge = sub(v[(i + 1) % 3], v[i]);
            let len2 = dot(edge, edge);
            let t = if len2 > 0.0 {
                (dot(sub(p, v[i]), edge) / len2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            consider(add(v[i], mul(edge, t)), false);
        }
    }
    best.map(|(q, d2, face)| {
        let distance = d2.sqrt();
        let normal = if distance > 0.0 {
            mul(sub(p, q), 1.0 / distance)
        } else {
            [0.0; 3]
        };
        (
            Vec3::from_array(q.map(|x| x as f32)),
            Vec3::from_array(normal.map(|x| x as f32)),
            distance,
            face,
        )
    })
}

/// 把粒子投影到所有刚体边界之外；返回修正后的位置与最后一个接触。
fn collide_position(
    prev: Vec3,
    mut p: Vec3,
    bodies: &[BodySlot],
    radius: f32,
    mode: FluidBoundaryVelocityMode,
    motion: FluidBoundaryMotionMode,
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
        for (part, (shape, local)) in body.parts().enumerate() {
            let world = body.part_world_transform(&local);
            if motion == FluidBoundaryMotionMode::TranslatingKinematicMeshes
                && body.body_type == crate::body::BodyType::Kinematic
                && body.angular_velocity == Vec3::ZERO
                && body.prev_transform.rotation == body.transform.rotation
                && (part == 0
                    || body.compound_transform_mode()
                        == crate::body::CompoundTransformMode::BodyLocal)
            {
                if let crate::shape::Shape::TriMesh { vertices, indices } = shape {
                    if let Some((point, normal, distance, face)) =
                        closest_mesh_feature(p, vertices, indices, world)
                    {
                        if !face && distance > 0.0 {
                            if distance < f64::from(radius) {
                                p = point + normal * radius;
                                #[cfg(test)]
                                MESH_PROJECTION_WORK.with(|n| {
                                    let mut w = n.get();
                                    w[3] += 1;
                                    n.set(w);
                                });
                                contact = Some(Contact {
                                    point,
                                    normal,
                                    velocity: body.velocity_at_point(point),
                                    body: body_index as u32,
                                });
                            }
                            // A finite edge/vertex has no infinite face half-space.
                            continue;
                        }
                    }
                }
            }
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
    fn swept_spheres_touch_tangent_earliest_and_legacy() {
        let run =
            |start: Vec3, velocity: Vec3, centers: &[Vec3], mode: Option<FluidCollisionMode>| {
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                for center in centers {
                    world.add_builder(
                        BodyBuilder::r#static(Shape::Sphere { radius: 0.125 }).position(*center),
                    );
                }
                let mut desc = FluidDesc {
                    initial_positions: Some(vec![start]),
                    initial_velocity: velocity,
                    radius: 0.03125,
                    spacing: 0.1,
                    ..FluidDesc::default()
                };
                if let Some(mode) = mode {
                    desc.collision_mode = mode;
                }
                world.add_fluid(desc).unwrap();
                world.step(1.0 / 60.0);
                (
                    world.fluids()[0].particles()[0],
                    world.fluids()[0].velocities()[0],
                )
            };
        let swept = Some(FluidCollisionMode::SweptSpheres);
        let r = 0.15625;
        for (start, velocity) in [
            (vec3(-0.25, r, 0.0), Vec3::X * 30.0),
            (vec3(-0.25, r + 0.001, 0.0), Vec3::X * 30.0),
            (-Vec3::X * r, -Vec3::X),
            (-Vec3::X * r, Vec3::Y),
            (Vec3::ZERO, Vec3::X * 30.0), // initial overlap deliberately retains old exit behavior
        ] {
            assert_eq!(
                run(start, velocity, &[Vec3::ZERO], swept),
                run(start, velocity, &[], swept),
                "depart/tangent/overlap {start:?} {velocity:?}"
            );
        }
        let inward = run(-Vec3::X * r, Vec3::X * 30.0, &[Vec3::ZERO], swept);
        assert_eq!(inward, (-Vec3::X * r, Vec3::ZERO));
        let a = run(
            -Vec3::X * 0.3,
            Vec3::X * 36.0,
            &[Vec3::ZERO, Vec3::X * 0.2],
            swept,
        );
        let b = run(
            -Vec3::X * 0.3,
            Vec3::X * 36.0,
            &[Vec3::X * 0.2, Vec3::ZERO],
            swept,
        );
        assert_eq!(a, b);
        assert!((a.0.x + r).abs() < 1e-6);
        assert_eq!(
            run(-Vec3::X * 0.3, Vec3::X * 36.0, &[Vec3::ZERO], None),
            run(
                -Vec3::X * 0.3,
                Vec3::X * 36.0,
                &[Vec3::ZERO],
                Some(FluidCollisionMode::Legacy)
            )
        );
    }

    #[test]
    fn swept_spheres_compound_and_velocity_policy() {
        use crate::body::{CompoundTransformMode, ShapePart};
        for compound in [false, true] {
            for mode in [
                FluidBoundaryVelocityMode::Legacy,
                FluidBoundaryVelocityMode::BodyMotion,
            ] {
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                let builder = if compound {
                    BodyBuilder::r#static(Shape::Box {
                        half_extents: Vec3::splat(0.01),
                    })
                    .position(-Vec3::Y * 2.0)
                    .rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2))
                    .part(ShapePart::at(
                        Shape::Sphere { radius: 0.125 },
                        Vec3::X * 2.0,
                        Quat::from_rotation_x(0.7),
                    ))
                    .compound_transform_mode(CompoundTransformMode::BodyLocal)
                } else {
                    BodyBuilder::r#static(Shape::Sphere { radius: 0.125 })
                };
                let h = world.add_builder(builder.linear_velocity(-Vec3::X * 3.0));
                world
                    .add_fluid(FluidDesc {
                        collision_mode: FluidCollisionMode::SweptSpheres,
                        boundary_velocity_mode: mode,
                        initial_positions: Some(vec![-Vec3::X * 0.3]),
                        initial_velocity: Vec3::X * 36.0,
                        radius: 0.03125,
                        spacing: 0.1,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                assert_eq!(world.body(h).unwrap().linear_velocity, -Vec3::X * 3.0);
                world.step(1.0 / 60.0);
                assert!((world.fluids()[0].particles()[0].x + 0.15625).abs() < 1e-5);
                let expected = if mode == FluidBoundaryVelocityMode::Legacy {
                    -3.0
                } else {
                    0.0
                };
                assert!((world.fluids()[0].velocities()[0].x - expected).abs() < 1e-4);
                if mode == FluidBoundaryVelocityMode::BodyMotion {
                    for _ in 0..12 {
                        world.step(1.0 / 60.0);
                    }
                    assert!(world.fluids()[0].particles()[0].x < -0.1562);
                }
            }
        }
    }

    #[test]
    fn swept_primitives_clearance_shell_is_one_sided() {
        for sphere in [false, true] {
            for (start, velocity) in [
                (vec3(-0.140625, 0.0, 0.0), Vec3::X * 6.0),
                (vec3(-0.140625, 0.0, 0.0), -Vec3::X * 6.0),
                (vec3(-0.140625, 0.0, 0.0), Vec3::Y * 6.0),
                (Vec3::ZERO, Vec3::X * 18.0),
            ] {
                let mut states = Vec::new();
                for mode in [
                    FluidCollisionMode::Legacy,
                    FluidCollisionMode::SweptSpheres,
                    FluidCollisionMode::SweptPrimitives,
                ] {
                    let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                    world.add_builder(BodyBuilder::r#static(if sphere {
                        Shape::Sphere { radius: 0.125 }
                    } else {
                        Shape::Box {
                            half_extents: Vec3::splat(0.125),
                        }
                    }));
                    world
                        .add_fluid(FluidDesc {
                            initial_positions: Some(vec![start]),
                            initial_velocity: velocity,
                            radius: 0.03125,
                            spacing: 0.1,
                            collision_mode: mode,
                            ..FluidDesc::default()
                        })
                        .unwrap();
                    world.step(1.0 / 60.0);
                    states.push((
                        world.fluids()[0].particles()[0],
                        world.fluids()[0].velocities()[0],
                    ));
                }
                if start.x < 0.0 && velocity.x > 0.0 {
                    assert!(states[2].0.x <= start.x);
                    assert!(states[2].1.x <= 0.0);
                } else {
                    assert_eq!(states[0], states[2]);
                    assert_eq!(states[1], states[2]);
                }
            }
        }
    }

    #[test]
    fn static_sweep_cache_refreshes_after_body_edits() {
        let mut results = Vec::new();
        for reference in [true, false] {
            SWEEP_CACHE_REFERENCE.with(|f| f.set(reference));
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            let mut wall = world.add_builder(
                BodyBuilder::r#static(Shape::Sphere { radius: 0.1 }).position(Vec3::Y * 10.0),
            );
            world
                .add_fluid(FluidDesc {
                    initial_positions: Some(vec![Vec3::X * -0.25]),
                    initial_velocity: Vec3::X * 30.0,
                    radius: 0.01,
                    spacing: 0.1,
                    viscosity: 0.0,
                    collision_mode: FluidCollisionMode::SweptPrimitives,
                    ..FluidDesc::default()
                })
                .unwrap();
            let mut bits = Vec::new();
            for step in 0..4 {
                if step == 1 {
                    world
                        .body_mut(wall)
                        .unwrap()
                        .set_transform(Transform::IDENTITY);
                } else if step == 2 {
                    world.remove_body(wall).unwrap();
                } else if step == 3 {
                    wall = world.add_builder(BodyBuilder::r#static(Shape::Sphere { radius: 0.1 }));
                }
                if step > 0 {
                    let f = &mut world.fluids_mut()[0];
                    f.positions[0] = Vec3::X * -0.25;
                    f.velocities[0] = Vec3::X * 30.0;
                }
                world.step(1.0 / 60.0);
                let f = &world.fluids()[0];
                if step % 2 == 0 {
                    assert!(f.positions[0].x > 0.2);
                } else {
                    assert!(f.positions[0].x < -0.1);
                }
                bits.extend(f.positions[0].to_array().map(f32::to_bits));
                bits.extend(f.velocities[0].to_array().map(f32::to_bits));
            }
            results.push(bits);
        }
        SWEEP_CACHE_REFERENCE.with(|f| f.set(false));
        assert_eq!(results[0], results[1]);
    }

    #[test]
    fn static_sweep_index_refreshes_after_body_edits() {
        let mut results = Vec::new();
        for reference in [true, false] {
            SWEEP_CACHE_REFERENCE.with(|f| f.set(false));
            SWEEP_INDEX_REFERENCE.with(|f| f.set(reference));
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            world.add_builder(
                BodyBuilder::r#static(Shape::Sphere { radius: 0.1 }).position(Vec3::splat(-20.0)),
            );
            let mut wall = world.add_builder(
                BodyBuilder::r#static(Shape::Sphere { radius: 0.1 }).position(Vec3::Y * 10.0),
            );
            world
                .add_fluid(FluidDesc {
                    initial_positions: Some(vec![Vec3::X * -0.25]),
                    initial_velocity: Vec3::X * 30.0,
                    radius: 0.01,
                    spacing: 0.1,
                    viscosity: 0.0,
                    collision_mode: FluidCollisionMode::SweptPrimitives,
                    ..FluidDesc::default()
                })
                .unwrap();
            let mut bits = Vec::new();
            for step in 0..4 {
                if step == 1 {
                    world
                        .body_mut(wall)
                        .unwrap()
                        .set_transform(Transform::IDENTITY);
                } else if step == 2 {
                    world.remove_body(wall).unwrap();
                } else if step == 3 {
                    wall = world.add_builder(BodyBuilder::r#static(Shape::Sphere { radius: 0.1 }));
                }
                if step > 0 {
                    let f = &mut world.fluids_mut()[0];
                    f.positions[0] = Vec3::X * -0.25;
                    f.velocities[0] = Vec3::X * 30.0;
                }
                SWEEP_INDEX_VISITS.with(|n| n.set(0));
                world.step(1.0 / 60.0);
                assert_eq!(
                    SWEEP_INDEX_VISITS.with(|n| n.get()) > 0,
                    !reference && step != 2
                );
                let f = &world.fluids()[0];
                if step % 2 == 0 {
                    assert!(f.positions[0].x > 0.2);
                } else {
                    assert!(f.positions[0].x < -0.1);
                }
                bits.extend(f.positions[0].to_array().map(f32::to_bits));
                bits.extend(f.velocities[0].to_array().map(f32::to_bits));
            }
            results.push(bits);
        }
        SWEEP_CACHE_REFERENCE.with(|f| f.set(false));
        SWEEP_INDEX_REFERENCE.with(|f| f.set(false));
        assert_eq!(results[0], results[1]);
    }

    #[test]
    fn static_sweep_bounds_build_once_per_step() {
        let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
        for i in 0..128 {
            world.add_builder(
                BodyBuilder::r#static(Shape::Sphere { radius: 0.1 }).position(vec3(
                    10.0 + i as f32,
                    10.0,
                    0.0,
                )),
            );
        }
        world
            .add_fluid(FluidDesc {
                initial_positions: Some(vec![Vec3::ZERO, Vec3::Y * 0.2]),
                initial_velocity: Vec3::X,
                collision_mode: FluidCollisionMode::SweptPrimitives,
                ..FluidDesc::default()
            })
            .unwrap();
        SWEEP_REFERENCE.with(|flag| flag.set(false));
        SWEEP_BOUNDS_BUILDS.with(|count| count.set(0));
        world.step(1.0 / 60.0);
        let builds = SWEEP_BOUNDS_BUILDS.with(|count| count.get());
        assert!(
            builds > 0 && builds <= 128,
            "static bounds builds={builds}, budget=128"
        );
    }

    fn mesh_sweep_perf_fixture(triangles: usize, separation: bool, wall: bool) -> PhysicsWorld {
        let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
        let mut vertices = Vec::new();
        for i in 0..triangles {
            let offset = if i == 0 {
                Vec3::ZERO
            } else {
                vec3(10.0 + (i % 32) as f32, 10.0 + (i / 32) as f32, 10.0)
            };
            vertices.extend([
                vec3(0.0, -2.0, -2.0) + offset,
                vec3(0.0, 2.0, -2.0) + offset,
                vec3(0.0, 0.0, 2.0) + offset,
            ]);
        }
        if wall {
            let indices = (0..vertices.len() as u32).collect();
            world.add_builder(BodyBuilder::r#static(Shape::TriMesh { vertices, indices }));
        }
        world
            .add_fluid(FluidDesc {
                initial_positions: Some(
                    (0..32)
                        .map(|i| {
                            vec3(
                                -0.004 - (i % 16) as f32 * 0.003,
                                (i / 16) as f32 * 0.3 - 0.15,
                                0.0,
                            )
                        })
                        .collect(),
                ),
                initial_velocity: Vec3::X * 2.0,
                spacing: 0.1,
                radius: 0.003,
                iterations: 4,
                viscosity: 0.0,
                particle_collision: separation,
                collision_mode: FluidCollisionMode::SweptMeshes,
                boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                ..FluidDesc::default()
            })
            .unwrap();
        world
    }

    #[test]
    fn mesh_sweep_cache_geometry_and_edits_match_full_scan() {
        use crate::body::{CompoundTransformMode, ShapePart};
        let mut totals = [0usize; 2];
        for case in 0..32 {
            let mode = if case % 4 == 0 {
                CompoundTransformMode::Legacy
            } else {
                CompoundTransformMode::BodyLocal
            };
            let pose = Transform {
                position: Vec3::splat(if case >= 16 { 1000.0 } else { 0.0 }),
                rotation: Quat::from_rotation_z(case as f32 * 0.13),
                scale: Vec3::ONE,
            };
            let local = Transform {
                position: Vec3::Y * 0.7,
                rotation: Quat::from_rotation_x(0.37),
                scale: Vec3::ONE,
            };
            let expected = if mode == CompoundTransformMode::BodyLocal {
                pose.mul_transform(&local)
            } else {
                local.mul_transform(&pose)
            };
            let mut vertices = match case % 4 {
                0 | 1 => vec![
                    vec3(0.0, -2.0, -2.0),
                    vec3(0.0, 2.0, -2.0),
                    vec3(0.0, 0.0, 2.0),
                ],
                2 => vec![-Vec3::Y, -Vec3::Y, Vec3::Y],
                _ => vec![Vec3::ZERO; 3],
            };
            for i in 0..8 {
                vertices.extend([
                    vec3(0.0, 10.0 + i as f32, 0.0),
                    vec3(0.0, 11.0 + i as f32, 0.0),
                    vec3(0.0, 10.0 + i as f32, 1.0),
                ]);
            }
            let mut indices: Vec<u32> = (0..vertices.len() as u32).collect();
            if case % 2 == 1 {
                indices.reverse();
            }
            if case % 8 == 4 {
                indices.extend([0, 1, 99999]);
            }
            if case % 8 == 5 {
                indices.clear();
            }
            if case % 8 == 6 {
                let n = vertices.len() as u32;
                vertices.extend([Vec3::splat(f32::NAN); 3]);
                indices.extend([n, n + 1, n + 2]);
            }
            let shape = Shape::TriMesh { vertices, indices };
            let mut snapshots = Vec::new();
            for (path, reference) in [true, false].into_iter().enumerate() {
                MESH_SWEEP_REFERENCE.with(|n| n.set(reference));
                MESH_SWEEP_WORK.with(|n| n.set([0; 2]));
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                let builder = || {
                    BodyBuilder::r#static(Shape::Sphere { radius: 0.01 })
                        .position(pose.position)
                        .rotation(pose.rotation)
                        .part(ShapePart::at(shape.clone(), local.position, local.rotation))
                        .compound_transform_mode(mode)
                };
                let mut wall = world.add_builder(builder());
                world
                    .add_fluid(FluidDesc {
                        initial_positions: Some(vec![expected.transform_point(vec3(
                            -0.25,
                            if case % 8 == 1 { 2.1 } else { 0.0 },
                            0.0,
                        ))]),
                        initial_velocity: expected.rotation * Vec3::X * 30.0,
                        radius: 0.01,
                        spacing: 0.1,
                        viscosity: 0.0,
                        collision_mode: FluidCollisionMode::SweptMeshes,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                let mut bits = Vec::new();
                for step in 0..5 {
                    if case % 8 == 0 {
                        if step == 1 {
                            let mut moved = pose;
                            moved.position += Vec3::Y * 20.0;
                            world.body_mut(wall).unwrap().set_transform(moved);
                        }
                        if step == 2 {
                            world.remove_body(wall).unwrap();
                        }
                        if step == 3 {
                            wall = world.add_builder(builder());
                        }
                        let fluid = &mut world.fluids_mut()[0];
                        fluid.positions[0] = expected.transform_point(Vec3::X * -0.25);
                        fluid.velocities[0] = expected.rotation * Vec3::X * 30.0;
                    }
                    world.step(1.0 / 60.0);
                    let f = &world.fluids()[0];
                    for v in f
                        .positions
                        .iter()
                        .chain(&f.velocities)
                        .chain(&f.predicted)
                        .chain(&f.delta)
                    {
                        bits.extend(v.to_array().map(f32::to_bits));
                    }
                    bits.extend(f.densities.iter().chain(&f.lambdas).map(|v| v.to_bits()));
                    for c in &f.contacts {
                        bits.push(u32::from(c.is_some()));
                        if let Some(c) = c {
                            bits.push(c.body);
                            for v in [c.point, c.normal, c.velocity] {
                                bits.extend(v.to_array().map(f32::to_bits));
                            }
                        }
                    }
                    bits.push(f.revision() as u32);
                }
                totals[path] += MESH_SWEEP_WORK.with(|n| n.get()[1]);
                snapshots.push(bits);
            }
            assert_eq!(snapshots[0], snapshots[1], "case={case}");
        }
        MESH_SWEEP_REFERENCE.with(|n| n.set(false));
        assert!(totals[1] < totals[0], "{totals:?}");
    }

    #[test]
    fn mesh_sweep_sparse_feature_budget() {
        let mut world = mesh_sweep_perf_fixture(2048, true, true);
        MESH_SWEEP_WORK.with(|n| n.set([0; 2]));
        world.step(1.0 / 60.0);
        let work = MESH_SWEEP_WORK.with(|n| n.get());
        assert!(work[1] < 10_000, "sparse mesh feature budget: {work:?}");
    }

    #[test]
    #[ignore = "fixed release mesh performance plan, not a wall-clock CI budget"]
    fn mesh_sweep_sparse_performance_plan() {
        for separation in [false, true] {
            let mut control = mesh_sweep_perf_fixture(1, separation, false);
            control.step(1.0 / 60.0);
            assert!(control.fluids()[0].particles().iter().any(|p| p.x > 0.0));
        }
        let mut references = [Vec::new(), Vec::new()];
        for round in 0..3 {
            let sizes = if round % 2 == 0 {
                [1, 128, 2048]
            } else {
                [2048, 128, 1]
            };
            for triangles in sizes {
                for separation in [false, true] {
                    for reference_path in if round % 2 == 0 {
                        [true, false]
                    } else {
                        [false, true]
                    } {
                        MESH_SWEEP_REFERENCE.with(|n| n.set(reference_path));
                        let mut world = mesh_sweep_perf_fixture(triangles, separation, true);
                        MESH_SWEEP_WORK.with(|n| n.set([0; 2]));
                        MESH_SWEEP_BOUNDS_CHECKS.with(|n| n.set(0));
                        SEPARATION_PUSHES.with(|n| n.set(0));
                        let timer = std::time::Instant::now();
                        world.step(1.0 / 60.0);
                        let us = timer.elapsed().as_secs_f64() * 1e6;
                        let work = MESH_SWEEP_WORK.with(|n| n.get());
                        let pushes = SEPARATION_PUSHES.with(|n| n.get());
                        let bounds_checks = MESH_SWEEP_BOUNDS_CHECKS.with(|n| n.get());
                        if triangles == 2048 {
                            assert_eq!(work[1] < 10_000, !reference_path);
                        }
                        let fluid = &world.fluids()[0];
                        let max_x = fluid
                            .particles()
                            .iter()
                            .map(|p| p.x)
                            .fold(f32::NEG_INFINITY, f32::max);
                        assert!(
                            max_x <= -0.003 + 1e-5,
                            "triangles={triangles} separation={separation} max_x={max_x}"
                        );
                        assert_eq!(pushes > 0, separation);
                        let bits: Vec<_> = fluid
                            .particles()
                            .iter()
                            .chain(fluid.velocities())
                            .flat_map(|v| v.to_array().map(f32::to_bits))
                            .collect();
                        let reference = &mut references[usize::from(separation)];
                        if reference.is_empty() {
                            *reference = bits;
                        } else {
                            assert_eq!(*reference, bits);
                        }
                        println!("round={round} triangles={triangles} particles=32 separation={separation} reference={reference_path} bounds_checks={bounds_checks} full_step_us={us:.3} triangle_transforms={} feature_queries={} pair_pushes={pushes} max_x={max_x}", work[0], work[1]);
                    }
                }
            }
        }
        MESH_SWEEP_REFERENCE.with(|n| n.set(false));
    }

    #[test]
    #[ignore = "fixed release performance plan, not a wall-clock CI budget"]
    fn static_sweep_sparse_performance_plan() {
        fn fixture(parts: usize, separation: bool, wall: bool) -> PhysicsWorld {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            if wall {
                world.add_builder(BodyBuilder::r#static(Shape::Box {
                    half_extents: vec3(0.005, 2.0, 2.0),
                }));
            }
            for i in 1..parts {
                let shape = if i % 2 == 0 {
                    Shape::Box {
                        half_extents: Vec3::splat(0.1),
                    }
                } else {
                    Shape::Sphere { radius: 0.1 }
                };
                world.add_builder(BodyBuilder::r#static(shape).position(vec3(
                    10.0 + (i % 32) as f32,
                    10.0 + (i / 32) as f32,
                    10.0,
                )));
            }
            world
                .add_fluid(FluidDesc {
                    initial_positions: Some(
                        (0..64)
                            .map(|i| {
                                vec3(
                                    -0.009 - (i % 16) as f32 * 0.003,
                                    (i / 16) as f32 * 0.3 - 0.45,
                                    0.0,
                                )
                            })
                            .collect(),
                    ),
                    initial_velocity: Vec3::X * 2.0,
                    spacing: 0.1,
                    radius: 0.003,
                    iterations: 4,
                    viscosity: 0.0,
                    particle_collision: separation,
                    collision_mode: FluidCollisionMode::SweptPrimitives,
                    boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                    ..FluidDesc::default()
                })
                .unwrap();
            world
        }
        for separation in [false, true] {
            let mut control = fixture(1, separation, false);
            control.step(1.0 / 60.0);
            assert!(control.fluids()[0].particles().iter().any(|p| p.x > 0.0));
        }
        let mut reference = [Vec::new(), Vec::new()];
        for round in 0..3 {
            let sizes = if round % 2 == 0 {
                [1, 128, 1024]
            } else {
                [1024, 128, 1]
            };
            for parts in sizes {
                for separation in [false, true] {
                    for full_scan in if round % 2 == 0 {
                        [true, false]
                    } else {
                        [false, true]
                    } {
                        SWEEP_REFERENCE.with(|flag| flag.set(full_scan));
                        let mut world = fixture(parts, separation, true);
                        SWEEP_WORK.with(|n| n.set([0; 3]));
                        SEPARATION_PUSHES.with(|n| n.set(0));
                        let timer = std::time::Instant::now();
                        world.step(1.0 / 60.0);
                        let us = timer.elapsed().as_secs_f64() * 1e6;
                        let work = SWEEP_WORK.with(|n| n.get());
                        let pushes = SEPARATION_PUSHES.with(|n| n.get());
                        if parts == 1024 {
                            assert_eq!(
                                work[1] + work[2] < 10_000,
                                !full_scan,
                                "sparse sweep work budget: {work:?}"
                            );
                        }
                        let fluid = &world.fluids()[0];
                        let max_x = fluid
                            .particles()
                            .iter()
                            .map(|p| p.x)
                            .fold(f32::NEG_INFINITY, f32::max);
                        assert!(
                            max_x <= -0.008 + 1e-5,
                            "parts={parts} separation={separation} maxx={max_x}"
                        );
                        assert_eq!(pushes > 0, separation);
                        let bits: Vec<_> = fluid
                            .particles()
                            .iter()
                            .chain(fluid.velocities())
                            .flat_map(|v| v.to_array().map(f32::to_bits))
                            .collect();
                        let slot = &mut reference[usize::from(separation)];
                        if slot.is_empty() {
                            *slot = bits;
                        } else {
                            assert_eq!(*slot, bits);
                        }
                        println!("round={round} parts={parts} particles=64 separation={separation} reference={full_scan} full_step_us={us:.3} candidates={} box_queries={} sphere_queries={} pair_pushes={pushes} max_x={max_x}", work[0], work[1], work[2]);
                    }
                }
            }
        }
        SWEEP_REFERENCE.with(|flag| flag.set(false));
    }

    #[test]
    fn static_sweep_sparse_body_check_budget() {
        let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
        world.add_builder(BodyBuilder::r#static(Shape::Box {
            half_extents: vec3(0.005, 2.0, 2.0),
        }));
        for i in 1..1024 {
            let shape = if i % 2 == 0 {
                Shape::Box {
                    half_extents: Vec3::splat(0.1),
                }
            } else {
                Shape::Sphere { radius: 0.1 }
            };
            world.add_builder(BodyBuilder::r#static(shape).position(vec3(
                10.0 + (i % 32) as f32,
                10.0 + (i / 32) as f32,
                10.0,
            )));
        }
        world
            .add_fluid(FluidDesc {
                initial_positions: Some(
                    (0..64)
                        .map(|i| {
                            vec3(
                                -0.009 - (i % 16) as f32 * 0.003,
                                (i / 16) as f32 * 0.3 - 0.45,
                                0.0,
                            )
                        })
                        .collect(),
                ),
                initial_velocity: Vec3::X * 2.0,
                spacing: 0.1,
                radius: 0.003,
                iterations: 4,
                viscosity: 0.0,
                particle_collision: true,
                collision_mode: FluidCollisionMode::SweptPrimitives,
                boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                ..FluidDesc::default()
            })
            .unwrap();
        SWEEP_BOUNDS_CHECKS.with(|n| n.set(0));
        SWEEP_INDEX_VISITS.with(|n| n.set(0));
        SEPARATION_PUSHES.with(|n| n.set(0));
        world.step(1.0 / 60.0);
        let checks = SWEEP_BOUNDS_CHECKS.with(|n| n.get()) + SWEEP_INDEX_VISITS.with(|n| n.get());
        assert!(SEPARATION_PUSHES.with(|n| n.get()) > 0);
        assert!(world.fluids()[0]
            .particles()
            .iter()
            .all(|p| p.x <= -0.008 + 1e-5));
        assert!(
            checks < 10_000,
            "sparse body bounds checks={checks}, budget=10000"
        );
    }

    #[test]
    #[ignore = "fixed release performance plan, not a wall-clock CI budget"]
    fn static_sweep_cache_performance_plan() {
        fn fixture(parts: usize, separation: bool, wall: bool) -> PhysicsWorld {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            if wall {
                world.add_builder(BodyBuilder::r#static(Shape::Box {
                    half_extents: vec3(0.005, 2.0, 2.0),
                }));
            }
            for i in 1..parts {
                let shape = if i % 2 == 0 {
                    Shape::Box {
                        half_extents: Vec3::splat(0.1),
                    }
                } else {
                    Shape::Sphere { radius: 0.1 }
                };
                world.add_builder(BodyBuilder::r#static(shape).position(vec3(
                    10.0 + (i % 32) as f32,
                    10.0 + (i / 32) as f32,
                    10.0,
                )));
            }
            world
                .add_fluid(FluidDesc {
                    initial_positions: Some(
                        (0..64)
                            .map(|i| {
                                vec3(
                                    -0.009 - (i % 16) as f32 * 0.003,
                                    (i / 16) as f32 * 0.3 - 0.45,
                                    0.0,
                                )
                            })
                            .collect(),
                    ),
                    initial_velocity: Vec3::X * 2.0,
                    spacing: 0.1,
                    radius: 0.003,
                    iterations: 4,
                    viscosity: 0.0,
                    particle_collision: separation,
                    collision_mode: FluidCollisionMode::SweptPrimitives,
                    boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                    ..FluidDesc::default()
                })
                .unwrap();
            world
        }
        for separation in [false, true] {
            let mut control = fixture(1, separation, false);
            control.step(1.0 / 60.0);
            assert!(control.fluids()[0].particles().iter().any(|p| p.x > 0.0));
        }
        let mut reference = [Vec::new(), Vec::new()];
        for round in 0..3 {
            let sizes = if round % 2 == 0 {
                [1, 128, 1024]
            } else {
                [1024, 128, 1]
            };
            for parts in sizes {
                for separation in [false, true] {
                    for full_scan in if round % 2 == 0 {
                        [true, false]
                    } else {
                        [false, true]
                    } {
                        SWEEP_REFERENCE.with(|flag| flag.set(false));
                        SWEEP_CACHE_REFERENCE.with(|flag| flag.set(full_scan));
                        let mut world = fixture(parts, separation, true);
                        SWEEP_WORK.with(|n| n.set([0; 3]));
                        SWEEP_BOUNDS_BUILDS.with(|n| n.set(0));
                        SEPARATION_PUSHES.with(|n| n.set(0));
                        let timer = std::time::Instant::now();
                        world.step(1.0 / 60.0);
                        let us = timer.elapsed().as_secs_f64() * 1e6;
                        let work = SWEEP_WORK.with(|n| n.get());
                        let pushes = SEPARATION_PUSHES.with(|n| n.get());
                        let builds = SWEEP_BOUNDS_BUILDS.with(|n| n.get());
                        assert_eq!(builds <= parts, !full_scan);
                        assert!(work[1] + work[2] < 10_000);
                        println!("cache_reference={full_scan} bounds_builds={builds}");
                        let fluid = &world.fluids()[0];
                        let max_x = fluid
                            .particles()
                            .iter()
                            .map(|p| p.x)
                            .fold(f32::NEG_INFINITY, f32::max);
                        assert!(
                            max_x <= -0.008 + 1e-5,
                            "parts={parts} separation={separation} maxx={max_x}"
                        );
                        assert_eq!(pushes > 0, separation);
                        let bits: Vec<_> = fluid
                            .particles()
                            .iter()
                            .chain(fluid.velocities())
                            .flat_map(|v| v.to_array().map(f32::to_bits))
                            .collect();
                        let slot = &mut reference[usize::from(separation)];
                        if slot.is_empty() {
                            *slot = bits;
                        } else {
                            assert_eq!(*slot, bits);
                        }
                        println!("round={round} parts={parts} particles=64 separation={separation} reference={full_scan} full_step_us={us:.3} candidates={} box_queries={} sphere_queries={} pair_pushes={pushes} max_x={max_x}", work[0], work[1], work[2]);
                    }
                }
            }
        }
        SWEEP_CACHE_REFERENCE.with(|flag| flag.set(false));
    }

    #[test]
    #[ignore = "fixed release performance plan, not a wall-clock CI budget"]
    fn static_sweep_index_performance_plan() {
        fn fixture(parts: usize, separation: bool, wall: bool, negative: bool) -> PhysicsWorld {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            if wall {
                world.add_builder(BodyBuilder::r#static(Shape::Box {
                    half_extents: vec3(0.005, 2.0, 2.0),
                }));
            }
            for i in 1..parts {
                let shape = if i % 2 == 0 {
                    Shape::Box {
                        half_extents: Vec3::splat(0.1),
                    }
                } else {
                    Shape::Sphere { radius: 0.1 }
                };
                world.add_builder(BodyBuilder::r#static(shape).position(vec3(
                    (10.0 + (i % 32) as f32) * if negative { -1.0 } else { 1.0 },
                    10.0 + (i / 32) as f32,
                    10.0,
                )));
            }
            world
                .add_fluid(FluidDesc {
                    initial_positions: Some(
                        (0..64)
                            .map(|i| {
                                vec3(
                                    -0.009 - (i % 16) as f32 * 0.003,
                                    (i / 16) as f32 * 0.3 - 0.45,
                                    0.0,
                                )
                            })
                            .collect(),
                    ),
                    initial_velocity: Vec3::X * 2.0,
                    spacing: 0.1,
                    radius: 0.003,
                    iterations: 4,
                    viscosity: 0.0,
                    particle_collision: separation,
                    collision_mode: FluidCollisionMode::SweptPrimitives,
                    boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                    ..FluidDesc::default()
                })
                .unwrap();
            world
        }
        for separation in [false, true] {
            let mut control = fixture(1, separation, false, false);
            control.step(1.0 / 60.0);
            assert!(control.fluids()[0].particles().iter().any(|p| p.x > 0.0));
        }
        let mut reference = [Vec::new(), Vec::new()];
        for negative in [false, true] {
            for round in 0..3 {
                let sizes = if round % 2 == 0 {
                    [1, 128, 1024]
                } else {
                    [1024, 128, 1]
                };
                for parts in sizes {
                    for separation in [false, true] {
                        if negative && (parts != 1024 || !separation) {
                            continue;
                        }
                        for full_scan in if round % 2 == 0 {
                            [true, false]
                        } else {
                            [false, true]
                        } {
                            SWEEP_REFERENCE.with(|flag| flag.set(false));
                            SWEEP_CACHE_REFERENCE.with(|flag| flag.set(false));
                            SWEEP_INDEX_REFERENCE.with(|flag| flag.set(full_scan));
                            let mut world = fixture(parts, separation, true, negative);
                            SWEEP_WORK.with(|n| n.set([0; 3]));
                            SWEEP_BOUNDS_BUILDS.with(|n| n.set(0));
                            SWEEP_BOUNDS_CHECKS.with(|n| n.set(0));
                            SWEEP_INDEX_VISITS.with(|n| n.set(0));
                            SEPARATION_PUSHES.with(|n| n.set(0));
                            let timer = std::time::Instant::now();
                            world.step(1.0 / 60.0);
                            let us = timer.elapsed().as_secs_f64() * 1e6;
                            let work = SWEEP_WORK.with(|n| n.get());
                            let pushes = SEPARATION_PUSHES.with(|n| n.get());
                            let builds = SWEEP_BOUNDS_BUILDS.with(|n| n.get());
                            assert!(builds <= parts);
                            let checks = SWEEP_BOUNDS_CHECKS.with(|n| n.get());
                            let visits = SWEEP_INDEX_VISITS.with(|n| n.get());
                            println!("negative={negative} index_reference={full_scan} old_bounds_checks={checks} index_visits={visits}");
                            assert!(work[1] + work[2] < 10_000);
                            println!("cache_reference={full_scan} bounds_builds={builds}");
                            let fluid = &world.fluids()[0];
                            let max_x = fluid
                                .particles()
                                .iter()
                                .map(|p| p.x)
                                .fold(f32::NEG_INFINITY, f32::max);
                            assert!(
                                max_x <= -0.008 + 1e-5,
                                "parts={parts} separation={separation} maxx={max_x}"
                            );
                            assert_eq!(pushes > 0, separation);
                            let bits: Vec<_> = fluid
                                .particles()
                                .iter()
                                .chain(fluid.velocities())
                                .flat_map(|v| v.to_array().map(f32::to_bits))
                                .collect();
                            let slot = &mut reference[usize::from(separation)];
                            if slot.is_empty() {
                                *slot = bits;
                            } else {
                                assert_eq!(*slot, bits);
                            }
                            println!("round={round} parts={parts} particles=64 separation={separation} reference={full_scan} full_step_us={us:.3} candidates={} box_queries={} sphere_queries={} pair_pushes={pushes} max_x={max_x}", work[0], work[1], work[2]);
                        }
                    }
                }
            }
        }
        SWEEP_CACHE_REFERENCE.with(|flag| flag.set(false));
        SWEEP_INDEX_REFERENCE.with(|flag| flag.set(false));
    }

    #[test]
    fn sparse_sweep_pruning_preserves_boundary_trajectories() {
        use crate::body::{CompoundTransformMode, ShapePart};
        let mut total = [0usize; 2];
        for case in 0..32 {
            let frame = Quat::from_rotation_z(case as f32 * 0.17);
            let pose = Transform {
                position: if case >= 16 {
                    Vec3::splat(1000.0)
                } else {
                    Vec3::ZERO
                },
                rotation: frame,
                scale: Vec3::ONE,
            };
            let local = Transform {
                position: Vec3::Y * 0.7,
                rotation: Quat::from_rotation_x(0.37),
                scale: Vec3::ONE,
            };
            let mode = if case % 4 == 0 {
                CompoundTransformMode::Legacy
            } else {
                CompoundTransformMode::BodyLocal
            };
            let compound = case % 3 != 0;
            let expected = if !compound {
                pose
            } else if mode == CompoundTransformMode::BodyLocal {
                pose.mul_transform(&local)
            } else {
                local.mul_transform(&pose)
            };
            let shape = if case % 2 == 0 {
                Shape::Box {
                    half_extents: Vec3::splat(0.125),
                }
            } else {
                Shape::Sphere { radius: 0.125 }
            };
            let start = match (case / 2) % 4 {
                0 => vec3(-0.3, 0.0, 0.0),
                1 => vec3(-0.140625, 0.0, 0.0),
                2 => vec3(-0.3, 0.15625, 0.0),
                _ => vec3(-0.3, 0.15, 0.15),
            };
            let mut snapshots = Vec::new();
            for (path, reference) in [true, false].into_iter().enumerate() {
                SWEEP_REFERENCE.with(|f| f.set(reference));
                SWEEP_INDEX_REFERENCE.with(|f| f.set(true));
                SWEEP_WORK.with(|n| n.set([0; 3]));
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                let builder = if compound {
                    BodyBuilder::r#static(Shape::Sphere { radius: 0.01 }).part(ShapePart::at(
                        shape.clone(),
                        local.position,
                        local.rotation,
                    ))
                } else {
                    BodyBuilder::r#static(shape.clone())
                };
                world.add_builder(
                    builder
                        .position(pose.position)
                        .rotation(pose.rotation)
                        .compound_transform_mode(mode),
                );
                for j in 0..16 {
                    world.add_builder(
                        BodyBuilder::r#static(Shape::Sphere { radius: 0.1 })
                            .position(pose.position + vec3(20.0 + j as f32, 10.0, 5.0)),
                    );
                }
                // Invalid plane bounds must keep the full scan, including its far extra part.
                world.add_builder(
                    BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
                        .position(Vec3::Y * -100.0)
                        .part_at(Shape::Sphere { radius: 0.1 }, Vec3::X * 50.0),
                );
                world
                    .add_fluid(FluidDesc {
                        initial_positions: Some(vec![expected.transform_point(start)]),
                        initial_velocity: expected.rotation
                            * (Vec3::X * if case % 8 == 3 { -12.0 } else { 12.0 }),
                        radius: 0.03125,
                        spacing: 0.1,
                        viscosity: 0.0,
                        collision_mode: FluidCollisionMode::SweptPrimitives,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                let mut bits = Vec::new();
                for _ in 0..5 {
                    world.step(1.0 / 60.0);
                    let f = &world.fluids()[0];
                    for v in f
                        .positions
                        .iter()
                        .chain(&f.velocities)
                        .chain(&f.predicted)
                        .chain(&f.delta)
                    {
                        bits.extend(v.to_array().map(f32::to_bits));
                    }
                    bits.extend(f.densities.iter().chain(&f.lambdas).map(|v| v.to_bits()));
                    for contact in &f.contacts {
                        bits.push(u32::from(contact.is_some()));
                        if let Some(c) = contact {
                            bits.push(c.body);
                            for v in [c.normal, c.velocity, c.point] {
                                bits.extend(v.to_array().map(f32::to_bits));
                            }
                        }
                    }
                    let stats = f.stats();
                    bits.extend(
                        [
                            stats.max_speed,
                            stats.avg_density_ratio,
                            stats.min_y,
                            stats.max_y,
                        ]
                        .map(f32::to_bits),
                    );
                    bits.extend([
                        stats.particles as u32,
                        stats.contacts as u32,
                        f.revision() as u32,
                    ]);
                }
                total[path] += SWEEP_WORK.with(|n| n.get()[1] + n.get()[2]);
                snapshots.push(bits);
            }
            assert_eq!(snapshots[0], snapshots[1], "case={case}");
        }
        SWEEP_REFERENCE.with(|f| f.set(false));
        SWEEP_INDEX_REFERENCE.with(|f| f.set(false));
        assert!(total[1] < total[0], "pruning was not exercised: {total:?}");
        println!("32 cases x 5 public steps: expensive queries={total:?}");
    }

    #[test]
    fn static_sweep_cache_preserves_boundary_trajectories() {
        use crate::body::{CompoundTransformMode, ShapePart};
        let mut total = [0usize; 2];
        for case in 0..32 {
            let frame = Quat::from_rotation_z(case as f32 * 0.17);
            let pose = Transform {
                position: if case >= 16 {
                    Vec3::splat(1000.0)
                } else {
                    Vec3::ZERO
                },
                rotation: frame,
                scale: Vec3::ONE,
            };
            let local = Transform {
                position: Vec3::Y * 0.7,
                rotation: Quat::from_rotation_x(0.37),
                scale: Vec3::ONE,
            };
            let mode = if case % 4 == 0 {
                CompoundTransformMode::Legacy
            } else {
                CompoundTransformMode::BodyLocal
            };
            let compound = case % 3 != 0;
            let expected = if !compound {
                pose
            } else if mode == CompoundTransformMode::BodyLocal {
                pose.mul_transform(&local)
            } else {
                local.mul_transform(&pose)
            };
            let shape = if case % 2 == 0 {
                Shape::Box {
                    half_extents: Vec3::splat(0.125),
                }
            } else {
                Shape::Sphere { radius: 0.125 }
            };
            let start = match (case / 2) % 4 {
                0 => vec3(-0.3, 0.0, 0.0),
                1 => vec3(-0.140625, 0.0, 0.0),
                2 => vec3(-0.3, 0.15625, 0.0),
                _ => vec3(-0.3, 0.15, 0.15),
            };
            let mut snapshots = Vec::new();
            for (path, reference) in [true, false].into_iter().enumerate() {
                SWEEP_REFERENCE.with(|f| f.set(false));
                SWEEP_CACHE_REFERENCE.with(|f| f.set(reference));
                SWEEP_WORK.with(|n| n.set([0; 3]));
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                let builder = if compound {
                    BodyBuilder::r#static(Shape::Sphere { radius: 0.01 }).part(ShapePart::at(
                        shape.clone(),
                        local.position,
                        local.rotation,
                    ))
                } else {
                    BodyBuilder::r#static(shape.clone())
                };
                world.add_builder(
                    builder
                        .position(pose.position)
                        .rotation(pose.rotation)
                        .compound_transform_mode(mode),
                );
                for j in 0..16 {
                    world.add_builder(
                        BodyBuilder::r#static(Shape::Sphere { radius: 0.1 })
                            .position(pose.position + vec3(20.0 + j as f32, 10.0, 5.0)),
                    );
                }
                // Invalid plane bounds must keep the full scan, including its far extra part.
                world.add_builder(
                    BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
                        .position(Vec3::Y * -100.0)
                        .part_at(Shape::Sphere { radius: 0.1 }, Vec3::X * 50.0),
                );
                world
                    .add_fluid(FluidDesc {
                        initial_positions: Some(vec![expected.transform_point(start)]),
                        initial_velocity: expected.rotation
                            * (Vec3::X * if case % 8 == 3 { -12.0 } else { 12.0 }),
                        radius: 0.03125,
                        spacing: 0.1,
                        viscosity: 0.0,
                        collision_mode: FluidCollisionMode::SweptPrimitives,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                let mut bits = Vec::new();
                for _ in 0..5 {
                    world.step(1.0 / 60.0);
                    let f = &world.fluids()[0];
                    for v in f
                        .positions
                        .iter()
                        .chain(&f.velocities)
                        .chain(&f.predicted)
                        .chain(&f.delta)
                    {
                        bits.extend(v.to_array().map(f32::to_bits));
                    }
                    bits.extend(f.densities.iter().chain(&f.lambdas).map(|v| v.to_bits()));
                    for contact in &f.contacts {
                        bits.push(u32::from(contact.is_some()));
                        if let Some(c) = contact {
                            bits.push(c.body);
                            for v in [c.normal, c.velocity, c.point] {
                                bits.extend(v.to_array().map(f32::to_bits));
                            }
                        }
                    }
                    let stats = f.stats();
                    bits.extend(
                        [
                            stats.max_speed,
                            stats.avg_density_ratio,
                            stats.min_y,
                            stats.max_y,
                        ]
                        .map(f32::to_bits),
                    );
                    bits.extend([
                        stats.particles as u32,
                        stats.contacts as u32,
                        f.revision() as u32,
                    ]);
                }
                total[path] += SWEEP_WORK.with(|n| n.get()[1] + n.get()[2]);
                snapshots.push(bits);
            }
            assert_eq!(snapshots[0], snapshots[1], "case={case}");
        }
        SWEEP_CACHE_REFERENCE.with(|f| f.set(false));
        assert_eq!(total[0], total[1], "cache must preserve pruning work");
        println!("32 cases x 5 public steps: expensive queries={total:?}");
    }

    #[test]
    fn static_sweep_index_preserves_boundary_trajectories() {
        use crate::body::{CompoundTransformMode, ShapePart};
        let mut total = [0usize; 2];
        for case in 0..32 {
            let frame = Quat::from_rotation_z(case as f32 * 0.17);
            let pose = Transform {
                position: if case >= 16 {
                    Vec3::splat(1000.0)
                } else {
                    Vec3::ZERO
                },
                rotation: frame,
                scale: Vec3::ONE,
            };
            let local = Transform {
                position: Vec3::Y * 0.7,
                rotation: Quat::from_rotation_x(0.37),
                scale: Vec3::ONE,
            };
            let mode = if case % 4 == 0 {
                CompoundTransformMode::Legacy
            } else {
                CompoundTransformMode::BodyLocal
            };
            let compound = case % 3 != 0;
            let expected = if !compound {
                pose
            } else if mode == CompoundTransformMode::BodyLocal {
                pose.mul_transform(&local)
            } else {
                local.mul_transform(&pose)
            };
            let shape = if case % 2 == 0 {
                Shape::Box {
                    half_extents: Vec3::splat(0.125),
                }
            } else {
                Shape::Sphere { radius: 0.125 }
            };
            let start = match (case / 2) % 4 {
                0 => vec3(-0.3, 0.0, 0.0),
                1 => vec3(-0.140625, 0.0, 0.0),
                2 => vec3(-0.3, 0.15625, 0.0),
                _ => vec3(-0.3, 0.15, 0.15),
            };
            let mut snapshots = Vec::new();
            for (path, reference) in [true, false].into_iter().enumerate() {
                SWEEP_REFERENCE.with(|f| f.set(false));
                SWEEP_CACHE_REFERENCE.with(|f| f.set(false));
                SWEEP_INDEX_REFERENCE.with(|f| f.set(reference));
                SWEEP_WORK.with(|n| n.set([0; 3]));
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                let builder = if compound {
                    BodyBuilder::r#static(Shape::Sphere { radius: 0.01 }).part(ShapePart::at(
                        shape.clone(),
                        local.position,
                        local.rotation,
                    ))
                } else {
                    BodyBuilder::r#static(shape.clone())
                };
                world.add_builder(
                    builder
                        .position(pose.position)
                        .rotation(pose.rotation)
                        .compound_transform_mode(mode),
                );
                for j in 0..16 {
                    world.add_builder(
                        BodyBuilder::r#static(Shape::Sphere { radius: 0.1 }).position(
                            pose.position
                                + vec3(
                                    (20.0 + j as f32) * if case % 2 == 0 { -1.0 } else { 1.0 },
                                    10.0,
                                    5.0,
                                ),
                        ),
                    );
                }
                // Invalid plane bounds must keep the full scan, including its far extra part.
                world.add_builder(
                    BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
                        .position(Vec3::Y * -100.0)
                        .part_at(Shape::Sphere { radius: 0.1 }, Vec3::X * 50.0),
                );
                world
                    .add_fluid(FluidDesc {
                        initial_positions: Some(vec![expected.transform_point(start)]),
                        initial_velocity: expected.rotation
                            * (Vec3::X * if case % 8 == 3 { -12.0 } else { 12.0 }),
                        radius: 0.03125,
                        spacing: 0.1,
                        viscosity: 0.0,
                        collision_mode: FluidCollisionMode::SweptPrimitives,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                let mut bits = Vec::new();
                for _ in 0..5 {
                    world.step(1.0 / 60.0);
                    let f = &world.fluids()[0];
                    for v in f
                        .positions
                        .iter()
                        .chain(&f.velocities)
                        .chain(&f.predicted)
                        .chain(&f.delta)
                    {
                        bits.extend(v.to_array().map(f32::to_bits));
                    }
                    bits.extend(f.densities.iter().chain(&f.lambdas).map(|v| v.to_bits()));
                    for contact in &f.contacts {
                        bits.push(u32::from(contact.is_some()));
                        if let Some(c) = contact {
                            bits.push(c.body);
                            for v in [c.normal, c.velocity, c.point] {
                                bits.extend(v.to_array().map(f32::to_bits));
                            }
                        }
                    }
                    let stats = f.stats();
                    bits.extend(
                        [
                            stats.max_speed,
                            stats.avg_density_ratio,
                            stats.min_y,
                            stats.max_y,
                        ]
                        .map(f32::to_bits),
                    );
                    bits.extend([
                        stats.particles as u32,
                        stats.contacts as u32,
                        f.revision() as u32,
                    ]);
                }
                total[path] += SWEEP_WORK.with(|n| n.get()[1] + n.get()[2]);
                snapshots.push(bits);
            }
            assert_eq!(snapshots[0], snapshots[1], "case={case}");
        }
        SWEEP_CACHE_REFERENCE.with(|f| f.set(false));
        assert_eq!(total[0], total[1], "cache must preserve pruning work");
        println!("32 cases x 5 public steps: expensive queries={total:?}");
        SWEEP_INDEX_REFERENCE.with(|f| f.set(false));
    }

    #[test]
    fn particle_separation_retains_original_pair_arithmetic() {
        let initial = vec![
            vec3(-0.01, 0.0, 0.0),
            Vec3::ZERO,
            vec3(0.002, 0.0, 0.0),
            Vec3::ZERO,
        ];
        let neighbors = vec![vec![1, 2, 3], vec![0, 2, 3], vec![0, 1, 3], vec![0, 1, 2]];
        let radius = 0.01f32;
        let mut expected = initial.clone();
        // Literal historical sequential pair arithmetic, independent of production dispatch.
        for i in 0..expected.len() {
            for &j in &neighbors[i] {
                let j = j as usize;
                if j <= i || j >= expected.len() {
                    continue;
                }
                let delta = expected[i] - expected[j];
                let dist = delta.length();
                if !dist.is_finite() || dist >= 2.0 * radius {
                    continue;
                }
                let dir = if dist > 0.0 { delta / dist } else { Vec3::X };
                let push = (2.0 * radius - dist) * 0.5;
                expected[i] += dir * push;
                expected[j] -= dir * push;
            }
        }
        for mode in [
            FluidCollisionMode::Legacy,
            FluidCollisionMode::SweptSpheres,
            FluidCollisionMode::SweptPrimitives,
        ] {
            let mut fluid = Fluid::new(FluidDesc {
                collision_mode: mode,
                ..FluidDesc::default()
            })
            .unwrap();
            fluid.predicted = initial.clone();
            fluid.neighbors = neighbors.clone();
            fluid.separate_particles(radius, &[], None, None, None);
            assert_eq!(
                fluid
                    .predicted
                    .iter()
                    .map(|p| p.to_array().map(f32::to_bits))
                    .collect::<Vec<_>>(),
                expected
                    .iter()
                    .map(|p| p.to_array().map(f32::to_bits))
                    .collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn particle_separation_sensor_and_free_paths_match_old_modes() {
        let mut states = Vec::new();
        for mode in [
            FluidCollisionMode::Legacy,
            FluidCollisionMode::SweptSpheres,
            FluidCollisionMode::SweptPrimitives,
        ] {
            for sensor in [false, true] {
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                if sensor {
                    world.add_builder(
                        BodyBuilder::r#static(Shape::Box {
                            half_extents: vec3(0.005, 2.0, 2.0),
                        })
                        .sensor(true),
                    );
                }
                world
                    .add_fluid(FluidDesc {
                        initial_positions: Some(
                            (0..12)
                                .map(|i| vec3(-0.009 - i as f32 * 0.003, 0.0, 0.0))
                                .collect(),
                        ),
                        initial_velocity: Vec3::X * 2.0,
                        radius: 0.003,
                        spacing: 0.1,
                        iterations: 1,
                        viscosity: 0.0,
                        particle_collision: true,
                        collision_mode: mode,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                SEPARATION_PUSHES.with(|count| count.set(0));
                let mut state = Vec::new();
                let mut max_x = f32::NEG_INFINITY;
                for _ in 0..8 {
                    world.step(1.0 / 60.0);
                    let f = &world.fluids()[0];
                    for p in f.particles() {
                        max_x = max_x.max(p.x);
                    }
                    for v in f.particles().iter().chain(f.velocities()) {
                        state.extend(v.to_array().map(f32::to_bits));
                    }
                }
                assert!(SEPARATION_PUSHES.with(|count| count.get()) > 0);
                assert!(max_x > 0.0);
                states.push(state);
            }
        }
        for state in &states[1..] {
            assert_eq!(&states[0], state);
        }
    }

    #[test]
    fn moving_mesh_pair_correction_bypass_and_static_order() {
        let run = |y: f32, sensor: bool, reverse: bool, wall: Option<f32>, mode| {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            let mesh = BodyBuilder::kinematic(Shape::TriMesh {
                vertices: vec![
                    vec3(0.0, -2.0, -2.0),
                    vec3(0.0, 2.0, -2.0),
                    vec3(0.0, 0.0, 2.0),
                ],
                indices: if reverse {
                    vec![2, 1, 0]
                } else {
                    vec![0, 1, 2]
                },
            })
            .kinematic_trimesh(true)
            .position(Vec3::X * -0.001)
            .linear_velocity(Vec3::X * 0.06)
            .sensor(sensor)
            .build();
            let obstacle = wall.map(|x| {
                BodyBuilder::r#static(Shape::Box {
                    half_extents: vec3(0.0005, 2.0, 2.0),
                })
                .position(Vec3::X * x)
                .build()
            });
            if reverse {
                if let Some(b) = obstacle.clone() {
                    world.add_body(b);
                }
            }
            world.add_body(mesh);
            if !reverse {
                if let Some(b) = obstacle {
                    world.add_body(b);
                }
            }
            world
                .add_fluid(FluidDesc {
                    initial_positions: Some(
                        (0..12)
                            .map(|i| vec3(0.004 + i as f32 * 0.003, y, 0.0))
                            .collect(),
                    ),
                    radius: 0.003,
                    spacing: 0.1,
                    iterations: 1,
                    particle_collision: true,
                    viscosity: 0.0,
                    collision_mode: FluidCollisionMode::SweptMeshes,
                    boundary_motion_mode: mode,
                    ..FluidDesc::default()
                })
                .unwrap();
            world.step(1.0 / 60.0);
            let f = &world.fluids()[0];
            (f.positions.clone(), f.velocities.clone())
        };
        let mode = FluidBoundaryMotionMode::TranslatingKinematicMeshes;
        // The same separation paths far outside the finite triangle must bypass.
        for reverse in [false, true] {
            let solid = run(3.0, false, reverse, None, mode);
            assert_eq!(solid, run(3.0, true, reverse, None, mode));
            assert_eq!(
                solid,
                run(3.0, false, reverse, None, FluidBoundaryMotionMode::Legacy)
            );
        }
        // Obstacles on either side of the mesh, reversed body and triangle order.
        for x in [-0.006, 0.002] {
            let a = run(0.0, false, false, Some(x), mode);
            let b = run(0.0, false, true, Some(x), mode);
            assert_eq!(a, b);
            assert!(a.0.iter().all(|p| p.is_finite() && p.x >= 0.00299));
        }
    }

    #[test]
    fn moving_mesh_coupled_contact_continuity_audit() {
        // Fixed 12 trajectories, eight public steps, cached/reference = 192 steps.
        let mut total = [0usize; 3];
        for sign in [-1.0f32, 1.0] {
            for extra in [false, true] {
                for phase in 0..3 {
                    let mut records = Vec::new();
                    for reference in [false, true] {
                        MOVING_MESH_REFERENCE.with(|f| f.set(reference));
                        let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                        let mesh = Shape::TriMesh {
                            vertices: vec![
                                vec3(0.0, -20.0, -20.0),
                                vec3(0.0, 20.0, -20.0),
                                vec3(0.0, 0.0, 20.0),
                            ],
                            indices: vec![0, 1, 2],
                        };
                        let offset = if extra { Vec3::Y * 40.0 } else { Vec3::ZERO };
                        let builder = if extra {
                            BodyBuilder::kinematic(Shape::Sphere { radius: 0.001 })
                                .part_at(mesh, -offset)
                                .compound_transform_mode(
                                    crate::body::CompoundTransformMode::BodyLocal,
                                )
                        } else {
                            BodyBuilder::kinematic(mesh)
                        };
                        let handle = world.add_builder(
                            builder
                                .kinematic_trimesh(true)
                                .position(offset - Vec3::X * (sign * 0.05))
                                .linear_velocity(Vec3::X * (sign * 5.0)),
                        );
                        world
                            .add_fluid(FluidDesc {
                                initial_positions: Some(
                                    (0..12)
                                        .map(|i| {
                                            vec3(
                                                0.0,
                                                (i % 3) as f32 * 0.0015,
                                                (i / 3) as f32 * 0.0015,
                                            )
                                        })
                                        .collect(),
                                ),
                                radius: 0.003,
                                spacing: 0.1,
                                iterations: 4,
                                particle_collision: true,
                                viscosity: 0.0,
                                collision_mode: FluidCollisionMode::SweptMeshes,
                                boundary_motion_mode:
                                    FluidBoundaryMotionMode::TranslatingKinematicMeshes,
                                boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                                ..FluidDesc::default()
                            })
                            .unwrap();
                        let mut bits = Vec::new();
                        let mut case_counts = [0usize; 3];
                        let mut traces = Vec::new();
                        let mut release = 0.0;
                        for step in 0..8 {
                            if step == 4 {
                                release = world.fluids()[0]
                                    .velocities()
                                    .iter()
                                    .map(|v| v.x * sign)
                                    .sum::<f32>()
                                    / 12.0;
                                assert!(release > 0.0);
                                world.body_mut(handle).unwrap().set_linear_velocity(
                                    Vec3::X
                                        * sign
                                        * match phase {
                                            0 => 5.0,
                                            1 => 0.0,
                                            _ => -5.0,
                                        },
                                );
                            }
                            let body = world.body(handle).unwrap();
                            let before = body.transform;
                            let velocity = body.linear_velocity;
                            let previous_centroid = if step == 0 {
                                0.0
                            } else {
                                world.fluids()[0]
                                    .particles()
                                    .iter()
                                    .map(|p| p.x * sign)
                                    .sum::<f32>()
                                    / 12.0
                            };
                            SEPARATION_PUSHES.with(|n| n.set(0));
                            MOVING_SCAN_WORK.with(|n| n.set([0; 4]));
                            MESH_DENSITY_TRACE.with(|t| *t.borrow_mut() = Some(Vec::new()));
                            MESH_CONTINUITY_TRACE.with(|t| *t.borrow_mut() = Some(Vec::new()));
                            world.step(1.0 / 60.0);
                            let density =
                                MESH_DENSITY_TRACE.with(|t| t.borrow_mut().take().unwrap());
                            let trace =
                                MESH_CONTINUITY_TRACE.with(|t| t.borrow_mut().take().unwrap());
                            let impacts = MOVING_SCAN_WORK.with(|n| n.get()[3]);
                            if step == 0 {
                                assert!(impacts > 0, "first step must have true temporal impact");
                            }
                            case_counts[0] += impacts;
                            case_counts[1] += density.iter().filter(|(_, a, b, _)| a != b).count();
                            case_counts[2] += SEPARATION_PUSHES.with(|n| n.get());
                            let body = world.body(handle).unwrap();
                            assert_eq!(body.body_type, crate::body::BodyType::Kinematic);
                            assert_eq!(body.prev_transform, before);
                            assert_eq!(body.transform.rotation, before.rotation);
                            assert_eq!(body.angular_velocity, Vec3::ZERO);
                            assert_eq!(body.linear_velocity, velocity);
                            assert!(
                                (body.position() - before.position - velocity / 60.0).length()
                                    < 2e-6
                            );
                            let face_x = body.position().x;
                            let f = &world.fluids()[0];
                            for (i, (p, v)) in f.particles().iter().zip(f.velocities()).enumerate()
                            {
                                assert!(p.is_finite() && v.is_finite());
                                assert!(v.length() <= MAX_SPEED + 1e-5);
                                let path: Vec<_> =
                                    trace.iter().filter(|(j, _, _, _)| *j == i).collect();
                                assert!(!path.is_empty());
                                for adjacent in path.windows(2) {
                                    assert_eq!(adjacent[0].3, adjacent[1].1);
                                }
                                assert_eq!(path.last().unwrap().3, *p);
                                for (_, a, _, b) in &path {
                                    // Independent strict finite triangle interior inequalities,
                                    // margin >1: z>-20, 2|y|+z<20. Convexity covers each segment.
                                    for q in [a, b] {
                                        assert!(q.z > -19.0 && 2.0 * q.y.abs() + q.z < 19.0);
                                    }
                                    let entry = (a.x - face_x) * sign;
                                    let exit = (b.x - face_x) * sign;
                                    assert!(
                                        entry >= -2e-6 && exit >= -2e-6,
                                        "actual correction crossed face: {a:?}->{b:?}"
                                    );
                                }
                                if step < 4 || phase == 0 {
                                    assert!((p.x - face_x) * sign >= 0.003 - 2e-6);
                                }
                            }
                            for c in f.contacts.iter().flatten() {
                                assert!(
                                    c.point.is_finite()
                                        && c.velocity.is_finite()
                                        && c.normal.is_finite()
                                );
                                assert!((c.normal.length() - 1.0).abs() < 1e-4);
                                assert!(c.normal.x * sign > 0.99);
                            }
                            if step >= 4 && phase != 0 {
                                let centroid =
                                    f.particles().iter().map(|p| p.x * sign).sum::<f32>() / 12.0;
                                let speed =
                                    f.velocities().iter().map(|v| v.x * sign).sum::<f32>() / 12.0;
                                assert!(
                                    release > 0.0 && speed > 0.0 && centroid > previous_centroid
                                );
                                assert!(
                                    speed - velocity.x * sign > 0.0,
                                    "departing liquid must not follow retreating boundary"
                                );
                            }
                            for v in f
                                .positions
                                .iter()
                                .chain(&f.velocities)
                                .chain(&f.predicted)
                                .chain(&f.delta)
                            {
                                bits.extend(v.to_array().map(f32::to_bits));
                            }
                            bits.extend(f.densities.iter().chain(&f.lambdas).map(|v| v.to_bits()));
                            for c in &f.contacts {
                                bits.push(u32::from(c.is_some()));
                                if let Some(c) = c {
                                    bits.push(c.body);
                                    for v in [c.point, c.normal, c.velocity] {
                                        bits.extend(v.to_array().map(f32::to_bits));
                                    }
                                }
                            }
                            bits.extend([f.revision() as u32, (f.revision() >> 32) as u32]);
                            for pose in [body.prev_transform, body.transform] {
                                bits.extend(pose.position.to_array().map(f32::to_bits));
                                bits.extend(pose.rotation.to_array().map(f32::to_bits));
                            }
                            if std::env::var_os("QUENCH3D_MESH_CONTINUITY_TRACE").is_some() {
                                println!("continuity sign={sign} extra={extra} phase={phase} reference={reference} step={step} impacts={impacts} density={} pair={} face={face_x} positions={:?} velocities={:?} actual_trace={trace:?}", density.iter().filter(|(_, a, b, _)| a != b).count(), SEPARATION_PUSHES.with(|n| n.get()), f.particles(), f.velocities());
                            }
                            traces.push(trace);
                        }
                        assert!(
                            case_counts.iter().all(|n| *n > 0),
                            "coupled nonvacuity: {case_counts:?}"
                        );
                        for (sum, n) in total.iter_mut().zip(case_counts) {
                            *sum += n;
                        }
                        records.push((bits, traces, case_counts));
                    }
                    assert_eq!(
                        records[0], records[1],
                        "cached/reference sign={sign} extra={extra} phase={phase}"
                    );
                }
            }
        }
        MOVING_MESH_REFERENCE.with(|f| f.set(false));
        println!("fixed12_192steps totals impacts_density_pair={total:?}");
    }

    #[test]
    fn moving_mesh_density_bypass_and_obstacle_order() {
        let run =
            |y: f32, sensor: bool, mode, wall: Option<f32>, reverse: bool, stationary: bool| {
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                let mesh = BodyBuilder::kinematic(Shape::TriMesh {
                    vertices: vec![
                        vec3(0.0, 0.0, -2.0),
                        vec3(0.0, 0.0, 2.0),
                        vec3(0.0, 2.0, 0.0),
                    ],
                    indices: if reverse {
                        vec![2, 1, 0]
                    } else {
                        vec![0, 1, 2]
                    },
                })
                .kinematic_trimesh(true)
                .sensor(sensor)
                .position(Vec3::X * if stationary { 0.0 } else { 0.001 })
                .linear_velocity(Vec3::X * if stationary { 0.0 } else { -0.06 });
                let add_wall = |world: &mut PhysicsWorld| {
                    if let Some(x) = wall {
                        world.add_builder(
                            BodyBuilder::r#static(Shape::Box {
                                half_extents: vec3(0.0001, 2.0, 2.0),
                            })
                            .position(Vec3::X * x),
                        );
                    }
                };
                if reverse {
                    add_wall(&mut world);
                }
                world.add_builder(mesh);
                if !reverse {
                    add_wall(&mut world);
                }
                world
                    .add_fluid(FluidDesc {
                        initial_positions: Some(
                            (0..12)
                                .map(|i| vec3(-0.004 - i as f32 * 0.003, y, 0.0))
                                .collect(),
                        ),
                        radius: 0.003,
                        spacing: 0.1,
                        iterations: 1,
                        particle_collision: false,
                        viscosity: 0.0,
                        collision_mode: FluidCollisionMode::SweptMeshes,
                        boundary_motion_mode: mode,
                        boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                SEPARATION_PUSHES.with(|n| n.set(0));
                MESH_DENSITY_TRACE.with(|n| *n.borrow_mut() = Some(Vec::new()));
                world.step(1.0 / 60.0);
                let trace = MESH_DENSITY_TRACE.with(|n| n.borrow_mut().take().unwrap());
                assert_eq!(SEPARATION_PUSHES.with(|n| n.get()), 0);
                assert!(trace.iter().any(|(_, a, b, _)| a != b));
                let fluid = &world.fluids()[0];
                (fluid.particles().to_vec(), fluid.velocities.clone(), trace)
            };
        let mode = FluidBoundaryMotionMode::TranslatingKinematicMeshes;
        // Beyond the expanded finite edge: actual density crossings remain legal.
        let bypass = run(-0.01, false, mode, None, false, false);
        assert!(bypass
            .2
            .iter()
            .any(|(_, a, b, c)| a.x < 0.0 && b.x > 0.0 && c.x > 0.0));
        assert_eq!(bypass, run(-0.01, true, mode, None, false, false));
        // Legacy retains its historical infinite-face crossing heuristic here.
        let legacy = run(
            -0.01,
            false,
            FluidBoundaryMotionMode::Legacy,
            None,
            false,
            false,
        );
        assert!(legacy.0[0].x < 0.0);
        assert_eq!(
            legacy,
            run(
                -0.01,
                false,
                FluidBoundaryMotionMode::TranslatingKinematicPrimitives,
                None,
                false,
                false
            )
        );
        // Zero boundary velocity still requires a current-geometry correction sweep.
        for wall in [None, Some(-0.0005), Some(0.006)] {
            let baseline = run(-0.002, false, mode, wall, false, true);
            let reversed = run(-0.002, false, mode, wall, true, true);
            assert_eq!(baseline, reversed);
            assert!(baseline.2[0].3.x < -0.0022);
            if let Some(x) = wall {
                if x < 0.0 {
                    assert!(baseline.2[0].3.x <= x - 0.0031 + 1e-6);
                }
            }
        }
    }

    #[test]
    fn moving_mesh_density_corrections_audit() {
        let mut failures = Vec::new();
        let mut total_nonzero = 0;
        let mut total_attempted = 0;
        for sign in [-1.0f32, 1.0] {
            for extra in [false, true] {
                for iterations in [1, 4] {
                    for edge in [false, true] {
                        for (sensor, mode) in [
                            (false, FluidBoundaryMotionMode::TranslatingKinematicMeshes),
                            (true, FluidBoundaryMotionMode::TranslatingKinematicMeshes),
                            (false, FluidBoundaryMotionMode::Legacy),
                        ] {
                            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                            let mesh = Shape::TriMesh {
                                vertices: vec![
                                    vec3(0.0, 0.0, -2.0),
                                    vec3(0.0, 0.0, 2.0),
                                    vec3(0.0, 2.0, 0.0),
                                ],
                                indices: vec![0, 1, 2],
                            };
                            let offset = if extra { Vec3::Y * 4.0 } else { Vec3::ZERO };
                            let builder = if extra {
                                BodyBuilder::kinematic(Shape::Sphere { radius: 0.001 })
                                    .part_at(mesh, -offset)
                                    .compound_transform_mode(
                                        crate::body::CompoundTransformMode::BodyLocal,
                                    )
                            } else {
                                BodyBuilder::kinematic(mesh)
                            };
                            let handle = world.add_builder(
                                builder
                                    .kinematic_trimesh(true)
                                    .sensor(sensor)
                                    .position(offset + Vec3::X * (sign * 0.001))
                                    .linear_velocity(Vec3::X * (-sign * 0.06)),
                            );
                            let before = world.body(handle).unwrap().transform;
                            let radius = 0.003;
                            let y = if edge { -0.002 } else { 0.5 };
                            let initial: Vec<_> = (0..12)
                                .map(|i| vec3(-sign * (radius + 0.001 + i as f32 * radius), y, 0.0))
                                .collect();
                            world
                                .add_fluid(FluidDesc {
                                    initial_positions: Some(initial.clone()),
                                    radius,
                                    spacing: 0.1,
                                    iterations,
                                    particle_collision: false,
                                    viscosity: 0.0,
                                    collision_mode: FluidCollisionMode::SweptMeshes,
                                    boundary_motion_mode: mode,
                                    boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                                    ..FluidDesc::default()
                                })
                                .unwrap();
                            SEPARATION_PUSHES.with(|n| n.set(0));
                            MOVING_SCAN_WORK.with(|n| n.set([0; 4]));
                            MESH_DENSITY_TRACE.with(|n| *n.borrow_mut() = Some(Vec::new()));
                            world.step(1.0 / 60.0);
                            let trace = MESH_DENSITY_TRACE.with(|n| n.borrow_mut().take().unwrap());
                            assert_eq!(SEPARATION_PUSHES.with(|n| n.get()), 0);
                            assert_eq!(MOVING_SCAN_WORK.with(|n| n.get()[3]), 0);
                            let body = world.body(handle).unwrap();
                            assert_eq!(body.body_type, crate::body::BodyType::Kinematic);
                            assert_eq!(body.prev_transform, before);
                            assert!(body.transform.position.x.abs() < 1e-8);
                            assert_eq!(body.linear_velocity, Vec3::X * (-sign * 0.06));
                            assert_eq!(body.angular_velocity, Vec3::ZERO);
                            assert_eq!(trace.len(), 12 * iterations as usize);
                            let nonzero = trace.iter().filter(|(_, a, b, _)| a != b).count();
                            assert!(nonzero > 0, "must exercise real density corrections");
                            total_nonzero += nonzero;
                            let attempted = trace
                                .iter()
                                .filter(|(_, a, b, _)| a.x * sign < 0.0 && b.x * sign > 0.0)
                                .count();
                            total_attempted += attempted;
                            let positions = world.fluids()[0].particles();
                            if edge
                                && !sensor
                                && mode == FluidBoundaryMotionMode::TranslatingKinematicMeshes
                            {
                                // Independent finite-edge cylinder intersection: the segment
                                // has y=-.002,z=0, strictly within the edge z in [-2,2].
                                // Its closest distance .002 is LESS than particle radius .003;
                                // crossing the plane here is not a legal rounded-edge bypass.
                                let (_, a, b, corrected) = trace[0];
                                assert_eq!(a.y, y);
                                assert_eq!(b.y, y);
                                assert_eq!(a.z, 0.0);
                                assert_eq!(b.z, 0.0);
                                let entry = ((radius as f64).powi(2) - (y as f64).powi(2)).sqrt();
                                assert!(-(a.x as f64) * sign as f64 > entry);
                                assert!((b.x as f64) * sign as f64 > entry);
                                let t =
                                    (-sign as f64 * entry - a.x as f64) / (b.x as f64 - a.x as f64);
                                assert!(t > 0.0 && t < 1.0);
                                println!("analytic_edge_entry sign={sign} extra={extra} iterations={iterations} particle=0 t={t} entry_x={} before={a:?} target={b:?} corrected={corrected:?}", -sign as f64 * entry);
                                if corrected.x * sign > 0.0 {
                                    failures.push((sign, extra, iterations, 0, corrected));
                                }
                            }
                            for (i, position) in positions.iter().enumerate() {
                                let path: Vec<_> =
                                    trace.iter().filter(|(j, _, _, _)| *j == i).collect();
                                assert_eq!(path[0].1, initial[i]);
                                for pair in path.windows(2) {
                                    assert_eq!(pair[0].3, pair[1].1);
                                }
                                assert_eq!(path.last().unwrap().3, *position);
                                if !sensor
                                    && mode == FluidBoundaryMotionMode::TranslatingKinematicMeshes
                                    && !edge
                                    && position.x * sign > radius + 1e-6
                                {
                                    assert!(path
                                        .iter()
                                        .any(|(_, a, b, _)| a.x * sign <= 0.0 && b.x * sign > 0.0));
                                    // All correction points remain at the strict interior y=.5,z=0.
                                    assert!(path.iter().all(|(_, a, b, c)| a.y == 0.5
                                        && b.y == 0.5
                                        && c.y == 0.5
                                        && a.z == 0.0
                                        && b.z == 0.0
                                        && c.z == 0.0));
                                    failures.push((sign, extra, iterations, i, *position));
                                }
                            }
                            println!("density sign={sign} extra={extra} iterations={iterations} edge={edge} sensor={sensor} mode={mode:?} nonzero={nonzero} attempted_plane_crossings={attempted} final0={:?} trace={trace:?}", positions[0]);
                        }
                    }
                }
            }
        }
        println!("fixed16_cases_48worlds nonzero={total_nonzero} attempted_plane_crossings={total_attempted} failures={failures:?}");
        assert!(
            failures.is_empty(),
            "density corrections crossed finite face: {failures:?}"
        );
    }

    #[test]
    fn moving_mesh_pair_corrections_respect_finite_faces() {
        let mut failures = Vec::new();
        let mut summaries = Vec::new();
        for sign in [-1.0f32, 1.0] {
            for extra in [false, true] {
                for iterations in [1, 4] {
                    for radius in [0.003f32, 0.006] {
                        for (sensor, mode) in [
                            (false, FluidBoundaryMotionMode::TranslatingKinematicMeshes),
                            (true, FluidBoundaryMotionMode::TranslatingKinematicMeshes),
                            (false, FluidBoundaryMotionMode::Legacy),
                        ] {
                            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                            let mesh = Shape::TriMesh {
                                vertices: vec![
                                    vec3(0.0, -2.0, -2.0),
                                    vec3(0.0, 2.0, -2.0),
                                    vec3(0.0, 0.0, 2.0),
                                ],
                                indices: vec![0, 1, 2],
                            };
                            let offset = if extra { Vec3::Y * 4.0 } else { Vec3::ZERO };
                            let builder = if extra {
                                BodyBuilder::kinematic(Shape::Sphere { radius: 0.001 })
                                    .part_at(mesh, -offset)
                                    .compound_transform_mode(
                                        crate::body::CompoundTransformMode::BodyLocal,
                                    )
                            } else {
                                BodyBuilder::kinematic(mesh)
                            };
                            let handle = world.add_builder(
                                builder
                                    .kinematic_trimesh(true)
                                    .sensor(sensor)
                                    .position(offset + Vec3::X * (sign * 0.001))
                                    .linear_velocity(Vec3::X * (-sign * 0.06)),
                            );
                            let before = world.body(handle).unwrap().transform;
                            let initial: Vec<_> = (0..12)
                                .map(|i| {
                                    vec3(-sign * (radius + 0.001 + i as f32 * radius), 0.0, 0.0)
                                })
                                .collect();
                            world
                                .add_fluid(FluidDesc {
                                    initial_positions: Some(initial),
                                    radius,
                                    spacing: 0.1,
                                    iterations,
                                    particle_collision: true,
                                    viscosity: 0.0,
                                    collision_mode: FluidCollisionMode::SweptMeshes,
                                    boundary_motion_mode: mode,
                                    boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                                    ..FluidDesc::default()
                                })
                                .unwrap();
                            SEPARATION_PUSHES.with(|n| n.set(0));
                            MOVING_SCAN_WORK.with(|n| n.set([0; 4]));
                            MESH_CORRECTION_TRACE.with(|n| *n.borrow_mut() = Some(Vec::new()));
                            world.step(1.0 / 60.0);
                            let trace =
                                MESH_CORRECTION_TRACE.with(|n| n.borrow_mut().take().unwrap());
                            let body = world.body(handle).unwrap();
                            assert_eq!(body.body_type, crate::body::BodyType::Kinematic);
                            assert_eq!(body.prev_transform, before);
                            assert!((body.transform.position.x).abs() < 1e-8);
                            assert_eq!(body.linear_velocity, Vec3::X * (-sign * 0.06));
                            assert_eq!(body.angular_velocity, Vec3::ZERO);
                            assert_eq!(
                                MOVING_SCAN_WORK.with(|n| n.get()[3]),
                                0,
                                "initial temporal sweep must not hit"
                            );
                            let pushes = SEPARATION_PUSHES.with(|n| n.get());
                            assert!(pushes > 0);
                            let crossed: Vec<_> = trace
                                .iter()
                                .filter(|(_, a, b)| {
                                    // Every segment is on the X axis, through the strict interior
                                    // of this finite triangle, >1m from every finite edge.
                                    assert_eq!(a.y, 0.0);
                                    assert_eq!(a.z, 0.0);
                                    assert_eq!(b.y, 0.0);
                                    assert_eq!(b.z, 0.0);
                                    a.x * sign < 0.0 && b.x * sign > 0.0
                                })
                                .collect();
                            let positions = world.fluids()[0].particles();
                            let escaped: Vec<_> = positions
                                .iter()
                                .enumerate()
                                .filter(|(_, p)| p.x * sign > radius + 1e-6)
                                .map(|(i, p)| (i, p.x))
                                .collect();
                            if !sensor
                                && mode == FluidBoundaryMotionMode::TranslatingKinematicMeshes
                                && !escaped.is_empty()
                            {
                                for (particle, _) in &escaped {
                                    let path: Vec<_> =
                                        trace.iter().filter(|(i, _, _)| i == particle).collect();
                                    assert!(path.first().is_some_and(|(_, a, _)| a.x * sign < 0.0));
                                    assert!(path.iter().any(|(_, a, b)| a.x * sign <= 0.0 && b.x * sign > 0.0),
                                        "escaped particle must cross the finite face, allowing a prior push to land exactly on it: {path:?}");
                                    for pair in path.windows(2) {
                                        assert_eq!(
                                            pair[0].2, pair[1].1,
                                            "unbroken correction history"
                                        );
                                    }
                                    assert_eq!(path.last().unwrap().2, positions[*particle]);
                                    println!(
                                        "escaped_particle={particle} correction_path={path:?}"
                                    );
                                }
                                println!("escaped_pair_segments sign={sign} extra={extra} iterations={iterations} radius={radius} segments={crossed:?}");
                                failures.push((sign, extra, iterations, radius, escaped.clone()));
                            }
                            summaries.push((
                                sign,
                                extra,
                                iterations,
                                radius,
                                sensor,
                                mode,
                                pushes,
                                crossed.len(),
                                escaped,
                            ));
                        }
                    }
                }
            }
        }
        println!("fixed16_cases_48worlds summaries={summaries:?} failures={failures:?}");
        assert!(
            failures.is_empty(),
            "pair corrections tunneled through current finite mesh: {failures:?}"
        );
    }

    #[test]
    fn swept_primitives_particle_separation_remains_on_entry_side() {
        // Fixed 16 cases. Collinear particles cannot legitimately go around a sphere;
        // the box face extends far beyond every recorded transverse coordinate.
        let mut failures = Vec::new();
        let mut summaries = Vec::new();
        for sphere in [false, true] {
            for half in [0.005f32, 0.02] {
                for iterations in [1, 4] {
                    for speed in [0.0f32, 2.0] {
                        let radius = 0.003;
                        let initial: Vec<_> = (0..12)
                            .map(|i| vec3(-half - radius - 0.001 - i as f32 * 0.003, 0.0, 0.0))
                            .collect();
                        let mut traces = Vec::new();
                        for particle_collision in [false, true] {
                            let obstacle = true;
                            SEPARATION_PUSHES.with(|count| count.set(0));
                            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                            if obstacle {
                                world.add_builder(BodyBuilder::r#static(if sphere {
                                    Shape::Sphere { radius: half }
                                } else {
                                    Shape::Box {
                                        half_extents: vec3(half, 2.0, 2.0),
                                    }
                                }));
                            }
                            let desc = FluidDesc {
                                initial_positions: Some(initial.clone()),
                                initial_velocity: Vec3::X * speed,
                                spacing: 0.1,
                                radius,
                                iterations,
                                particle_collision,
                                viscosity: 0.0,
                                collision_mode: FluidCollisionMode::SweptPrimitives,
                                boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                                ..FluidDesc::default()
                            };
                            assert!(desc.is_valid());
                            world.add_fluid(desc).unwrap();
                            let mut max_x = f32::NEG_INFINITY;
                            for step in 0..8 {
                                world.step(1.0 / 60.0);
                                let fluid = &world.fluids()[0];
                                for (i, p) in fluid.particles().iter().enumerate() {
                                    assert!(p.is_finite());
                                    assert_eq!(p.y, 0.0, "transverse escape");
                                    assert_eq!(p.z, 0.0, "transverse escape");
                                    max_x = max_x.max(p.x);
                                    if obstacle && p.x > -half - radius + 1e-5 {
                                        failures
                                            .push((sphere, half, iterations, speed, step, i, p.x));
                                    }
                                }
                            }
                            let pushes = SEPARATION_PUSHES.with(|count| count.get());
                            if particle_collision {
                                assert!(pushes > 0, "separation did not execute");
                            } else {
                                assert_eq!(pushes, 0);
                            }
                            traces.push((max_x, pushes));
                        }

                        summaries.push((sphere, half, iterations, speed, traces));
                    }
                }
            }
        }
        println!("fixed_cases=16 summaries={summaries:?} failures={failures:?}");
        assert!(
            failures.is_empty(),
            "compressed particle penetration: {failures:?}"
        );
    }

    #[test]
    fn swept_primitives_compressed_particles_remain_on_entry_side() {
        // Fixed 16 cases. Collinear particles cannot legitimately go around a sphere;
        // the box face extends far beyond every recorded transverse coordinate.
        let mut failures = Vec::new();
        let mut summaries = Vec::new();
        for sphere in [false, true] {
            for half in [0.005f32, 0.02] {
                for iterations in [1, 4] {
                    for speed in [0.0f32, 2.0] {
                        let radius = 0.003;
                        let initial: Vec<_> = (0..12)
                            .map(|i| vec3(-half - radius - 0.001 - i as f32 * 0.003, 0.0, 0.0))
                            .collect();
                        let mut traces = Vec::new();
                        for obstacle in [false, true] {
                            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                            if obstacle {
                                world.add_builder(BodyBuilder::r#static(if sphere {
                                    Shape::Sphere { radius: half }
                                } else {
                                    Shape::Box {
                                        half_extents: vec3(half, 2.0, 2.0),
                                    }
                                }));
                            }
                            let desc = FluidDesc {
                                initial_positions: Some(initial.clone()),
                                initial_velocity: Vec3::X * speed,
                                spacing: 0.1,
                                radius,
                                iterations,
                                viscosity: 0.0,
                                collision_mode: FluidCollisionMode::SweptPrimitives,
                                boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                                ..FluidDesc::default()
                            };
                            assert!(desc.is_valid());
                            world.add_fluid(desc).unwrap();
                            let mut max_x = f32::NEG_INFINITY;
                            for step in 0..8 {
                                world.step(1.0 / 60.0);
                                let fluid = &world.fluids()[0];
                                for (i, p) in fluid.particles().iter().enumerate() {
                                    assert!(p.is_finite());
                                    assert_eq!(p.y, 0.0, "transverse escape");
                                    assert_eq!(p.z, 0.0, "transverse escape");
                                    max_x = max_x.max(p.x);
                                    if obstacle && p.x > -half - radius + 1e-5 {
                                        failures
                                            .push((sphere, half, iterations, speed, step, i, p.x));
                                    }
                                }
                            }
                            traces.push(max_x);
                        }
                        assert!(traces[0] > 0.0, "control did not challenge obstacle");
                        summaries.push((sphere, half, iterations, speed, traces));
                    }
                }
            }
        }
        println!("fixed_cases=16 summaries={summaries:?} failures={failures:?}");
        assert!(
            failures.is_empty(),
            "compressed particle penetration: {failures:?}"
        );
    }

    #[test]
    fn swept_primitives_preserves_sphere_mode_bits() {
        for y in [0.0, 0.05, 0.12, 0.2] {
            let mut traces = Vec::new();
            for mode in [
                FluidCollisionMode::SweptSpheres,
                FluidCollisionMode::SweptPrimitives,
            ] {
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                world.add_builder(BodyBuilder::r#static(Shape::Sphere { radius: 0.1 }));
                world
                    .add_fluid(FluidDesc {
                        collision_mode: mode,
                        initial_positions: Some(vec![vec3(-0.25, y, 0.0)]),
                        initial_velocity: Vec3::X * 30.0,
                        radius: 0.01,
                        spacing: 0.1,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                let mut trace = Vec::new();
                for _ in 0..12 {
                    world.step(1.0 / 60.0);
                    trace.extend(
                        world.fluids()[0].particles()[0]
                            .to_array()
                            .map(f32::to_bits),
                    );
                    trace.extend(
                        world.fluids()[0].velocities()[0]
                            .to_array()
                            .map(f32::to_bits),
                    );
                }
                traces.push(trace);
            }
            assert_eq!(traces[0], traces[1], "y={y}");
        }
    }

    #[test]
    fn rounded_box_sweep_edges_corners_and_misses() {
        let h = Vec3::splat(0.125);
        let r = 0.03125;
        for direction in [-1.0f32, 1.0] {
            for (axis, count) in [
                (Vec3::X, 1.0f64),
                (vec3(1.0, 1.0, 0.0), 2.0),
                (Vec3::ONE, 3.0),
            ] {
                let start = axis * (0.3 * direction);
                let end = -start;
                let (t, _, n) = sweep_rounded_box(start, end, h, r).unwrap();
                let expected = (f64::from(0.3f32) - (0.125 + f64::from(r) / count.sqrt()))
                    / (2.0 * f64::from(0.3f32));
                assert!((t - expected).abs() < 1e-12, "{axis:?} {t} {expected}");
                assert!((n - axis.normalize() * direction).length() < 1e-6);
            }
        }
        // Inside the inflated AABB, outside the true rounded edge cylinder.
        let offset = 0.125 + r * 0.8;
        assert!(
            sweep_rounded_box(vec3(-0.3, offset, offset), vec3(0.3, offset, offset), h, r)
                .is_none()
        );
        assert!(
            sweep_rounded_box(vec3(-0.3, 0.125 + r, 0.0), vec3(0.3, 0.125 + r, 0.0), h, r)
                .is_none()
        );
        assert!(
            sweep_rounded_box(vec3(-0.125 - r, 0.0, 0.0), vec3(-0.3, 0.0, 0.0), h, r).is_none()
        );
        assert_eq!(
            sweep_rounded_box(vec3(-0.125 - r, 0.0, 0.0), Vec3::ZERO, h, r)
                .unwrap()
                .0,
            0.0
        );
        assert!(sweep_rounded_box(Vec3::ZERO, Vec3::X, h, r).is_none());
    }

    #[test]
    fn swept_primitives_public_corner_miss_and_old_modes() {
        let offset = 0.125 + 0.03125 * 0.8;
        for start in [vec3(-0.25, offset, offset), vec3(-0.25, 0.0, 0.0)] {
            let mut states = Vec::new();
            for mode in [
                FluidCollisionMode::Legacy,
                FluidCollisionMode::SweptSpheres,
                FluidCollisionMode::SweptPrimitives,
            ] {
                for sensor in [false, true] {
                    let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                    world.add_builder(
                        BodyBuilder::r#static(Shape::Box {
                            half_extents: Vec3::splat(0.125),
                        })
                        .sensor(sensor),
                    );
                    world
                        .add_fluid(FluidDesc {
                            collision_mode: mode,
                            initial_positions: Some(vec![start]),
                            initial_velocity: Vec3::X * 30.0,
                            radius: 0.03125,
                            spacing: 0.1,
                            ..FluidDesc::default()
                        })
                        .unwrap();
                    world.step(1.0 / 60.0);
                    states.push((
                        world.fluids()[0].particles()[0],
                        world.fluids()[0].velocities()[0],
                    ));
                }
            }
            assert_eq!(states[0], states[2]); // Both historical modes still ignore initial box sweeps.
            if start.y != 0.0 {
                assert!(states.iter().all(|s| *s == states[0]), "{states:?}");
            } else {
                assert!(states[4].0.x < -0.15);
                assert_eq!(states[5], states[1]);
            }
        }
    }

    #[test]
    fn swept_primitives_rotated_compound_and_mixed_earliest() {
        use crate::body::{CompoundTransformMode, ShapePart};
        let pose = Transform {
            position: vec3(0.4, 0.3, 0.2),
            rotation: Quat::from_rotation_z(0.6),
            scale: Vec3::ONE,
        };
        let local = Transform {
            position: Vec3::Y,
            rotation: Quat::from_rotation_x(0.7),
            scale: Vec3::ONE,
        };
        let target = pose.mul_transform(&local);
        let mut positions = Vec::new();
        for compound in [false, true] {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            let shape = Shape::Box {
                half_extents: Vec3::splat(0.125),
            };
            let builder = if compound {
                BodyBuilder::r#static(Shape::Sphere { radius: 0.01 })
                    .position(pose.position)
                    .rotation(pose.rotation)
                    .part(ShapePart::at(shape, local.position, local.rotation))
                    .compound_transform_mode(CompoundTransformMode::BodyLocal)
            } else {
                BodyBuilder::r#static(shape)
                    .position(target.position)
                    .rotation(target.rotation)
            };
            world.add_builder(builder);
            world
                .add_fluid(FluidDesc {
                    collision_mode: FluidCollisionMode::SweptPrimitives,
                    initial_positions: Some(vec![target.transform_point(-Vec3::X * 0.25)]),
                    initial_velocity: target.rotation * Vec3::X * 30.0,
                    radius: 0.03125,
                    spacing: 0.1,
                    ..FluidDesc::default()
                })
                .unwrap();
            world.step(1.0 / 60.0);
            let p = world.fluids()[0].particles()[0];
            assert!((target.inverse_transform_point(p).x + 0.15625).abs() < 2e-6);
            positions.push(p);
        }
        assert_eq!(positions[0], positions[1]);
        for sphere_first in [false, true] {
            let mut states = Vec::new();
            for reverse in [false, true] {
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                for i in if reverse { [1, 0] } else { [0, 1] } {
                    let shape = if (i == 0) == sphere_first {
                        Shape::Sphere { radius: 0.05 }
                    } else {
                        Shape::Box {
                            half_extents: Vec3::splat(0.05),
                        }
                    };
                    world.add_builder(
                        BodyBuilder::r#static(shape).position(Vec3::X * (i as f32 * 0.2)),
                    );
                }
                world
                    .add_fluid(FluidDesc {
                        collision_mode: FluidCollisionMode::SweptPrimitives,
                        initial_positions: Some(vec![-Vec3::X * 0.25]),
                        initial_velocity: Vec3::X * 30.0,
                        radius: 0.01,
                        spacing: 0.1,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                world.step(1.0 / 60.0);
                states.push((
                    world.fluids()[0].particles()[0],
                    world.fluids()[0].velocities()[0],
                ));
            }
            assert_eq!(states[0], states[1]);
            assert!((states[0].0.x + 0.06).abs() < 1e-6);
        }
    }

    #[test]
    fn swept_primitives_box_face_stops_public_particle() {
        for direction in [-1.0f32, 1.0] {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            world.add_builder(BodyBuilder::r#static(Shape::Box {
                half_extents: Vec3::splat(0.1),
            }));
            world
                .add_fluid(FluidDesc {
                    collision_mode: FluidCollisionMode::SweptPrimitives,
                    initial_positions: Some(vec![Vec3::X * (-0.25 * direction)]),
                    initial_velocity: Vec3::X * (30.0 * direction),
                    radius: 0.01,
                    spacing: 0.1,
                    ..FluidDesc::default()
                })
                .unwrap();
            assert_eq!(
                world.fluids()[0].velocities()[0],
                Vec3::X * (30.0 * direction)
            );
            world.step(1.0 / 60.0);
            let x = world.fluids()[0].particles()[0].x;
            assert!(
                (x * direction + 0.11).abs() < 1e-5,
                "direction={direction} x={x}"
            );
        }
    }

    #[test]
    fn finite_boundaries_respect_analytic_particle_segments() {
        let radius = 0.01;
        let mut failures = Vec::new();
        for sphere in [false, true] {
            for y in [0.0f32, 0.12, 0.2] {
                for direction in [-1.0f32, 1.0] {
                    let start = vec3(-0.25 * direction, y, 0.0);
                    let velocity = Vec3::X * (30.0 * direction);
                    // At y > .11 the complete segment misses both the radius-.1
                    // sphere expanded by .01 and the conservatively expanded box.
                    let misses = y > 0.1 + radius;
                    let mut states = Vec::new();
                    for variant in 0..3 {
                        let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                        if variant != 0 {
                            let shape = if sphere {
                                Shape::Sphere { radius: 0.1 }
                            } else {
                                Shape::Box {
                                    half_extents: Vec3::splat(0.1),
                                }
                            };
                            let handle = world
                                .add_builder(BodyBuilder::r#static(shape).sensor(variant == 1));
                            assert_eq!(world.body(handle).unwrap().position(), Vec3::ZERO);
                        }
                        world
                            .add_fluid(FluidDesc {
                                collision_mode: FluidCollisionMode::SweptSpheres,
                                initial_positions: Some(vec![start]),
                                initial_velocity: velocity,
                                radius,
                                spacing: 0.1,
                                ..FluidDesc::default()
                            })
                            .unwrap();
                        assert_eq!(world.fluids()[0].particles()[0], start);
                        assert_eq!(world.fluids()[0].velocities()[0], velocity);
                        assert!(velocity.length() < MAX_SPEED);
                        world.step(1.0 / 60.0);
                        let fluid = &world.fluids()[0];
                        states.push((fluid.particles()[0], fluid.velocities()[0]));
                    }
                    assert_eq!(states[0], states[1], "sensor control");
                    assert!((states[0].0.x - 0.25 * direction).abs() < 1e-6);
                    if misses {
                        if states[0] != states[2] {
                            failures.push(format!(
                                "miss sphere={sphere} y={y} dir={direction} states={states:?}"
                            ));
                        }
                    } else if sphere && states[2].0.x * direction > -0.1 - radius + 1e-5 {
                        failures.push(format!(
                            "cross sphere={sphere} y={y} dir={direction} states={states:?}"
                        ));
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "analytic finite-boundary differences: {failures:#?}"
        );
    }

    #[test]
    fn mesh_mode_preserves_primitives_and_selects_earliest() {
        for sphere in [false, true] {
            let mut states = Vec::new();
            for mode in [
                FluidCollisionMode::SweptPrimitives,
                FluidCollisionMode::SweptMeshes,
            ] {
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                world.add_builder(BodyBuilder::r#static(if sphere {
                    Shape::Sphere { radius: 0.1 }
                } else {
                    Shape::Box {
                        half_extents: Vec3::splat(0.1),
                    }
                }));
                world
                    .add_fluid(FluidDesc {
                        collision_mode: mode,
                        initial_positions: Some(vec![-Vec3::X * 0.25]),
                        initial_velocity: Vec3::X * 30.0,
                        radius: 0.01,
                        spacing: 0.1,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                let mut bits = Vec::new();
                for _ in 0..12 {
                    world.step(1.0 / 60.0);
                    let f = &world.fluids()[0];
                    bits.push((
                        f.particles()[0].to_array().map(f32::to_bits),
                        f.velocities()[0].to_array().map(f32::to_bits),
                    ));
                }
                states.push(bits);
            }
            assert_eq!(states[0], states[1]);
        }
        let mut states = Vec::new();
        for reverse_bodies in [false, true] {
            for reverse_triangles in [false, true] {
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                let vertices = [-0.05, 0.05]
                    .into_iter()
                    .flat_map(|x| [vec3(x, -1.0, -1.0), vec3(x, 1.0, -1.0), vec3(x, 0.0, 1.0)])
                    .collect();
                let mesh = Shape::TriMesh {
                    vertices,
                    indices: if reverse_triangles {
                        vec![3, 4, 5, 0, 1, 2]
                    } else {
                        vec![0, 1, 2, 3, 4, 5]
                    },
                };
                let mut shapes = vec![
                    mesh,
                    Shape::Sphere { radius: 0.02 },
                    Shape::Box {
                        half_extents: Vec3::splat(0.01),
                    },
                ];
                if reverse_bodies {
                    shapes.reverse();
                }
                for shape in shapes {
                    world.add_builder(BodyBuilder::r#static(shape));
                }
                world
                    .add_fluid(FluidDesc {
                        collision_mode: FluidCollisionMode::SweptMeshes,
                        initial_positions: Some(vec![-Vec3::X * 0.25]),
                        initial_velocity: Vec3::X * 30.0,
                        radius: 0.01,
                        spacing: 0.1,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                world.step(1.0 / 60.0);
                let f = &world.fluids()[0];
                assert!((f.particles()[0].x + 0.06).abs() < 1e-6);
                states.push((
                    f.particles()[0].to_array().map(f32::to_bits),
                    f.velocities()[0].to_array().map(f32::to_bits),
                ));
            }
        }
        assert!(states.iter().all(|s| s == &states[0]));
    }

    #[test]
    fn moving_sphere_radius_and_rotation_scope() {
        for radius in [-0.1, 0.0, 0.1] {
            for angular in [Vec3::ZERO, Vec3::Y] {
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                world.add_builder(
                    BodyBuilder::kinematic(Shape::Sphere { radius })
                        .position(Vec3::X * -0.25)
                        .linear_velocity(Vec3::X * 30.0)
                        .angular_velocity(angular),
                );
                world
                    .add_fluid(FluidDesc {
                        initial_positions: Some(vec![Vec3::ZERO]),
                        radius: 0.01,
                        spacing: 0.1,
                        viscosity: 0.0,
                        collision_mode: FluidCollisionMode::SweptMeshes,
                        boundary_motion_mode: FluidBoundaryMotionMode::TranslatingKinematicSpheres,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                world.step(1.0 / 60.0);
                let expected = if angular == Vec3::ZERO {
                    0.25 + radius.abs() + 0.01
                } else {
                    0.0
                };
                assert!((world.fluids()[0].particles()[0].x - expected).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn moving_sphere_static_safety_and_first_impact() {
        for scenario in 0..4 {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            let starts = if scenario == 3 {
                [-0.4, -0.25]
            } else {
                [-0.25, -0.4]
            };
            for x in starts.into_iter().take(if scenario < 2 { 1 } else { 2 }) {
                world.add_builder(
                    BodyBuilder::kinematic(Shape::Sphere { radius: 0.1 })
                        .position(Vec3::X * x)
                        .linear_velocity(Vec3::X * 30.0),
                );
            }
            if scenario < 2 {
                world.add_builder(
                    BodyBuilder::r#static(Shape::Box {
                        half_extents: vec3(0.005, 1.0, 1.0),
                    })
                    .position(Vec3::X * if scenario == 0 { -0.30 } else { 0.15 }),
                );
            }
            world
                .add_fluid(FluidDesc {
                    initial_positions: Some(vec![if scenario == 0 {
                        Vec3::X * -0.4
                    } else {
                        Vec3::ZERO
                    }]),
                    initial_velocity: if scenario == 0 {
                        Vec3::X * 36.0
                    } else {
                        Vec3::ZERO
                    },
                    radius: 0.01,
                    spacing: 0.1,
                    viscosity: 0.0,
                    collision_mode: FluidCollisionMode::SweptMeshes,
                    boundary_motion_mode: FluidBoundaryMotionMode::TranslatingKinematicSpheres,
                    ..FluidDesc::default()
                })
                .unwrap();
            world.step(1.0 / 60.0);
            let x = world.fluids()[0].particles()[0].x;
            if scenario < 2 {
                let wall = if scenario == 0 { -0.30 } else { 0.15 };
                assert!(
                    (x - (wall - 0.015)).abs() < 1e-5,
                    "scenario={scenario} x={x}"
                );
                if scenario == 0 {
                    let moving_hit_x = -0.4 + 0.6 * ((0.15 - 0.11) / 0.1);
                    assert!(wall - 0.015 < moving_hit_x);
                }
            } else {
                assert!((x - 0.36).abs() < 1e-5, "scenario={scenario} x={x}");
            }
        }
        for mode in [
            FluidCollisionMode::Legacy,
            FluidCollisionMode::SweptSpheres,
            FluidCollisionMode::SweptPrimitives,
        ] {
            assert!(!FluidDesc {
                collision_mode: mode,
                boundary_motion_mode: FluidBoundaryMotionMode::TranslatingKinematicSpheres,
                ..FluidDesc::default()
            }
            .is_valid());
        }
    }

    #[test]
    fn moving_body_local_extra_spheres_match_independent_boundaries() {
        use crate::body::CompoundTransformMode::BodyLocal;
        let mut failures = Vec::new();
        for sign in [-1.0f32, 1.0] {
            for case in 0..6 {
                let center = Vec3::X * (-0.25 * sign);
                let velocity = Vec3::X * (30.0 * sign);
                let particle = if case == 1 {
                    Vec3::Y * 0.12
                } else {
                    Vec3::ZERO
                };
                let mut records = Vec::new();
                for independent in [true, false] {
                    let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                    let builder = if independent {
                        BodyBuilder::kinematic(Shape::Sphere { radius: 0.1 }).position(center)
                    } else {
                        let primary = if case == 2 {
                            Shape::Box {
                                half_extents: Vec3::splat(0.05),
                            }
                        } else {
                            Shape::Sphere { radius: 0.05 }
                        };
                        let mut builder = BodyBuilder::kinematic(primary)
                            .position(center + Vec3::Y * 2.0)
                            .compound_transform_mode(BodyLocal);
                        let near = Vec3::Y * -2.0;
                        let far = near - Vec3::X * (0.15 * sign);
                        if case == 3 {
                            builder = builder.part_at(Shape::Sphere { radius: 0.1 }, far);
                        }
                        builder = builder.part_at(Shape::Sphere { radius: 0.1 }, near);
                        if case == 4 {
                            builder = builder.part_at(Shape::Sphere { radius: 0.1 }, far);
                        }
                        if case == 5 {
                            // Constant nonidentity body orientation: local Y offset is
                            // invariant but composition order is not (world center rotates otherwise).
                            builder = builder.rotation(Quat::from_rotation_y(0.7));
                        }
                        builder
                    }
                    .linear_velocity(velocity);
                    let handle = world.add_builder(builder);
                    let initial = world.body(handle).unwrap().transform;
                    assert_eq!(world.body(handle).unwrap().linear_velocity, velocity);
                    assert_eq!(world.body(handle).unwrap().angular_velocity, Vec3::ZERO);
                    world
                        .add_fluid(FluidDesc {
                            initial_positions: Some(vec![particle]),
                            radius: 0.01,
                            spacing: 0.1,
                            viscosity: 0.0,
                            collision_mode: FluidCollisionMode::SweptMeshes,
                            boundary_motion_mode:
                                FluidBoundaryMotionMode::TranslatingKinematicSpheres,
                            boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                            ..FluidDesc::default()
                        })
                        .unwrap();
                    world.step(1.0 / 60.0);
                    let body = world.body(handle).unwrap();
                    assert_eq!(body.prev_transform, initial);
                    assert_eq!(body.transform.rotation, initial.rotation);
                    assert!((body.position() - initial.position - velocity / 60.0).length() < 1e-6);
                    records.push((
                        world.fluids()[0].particles()[0],
                        world.fluids()[0].velocities()[0],
                    ));
                }
                if case != 1 {
                    assert!(records[0].0.x * sign > 0.359);
                }
                if records[0] != records[1] {
                    failures.push((sign, case, records));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "compound moving sphere differences: {failures:?}"
        );
    }

    #[test]
    fn moving_kinematic_sphere_liquid_crossing() {
        let mut failures = Vec::new();
        for sign in [-1.0f32, 1.0] {
            // Crossing, clear near miss, equal-speed motion, and a stationary control.
            for case in 0..4 {
                let center = Vec3::X * (-0.25 * sign);
                let body_v = if case == 3 {
                    Vec3::ZERO
                } else {
                    Vec3::X * (30.0 * sign)
                };
                let particle_start = if case == 1 {
                    Vec3::Y * 0.12
                } else {
                    Vec3::ZERO
                };
                let particle_v = if case == 2 { body_v } else { Vec3::ZERO };
                let mut records = Vec::new();
                for sensor in [false, true] {
                    let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                    let handle = world.add_builder(
                        BodyBuilder::kinematic(Shape::Sphere { radius: 0.1 })
                            .position(center)
                            .linear_velocity(body_v)
                            .sensor(sensor),
                    );
                    assert_eq!(world.body(handle).unwrap().linear_velocity, body_v);
                    assert_eq!(world.body(handle).unwrap().angular_velocity, Vec3::ZERO);
                    world
                        .add_fluid(FluidDesc {
                            initial_positions: Some(vec![particle_start]),
                            initial_velocity: particle_v,
                            radius: 0.01,
                            spacing: 0.1,
                            viscosity: 0.0,
                            collision_mode: FluidCollisionMode::SweptMeshes,
                            boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                            boundary_motion_mode:
                                FluidBoundaryMotionMode::TranslatingKinematicSpheres,
                            ..FluidDesc::default()
                        })
                        .unwrap();
                    assert_eq!(world.fluids()[0].velocities()[0], particle_v);
                    world.step(1.0 / 60.0);
                    let body = world.body(handle).unwrap();
                    assert_eq!(body.prev_transform.position, center);
                    assert!((body.position() - (center + body_v / 60.0)).length() < 1e-6);
                    let fluid = &world.fluids()[0];
                    records.push((fluid.particles()[0], fluid.velocities()[0]));
                }
                if case == 0 {
                    // The translated solid must carry a contacted particle ahead of its
                    // final leading surface, not pass completely through it.
                    let (p, v) = records[0];
                    if p.x * sign < 0.36 - 1e-5 || v.x * sign < 29.99 {
                        failures.push((sign, p, v));
                    }
                    assert_eq!(records[1], (particle_start, particle_v));
                } else {
                    assert_eq!(records[0], records[1], "case={case} sign={sign}");
                }
            }
        }
        assert!(
            failures.is_empty(),
            "moving sphere swept through liquid: {failures:?}"
        );
    }

    #[test]
    fn translating_box_shell_parts_and_static_safety() {
        for extra in [false, true] {
            for rotation in [Quat::IDENTITY, Quat::from_rotation_x(0.7)] {
                for motion in 0..3 {
                    let velocity = match motion {
                        0 => Vec3::X * 21.3,
                        1 => Vec3::X * -21.3,
                        _ => Vec3::Y * 21.3,
                    };
                    let offset = if extra { Vec3::X * 2.0 } else { Vec3::ZERO };
                    let shape = Shape::Box {
                        half_extents: vec3(0.01, 0.2, 0.2),
                    };
                    let builder = if extra {
                        BodyBuilder::kinematic(Shape::Sphere { radius: 0.01 })
                            .compound_transform_mode(crate::body::CompoundTransformMode::BodyLocal)
                            .part_at(shape, -offset)
                    } else {
                        BodyBuilder::kinematic(shape)
                    };
                    let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                    world.add_builder(
                        builder
                            .position(Vec3::X * -0.015 + offset)
                            .rotation(rotation)
                            .linear_velocity(velocity),
                    );
                    world
                        .add_fluid(FluidDesc {
                            initial_positions: Some(vec![Vec3::ZERO]),
                            radius: 0.01,
                            spacing: 0.1,
                            viscosity: 0.0,
                            collision_mode: FluidCollisionMode::SweptMeshes,
                            boundary_motion_mode:
                                FluidBoundaryMotionMode::TranslatingKinematicPrimitives,
                            ..FluidDesc::default()
                        })
                        .unwrap();
                    world.step(1.0 / 60.0);
                    let p = world.fluids()[0].particles()[0];
                    if motion == 0 {
                        assert!(p.x > 0.35, "extra={extra} p={p:?}");
                    } else {
                        assert_eq!(p, Vec3::ZERO);
                    }
                }
            }
        }
        for approach in [false, true] {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            world.add_builder(
                BodyBuilder::kinematic(Shape::Box {
                    half_extents: vec3(0.01, 0.2, 0.2),
                })
                .position(Vec3::X * -0.25)
                .linear_velocity(Vec3::X * 30.0),
            );
            let wall = if approach { -0.30 } else { 0.15 };
            world.add_builder(
                BodyBuilder::r#static(Shape::Box {
                    half_extents: vec3(0.005, 1.0, 1.0),
                })
                .position(Vec3::X * wall),
            );
            world
                .add_fluid(FluidDesc {
                    initial_positions: Some(vec![if approach {
                        Vec3::X * -0.4
                    } else {
                        Vec3::ZERO
                    }]),
                    initial_velocity: if approach { Vec3::X * 39.0 } else { Vec3::ZERO },
                    radius: 0.01,
                    spacing: 0.1,
                    viscosity: 0.0,
                    collision_mode: FluidCollisionMode::SweptMeshes,
                    boundary_motion_mode: FluidBoundaryMotionMode::TranslatingKinematicPrimitives,
                    ..FluidDesc::default()
                })
                .unwrap();
            world.step(1.0 / 60.0);
            let p = world.fluids()[0].particles()[0];
            assert!(
                (p.x - (wall - 0.015)).abs() < 2e-6,
                "approach={approach} p={p:?}"
            );
        }
    }

    #[test]
    fn translating_primitive_sphere_compatibility() {
        let mut states = Vec::new();
        for mode in [
            FluidBoundaryMotionMode::TranslatingKinematicSpheres,
            FluidBoundaryMotionMode::TranslatingKinematicPrimitives,
        ] {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            world.add_builder(
                BodyBuilder::kinematic(Shape::Sphere { radius: 0.1 })
                    .position(Vec3::X * -0.15)
                    .linear_velocity(Vec3::X * 5.0),
            );
            world
                .add_fluid(FluidDesc {
                    initial_positions: Some(vec![Vec3::ZERO]),
                    radius: 0.01,
                    spacing: 0.1,
                    collision_mode: FluidCollisionMode::SweptMeshes,
                    boundary_motion_mode: mode,
                    ..FluidDesc::default()
                })
                .unwrap();
            let mut bits = Vec::new();
            for _ in 0..8 {
                world.step(1.0 / 60.0);
                let f = &world.fluids()[0];
                bits.extend(f.particles()[0].to_array().map(f32::to_bits));
                bits.extend(f.velocities()[0].to_array().map(f32::to_bits));
            }
            states.push(bits);
        }
        assert_eq!(states[0], states[1]);
        assert!(!FluidDesc {
            boundary_motion_mode: FluidBoundaryMotionMode::TranslatingKinematicPrimitives,
            ..FluidDesc::default()
        }
        .is_valid());
    }

    #[test]
    fn translating_mixed_shapes_select_earliest_independent_of_body_order() {
        for box_first in [false, true] {
            let mut states = Vec::new();
            for reverse in [false, true] {
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                let mut builders = [
                    BodyBuilder::kinematic(Shape::Sphere { radius: 0.1 })
                        .position(Vec3::X * -0.25)
                        .linear_velocity(Vec3::X * 30.0),
                    BodyBuilder::kinematic(Shape::Box {
                        half_extents: vec3(0.01, 0.2, 0.2),
                    })
                    .position(Vec3::X * if box_first { -0.12 } else { -0.3 })
                    .linear_velocity(Vec3::X * 30.0),
                ];
                if reverse {
                    builders.reverse();
                }
                for builder in builders {
                    world.add_builder(builder);
                }
                world
                    .add_fluid(FluidDesc {
                        initial_positions: Some(vec![Vec3::ZERO]),
                        radius: 0.01,
                        spacing: 0.1,
                        viscosity: 0.0,
                        collision_mode: FluidCollisionMode::SweptMeshes,
                        boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                        boundary_motion_mode:
                            FluidBoundaryMotionMode::TranslatingKinematicPrimitives,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                // Sphere entry t=.28; box entry is .20 or .56. The final
                // leading position is outside both bodies, avoiding projection ambiguity.
                world.step(1.0 / 60.0);
                let f = &world.fluids()[0];
                let expected = if box_first { 0.4 } else { 0.36 };
                assert!((f.particles()[0].x - expected).abs() < 2e-6);
                assert!((f.velocities()[0].x - 30.0).abs() < 2e-4);
                states.push((
                    f.particles()[0].to_array().map(f32::to_bits),
                    f.velocities()[0].to_array().map(f32::to_bits),
                ));
            }
            assert_eq!(states[0], states[1]);
        }
    }

    #[test]
    fn translating_box_edge_corner_analytic_audit() {
        // 16 fixed trajectories: two edge orientations and two corner sides,
        // mirrored travel, primary/BodyLocal box. Analytic circle sections
        // distinguish rounded contact from the earlier inflated-AABB entry.
        let half = 0.1f32;
        let radius = 0.02f32;
        for sign in [-1.0f32, 1.0] {
            for extra in [false, true] {
                for (y, z) in [(0.11f32, 0.0f32), (0.0, 0.11), (0.11, 0.11), (-0.11, 0.11)] {
                    let dy = f64::from(y.abs().max(half)) - f64::from(half);
                    let dz = f64::from(z.abs().max(half)) - f64::from(half);
                    let x = (f64::from(radius).powi(2) - dy * dy - dz * dz).sqrt();
                    let entry = f64::from(half) + x;
                    let toi = (0.25 - entry) / 0.5;
                    let start = vec3(sign * 0.25, y, z);
                    let end = vec3(sign * -0.25, y, z);
                    let (actual_t, point, normal) =
                        sweep_rounded_box(start, end, Vec3::splat(half), radius).unwrap();
                    assert!((actual_t - toi).abs() < 1e-7);
                    assert!(actual_t > (0.25 - f64::from(half + radius)) / 0.5 + 0.001);
                    let expected_normal = vec3(
                        sign * (x / f64::from(radius)) as f32,
                        y.signum() * (dy / f64::from(radius)) as f32,
                        z.signum() * (dz / f64::from(radius)) as f32,
                    );
                    assert!((normal - expected_normal).length() < 2e-6);
                    assert!(
                        (point - vec3(sign * half, y.clamp(-half, half), z.clamp(-half, half)))
                            .length()
                            < 2e-6
                    );

                    let offset = if extra { Vec3::Y } else { Vec3::ZERO };
                    let shape = Shape::Box {
                        half_extents: Vec3::splat(half),
                    };
                    let builder = if extra {
                        BodyBuilder::kinematic(Shape::Sphere { radius: 0.01 })
                            .compound_transform_mode(crate::body::CompoundTransformMode::BodyLocal)
                            .part_at(shape, -offset)
                    } else {
                        BodyBuilder::kinematic(shape)
                    };
                    let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                    let handle = world.add_builder(
                        builder
                            .position(offset - Vec3::X * (sign * 0.25))
                            .linear_velocity(Vec3::X * (sign * 30.0)),
                    );
                    let particle = vec3(0.0, y, z);
                    world
                        .add_fluid(FluidDesc {
                            initial_positions: Some(vec![particle]),
                            radius,
                            spacing: 0.1,
                            viscosity: 0.0,
                            collision_mode: FluidCollisionMode::SweptMeshes,
                            boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                            boundary_motion_mode:
                                FluidBoundaryMotionMode::TranslatingKinematicPrimitives,
                            ..FluidDesc::default()
                        })
                        .unwrap();
                    let before = world.body(handle).unwrap().transform;
                    world.step(1.0 / 60.0);
                    let body = world.body(handle).unwrap();
                    assert_eq!(body.prev_transform, before);
                    assert_eq!(body.angular_velocity, Vec3::ZERO);
                    assert_eq!(body.transform.rotation, before.rotation);
                    assert!((body.position() - offset - Vec3::X * (sign * 0.25)).length() < 1e-6);
                    let expected_position = vec3(sign * (0.25 + entry) as f32, y, z);
                    let fluid = &world.fluids()[0];
                    assert!(
                        (fluid.particles()[0] - expected_position).length() < 3e-6,
                        "position sign={sign} extra={extra} yz={y},{z}: {:?}",
                        fluid.particles()[0]
                    );
                    // Remove inward relative normal speed, retain 80% tangent.
                    // Oblique contacts must not be mistaken for velocity=(30,0,0).
                    let carried_speed = sign * (0.5 * (1.0 - toi) * 60.0) as f32;
                    let relative_x = carried_speed - sign * 30.0;
                    let tangential =
                        Vec3::X * relative_x - expected_normal * (relative_x * expected_normal.x);
                    let expected_velocity = Vec3::X * (sign * 30.0) + tangential * 0.8;
                    assert!((fluid.velocities()[0] - expected_velocity).length() < 5e-4,
                        "velocity sign={sign} extra={extra} yz={y},{z}: actual={:?} expected={expected_velocity:?}", fluid.velocities()[0]);
                    let contact = fluid.contacts[0].expect("rounded feature contact");
                    assert!((contact.normal - expected_normal).length() < 3e-5);
                }
            }
        }
    }

    #[test]
    fn mesh_feature_projection_scope_and_degeneracy() {
        let vertices = vec![Vec3::ZERO, Vec3::Y, Vec3::Z];
        let feature =
            |p, v: &[Vec3]| closest_mesh_feature(p, v, &[0, 1, 2], Transform::IDENTITY).unwrap();
        assert!(feature(vec3(0.005, 0.3, 0.3), &vertices).3);
        assert!(!feature(vec3(0.005, -0.005, 0.3), &vertices).3);
        assert_eq!(feature(vec3(0.0, 0.3, 0.3), &vertices).2, 0.0);
        for v in [vec![Vec3::ZERO; 3], vec![Vec3::ZERO, Vec3::Y, Vec3::Y]] {
            let (point, normal, distance, face) = feature(Vec3::X * 0.005, &v);
            assert_eq!(point, Vec3::ZERO);
            assert_eq!(normal, Vec3::X);
            assert!((distance - 0.005).abs() < 1e-9);
            assert!(!face);
        }
        assert!(
            closest_mesh_feature(Vec3::ZERO, &vertices, &[0, 1, 99], Transform::IDENTITY).is_none()
        );
        let run = |motion, static_body, omega, start, velocity| {
            let mesh = Shape::TriMesh {
                vertices: vertices.clone(),
                indices: vec![0, 1, 2],
            };
            let builder = if static_body {
                BodyBuilder::r#static(mesh)
            } else {
                BodyBuilder::kinematic(mesh).kinematic_trimesh(true)
            };
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            world.add_builder(builder.angular_velocity(omega));
            world
                .add_fluid(FluidDesc {
                    initial_positions: Some(vec![start]),
                    initial_velocity: velocity,
                    radius: 0.02,
                    spacing: 0.1,
                    viscosity: 0.0,
                    collision_mode: FluidCollisionMode::SweptMeshes,
                    boundary_motion_mode: motion,
                    ..FluidDesc::default()
                })
                .unwrap();
            world.step(1.0 / 60.0);
            let f = &world.fluids()[0];
            (f.particles()[0], f.velocities()[0])
        };
        let new = FluidBoundaryMotionMode::TranslatingKinematicMeshes;
        let old = FluidBoundaryMotionMode::TranslatingKinematicPrimitives;
        let edge = vec3(0.005, -0.005, 0.3);
        let projected = run(new, false, Vec3::ZERO, edge, Vec3::ZERO).0;
        let expected = vec3(0.02 / 2.0f32.sqrt(), -0.02 / 2.0f32.sqrt(), 0.3);
        assert!((projected - expected).length() < 1e-6);
        // Static meshes, rotating boundaries and face interiors keep old arithmetic.
        for (static_body, omega, start, velocity) in [
            (true, Vec3::ZERO, edge, Vec3::ZERO),
            (false, Vec3::Y, edge, Vec3::ZERO),
            (false, Vec3::ZERO, vec3(0.005, 0.3, 0.3), Vec3::ZERO),
            (false, Vec3::ZERO, vec3(0.0, 0.3, 0.3), Vec3::ZERO),
            (false, Vec3::ZERO, vec3(-0.03, 0.3, 0.3), Vec3::X * 3.0),
        ] {
            assert_eq!(
                run(new, static_body, omega, start, velocity),
                run(old, static_body, omega, start, velocity)
            );
        }
        // A departing or tangential particle outside clearance is not projected to a face.
        for velocity in [Vec3::X, Vec3::Z] {
            let start = vec3(0.025, -0.01, 0.3);
            let p = run(new, false, Vec3::ZERO, start, velocity).0;
            assert!((p - (start + velocity / 60.0)).length() < 1e-6);
        }
    }

    #[test]
    fn translating_mesh_edge_vertex_analytic_audit() {
        // 16 trajectories, each with and without the moving-triangle cache.
        // Circle sections give an independent entry time outside the finite face.
        let triangle = [Vec3::ZERO, Vec3::Y, Vec3::Z];
        let radius = 0.02f32;
        for sign in [-1.0f32, 1.0] {
            for extra in [false, true] {
                for (y, z, nearest_y, nearest_z) in [
                    (-0.01f32, 0.3f32, 0.0f32, 0.3f32),
                    (0.3, -0.01, 0.3, 0.0),
                    (-0.01, -0.01, 0.0, 0.0),
                    (1.01, -0.01, 1.0, 0.0),
                ] {
                    let dy = f64::from(y) - f64::from(nearest_y);
                    let dz = f64::from(z) - f64::from(nearest_z);
                    let x = (f64::from(radius).powi(2) - dy * dy - dz * dz).sqrt();
                    let toi = (0.25 - x) / 0.5;
                    let expected_normal = vec3(
                        sign * (x / f64::from(radius)) as f32,
                        (dy / f64::from(radius)) as f32,
                        (dz / f64::from(radius)) as f32,
                    );
                    let nearest = vec3(0.0, nearest_y, nearest_z);
                    let (actual_t, point, normal) = sweep_triangle(
                        vec3(sign * 0.25, y, z),
                        vec3(sign * -0.25, y, z),
                        triangle,
                        radius,
                    )
                    .expect("finite triangle edge/vertex entry");
                    assert!((actual_t - toi).abs() < 1e-7);
                    assert!(actual_t > (0.25 - f64::from(radius)) / 0.5 + 0.001);
                    assert!((point - nearest).length() < 2e-6);
                    assert!((normal - expected_normal).length() < 2e-6);
                    let mut records = Vec::new();
                    for reference in [true, false] {
                        MOVING_MESH_REFERENCE.with(|flag| flag.set(reference));
                        let offset = if extra { Vec3::Y * 2.0 } else { Vec3::ZERO };
                        let mesh = Shape::TriMesh {
                            vertices: triangle.to_vec(),
                            indices: vec![0, 1, 2],
                        };
                        let builder = if extra {
                            BodyBuilder::kinematic(Shape::Sphere { radius: 0.01 })
                                .compound_transform_mode(
                                    crate::body::CompoundTransformMode::BodyLocal,
                                )
                                .part_at(mesh, -offset)
                        } else {
                            BodyBuilder::kinematic(mesh)
                        };
                        let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                        let velocity = Vec3::X * (sign * 30.0);
                        let handle = world.add_builder(
                            builder
                                .kinematic_trimesh(true)
                                .position(offset - Vec3::X * (sign * 0.25))
                                .linear_velocity(velocity),
                        );
                        world
                            .add_fluid(FluidDesc {
                                initial_positions: Some(vec![vec3(0.0, y, z)]),
                                radius,
                                spacing: 0.1,
                                viscosity: 0.0,
                                collision_mode: FluidCollisionMode::SweptMeshes,
                                boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                                boundary_motion_mode:
                                    FluidBoundaryMotionMode::TranslatingKinematicMeshes,
                                ..FluidDesc::default()
                            })
                            .unwrap();
                        let body = world.body(handle).unwrap();
                        let before = body.transform;
                        assert_eq!(body.body_type, crate::body::BodyType::Kinematic);
                        assert_eq!(body.linear_velocity, velocity);
                        assert_eq!(body.angular_velocity, Vec3::ZERO);
                        world.step(1.0 / 60.0);
                        let body = world.body(handle).unwrap();
                        assert_eq!(body.prev_transform, before);
                        assert_eq!(body.transform.rotation, before.rotation);
                        assert!(
                            (body.position() - offset - Vec3::X * (sign * 0.25)).length() < 1e-6
                        );
                        let f = &world.fluids()[0];
                        let expected_position = vec3(sign * (0.25 + x) as f32, y, z);
                        assert!((f.particles()[0] - expected_position).length() < 3e-6,
                            "carry sign={sign} extra={extra} yz={y},{z} reference={reference}: {:?}", f.particles()[0]);
                        // Existing contact response removes inward relative normal
                        // speed and retains 80% of relative tangential speed.
                        let relative_x = sign * (0.5 * (1.0 - toi) * 60.0) as f32 - sign * 30.0;
                        let tangent = Vec3::X * relative_x
                            - expected_normal * (relative_x * expected_normal.x);
                        let expected_velocity = velocity + tangent * 0.8;
                        assert!(
                            (f.velocities()[0] - expected_velocity).length() < 5e-4,
                            "response sign={sign} extra={extra} yz={y},{z}: {:?}",
                            f.velocities()[0]
                        );
                        let contact = f.contacts[0].expect("oblique moving mesh contact");
                        assert!((contact.normal - expected_normal).length() < 3e-5);
                        assert!(
                            (contact.point - (nearest + Vec3::X * (sign * 0.25))).length() < 3e-6
                        );
                        assert!((contact.velocity - velocity).length() < 1e-5);
                        let mut bits = vec![contact.body];
                        for v in [
                            f.particles()[0],
                            f.velocities()[0],
                            contact.point,
                            contact.normal,
                            contact.velocity,
                        ] {
                            bits.extend(v.to_array().map(f32::to_bits));
                        }
                        records.push(bits);
                    }
                    MOVING_MESH_REFERENCE.with(|flag| flag.set(false));
                    assert_eq!(records[0], records[1], "cache changed edge/vertex response");
                }
            }
        }
    }

    #[test]
    fn translating_mesh_features_parts_and_safety() {
        for kind in 0..5 {
            for extra in [false, true] {
                for motion in 0..3 {
                    let vertices = match kind {
                        0 => vec![
                            vec3(0.0, -1.0, -1.0),
                            vec3(0.0, 1.0, -1.0),
                            vec3(0.0, 0.0, 1.0),
                        ],
                        1 => vec![Vec3::Y * -1.0, Vec3::Y, Vec3::Y],
                        2 => vec![Vec3::ZERO; 3],
                        _ => vec![Vec3::ZERO, Vec3::Y, Vec3::Z],
                    };
                    let indices = match kind {
                        3 => vec![],
                        4 => vec![0, 1, 99],
                        _ => vec![0, 1, 2],
                    };
                    let mesh = Shape::TriMesh { vertices, indices };
                    let offset = if extra { Vec3::Y * 2.0 } else { Vec3::ZERO };
                    let builder = if extra {
                        BodyBuilder::kinematic(Shape::Sphere { radius: 0.01 })
                            .compound_transform_mode(crate::body::CompoundTransformMode::BodyLocal)
                            .part_at(mesh, -offset)
                            .rotation(Quat::from_rotation_y(0.4))
                    } else {
                        BodyBuilder::kinematic(mesh)
                    };
                    let velocity = match motion {
                        0 => Vec3::X * 15.3,
                        1 => Vec3::X * -15.3,
                        _ => Vec3::Z * 15.3,
                    };
                    let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                    world.add_builder(
                        builder
                            .kinematic_trimesh(true)
                            .position(offset - Vec3::X * 0.005)
                            .linear_velocity(velocity),
                    );
                    world
                        .add_fluid(FluidDesc {
                            initial_positions: Some(vec![Vec3::ZERO]),
                            radius: 0.01,
                            spacing: 0.1,
                            viscosity: 0.0,
                            collision_mode: FluidCollisionMode::SweptMeshes,
                            boundary_motion_mode:
                                FluidBoundaryMotionMode::TranslatingKinematicMeshes,
                            ..FluidDesc::default()
                        })
                        .unwrap();
                    world.step(1.0 / 60.0);
                    let p = world.fluids()[0].particles()[0];
                    if kind < 3 && motion == 0 {
                        assert!(p.x > 0.24, "kind={kind} extra={extra} p={p:?}");
                    }
                    // Rotated finite faces have a nonzero normal component along Z.
                    if kind >= 3 || motion == 1 {
                        assert_eq!(p, Vec3::ZERO);
                    }
                    if motion == 2 && !extra {
                        // The existing endpoint projection may push a tangent
                        // face out of the clearance shell, without carrying it.
                        assert!(p.length() <= 0.011, "kind={kind} tangent p={p:?}");
                    }
                }
            }
        }
        for approach in [false, true] {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            world.add_builder(
                BodyBuilder::kinematic(Shape::TriMesh {
                    vertices: vec![
                        vec3(0.0, -2.0, -2.0),
                        vec3(0.0, 2.0, -2.0),
                        vec3(0.0, 0.0, 2.0),
                    ],
                    indices: vec![0, 1, 2],
                })
                .kinematic_trimesh(true)
                .position(Vec3::X * -0.25)
                .linear_velocity(Vec3::X * 30.0),
            );
            let wall = if approach { -0.30 } else { 0.15 };
            world.add_builder(
                BodyBuilder::r#static(Shape::Box {
                    half_extents: vec3(0.005, 1.0, 1.0),
                })
                .position(Vec3::X * wall),
            );
            world
                .add_fluid(FluidDesc {
                    initial_positions: Some(vec![if approach {
                        Vec3::X * -0.4
                    } else {
                        Vec3::ZERO
                    }]),
                    initial_velocity: if approach { Vec3::X * 39.0 } else { Vec3::ZERO },
                    radius: 0.01,
                    spacing: 0.1,
                    viscosity: 0.0,
                    collision_mode: FluidCollisionMode::SweptMeshes,
                    boundary_motion_mode: FluidBoundaryMotionMode::TranslatingKinematicMeshes,
                    ..FluidDesc::default()
                })
                .unwrap();
            world.step(1.0 / 60.0);
            assert!((world.fluids()[0].particles()[0].x - (wall - 0.015)).abs() < 2e-6);
        }
    }

    #[test]
    fn translating_mesh_mixed_body_earliest_order() {
        for mesh_first in [false, true] {
            let mut records = Vec::new();
            for reverse in [false, true] {
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                let mesh_start = if mesh_first { -0.1 } else { -0.2 };
                let mesh = BodyBuilder::kinematic(Shape::TriMesh {
                    vertices: vec![
                        vec3(0.0, -1.0, -1.0),
                        vec3(0.0, 1.0, -1.0),
                        vec3(0.0, 0.0, 1.0),
                    ],
                    indices: vec![0, 1, 2],
                })
                .kinematic_trimesh(true)
                .position(Vec3::X * mesh_start)
                .linear_velocity(Vec3::X * 30.0);
                let sphere = BodyBuilder::kinematic(Shape::Sphere { radius: 0.1 })
                    .position(Vec3::X * -0.25)
                    .linear_velocity(Vec3::X * 30.0);
                if reverse {
                    world.add_builder(sphere);
                    world.add_builder(mesh);
                } else {
                    world.add_builder(mesh);
                    world.add_builder(sphere);
                }
                world
                    .add_fluid(FluidDesc {
                        initial_positions: Some(vec![Vec3::ZERO]),
                        radius: 0.01,
                        spacing: 0.1,
                        collision_mode: FluidCollisionMode::SweptMeshes,
                        boundary_motion_mode: FluidBoundaryMotionMode::TranslatingKinematicMeshes,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                world.step(1.0 / 60.0);
                let f = &world.fluids()[0];
                assert!((f.particles()[0].x - if mesh_first { 0.41 } else { 0.36 }).abs() < 2e-6);
                records.push((
                    f.particles()[0].to_array().map(f32::to_bits),
                    f.velocities()[0].to_array().map(f32::to_bits),
                ));
            }
            assert_eq!(records[0], records[1]);
        }
    }

    #[test]
    fn translating_mesh_triangle_order_and_primitive_compatibility() {
        for reverse in [false, true] {
            let mut vertices = Vec::new();
            for x in if reverse { [0.0, 0.1] } else { [0.1, 0.0] } {
                vertices.extend([vec3(x, -1.0, -1.0), vec3(x, 1.0, -1.0), vec3(x, 0.0, 1.0)]);
            }
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            world.add_builder(
                BodyBuilder::kinematic(Shape::TriMesh {
                    vertices,
                    indices: vec![0, 1, 2, 3, 4, 5],
                })
                .kinematic_trimesh(true)
                .position(Vec3::X * -0.25)
                .linear_velocity(Vec3::X * 30.0),
            );
            world
                .add_fluid(FluidDesc {
                    initial_positions: Some(vec![Vec3::ZERO]),
                    radius: 0.01,
                    spacing: 0.1,
                    collision_mode: FluidCollisionMode::SweptMeshes,
                    boundary_motion_mode: FluidBoundaryMotionMode::TranslatingKinematicMeshes,
                    ..FluidDesc::default()
                })
                .unwrap();
            world.step(1.0 / 60.0);
            assert!((world.fluids()[0].particles()[0].x - 0.36).abs() < 2e-6);
        }
        for shape in [
            Shape::Sphere { radius: 0.1 },
            Shape::Box {
                half_extents: Vec3::splat(0.1),
            },
        ] {
            let mut records = Vec::new();
            for mode in [
                FluidBoundaryMotionMode::TranslatingKinematicPrimitives,
                FluidBoundaryMotionMode::TranslatingKinematicMeshes,
            ] {
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                world.add_builder(
                    BodyBuilder::kinematic(shape.clone())
                        .position(Vec3::X * -0.15)
                        .linear_velocity(Vec3::X * 5.0),
                );
                world
                    .add_fluid(FluidDesc {
                        initial_positions: Some(vec![Vec3::ZERO]),
                        radius: 0.01,
                        spacing: 0.1,
                        collision_mode: FluidCollisionMode::SweptMeshes,
                        boundary_motion_mode: mode,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                let mut bits = Vec::new();
                for _ in 0..8 {
                    world.step(1.0 / 60.0);
                    let f = &world.fluids()[0];
                    bits.extend(f.particles()[0].to_array().map(f32::to_bits));
                    bits.extend(f.velocities()[0].to_array().map(f32::to_bits));
                }
                records.push(bits);
            }
            assert_eq!(records[0], records[1]);
        }
    }

    #[test]
    fn translating_mesh_public_entry_matrix() {
        // New capability: current primitive motion policy does not sweep meshes.
        // Sixteen fixed trajectories, with historical motion and sensor controls.
        let mut failures = Vec::new();
        let mut crossing_cases = 0;
        let mut miss_cases = 0;
        for sign in [-1.0f32, 1.0] {
            for reverse in [false, true] {
                for y in [0.1f32, 0.0, -0.02, 0.7] {
                    let particle = vec3(0.0, y, 0.0);
                    let velocity = Vec3::X * (30.0 * sign);
                    let mut states = Vec::new();
                    for mode in [
                        FluidBoundaryMotionMode::Legacy,
                        FluidBoundaryMotionMode::TranslatingKinematicMeshes,
                    ] {
                        for sensor in [false, true] {
                            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                            let handle = world.add_builder(
                                BodyBuilder::kinematic(Shape::TriMesh {
                                    vertices: vec![
                                        vec3(0.0, 0.0, -0.5),
                                        vec3(0.0, 0.5, -0.5),
                                        vec3(0.0, 0.0, 0.5),
                                    ],
                                    indices: if reverse {
                                        vec![0, 2, 1]
                                    } else {
                                        vec![0, 1, 2]
                                    },
                                })
                                .kinematic_trimesh(true)
                                .position(Vec3::X * (-0.25 * sign))
                                .linear_velocity(velocity)
                                .sensor(sensor),
                            );
                            world
                                .add_fluid(FluidDesc {
                                    initial_positions: Some(vec![particle]),
                                    initial_velocity: Vec3::ZERO,
                                    radius: 0.01,
                                    spacing: 0.1,
                                    viscosity: 0.0,
                                    collision_mode: FluidCollisionMode::SweptMeshes,
                                    boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                                    boundary_motion_mode: mode,
                                    ..FluidDesc::default()
                                })
                                .unwrap();
                            let before = world.body(handle).unwrap().transform;
                            assert_eq!(
                                world.body(handle).unwrap().body_type,
                                crate::body::BodyType::Kinematic,
                                "kinematic mesh must remain kinematic before testing fluid motion"
                            );
                            assert_eq!(world.body(handle).unwrap().linear_velocity, velocity);
                            assert_eq!(world.fluids()[0].velocities()[0], Vec3::ZERO);
                            world.step(1.0 / 60.0);
                            let body = world.body(handle).unwrap();
                            assert_eq!(body.prev_transform, before);
                            assert_eq!(body.angular_velocity, Vec3::ZERO);
                            assert_eq!(body.transform.rotation, before.rotation);
                            assert!((body.position() - Vec3::X * (0.25 * sign)).length() < 1e-6);
                            let f = &world.fluids()[0];
                            states.push((f.particles()[0], f.velocities()[0]));
                        }
                    }
                    assert_eq!(states[0], (particle, Vec3::ZERO), "historical motion");
                    assert_eq!(states[1], (particle, Vec3::ZERO), "historical sensor");
                    assert_eq!(states[3], (particle, Vec3::ZERO), "motion sensor");
                    if y == 0.0 || y == 0.1 {
                        crossing_cases += 1;
                        // Relative entry t=(.25-.01)/.5=.48; carry .5*(1-t)=.26.
                        if (states[2].0 - vec3(sign * 0.26, y, 0.0)).length() > 2e-6
                            || (states[2].1 - velocity).length() > 2e-4
                        {
                            failures.push((sign, reverse, y, states[2]));
                        }
                    } else {
                        miss_cases += 1;
                        assert_eq!(
                            states[2], states[3],
                            "near miss sign={sign} reverse={reverse} y={y}"
                        );
                    }
                }
            }
        }
        assert_eq!((crossing_cases, miss_cases), (8, 8));
        eprintln!("moving mesh matrix: {crossing_cases} crossings, {miss_cases} misses, {} crossing failures; all 64 public worlds executed", failures.len());
        assert!(failures.is_empty(), "moving mesh entry gaps: {failures:?}");
    }

    #[test]
    fn translating_box_public_entry_matrix() {
        // New capability audit: the existing sphere-only policy cannot sweep boxes.
        let mut failures = Vec::new();
        for sign in [-1.0f32, 1.0] {
            for case in 0..5 {
                let particle = match case {
                    1 => vec3(0.0, 0.212, 0.0),
                    // Inside an inflated AABB, outside the exact rounded corner.
                    2 => vec3(0.0, 0.208, 0.208),
                    _ => Vec3::ZERO,
                };
                let boundary_velocity = if case == 4 {
                    Vec3::ZERO
                } else {
                    Vec3::X * (30.0 * sign)
                };
                let particle_velocity = if case == 3 {
                    boundary_velocity
                } else {
                    Vec3::ZERO
                };
                let mut states = Vec::new();
                for sensor in [false, true] {
                    let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                    let handle = world.add_builder(
                        BodyBuilder::kinematic(Shape::Box {
                            half_extents: vec3(0.01, 0.2, 0.2),
                        })
                        .position(Vec3::X * (-0.25 * sign))
                        .linear_velocity(boundary_velocity)
                        .sensor(sensor),
                    );
                    world
                        .add_fluid(FluidDesc {
                            initial_positions: Some(vec![particle]),
                            initial_velocity: particle_velocity,
                            radius: 0.01,
                            spacing: 0.1,
                            viscosity: 0.0,
                            collision_mode: FluidCollisionMode::SweptMeshes,
                            boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                            boundary_motion_mode:
                                FluidBoundaryMotionMode::TranslatingKinematicPrimitives,
                            ..FluidDesc::default()
                        })
                        .unwrap();
                    let before = world.body(handle).unwrap().transform;
                    assert_eq!(
                        world.body(handle).unwrap().linear_velocity,
                        boundary_velocity
                    );
                    assert_eq!(world.fluids()[0].velocities()[0], particle_velocity);
                    world.step(1.0 / 60.0);
                    let body = world.body(handle).unwrap();
                    assert_eq!(body.prev_transform, before);
                    assert_eq!(body.angular_velocity, Vec3::ZERO);
                    assert_eq!(body.transform.rotation, before.rotation);
                    assert!(
                        (body.position() - before.position - boundary_velocity / 60.0).length()
                            < 1e-6
                    );
                    let f = &world.fluids()[0];
                    states.push((f.particles()[0], f.velocities()[0]));
                }
                if case == 0 {
                    if (states[0].0.x - sign * 0.27).abs() > 2e-6
                        || (states[0].1.x - sign * 30.0).abs() > 2e-4
                    {
                        failures.push((sign, case, states[0]));
                    }
                    assert_eq!(states[1], (particle, particle_velocity));
                } else {
                    assert_eq!(states[0], states[1], "control sign={sign} case={case}");
                }
            }
        }
        assert!(failures.is_empty(), "moving box entry gaps: {failures:?}");
    }

    fn moving_mesh_scale_fixture(count: usize, separation: bool) -> PhysicsWorld {
        let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for i in 0..count {
            let offset = if i == 0 {
                Vec3::ZERO
            } else {
                vec3(10.0 + i as f32, 10.0, 0.0)
            };
            let first = vertices.len() as u32;
            vertices.extend(
                [
                    vec3(0.0, -1.0, -1.0),
                    vec3(0.0, 1.0, -1.0),
                    vec3(0.0, 0.0, 1.0),
                ]
                .map(|p| p + offset),
            );
            indices.extend([first, first + 1, first + 2]);
        }
        world.add_builder(
            BodyBuilder::kinematic(Shape::TriMesh { vertices, indices })
                .kinematic_trimesh(true)
                .position(Vec3::X * -0.05)
                .linear_velocity(Vec3::X * 10.0),
        );
        world
            .add_fluid(FluidDesc {
                initial_positions: Some(
                    (0..32)
                        .map(|i| {
                            vec3(
                                0.0,
                                (i % 8) as f32 * 0.003 - 0.0105,
                                (i / 8) as f32 * 0.003 - 0.0045,
                            )
                        })
                        .collect(),
                ),
                radius: 0.01,
                spacing: 0.03,
                viscosity: 0.0,
                particle_collision: separation,
                collision_mode: FluidCollisionMode::SweptMeshes,
                boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                boundary_motion_mode: FluidBoundaryMotionMode::TranslatingKinematicMeshes,
                ..FluidDesc::default()
            })
            .unwrap();
        world
    }

    fn moving_mesh_scale_step(
        count: usize,
        separation: bool,
    ) -> (Vec<u32>, [usize; 2], f64, usize) {
        let mut world = moving_mesh_scale_fixture(count, separation);
        let ray = world
            .raycast(Vec3::new(-0.2, 0.0, 0.0), Vec3::X, 1.0)
            .unwrap();
        let handle = ray.handle;
        let before = world.body(handle).unwrap().transform;
        assert_eq!(world.body(handle).unwrap().linear_velocity, Vec3::X * 10.0);
        MOVING_MESH_BOUNDS.with(|n| n.set(0));
        MOVING_MESH_WORK.with(|n| n.set([0; 2]));
        MOVING_SCAN_WORK.with(|n| n.set([0; 4]));
        SEPARATION_PUSHES.with(|n| n.set(0));
        let start = std::time::Instant::now();
        world.step(1.0 / 60.0);
        let ms = start.elapsed().as_secs_f64() * 1000.0;
        let body = world.body(handle).unwrap();
        assert_eq!(body.prev_transform, before);
        assert_eq!(body.angular_velocity, Vec3::ZERO);
        assert_eq!(body.transform.rotation, before.rotation);
        assert!((body.position().x - (-0.05 + 10.0 / 60.0)).abs() < 1e-6);
        assert_eq!(
            MOVING_SCAN_WORK.with(|n| n.get()[3]),
            32,
            "actual accepted moving contacts"
        );
        let pushes = SEPARATION_PUSHES.with(|n| n.get());
        if separation {
            assert!(pushes > 0);
        }
        let f = &world.fluids()[0];
        let mut bits = Vec::new();
        for (p, v) in f.particles().iter().zip(f.velocities()) {
            assert!(p.is_finite() && v.is_finite());
            assert!(p.x - body.position().x >= 0.01 - 2e-6, "clearance: {p:?}");
            bits.extend(p.to_array().map(f32::to_bits));
            bits.extend(v.to_array().map(f32::to_bits));
        }
        (bits, MOVING_MESH_WORK.with(|n| n.get()), ms, pushes)
    }

    #[test]
    #[ignore = "fixed 18 full-step current-correctness projection measurements"]
    fn moving_mesh_projection_scale_baseline() {
        MOVING_MESH_REFERENCE.with(|f| f.set(false));
        for round in 0..3 {
            for separation in [false, true] {
                let mut reference = None;
                for count in [1, 128, 2048] {
                    MESH_PROJECTION_WORK.with(|n| n.set([0; 4]));
                    let (bits, sweep, ms, pushes) = moving_mesh_scale_step(count, separation);
                    if let Some(expected) = &reference {
                        assert_eq!(&bits, expected);
                    } else {
                        reference = Some(bits);
                    }
                    let work = MESH_PROJECTION_WORK.with(|n| n.get());
                    assert!(work[2] > 0, "closest-feature path must actually execute");
                    eprintln!("MESH_PROJECTION_BASE round={round} triangles={count} separation={separation} ms={ms:.6} visits={} transforms={} features={} radial_contacts={} sweep_transforms={} sweep_features={} moving_hits=32 pushes={pushes}", work[0], work[1], work[2], work[3], sweep[0], sweep[1]);
                }
            }
        }
    }

    #[test]
    #[ignore = "known unmet projection budget; cache withdrawn after single-triangle regression"]
    fn moving_mesh_projection_feature_budget() {
        MOVING_MESH_REFERENCE.with(|f| f.set(false));
        MESH_PROJECTION_WORK.with(|n| n.set([0; 4]));
        let _ = moving_mesh_scale_step(2048, true);
        let work = MESH_PROJECTION_WORK.with(|n| n.get());
        assert!(
            work[2] < 10_000,
            "closest-feature queries {} exceed 10000 budget; visits={} transforms={}",
            work[2],
            work[0],
            work[1]
        );
    }

    #[test]
    fn moving_mesh_projection_scale_state_matches() {
        MOVING_MESH_REFERENCE.with(|f| f.set(false));
        for separation in [false, true] {
            MESH_PROJECTION_WORK.with(|n| n.set([0; 4]));
            let (small, _, _, small_pushes) = moving_mesh_scale_step(1, separation);
            let small_work = MESH_PROJECTION_WORK.with(|n| n.get());
            MESH_PROJECTION_WORK.with(|n| n.set([0; 4]));
            let (large, _, _, large_pushes) = moving_mesh_scale_step(128, separation);
            let large_work = MESH_PROJECTION_WORK.with(|n| n.get());
            assert_eq!(small, large);
            assert_eq!(small_pushes, large_pushes);
            assert!(small_work[2] > 0);
            assert_eq!(large_work[2], small_work[2] * 128);
            assert_eq!(small_work[3], 0); // Face-interior fixture, not radial-contact coverage.
            assert_eq!(large_work[3], 0);
        }
    }

    #[test]
    #[ignore = "fixed full-step translating mesh scale baseline"]
    fn moving_mesh_scale_baseline() {
        MOVING_MESH_REFERENCE.with(|f| f.set(true));
        for round in 0..3 {
            for separation in [false, true] {
                let mut reference = None;
                for count in [1, 128, 2048] {
                    let (bits, work, ms, pushes) = moving_mesh_scale_step(count, separation);
                    if let Some(expected) = &reference {
                        assert_eq!(&bits, expected);
                    } else {
                        reference = Some(bits);
                    }
                    eprintln!("MOVING_MESH_BASE round={round} triangles={count} separation={separation} ms={ms:.6} transforms={} features={} hits=32 pushes={pushes}", work[0], work[1]);
                }
            }
        }
    }

    #[test]
    #[ignore = "fixed 36 full-step moving mesh cache comparisons"]
    fn moving_mesh_cache_ab() {
        for round in 0..3 {
            for separation in [false, true] {
                for count in [1, 128, 2048] {
                    let mut states = Vec::new();
                    for reference in if round % 2 == 0 {
                        [true, false]
                    } else {
                        [false, true]
                    } {
                        MOVING_MESH_REFERENCE.with(|f| f.set(reference));
                        let (bits, work, ms, pushes) = moving_mesh_scale_step(count, separation);
                        let bounds = MOVING_MESH_BOUNDS.with(|n| n.get());
                        eprintln!("MOVING_CACHE_AB round={round} triangles={count} separation={separation} reference={reference} ms={ms:.6} transforms={} features={} bounds={bounds} pushes={pushes}", work[0], work[1]);
                        if !reference {
                            assert!(work[0] <= count);
                            assert!(work[1] < 10000);
                        }
                        states.push(bits);
                    }
                    assert_eq!(states[0], states[1]);
                }
            }
        }
        MOVING_MESH_REFERENCE.with(|f| f.set(false));
    }

    #[test]
    fn moving_mesh_cache_state_and_refresh_audit() {
        let mut hits = [0usize; 2];
        for case in 0..32 {
            let kind = case % 8;
            let mut vertices = match kind {
                2 => vec![-Vec3::Y, Vec3::Y, Vec3::Y],
                3 => vec![Vec3::ZERO; 3],
                _ => vec![
                    vec3(0.0, -2.0, -2.0),
                    vec3(0.0, 2.0, -2.0),
                    vec3(0.0, 0.0, 2.0),
                ],
            };
            let mut indices = if kind == 4 { vec![] } else { vec![0, 1, 2] };
            if kind == 1 {
                indices.reverse();
            }
            if kind == 5 {
                indices.extend([0, 1, 999]);
            }
            if kind == 6 {
                vertices.extend([Vec3::splat(f32::NAN); 3]);
                indices.extend([3, 4, 5]);
            }
            let shape = Shape::TriMesh { vertices, indices };
            let extra = case >= 16;
            let rotation = if case >= 8 {
                Quat::from_rotation_x(0.4)
            } else {
                Quat::IDENTITY
            };
            let offset = if extra { Vec3::X * 2.0 } else { Vec3::ZERO };
            let initial = if kind == 7 { -0.005 } else { -0.05 };
            let speed = if case % 4 == 1 { -10.0 } else { 10.0 };
            let builder = || {
                let b = if extra {
                    BodyBuilder::kinematic(Shape::Sphere { radius: 0.001 })
                        .part_at(shape.clone(), -offset)
                        .compound_transform_mode(crate::body::CompoundTransformMode::BodyLocal)
                } else {
                    BodyBuilder::kinematic(shape.clone())
                };
                b.kinematic_trimesh(true)
                    .position(offset + Vec3::X * initial)
                    .rotation(rotation)
                    .linear_velocity(Vec3::X * speed)
            };
            let mut records = Vec::new();
            for (path, reference) in [true, false].into_iter().enumerate() {
                MOVING_MESH_REFERENCE.with(|f| f.set(reference));
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                let mut wall = world.add_builder(builder());
                if case % 4 == 0 {
                    world.add_builder(
                        BodyBuilder::r#static(Shape::Box {
                            half_extents: vec3(0.005, 2.0, 2.0),
                        })
                        .position(Vec3::X * 0.08),
                    );
                }
                world
                    .add_fluid(FluidDesc {
                        initial_positions: Some(vec![Vec3::ZERO]),
                        radius: 0.01,
                        spacing: 0.1,
                        viscosity: 0.0,
                        collision_mode: FluidCollisionMode::SweptMeshes,
                        boundary_motion_mode: FluidBoundaryMotionMode::TranslatingKinematicMeshes,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                let mut bits = Vec::new();
                for step in 0..5 {
                    if step == 1 {
                        let mut pose = world.body(wall).unwrap().transform;
                        pose.position += Vec3::Y * 10.0;
                        world.body_mut(wall).unwrap().set_transform(pose);
                    }
                    if step == 2 {
                        world.remove_body(wall).unwrap();
                    }
                    if step == 3 {
                        wall = world.add_builder(builder());
                    }
                    // Revisit the same particle trajectory after public body edits.
                    let f = &mut world.fluids_mut()[0];
                    f.positions[0] = Vec3::ZERO;
                    f.velocities[0] = Vec3::ZERO;
                    world.step(1.0 / 60.0);
                    let f = &world.fluids()[0];
                    for v in f
                        .positions
                        .iter()
                        .chain(&f.velocities)
                        .chain(&f.predicted)
                        .chain(&f.delta)
                    {
                        bits.extend(v.to_array().map(f32::to_bits));
                    }
                    bits.extend(f.densities.iter().chain(&f.lambdas).map(|v| v.to_bits()));
                    for c in &f.contacts {
                        bits.push(u32::from(c.is_some()));
                        if let Some(c) = c {
                            hits[path] += 1;
                            bits.push(c.body);
                            for v in [c.point, c.normal, c.velocity] {
                                bits.extend(v.to_array().map(f32::to_bits));
                            }
                        }
                    }
                    bits.push(f.revision() as u32);
                }
                records.push(bits);
            }
            assert_eq!(records[0], records[1], "case={case}");
        }
        MOVING_MESH_REFERENCE.with(|f| f.set(false));
        assert!(hits[0] > 20, "non-vacuous contact count: {hits:?}");
        assert_eq!(hits[0], hits[1]);
    }

    #[test]
    fn moving_mesh_scale_feature_budget() {
        let (_, work, _, _) = moving_mesh_scale_step(2048, true);
        assert!(
            work[1] < 10_000,
            "moving mesh feature budget exceeded: {work:?}"
        );
    }

    fn moving_scan_fixture(count: usize, separation: bool) -> PhysicsWorld {
        let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
        for i in 0..count {
            let position = if i == 0 {
                vec3(-0.15, 0.0, 0.0)
            } else {
                vec3(10.0 + i as f32, 10.0, 0.0)
            };
            world.add_builder(
                BodyBuilder::kinematic(Shape::Sphere { radius: 0.1 })
                    .position(position)
                    .linear_velocity(Vec3::X * 10.0),
            );
        }
        world
            .add_fluid(FluidDesc {
                initial_positions: Some(
                    (0..32)
                        .map(|i| {
                            vec3(
                                0.0,
                                (i % 8) as f32 * 0.003 - 0.0105,
                                (i / 8) as f32 * 0.003 - 0.0045,
                            )
                        })
                        .collect(),
                ),
                radius: 0.01,
                spacing: 0.03,
                viscosity: 0.0,
                particle_collision: separation,
                collision_mode: FluidCollisionMode::SweptMeshes,
                boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                boundary_motion_mode: FluidBoundaryMotionMode::TranslatingKinematicSpheres,
                ..FluidDesc::default()
            })
            .unwrap();
        world
    }

    #[test]
    #[ignore = "diagnostic budget: current implementation exceeds 10000 relative queries"]
    fn moving_scan_query_budget() {
        let mut world = moving_scan_fixture(1024, true);
        MOVING_SCAN_WORK.with(|n| n.set([0; 4]));
        world.step(1.0 / 60.0);
        let work = MOVING_SCAN_WORK.with(|n| n.get());
        assert!(work[3] > 0);
        assert!(
            work[2] < 10_000,
            "moving relative query budget exceeded: {work:?}"
        );
    }

    #[test]
    fn moving_scan_scale_preserves_state() {
        for separation in [false, true] {
            let mut states = Vec::new();
            for count in [1, 128] {
                let mut world = moving_scan_fixture(count, separation);
                MOVING_SCAN_WORK.with(|n| n.set([0; 4]));
                SEPARATION_PUSHES.with(|n| n.set(0));
                world.step(1.0 / 60.0);
                assert_eq!(MOVING_SCAN_WORK.with(|n| n.get()[3]), 32);
                if separation {
                    assert!(SEPARATION_PUSHES.with(|n| n.get()) > 0);
                }
                let mut bits = Vec::new();
                let f = &world.fluids()[0];
                for (p, v) in f.particles().iter().zip(f.velocities()) {
                    assert!(p.is_finite() && v.is_finite() && p.x > 0.0);
                    bits.extend(p.to_array().map(f32::to_bits));
                    bits.extend(v.to_array().map(f32::to_bits));
                }
                states.push(bits);
            }
            assert_eq!(states[0], states[1]);
        }
    }

    #[test]
    #[ignore = "fixed full-step moving sphere performance baseline"]
    fn moving_scan_baseline() {
        for round in 0..3 {
            for separation in [false, true] {
                let mut reference = None;
                for count in [1, 128, 1024] {
                    let mut world = moving_scan_fixture(count, separation);
                    MOVING_SCAN_WORK.with(|n| n.set([0; 4]));
                    SEPARATION_PUSHES.with(|n| n.set(0));
                    let start = std::time::Instant::now();
                    world.step(1.0 / 60.0);
                    let ms = start.elapsed().as_secs_f64() * 1000.0;
                    let work = MOVING_SCAN_WORK.with(|n| n.get());
                    let pushes = SEPARATION_PUSHES.with(|n| n.get());
                    assert!(work[3] > 0, "fixture must accept moving hits");
                    if separation {
                        assert!(pushes > 0);
                    }
                    let f = &world.fluids()[0];
                    let mut bits = Vec::new();
                    for (p, v) in f.particles().iter().zip(f.velocities()) {
                        assert!(p.is_finite() && v.is_finite());
                        assert!(p.x > 0.0, "real carry required: {p:?}");
                        bits.extend(p.to_array().map(f32::to_bits));
                        bits.extend(v.to_array().map(f32::to_bits));
                    }
                    if let Some(expected) = &reference {
                        assert_eq!(&bits, expected);
                    } else {
                        reference = Some(bits);
                    }
                    eprintln!("MOVING_BASE round={round} bodies={count} separation={separation} ms={ms:.6} work={work:?} pushes={pushes}");
                }
            }
        }
    }

    #[test]
    fn moving_sphere_contact_continuity_audit() {
        // 24 fixed trajectories, eight public steps each. Stop/reverse use the
        // public setter; compare subsequent motion to its actual pre-change velocity.
        for sign in [-1.0f32, 1.0] {
            for extra in [false, true] {
                for shell in [false, true] {
                    for phase in 0..3 {
                        let offset = if extra { Vec3::Y } else { Vec3::ZERO };
                        let builder = if extra {
                            BodyBuilder::kinematic(Shape::Box {
                                half_extents: Vec3::splat(0.02),
                            })
                            .compound_transform_mode(crate::body::CompoundTransformMode::BodyLocal)
                            .part_at(Shape::Sphere { radius: 0.1 }, -offset)
                        } else {
                            BodyBuilder::kinematic(Shape::Sphere { radius: 0.1 })
                        };
                        let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                        let handle = world.add_builder(
                            builder
                                .position(
                                    offset - Vec3::X * sign * if shell { 0.105 } else { 0.15 },
                                )
                                .linear_velocity(Vec3::X * (sign * 5.0)),
                        );
                        world
                            .add_fluid(FluidDesc {
                                initial_positions: Some(vec![Vec3::ZERO]),
                                radius: 0.01,
                                spacing: 0.1,
                                viscosity: 0.0,
                                collision_mode: FluidCollisionMode::SweptMeshes,
                                boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                                boundary_motion_mode:
                                    FluidBoundaryMotionMode::TranslatingKinematicSpheres,
                                ..FluidDesc::default()
                            })
                            .unwrap();
                        let mut release_velocity = Vec3::ZERO;
                        for step in 0..8 {
                            if step == 4 {
                                release_velocity = world.fluids()[0].velocities()[0];
                                let speed = match phase {
                                    0 => 5.0,
                                    1 => 0.0,
                                    _ => -5.0,
                                };
                                world
                                    .body_mut(handle)
                                    .unwrap()
                                    .set_linear_velocity(Vec3::X * (sign * speed));
                            }
                            let body = world.body(handle).unwrap();
                            let before = body.transform;
                            let boundary_velocity = body.linear_velocity;
                            let previous_particle = world.fluids()[0].particles()[0];
                            assert_eq!(body.angular_velocity, Vec3::ZERO);
                            world.step(1.0 / 60.0);
                            let body = world.body(handle).unwrap();
                            assert_eq!(body.prev_transform, before);
                            assert_eq!(body.transform.rotation, before.rotation);
                            assert!(
                                (body.position() - before.position - boundary_velocity / 60.0)
                                    .length()
                                    < 1e-6
                            );
                            let center = body.position() - offset;
                            let f = &world.fluids()[0];
                            let p = f.particles()[0];
                            let v = f.velocities()[0];
                            assert!(p.is_finite() && v.is_finite());
                            assert!(v.length() <= MAX_SPEED + 1e-5);
                            assert!((p.x - center.x) * sign >= 0.11 - 2e-6,
                                "gap: sign={sign} extra={extra} shell={shell} phase={phase} step={step} p={p:?} center={center:?}");
                            assert!(v.x * sign > 4.99, "contact must impart forward motion");
                            if step >= 4 && phase != 0 {
                                assert!(
                                    (v - release_velocity).length() < 2e-5,
                                    "stop/reverse must not pull departing liquid back"
                                );
                                assert!(
                                    (p - previous_particle - release_velocity / 60.0).length()
                                        < 2e-6
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn moving_sphere_clearance_shell_entry() {
        let mut failures = Vec::new();
        for sign in [-1.0f32, 1.0] {
            for extra in [false, true] {
                for motion in 0..3 {
                    let center = Vec3::X * (-0.105 * sign);
                    let velocity = match motion {
                        0 => Vec3::X * (21.3 * sign),
                        1 => Vec3::X * (-21.3 * sign),
                        _ => Vec3::Y * 21.3,
                    };
                    let mut records = Vec::new();
                    for sensor in [false, true] {
                        let offset = if extra { Vec3::Y } else { Vec3::ZERO };
                        let builder = if extra {
                            BodyBuilder::kinematic(Shape::Box {
                                half_extents: Vec3::splat(0.02),
                            })
                            .compound_transform_mode(crate::body::CompoundTransformMode::BodyLocal)
                            .part_at(Shape::Sphere { radius: 0.1 }, -offset)
                        } else {
                            BodyBuilder::kinematic(Shape::Sphere { radius: 0.1 })
                        };
                        let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                        let handle = world.add_builder(
                            builder
                                .position(center + offset)
                                .linear_velocity(velocity)
                                .sensor(sensor),
                        );
                        world
                            .add_fluid(FluidDesc {
                                initial_positions: Some(vec![Vec3::ZERO]),
                                radius: 0.01,
                                spacing: 0.1,
                                viscosity: 0.0,
                                collision_mode: FluidCollisionMode::SweptMeshes,
                                boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                                boundary_motion_mode:
                                    FluidBoundaryMotionMode::TranslatingKinematicSpheres,
                                ..FluidDesc::default()
                            })
                            .unwrap();
                        MOVING_SHELL_CARRY.with(|value| value.set(None));
                        let initial = world.body(handle).unwrap().transform;
                        assert!(center.length() > 0.1 && center.length() < 0.11);
                        assert_eq!(world.body(handle).unwrap().linear_velocity, velocity);
                        assert_eq!(world.body(handle).unwrap().angular_velocity, Vec3::ZERO);
                        world.step(1.0 / 60.0);
                        let body = world.body(handle).unwrap();
                        assert_eq!(body.prev_transform, initial);
                        assert!(
                            (body.position() - (initial.position + velocity / 60.0)).length()
                                < 1e-6
                        );
                        if !sensor && motion == 0 {
                            let (actual, expected) =
                                MOVING_SHELL_CARRY.with(|value| value.get()).unwrap();
                            assert_eq!(actual, expected, "carry must not add a radial push");
                            assert!((actual.x.abs() - 0.355).abs() < 1e-6);
                        } else {
                            assert!(MOVING_SHELL_CARRY.with(|value| value.get()).is_none());
                        }
                        let f = &world.fluids()[0];
                        records.push((f.particles()[0], f.velocities()[0]));
                    }
                    assert_eq!(records[1], (Vec3::ZERO, Vec3::ZERO));
                    if motion == 0 {
                        let (p, v) = records[0];
                        if p.x * sign < 0.354 || v.x * sign < 21.29 {
                            failures.push((sign, extra, p, v));
                        }
                    } else {
                        assert_eq!(
                            records[0], records[1],
                            "motion={motion} sign={sign} extra={extra}"
                        );
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "clearance-shell inward motion missed: {failures:?}"
        );
    }

    #[test]
    fn moving_shell_radius_exclusions_and_static_wall() {
        for (radius, distance, catches) in [
            (0.1f32, 0.105, true),
            (-0.1, 0.105, true),
            (0.0, 0.005, true),
            (0.1, 0.1, false),
            (0.1, 0.05, false),
            (0.1, 0.0, false),
        ] {
            for wall in [false, true] {
                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                world.add_builder(
                    BodyBuilder::kinematic(Shape::Sphere { radius })
                        .position(Vec3::X * -distance)
                        .linear_velocity(Vec3::X * 21.3),
                );
                if wall {
                    world.add_builder(
                        BodyBuilder::r#static(Shape::Box {
                            half_extents: vec3(0.005, 1.0, 1.0),
                        })
                        .position(Vec3::X * 0.15),
                    );
                }
                world
                    .add_fluid(FluidDesc {
                        initial_positions: Some(vec![Vec3::ZERO]),
                        radius: 0.01,
                        spacing: 0.1,
                        viscosity: 0.0,
                        collision_mode: FluidCollisionMode::SweptMeshes,
                        boundary_motion_mode: FluidBoundaryMotionMode::TranslatingKinematicSpheres,
                        ..FluidDesc::default()
                    })
                    .unwrap();
                world.step(1.0 / 60.0);
                let p = world.fluids()[0].particles()[0];
                if catches {
                    if wall {
                        assert!((p.x - 0.135).abs() < 1e-6, "radius={radius} p={p:?}");
                    } else {
                        assert!(p.x >= 0.354, "radius={radius} p={p:?}");
                    }
                } else {
                    assert_eq!(p, Vec3::ZERO, "distance={distance} wall={wall}");
                }
            }
        }
    }

    #[test]
    fn swept_mesh_contact_departure_fixed_audit() {
        // 24 fixed cases. Existing projection retains 80% tangential velocity;
        // compare modes rather than incorrectly requiring frictionless motion.
        let mut differences = Vec::new();
        for kind in 0..3 {
            for shell in [false, true] {
                for tangent in [false, true] {
                    for sign in [-1.0f32, 1.0] {
                        let surface = if kind == 2 { 0.0 } else { 0.125 };
                        let gap = if shell { 0.015625 } else { 0.03125 };
                        let start = vec3(sign * (surface + gap), 0.0, 0.0);
                        let velocity = if tangent {
                            Vec3::Y * 0.75
                        } else {
                            Vec3::X * sign * 0.75
                        };
                        let shape = match kind {
                            0 => Shape::Sphere { radius: 0.125 },
                            1 => Shape::Box {
                                half_extents: Vec3::splat(0.125),
                            },
                            _ => Shape::TriMesh {
                                vertices: vec![
                                    vec3(0.0, -1.0, -1.0),
                                    vec3(0.0, 1.0, -1.0),
                                    vec3(0.0, 0.0, 1.0),
                                ],
                                indices: vec![0, 1, 2],
                            },
                        };
                        let mut records = Vec::new();
                        for mode in [FluidCollisionMode::Legacy, FluidCollisionMode::SweptMeshes] {
                            for sensor in [false, true] {
                                let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                                world.add_builder(
                                    BodyBuilder::r#static(shape.clone()).sensor(sensor),
                                );
                                world
                                    .add_fluid(FluidDesc {
                                        initial_positions: Some(vec![start]),
                                        initial_velocity: velocity,
                                        radius: 0.03125,
                                        spacing: 0.1,
                                        viscosity: 0.0,
                                        collision_mode: mode,
                                        ..FluidDesc::default()
                                    })
                                    .unwrap();
                                assert_eq!(world.fluids()[0].velocities()[0], velocity);
                                let mut bits = Vec::new();
                                for _ in 0..4 {
                                    world.step(1.0 / 60.0);
                                    let f = &world.fluids()[0];
                                    bits.extend(f.particles()[0].to_array().map(f32::to_bits));
                                    bits.extend(f.velocities()[0].to_array().map(f32::to_bits));
                                }
                                assert_ne!(world.fluids()[0].particles()[0], start);
                                records.push(bits);
                            }
                        }
                        if records[0] != records[2] || records[1] != records[3] {
                            differences.push((kind, shell, tangent, sign));
                        }
                    }
                }
            }
        }
        assert!(
            differences.is_empty(),
            "departure audit differences: {differences:?}"
        );
    }

    #[test]
    fn swept_mesh_departure_can_hit_another_triangle() {
        let tri = [
            vec3(0.0, -1.0, -1.0),
            vec3(0.0, 1.0, -1.0),
            vec3(0.0, 0.0, 1.0),
        ];
        let start = Vec3::X * 0.015625;
        let end = start + Vec3::X * 0.5;
        assert!(sweep_triangle(start, end, tri, 0.03125).is_none());
        let second = tri.map(|p| p + Vec3::X * 0.25);
        let hit = sweep_triangle(start, end, second, 0.03125).unwrap();
        assert_eq!(hit.0, (0.25 - 0.03125 - 0.015625) / 0.5);
        for reverse in [false, true] {
            let mut triangles = [tri, second];
            if reverse {
                triangles.reverse();
            }
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            world.add_builder(BodyBuilder::r#static(Shape::TriMesh {
                vertices: triangles.into_iter().flatten().collect(),
                indices: vec![0, 1, 2, 3, 4, 5],
            }));
            world
                .add_fluid(FluidDesc {
                    initial_positions: Some(vec![start]),
                    initial_velocity: Vec3::X * 30.0,
                    radius: 0.03125,
                    spacing: 0.1,
                    viscosity: 0.0,
                    collision_mode: FluidCollisionMode::SweptMeshes,
                    ..FluidDesc::default()
                })
                .unwrap();
            world.step(1.0 / 60.0);
            let p = world.fluids()[0].particles()[0];
            assert!((p.x - 0.21875).abs() < 1e-6, "reverse={reverse} p={p:?}");
        }
    }

    #[test]
    fn triangle_sweep_features_and_degenerate_geometry() {
        let tri = [
            vec3(0.0, -0.5, -0.5),
            vec3(0.0, 0.5, -0.5),
            vec3(0.0, 0.5, 0.5),
        ];
        for points in [
            tri,
            [tri[2], tri[1], tri[0]],
            [-Vec3::Y, -Vec3::Y, Vec3::Y],
            [Vec3::ZERO; 3],
        ] {
            for direction in [-1.0, 1.0] {
                let (t, _, n) = sweep_triangle(
                    -Vec3::X * 0.25 * direction,
                    Vec3::X * 0.25 * direction,
                    points,
                    0.01,
                )
                .unwrap();
                assert!((t - (0.25 - f64::from(0.01f32)) / 0.5).abs() < 1e-12);
                assert!((n + Vec3::X * direction).length() < 1e-6);
            }
        }
        // Edge and vertex have a radial (not plane-only) entry.
        for (y, z) in [(0.506f32, 0.0f32), (0.504f32, 0.504f32)] {
            let (t, _, _) = sweep_triangle(vec3(-0.25, y, z), vec3(0.25, y, z), tri, 0.01).unwrap();
            let dy = f64::from(y) - 0.5;
            let dz = if z > 0.5 { f64::from(z) - 0.5 } else { 0.0 };
            let expected = (0.25 - (f64::from(0.01f32).powi(2) - dy * dy - dz * dz).sqrt()) / 0.5;
            assert!((t - expected).abs() < 1e-11, "{t} {expected}");
        }
        let point = [Vec3::ZERO; 3];
        assert!(sweep_triangle(
            vec3(-0.25, 0.03125, 0.0),
            vec3(0.25, 0.03125, 0.0),
            point,
            0.03125
        )
        .is_none());
        assert!(sweep_triangle(-Vec3::X * 0.005, -Vec3::X * 0.25, point, 0.01).is_none());
        assert_eq!(
            sweep_triangle(-Vec3::X * 0.005, Vec3::X * 0.25, point, 0.01)
                .unwrap()
                .0,
            0.0
        );
        assert!(sweep_triangle(Vec3::ZERO, Vec3::X, point, 0.01).is_none());
    }

    #[test]
    fn mesh_sweeps_cover_compounds_density_and_separation() {
        use crate::body::{CompoundTransformMode::BodyLocal, ShapePart};
        // A finite line/point can be legitimately bypassed by lateral separation.
        // Use a broad face for the multi-particle no-crossing invariant.
        for kind in 0..1 {
            for separation in [false, true] {
                let mut records = Vec::new();
                for compound in [false, true] {
                    let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                    let pose = Transform {
                        position: vec3(0.3, 0.2, -0.1),
                        rotation: Quat::from_rotation_z(0.4),
                        scale: Vec3::ONE,
                    };
                    let local = Transform {
                        position: Vec3::Y * 2.0,
                        rotation: Quat::from_rotation_x(0.7),
                        scale: Vec3::ONE,
                    };
                    let target = pose.mul_transform(&local);
                    let vertices = match kind {
                        0 => vec![
                            vec3(0.0, -1.0, -1.0),
                            vec3(0.0, 1.0, -1.0),
                            vec3(0.0, 0.0, 1.0),
                        ],
                        1 => vec![-Vec3::Y, -Vec3::Y, Vec3::Y],
                        _ => vec![Vec3::ZERO; 3],
                    };
                    let shape = Shape::TriMesh {
                        vertices,
                        indices: vec![0, 1, 2],
                    };
                    let builder = if compound {
                        BodyBuilder::r#static(Shape::Sphere { radius: 0.01 })
                            .position(pose.position)
                            .rotation(pose.rotation)
                            .part(ShapePart::at(shape, local.position, local.rotation))
                            .compound_transform_mode(BodyLocal)
                    } else {
                        BodyBuilder::r#static(shape)
                            .position(target.position)
                            .rotation(target.rotation)
                    };
                    world.add_builder(builder);
                    let initial: Vec<_> = (0..12)
                        .map(|i| target.transform_point(vec3(-0.01 - i as f32 * 0.004, 0.0, 0.0)))
                        .collect();
                    world
                        .add_fluid(FluidDesc {
                            initial_positions: Some(initial),
                            radius: 0.003,
                            spacing: 0.1,
                            collision_mode: FluidCollisionMode::SweptMeshes,
                            particle_collision: separation,
                            ..FluidDesc::default()
                        })
                        .unwrap();
                    let mut trajectory = Vec::new();
                    for _ in 0..8 {
                        world.step(1.0 / 60.0);
                        for p in world.fluids()[0].particles() {
                            let q = target.inverse_transform_point(*p);
                            assert!(
                                q.x <= -0.003 + 1e-5,
                                "kind={kind} separation={separation} compound={compound} q={q:?}"
                            );
                            trajectory.push(p.to_array().map(f32::to_bits));
                        }
                    }
                    records.push(trajectory);
                }
                assert_eq!(records[0], records[1], "kind={kind} sep={separation}");
            }
        }
    }

    #[test]
    fn mesh_sweep_degenerate_public_and_invalid_geometry() {
        for vertices in [
            vec![-Vec3::Y, -Vec3::Y, Vec3::Y],
            vec![Vec3::ZERO; 3],
            vec![],
        ] {
            for direction in [-1.0, 1.0] {
                for mode in [
                    FluidCollisionMode::Legacy,
                    FluidCollisionMode::SweptPrimitives,
                    FluidCollisionMode::SweptMeshes,
                ] {
                    let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                    world.add_builder(BodyBuilder::r#static(Shape::TriMesh {
                        vertices: vertices.clone(),
                        indices: vec![99, 1, 2, 0, 1, 2, 0],
                    }));
                    world
                        .add_fluid(FluidDesc {
                            initial_positions: Some(vec![-Vec3::X * 0.25 * direction]),
                            initial_velocity: Vec3::X * 30.0 * direction,
                            radius: 0.01,
                            spacing: 0.1,
                            collision_mode: mode,
                            ..FluidDesc::default()
                        })
                        .unwrap();
                    world.step(1.0 / 60.0);
                    let x = world.fluids()[0].particles()[0].x;
                    let expected =
                        if !vertices.is_empty() && mode == FluidCollisionMode::SweptMeshes {
                            -0.01 * direction
                        } else {
                            0.25 * direction
                        };
                    assert!((x - expected).abs() < 1e-6, "{mode:?} {vertices:?} {x}");
                }
            }
        }
    }

    #[test]
    fn finite_triangle_particle_segments_match_geometry() {
        let mut failures = Vec::new();
        for reversed in [false, true] {
            for y in [0.0f32, 0.1, 0.52, 0.7] {
                for direction in [-1.0f32, 1.0] {
                    let start = vec3(-0.25 * direction, y, 0.0);
                    let velocity = Vec3::X * (30.0 * direction);
                    let mut states = Vec::new();
                    for variant in 0..3 {
                        let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                        if variant != 0 {
                            world.add_builder(
                                BodyBuilder::r#static(Shape::TriMesh {
                                    vertices: vec![
                                        vec3(0.0, -0.5, -0.5),
                                        vec3(0.0, 0.5, -0.5),
                                        vec3(0.0, 0.5, 0.5),
                                    ],
                                    indices: if reversed {
                                        vec![2, 1, 0]
                                    } else {
                                        vec![0, 1, 2]
                                    },
                                })
                                .sensor(variant == 1),
                            );
                        }
                        world
                            .add_fluid(FluidDesc {
                                collision_mode: FluidCollisionMode::SweptMeshes,
                                initial_positions: Some(vec![start]),
                                initial_velocity: velocity,
                                radius: 0.01,
                                spacing: 0.1,
                                ..FluidDesc::default()
                            })
                            .unwrap();
                        assert_eq!(world.fluids()[0].particles()[0], start);
                        assert_eq!(world.fluids()[0].velocities()[0], velocity);
                        assert!(velocity.length() < MAX_SPEED);
                        world.step(1.0 / 60.0);
                        states.push((
                            world.fluids()[0].particles()[0],
                            world.fluids()[0].velocities()[0],
                        ));
                    }
                    assert_eq!(states[0], states[1], "sensor control");
                    assert!((states[0].0.x - 0.25 * direction).abs() < 1e-6);
                    // y>.51 is outside the radius-expanded finite triangle; y=0
                    // lies on its diagonal edge and has the same analytic entry t=.48.
                    if y > 0.51 {
                        if states[0] != states[2] {
                            failures.push(format!(
                                "miss reversed={reversed} y={y} direction={direction} {states:?}"
                            ));
                        }
                    } else if (states[2].0.x + 0.01 * direction).abs() > 1e-5 {
                        failures.push(format!("cross reversed={reversed} y={y} direction={direction} expected t=.48 {states:?}"));
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "finite triangle sweep differences: {failures:#?}"
        );
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

    /// Public fixed-step audit of a finite, closed container, not an infinite plane.
    /// Each dt executes 120 actual steps (2 s at 60 Hz, 1 s at 120 Hz).
    #[test]
    fn closed_static_container_long_term_public_step_audit() {
        let mut failures = Vec::new();
        let mut worlds_run = 0;
        for mode in [FluidCollisionMode::Legacy, FluidCollisionMode::SweptMeshes] {
            for particle_collision in [false, true] {
                for dt in [1.0f32 / 60.0, 1.0f32 / 120.0] {
                    let label = format!("mode={mode:?} pairs={particle_collision} dt={dt:.9}");
                    let mut world = PhysicsWorld::with_gravity(vec3(0.0, -9.81, 0.0));
                    world.set_fixed_dt(dt);
                    // Inner faces: X/Z +/- .5, Y 0/1. All walls are .1 thick.
                    // Full outer spans overlap at edges/corners, leaving no legal bypass.
                    for (position, half_extents) in [
                        (vec3(0.0, -0.05, 0.0), vec3(0.6, 0.05, 0.6)),
                        (vec3(0.0, 1.05, 0.0), vec3(0.6, 0.05, 0.6)),
                        (vec3(-0.55, 0.5, 0.0), vec3(0.05, 0.6, 0.6)),
                        (vec3(0.55, 0.5, 0.0), vec3(0.05, 0.6, 0.6)),
                        (vec3(0.0, 0.5, -0.55), vec3(0.6, 0.6, 0.05)),
                        (vec3(0.0, 0.5, 0.55), vec3(0.6, 0.6, 0.05)),
                    ] {
                        world.add_builder(
                            BodyBuilder::r#static(Shape::Box { half_extents }).position(position),
                        );
                    }
                    let desc = FluidDesc {
                        center: vec3(0.0, 0.5, 0.0),
                        half_extents: Vec3::splat(0.2),
                        spacing: 0.1,
                        radius: 0.04,
                        max_particles: 2000,
                        initial_velocity: vec3(0.15, 0.0, -0.1),
                        collision_mode: mode,
                        particle_collision,
                        ..FluidDesc::default()
                    };
                    let radius = f64::from(desc.radius);
                    assert_eq!(desc.boundary_motion_mode, FluidBoundaryMotionMode::Legacy);
                    assert_eq!(desc.estimated_particles(), 125);
                    world
                        .add_fluid(desc)
                        .expect("valid closed-container fixture");
                    let mut density_entries = 0;
                    let mut density_nonzero = 0;
                    let mut boundary_contacts = 0;
                    let mut min_clearance = f64::INFINITY;
                    let mut min_pair = f64::INFINITY;
                    let mut max_speed = 0.0f64;
                    let mut first_bad = None;
                    SEPARATION_PUSHES.with(|n| n.set(0));
                    for step in 1..=120 {
                        MESH_DENSITY_TRACE.with(|trace| *trace.borrow_mut() = Some(Vec::new()));
                        world.step(dt);
                        let trace =
                            MESH_DENSITY_TRACE.with(|trace| trace.borrow_mut().take().unwrap());
                        density_entries += trace.len();
                        density_nonzero += trace.iter().filter(|(_, a, b, _)| a != b).count();
                        let fluid = &world.fluids()[0];
                        boundary_contacts += fluid.contacts.iter().filter(|c| c.is_some()).count();
                        let mut step_bad = Vec::new();
                        if world.step_count != step || world.accumulator() != 0.0 {
                            step_bad.push(format!(
                                "actual_steps={} accumulator={}",
                                world.step_count,
                                world.accumulator()
                            ));
                        }
                        if fluid.particles().len() != 125 || fluid.velocities().len() != 125 {
                            step_bad.push(format!(
                                "particle_count={} velocity_count={}",
                                fluid.particles().len(),
                                fluid.velocities().len()
                            ));
                        }
                        for (i, (p, v)) in
                            fluid.particles().iter().zip(fluid.velocities()).enumerate()
                        {
                            if !p.is_finite() || !v.is_finite() {
                                step_bad.push(format!("nonfinite particle={i} p={p:?} v={v:?}"));
                                continue;
                            }
                            let p64 = [f64::from(p.x), f64::from(p.y), f64::from(p.z)];
                            // Independent analytic inner-face signed sphere clearance.
                            let clearance = [
                                p64[0] + 0.5 - radius,
                                0.5 - p64[0] - radius,
                                p64[1] - radius,
                                1.0 - p64[1] - radius,
                                p64[2] + 0.5 - radius,
                                0.5 - p64[2] - radius,
                            ]
                            .into_iter()
                            .fold(f64::INFINITY, f64::min);
                            min_clearance = min_clearance.min(clearance);
                            let speed = (f64::from(v.x).powi(2)
                                + f64::from(v.y).powi(2)
                                + f64::from(v.z).powi(2))
                            .sqrt();
                            max_speed = max_speed.max(speed);
                            if clearance < -1e-4 || speed > f64::from(MAX_SPEED) + 1e-4 {
                                step_bad.push(format!("particle={i} p={p:?} v={v:?} clearance={clearance:.9} speed={speed:.9}"));
                            }
                            for q in &fluid.particles()[i + 1..] {
                                let distance = ((p64[0] - f64::from(q.x)).powi(2)
                                    + (p64[1] - f64::from(q.y)).powi(2)
                                    + (p64[2] - f64::from(q.z)).powi(2))
                                .sqrt();
                                min_pair = min_pair.min(distance);
                            }
                        }
                        if first_bad.is_none() && !step_bad.is_empty() {
                            first_bad = Some(format!("step={step}: {}", step_bad.join("; ")));
                        }
                    }
                    let pushes = SEPARATION_PUSHES.with(|n| n.get());
                    if density_entries != 120 * 125 * 4
                        || density_nonzero == 0
                        || boundary_contacts == 0
                        || (particle_collision && pushes == 0)
                        || (!particle_collision && pushes != 0)
                    {
                        failures.push(format!("{label}: vacuous/incomplete work density_entries={density_entries} density_nonzero={density_nonzero} boundary_contacts={boundary_contacts} pair_pushes={pushes}"));
                    }
                    if let Some(bad) = first_bad {
                        failures.push(format!("{label}: {bad}"));
                    }
                    println!("closed_container {label} actual_steps={} duration_s={:.9} particles={} density_entries={density_entries} density_nonzero={density_nonzero} boundary_contacts={boundary_contacts} pair_pushes={pushes} min_clearance={min_clearance:.9} min_pair={min_pair:.9} max_stored_speed={max_speed:.9}", world.step_count, f64::from(dt) * world.step_count as f64, world.fluids()[0].particles().len());
                    worlds_run += 1;
                }
            }
        }
        assert_eq!(worlds_run, 8);
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn candidate_storage_matches_independent_full_scan() {
        let mut seed = 0x71828182u32;
        for n in [0usize, 1, 2, 6, 32, 33, 64, 2048] {
            for variant in 0..3 {
                let mut index = SweepBodyIndex {
                    ordered: Vec::new(),
                    fallback: Vec::new(),
                    scale: 1.0,
                };
                for slot in 0..n {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    let x = (seed % 100) as f32 * 0.02 - 1.0;
                    let id = if variant == 2 { slot / 2 } else { slot };
                    if variant == 1 || slot % 5 == 0 {
                        index.fallback.push(id);
                    } else {
                        index.ordered.push((
                            id,
                            Aabb {
                                min: vec3(x, -1.0, -1.0),
                                max: vec3(x + 0.2, 1.0, 1.0),
                            },
                        ));
                    }
                }
                index
                    .ordered
                    .sort_by(|a, b| a.1.min.x.total_cmp(&b.1.min.x).then(a.0.cmp(&b.0)));
                for (start, end, radius) in [
                    (Vec3::ZERO, Vec3::X, 0.04),
                    (Vec3::splat(-3.0), Vec3::splat(3.0), 0.04),
                    (Vec3::splat(99.0), Vec3::splat(100.0), 0.04),
                    (Vec3::splat(f32::NAN), Vec3::X, 0.04),
                    (Vec3::ZERO, Vec3::splat(f32::INFINITY), 0.04),
                    (Vec3::ZERO, Vec3::X, -1.0),
                    (Vec3::ZERO, Vec3::X, f32::INFINITY),
                ] {
                    // Independent full scan: no interval-prefix search and no Aabb::intersects.
                    let expected = if !start.is_finite() || !end.is_finite() {
                        None
                    } else {
                        let scale = index.scale.max(start.abs().max(end.abs()).max_element());
                        let margin = radius + 64.0 * f32::EPSILON * scale;
                        let lo = start.min(end) - Vec3::splat(margin);
                        let hi = start.max(end) + Vec3::splat(margin);
                        if !margin.is_finite()
                            || margin < 0.0
                            || !lo.is_finite()
                            || !hi.is_finite()
                            || !(0..3).all(|axis| lo[axis] <= hi[axis])
                        {
                            None
                        } else {
                            let mut result = index.fallback.clone();
                            for &(id, bounds) in &index.ordered {
                                if (0..3).all(|axis| {
                                    bounds.min[axis] <= hi[axis] && bounds.max[axis] >= lo[axis]
                                }) {
                                    result.push(id);
                                }
                            }
                            result.sort_unstable();
                            Some(result)
                        }
                    };
                    CANDIDATE_HEAP_SITES.with(|site| site.set(Some(0)));
                    let actual = index.candidates(start, end, radius);
                    let sites = CANDIDATE_HEAP_SITES.with(|site| site.take().unwrap());
                    assert_eq!(
                        actual.as_deref(),
                        expected.as_deref(),
                        "n={n} variant={variant}"
                    );
                    assert_eq!(sites, usize::from(expected.is_some()));
                }
            }
        }
    }

    #[test]
    #[ignore = "unmet: inline candidate storage experiment rejected by preregistered performance threshold"]
    fn candidate_small_storage_has_no_heap_fallback() {
        let bounds = Aabb {
            min: Vec3::splat(-1.0),
            max: Vec3::splat(1.0),
        };
        let index = SweepBodyIndex {
            ordered: (0..6).map(|i| (i, bounds)).collect(),
            fallback: Vec::new(),
            scale: 1.0,
        };
        CANDIDATE_HEAP_SITES.with(|n| n.set(Some(0)));
        let slots = index.candidates(Vec3::ZERO, Vec3::X, 0.04).unwrap();
        let sites = CANDIDATE_HEAP_SITES.with(|n| n.take().unwrap());
        assert_eq!(slots.as_slice(), &[0, 1, 2, 3, 4, 5]);
        assert_eq!(
            sites, 0,
            "candidate Vec-backed storage/heap fallback site proxy, not allocator count"
        );
    }

    #[test]
    #[ignore = "fixed independent work-count replay; production timing uses separation_cost example"]
    fn separation_cost_work_replay() {
        let mut worlds = 0;
        for side in [5usize, 10] {
            for geometry in 0..3 {
                let mut controls = Vec::new();
                for mode in [
                    FluidSeparationMode::Legacy,
                    FluidSeparationMode::BoundaryConstrained,
                ] {
                    let mut world = PhysicsWorld::with_gravity(if geometry == 0 {
                        Vec3::ZERO
                    } else {
                        vec3(0.0, -9.81, 0.0)
                    });
                    world.set_fixed_dt(1.0 / 60.0);
                    let half = side as f32 * 0.05 + 0.25;
                    let height = 2.0 * half;
                    if geometry != 0 {
                        let mut walls =
                            vec![(vec3(0.0, -0.05, 0.0), vec3(half + 0.1, 0.05, half + 0.1))];
                        if geometry == 2 {
                            walls.extend([
                                (
                                    vec3(0.0, height + 0.05, 0.0),
                                    vec3(half + 0.1, 0.05, half + 0.1),
                                ),
                                (
                                    vec3(-half - 0.05, height * 0.5, 0.0),
                                    vec3(0.05, height * 0.5 + 0.1, half + 0.1),
                                ),
                                (
                                    vec3(half + 0.05, height * 0.5, 0.0),
                                    vec3(0.05, height * 0.5 + 0.1, half + 0.1),
                                ),
                                (
                                    vec3(0.0, height * 0.5, -half - 0.05),
                                    vec3(half + 0.1, height * 0.5 + 0.1, 0.05),
                                ),
                                (
                                    vec3(0.0, height * 0.5, half + 0.05),
                                    vec3(half + 0.1, height * 0.5 + 0.1, 0.05),
                                ),
                            ]);
                        }
                        for (p, h) in walls {
                            world.add_builder(
                                BodyBuilder::r#static(Shape::Box { half_extents: h }).position(p),
                            );
                        }
                    }
                    let mut positions = Vec::new();
                    for z in 0..side {
                        for y in 0..side {
                            for x in 0..side {
                                positions.push(vec3(
                                    (x as f32 - (side - 1) as f32 * 0.5) * 0.1,
                                    0.04 + y as f32 * 0.1,
                                    (z as f32 - (side - 1) as f32 * 0.5) * 0.1,
                                ));
                            }
                        }
                    }
                    world
                        .add_fluid(FluidDesc {
                            initial_positions: Some(positions),
                            radius: 0.04,
                            spacing: 0.1,
                            max_particles: side.pow(3),
                            initial_velocity: if geometry == 2 {
                                vec3(0.15, 0.0, -0.1)
                            } else {
                                Vec3::ZERO
                            },
                            particle_collision: true,
                            collision_mode: FluidCollisionMode::SweptMeshes,
                            separation_mode: mode,
                            ..FluidDesc::default()
                        })
                        .unwrap();
                    for _ in 0..8 {
                        world.step(1.0 / 60.0);
                    }
                    SEPARATION_COST.with(|c| c.set(Some([0; 4])));
                    SEPARATION_REDUNDANCY.with(|c| c.set(Some([0; 6])));
                    let mut bits = Vec::new();
                    for _ in 0..32 {
                        world.step(1.0 / 60.0);
                        let f = &world.fluids()[0];
                        assert_eq!(f.particles().len(), side.pow(3));
                        assert!(f
                            .particles()
                            .iter()
                            .chain(f.velocities())
                            .all(|p| p.is_finite()));
                        for p in f.particles().iter().chain(f.velocities()) {
                            bits.extend(p.to_array().map(f32::to_bits));
                        }
                    }
                    let counts = SEPARATION_COST.with(|c| c.take().unwrap());
                    let redundancy = SEPARATION_REDUNDANCY.with(|c| c.take().unwrap());
                    if mode == FluidSeparationMode::Legacy {
                        assert_eq!(redundancy[0], 0);
                    } else {
                        assert_eq!(redundancy[0], counts[1]);
                    }
                    assert!(redundancy[1] <= redundancy[0]);
                    assert!(redundancy[2] <= redundancy[0]);
                    println!("ROUND170_REDUNDANCY particles={} geometry={geometry} mode={mode:?} helper_calls={} zero_actual_initial={} zero_tangent_guard={} zero_actual_tangent={} all_static_calls={} enumerated_slots={}",side.pow(3),redundancy[0],redundancy[1],redundancy[2],redundancy[3],redundancy[4],redundancy[5]);
                    let f = &world.fluids()[0];
                    let checksum = f
                        .particles()
                        .iter()
                        .flat_map(|p| p.to_array())
                        .fold(0u64, |a, v| {
                            a.wrapping_mul(31).wrapping_add(u64::from(v.to_bits()))
                        });
                    println!("ROUND169_WORK particles={} geometry={geometry} mode={mode:?} measured_steps=32 pairs={} initial_sweeps={} slide_sweeps={} residual_pairs={} checksum={checksum}",side.pow(3),counts[0],counts[1],counts[2],counts[3]);
                    controls.push(bits);
                    worlds += 1;
                }
                if geometry == 0 {
                    assert_eq!(
                        controls[0], controls[1],
                        "free-space trajectory exact control"
                    );
                }
            }
        }
        assert_eq!(worlds, 12);
    }

    #[test]
    fn boundary_constrained_mesh_correction_audit() {
        // Independent f64 point/triangle distance: projected barycentrics plus
        // three clamped edge distances, with no production geometry calls.
        fn distance(p: Vec3, tri: [Vec3; 3]) -> f64 {
            let v = |p: Vec3| [f64::from(p.x), f64::from(p.y), f64::from(p.z)];
            let sub = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
            let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
            let [a, b, c] = tri.map(v);
            let p = v(p);
            let ab = sub(b, a);
            let ac = sub(c, a);
            let ap = sub(p, a);
            let aa = dot(ab, ab);
            let bb = dot(ab, ac);
            let cc = dot(ac, ac);
            let denom = aa * cc - bb * bb;
            let u = (cc * dot(ap, ab) - bb * dot(ap, ac)) / denom;
            let w = (aa * dot(ap, ac) - bb * dot(ap, ab)) / denom;
            let mut best = f64::INFINITY;
            if u >= 0.0 && w >= 0.0 && u + w <= 1.0 {
                let q = [
                    a[0] + u * ab[0] + w * ac[0],
                    a[1] + u * ab[1] + w * ac[1],
                    a[2] + u * ab[2] + w * ac[2],
                ];
                best = dot(sub(p, q), sub(p, q));
            }
            for (a, b) in [(a, b), (b, c), (c, a)] {
                let edge = sub(b, a);
                let t = (dot(sub(p, a), edge) / dot(edge, edge)).clamp(0.0, 1.0);
                let d = [
                    p[0] - a[0] - t * edge[0],
                    p[1] - a[1] - t * edge[1],
                    p[2] - a[2] - t * edge[2],
                ];
                best = best.min(dot(d, d));
            }
            best.sqrt()
        }
        let floor = [
            vec3(-4.0, 0.0, -4.0),
            vec3(4.0, 0.0, -4.0),
            vec3(0.0, 0.0, 4.0),
        ];
        let mut worlds = 0;
        let mut total = [0usize; 3];
        for dense in [false, true] {
            for sign in [-1.0f32, 1.0] {
                for moving in [false, true] {
                    if dense && !moving {
                        continue;
                    }
                    for reverse in [false, true] {
                        if dense && reverse {
                            continue;
                        }
                        let mut first_distances = Vec::new();
                        let mut mode_states = Vec::new();
                        for separation_mode in [
                            FluidSeparationMode::Legacy,
                            FluidSeparationMode::BoundaryConstrained,
                        ] {
                            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                            let wall = [
                                vec3(-4.0, -4.0, 0.0),
                                vec3(4.0, -4.0, 0.0),
                                vec3(0.0, 4.0, 0.0),
                            ];
                            let mesh = |vertices: [Vec3; 3]| Shape::TriMesh {
                                vertices: vertices.to_vec(),
                                indices: if reverse {
                                    vec![2, 1, 0]
                                } else {
                                    vec![0, 1, 2]
                                },
                            };
                            let floor_builder = BodyBuilder::r#static(mesh(floor));
                            if !reverse {
                                world.add_builder(BodyBuilder::r#static(mesh(floor)));
                            }
                            // Starts .0005 outside contact, moves .001 inward: real temporal impact.
                            let wall_start = sign * if dense { 0.0075 } else { 0.5005 };
                            let builder = if moving {
                                BodyBuilder::kinematic(mesh(wall)).kinematic_trimesh(true)
                            } else {
                                BodyBuilder::r#static(mesh(wall))
                            };
                            let handle = world.add_builder(
                                builder
                                    .position(Vec3::Z * wall_start)
                                    .linear_velocity(if moving {
                                        Vec3::Z * (-sign * 0.06)
                                    } else {
                                        Vec3::ZERO
                                    }),
                            );
                            if reverse {
                                world.add_builder(floor_builder);
                            }
                            let radius = if dense { 0.003 } else { 0.04 };
                            let positions = if dense {
                                (0..12)
                                    .map(|i| {
                                        vec3(
                                            (i % 3) as f32 * 0.0015,
                                            0.003 + (i / 3) as f32 * 0.0015,
                                            sign * 0.004,
                                        )
                                    })
                                    .collect()
                            } else {
                                vec![
                                    vec3(0.056195434, 0.04, sign * 0.45950133),
                                    vec3(0.077126235, 0.057065815, sign * 0.46),
                                ]
                            };
                            let mut positions: Vec<Vec3> = positions;
                            if reverse {
                                positions.reverse();
                            }
                            world
                                .add_fluid(FluidDesc {
                                    initial_positions: Some(positions),
                                    radius,
                                    spacing: 0.1,
                                    max_particles: 12,
                                    particle_collision: true,
                                    separation_mode,
                                    collision_mode: FluidCollisionMode::SweptMeshes,
                                    boundary_motion_mode: if moving {
                                        FluidBoundaryMotionMode::TranslatingKinematicMeshes
                                    } else {
                                        FluidBoundaryMotionMode::Legacy
                                    },
                                    boundary_velocity_mode: FluidBoundaryVelocityMode::BodyMotion,
                                    viscosity: 0.0,
                                    ..FluidDesc::default()
                                })
                                .unwrap();
                            let mut counts = [0usize; 3];
                            let mut minimum = f64::INFINITY;
                            let mut states = Vec::new();
                            for step in 0..8 {
                                if moving {
                                    let velocity = if step < 3 {
                                        -sign * 0.06
                                    } else if step < 5 {
                                        0.0
                                    } else {
                                        sign * 0.06
                                    };
                                    world
                                        .body_mut(handle)
                                        .unwrap()
                                        .set_linear_velocity(Vec3::Z * velocity);
                                }
                                let before = world.body(handle).unwrap().position();
                                let velocity = world.body(handle).unwrap().linear_velocity;
                                SEPARATION_PUSHES.with(|n| n.set(0));
                                MOVING_SCAN_WORK.with(|n| n.set([0; 4]));
                                MESH_DENSITY_TRACE.with(|t| *t.borrow_mut() = Some(Vec::new()));
                                MESH_CONTINUITY_TRACE.with(|t| *t.borrow_mut() = Some(Vec::new()));
                                SEPARATION_SEGMENT_TRACE
                                    .with(|t| *t.borrow_mut() = Some(Vec::new()));
                                world.step(1.0 / 60.0);
                                let density =
                                    MESH_DENSITY_TRACE.with(|t| t.borrow_mut().take().unwrap());
                                let trace =
                                    MESH_CONTINUITY_TRACE.with(|t| t.borrow_mut().take().unwrap());
                                let segments = SEPARATION_SEGMENT_TRACE
                                    .with(|t| t.borrow_mut().take().unwrap());
                                let impacts = MOVING_SCAN_WORK.with(|n| n.get()[3]);
                                counts[0] += impacts;
                                counts[1] += density.iter().filter(|(_, a, b, _)| a != b).count();
                                counts[2] += SEPARATION_PUSHES.with(|n| n.get());
                                let face = world.body(handle).unwrap().position().z;
                                assert!(
                                    (world.body(handle).unwrap().position()
                                        - before
                                        - velocity / 60.0)
                                        .length()
                                        < 1e-6
                                );
                                let triangles = [floor, wall.map(|p| p + Vec3::Z * face)];
                                let fluid = &world.fluids()[0];
                                assert_eq!(world.step_count, step + 1);
                                for (p, v) in fluid.particles().iter().zip(fluid.velocities()) {
                                    assert!(
                                        p.is_finite()
                                            && v.is_finite()
                                            && v.length() <= MAX_SPEED + 1e-4
                                    );
                                    assert!(p.y>=radius-2e-6 && (face-p.z)*sign>=radius-2e-6,"dense={dense} moving={moving} sign={sign} reverse={reverse} mode={separation_mode:?} step={step} p={p:?} face={face}");
                                    for triangle in triangles {
                                        assert!(
                                            distance(*p, triangle) >= f64::from(radius) - 2e-6,
                                            "triangle clearance p={p:?}"
                                        );
                                    }
                                }
                                if separation_mode == FluidSeparationMode::Legacy {
                                    assert!(segments.is_empty());
                                }
                                for (a, _, b) in segments {
                                    for p in [a, b] {
                                        assert!(
                                            p.is_finite()
                                                && p.y >= radius - 2e-6
                                                && (face - p.z) * sign >= radius - 2e-6,
                                            "per-sweep clearance step={step} p={p:?} face={face}"
                                        );
                                        for triangle in triangles {
                                            assert!(
                                                distance(p, triangle) >= f64::from(radius) - 2e-6
                                            );
                                        }
                                    }
                                }
                                // Every correction occurs at the current frozen wall pose.
                                // Inside finite faces by metres: no legitimate edge bypass.
                                for (i, a, _, b) in trace {
                                    assert!(
                                        a.x.abs() < 1.0
                                            && a.y < 1.0
                                            && a.z.abs() < 1.0
                                            && b.x.abs() < 1.0
                                            && b.y < 1.0
                                            && b.z.abs() < 1.0
                                    );
                                    assert!(
                                        (face - a.z) * sign >= -2e-6
                                            && (face - b.z) * sign >= -2e-6,
                                        "current wall correction crossed i={i} {a:?}->{b:?}"
                                    );
                                }
                                let mut state = Vec::new();
                                for p in fluid
                                    .positions
                                    .iter()
                                    .chain(&fluid.velocities)
                                    .chain(&fluid.predicted)
                                    .chain(&fluid.delta)
                                {
                                    state.extend(p.to_array().map(f32::to_bits));
                                }
                                state.extend(
                                    fluid
                                        .densities
                                        .iter()
                                        .chain(&fluid.lambdas)
                                        .map(|v| v.to_bits()),
                                );
                                states.push(state);
                                minimum = minimum.min(overlap_minimum(fluid.particles()).0);
                                if step == 0 {
                                    first_distances.push(overlap_minimum(fluid.particles()).0);
                                    if moving {
                                        assert!(impacts > 0, "real initial moving impact");
                                    }
                                }
                            }
                            assert!(counts[2] > 0, "actual pair correction required {counts:?}");
                            if dense {
                                assert!(
                                    counts.iter().all(|n| *n > 0),
                                    "density/impact/pair nonvacuity {counts:?}"
                                );
                            }
                            for (sum, n) in total.iter_mut().zip(counts) {
                                *sum += n;
                            }
                            println!("ROUND168_MESH dense={dense} moving={moving} sign={sign} reverse={reverse} mode={separation_mode:?} steps=8 minpair={minimum:.12} counts={counts:?}");
                            mode_states.push(states);
                            worlds += 1;
                        }
                        if dense {
                            assert_eq!(
                                mode_states[0], mode_states[1],
                                "normal-only dense mesh correction stays bitwise equal"
                            );
                        }
                        if !dense {
                            assert!(
                                first_distances[1] > first_distances[0] + 0.005,
                                "meshcorner progress {first_distances:?}"
                            );
                        }
                    }
                }
            }
        }
        assert_eq!(worlds, 20);
        println!(
            "ROUND168_TOTAL worlds={worlds} steps={} counts={total:?}",
            worlds * 8
        );
    }

    #[test]
    fn boundary_constrained_separation_corner_progress() {
        fn run(mode: FluidSeparationMode) -> Vec<Vec3> {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            for (position, half_extents) in [
                (vec3(0.0, -0.05, 0.0), vec3(0.6, 0.05, 0.6)),
                (vec3(0.0, 0.5, 0.55), vec3(0.6, 0.6, 0.05)),
            ] {
                world.add_builder(
                    BodyBuilder::r#static(Shape::Box { half_extents }).position(position),
                );
            }
            world
                .add_fluid(FluidDesc {
                    initial_positions: Some(vec![
                        vec3(0.056195434, 0.04, 0.45950133),
                        vec3(0.077126235, 0.057065815, 0.46),
                    ]),
                    radius: 0.04,
                    spacing: 0.1,
                    max_particles: 2,
                    particle_collision: true,
                    collision_mode: FluidCollisionMode::SweptMeshes,
                    separation_mode: mode,
                    ..FluidDesc::default()
                })
                .unwrap();
            world.step(1.0 / 60.0);
            world.fluids()[0].particles().to_vec()
        }
        let legacy = run(FluidSeparationMode::Legacy);
        let constrained = run(FluidSeparationMode::BoundaryConstrained);
        let before = overlap_distance(legacy[0], legacy[1]);
        let after = overlap_distance(constrained[0], constrained[1]);
        println!("ROUND166_CORNER legacy={before:.12} constrained={after:.12} legacy_positions={legacy:?} constrained_positions={constrained:?}");
        for p in &constrained {
            assert!(p.is_finite() && p.y >= 0.04 - 1e-6 && p.z <= 0.46 + 1e-6);
        }
        assert!(after > before + 0.005, "opt-in must make measurable safe progress at the attributed corner: {before} -> {after}");
    }

    #[test]
    fn boundary_constrained_separation_controls() {
        assert_eq!(
            FluidDesc::default().separation_mode,
            FluidSeparationMode::Legacy
        );
        for collision_mode in [
            FluidCollisionMode::Legacy,
            FluidCollisionMode::SweptSpheres,
            FluidCollisionMode::SweptPrimitives,
            FluidCollisionMode::SweptMeshes,
        ] {
            for particle_collision in [false, true] {
                let desc = FluidDesc {
                    separation_mode: FluidSeparationMode::BoundaryConstrained,
                    collision_mode,
                    particle_collision,
                    ..FluidDesc::default()
                };
                assert_eq!(
                    desc.is_valid(),
                    particle_collision
                        && matches!(
                            collision_mode,
                            FluidCollisionMode::SweptPrimitives | FluidCollisionMode::SweptMeshes
                        )
                );
                assert!(FluidDesc {
                    separation_mode: FluidSeparationMode::Legacy,
                    ..desc
                }
                .is_valid());
            }
        }
        for sign in [-1.0, 1.0] {
            for reverse in [false, true] {
                for collision_mode in [
                    FluidCollisionMode::SweptPrimitives,
                    FluidCollisionMode::SweptMeshes,
                ] {
                    let mut distances = Vec::new();
                    for separation_mode in [
                        FluidSeparationMode::Legacy,
                        FluidSeparationMode::BoundaryConstrained,
                    ] {
                        let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
                        let mut walls = vec![
                            (vec3(0.0, -0.05, 0.0), vec3(0.6, 0.05, 0.6)),
                            (vec3(0.0, 0.5, sign * 0.55), vec3(0.6, 0.6, 0.05)),
                        ];
                        let mut positions = vec![
                            vec3(0.056195434, 0.04, sign * 0.45950133),
                            vec3(0.077126235, 0.057065815, sign * 0.46),
                        ];
                        if reverse {
                            walls.reverse();
                            positions.reverse();
                        }
                        for (p, h) in walls {
                            world.add_builder(
                                BodyBuilder::r#static(Shape::Box { half_extents: h }).position(p),
                            );
                        }
                        world
                            .add_fluid(FluidDesc {
                                initial_positions: Some(positions),
                                radius: 0.04,
                                spacing: 0.1,
                                max_particles: 2,
                                particle_collision: true,
                                collision_mode,
                                separation_mode,
                                ..FluidDesc::default()
                            })
                            .unwrap();
                        world.step(1.0 / 60.0);
                        let p = world.fluids()[0].particles();
                        for p in p {
                            assert!(
                                p.is_finite() && p.y >= 0.04 - 1e-6 && sign * p.z <= 0.46 + 1e-6
                            );
                        }
                        distances.push(overlap_distance(p[0], p[1]));
                    }
                    assert!(distances[1] > distances[0]+0.005, "sign={sign} reverse={reverse} mode={collision_mode:?} distances={distances:?}");
                }
            }
        }
        // No boundaries: opt-in and old policy use identical first-push arithmetic.
        let mut free = Vec::new();
        for separation_mode in [
            FluidSeparationMode::Legacy,
            FluidSeparationMode::BoundaryConstrained,
        ] {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            world
                .add_fluid(FluidDesc {
                    initial_positions: Some(vec![Vec3::ZERO, vec3(0.01, 0.02, 0.0)]),
                    radius: 0.04,
                    spacing: 0.1,
                    max_particles: 2,
                    particle_collision: true,
                    collision_mode: FluidCollisionMode::SweptMeshes,
                    separation_mode,
                    ..FluidDesc::default()
                })
                .unwrap();
            world.step(1.0 / 60.0);
            free.push((
                world.fluids()[0].particles().to_vec(),
                world.fluids()[0].velocities().to_vec(),
            ));
        }
        assert_eq!(free[0], free[1]);
        // General curved obstacle and very thin wall: a safe correction must remain
        // outside both, even when pair separation requests motion through them.
        for sphere in [false, true] {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            let shape = if sphere {
                Shape::Sphere { radius: 0.2 }
            } else {
                Shape::Box {
                    half_extents: vec3(0.001, 1.0, 1.0),
                }
            };
            world.add_builder(BodyBuilder::r#static(shape));
            let x = if sphere { 0.24 } else { 0.041 };
            world
                .add_fluid(FluidDesc {
                    initial_positions: Some(vec![vec3(x, 0.0, 0.0), vec3(x + 0.01, 0.02, 0.0)]),
                    radius: 0.04,
                    spacing: 0.1,
                    max_particles: 2,
                    particle_collision: true,
                    collision_mode: FluidCollisionMode::SweptMeshes,
                    separation_mode: FluidSeparationMode::BoundaryConstrained,
                    ..FluidDesc::default()
                })
                .unwrap();
            for _ in 0..8 {
                world.step(1.0 / 60.0);
                for p in world.fluids()[0].particles() {
                    assert!(p.is_finite());
                    if sphere {
                        assert!(overlap_distance(*p, Vec3::ZERO) >= 0.24 - 1e-6);
                    } else {
                        assert!(p.x >= 0.041 - 1e-6, "thin wall tunneled: {p:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn boundary_constrained_separation_closed_container_120_steps() {
        let mut world = PhysicsWorld::with_gravity(vec3(0.0, -9.81, 0.0));
        world.set_fixed_dt(1.0 / 60.0);
        for (p, h) in [
            (vec3(0.0, -0.05, 0.0), vec3(0.6, 0.05, 0.6)),
            (vec3(0.0, 1.05, 0.0), vec3(0.6, 0.05, 0.6)),
            (vec3(-0.55, 0.5, 0.0), vec3(0.05, 0.6, 0.6)),
            (vec3(0.55, 0.5, 0.0), vec3(0.05, 0.6, 0.6)),
            (vec3(0.0, 0.5, -0.55), vec3(0.6, 0.6, 0.05)),
            (vec3(0.0, 0.5, 0.55), vec3(0.6, 0.6, 0.05)),
        ] {
            world.add_builder(BodyBuilder::r#static(Shape::Box { half_extents: h }).position(p));
        }
        world
            .add_fluid(FluidDesc {
                center: vec3(0.0, 0.5, 0.0),
                half_extents: Vec3::splat(0.2),
                spacing: 0.1,
                radius: 0.04,
                max_particles: 2000,
                initial_velocity: vec3(0.15, 0.0, -0.1),
                particle_collision: true,
                collision_mode: FluidCollisionMode::SweptMeshes,
                separation_mode: FluidSeparationMode::BoundaryConstrained,
                ..FluidDesc::default()
            })
            .unwrap();
        let mut minimum = f64::INFINITY;
        let mut density_nonzero = 0;
        let mut contacts = 0;
        SEPARATION_PUSHES.with(|n| n.set(0));
        for step in 1..=120 {
            MESH_DENSITY_TRACE.with(|t| *t.borrow_mut() = Some(Vec::new()));
            world.step(1.0 / 60.0);
            let density = MESH_DENSITY_TRACE.with(|t| t.borrow_mut().take().unwrap());
            assert_eq!(density.len(), 125 * 4);
            density_nonzero += density.iter().filter(|(_, a, b, _)| a != b).count();
            assert_eq!(world.step_count, step);
            let f = &world.fluids()[0];
            assert_eq!(f.particles().len(), 125);
            minimum = minimum.min(overlap_minimum(f.particles()).0);
            contacts += f.contacts.iter().filter(|c| c.is_some()).count();
            for (p, v) in f.particles().iter().zip(f.velocities()) {
                assert!(p.is_finite() && v.is_finite() && v.length() <= MAX_SPEED + 1e-4);
                assert!(
                    p.x.abs() <= 0.46 + 1e-4
                        && p.z.abs() <= 0.46 + 1e-4
                        && p.y >= 0.04 - 1e-4
                        && p.y <= 0.96 + 1e-4,
                    "step={step} p={p:?}"
                );
            }
        }
        let pushes = SEPARATION_PUSHES.with(|n| n.get());
        assert!(pushes > 0 && density_nonzero > 0 && contacts > 0);
        println!("ROUND166_LONGRUN steps=120 particles=125 min_pair={minimum:.12} pair_pushes={pushes} density_nonzero={density_nonzero} contacts={contacts}");
    }

    /// Pins the measured limitation of the existing single-pass solver, rather than
    /// asserting a hard non-overlap guarantee that this API does not make.
    #[test]
    fn closed_container_worst_pair_phase_attribution() {
        fn fixture() -> PhysicsWorld {
            let mut world = PhysicsWorld::with_gravity(vec3(0.0, -9.81, 0.0));
            world.set_fixed_dt(1.0 / 60.0);
            for (position, half_extents) in [
                (vec3(0.0, -0.05, 0.0), vec3(0.6, 0.05, 0.6)),
                (vec3(0.0, 1.05, 0.0), vec3(0.6, 0.05, 0.6)),
                (vec3(-0.55, 0.5, 0.0), vec3(0.05, 0.6, 0.6)),
                (vec3(0.55, 0.5, 0.0), vec3(0.05, 0.6, 0.6)),
                (vec3(0.0, 0.5, -0.55), vec3(0.6, 0.6, 0.05)),
                (vec3(0.0, 0.5, 0.55), vec3(0.6, 0.6, 0.05)),
            ] {
                world.add_builder(
                    BodyBuilder::r#static(Shape::Box { half_extents }).position(position),
                );
            }
            let desc = FluidDesc {
                center: vec3(0.0, 0.5, 0.0),
                half_extents: Vec3::splat(0.2),
                spacing: 0.1,
                radius: 0.04,
                max_particles: 2000,
                initial_velocity: vec3(0.15, 0.0, -0.1),
                collision_mode: FluidCollisionMode::SweptMeshes,
                particle_collision: true,
                ..FluidDesc::default()
            };
            assert_eq!(desc.estimated_particles(), 125);
            assert_eq!(desc.iterations, 4);
            world.add_fluid(desc).unwrap();
            world
        }
        let dt = 1.0 / 60.0;
        let mut control = fixture();
        let mut worst = (f64::INFINITY, 0, 0, 0);
        let mut trajectory = Vec::new();
        for step in 1usize..=120 {
            control.step(dt);
            assert_eq!(control.step_count, step as u64);
            let positions = control.fluids()[0].particles();
            let (distance, i, j) = overlap_minimum(positions);
            if distance < worst.0 {
                worst = (distance, step, i, j);
            }
            trajectory.push(positions.to_vec());
        }
        println!(
            "ROUND165_WORST distance={:.12} step={} pair=({}, {}) diameter={:.12}",
            worst.0,
            worst.1,
            worst.2,
            worst.3,
            f64::from(0.08f32)
        );
        assert!(
            (worst.0 - 0.027011102).abs() < 1e-8,
            "same frozen Round164 reproduction: {worst:?}"
        );
        OVERLAP_TRACE.with(|trace| {
            *trace.borrow_mut() = Some(OverlapTrace {
                step: 0,
                target: [worst.2, worst.3],
                phases: Vec::new(),
                pairs: Vec::new(),
            })
        });
        let mut replay = fixture();
        for step in 1..=120 {
            OVERLAP_TRACE.with(|trace| trace.borrow_mut().as_mut().unwrap().step = step);
            replay.step(dt);
            assert_eq!(
                replay.fluids()[0].particles(),
                trajectory[step - 1],
                "tracing preserves exact trajectory step={step}"
            );
        }
        let trace = OVERLAP_TRACE.with(|trace| trace.borrow_mut().take().unwrap());
        assert_eq!(
            trace
                .phases
                .iter()
                .filter(|p| p.phase == "density_pre")
                .count(),
            480
        );
        assert_eq!(
            trace
                .phases
                .iter()
                .filter(|p| p.phase == "density_post")
                .count(),
            480
        );
        assert_eq!(
            trace
                .phases
                .iter()
                .filter(|p| p.phase == "final_wall_post")
                .count(),
            120
        );
        let detailed = std::env::var_os("Q3D_TRACE_OVERLAP").is_some();
        for phase in trace
            .phases
            .iter()
            .filter(|p| detailed || p.step == worst.1)
        {
            println!(
                "ROUND165_PHASE {phase:?} target_distance={:.12}",
                overlap_distance(phase.positions[0], phase.positions[1])
            );
        }
        for pair in trace.pairs.iter().filter(|p| detailed || p.step == worst.1) {
            println!("ROUND165_PAIR {pair:?} target_distance={:.12} own_before={:.12} own_intended={:.12} own_after={:.12}", overlap_distance(pair.target_after[0], pair.target_after[1]), overlap_distance(pair.before[0], pair.before[1]), overlap_distance(pair.intended[0], pair.intended[1]), overlap_distance(pair.after[0], pair.after[1]));
        }
        let phases: Vec<_> = trace.phases.iter().filter(|p| p.step == worst.1).collect();
        let pre = phases.iter().find(|p| p.phase == "separation_pre").unwrap();
        let post = phases
            .iter()
            .find(|p| p.phase == "separation_post_before_final_wall")
            .unwrap();
        let wall = phases
            .iter()
            .find(|p| p.phase == "final_wall_post")
            .unwrap();
        println!("ROUND165_ATTRIBUTION candidate={} pre_min={:?} post_min={:?} final_min={:?} target_pair_pushes={} boundary_limited_pushes={}", pre.candidate, pre.minimum, post.minimum, wall.minimum, trace.pairs.iter().filter(|p| p.step == worst.1 && p.pair == trace.target).count(), trace.pairs.iter().filter(|p| p.step == worst.1 && p.boundary_limited).count());
        // Actual cause: both equal-half corrections cross a different container
        // face. Segment sweeps stop at first contact; they do not promise sliding
        // or redistribution of the blocked half to the other particle.
        let own = trace
            .pairs
            .iter()
            .find(|p| p.step == worst.1 && p.pair == trace.target)
            .unwrap();
        assert!(
            pre.candidate,
            "this pair was in the frozen prediction neighborhood"
        );
        assert!(
            own.boundary_limited,
            "offending phase must remain boundary-limited pair sweep"
        );
        assert!(
            own.intended[0].y < 0.04,
            "particle 112 ideal half-push crosses floor"
        );
        assert!(
            own.intended[1].z > 0.46,
            "particle 122 ideal half-push crosses +Z wall"
        );
        assert!(
            (overlap_distance(own.intended[0], own.intended[1]) - f64::from(0.08f32)).abs() < 1e-7
        );
        assert!(
            own.after[0].y >= 0.04 - 1e-6 && own.after[1].z <= 0.46 + 1e-6,
            "boundary-limited pair correction preserves container clearance"
        );
        assert_eq!(
            post.positions, wall.positions,
            "final wall projection does not manufacture this overlap"
        );
        let density: Vec<_> = phases
            .iter()
            .filter(|p| p.phase == "density_pre" || p.phase == "density_post")
            .collect();
        assert!(
            density.iter().all(|p| p.positions == pre.positions),
            "density did not manufacture the worst-step overlap"
        );
        assert_eq!(wall.minimum.1, worst.2);
        assert_eq!(wall.minimum.2, worst.3);
        assert_eq!(wall.minimum.0, worst.0);
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

    /// Registered operator target: later pair visits must not recompress an earlier pair.
    /// No public time step, density solve, rigid boundary, or repeated separation call.
    #[test]
    #[ignore = "registered unmet all-pair separation target for a single three-particle traversal"]
    fn particle_chain_recompression_target() {
        fn distance(a: Vec3, b: Vec3) -> f64 {
            a.to_array()
                .into_iter()
                .zip(b.to_array())
                .map(|(x, y)| (f64::from(x) - f64::from(y)).powi(2))
                .sum::<f64>()
                .sqrt()
        }
        fn run(points: Vec<Vec3>, label: &str) -> (Vec<Vec3>, Vec<DensityTraceEntry>) {
            let n = points.len();
            let mut fluid = Fluid::new(FluidDesc {
                initial_positions: Some(points.clone()),
                max_particles: n,
                radius: 0.04,
                spacing: 0.1,
                particle_collision: true,
                collision_mode: FluidCollisionMode::SweptMeshes,
                separation_mode: FluidSeparationMode::BoundaryConstrained,
                ..FluidDesc::default()
            })
            .unwrap();
            let mass_bits = fluid.mass.to_bits();
            let total_mass = f64::from(fluid.mass) * n as f64;
            let velocities = fluid.velocities.clone();
            let positions = fluid.positions.clone();
            let h = fluid.desc.smoothing_radius();
            let radius = fluid.desc.radius;
            fluid.predicted = points.clone();
            fluid.neighbors = vec![Vec::new(); n];
            let grid = FluidGrid::new(&fluid.predicted, h).unwrap();
            for (i, neighbors) in fluid.neighbors.iter_mut().enumerate() {
                grid.neighbors(&fluid.predicted, i, h * h, neighbors);
                assert_eq!(neighbors.len(), n - 1, "{label} neighbor count i={i}");
                for j in 0..n {
                    assert_eq!(neighbors.contains(&(j as u32)), i != j);
                }
            }
            println!("CHAIN_INPUT label={label} bits={:?} neighbors={:?} radius_bits={} mass_bits={mass_bits} total_mass={total_mass:.12}", points.iter().map(|p| p.to_array().map(f32::to_bits)).collect::<Vec<_>>(), fluid.neighbors, radius.to_bits());
            MESH_CONTINUITY_TRACE.with(|t| *t.borrow_mut() = Some(Vec::new()));
            fluid.separate_particles(radius, &[], None, None, None);
            let trace = MESH_CONTINUITY_TRACE.with(|t| t.borrow_mut().take().unwrap());
            assert_eq!(fluid.predicted.len(), n);
            assert_eq!(
                fluid.positions, positions,
                "position operator leaves stored positions unchanged"
            );
            assert_eq!(fluid.velocities, velocities);
            assert_eq!(fluid.mass.to_bits(), mass_bits);
            assert_eq!(
                f64::from(fluid.mass) * fluid.predicted.len() as f64,
                total_mass
            );
            assert!(total_mass.is_finite() && total_mass > 0.0);
            for (before, after) in points.iter().zip(&fluid.predicted) {
                assert!(after.is_finite());
                assert!((f64::from(after.y) - f64::from(before.y)).abs() <= 1e-6);
                assert!((f64::from(after.z) - f64::from(before.z)).abs() <= 1e-6);
            }
            for axis in 0..3 {
                let before: f64 = points.iter().map(|p| f64::from(p[axis])).sum();
                let after: f64 = fluid.predicted.iter().map(|p| f64::from(p[axis])).sum();
                assert!(
                    (before - after).abs() <= 1e-6,
                    "{label} position sum axis={axis}: {before} -> {after}"
                );
            }
            (fluid.predicted, trace)
        }

        let physical = [
            vec3(0.0, 1.0, 0.0),
            vec3(0.06, 1.0, 0.0),
            vec3(0.12, 1.0, 0.0),
        ];
        let diameter = 2.0 * f64::from(0.04f32);
        let target = diameter - 1e-4;
        let (control, _) = run(physical[..2].to_vec(), "AB_control");
        assert!(
            distance(control[0], control[1]) >= target,
            "isolated pair control"
        );
        let mut failures = Vec::new();
        let mut completed = 0;
        for order in [[0usize, 1, 2], [2usize, 1, 0]] {
            let label = format!("{order:?}");
            let initial: Vec<_> = order.iter().map(|&i| physical[i]).collect();
            let (actual, trace) = run(initial.clone(), &label);
            assert!(trace.len() >= 7);
            let split = trace.len() - initial.len();
            assert_eq!(split % 2, 0);
            let mut state = initial.clone();
            let mut first_pair = None;
            let mut recompressed = false;
            for (visit, pair) in trace[..split].as_chunks::<2>().0.iter().enumerate() {
                let [(i, before_i, request_i, accepted_i), (j, before_j, request_j, accepted_j)] =
                    pair;
                assert_ne!(i, j);
                assert_eq!(*before_i, state[*i]);
                assert_eq!(*before_j, state[*j]);
                assert_eq!(request_i, accepted_i, "free-space correction i");
                assert_eq!(request_j, accepted_j, "free-space correction j");
                state[*i] = *accepted_i;
                state[*j] = *accepted_j;
                let own_distance = distance(state[*i], state[*j]);
                if visit == 0 {
                    assert_eq!(
                        order[*i].abs_diff(order[*j]),
                        1,
                        "first pair is physically adjacent"
                    );
                    assert!((own_distance - diameter).abs() <= 1e-6);
                    first_pair = Some((*i, *j, own_distance));
                } else {
                    let (a, b, separated) = first_pair.unwrap();
                    if distance(state[a], state[b]) < separated - 1e-6 {
                        recompressed = true;
                    }
                }
                let mut physical_state = [Vec3::ZERO; 3];
                for (index, &identity) in order.iter().enumerate() {
                    physical_state[identity] = state[index];
                }
                println!("CHAIN_PAIR order={order:?} visit={visit} physical_pair=({}, {}) own_distance={own_distance:.12} state_ABC={physical_state:?}", order[*i], order[*j]);
            }
            assert!(
                recompressed,
                "{label}: actual later pair must reduce first-pair distance"
            );
            for &(i, before, request, accepted) in &trace[split..] {
                assert_eq!(before, state[i]);
                assert_eq!(request, before);
                assert_eq!(accepted, before, "no-boundary final projection");
                state[i] = accepted;
            }
            assert_eq!(
                state, actual,
                "trace reconstructs actual final operator state"
            );
            for i in 0..3 {
                for j in i + 1..3 {
                    let measured = distance(actual[i], actual[j]);
                    println!("CHAIN_RESULT order={order:?} physical_pair=({}, {}) initial={:.12} final={measured:.12} target={target:.12} positions={:?}", order[i], order[j], distance(initial[i], initial[j]), [actual[i], actual[j]]);
                    if measured < target {
                        failures.push(format!("order={order:?} physical_pair=({}, {}) distance={measured:.12} target={target:.12}", order[i], order[j]));
                    }
                }
            }
            completed += 1;
        }
        assert_eq!(
            completed, 2,
            "both index orders finish before strong target assertion"
        );
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    #[ignore = "registered single-visit floor separation target"]
    fn boundary_floor_single_visit_target() {
        let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
        world.add_builder(
            BodyBuilder::r#static(Shape::Box {
                half_extents: vec3(0.6, 0.05, 0.6),
            })
            .position(vec3(0.0, -0.05, 0.0)),
        );
        let initial = vec![vec3(0.0, 0.04, 0.0), vec3(0.0, 0.10, 0.0)];
        let radius = 0.04f32;
        let mut fluid = Fluid::new(FluidDesc {
            initial_positions: Some(initial.clone()),
            max_particles: 2,
            radius,
            spacing: 0.1,
            particle_collision: true,
            collision_mode: FluidCollisionMode::SweptMeshes,
            separation_mode: FluidSeparationMode::BoundaryConstrained,
            ..FluidDesc::default()
        })
        .unwrap();
        fluid.predicted = initial;
        fluid.neighbors = vec![vec![1], vec![0]];
        fluid.separate_particles(radius, world.fluid_test_body_slots(), None, None, None);
        let a = fluid.predicted[0];
        let b = fluid.predicted[1];
        let distance = overlap_distance(a, b);
        let target = 2.0 * f64::from(radius) - 1e-4;
        println!(
            "FLOOR_SINGLE_VISIT distance={distance:.12} target={target:.12} positions={:?}",
            fluid.predicted
        );
        assert_eq!(fluid.predicted.len(), 2);
        assert!(fluid
            .predicted
            .iter()
            .all(|p| p.is_finite() && f64::from(p.y) - f64::from(radius) >= -1e-4));
        assert!(
            distance >= target,
            "single-visit floor separation unmet: {distance:.12} < {target:.12}"
        );
    }

    /// Registered strong target; the existing boundary audit does not promise this.
    #[test]
    #[ignore = "unmet diameter-level separation target in the fixed sealed 125-particle fixture"]
    fn sealed_container_diameter_separation_target() {
        let failures = sealed_container_separation_goal(FluidSeparationMode::BoundaryConstrained);
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    fn sealed_container_separation_goal(mode: FluidSeparationMode) -> Vec<String> {
        let dt = 1.0 / 60.0;
        let mut world = PhysicsWorld::with_gravity(vec3(0.0, -9.81, 0.0));
        world.set_fixed_dt(dt);
        for (position, half_extents) in [
            (vec3(0.0, -0.05, 0.0), vec3(0.6, 0.05, 0.6)),
            (vec3(0.0, 1.05, 0.0), vec3(0.6, 0.05, 0.6)),
            (vec3(-0.55, 0.5, 0.0), vec3(0.05, 0.6, 0.6)),
            (vec3(0.55, 0.5, 0.0), vec3(0.05, 0.6, 0.6)),
            (vec3(0.0, 0.5, -0.55), vec3(0.6, 0.6, 0.05)),
            (vec3(0.0, 0.5, 0.55), vec3(0.6, 0.6, 0.05)),
        ] {
            world
                .add_builder(BodyBuilder::r#static(Shape::Box { half_extents }).position(position));
        }
        let desc = FluidDesc {
            center: vec3(0.0, 0.5, 0.0),
            half_extents: Vec3::splat(0.2),
            spacing: 0.1,
            radius: 0.04,
            max_particles: 2000,
            initial_velocity: vec3(0.15, 0.0, -0.1),
            particle_collision: true,
            collision_mode: FluidCollisionMode::SweptMeshes,
            separation_mode: mode,
            ..FluidDesc::default()
        };
        assert_eq!(desc.iterations, 4);
        assert_eq!(desc.estimated_particles(), 125);
        let radius = f64::from(desc.radius);
        let target = 2.0 * radius - 1e-4;
        world.add_fluid(desc).unwrap();
        let mass_bits = world.fluids()[0].mass.to_bits();
        let total_mass = f64::from(world.fluids()[0].mass) * 125.0;
        assert!(total_mass.is_finite() && total_mass > 0.0);
        let mut first_bad_pair = None;
        let mut worst = (f64::INFINITY, 0, 0, 0, Vec3::ZERO, Vec3::ZERO);
        let mut minimum_clearance = f64::INFINITY;
        let mut invariant_failure = None;
        let mut pair_samples = 0usize;
        let mut contacts = 0usize;
        let mut density_nonzero = 0usize;
        let mut uncertified = 0usize;
        SEPARATION_PUSHES.with(|n| n.set(0));
        for step in 1..=120 {
            MESH_DENSITY_TRACE.with(|t| *t.borrow_mut() = Some(Vec::new()));
            world.step(dt);
            let trace = MESH_DENSITY_TRACE.with(|t| t.borrow_mut().take().unwrap());
            density_nonzero += trace.iter().filter(|(_, a, b, _)| a != b).count();
            let fluid = &world.fluids()[0];
            if mode == FluidSeparationMode::GlobalHalfspaceExperimental
                && fluid.experimental_separation_status != ExperimentalSeparationStatus::Certified
            {
                uncertified += 1;
            }
            let positions = fluid.particles();
            let velocities = fluid.velocities();
            if (world.step_count != step
                || world.accumulator() != 0.0
                || positions.len() != 125
                || velocities.len() != 125
                || fluid.mass.to_bits() != mass_bits
                || f64::from(fluid.mass) * positions.len() as f64 != total_mass
                || trace.len() != 125 * 4)
                && invariant_failure.is_none()
            {
                invariant_failure = Some(format!("step={step} count/mass/step/work invariant"));
            }
            contacts += fluid.contacts.iter().filter(|c| c.is_some()).count();
            for (i, &p) in positions.iter().enumerate() {
                let [x, y, z] = p.to_array().map(f64::from);
                let clearance = [
                    x + 0.5 - radius,
                    0.5 - x - radius,
                    y - radius,
                    1.0 - y - radius,
                    z + 0.5 - radius,
                    0.5 - z - radius,
                ]
                .into_iter()
                .fold(f64::INFINITY, f64::min);
                minimum_clearance = minimum_clearance.min(clearance);
                let valid_velocity = velocities.get(i).is_some_and(|v| {
                    let [vx, vy, vz] = v.to_array().map(f64::from);
                    v.is_finite()
                        && (vx * vx + vy * vy + vz * vz).sqrt() <= f64::from(MAX_SPEED) + 1e-4
                });
                if (!p.is_finite() || !valid_velocity || clearance < -1e-4)
                    && invariant_failure.is_none()
                {
                    invariant_failure = Some(format!(
                        "step={step} particle={i} p={p:?} clearance={clearance:.12}"
                    ));
                }
                for (j, &q) in positions.iter().enumerate().skip(i + 1) {
                    let [qx, qy, qz] = q.to_array().map(f64::from);
                    let distance = ((x - qx).powi(2) + (y - qy).powi(2) + (z - qz).powi(2)).sqrt();
                    pair_samples += 1;
                    if !distance.is_finite() && invariant_failure.is_none() {
                        invariant_failure =
                            Some(format!("step={step} pair=({i},{j}) nonfinite distance"));
                    }
                    let sample = (distance, step, i, j, p, q);
                    if distance < worst.0 {
                        worst = sample;
                    }
                    if distance < target && first_bad_pair.is_none() {
                        first_bad_pair = Some(sample);
                    }
                }
            }
        }
        let pushes = SEPARATION_PUSHES.with(|n| n.get());
        println!("SEALED_SEPARATION steps={} samples={pair_samples} target={target:.12} worst={worst:?} first_bad={first_bad_pair:?} clearance={minimum_clearance:.12} total_mass={total_mass:.12} contacts={contacts} pushes={pushes} density_nonzero={density_nonzero} invariant_failure={invariant_failure:?}", world.step_count);
        assert_eq!(pair_samples, 120 * 125 * 124 / 2);
        assert!(
            contacts > 0 && pushes > 0 && density_nonzero > 0,
            "non-vacuous solver work"
        );
        let mut failures = Vec::new();
        if let Some(failure) = invariant_failure {
            failures.push(failure);
        }
        if let Some(first) = first_bad_pair {
            failures.push(format!(
                "diameter separation unmet: target={target:.12} first={first:?} worst={worst:?}"
            ));
        }
        if uncertified > 0 {
            failures.push(format!(
                "mode={mode:?} uncertified_attempts={uncertified}/120"
            ));
        }
        println!("GLOBAL_SEALED mode={mode:?} completed_steps={} uncertified={uncertified} failures={failures:?}", world.step_count);
        failures
    }

    #[derive(Debug)]
    struct GlobalGuardObservation {
        positions: Vec<Vec3>,
        status: ExperimentalSeparationStatus,
        reason: &'static str,
        rows: usize,
        sweeps: usize,
        dual_decreases: usize,
        post_guard_checked: bool,
    }

    // Future adapters must call the actual experimental wrapper/core.
    // These intentionally unimplemented adapters are not validation substitutes.
    fn global_halfspace_218_world_guard_probe(
        points: &[Vec3],
        _masses: &[f64],
        _bodies: &[BodySlot],
        _damage: bool,
    ) -> GlobalGuardObservation {
        GlobalGuardObservation {
            positions: points.to_vec(),
            status: ExperimentalSeparationStatus::StubUncertified,
            reason: "stub",
            rows: 0,
            sweeps: 0,
            dual_decreases: 0,
            post_guard_checked: false,
        }
    }

    fn global_halfspace_218_core_guard_probe(_rows: &[(Vec<f64>, f64)]) -> GlobalGuardObservation {
        global_halfspace_218_world_guard_probe(&[], &[], &[], false)
    }

    #[test]
    #[ignore = "registered atomic wall/capacity guards; wrapper is unimplemented"]
    fn global_halfspace_218_atomic_wall_and_capacity_guards() {
        let initial = vec![vec3(0.0, 0.04, 0.0), vec3(0.0, 0.10, 0.0)];
        let masses = vec![1.0; 2];
        let bits = |points: &[Vec3]| {
            points
                .iter()
                .map(|p| p.to_array().map(f32::to_bits))
                .collect::<Vec<_>>()
        };
        let mut failures = Vec::new();
        let mut checked = 0;
        for plane in [false, true] {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            let shape = if plane {
                Shape::Plane { normal: Vec3::Y }
            } else {
                Shape::Box {
                    half_extents: vec3(0.6, 0.05, 0.6),
                }
            };
            world.add_builder(BodyBuilder::r#static(shape).position(if plane {
                Vec3::ZERO
            } else {
                vec3(0.0, -0.05, 0.0)
            }));
            let result = global_halfspace_218_world_guard_probe(
                &initial,
                &masses,
                world.fluid_test_body_slots(),
                false,
            );
            println!("GLOBAL_GUARD floor_plane={plane} result={result:?}");
            if result.status != ExperimentalSeparationStatus::Certified {
                failures.push(format!("supported floor plane={plane}: {}", result.reason));
            } else {
                assert_eq!(result.sweeps, 128);
                assert_eq!(result.rows, 3);
                let [a, b] = result.positions.as_slice() else {
                    panic!("two particles");
                };
                assert!(
                    f64::from(a.y) >= f64::from(0.04f32) - 1e-4
                        && f64::from(b.y) >= f64::from(0.04f32) - 1e-4
                );
                assert!(overlap_distance(*a, *b) >= 2.0 * f64::from(0.04f32) - 1e-4);
                assert!(result.post_guard_checked);
            }
            checked += 1;
        }
        for (name, builder) in [
            (
                "sphere",
                BodyBuilder::r#static(Shape::Sphere { radius: 0.5 }),
            ),
            (
                "dynamic",
                BodyBuilder::dynamic(Shape::Box {
                    half_extents: Vec3::ONE,
                }),
            ),
            (
                "kinematic",
                BodyBuilder::kinematic(Shape::Box {
                    half_extents: Vec3::ONE,
                }),
            ),
            (
                "compound",
                BodyBuilder::r#static(Shape::Box {
                    half_extents: Vec3::ONE,
                })
                .part_at(Shape::Sphere { radius: 0.1 }, Vec3::X),
            ),
        ] {
            let mut world = PhysicsWorld::with_gravity(Vec3::ZERO);
            world.add_builder(builder);
            let result = global_halfspace_218_world_guard_probe(
                &initial,
                &masses,
                world.fluid_test_body_slots(),
                false,
            );
            assert_eq!(bits(&result.positions), bits(&initial), "{name} rollback");
            if result.status == ExperimentalSeparationStatus::Certified
                || result.reason != "unsupported"
            {
                failures.push(format!("{name} must reject unsupported: {result:?}"));
            }
            checked += 1;
        }
        // Builder intentionally ignores scale; inject the unsupported runtime transform directly.
        let mut scaled = PhysicsWorld::with_gravity(Vec3::ZERO);
        let handle = scaled.add_builder(BodyBuilder::r#static(Shape::Box {
            half_extents: Vec3::ONE,
        }));
        scaled.body_mut(handle).unwrap().transform.scale = Vec3::splat(2.0);
        let result = global_halfspace_218_world_guard_probe(
            &initial,
            &masses,
            scaled.fluid_test_body_slots(),
            false,
        );
        assert_eq!(bits(&result.positions), bits(&initial));
        if result.reason != "unsupported"
            || result.status == ExperimentalSeparationStatus::Certified
        {
            failures.push(format!("nonunit runtime scale must reject: {result:?}"));
        }
        checked += 1;
        // 32640 pair rows fit; one wall adds256 and must exceed32768.
        let many: Vec<_> = (0..256).map(|i| vec3(i as f32 * 0.1, 1.0, 0.0)).collect();
        let free = global_halfspace_218_world_guard_probe(&many, &vec![1.0; 256], &[], false);
        if free.status != ExperimentalSeparationStatus::Certified
            || free.rows != 32640
            || free.sweeps != 128
        {
            failures.push(format!("below-cap free graph positive control: {free:?}"));
        }
        let mut wall = PhysicsWorld::with_gravity(Vec3::ZERO);
        wall.add_builder(BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y }));
        let result = global_halfspace_218_world_guard_probe(
            &many,
            &vec![1.0; 256],
            wall.fluid_test_body_slots(),
            false,
        );
        assert_eq!(bits(&result.positions), bits(&many));
        if result.reason != "row_cap"
            || result.status == ExperimentalSeparationStatus::Certified
            || result.rows != 0
            || result.sweeps != 0
        {
            failures.push(format!(
                "wall-inclusive cap rejects before generation: {result:?}"
            ));
        }
        checked += 2;
        for invalid_mass in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let result =
                global_halfspace_218_world_guard_probe(&initial, &[1.0, invalid_mass], &[], false);
            assert_eq!(bits(&result.positions), bits(&initial));
            if result.reason != "invalid_input"
                || result.status == ExperimentalSeparationStatus::Certified
            {
                failures.push(format!("invalid mass={invalid_mass}: {result:?}"));
            }
            checked += 1;
        }
        let mut nonfinite = initial.clone();
        nonfinite[1].x = f32::NAN;
        let result = global_halfspace_218_world_guard_probe(&nonfinite, &masses, &[], false);
        assert_eq!(bits(&result.positions), bits(&nonfinite));
        if result.reason != "invalid_input" {
            failures.push("nonfinite coordinate must reject".to_owned());
        }
        checked += 1;
        for damage in [false, true] {
            let result = global_halfspace_218_world_guard_probe(&initial, &masses, &[], damage);
            if damage {
                assert_eq!(bits(&result.positions), bits(&initial));
                if result.reason != "post_guard_certificate"
                    || result.status == ExperimentalSeparationStatus::Certified
                    || !result.post_guard_checked
                {
                    failures.push(format!(
                        "post-guard overlap must reject atomically: {result:?}"
                    ));
                }
            } else if result.status != ExperimentalSeparationStatus::Certified
                || !result.post_guard_checked
            {
                failures.push("undamaged free positive control must certify".to_owned());
            }
            checked += 1;
        }
        println!("GLOBAL_GUARDS completed={checked} failures={failures:?}");
        assert_eq!(checked, 16);
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    #[ignore = "registered fixed-budget dual-memory guards; core is unimplemented"]
    fn global_halfspace_218_dual_memory_and_budget_guards() {
        let cases = [
            (
                "decrease",
                vec![(vec![1.0, 0.0], 1.0), (vec![1.0, 1.0], 3.0)],
                true,
            ),
            (
                "decrease_reversed",
                vec![(vec![1.0, 1.0], 3.0), (vec![1.0, 0.0], 1.0)],
                true,
            ),
            (
                "inconsistent",
                vec![(vec![1.0, 0.0], 1.0), (vec![-1.0, 0.0], 1.0)],
                false,
            ),
            (
                "near_parallel_budget",
                vec![(vec![1.0, 0.0], 1.0), (vec![-1.0, 1e-6], 1.0)],
                false,
            ),
        ];
        let mut failures = Vec::new();
        for (name, rows, should_certify) in cases {
            let result = global_halfspace_218_core_guard_probe(&rows);
            println!("GLOBAL_CORE_GUARD name={name} result={result:?}");
            if should_certify {
                if result.status != ExperimentalSeparationStatus::Certified || result.sweeps != 128
                {
                    failures.push(format!("{name}: expected certified fixed-budget result"));
                }
                if name == "decrease" && result.dual_decreases == 0 {
                    failures.push("must undo prior multiplier contribution".to_owned());
                }
            } else if result.status == ExperimentalSeparationStatus::Certified
                || result.reason != "budget_residual"
                || result.sweeps != 128
            {
                failures.push(format!("{name}: expected fixed128 uncertified residual"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    // Test-only algebra entrypoint; stage one intentionally has no solver.
    fn global_halfspace_218_algebra_stub(_rows: &[(Vec<f64>, f64)]) -> Option<Vec<f64>> {
        None
    }

    #[test]
    #[ignore = "registered algebra goal; no global solver implemented"]
    fn global_halfspace_218_dense_algebra_targets() {
        // Independent two-variable active-subset enumeration, not dual iteration.
        fn dense_oracle(rows: &[(Vec<f64>, f64)]) -> Option<[f64; 2]> {
            let feasible = |x: [f64; 2]| {
                rows.iter()
                    .all(|(a, b)| a[0] * x[0] + a[1] * x[1] >= b - 1e-10)
            };
            let mut candidates = vec![[0.0, 0.0]];
            for (i, (a, b)) in rows.iter().enumerate() {
                let aa = a[0] * a[0] + a[1] * a[1];
                if aa > 0.0 {
                    candidates.push([a[0] * b / aa, a[1] * b / aa]);
                }
                for (c, d) in &rows[i + 1..] {
                    let determinant = a[0] * c[1] - a[1] * c[0];
                    if determinant.abs() > 1e-12 {
                        candidates.push([
                            (b * c[1] - a[1] * d) / determinant,
                            (a[0] * d - b * c[0]) / determinant,
                        ]);
                    }
                }
            }
            candidates
                .into_iter()
                .filter(|&x| feasible(x))
                .min_by(|a, b| (a[0] * a[0] + a[1] * a[1]).total_cmp(&(b[0] * b[0] + b[1] * b[1])))
        }
        let cases = [
            (
                "equal_pair",
                vec![(vec![-1.0, 1.0], 0.02)],
                Some([-0.01, 0.01]),
            ),
            (
                "unequal_pair",
                vec![(vec![-1.0, 0.5], 0.02)],
                Some([-0.016, 0.008]),
            ),
            ("slack", vec![(vec![-1.0, 1.0], -0.02)], Some([0.0, 0.0])),
            (
                "floor_pair",
                vec![(vec![-1.0, 1.0], 0.02), (vec![1.0, 0.0], 0.0)],
                Some([0.0, 0.02]),
            ),
            (
                "redundant",
                vec![(vec![-1.0, 1.0], 0.02), (vec![-1.0, 1.0], 0.02)],
                Some([-0.01, 0.01]),
            ),
            (
                "inconsistent",
                vec![(vec![1.0, 0.0], 1.0), (vec![-1.0, 0.0], 1.0)],
                None,
            ),
        ];
        let mut failures = Vec::new();
        for (name, rows, expected) in cases {
            let oracle = dense_oracle(&rows);
            assert_eq!(oracle.is_some(), expected.is_some());
            if let (Some(actual), Some(expected)) = (oracle, expected) {
                for axis in 0..2 {
                    assert!((actual[axis] - expected[axis]).abs() < 1e-10);
                }
                if name == "unequal_pair" {
                    // y=(sqrt(1)*delta_a,sqrt(4)*delta_b).
                    assert!((actual[0] + 4.0 * actual[1] / 2.0).abs() < 1e-10);
                }
            }
            let candidate = global_halfspace_218_algebra_stub(&rows);
            println!("GLOBAL_ALGEBRA name={name} oracle={oracle:?} candidate={candidate:?}");
            if let Some(reference) = oracle {
                match candidate {
                    Some(x)
                        if x.len() == 2
                            && (x[0] - reference[0]).abs() <= 1e-7
                            && (x[1] - reference[1]).abs() <= 1e-7 => {}
                    _ => failures.push(format!(
                        "{name}: missing/incorrect candidate, oracle={reference:?}"
                    )),
                }
            } else {
                assert!(candidate.is_none());
            }
        }
        assert!(
            failures.is_empty(),
            "algebra stub only, not physical red: {}",
            failures.join("; ")
        );
    }

    #[test]
    #[ignore = "registered experimental global separation precision target; stub is uncertified"]
    fn global_halfspace_218_physical_targets() {
        fn distance(a: Vec3, b: Vec3) -> f64 {
            a.to_array()
                .into_iter()
                .zip(b.to_array())
                .map(|(x, y)| (f64::from(x) - f64::from(y)).powi(2))
                .sum::<f64>()
                .sqrt()
        }
        let physical = [
            vec3(0.0, 1.0, 0.0),
            vec3(0.06, 1.0, 0.0),
            vec3(0.12, 1.0, 0.0),
        ];
        let radius = 0.04f32;
        let target = 2.0 * f64::from(radius) - 1e-4;
        let mut failures = Vec::new();
        let mut completed = 0;
        // Isolated control and both orders all execute before the final assertion.
        for order in [vec![0usize, 1], vec![0usize, 1, 2], vec![2usize, 1, 0]] {
            let initial: Vec<_> = order.iter().map(|&i| physical[i]).collect();
            let n = initial.len();
            let mut fluid = Fluid::new(FluidDesc {
                initial_positions: Some(initial.clone()),
                max_particles: n,
                radius,
                spacing: 0.1,
                particle_collision: true,
                collision_mode: FluidCollisionMode::SweptMeshes,
                separation_mode: FluidSeparationMode::GlobalHalfspaceExperimental,
                ..FluidDesc::default()
            })
            .unwrap();
            let mass = fluid.mass.to_bits();
            let velocities = fluid.velocities.clone();
            fluid.predicted = initial.clone();
            fluid.neighbors = vec![Vec::new(); n];
            let h = fluid.desc.smoothing_radius();
            let grid = FluidGrid::new(&initial, h).unwrap();
            for (i, neighbors) in fluid.neighbors.iter_mut().enumerate() {
                grid.neighbors(&initial, i, h * h, neighbors);
                assert_eq!(neighbors.len(), n - 1);
                for j in 0..n {
                    assert_eq!(neighbors.contains(&(j as u32)), i != j);
                }
            }
            fluid.separate_particles(radius, &[], None, None, None);
            assert_eq!(fluid.predicted.len(), n);
            assert_eq!(fluid.positions, initial);
            assert_eq!(fluid.mass.to_bits(), mass);
            assert_eq!(fluid.velocities, velocities);
            for axis in 0..3 {
                let before: f64 = initial.iter().map(|p| f64::from(p[axis])).sum();
                let after: f64 = fluid.predicted.iter().map(|p| f64::from(p[axis])).sum();
                assert!((before - after).abs() <= 1e-6);
            }
            for p in &fluid.predicted {
                assert!(p.is_finite());
                assert!((f64::from(p.y) - 1.0).abs() <= 1e-6 && f64::from(p.z).abs() <= 1e-6);
            }
            for i in 0..n {
                for j in i + 1..n {
                    let measured = distance(fluid.predicted[i], fluid.predicted[j]);
                    println!("GLOBAL_CHAIN order={order:?} physical_pair=({}, {}) distance={measured:.12} target={target:.12} status={:?}", order[i], order[j], fluid.experimental_separation_status);
                    if measured < target {
                        failures.push(format!("order={order:?} pair=({}, {}) distance={measured:.12} target={target:.12}", order[i], order[j]));
                    }
                }
            }
            if fluid.experimental_separation_status != ExperimentalSeparationStatus::Certified {
                failures.push(format!(
                    "order={order:?} status={:?}",
                    fluid.experimental_separation_status
                ));
            }
            completed += 1;
        }
        assert_eq!(completed, 3);
        failures.extend(sealed_container_separation_goal(
            FluidSeparationMode::GlobalHalfspaceExperimental,
        ));
        println!("GLOBAL_COMPLETED control=1 chains=2 sealed_steps=120 failures={failures:?}");
        assert!(failures.is_empty(), "{}", failures.join("\n"));
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
