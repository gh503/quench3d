//! 顺序冲量求解器：热启动 + 累积摩擦 + 分裂冲量（伪速度）+ 松弛 + 睡眠。
//!
//! 关键设计：
//! * 每个接触点维护「法向 1 + 切向 2」三个累积冲量，逐迭代做投影（clamp）；
//! * 恢复冲量使用**求解前**的相对法向速度，避免求解过程改变恢复目标；
//! * 位置修正走**分裂冲量**：单独维护伪速度，只用于积分位置，不进入真实速度，
//!   因此不会给系统注入能量；
//! * 睡眠：低速持续 0.5s 后冻结速度并停止积分，接触配对时会被运动体唤醒。

use quench3d_math::*;

use crate::body::{
    BodyType, RigidBody, MAX_ANGULAR_SPEED, MAX_LINEAR_SPEED, SLEEP_ANGULAR_TOL, SLEEP_LINEAR_TOL,
    SLEEP_TIME,
};
use crate::contact::ContactPoint;
use crate::small_vec::SmallVec;

/// 求解器侧每个流形最多保留的接触点数（与 `ContactManifold::MAX_POINTS` 一致）。
const MAX_MANIFOLD_POINTS: usize = 4;

/// 求解器可调参数。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SolverConfig {
    /// 速度迭代次数。
    pub velocity_iterations: usize,
    /// 位置（分裂冲量）迭代次数。
    pub position_iterations: usize,
    /// 松弛迭代次数。
    pub relaxation_iterations: usize,
    /// 是否启用热启动。
    pub warm_starting: bool,
    /// 是否允许 island 并行求解（native）。
    ///
    /// 并行求解在数学上与串行**逐位相同**（岛与岛不共享清醒刚体、岛内保持原顺序），
    /// 这个开关的存在是为了让回归测试能把两条路径的结果直接对比。
    /// wasm 上没有多线程，这个字段恒被忽略。
    pub parallel: bool,
    /// 并行求解最多用几个线程；`0` = 自动（按硬件并行度）。
    ///
    /// 线程数**不影响结果**：岛与岛不共享清醒刚体，每个刚体经历的运算顺序
    /// 与串行完全一致。这个字段主要用于测试「不同线程数结果逐位相同」。
    pub max_threads: usize,
    /// Baumgarte 位置修正系数。
    pub baumgarte: f32,
    /// 允许的穿透余量。
    pub slop: f32,
    /// 位置修正的最大伪速度。
    pub max_correction_velocity: f32,
    /// 低于此相对速度不做恢复（防止抖动）。
    pub restitution_threshold: f32,
    /// 是否启用自动睡眠。
    pub sleep_enabled: bool,
}

impl Default for SolverConfig {
    fn default() -> Self {
        Self {
            velocity_iterations: 10,
            position_iterations: 4,
            relaxation_iterations: 1,
            warm_starting: true,
            parallel: true,
            max_threads: 0,
            baumgarte: 0.2,
            slop: 0.005,
            max_correction_velocity: 3.0,
            restitution_threshold: 1.0,
            sleep_enabled: true,
        }
    }
}

/// 刚体存储抽象：世界用带世代的槽位，单元测试用 `Vec<Option<RigidBody>>`。
pub(crate) trait BodyStore {
    /// 按槽位下标可变访问。
    fn slot_mut(&mut self, index: usize) -> Option<&mut RigidBody>;
    /// 槽位数量。
    fn slot_count(&self) -> usize;
}

impl BodyStore for Vec<Option<RigidBody>> {
    fn slot_mut(&mut self, index: usize) -> Option<&mut RigidBody> {
        self.get_mut(index).and_then(|s| s.as_mut())
    }

    fn slot_count(&self) -> usize {
        self.len()
    }
}

/// 求解器看到的刚体视图（值语义，避免迭代时的借用冲突）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct BodyView {
    /// 形状原点位置。
    pub pos: Vec3,
    /// 旋转。
    pub rot: Quat,
    /// 质心世界位置。
    pub com: Vec3,
    /// 线速度。
    pub lin: Vec3,
    /// 角速度。
    pub ang: Vec3,
    /// 伪线速度（位置修正用）。
    pub pseudo_lin: Vec3,
    /// 伪角速度。
    pub pseudo_ang: Vec3,
    /// 有效质量倒数（睡眠/静态/运动学为 0）。
    pub inv_m: f32,
    /// 世界逆惯量张量 `R·diag(inv_i_local)·Rᵀ`（冻结时为零矩阵）。
    ///
    /// 每个刚体每步只在这里算一次，之后 `inv_i_world` 就只是一次 3×3 乘向量。
    /// 之前是每次调用都做「四元数逆旋转 → 乘对角 → 四元数正旋转」，
    /// 而它在求解器里每个接触每次迭代都要调用（两侧各一次），
    /// 深堆场景一步就是十几万次，占了相当一部分时间。
    pub inv_i_world_mat: Mat3,
}

/// 世界坐标下的逆惯量张量：`R·diag(local)·Rᵀ`。
///
/// `R·diag(local)` 就是把旋转矩阵的各列按 `local` 缩放，再右乘 `Rᵀ` 即可。
#[inline]
pub(crate) fn world_inv_inertia(rot: Quat, local: Vec3) -> Mat3 {
    let r = rot.to_mat3();
    let scaled = Mat3::from_cols(
        r.cols[0] * local.x,
        r.cols[1] * local.y,
        r.cols[2] * local.z,
    );
    scaled * r.transpose()
}

impl BodyView {
    /// 从刚体构造；睡眠或非动态体按无限质量处理。
    pub fn from_body(b: &RigidBody) -> Self {
        let frozen = b.sleeping || b.body_type != BodyType::Dynamic;
        Self {
            pos: b.transform.position,
            rot: b.transform.rotation,
            com: b.com_world(),
            lin: b.linear_velocity,
            ang: b.angular_velocity,
            pseudo_lin: Vec3::ZERO,
            pseudo_ang: Vec3::ZERO,
            inv_m: if frozen { 0.0 } else { b.inv_mass },
            inv_i_world_mat: if frozen {
                Mat3::ZERO
            } else {
                world_inv_inertia(b.transform.rotation, b.inv_inertia_local)
            },
        }
    }

    /// 空槽位占位（完全冻结、位于原点）。
    pub fn frozen() -> Self {
        Self {
            pos: Vec3::ZERO,
            rot: Quat::IDENTITY,
            com: Vec3::ZERO,
            lin: Vec3::ZERO,
            ang: Vec3::ZERO,
            pseudo_lin: Vec3::ZERO,
            pseudo_ang: Vec3::ZERO,
            inv_m: 0.0,
            inv_i_world_mat: Mat3::ZERO,
        }
    }

    /// 世界逆惯量张量作用。
    #[inline]
    pub fn inv_i_world(&self, v: Vec3) -> Vec3 {
        self.inv_i_world_mat.mul_vec3(v)
    }

    /// 某点速度：`v + ω × r`。
    #[inline]
    pub fn velocity_at(&self, r: Vec3) -> Vec3 {
        self.lin + self.ang.cross(r)
    }

    /// 某点伪速度。
    #[inline]
    pub fn pseudo_velocity_at(&self, r: Vec3) -> Vec3 {
        self.pseudo_lin + self.pseudo_ang.cross(r)
    }

    /// 沿 `n` 的有效质量倒数：`1/m + n·[(I⁻¹(r×n))×r]`。
    #[inline]
    pub fn k_axis(&self, r: Vec3, n: Vec3) -> f32 {
        self.inv_m + n.dot(self.inv_i_world(r.cross(n)).cross(r))
    }
}

/// 缓存的冲量（跨步热启动）。
#[derive(Clone, Copy, Debug)]
pub(crate) struct CachedImpulse {
    /// A 的本地接触点。
    pub local_a: Vec3,
    /// B 的本地接触点。
    pub local_b: Vec3,
    /// 累积法向冲量。
    pub normal: f32,
    /// 累积切向冲量。
    pub tangent: [f32; 2],
}

/// 接触冲量缓存（热启动用）。
///
/// 结构是「排序后的刚体对键 + 区间 + 展平的冲量」，所有缓冲跨步复用：
/// 早期实现每步把所有条目塞进 `BTreeMap` 再清空，一个 2000 体场景因此每步
/// 要分配上万次——换掉之后稳态零分配，行为完全一致（按键稳定排序，
/// 同一个键内部保持原来的插入顺序，选中规则不变）。
#[derive(Clone, Debug, Default)]
pub(crate) struct ContactCache {
    /// 稳定排序后的刚体对键。
    keys: Vec<(u32, u32)>,
    /// 每个键在 [`ContactCache::impulses`] 中的区间 `[start, end)`。
    ranges: Vec<(u32, u32)>,
    /// 展平的冲量，按键分组。
    impulses: Vec<CachedImpulse>,
    /// 重建时的排序缓冲 `(键, 原始序号, 冲量)`；
    /// 序号保证不稳定排序后的顺序与稳定排序完全一致（且不分配临时缓冲）。
    scratch: Vec<((u32, u32), u32, CachedImpulse)>,
}

impl ContactCache {
    /// 清空（保留已分配的容量）。
    pub fn clear(&mut self) {
        self.keys.clear();
        self.ranges.clear();
        self.impulses.clear();
        self.scratch.clear();
    }

    /// 用当前这一步的接触重建缓存（缓冲复用，稳态零分配）。
    pub fn rebuild(&mut self, contacts: &[SolverContact]) {
        self.keys.clear();
        self.ranges.clear();
        self.impulses.clear();
        self.scratch.clear();
        self.scratch.reserve(contacts.len());
        for (seq, c) in contacts.iter().enumerate() {
            self.scratch.push((
                (c.a as u32, c.b as u32),
                seq as u32,
                CachedImpulse {
                    local_a: c.local_a,
                    local_b: c.local_b,
                    normal: c.normal_impulse,
                    tangent: c.tangent_impulse,
                },
            ));
        }
        // 按 (刚体对, 原始序号) 做**不稳定**排序：结果与稳定排序完全一致，
        // 但不会为了稳定性分配临时缓冲（那会变成每步一次分配）。
        self.scratch
            .sort_unstable_by(|x, y| x.0.cmp(&y.0).then(x.1.cmp(&y.1)));
        let mut k = 0usize;
        while k < self.scratch.len() {
            let key = self.scratch[k].0;
            let start = self.impulses.len() as u32;
            while k < self.scratch.len() && self.scratch[k].0 == key {
                self.impulses.push(self.scratch[k].2);
                k += 1;
            }
            self.keys.push(key);
            self.ranges.push((start, self.impulses.len() as u32));
        }
    }

    /// 某个刚体对的缓存区间。
    #[inline]
    fn list(&self, a: u32, b: u32) -> &[CachedImpulse] {
        match self.keys.binary_search(&(a, b)) {
            Ok(pos) => {
                let (start, end) = self.ranges[pos];
                &self.impulses[start as usize..end as usize]
            }
            Err(_) => &[],
        }
    }

    /// 缓存条目总数（调试用）。
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.impulses.len()
    }

    /// 查询与给定本地锚点最接近的缓存冲量。
    fn lookup(
        &self,
        a: u32,
        b: u32,
        local_a: Vec3,
        local_b: Vec3,
        tol: f32,
    ) -> Option<CachedImpulse> {
        let list = self.list(a, b);
        let mut best: Option<(f32, CachedImpulse)> = None;
        for c in list {
            // 两端锚点都要接近，避免把不同特征点的冲量错误沿用。
            let d = (c.local_a - local_a).length_squared() + (c.local_b - local_b).length_squared();
            if d > tol * tol {
                continue;
            }
            if best.is_none() || d < best.unwrap().0 {
                best = Some((d, *c));
            }
        }
        best.map(|(_, c)| c)
    }
}

/// 求解器内部使用的接触点。
#[derive(Clone, Copy, Debug)]
pub(crate) struct SolverContact {
    /// A 的刚体下标。
    pub a: usize,
    /// B 的刚体下标。
    pub b: usize,
    /// 法线（A→B）。
    pub normal: Vec3,
    /// A 上接触点相对质心的向量。
    pub ra: Vec3,
    /// B 上接触点相对质心的向量。
    pub rb: Vec3,
    /// A 的本地接触点（相对形状原点）。
    pub local_a: Vec3,
    /// B 的本地接触点。
    pub local_b: Vec3,
    /// 切向基。
    pub tangent: [Vec3; 2],
    /// 法向有效质量倒数。
    pub k_normal: f32,
    /// 切向有效质量倒数。
    pub k_tangent: [f32; 2],
    /// 摩擦系数。
    pub friction: f32,
    /// 恢复目标速度。
    pub restitution_bias: f32,
    /// 初始穿透深度。
    pub penetration: f32,
    /// 累积法向冲量。
    pub normal_impulse: f32,
    /// 累积切向冲量。
    pub tangent_impulse: [f32; 2],
    /// 伪冲量（位置修正）。
    pub pseudo_impulse: f32,
}

/// 一个待求解的接触流形（记录刚体下标）。
#[derive(Clone, Debug)]
pub(crate) struct ManifoldEntry {
    /// A 下标。
    pub a: usize,
    /// B 下标。
    pub b: usize,
    /// 接触点（内联存放，容量 4；超出会自动退化成堆，行为不变）。
    pub points: SmallVec<ContactPoint, MAX_MANIFOLD_POINTS>,
}

/// 接触求解器。
#[derive(Clone, Debug, Default)]
pub(crate) struct ContactSolver {
    /// 所有求解接触点。
    pub contacts: Vec<SolverContact>,
}

impl ContactSolver {
    /// 准备阶段：计算有效质量、恢复目标、热启动冲量。
    pub fn prepare(
        &mut self,
        views: &[BodyView],
        materials: &[(f32, f32)],
        manifolds: &[ManifoldEntry],
        cache: &ContactCache,
        cfg: &SolverConfig,
    ) {
        self.contacts.clear();
        for m in manifolds {
            let (a, b) = (m.a, m.b);
            let (fa, ea) = match materials.get(a) {
                Some(x) => *x,
                None => continue,
            };
            let (fb, eb) = match materials.get(b) {
                Some(x) => *x,
                None => continue,
            };
            let va = views[a];
            let vb = views[b];
            let friction = (fa.max(0.0) * fb.max(0.0)).sqrt();
            let restitution = ea.max(eb);
            for cp in &m.points {
                let (position, normal, penetration) = (cp.position, cp.normal, cp.penetration);
                if !position.is_finite() || !normal.is_finite() || !penetration.is_finite() {
                    continue;
                }
                let n = normal.normalize_or_zero();
                if n.length_squared() < 0.5 {
                    continue;
                }
                let ra = position - va.com;
                let rb = position - vb.com;
                let k_normal = va.k_axis(ra, n) + vb.k_axis(rb, n);
                if !k_normal.is_finite() || k_normal <= 1e-12 {
                    continue;
                }
                let (t1, t2) = n.any_orthonormal_pair();
                let k1 = va.k_axis(ra, t1) + vb.k_axis(rb, t1);
                let k2 = va.k_axis(ra, t2) + vb.k_axis(rb, t2);
                // 恢复：用求解前的相对法向速度。
                let vrel = vb.velocity_at(rb) - va.velocity_at(ra);
                let vn = vrel.dot(n);
                let restitution_bias = if vn < -cfg.restitution_threshold {
                    -restitution * vn
                } else {
                    0.0
                };
                let local_a = va.rot.inverse().mul_vec3(position - va.pos);
                let local_b = vb.rot.inverse().mul_vec3(position - vb.pos);
                let mut normal_impulse = 0.0;
                let mut tangent_impulse = [0.0f32; 2];
                if cfg.warm_starting {
                    if let Some(c) = cache.lookup(a as u32, b as u32, local_a, local_b, 0.02) {
                        normal_impulse = c.normal.max(0.0);
                        tangent_impulse = c.tangent;
                    }
                }
                self.contacts.push(SolverContact {
                    a,
                    b,
                    normal: n,
                    ra,
                    rb,
                    local_a,
                    local_b,
                    tangent: [t1, t2],
                    k_normal,
                    k_tangent: [k1, k2],
                    friction,
                    restitution_bias,
                    penetration,
                    normal_impulse,
                    tangent_impulse,
                    pseudo_impulse: 0.0,
                });
            }
        }
    }

    /// 热启动：把缓存冲量作为初始猜测应用到速度上。
    pub fn warm_start(&self, views: &mut [BodyView], cfg: &SolverConfig) {
        if !cfg.warm_starting {
            return;
        }
        for c in &self.contacts {
            let p = c.normal * c.normal_impulse
                + c.tangent[0] * c.tangent_impulse[0]
                + c.tangent[1] * c.tangent_impulse[1];
            apply_impulse(views, c.a, c.b, c.ra, c.rb, p);
        }
    }

    /// 一轮速度迭代。
    pub fn solve_velocity(&mut self, views: &mut [BodyView]) {
        solve_velocity_contacts(&mut self.contacts, views);
    }

    /// 松弛：只在仍然相互接近时补一点冲量，绝不抽走能量。
    pub fn relax(&mut self, views: &mut [BodyView], iterations: usize, gain: f32) {
        relax_contacts(&mut self.contacts, views, iterations, gain);
    }

    /// 位置修正（分裂冲量）：只更新伪速度。
    pub fn solve_position(
        &mut self,
        views: &mut [BodyView],
        dt: f32,
        cfg: &SolverConfig,
        iterations: usize,
    ) {
        solve_position_contacts(&mut self.contacts, views, dt, cfg, iterations);
    }

    /// 把本步的冲量写回缓存，供下一帧热启动。
    pub fn store_cache(&self, cache: &mut ContactCache) {
        cache.rebuild(&self.contacts);
    }
}

/// 对一组接触做一轮速度迭代。
///
/// 串行路径传整张接触表，并行路径传某个 island 的接触切片——**同一份实现**，
/// 避免两条路径的浮点运算顺序悄悄分叉。调用方必须保证切片内接触的下标
/// 已经映射到 `views` 的局部下标。
pub(crate) fn solve_velocity_contacts(contacts: &mut [SolverContact], views: &mut [BodyView]) {
    {
        for c in contacts.iter_mut() {
            let va = views[c.a];
            let vb = views[c.b];
            let mut vrel = vb.velocity_at(c.rb) - va.velocity_at(c.ra);
            // 摩擦（用上一轮的法向冲量作为上界）
            let max_friction = c.friction * c.normal_impulse;
            for i in 0..2 {
                let k = c.k_tangent[i];
                if k <= 1e-12 {
                    continue;
                }
                let vt = vrel.dot(c.tangent[i]);
                let mut d = -vt / k;
                let new = clamp(c.tangent_impulse[i] + d, -max_friction, max_friction);
                d = new - c.tangent_impulse[i];
                c.tangent_impulse[i] = new;
                apply_impulse(views, c.a, c.b, c.ra, c.rb, c.tangent[i] * d);
                let va2 = views[c.a];
                let vb2 = views[c.b];
                vrel = vb2.velocity_at(c.rb) - va2.velocity_at(c.ra);
            }
            // 法向
            let vn = vrel.dot(c.normal);
            let mut d = -(vn - c.restitution_bias) / c.k_normal;
            let new = (c.normal_impulse + d).max(0.0);
            d = new - c.normal_impulse;
            c.normal_impulse = new;
            apply_impulse(views, c.a, c.b, c.ra, c.rb, c.normal * d);
        }
    }
}

/// 对一组接触做松弛（实现见 [`ContactSolver::relax`]）。
pub(crate) fn relax_contacts(
    contacts: &mut [SolverContact],
    views: &mut [BodyView],
    iterations: usize,
    gain: f32,
) {
    {
        for _ in 0..iterations {
            for c in contacts.iter_mut() {
                let va = views[c.a];
                let vb = views[c.b];
                let vrel = vb.velocity_at(c.rb) - va.velocity_at(c.ra);
                let vn = vrel.dot(c.normal);
                if vn >= 0.0 || c.normal_impulse <= 0.0 {
                    continue;
                }
                let d = -gain * vn / c.k_normal;
                c.normal_impulse += d;
                apply_impulse(views, c.a, c.b, c.ra, c.rb, c.normal * d);
            }
        }
    }
}

/// 对一组接触做位置修正（实现见 [`ContactSolver::solve_position`]）。
pub(crate) fn solve_position_contacts(
    contacts: &mut [SolverContact],
    views: &mut [BodyView],
    dt: f32,
    cfg: &SolverConfig,
    iterations: usize,
) {
    {
        if dt <= 0.0 {
            return;
        }
        for c in contacts.iter_mut() {
            c.pseudo_impulse = 0.0;
        }
        for _ in 0..iterations {
            for c in contacts.iter_mut() {
                // 用窄相本步给出的穿透深度作为目标：解析接触的两个「锚点」可能
                // 退化成同一点（例如球-面），因此不能用锚点间距反推穿透。
                let pen = c.penetration;
                if pen <= cfg.slop {
                    continue;
                }
                let va = views[c.a];
                let vb = views[c.b];
                let target =
                    ((cfg.baumgarte / dt) * (pen - cfg.slop)).min(cfg.max_correction_velocity);
                let vrel = vb.pseudo_velocity_at(c.rb) - va.pseudo_velocity_at(c.ra);
                let vn = vrel.dot(c.normal);
                let mut d = (target - vn) / c.k_normal;
                let new = (c.pseudo_impulse + d).max(0.0);
                d = new - c.pseudo_impulse;
                if d <= 0.0 {
                    continue;
                }
                c.pseudo_impulse = new;
                apply_pseudo_impulse(views, c.a, c.b, c.ra, c.rb, c.normal * d);
            }
        }
    }
}

/// 对一对刚体施加世界冲量（作用于指定力臂）。
#[inline]
pub(crate) fn apply_impulse(
    views: &mut [BodyView],
    a: usize,
    b: usize,
    ra: Vec3,
    rb: Vec3,
    impulse: Vec3,
) {
    if !impulse.is_finite() {
        return;
    }
    if let Some(va) = views.get_mut(a) {
        if va.inv_m > 0.0 {
            va.lin -= impulse * va.inv_m;
            va.ang -= va.inv_i_world(ra.cross(impulse));
        }
    }
    if let Some(vb) = views.get_mut(b) {
        if vb.inv_m > 0.0 {
            vb.lin += impulse * vb.inv_m;
            vb.ang += vb.inv_i_world(rb.cross(impulse));
        }
    }
}

/// 伪冲量版本（只影响位置）。
#[inline]
pub(crate) fn apply_pseudo_impulse(
    views: &mut [BodyView],
    a: usize,
    b: usize,
    ra: Vec3,
    rb: Vec3,
    impulse: Vec3,
) {
    if !impulse.is_finite() {
        return;
    }
    if let Some(va) = views.get_mut(a) {
        if va.inv_m > 0.0 {
            va.pseudo_lin -= impulse * va.inv_m;
            va.pseudo_ang -= va.inv_i_world(ra.cross(impulse));
        }
    }
    if let Some(vb) = views.get_mut(b) {
        if vb.inv_m > 0.0 {
            vb.pseudo_lin += impulse * vb.inv_m;
            vb.pseudo_ang += vb.inv_i_world(rb.cross(impulse));
        }
    }
}

/// 速度积分：重力、外力、阻尼。
pub(crate) fn integrate_velocities<S: BodyStore>(bodies: &mut S, gravity: Vec3, dt: f32) {
    for i in 0..bodies.slot_count() {
        let b = match bodies.slot_mut(i) {
            Some(b) => b,
            None => continue,
        };
        if b.body_type != BodyType::Dynamic || b.sleeping {
            b.force = Vec3::ZERO;
            b.torque = Vec3::ZERO;
            continue;
        }
        let mut v = b.linear_velocity + (gravity * b.gravity_scale + b.force * b.inv_mass) * dt;
        let mut w = b.angular_velocity + b.apply_inv_inertia_world(b.torque) * dt;
        if b.linear_damping > 0.0 {
            v /= 1.0 + b.linear_damping * dt;
        }
        if b.angular_damping > 0.0 {
            w /= 1.0 + b.angular_damping * dt;
        }
        b.linear_velocity = v.clamp_length(MAX_LINEAR_SPEED);
        b.angular_velocity = w.clamp_length(MAX_ANGULAR_SPEED);
        b.force = Vec3::ZERO;
        b.torque = Vec3::ZERO;
    }
}

/// 位置积分：`x += (v + v_pseudo)·dt`，四元数用一阶导数并重新归一化。
pub(crate) fn integrate_positions<S: BodyStore>(bodies: &mut S, dt: f32) {
    for i in 0..bodies.slot_count() {
        let b = match bodies.slot_mut(i) {
            Some(b) => b,
            None => continue,
        };
        if b.body_type == BodyType::Static || b.sleeping {
            continue;
        }
        let v = b.linear_velocity + b.pseudo_linear_velocity;
        let w = b.angular_velocity + b.pseudo_angular_velocity;
        if v.is_finite() {
            b.transform.position += v * dt;
        }
        if w.is_finite() {
            let q = b.transform.rotation;
            let dq = Quat::new(w.x, w.y, w.z, 0.0) * q;
            let half = 0.5 * dt;
            b.transform.rotation = Quat::new(
                q.x + dq.x * half,
                q.y + dq.y * half,
                q.z + dq.z * half,
                q.w + dq.w * half,
            )
            .normalize_or_identity();
        }
        b.pseudo_linear_velocity = Vec3::ZERO;
        b.pseudo_angular_velocity = Vec3::ZERO;
        b.sanitize();
    }
}

/// 睡眠状态更新。
pub(crate) fn update_sleeping<S: BodyStore>(
    bodies: &mut S,
    dt: f32,
    cfg: &SolverConfig,
    islands: &crate::island::Islands,
) {
    if !cfg.sleep_enabled {
        for i in 0..bodies.slot_count() {
            if let Some(b) = bodies.slot_mut(i) {
                b.sleep_timer = 0.0;
                b.sleeping = false;
            }
        }
        return;
    }
    // 第一遍：逐体累计「足够慢」的时间。
    //
    // 注意这里**不直接入睡**——是否入睡由整个 island 统一决定（第二遍）。
    // 逐体入睡会留下「一半睡着、一半还在抖」的堆叠：醒着的那几个继续被求解，
    // 睡着的那几个被当成无限质量冻结，接触力传不下去，堆叠就会慢慢渗进去。
    for i in 0..bodies.slot_count() {
        let b = match bodies.slot_mut(i) {
            Some(b) => b,
            None => continue,
        };
        if b.body_type != BodyType::Dynamic {
            continue;
        }
        let slow = b.linear_velocity.length() < SLEEP_LINEAR_TOL
            && b.angular_velocity.length() < SLEEP_ANGULAR_TOL;
        if slow {
            b.sleep_timer += dt;
        } else {
            b.sleep_timer = 0.0;
            b.sleeping = false;
        }
    }
    // 第二遍：岛内**所有**成员都慢够了时间，整岛一起入睡。
    //
    // 「慢够了」用计时器判断：计时器到阈值就意味着它一直在阈值以下
    // （一旦超阈值计时器立刻清零）。`allow_sleep == false` 的刚体（例如玩家控制的
    // 角色）不阻止同岛的其他刚体入睡，但它自己不会被置为睡眠。
    for k in 0..islands.len() {
        let members = islands.bodies_of(k as u32);
        if members.is_empty() {
            continue;
        }
        let mut all_ready = true;
        for &slot in members {
            if let Some(b) = bodies.slot_mut(slot as usize) {
                if b.sleep_timer < SLEEP_TIME {
                    all_ready = false;
                }
            }
        }
        if all_ready {
            for &slot in members {
                if let Some(b) = bodies.slot_mut(slot as usize) {
                    if b.allow_sleep && !b.sleeping {
                        b.sleep();
                    }
                }
            }
        }
    }
    // 兜底：没有进入任何岛的动态刚体（正常情况不会有）仍然按逐体计时入睡，
    // 避免因为划分的边界情况让某个刚体永远醒着。
    for i in 0..bodies.slot_count() {
        if islands.island_of(i).is_some() {
            continue;
        }
        if let Some(b) = bodies.slot_mut(i) {
            if b.body_type == BodyType::Dynamic
                && b.allow_sleep
                && !b.sleeping
                && b.sleep_timer >= SLEEP_TIME
            {
                b.sleep();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::BodyBuilder;
    use crate::shape::Shape;

    fn box_body(y: f32) -> RigidBody {
        BodyBuilder::dynamic(Shape::Box {
            half_extents: Vec3::splat(0.5),
        })
        .position(vec3(0.0, y, 0.0))
        .build()
    }

    #[test]
    fn body_view_freezes_sleeping_bodies() {
        let mut b = box_body(0.0);
        b.sleeping = true;
        let v = BodyView::from_body(&b);
        assert_eq!(v.inv_m, 0.0);
        // 冻结体：世界逆惯量是零矩阵，任何冲量都推不动
        assert_eq!(v.inv_i_world(Vec3::new(1.0, 2.0, 3.0)), Vec3::ZERO);
        b.sleeping = false;
        let v2 = BodyView::from_body(&b);
        assert!(v2.inv_m > 0.0);
        assert!(v2.inv_i_world(Vec3::new(0.0, 1.0, 0.0)).length() > 0.0);
    }

    #[test]
    fn impulse_application_changes_velocity() {
        let bodies = [Some(box_body(0.0)), Some(box_body(1.0))];
        let mut views: Vec<BodyView> = bodies
            .iter()
            .map(|b| BodyView::from_body(b.as_ref().unwrap()))
            .collect();
        apply_impulse(
            &mut views,
            0,
            1,
            Vec3::ZERO,
            Vec3::ZERO,
            vec3(1.0, 0.0, 0.0),
        );
        assert!(views[0].lin.x < 0.0, "A 应被推向 -X");
        assert!(views[1].lin.x > 0.0, "B 应被推向 +X");
    }

    #[test]
    fn integration_keeps_quaternion_normalized() {
        let mut b = box_body(0.0);
        b.angular_velocity = vec3(1.0, 2.0, 3.0);
        let mut bodies = vec![Some(b)];
        for _ in 0..600 {
            integrate_positions(&mut bodies, 1.0 / 60.0);
        }
        let q = bodies[0].as_ref().unwrap().transform.rotation;
        assert!((q.length() - 1.0).abs() < 1e-4, "四元数模长 {}", q.length());
        assert!(q.is_finite());
    }

    /// 把一个刚体数组做成「每个刚体各自一个岛」的划分（单元测试用）。
    fn singleton_islands(bodies: &[Option<RigidBody>]) -> crate::island::Islands {
        let views: Vec<BodyView> = bodies.iter().flatten().map(BodyView::from_body).collect();
        let mut islands = crate::island::Islands::new();
        islands.begin(&views);
        islands.finish();
        islands
    }

    #[test]
    fn sleep_after_still_period() {
        let mut bodies = vec![Some(box_body(0.0))];
        let cfg = SolverConfig::default();
        for _ in 0..60 {
            let islands = singleton_islands(&bodies);
            update_sleeping(&mut bodies, 1.0 / 60.0, &cfg, &islands);
        }
        assert!(bodies[0].as_ref().unwrap().is_sleeping());
        bodies[0].as_mut().unwrap().apply_impulse(Vec3::Y);
        assert!(!bodies[0].as_ref().unwrap().is_sleeping());
    }
}

#[cfg(test)]
mod world_inv_inertia_tests {
    use super::*;

    /// 旧的实现方式：两次四元数旋转 + 逐分量乘对角。
    fn by_quat_formula(rot: Quat, local: Vec3, v: Vec3) -> Vec3 {
        let l = rot.inverse().mul_vec3(v) * local;
        rot.mul_vec3(l)
    }

    fn sample_quat(i: usize) -> Quat {
        // 确定性「伪随机」单位四元数：不用 rand，保证测试可复现
        let a = (i as f32 * 0.7).sin();
        let b = (i as f32 * 1.3).cos();
        let c = (i as f32 * 2.1).sin();
        Quat::from_axis_angle(Vec3::new(a, b, c).normalize_or_zero(), i as f32 * 0.37)
    }

    /// 预计算矩阵必须与「每次重建」的公式数值等价（差异只在 ULP 量级）。
    #[test]
    fn matrix_form_matches_quat_formula() {
        let mut worst: f32 = 0.0;
        for i in 0..500 {
            let rot = sample_quat(i);
            let local = Vec3::new(
                0.5 + (i % 7) as f32 * 0.3,
                0.4 + (i % 5) as f32 * 0.2,
                0.6 + (i % 3) as f32 * 0.4,
            );
            let m = world_inv_inertia(rot, local);
            for j in 0..9 {
                let v = Vec3::new(
                    (i * 3 + j) as f32 * 0.11 - 1.0,
                    (i * 5 + j) as f32 * 0.07 - 0.5,
                    (i * 7 + j) as f32 * 0.13 - 0.8,
                );
                let a = m.mul_vec3(v);
                let b = by_quat_formula(rot, local, v);
                let scale = b.length().max(1e-3);
                worst = worst.max((a - b).length() / scale);
            }
        }
        println!("矩阵形式与四元数公式的最大相对偏差: {worst:e}");
        assert!(
            worst < 1e-4,
            "矩阵形式与四元数公式的最大相对偏差 {worst:e} 过大"
        );
    }

    /// 冻结体必须是零矩阵（无限惯量），否则睡眠体会被冲量推动。
    #[test]
    fn frozen_view_has_zero_inertia() {
        let v = BodyView::frozen();
        assert!(v.inv_i_world(Vec3::new(1.0, 2.0, 3.0)).length() == 0.0);
    }
}
