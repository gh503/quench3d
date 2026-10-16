//! 查询加速结构：均匀空间哈希网格（**快照式**）。
//!
//! 为什么要有它：[`PhysicsWorld`] 上的查询是 O(N) 线性扫描（每个刚体先做 AABB
//! 粗筛、再做精确求交）。单次查询这样最省事也最不容易错，但"一帧里打几百条射线"
//! （AI 视线、拾取、贴地检测）就会被 N 拖住。`QueryIndex` 把每个刚体的 AABB
//! 铺进和宽相同一套网格，查询只测射线/形状**实际经过的单元**里的刚体。
//!
//! 两条硬约束（写在这里，免得用错）：
//!
//! * **它是快照**。索引记的是建的时候的 AABB 与刚体集合。世界动过
//!   （[`PhysicsWorld::version`] 变了）就是过期的——此时查询方法会**自动回退到
//!   线性扫描**，结果永远正确，只是不快；[`QueryIndex::is_stale`] 让你自己判断
//!   要不要重建。想让索引一直有效，就"每帧重建一次、帧内查很多次"。
//! * **结果与线性路径逐位一致**：同一批射线/形状，索引路径与
//!   [`PhysicsWorld::raycast_all`] / [`PhysicsWorld::overlap_shape`] 返回相同的
//!   句柄与距离，排序规则也一样（距离升序，平局按槽位下标）。有专门的用例把
//!   两条路径逐条对比。
//!
//! 典型用法：
//!
//! ```
//! use quench3d_math::vec3;
//! use quench3d_physics::{BodyBuilder, PhysicsWorld, QueryFilter, Shape};
//!
//! let mut world = PhysicsWorld::with_gravity(vec3(0.0, -9.81, 0.0));
//! world.add_body(BodyBuilder::r#static(Shape::Sphere { radius: 1.0 }).build());
//! world.step(1.0 / 60.0);
//!
//! let index = world.build_query_index(); // O(N)，一帧一次
//! let filter = QueryFilter::default();
//! let hit = index.raycast(&world, vec3(0.0, 0.0, 5.0), vec3(0.0, 0.0, -1.0), 100.0, filter);
//! assert!(hit.is_some());
//! // 与线性扫描结果一致
//! assert_eq!(
//!     hit.map(|h| h.handle),
//!     world.raycast(vec3(0.0, 0.0, 5.0), vec3(0.0, 0.0, -1.0), 100.0).map(|h| h.handle),
//! );
//! ```

use quench3d_math::*;

use crate::body::{BodyHandle, RigidBody};
use crate::broadphase::{pick_cell_size, spread_into_cells};
use crate::query::{self, QueryFilter};
use crate::shape::Shape;
use crate::world::{PhysicsWorld, RaycastHit};

/// 网格走查（DDA）的安全上限：射线太长而单元太小时不至于卡住。
const MAX_DDA_STEPS: usize = 100_000;

/// 一次射线查询的结果，外加"花了多少功夫"（诊断与测试用）。
#[derive(Clone, Debug, Default)]
pub struct RaycastHits {
    /// 命中，按距离升序（平局按槽位下标）。
    pub hits: Vec<RaycastHit>,
    /// 精确求交前逐个检查过的候选刚体数（索引路径远小于刚体总数）。
    pub tested: usize,
    /// 是否因为索引过期而回退到线性扫描。
    pub fallback: bool,
}

/// 一次重叠查询的结果，外加"花了多少功夫"。
#[derive(Clone, Debug, Default)]
pub struct OverlapHits {
    /// 命中的刚体，按槽位下标升序。
    pub handles: Vec<BodyHandle>,
    /// 精确求交前逐个检查过的候选刚体数。
    pub tested: usize,
    /// 是否因为索引过期而回退到线性扫描。
    pub fallback: bool,
}

/// 查询加速结构（快照）。用 [`PhysicsWorld::build_query_index`] 生成。
#[derive(Clone, Debug, Default)]
pub struct QueryIndex {
    /// 单元边长（与宽相同一套策略：AABB 最长边 25 分位 × 2）。
    cell_size: f32,
    /// 排序去重后的单元键。
    keys: Vec<(i32, i32, i32)>,
    /// 每个单元键在 `items` 中的区间 `[start, end)`。
    ranges: Vec<(u32, u32)>,
    /// 展平的刚体句柄，按单元键分组（单元键在 `keys`/`ranges` 里）。
    items: Vec<BodyHandle>,
    /// 全局代理：半空间与超大物体（不在网格里，每次查询都测）。
    globals: Vec<BodyHandle>,
    /// 建索引时世界的版本号（用来判断过期）。
    version: u64,
    /// 参与索引的刚体数。
    bodies: usize,
}

impl QueryIndex {
    /// 建索引：O(N)，只做 AABB 铺格子，不碰精确几何。
    pub fn build(world: &PhysicsWorld) -> Self {
        let mut index = Self {
            version: world.version(),
            ..Default::default()
        };
        let mut sizes: Vec<f32> = Vec::new();
        let mut spread: Vec<(Aabb, BodyHandle)> = Vec::new();
        for (handle, body) in world.bodies() {
            index.bodies += 1;
            if matches!(body.shape, Shape::Plane { .. }) {
                index.globals.push(handle);
                continue;
            }
            let aabb = body
                .shape
                .local_aabb()
                .transformed_conservative(&body.transform.matrix());
            if !aabb.is_valid() {
                // 坐标是 NaN 的刚体铺不进格子；当全局代理处理，
                // "宁可贵一点也不能漏"（它的精确求交自己会拒绝）。
                index.globals.push(handle);
                continue;
            }
            let s = aabb.size();
            let m = s.x.max(s.y).max(s.z);
            if m.is_finite() && m > 1e-6 {
                sizes.push(m);
            }
            spread.push((aabb, handle));
        }
        index.cell_size = pick_cell_size(&mut sizes);

        let mut flat: Vec<((i32, i32, i32), BodyHandle)> = Vec::new();
        for (aabb, handle) in spread {
            if !spread_into_cells(&aabb, index.cell_size, handle, &mut flat) {
                index.globals.push(handle);
            }
        }
        flat.sort_unstable_by_key(|(k, h)| (*k, h.index, h.generation));

        // 压缩成「键 + 区间 + 展平项」（与宽相同一套结构）
        let mut k = 0usize;
        while k < flat.len() {
            let key = flat[k].0;
            let start = index.items.len() as u32;
            while k < flat.len() && flat[k].0 == key {
                index.items.push(flat[k].1);
                k += 1;
            }
            index.keys.push(key);
            index.ranges.push((start, index.items.len() as u32));
        }
        index
    }

    /// 参与索引的刚体数（含全局代理）。
    pub fn len(&self) -> usize {
        self.bodies
    }

    /// 没有任何刚体。
    pub fn is_empty(&self) -> bool {
        self.bodies == 0
    }

    /// 单元边长。
    pub fn cell_size(&self) -> f32 {
        self.cell_size
    }

    /// 非空单元数。
    pub fn cells(&self) -> usize {
        self.keys.len()
    }

    /// 全局代理数（半空间 + 超大物体）。
    pub fn globals(&self) -> usize {
        self.globals.len()
    }

    /// 索引记下的世界版本号。
    pub fn version(&self) -> u64 {
        self.version
    }

    /// 世界动过之后索引就过期了；过期的查询会自动回退到线性扫描。
    pub fn is_stale(&self, world: &PhysicsWorld) -> bool {
        self.version != world.version()
    }

    /// 射线检测，返回最近的一条。
    pub fn raycast(
        &self,
        world: &PhysicsWorld,
        origin: Vec3,
        dir: Vec3,
        max_distance: f32,
        filter: QueryFilter,
    ) -> Option<RaycastHit> {
        self.raycast_detailed(world, origin, dir, max_distance, filter, false)
            .hits
            .into_iter()
            .next()
    }

    /// 射线检测，返回 `max_distance` 内的**所有**命中（按距离升序）。
    pub fn raycast_all(
        &self,
        world: &PhysicsWorld,
        origin: Vec3,
        dir: Vec3,
        max_distance: f32,
        filter: QueryFilter,
    ) -> Vec<RaycastHit> {
        self.raycast_detailed(world, origin, dir, max_distance, filter, true)
            .hits
    }

    /// 射线检测 + 诊断信息。`all == false` 时只需要最近的一条（会随命中收紧走查范围）。
    pub fn raycast_detailed(
        &self,
        world: &PhysicsWorld,
        origin: Vec3,
        dir: Vec3,
        max_distance: f32,
        filter: QueryFilter,
        all: bool,
    ) -> RaycastHits {
        let mut out = RaycastHits::default();
        let d = dir.normalize_or_zero();
        if !origin.is_finite()
            || !dir.is_finite()
            || !max_distance.is_finite()
            || max_distance <= 0.0
            || d.length_squared() < 0.5
        {
            return out;
        }
        if self.is_stale(world) {
            out.fallback = true;
            if all {
                out.hits = query::raycast_all_filtered(world, origin, d, max_distance, filter);
            } else if let Some(hit) = world.raycast_filtered(origin, d, max_distance, |h| {
                world.body(h).is_some_and(|b| filter.accepts(b))
            }) {
                out.hits.push(hit);
            }
            out.tested = world.bodies().count();
            return out;
        }

        let mut hits: Vec<RaycastHit> = Vec::new();
        let mut tested = 0usize;

        // 全局代理（半空间 / 超大物体）不在网格里，任何射线都可能撞上
        for i in 0..self.globals.len() {
            let handle = self.globals[i];
            if let Some(body) = world.body(handle) {
                if filter.accepts(body) {
                    ray_candidate(
                        handle,
                        body,
                        origin,
                        d,
                        max_distance,
                        &mut hits,
                        &mut tested,
                    );
                }
            }
        }

        // 3D DDA：沿射线走格子，只测经过的单元
        let cs = self.cell_size;
        let inv = 1.0 / cs;
        let mut cell = [
            (origin.x * inv).floor() as i32,
            (origin.y * inv).floor() as i32,
            (origin.z * inv).floor() as i32,
        ];
        let o = [origin.x, origin.y, origin.z];
        let dd = [d.x, d.y, d.z];
        let mut step = [0i32; 3];
        let mut t_max = [f32::INFINITY; 3];
        let mut t_delta = [f32::INFINITY; 3];
        for k in 0..3 {
            if dd[k] > 1e-12 {
                step[k] = 1;
                t_delta[k] = cs / dd[k];
                t_max[k] = ((cell[k] as f32 + 1.0) * cs - o[k]) / dd[k];
            } else if dd[k] < -1e-12 {
                step[k] = -1;
                t_delta[k] = -cs / dd[k];
                t_max[k] = (cell[k] as f32 * cs - o[k]) / dd[k];
            }
        }

        let mut limit = max_distance;
        for _ in 0..MAX_DDA_STEPS {
            if let Some((start, end)) = self.cell_range(cell) {
                for item in &self.items[start as usize..end as usize] {
                    let handle = *item;
                    let Some(body) = world.body(handle) else {
                        continue;
                    };
                    if !filter.accepts(body) {
                        continue;
                    }
                    ray_candidate(
                        handle,
                        body,
                        origin,
                        d,
                        max_distance,
                        &mut hits,
                        &mut tested,
                    );
                }
            }
            if !all {
                // 最近的一条：命中之后走查范围收紧到它，后面的格子不用看了
                if let Some(best) = hits.iter().map(|h| h.distance).reduce(f32::min) {
                    limit = limit.min(best);
                }
            }
            // 下一个格子：选 `t_max` 最小的那一轴
            let (axis, t_next) = if t_max[0] <= t_max[1] && t_max[0] <= t_max[2] {
                (0, t_max[0])
            } else if t_max[1] <= t_max[2] {
                (1, t_max[1])
            } else {
                (2, t_max[2])
            };
            // NaN 也停（理论上不该出现；宁可少走一步也不要空转）
            if step[axis] == 0 || t_next.is_nan() || t_next > limit {
                break;
            }
            cell[axis] += step[axis];
            t_max[axis] = t_next + t_delta[axis];
        }

        hits.sort_by(|a, b| {
            a.distance
                .partial_cmp(&b.distance)
                .unwrap_or(core::cmp::Ordering::Equal)
                .then_with(|| a.handle.index().cmp(&b.handle.index()))
        });
        if all {
            // 同一个刚体可能落在多个单元里（大 AABB）：排序后同 (距离, 句柄) 的条目相邻
            hits.dedup_by_key(|h| (h.handle.index(), h.handle.generation));
        } else {
            hits.truncate(1);
        }
        out.hits = hits;
        out.tested = tested;
        out
    }

    /// 任意形状重叠查询（结果按槽位下标升序，与线性路径一致）。
    pub fn overlap_shape(
        &self,
        world: &PhysicsWorld,
        shape: &Shape,
        transform: &Transform,
        filter: QueryFilter,
    ) -> Vec<BodyHandle> {
        self.overlap_shape_detailed(world, shape, transform, filter)
            .handles
    }

    /// 任意形状重叠查询 + 诊断信息。
    pub fn overlap_shape_detailed(
        &self,
        world: &PhysicsWorld,
        shape: &Shape,
        transform: &Transform,
        filter: QueryFilter,
    ) -> OverlapHits {
        let mut out = OverlapHits::default();
        if !transform.position.is_finite() || !transform.rotation.is_finite() {
            return out;
        }
        if self.is_stale(world) {
            out.fallback = true;
            out.handles = query::overlap_shape_filtered(world, shape, transform, filter);
            out.tested = world.bodies().count();
            return out;
        }

        let probe_aabb = shape
            .local_aabb()
            .transformed_conservative(&transform.matrix());
        let mut handles: Vec<BodyHandle> = Vec::new();
        let mut tested = 0usize;

        // 全局代理（半空间 / 超大物体）
        for i in 0..self.globals.len() {
            overlap_candidate(
                world,
                shape,
                transform,
                &probe_aabb,
                self.globals[i],
                &mut handles,
                &mut tested,
            );
        }

        // 探针 AABB 覆盖的单元
        if probe_aabb.is_valid() {
            let inv = 1.0 / self.cell_size;
            let lo = probe_aabb.min * inv;
            let hi = probe_aabb.max * inv;
            let lo = [
                lo.x.floor() as i32,
                lo.y.floor() as i32,
                lo.z.floor() as i32,
            ];
            let hi = [
                hi.x.floor() as i32,
                hi.y.floor() as i32,
                hi.z.floor() as i32,
            ];
            // 单元数上限：探针特别大（或单元特别小）时不至于扫爆，
            // 超了就退回线性扫描（正确性优先）
            let span = (hi[0] as i64 - lo[0] as i64 + 1)
                .saturating_mul(hi[1] as i64 - lo[1] as i64 + 1)
                .saturating_mul(hi[2] as i64 - lo[2] as i64 + 1);
            if span > MAX_OVERLAP_CELLS {
                out.fallback = true;
                out.handles = query::overlap_shape_filtered(world, shape, transform, filter);
                out.tested = world.bodies().count();
                return out;
            }
            for x in lo[0]..=hi[0] {
                for y in lo[1]..=hi[1] {
                    for z in lo[2]..=hi[2] {
                        if let Some((start, end)) = self.cell_range([x, y, z]) {
                            for item in &self.items[start as usize..end as usize] {
                                let handle = *item;
                                let Some(body) = world.body(handle) else {
                                    continue;
                                };
                                if !filter.accepts(body) {
                                    continue;
                                }
                                overlap_candidate(
                                    world,
                                    shape,
                                    transform,
                                    &probe_aabb,
                                    handle,
                                    &mut handles,
                                    &mut tested,
                                );
                            }
                        }
                    }
                }
            }
        }

        // 同一刚体可能被多个单元覆盖：去重（保留槽位下标最小的那个句柄）后按下标排序
        handles.sort_unstable_by_key(|h| (h.index, h.generation));
        handles.dedup_by_key(|h| h.index);
        out.handles = handles;
        out.tested = tested;
        out
    }

    /// 单元键 → `items` 区间（二分）。
    #[inline]
    fn cell_range(&self, key: [i32; 3]) -> Option<(u32, u32)> {
        let key = (key[0], key[1], key[2]);
        self.keys.binary_search(&key).ok().map(|i| self.ranges[i])
    }
}

/// 重叠查询最多扫多少个单元（超了退回线性扫描）。
const MAX_OVERLAP_CELLS: i64 = 4096;

/// 射线候选：AABB 粗筛 + 精确求交，命中就写进 `hits`。
fn ray_candidate(
    handle: BodyHandle,
    body: &RigidBody,
    origin: Vec3,
    d: Vec3,
    max_distance: f32,
    hits: &mut Vec<RaycastHit>,
    tested: &mut usize,
) {
    *tested += 1;
    if !matches!(body.shape, Shape::Plane { .. }) {
        let aabb = body
            .shape
            .local_aabb()
            .transformed_conservative(&body.transform.matrix());
        match aabb.intersect_ray(origin, d) {
            Some(t) if t <= max_distance => {}
            _ => return,
        }
    }
    if let Some((t, n)) = query::ray_shape(&body.shape, &body.transform, origin, d, max_distance) {
        if t.is_finite() && t >= 0.0 && t <= max_distance {
            let normal = if n.dot(d) > 0.0 { -n } else { n };
            hits.push(RaycastHit {
                handle,
                point: origin + d * t,
                normal,
                distance: t,
            });
        }
    }
}

/// 重叠候选：AABB 粗筛 + 精确距离，相交就写进 `handles`。
fn overlap_candidate(
    world: &PhysicsWorld,
    shape: &Shape,
    transform: &Transform,
    probe_aabb: &Aabb,
    handle: BodyHandle,
    handles: &mut Vec<BodyHandle>,
    tested: &mut usize,
) {
    let Some(body) = world.body(handle) else {
        return;
    };
    *tested += 1;
    if !matches!(body.shape, Shape::Plane { .. }) && probe_aabb.is_valid() {
        let aabb = body
            .shape
            .local_aabb()
            .transformed_conservative(&body.transform.matrix());
        if !aabb.intersects(probe_aabb) {
            return;
        }
    }
    if query::distance_to_body(shape, transform, body).distance <= query::TOUCH {
        handles.push(handle);
    }
}
