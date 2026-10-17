//! 场景查询：球形重叠、射线检测、形状投射。
//!
//! * 射线检测对球 / 盒 / 半空间 / 胶囊 / 圆柱 / 三角网格使用解析求交，
//!   凸包用「球体步进」（sphere tracing，用 GJK 点距离，保证不穿透）；
//! * 形状投射使用**保守推进**：每步前进当前最小距离，因此绝不会穿过障碍物；
//! * 全部按槽位序遍历，取最近命中，平局取下标小者 → 结果确定。

use quench3d_math::*;

use crate::body::{BodyHandle, RigidBody};
use crate::gjk::{self, Support};
use crate::shape::Shape;
use crate::world::{PhysicsWorld, RaycastHit};

/// 判定「接触」的距离阈值。
pub(crate) const TOUCH: f32 = 1e-4;
/// 形状投射的最大推进次数。
const MAX_CAST_ITERATIONS: usize = 64;
/// 球体步进的最大次数。
const MAX_MARCH_ITERATIONS: usize = 48;

/// 距离查询结果：查询形状上的点、对方形状上的点、距离、A→B 法线。
#[derive(Clone, Copy, Debug)]
pub(crate) struct DistanceResult {
    pub(crate) point_a: Vec3,
    pub(crate) point_b: Vec3,
    pub(crate) distance: f32,
    pub(crate) normal: Vec3,
}

/// 查询形状与单个刚体的距离（半空间解析处理，其余走 GJK）。
///
/// 复合形状**逐部件**求距离，取最近的那一组；单形状走老路径（只有一个部件）。
pub(crate) fn distance_to_body(shape: &Shape, t: &Transform, body: &RigidBody) -> DistanceResult {
    if body.extra_parts.is_empty() {
        return distance_to_shape(shape, t, &body.shape, &body.transform);
    }
    let mut best: Option<DistanceResult> = None;
    for (part, local) in body.parts() {
        let world = body.part_world_transform(&local);
        let d = distance_to_shape(shape, t, part, &world);
        if best.as_ref().is_none_or(|b| d.distance < b.distance) {
            best = Some(d);
        }
    }
    best.unwrap_or_else(|| distance_to_shape(shape, t, &body.shape, &body.transform))
}

/// 射线与**一个刚体**求交：逐部件求交，取最近的命中。
///
/// 单形状走老路径（一个部件），行为与以前一致。
pub(crate) fn ray_body(
    body: &RigidBody,
    origin: Vec3,
    d: Vec3,
    max_distance: f32,
) -> Option<(f32, Vec3)> {
    if body.extra_parts.is_empty() {
        return ray_shape(&body.shape, &body.transform, origin, d, max_distance);
    }
    let mut best: Option<(f32, Vec3)> = None;
    for (part, local) in body.parts() {
        let world = body.part_world_transform(&local);
        if let Some((t, n)) = ray_shape(part, &world, origin, d, max_distance) {
            if t.is_finite() && t >= 0.0 && best.as_ref().is_none_or(|b| t < b.0) {
                best = Some((t, n));
            }
        }
    }
    best
}

/// 查询形状与"某个形状 + 世界变换"的距离。
fn distance_to_shape(
    shape: &Shape,
    t: &Transform,
    target: &Shape,
    target_t: &Transform,
) -> DistanceResult {
    if let Shape::Plane { normal } = target {
        let n = target_t
            .rotation
            .mul_vec3(normal.normalize_or_zero())
            .normalize_or_zero();
        let sup = Support::new(shape, t);
        let p = sup.support(-n);
        let sd = n.dot(p - target_t.position);
        return DistanceResult {
            point_a: p,
            point_b: p - n * sd,
            distance: sd.max(0.0),
            // A→B 方向为 -n（B 是半空间）。
            normal: -n,
        };
    }
    let (pa, pb, d, n) = gjk::closest_points(shape, t, target, target_t);
    DistanceResult {
        point_a: pa,
        point_b: pb,
        distance: d,
        normal: if n.length_squared() > 0.5 { n } else { Vec3::Y },
    }
}

/// 查询过滤：**单向**判据（与物理碰撞的对称判据不同）。
///
/// 物理那边要"双方都愿意才碰"（`A.memberships & B.filter` 且 `B.memberships & A.filter`）；
/// 查询是"我问你答"，只有提问的一方有立场，所以只看被查的刚体：
/// `body.memberships & mask != 0` 就参与。
///
/// 缺省是"所有层、含传感器"——不设置就等于没有过滤，与老行为一致。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryFilter {
    /// 层掩码：只查**成员层**与之相交的刚体（缺省 `u32::MAX`）。
    pub mask: u32,
    /// 是否把传感器（触发区）算作可命中（缺省 `true`——触发区也是几何体，
    /// 想知道"这条射线经不经过拾取区"是常见需求）。
    pub sensors: bool,
}

impl Default for QueryFilter {
    #[inline]
    fn default() -> Self {
        Self {
            mask: u32::MAX,
            sensors: true,
        }
    }
}

impl QueryFilter {
    /// 只查这些层。
    #[inline]
    pub const fn mask(mut self, mask: u32) -> Self {
        self.mask = mask;
        self
    }

    /// 是否把传感器算作可命中。
    #[inline]
    pub const fn sensors(mut self, sensors: bool) -> Self {
        self.sensors = sensors;
        self
    }

    /// 这个刚体参不参与本次查询。
    #[inline]
    pub fn accepts(&self, body: &RigidBody) -> bool {
        (body.groups.memberships & self.mask) != 0 && (self.sensors || !body.sensor)
    }
}

/// 球形重叠查询：返回与球体相交（或相切）的所有刚体句柄。
pub fn overlap_sphere(world: &PhysicsWorld, center: Vec3, radius: f32) -> Vec<BodyHandle> {
    overlap_shape_filtered(
        world,
        &Shape::Sphere { radius },
        &Transform::from_translation(center),
        QueryFilter::default(),
    )
}

/// 任意形状重叠查询：返回与 `shape`（放在 `transform`）相交或相切的所有刚体句柄。
///
/// 按槽位下标升序（与线性扫描的遍历顺序一致），结果确定。
pub fn overlap_shape_filtered(
    world: &PhysicsWorld,
    shape: &Shape,
    transform: &Transform,
    filter: QueryFilter,
) -> Vec<BodyHandle> {
    let mut out = Vec::new();
    if !transform.position.is_finite() || !transform.rotation.is_finite() {
        return out;
    }
    let probe_aabb = shape
        .local_aabb()
        .transformed_conservative(&transform.matrix());
    for (handle, body) in world.bodies() {
        if !filter.accepts(body) {
            continue;
        }
        // 半空间没有有限 AABB（`world_aabb` 无效），只能直接算距离
        // （`distance_to_body` 对它是解析的）。复合形状的 `world_aabb` 已经是并集。
        if probe_aabb.is_valid() && body.world_aabb().is_valid() {
            let aabb = body.world_aabb();
            if !aabb.intersects(&probe_aabb) {
                continue;
            }
        }
        if distance_to_body(shape, transform, body).distance <= TOUCH {
            out.push(handle);
        }
    }
    out
}

/// 两个形状之间的最近距离（查询形状 → 刚体）。
#[derive(Clone, Copy, Debug)]
pub struct ShapeDistance {
    /// 最近距离（相切或相交时为 0）。
    pub distance: f32,
    /// 查询形状上的点。
    pub point: Vec3,
    /// 对方刚体上的点。
    pub other_point: Vec3,
    /// 查询形状 → 刚体的单位法线。
    pub normal: Vec3,
}

/// 查询形状与某个刚体的最近距离（刚体句柄失效时返回 `None`）。
pub fn distance_to_body_handle(
    world: &PhysicsWorld,
    shape: &Shape,
    transform: &Transform,
    handle: BodyHandle,
) -> Option<ShapeDistance> {
    let body = world.body(handle)?;
    if !transform.position.is_finite() || !transform.rotation.is_finite() {
        return None;
    }
    let r = distance_to_body(shape, transform, body);
    Some(ShapeDistance {
        distance: r.distance,
        point: r.point_a,
        other_point: r.point_b,
        normal: r.normal,
    })
}

/// 射线检测，返回**所有**在 `max_distance` 内命中的刚体（按距离升序，平局按槽位下标）。
///
/// 与 [`PhysicsWorld::raycast`] 的区别：那个只给最近的一条。做"这条线上都有什么"
/// （视线遮挡、子弹穿透、agent 观察场景）时需要全部。
pub fn raycast_all_filtered(
    world: &PhysicsWorld,
    origin: Vec3,
    dir: Vec3,
    max_distance: f32,
    filter: QueryFilter,
) -> Vec<RaycastHit> {
    let mut out = Vec::new();
    if !origin.is_finite() || !dir.is_finite() || !max_distance.is_finite() || max_distance <= 0.0 {
        return out;
    }
    let d = dir.normalize_or_zero();
    if d.length_squared() < 0.5 {
        return out;
    }
    for (handle, body) in world.bodies() {
        if !filter.accepts(body) {
            continue;
        }
        let aabb = body.world_aabb();
        if aabb.is_valid() {
            match aabb.intersect_ray(origin, d) {
                Some(t) if t <= max_distance => {}
                _ => continue,
            }
        }
        if let Some((t, n)) = ray_body(body, origin, d, max_distance) {
            if t.is_finite() && t >= 0.0 && t <= max_distance {
                let normal = if n.dot(d) > 0.0 { -n } else { n };
                out.push(RaycastHit {
                    handle,
                    point: origin + d * t,
                    normal,
                    distance: t,
                });
            }
        }
    }
    // 距离升序；完全同距时按槽位下标（`raycast` 取第一个，于是两者一致）。
    out.sort_by(|a, b| {
        a.distance
            .partial_cmp(&b.distance)
            .unwrap_or(core::cmp::Ordering::Equal)
            .then_with(|| a.handle.index().cmp(&b.handle.index()))
    });
    out
}

/// 形状投射（保守推进）：`shape` 位于 `from`，沿 `motion` 平移，返回首次接触。
pub fn shape_cast(
    world: &PhysicsWorld,
    shape: &Shape,
    from: &Transform,
    motion: Vec3,
) -> Option<RaycastHit> {
    shape_cast_world(world, shape, from, motion)
}

/// 世界内部使用的带过滤射线检测。
pub(crate) fn raycast_world(
    world: &PhysicsWorld,
    origin: Vec3,
    dir: Vec3,
    max_distance: f32,
    filter: &dyn Fn(BodyHandle) -> bool,
) -> Option<RaycastHit> {
    if !origin.is_finite() || !dir.is_finite() || !max_distance.is_finite() || max_distance <= 0.0 {
        return None;
    }
    let d = dir.normalize_or_zero();
    if d.length_squared() < 0.5 {
        return None;
    }
    let mut best: Option<RaycastHit> = None;
    for (handle, body) in world.bodies() {
        if !filter(handle) {
            continue;
        }
        let aabb = body.world_aabb();
        if aabb.is_valid() {
            match aabb.intersect_ray(origin, d) {
                Some(t) if t <= max_distance => {}
                _ => continue,
            }
        }
        if let Some((t, n)) = ray_body(body, origin, d, max_distance) {
            if t.is_finite() && t >= 0.0 && t <= max_distance {
                let better = match &best {
                    Some(b) => t < b.distance,
                    None => true,
                };
                if better {
                    let normal = if n.dot(d) > 0.0 { -n } else { n };
                    best = Some(RaycastHit {
                        handle,
                        point: origin + d * t,
                        normal,
                        distance: t,
                    });
                }
            }
        }
    }
    best
}

/// 世界内部使用的形状投射（保守推进）。
pub(crate) fn shape_cast_world(
    world: &PhysicsWorld,
    shape: &Shape,
    from: &Transform,
    motion: Vec3,
) -> Option<RaycastHit> {
    shape_cast_world_filtered(world, shape, from, motion, &|_, _| true)
}

/// 带过滤的形状扫掠（CCD 用：必须能排除"自己"）。
///
/// `filter(句柄, 刚体)` 返回 `false` 的刚体完全不参与这次扫掠——
/// 自扫掠不排除自己的话，第一步就会在距离 0 处"命中自己"。
pub(crate) fn shape_cast_world_filtered(
    world: &PhysicsWorld,
    shape: &Shape,
    from: &Transform,
    motion: Vec3,
    filter: &dyn Fn(BodyHandle, &RigidBody) -> bool,
) -> Option<RaycastHit> {
    if !motion.is_finite() || matches!(shape, Shape::Plane { .. }) {
        return None;
    }
    let total = motion.length();
    if total < TOUCH || !from.position.is_finite() {
        return None;
    }
    let dir = motion / total;
    let mut travelled = 0.0f32;
    for _ in 0..MAX_CAST_ITERATIONS {
        let cur = Transform::new(from.position + dir * travelled, from.rotation, Vec3::ONE);
        let mut min_d = total - travelled;
        let mut closest: Option<(BodyHandle, DistanceResult)> = None;
        for (handle, body) in world.bodies() {
            if !filter(handle, body) {
                continue;
            }
            let r = distance_to_body(shape, &cur, body);
            if r.distance <= TOUCH {
                closest = Some((handle, r));
                min_d = 0.0;
                break;
            }
            if r.distance < min_d {
                min_d = r.distance;
                closest = Some((handle, r));
            }
        }
        if min_d <= TOUCH {
            if let Some((handle, r)) = closest {
                let delta = r.point_a - r.point_b;
                let normal = if delta.length_squared() > 1e-12 {
                    delta.normalize_or_zero()
                } else if r.normal.length_squared() > 0.5 {
                    -r.normal
                } else {
                    -dir
                };
                let normal = if normal.dot(dir) > 0.0 {
                    -normal
                } else {
                    normal
                };
                return Some(RaycastHit {
                    handle,
                    point: r.point_b,
                    normal,
                    distance: travelled,
                });
            }
            return None;
        }
        // 保守前进（略微收缩，避免浮点越过表面）。
        travelled += min_d * 0.999;
        if travelled >= total {
            return None;
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 射线 - 形状
// ---------------------------------------------------------------------------

/// 射线与形状求交，返回 `(距离, 外观法线)`。
pub(crate) fn ray_shape(
    shape: &Shape,
    t: &Transform,
    origin: Vec3,
    dir: Vec3,
    max_distance: f32,
) -> Option<(f32, Vec3)> {
    match shape {
        Shape::Sphere { radius } => {
            let ray = Ray::new(origin, dir);
            let tt = ray.intersect_sphere(t.position, radius.abs())?;
            if tt > max_distance {
                return None;
            }
            let n = (ray.point_at(tt) - t.position).normalize_or_zero();
            Some((tt, n))
        }
        Shape::Box { half_extents } => ray_box(*half_extents, t, origin, dir, max_distance),
        Shape::Plane { normal } => {
            let n = t.rotation.mul_vec3(normal.normalize_or_zero());
            let denom = n.dot(dir);
            if denom.abs() < 1e-9 {
                return None;
            }
            let tt = n.dot(t.position - origin) / denom;
            if tt < 0.0 || tt > max_distance {
                return None;
            }
            Some((tt, n))
        }
        Shape::Capsule {
            half_height,
            radius,
        } => ray_capsule(*half_height, *radius, t, origin, dir, max_distance),
        Shape::Cylinder {
            half_height,
            radius,
        } => ray_cylinder(*half_height, *radius, t, origin, dir, max_distance),
        Shape::Cone {
            half_height,
            radius,
        } => ray_cone(*half_height, *radius, t, origin, dir, max_distance),
        Shape::TriMesh { vertices, indices } => {
            let ray = Ray::new(origin, dir);
            let mut best: Option<(f32, Vec3)> = None;
            for tri in indices.as_chunks::<3>().0 {
                let (ia, ib, ic) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
                if ia >= vertices.len() || ib >= vertices.len() || ic >= vertices.len() {
                    continue;
                }
                let a = t.transform_point(vertices[ia]);
                let b = t.transform_point(vertices[ib]);
                let c = t.transform_point(vertices[ic]);
                if let Some(tt) = ray.intersect_triangle(a, b, c) {
                    if tt <= max_distance && best.map(|x| tt < x.0).unwrap_or(true) {
                        let n = (b - a).cross(c - a).normalize_or_zero();
                        best = Some((tt, n));
                    }
                }
            }
            best
        }
        Shape::ConvexHull { points } => {
            if points.is_empty() {
                return None;
            }
            ray_convex_hull(shape, t, origin, dir, max_distance)
        }
        // 圆角盒是凸的且有支持函数：直接走同一条"球体追踪 + GJK 最近点"的路径
        // （它只要求形状会算支持函数，所以无需为圆角盒另写一套解析求交）。
        Shape::RoundedBox {
            half_extents,
            radius,
        } => ray_rounded_box(*half_extents, *radius, t, origin, dir, max_distance),
    }
}

/// 射线 - 圆角盒（局部空间，用**解析 SDF** 做球体步进）。
///
/// 为什么不用 `ray_convex_hull` 那条 GJK 球体步进：GJK 的"点-形状最近点"在
/// **点位于形状内部**时不返回 0（返回的是单纯形上某个顶点的距离 —— 实测一个
/// 稍微陷进圆角盒的点会得到 0.57），而球体步进一旦踩进内部就会跳过头。
/// 圆角盒的 SDF 是闭式的（`dist(p, 内盒) - r`，1-Lipschitz），步进永不穿透，
/// 落进内部时再做一次二分即可。
fn ray_rounded_box(
    half_extents: Vec3,
    radius: f32,
    t: &Transform,
    origin: Vec3,
    dir: Vec3,
    max_distance: f32,
) -> Option<(f32, Vec3)> {
    let he = half_extents.abs();
    let r = radius.abs().min(he.min_element());
    let inner = (he - Vec3::splat(r)).max(Vec3::ZERO);
    let o = t.inverse_transform_point(origin);
    let dl = t.rotation.inverse().mul_vec3(dir);
    let len = dl.length();
    if len < 1e-9 {
        return None;
    }
    let d = dl / len;
    let sdf = |p: Vec3| (p - p.clamp(-inner, inner)).length() - r;
    let normal_at = |p: Vec3| {
        let n = (p - p.clamp(-inner, inner)).normalize_or_zero();
        if n.length_squared() > 0.5 {
            return n;
        }
        // `r == 0`（退化成普通盒）时梯度为 0：取最外侧那个轴。
        let a = [p.x.abs(), p.y.abs(), p.z.abs()];
        let axis = if a[0] >= a[1] && a[0] >= a[2] {
            0
        } else if a[1] >= a[2] {
            1
        } else {
            2
        };
        let mut n = Vec3::ZERO;
        n[axis] = if [p.x, p.y, p.z][axis] >= 0.0 {
            1.0
        } else {
            -1.0
        };
        n
    };
    // 起点在内部：与其他形状一致，报**出射点**。
    let mut s = 0.0f32;
    if sdf(o) <= 0.0 {
        let mut lo = 0.0f32;
        let mut step = 1e-4f32;
        let mut hi = lo + step;
        while sdf(o + d * hi) <= 0.0 {
            lo = hi;
            step *= 2.0;
            hi = lo + step;
            if hi > max_distance {
                return None;
            }
        }
        for _ in 0..40 {
            let mid = 0.5 * (lo + hi);
            if sdf(o + d * mid) > 0.0 {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        s = 0.5 * (lo + hi);
        return Some((s, t.rotation.mul_vec3(normal_at(o + d * s))));
    }
    let mut prev = 0.0f32;
    for _ in 0..64 {
        let f = sdf(o + d * s);
        if f <= 0.0 {
            // 踩进了内部（数值抖动）：在 (prev, s) 之间二分出表面
            let mut lo = prev;
            let mut hi = s;
            for _ in 0..40 {
                let mid = 0.5 * (lo + hi);
                if sdf(o + d * mid) > 0.0 {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            s = 0.5 * (lo + hi);
            return Some((s, t.rotation.mul_vec3(normal_at(o + d * s))));
        }
        if f <= 1e-6 {
            return Some((s, t.rotation.mul_vec3(normal_at(o + d * s))));
        }
        prev = s;
        s += f;
        if s > max_distance {
            return None;
        }
    }
    None
}

/// 射线 - 盒（局部空间平板法）。
fn ray_box(
    half_extents: Vec3,
    t: &Transform,
    origin: Vec3,
    dir: Vec3,
    max_distance: f32,
) -> Option<(f32, Vec3)> {
    let he = half_extents.abs();
    let o = t.inverse_transform_point(origin);
    let d = t.rotation.inverse().mul_vec3(dir);
    let oa = [o.x, o.y, o.z];
    let da = [d.x, d.y, d.z];
    let ha = [he.x, he.y, he.z];
    let mut t_enter = f32::NEG_INFINITY;
    let mut t_exit = f32::INFINITY;
    let mut enter_axis = 0usize;
    let mut enter_sign = -1.0f32;
    let mut exit_axis = 0usize;
    let mut exit_sign = 1.0f32;
    for i in 0..3 {
        if da[i].abs() < 1e-9 {
            if oa[i].abs() > ha[i] {
                return None;
            }
            continue;
        }
        let inv = 1.0 / da[i];
        let t1 = (-ha[i] - oa[i]) * inv;
        let t2 = (ha[i] - oa[i]) * inv;
        let (near, far, near_sign, far_sign) = if t1 <= t2 {
            (t1, t2, -1.0f32, 1.0f32)
        } else {
            (t2, t1, 1.0f32, -1.0f32)
        };
        if near > t_enter {
            t_enter = near;
            enter_axis = i;
            enter_sign = near_sign;
        }
        if far < t_exit {
            t_exit = far;
            exit_axis = i;
            exit_sign = far_sign;
        }
        if t_enter > t_exit {
            return None;
        }
    }
    if t_exit < 0.0 {
        return None;
    }
    let (tt, axis, sign) = if t_enter >= 0.0 {
        (t_enter, enter_axis, enter_sign)
    } else {
        (t_exit, exit_axis, exit_sign)
    };
    if tt > max_distance {
        return None;
    }
    let mut n = Vec3::ZERO;
    match axis {
        0 => n.x = sign,
        1 => n.y = sign,
        _ => n.z = sign,
    }
    Some((tt, t.rotation.mul_vec3(n)))
}

/// 射线 - 胶囊（局部 Y 轴）。
fn ray_capsule(
    half_height: f32,
    radius: f32,
    t: &Transform,
    origin: Vec3,
    dir: Vec3,
    max_distance: f32,
) -> Option<(f32, Vec3)> {
    let h = half_height.abs();
    let r = radius.abs();
    let o = t.inverse_transform_point(origin);
    let d = t.rotation.inverse().mul_vec3(dir);
    let mut best: Option<(f32, Vec3)> = None;
    // 侧面
    if let Some((tt, n)) = ray_infinite_cylinder(o, d, r, h, max_distance) {
        best = Some((tt, n));
    }
    // 两个球帽
    for &sy in &[-1.0f32, 1.0] {
        let center = vec3(0.0, sy * h, 0.0);
        let up = vec3(0.0, sy, 0.0);
        if let Some((tt, n)) = ray_hemisphere(o, d, center, r, up, max_distance) {
            if best.map(|b| tt < b.0).unwrap_or(true) {
                best = Some((tt, n));
            }
        }
    }
    best.map(|(tt, n)| (tt, t.rotation.mul_vec3(n)))
}

/// 射线 - 圆锥（底面在 `-h`、顶点在 `+h`）。
///
/// 侧面是二次曲面 `x² + z² = k²(y - h)²`（`k = r / 2h`），解这个二次方程并把解限制在
/// `|y| <= h`；底面再单独当圆盘求一次。侧面与底面里取最近的那个解。
fn ray_cone(
    half_height: f32,
    radius: f32,
    t: &Transform,
    origin: Vec3,
    dir: Vec3,
    max_distance: f32,
) -> Option<(f32, Vec3)> {
    let h = half_height.abs();
    let r = radius.abs();
    if h <= 0.0 || r <= 0.0 {
        return None;
    }
    let o = t.inverse_transform_point(origin);
    let d = t.rotation.inverse().mul_vec3(dir);
    let k = r / (2.0 * h);
    let k2 = k * k;
    let mut best: Option<(f32, Vec3)> = None;
    // 侧面：a t² + b t + c = 0
    let a = d.x * d.x + d.z * d.z - k2 * d.y * d.y;
    let b = 2.0 * (o.x * d.x + o.z * d.z - k2 * d.y * (o.y - h));
    let c = o.x * o.x + o.z * o.z - k2 * (o.y - h) * (o.y - h);
    let consider = |tt: f32, n: Vec3, best: &mut Option<(f32, Vec3)>| {
        if tt >= 0.0 && tt <= max_distance && best.map(|x| tt < x.0).unwrap_or(true) {
            *best = Some((tt, n));
        }
    };
    let lateral = |tt: f32| {
        let p = o + d * tt;
        if p.y <= h + 1e-6 && p.y >= -h - 1e-6 {
            // 侧面外法线 = ∇(x² + z² - k²(y-h)²) = (2x, -2k²(y-h), 2z)
            Some(vec3(p.x, -k2 * (p.y - h), p.z).normalize_or_zero())
        } else {
            None
        }
    };
    if a.abs() > 1e-12 {
        let disc = b * b - 4.0 * a * c;
        if disc >= 0.0 {
            let sq = disc.sqrt();
            for tt in [(-b - sq) / (2.0 * a), (-b + sq) / (2.0 * a)] {
                if let Some(n) = lateral(tt) {
                    consider(tt, n, &mut best);
                }
            }
        }
    } else if b.abs() > 1e-12 {
        // 射线平行于某条母线：退化成一次方程。
        let tt = -c / b;
        if let Some(n) = lateral(tt) {
            consider(tt, n, &mut best);
        }
    }
    // 底面圆盘（外法线 -Y）
    if d.y.abs() > 1e-9 {
        let tt = (-h - o.y) / d.y;
        let p = o + d * tt;
        if p.x * p.x + p.z * p.z <= r * r + 1e-6 {
            consider(tt, -Vec3::Y, &mut best);
        }
    }
    best.map(|(tt, n)| (tt, t.rotation.mul_vec3(n)))
}

/// 射线 - 圆柱（局部 Y 轴）。
fn ray_cylinder(
    half_height: f32,
    radius: f32,
    t: &Transform,
    origin: Vec3,
    dir: Vec3,
    max_distance: f32,
) -> Option<(f32, Vec3)> {
    let h = half_height.abs();
    let r = radius.abs();
    let o = t.inverse_transform_point(origin);
    let d = t.rotation.inverse().mul_vec3(dir);
    let mut best: Option<(f32, Vec3)> = None;
    if let Some((tt, n)) = ray_infinite_cylinder(o, d, r, h, max_distance) {
        best = Some((tt, n));
    }
    // 两个端面
    for &sy in &[-1.0f32, 1.0] {
        if d.y.abs() < 1e-9 {
            continue;
        }
        let tt = (sy * h - o.y) / d.y;
        if tt < 0.0 || tt > max_distance {
            continue;
        }
        let p = o + d * tt;
        if p.x * p.x + p.z * p.z > r * r + 1e-6 {
            continue;
        }
        if best.map(|b| tt < b.0).unwrap_or(true) {
            best = Some((tt, vec3(0.0, sy, 0.0)));
        }
    }
    best.map(|(tt, n)| (tt, t.rotation.mul_vec3(n)))
}

/// 无限长圆柱侧面（限制 `|y| <= h`）。
fn ray_infinite_cylinder(
    o: Vec3,
    d: Vec3,
    r: f32,
    h: f32,
    max_distance: f32,
) -> Option<(f32, Vec3)> {
    let a = d.x * d.x + d.z * d.z;
    if a < 1e-12 {
        return None;
    }
    let b = 2.0 * (o.x * d.x + o.z * d.z);
    let c = o.x * o.x + o.z * o.z - r * r;
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        return None;
    }
    let sq = disc.sqrt();
    let roots = [(-b - sq) / (2.0 * a), (-b + sq) / (2.0 * a)];
    for &tt in &roots {
        if tt < 0.0 || tt > max_distance {
            continue;
        }
        let p = o + d * tt;
        if p.y.abs() <= h + 1e-6 {
            let n = vec3(p.x, 0.0, p.z).normalize_or_zero();
            return Some((tt, n));
        }
    }
    None
}

/// 射线 - 半球（法线朝向 `up` 的一侧）。
fn ray_hemisphere(
    o: Vec3,
    d: Vec3,
    center: Vec3,
    r: f32,
    up: Vec3,
    max_distance: f32,
) -> Option<(f32, Vec3)> {
    let oc = o - center;
    let a = d.length_squared();
    if a < 1e-12 {
        return None;
    }
    let b = 2.0 * oc.dot(d);
    let c = oc.length_squared() - r * r;
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        return None;
    }
    let sq = disc.sqrt();
    let mut roots = [(-b - sq) / (2.0 * a), (-b + sq) / (2.0 * a)];
    roots.sort_by(|x, y| x.partial_cmp(y).unwrap_or(core::cmp::Ordering::Equal));
    for &tt in &roots {
        if tt < 0.0 || tt > max_distance {
            continue;
        }
        let p = o + d * tt;
        let n = (p - center).normalize_or_zero();
        if n.dot(up) >= -1e-6 {
            return Some((tt, n));
        }
    }
    None
}

/// 射线 - 凸包（球体步进，用 GJK 点距离保证不穿透）。
fn ray_convex_hull(
    shape: &Shape,
    t: &Transform,
    origin: Vec3,
    dir: Vec3,
    max_distance: f32,
) -> Option<(f32, Vec3)> {
    let pt_shape = Shape::ConvexHull {
        points: vec![Vec3::ZERO],
    };
    let mut s = 0.0f32;
    for _ in 0..MAX_MARCH_ITERATIONS {
        let p = origin + dir * s;
        let (pa, pb, dist, n) =
            gjk::closest_points(shape, t, &pt_shape, &Transform::from_translation(p));
        if dist <= TOUCH {
            let _ = pa;
            let normal = if n.length_squared() > 0.5 {
                n
            } else if (pb - pa).length_squared() > 1e-12 {
                (pb - pa).normalize_or_zero()
            } else {
                -dir
            };
            return Some((s, normal));
        }
        s += dist;
        if s > max_distance {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::{BodyBuilder, BodyType};

    fn world_with_sphere() -> (PhysicsWorld, BodyHandle) {
        let mut w = PhysicsWorld::new();
        let h = w.add_body(
            BodyBuilder::r#static(Shape::Sphere { radius: 0.5 })
                .position(vec3(0.0, 0.0, 0.0))
                .build(),
        );
        (w, h)
    }

    #[test]
    fn raycast_hits_sphere() {
        let (w, h) = world_with_sphere();
        let hit = w
            .raycast(vec3(-5.0, 0.0, 0.0), Vec3::X, 10.0)
            .expect("应命中球");
        assert_eq!(hit.handle, h);
        assert!((hit.distance - 4.5).abs() < 1e-3, "距离 {}", hit.distance);
        assert!((hit.point.x + 0.5).abs() < 1e-3);
        assert!(hit.normal.x < -0.9, "法线应朝向射线来向 {:?}", hit.normal);
    }

    #[test]
    fn raycast_misses_when_out_of_range() {
        let (w, _) = world_with_sphere();
        assert!(w.raycast(vec3(-5.0, 0.0, 0.0), Vec3::X, 1.0).is_none());
        assert!(w.raycast(vec3(-5.0, 3.0, 0.0), Vec3::X, 100.0).is_none());
        assert!(w.raycast(vec3(0.0, 0.0, 0.0), Vec3::ZERO, 100.0).is_none());
    }

    #[test]
    fn raycast_hits_plane_and_box() {
        let mut w = PhysicsWorld::new();
        w.add_body(
            BodyBuilder::r#static(Shape::Plane { normal: Vec3::Y })
                .position(vec3(0.0, 0.0, 0.0))
                .build(),
        );
        let b = w.add_body(
            BodyBuilder::r#static(Shape::Box {
                half_extents: Vec3::splat(0.5),
            })
            .position(vec3(0.0, 5.0, 0.0))
            .build(),
        );
        let hit = w
            .raycast(vec3(0.0, 10.0, 0.0), Vec3::NEG_Y, 100.0)
            .expect("应命中");
        assert_eq!(hit.handle, b, "更近的盒应优先");
        assert!((hit.distance - 4.5).abs() < 1e-3, "距离 {}", hit.distance);

        let hit2 = w
            .raycast(vec3(10.0, 10.0, 0.0), Vec3::NEG_Y, 100.0)
            .expect("应命中地面");
        assert!((hit2.distance - 10.0).abs() < 1e-2);
        assert!(hit2.normal.y > 0.9);
    }

    #[test]
    fn raycast_filter_skips_body() {
        let (w, h) = world_with_sphere();
        assert!(w
            .raycast_filtered(vec3(-5.0, 0.0, 0.0), Vec3::X, 10.0, |x| x != h)
            .is_none());
    }

    #[test]
    fn overlap_sphere_finds_close_bodies() {
        let mut w = PhysicsWorld::new();
        let a = w.add_body(
            BodyBuilder::r#static(Shape::Sphere { radius: 0.5 })
                .position(vec3(0.0, 0.0, 0.0))
                .build(),
        );
        let b = w.add_body(
            BodyBuilder::r#static(Shape::Sphere { radius: 0.5 })
                .position(vec3(10.0, 0.0, 0.0))
                .build(),
        );
        let found = overlap_sphere(&w, Vec3::ZERO, 1.5);
        assert!(found.contains(&a));
        assert!(!found.contains(&b));
        assert!(overlap_sphere(&w, Vec3::ZERO, f32::NAN).is_empty());
    }

    #[test]
    fn shape_cast_stops_before_wall() {
        let mut w = PhysicsWorld::new();
        w.add_body(
            BodyBuilder::r#static(Shape::Box {
                half_extents: Vec3::splat(0.5),
            })
            .position(vec3(3.0, 0.0, 0.0))
            .build(),
        );
        let shape = Shape::Sphere { radius: 0.5 };
        let from = Transform::from_translation(Vec3::ZERO);
        let hit = w
            .shape_cast(&shape, &from, vec3(10.0, 0.0, 0.0))
            .expect("应撞上盒");
        assert!(
            (hit.distance - 2.0).abs() < 0.05,
            "应停在 2.0 附近，实际 {}",
            hit.distance
        );
        let miss = w.shape_cast(&shape, &from, vec3(10.0, 5.0, 0.0));
        assert!(miss.is_none() || miss.unwrap().distance > 9.0);
    }

    /// 圆锥的射线求交：侧面用二次曲面解、底面单独当圆盘，两者取近的那个。
    ///
    /// 数字都是手算的：`h = 0.5, r = 0.5` ⇒ 半顶角处 `k = r / 2h = 0.5`。
    /// * y=0 那一圈半径 = `k·(h - 0) = 0.25`（从 (0,0,5) 沿 -Z 打，距离 4.75）；
    /// * 从下方沿 +Y 打，先撞底面圆盘（y=-0.5，距离 5.5）；
    /// * 从上方沿 -Y 打，先撞顶点（y=+0.5，距离 5.5）。
    #[test]
    fn raycast_hits_cone_lateral_base_and_apex() {
        let mut w = PhysicsWorld::new();
        let h = w.add_body(
            BodyBuilder::r#static(Shape::Cone {
                half_height: 0.5,
                radius: 0.5,
            })
            .build(),
        );
        // 侧面
        let side = w
            .raycast(vec3(0.0, 0.0, 5.0), Vec3::NEG_Z, 100.0)
            .expect("应命中侧面");
        assert_eq!(side.handle, h);
        assert!(
            (side.distance - 4.75).abs() < 1e-3,
            "侧面命中距离 {}",
            side.distance
        );
        assert!(
            (side.point.z - 0.25).abs() < 1e-3,
            "命中点 {:?}",
            side.point
        );
        // 法线应朝外（有 +Z 分量，也有 +Y 分量：侧面是斜的）
        assert!(side.normal.z > 0.5, "法线 {:?}", side.normal);
        assert!(
            side.normal.y > 0.0,
            "斜侧面法线应有 +Y 分量 {:?}",
            side.normal
        );
        // 底面（从下方打）
        let base = w
            .raycast(vec3(0.0, -5.0, 0.0), Vec3::Y, 100.0)
            .expect("应命中底面");
        assert!(
            (base.distance - 4.5).abs() < 1e-3,
            "底面距离 {}",
            base.distance
        );
        assert!((base.point.y + 0.5).abs() < 1e-3, "命中点 {:?}", base.point);
        assert!(base.normal.y < -0.9, "底面法线应朝下 {:?}", base.normal);
        // 顶点（从上方沿轴打：x=z=0 处侧面退化成一点，仍要给出 +h 的命中）
        let apex = w
            .raycast(vec3(0.0, 5.0, 0.0), Vec3::NEG_Y, 100.0)
            .expect("应命中顶点");
        assert!(
            (apex.distance - 4.5).abs() < 1e-3,
            "顶点距离 {}",
            apex.distance
        );
        assert!((apex.point.y - 0.5).abs() < 1e-3, "命中点 {:?}", apex.point);
        // 打偏了就不该命中
        assert!(w.raycast(vec3(0.0, 5.0, 3.0), Vec3::NEG_Y, 100.0).is_none());
    }

    #[test]
    fn body_type_helper_is_dynamic() {
        let b = BodyBuilder::dynamic(Shape::Sphere { radius: 1.0 }).build();
        assert_eq!(b.body_type, BodyType::Dynamic);
        assert!(b.is_dynamic());
    }
}
