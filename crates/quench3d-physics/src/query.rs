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
const TOUCH: f32 = 1e-4;
/// 形状投射的最大推进次数。
const MAX_CAST_ITERATIONS: usize = 64;
/// 球体步进的最大次数。
const MAX_MARCH_ITERATIONS: usize = 48;

/// 距离查询结果：查询形状上的点、对方形状上的点、距离、A→B 法线。
#[derive(Clone, Copy, Debug)]
struct DistanceResult {
    point_a: Vec3,
    point_b: Vec3,
    distance: f32,
    normal: Vec3,
}

/// 查询形状与单个刚体的距离（半空间解析处理，其余走 GJK）。
fn distance_to_body(shape: &Shape, t: &Transform, body: &RigidBody) -> DistanceResult {
    if let Shape::Plane { normal } = body.shape {
        let n = body
            .transform
            .rotation
            .mul_vec3(normal.normalize_or_zero())
            .normalize_or_zero();
        let sup = Support::new(shape, t);
        let p = sup.support(-n);
        let sd = n.dot(p - body.transform.position);
        return DistanceResult {
            point_a: p,
            point_b: p - n * sd,
            distance: sd.max(0.0),
            // A→B 方向为 -n（B 是半空间）。
            normal: -n,
        };
    }
    let (pa, pb, d, n) = gjk::closest_points(shape, t, &body.shape, &body.transform);
    DistanceResult {
        point_a: pa,
        point_b: pb,
        distance: d,
        normal: if n.length_squared() > 0.5 { n } else { Vec3::Y },
    }
}

/// 球形重叠查询：返回与球体相交（或相切）的所有刚体句柄。
pub fn overlap_sphere(world: &PhysicsWorld, center: Vec3, radius: f32) -> Vec<BodyHandle> {
    let mut out = Vec::new();
    if !center.is_finite() || !radius.is_finite() || radius < 0.0 {
        return out;
    }
    let probe = Shape::Sphere { radius };
    let pt = Transform::from_translation(center);
    let probe_aabb = Aabb::from_center_half_extents(center, Vec3::splat(radius));
    for (handle, body) in world.bodies() {
        if !matches!(body.shape, Shape::Plane { .. }) {
            let aabb = body
                .shape
                .local_aabb()
                .transformed_conservative(&body.transform.matrix());
            if !aabb.intersects(&probe_aabb) {
                continue;
            }
        }
        let r = distance_to_body(&probe, &pt, body);
        if r.distance <= TOUCH {
            out.push(handle);
        }
    }
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
        if !matches!(body.shape, Shape::Plane { .. }) {
            let aabb = body
                .shape
                .local_aabb()
                .transformed_conservative(&body.transform.matrix());
            match aabb.intersect_ray(origin, d) {
                Some(t) if t <= max_distance => {}
                _ => continue,
            }
        }
        if let Some((t, n)) = ray_shape(&body.shape, &body.transform, origin, d, max_distance) {
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
fn ray_shape(
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
        Shape::TriMesh { vertices, indices } => {
            let ray = Ray::new(origin, dir);
            let mut best: Option<(f32, Vec3)> = None;
            for tri in indices.chunks_exact(3) {
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
    }
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

    #[test]
    fn body_type_helper_is_dynamic() {
        let b = BodyBuilder::dynamic(Shape::Sphere { radius: 1.0 }).build();
        assert_eq!(b.body_type, BodyType::Dynamic);
        assert!(b.is_dynamic());
    }
}
