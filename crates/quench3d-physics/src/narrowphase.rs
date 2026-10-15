//! 窄相碰撞检测。
//!
//! 策略：
//! * 半空间（`Plane`）用解析法，对球 / 盒 / 胶囊 / 圆柱 / 凸包分别生成候选点；
//! * 盒-盒用 SAT + 参考面/入射面裁剪，稳定产出最多 4 个接触点（堆叠关键）；
//! * 球-球 / 球-盒 / 球-胶囊 有解析快速路径；
//! * 其余凸-凸走 GJK + EPA，单点接触；
//! * `TriMesh` 逐三角形（当作退化凸包）与对方形状求解，EPA 退化时用三角形
//!   平面 + 最近点法线兜底。
//!
//! 所有流形的法线都由 A 指向 B，穿透深度为正。

use quench3d_math::*;

use crate::contact::{ContactManifold, ContactPoint};
use crate::gjk::{self, Support};
use crate::shape::Shape;
use crate::small_vec::SmallVec;

/// 单个流形的最大接触点数。
const MAX_POINTS: usize = 4;
/// 裁剪前允许的候选接触点数（内联容量；超出会退化成堆，行为不变）。
const MAX_CANDIDATES: usize = 8;
/// 凸多边形裁剪路径的内联顶点容量。
const POLY_CAPACITY: usize = 12;
/// 判定「已接触」的最小穿透（略为负值也接受，避免抖动时接触闪断）。
const PENETRATION_EPS: f32 = -1e-4;

/// 由候选点构造流形：过滤非法数据、按空间分布裁剪到 4 个点。
fn build_manifold(candidates: impl AsRef<[ContactPoint]>) -> Option<ContactManifold> {
    let mut points: SmallVec<ContactPoint, MAX_CANDIDATES> = SmallVec::new();
    points.extend_from_slice(candidates.as_ref());
    points.retain(|p| {
        p.position.is_finite()
            && p.normal.is_finite()
            && p.penetration.is_finite()
            && p.penetration > PENETRATION_EPS
            && p.normal.length_squared() > 0.5
    });
    if points.is_empty() {
        return None;
    }
    let points: SmallVec<ContactPoint, MAX_POINTS> = if points.len() > MAX_POINTS {
        select_spread(&points, MAX_POINTS)
    } else {
        points.iter().copied().collect()
    };
    Some(ContactManifold { points })
}

/// 选取最多 `max` 个「最深 + 空间分散」的接触点（确定性）。
fn select_spread(points: &[ContactPoint], max: usize) -> SmallVec<ContactPoint, MAX_POINTS> {
    let mut chosen: SmallVec<usize, MAX_POINTS> = SmallVec::new();
    // 1) 最深的一个（并列时取下标最小）。
    let mut best = 0usize;
    for (i, p) in points.iter().enumerate() {
        if p.penetration > points[best].penetration {
            best = i;
        }
    }
    chosen.push(best);
    // 2) 依次挑选与已选点最小距离最大的点。
    while chosen.len() < max {
        let mut best_i = usize::MAX;
        let mut best_d = -1.0f32;
        for (i, p) in points.iter().enumerate() {
            if chosen.contains(&i) {
                continue;
            }
            let mut min_d = f32::INFINITY;
            for &c in &chosen {
                min_d = min_d.min((p.position - points[c].position).length_squared());
            }
            if min_d > best_d {
                best_d = min_d;
                best_i = i;
            }
        }
        if best_i == usize::MAX {
            break;
        }
        chosen.push(best_i);
    }
    chosen.sort_unstable();
    chosen.iter().map(|&i| points[i]).collect()
}

/// 凸-凸单点接触（GJK + EPA）。
fn convex_pair(sa: &Shape, ta: &Transform, sb: &Shape, tb: &Transform) -> Option<ContactManifold> {
    let sup_a = Support::new(sa, ta);
    let sup_b = Support::new(sb, tb);
    let (normal, depth, pa, pb) = gjk::convex_convex(&sup_a, &sup_b)?;
    if !normal.is_finite() || !depth.is_finite() {
        return None;
    }
    let position = (pa + pb) * 0.5;
    build_manifold([ContactPoint {
        position,
        normal: normal.normalize_or_zero(),
        penetration: depth,
    }])
}

// ---------------------------------------------------------------------------
// 半空间
// ---------------------------------------------------------------------------

/// 形状相对半空间 `n·(p - origin) = 0`（实体在负侧）的候选接触点。
fn shape_plane_candidates(
    shape: &Shape,
    t: &Transform,
    n_world: Vec3,
    plane_origin: Vec3,
) -> SmallVec<(Vec3, f32), MAX_CANDIDATES> {
    let n = n_world.normalize_or_zero();
    let sd = |p: Vec3| n.dot(p - plane_origin);
    let mut out: SmallVec<(Vec3, f32), MAX_CANDIDATES> = SmallVec::new();
    match shape {
        Shape::Sphere { radius } => {
            let c = t.position;
            out.push((c - n * radius.abs(), radius.abs() - sd(c)));
        }
        Shape::Box { half_extents } => {
            let he = half_extents.abs();
            for &sx in &[-1.0f32, 1.0] {
                for &sy in &[-1.0f32, 1.0] {
                    for &sz in &[-1.0f32, 1.0] {
                        let p = t.transform_point(vec3(sx * he.x, sy * he.y, sz * he.z));
                        out.push((p, -sd(p)));
                    }
                }
            }
        }
        Shape::Capsule {
            half_height,
            radius,
        } => {
            let r = radius.abs();
            for &sy in &[-1.0f32, 1.0] {
                let e = t.transform_point(vec3(0.0, sy * half_height.abs(), 0.0));
                out.push((e - n * r, r - sd(e)));
            }
        }
        Shape::Cylinder {
            half_height,
            radius,
        } => {
            let r = radius.abs();
            let h = half_height.abs();
            let count = 8;
            for &sy in &[-1.0f32, 1.0] {
                for i in 0..count {
                    let a = i as f32 / count as f32 * core::f32::consts::TAU;
                    let p = t.transform_point(vec3(a.cos() * r, sy * h, a.sin() * r));
                    out.push((p, -sd(p)));
                }
            }
        }
        Shape::ConvexHull { points } => {
            for p in points {
                let w = t.transform_point(*p);
                out.push((w, -sd(w)));
            }
        }
        // 半空间-半空间、三角网格-半空间都跳过。
        Shape::Plane { .. } | Shape::TriMesh { .. } => {}
    }
    out
}

/// 返回法线由 `shape` 指向半空间（即 `-n_world`）的流形。
fn shape_vs_plane(
    shape: &Shape,
    t: &Transform,
    plane_normal_local: Vec3,
    plane_transform: &Transform,
) -> Option<ContactManifold> {
    let n_world = plane_transform
        .rotation
        .mul_vec3(plane_normal_local.normalize_or_zero());
    if n_world.length_squared() < 0.5 {
        return None;
    }
    let candidates = shape_plane_candidates(shape, t, n_world, plane_transform.position);
    let normal = -n_world;
    let mut points: SmallVec<ContactPoint, MAX_CANDIDATES> = SmallVec::new();
    for &(p, pen) in &candidates {
        if pen <= PENETRATION_EPS {
            continue;
        }
        points.push(ContactPoint {
            position: p,
            normal,
            penetration: pen,
        });
    }
    build_manifold(&points)
}

// ---------------------------------------------------------------------------
// 球体快速路径
// ---------------------------------------------------------------------------

fn sphere_sphere(ra: f32, ta: &Transform, rb: f32, tb: &Transform) -> Option<ContactManifold> {
    let d = tb.position - ta.position;
    let dist = d.length();
    let sum = ra + rb;
    if dist >= sum {
        return None;
    }
    let normal = if dist > 1e-6 { d / dist } else { Vec3::Y };
    let penetration = sum - dist;
    let position = ta.position + normal * (ra - penetration * 0.5);
    build_manifold([ContactPoint {
        position,
        normal,
        penetration,
    }])
}

/// 球（A）对盒（B）。法线由 A 指向 B。
fn sphere_box(radius: f32, ta: &Transform, he: Vec3, tb: &Transform) -> Option<ContactManifold> {
    let he = he.abs();
    let local_c = tb.inverse_transform_point(ta.position);
    let clamped = local_c.clamp(-he, he);
    let inside = local_c == clamped;
    let (local_point, normal_local, penetration) = if !inside {
        let delta = local_c - clamped;
        let dist = delta.length();
        if dist >= radius {
            return None;
        }
        let n = if dist > 1e-6 { delta / dist } else { Vec3::Y };
        // n 由盒指向球，所以 A→B 是 -n。
        (clamped, -n, radius - dist)
    } else {
        let dx = he.x - local_c.x.abs();
        let dy = he.y - local_c.y.abs();
        let dz = he.z - local_c.z.abs();
        let (axis, dist) = if dx <= dy && dx <= dz {
            (Vec3::X * local_c.x.signum(), dx)
        } else if dy <= dz {
            (Vec3::Y * local_c.y.signum(), dy)
        } else {
            (Vec3::Z * local_c.z.signum(), dz)
        };
        (local_c, -axis, dist + radius)
    };
    let normal = tb.rotation.mul_vec3(normal_local).normalize_or_zero();
    if normal.length_squared() < 0.5 {
        return None;
    }
    let world_point = tb.transform_point(local_point);
    let position = world_point + normal * (-penetration * 0.5);
    build_manifold([ContactPoint {
        position,
        normal,
        penetration,
    }])
}

/// 球（A）对胶囊（B，沿本地 Y 轴）。法线由 A 指向 B。
fn sphere_capsule(
    radius: f32,
    ta: &Transform,
    half_height: f32,
    cap_radius: f32,
    tb: &Transform,
) -> Option<ContactManifold> {
    let local_c = tb.inverse_transform_point(ta.position);
    let h = half_height.abs();
    let closest = vec3(0.0, local_c.y.clamp(-h, h), 0.0);
    let delta = local_c - closest;
    let dist = delta.length();
    let sum = radius + cap_radius;
    if dist >= sum {
        return None;
    }
    let n_local = if dist > 1e-6 { delta / dist } else { Vec3::Y };
    // n_local 由胶囊轴指向球心，故 A→B 的法线是 -n_local。
    let normal = tb.rotation.mul_vec3(-n_local).normalize_or_zero();
    let penetration = sum - dist;
    let contact_local = closest + n_local * (cap_radius - penetration * 0.5);
    let position = tb.transform_point(contact_local);
    build_manifold([ContactPoint {
        position,
        normal,
        penetration,
    }])
}

// ---------------------------------------------------------------------------
// 盒 - 盒：SAT + 参考面/入射面裁剪
// ---------------------------------------------------------------------------

#[inline]
fn proj_radius(axes: &[Vec3; 3], he: Vec3, l: Vec3) -> f32 {
    he.x * l.dot(axes[0]).abs() + he.y * l.dot(axes[1]).abs() + he.z * l.dot(axes[2]).abs()
}

/// 用平面 `n·p <= offset` 裁剪凸多边形。
fn clip_polygon(poly: &[Vec3], n: Vec3, offset: f32) -> SmallVec<Vec3, POLY_CAPACITY> {
    let mut out: SmallVec<Vec3, POLY_CAPACITY> = SmallVec::new();
    if poly.is_empty() {
        return out;
    }
    let d = |p: Vec3| n.dot(p) - offset;
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        let da = d(a);
        let db = d(b);
        if da <= 0.0 {
            out.push(a);
        }
        if (da < 0.0 && db > 0.0) || (da > 0.0 && db < 0.0) {
            let t = da / (da - db);
            out.push(a + (b - a) * t);
        }
    }
    out
}

fn box_box(he_a: Vec3, ta: &Transform, he_b: Vec3, tb: &Transform) -> Option<ContactManifold> {
    let he_a = he_a.abs();
    let he_b = he_b.abs();
    let axes_a = [
        ta.rotation.mul_vec3(Vec3::X),
        ta.rotation.mul_vec3(Vec3::Y),
        ta.rotation.mul_vec3(Vec3::Z),
    ];
    let axes_b = [
        tb.rotation.mul_vec3(Vec3::X),
        tb.rotation.mul_vec3(Vec3::Y),
        tb.rotation.mul_vec3(Vec3::Z),
    ];
    let ca = ta.position;
    let cb = tb.position;
    let d = cb - ca;

    let mut best_face_overlap = f32::INFINITY;
    let mut best_face_from_a = true;
    let mut best_face_idx = 0usize;
    for i in 0..3 {
        let l = axes_a[i];
        let overlap =
            proj_radius(&axes_a, he_a, l) + proj_radius(&axes_b, he_b, l) - d.dot(l).abs();
        if overlap <= 0.0 {
            return None;
        }
        if overlap < best_face_overlap {
            best_face_overlap = overlap;
            best_face_from_a = true;
            best_face_idx = i;
        }
    }
    for j in 0..3 {
        let l = axes_b[j];
        let overlap =
            proj_radius(&axes_a, he_a, l) + proj_radius(&axes_b, he_b, l) - d.dot(l).abs();
        if overlap <= 0.0 {
            return None;
        }
        if overlap < best_face_overlap {
            best_face_overlap = overlap;
            best_face_from_a = false;
            best_face_idx = j;
        }
    }

    let mut best_edge_overlap = f32::INFINITY;
    let mut best_edge_axis = Vec3::ZERO;
    let mut best_edge_pair = (0usize, 0usize);
    for i in 0..3 {
        for j in 0..3 {
            let l = axes_a[i].cross(axes_b[j]);
            if l.length_squared() < 1e-6 {
                continue; // 平行边，跳过
            }
            let l = l.normalize_or_zero();
            let overlap =
                proj_radius(&axes_a, he_a, l) + proj_radius(&axes_b, he_b, l) - d.dot(l).abs();
            if overlap <= 0.0 {
                return None;
            }
            if overlap < best_edge_overlap {
                best_edge_overlap = overlap;
                best_edge_axis = l;
                best_edge_pair = (i, j);
            }
        }
    }

    // 面轴优先（数值更稳，且能产出多点流形）。
    let prefer_edge = best_edge_overlap < best_face_overlap
        && best_edge_overlap < best_face_overlap - 1e-3 * best_face_overlap.abs().max(1e-4);

    if prefer_edge {
        let l = best_edge_axis;
        let n = if d.dot(l) >= 0.0 { l } else { -l };
        let (i, j) = best_edge_pair;
        let k1 = (i + 1) % 3;
        let k2 = (i + 2) % 3;
        let ea_c = ca
            + axes_a[k1] * (axes_a[k1].dot(n).signum() * he_a[k1])
            + axes_a[k2] * (axes_a[k2].dot(n).signum() * he_a[k2]);
        let l1 = (j + 1) % 3;
        let l2 = (j + 2) % 3;
        let eb_c = cb
            + axes_b[l1] * (-axes_b[l1].dot(n).signum() * he_b[l1])
            + axes_b[l2] * (-axes_b[l2].dot(n).signum() * he_b[l2]);
        let (pa, pb) = closest_points_segments(
            ea_c - axes_a[i] * he_a[i],
            ea_c + axes_a[i] * he_a[i],
            eb_c - axes_b[j] * he_b[j],
            eb_c + axes_b[j] * he_b[j],
        );
        let position = (pa + pb) * 0.5;
        return build_manifold([ContactPoint {
            position,
            normal: n,
            penetration: best_edge_overlap,
        }]);
    }

    // ---- 参考面 / 入射面裁剪 ----
    let (ref_axes, ref_he, ref_pos, inc_axes, inc_he, inc_pos, ref_idx, flip) = if best_face_from_a
    {
        (axes_a, he_a, ca, axes_b, he_b, cb, best_face_idx, false)
    } else {
        (axes_b, he_b, cb, axes_a, he_a, ca, best_face_idx, true)
    };
    let d_ref = inc_pos - ref_pos;
    let sign = if d_ref.dot(ref_axes[ref_idx]) >= 0.0 {
        1.0
    } else {
        -1.0
    };
    let n_ref = ref_axes[ref_idx] * sign;
    let ref_face_center = ref_pos + n_ref * ref_he[ref_idx];

    // 入射面：法线与 n_ref 最同向的面（即最「朝向」参考盒的面）。
    let mut inc_idx = 0usize;
    let mut best_dot = 0.0f32;
    for (k, axis) in inc_axes.iter().enumerate() {
        let dd = axis.dot(n_ref);
        if dd.abs() > best_dot.abs() {
            best_dot = dd;
            inc_idx = k;
        }
    }
    if best_dot.abs() < 1e-6 {
        return None;
    }
    let inc_dir = inc_axes[inc_idx] * best_dot.signum();
    // 入射面取朝向参考盒的那一面（与 n_ref 反向）。
    let inc_face_center = inc_pos - inc_dir * inc_he[inc_idx];
    let k1 = (inc_idx + 1) % 3;
    let k2 = (inc_idx + 2) % 3;
    let mut poly: SmallVec<Vec3, POLY_CAPACITY> = SmallVec::new();
    for &s1 in &[-1.0f32, 1.0] {
        for &s2 in &[-1.0f32, 1.0] {
            poly.push(
                inc_face_center
                    + inc_axes[k1] * (s1 * inc_he[k1])
                    + inc_axes[k2] * (s2 * inc_he[k2]),
            );
        }
    }
    // 用参考面的 4 个侧面裁剪。
    for k in 0..3 {
        if k == ref_idx {
            continue;
        }
        let axis = ref_axes[k];
        let center = ref_face_center.dot(axis);
        poly = clip_polygon(&poly, axis, center + ref_he[k]);
        if poly.is_empty() {
            return None;
        }
        poly = clip_polygon(&poly, -axis, -center + ref_he[k]);
        if poly.is_empty() {
            return None;
        }
    }

    let normal = if flip { -n_ref } else { n_ref };
    let mut points: SmallVec<ContactPoint, POLY_CAPACITY> = SmallVec::new();
    for p in poly.iter().copied() {
        let sep = n_ref.dot(p - ref_face_center);
        if sep > 0.0 {
            continue;
        }
        points.push(ContactPoint {
            position: p + n_ref * (sep * 0.5),
            normal,
            penetration: -sep,
        });
    }
    build_manifold(&points)
}

/// 两条线段之间的最近点对（Ericson《Real-Time Collision Detection》）。
fn closest_points_segments(p1: Vec3, q1: Vec3, p2: Vec3, q2: Vec3) -> (Vec3, Vec3) {
    let d1 = q1 - p1;
    let d2 = q2 - p2;
    let r = p1 - p2;
    let a = d1.dot(d1);
    let e = d2.dot(d2);
    let f = d2.dot(r);
    #[allow(unused_assignments)]
    let (mut s, mut t);
    if a <= 1e-12 && e <= 1e-12 {
        return (p1, p2);
    }
    if a <= 1e-12 {
        s = 0.0;
        t = clamp(f / e, 0.0, 1.0);
    } else {
        let c = d1.dot(r);
        if e <= 1e-12 {
            t = 0.0;
            s = clamp(-c / a, 0.0, 1.0);
        } else {
            let b = d1.dot(d2);
            let denom = a * e - b * b;
            s = if denom.abs() > 1e-12 {
                clamp((b * f - c * e) / denom, 0.0, 1.0)
            } else {
                0.0
            };
            t = (b * s + f) / e;
            if t < 0.0 {
                t = 0.0;
                s = clamp(-c / a, 0.0, 1.0);
            } else if t > 1.0 {
                t = 1.0;
                s = clamp((b - c) / a, 0.0, 1.0);
            }
        }
    }
    (p1 + d1 * s, p2 + d2 * t)
}

// ---------------------------------------------------------------------------
// 三角网格
// ---------------------------------------------------------------------------

/// 三角形（世界坐标）与另一形状求接触。法线由三角形指向对方。
///
/// 扁平三角形的 GJK/EPA 容易退化，因此分级处理：
/// 1. 球体走解析最近点（边、顶点也精确）；
/// 2. 一般凸体先试 GJK + EPA；
/// 3. EPA 失败或 GJK 假阴性时，用「三角形平面 + 支持点」给出面接触
///    （要求最近特征落在三角形内部，或有三角形顶点确实嵌在对方体内）；
/// 4. 仍无结果则把三条边当作退化凸包（线段）各求一次，取最深接触。
fn triangle_vs_shape(
    tri: [Vec3; 3],
    other: &Shape,
    t_other: &Transform,
) -> Option<ContactManifold> {
    let identity = Transform::IDENTITY;
    // --- 1. 球体解析路径 ---
    if let Shape::Sphere { radius } = other {
        let r = radius.abs();
        let c = t_other.position;
        let (q, _w) = gjk::closest_on_triangle(tri[0] - c, tri[1] - c, tri[2] - c);
        let q = q + c;
        let d = (c - q).length();
        if d >= r {
            return None;
        }
        let face = (tri[1] - tri[0]).cross(tri[2] - tri[0]).normalize_or_zero();
        let dir = if d > 1e-6 { (c - q) / d } else { face };
        if dir.length_squared() < 0.5 {
            return None;
        }
        return build_manifold([ContactPoint {
            position: q,
            normal: dir,
            penetration: r - d,
        }]);
    }

    // --- 2. GJK + EPA ---
    let tri_shape = Shape::ConvexHull {
        points: vec![tri[0], tri[1], tri[2]],
    };
    let sup_a = Support::new(&tri_shape, &identity);
    let sup_b = Support::new(other, t_other);
    let r = gjk::gjk(&sup_a, &sup_b);
    if r.intersect {
        if let Some(e) = gjk::epa(&sup_a, &sup_b, &r) {
            if e.normal.is_finite() && e.depth.is_finite() {
                let position = (e.point_a + e.point_b) * 0.5;
                let normal = e.normal.normalize_or_zero();
                if normal.length_squared() > 0.5 {
                    if let Some(m) = build_manifold([ContactPoint {
                        position,
                        normal,
                        penetration: e.depth,
                    }]) {
                        return Some(m);
                    }
                }
            }
        }
    }

    // --- 3. 平面兜底 ---
    if r.intersect || r.distance <= 1e-3 {
        if let Some(m) = triangle_plane_fallback(tri, other, t_other) {
            return Some(m);
        }
    }

    // --- 4. 边（线段）兜底 ---
    let edges = [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])];
    let mut best: Option<ContactManifold> = None;
    let mut best_depth = 0.0f32;
    for (ea, eb) in edges {
        let seg_shape = Shape::ConvexHull {
            points: vec![ea, eb],
        };
        let sa = Support::new(&seg_shape, &identity);
        if let Some((n, depth, pa, pb)) = gjk::convex_convex(&sa, &sup_b) {
            if depth.is_finite() && depth > best_depth && n.is_finite() {
                let normal = n.normalize_or_zero();
                if normal.length_squared() > 0.5 {
                    if let Some(m) = build_manifold([ContactPoint {
                        position: (pa + pb) * 0.5,
                        normal,
                        penetration: depth,
                    }]) {
                        best_depth = depth;
                        best = Some(m);
                    }
                }
            }
        }
    }
    best
}

/// 面接触兜底：用三角形平面法线与对方沿该方向的支持点构造接触。
fn triangle_plane_fallback(
    tri: [Vec3; 3],
    other: &Shape,
    t_other: &Transform,
) -> Option<ContactManifold> {
    let mut n = (tri[1] - tri[0]).cross(tri[2] - tri[0]);
    if n.length_squared() < 1e-16 {
        return None;
    }
    n = n.normalize_or_zero();
    let sup = Support::new(other, t_other);
    let p = sup.support(-n);
    let sd = n.dot(p - tri[0]);
    if sd >= 0.0 {
        return None;
    }
    // 最近特征必须落在三角形内部（投影在面内），否则交给边兜底。
    let (q, _w) = gjk::closest_on_triangle(tri[0] - p, tri[1] - p, tri[2] - p);
    let q = q + p;
    let lateral = (p - q).length();
    let projected_inside = lateral <= -sd + 1e-3;
    let vertex_inside = tri
        .iter()
        .any(|v| gjk::distance_point_to_shape(other, t_other, *v) <= 0.0);
    if !projected_inside && !vertex_inside {
        return None;
    }
    let position = p - n * sd;
    build_manifold([ContactPoint {
        position,
        normal: n,
        penetration: -sd,
    }])
}

/// 三角网格（A）与其他形状（B）。法线由网格指向对方。
fn trimesh_vs_shape(
    vertices: &[Vec3],
    indices: &[u32],
    t_mesh: &Transform,
    other: &Shape,
    t_other: &Transform,
) -> Option<ContactManifold> {
    let other_aabb = other
        .local_aabb()
        .transformed_conservative(&t_other.matrix());
    let mut points: SmallVec<ContactPoint, MAX_CANDIDATES> = SmallVec::new();
    for tri in indices.chunks_exact(3) {
        let (ia, ib, ic) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
        if ia >= vertices.len() || ib >= vertices.len() || ic >= vertices.len() {
            continue;
        }
        let a = t_mesh.transform_point(vertices[ia]);
        let b = t_mesh.transform_point(vertices[ib]);
        let c = t_mesh.transform_point(vertices[ic]);
        let mut tri_aabb = Aabb::EMPTY;
        tri_aabb.expand_point(a);
        tri_aabb.expand_point(b);
        tri_aabb.expand_point(c);
        if !tri_aabb.intersects(&other_aabb) {
            continue;
        }
        if let Some(m) = triangle_vs_shape([a, b, c], other, t_other) {
            points.extend_from_slice(&m.points);
        }
    }
    build_manifold(&points)
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

/// 计算两个形状之间的接触流形（法线由 A 指向 B）。
///
/// 静态-静态组合（半空间-半空间、网格-网格）直接返回 `None`。
pub(crate) fn collide_shapes(
    sa: &Shape,
    ta: &Transform,
    sb: &Shape,
    tb: &Transform,
) -> Option<ContactManifold> {
    let plane_a = if let Shape::Plane { normal } = sa {
        Some(*normal)
    } else {
        None
    };
    let plane_b = if let Shape::Plane { normal } = sb {
        Some(*normal)
    } else {
        None
    };
    match (plane_a, plane_b) {
        (Some(_), Some(_)) => return None,
        (Some(n), None) => return shape_vs_plane(sb, tb, n, ta).map(|m| m.flipped()),
        (None, Some(n)) => return shape_vs_plane(sa, ta, n, tb),
        (None, None) => {}
    }

    if let Shape::TriMesh { vertices, indices } = sb {
        if matches!(sa, Shape::TriMesh { .. }) {
            return None;
        }
        return trimesh_vs_shape(vertices, indices, tb, sa, ta).map(|m| m.flipped());
    }
    if let Shape::TriMesh { vertices, indices } = sa {
        return trimesh_vs_shape(vertices, indices, ta, sb, tb);
    }

    // 球体快速路径
    if let Shape::Sphere { radius } = sa {
        let r = radius.abs();
        match sb {
            Shape::Sphere { radius: r2 } => return sphere_sphere(r, ta, r2.abs(), tb),
            Shape::Box { half_extents } => return sphere_box(r, ta, *half_extents, tb),
            Shape::Capsule {
                half_height,
                radius: cr,
            } => return sphere_capsule(r, ta, *half_height, cr.abs(), tb),
            _ => return convex_pair(sa, ta, sb, tb),
        }
    }
    if let Shape::Sphere { radius } = sb {
        let r = radius.abs();
        match sa {
            Shape::Box { half_extents } => {
                return sphere_box(r, tb, *half_extents, ta).map(|m| m.flipped())
            }
            Shape::Capsule {
                half_height,
                radius: cr,
            } => return sphere_capsule(r, tb, *half_height, cr.abs(), ta).map(|m| m.flipped()),
            _ => return convex_pair(sa, ta, sb, tb),
        }
    }

    // 盒-盒
    if let (Shape::Box { half_extents: ha }, Shape::Box { half_extents: hb }) = (sa, sb) {
        return box_box(*ha, ta, *hb, tb);
    }

    // 其余凸-凸
    convex_pair(sa, ta, sb, tb)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, y: f32, z: f32) -> Transform {
        Transform::from_translation(vec3(x, y, z))
    }

    fn plane_y0() -> (Shape, Transform) {
        (Shape::Plane { normal: Vec3::Y }, Transform::IDENTITY)
    }

    #[test]
    fn sphere_plane_contact_points_down() {
        let (p, tp) = plane_y0();
        let s = Shape::Sphere { radius: 0.5 };
        let m = collide_shapes(&s, &at(0.0, 0.4, 0.0), &p, &tp).expect("应接触");
        assert_eq!(m.len(), 1);
        assert!((m.points[0].penetration - 0.1).abs() < 1e-5);
        assert!((m.points[0].normal - Vec3::NEG_Y).length() < 1e-5);
    }

    #[test]
    fn box_plane_four_points() {
        let (p, tp) = plane_y0();
        let b = Shape::Box {
            half_extents: Vec3::splat(0.5),
        };
        let m = collide_shapes(&b, &at(0.0, 0.45, 0.0), &p, &tp).expect("应接触");
        assert_eq!(m.len(), 4, "盒-面应有 4 个接触点");
        for cp in &m.points {
            assert!((cp.penetration - 0.05).abs() < 1e-5);
            assert!(cp.normal.y < -0.99);
        }
    }

    #[test]
    fn box_box_face_contact() {
        let a = Shape::Box {
            half_extents: Vec3::splat(0.5),
        };
        let b = Shape::Box {
            half_extents: Vec3::splat(0.5),
        };
        let m = collide_shapes(&a, &at(0.0, 0.0, 0.0), &b, &at(0.0, 0.9, 0.0)).expect("应接触");
        assert_eq!(m.len(), 4, "面-面接触应有 4 点，实际 {}", m.len());
        for cp in &m.points {
            assert!(
                (cp.penetration - 0.1).abs() < 1e-3,
                "穿透 {}",
                cp.penetration
            );
            assert!(cp.normal.y > 0.99, "法线 {:?}", cp.normal);
        }
    }

    #[test]
    fn box_box_corner_contact() {
        let a = Shape::Box {
            half_extents: Vec3::splat(0.5),
        };
        let b = Shape::Box {
            half_extents: Vec3::splat(0.5),
        };
        let m = collide_shapes(&a, &at(0.0, 0.0, 0.0), &b, &at(0.9, 0.9, 0.0)).expect("应接触");
        assert!(!m.is_empty());
        for cp in &m.points {
            assert!(cp.penetration > 0.0 && cp.penetration < 0.11);
        }
    }

    #[test]
    fn separated_shapes_produce_nothing() {
        let (p, tp) = plane_y0();
        let s = Shape::Sphere { radius: 0.5 };
        assert!(collide_shapes(&s, &at(0.0, 0.6, 0.0), &p, &tp).is_none());
        let b = Shape::Box {
            half_extents: Vec3::splat(0.5),
        };
        assert!(collide_shapes(&b, &at(3.0, 0.0, 0.0), &b, &at(0.0, 0.0, 0.0)).is_none());
    }

    #[test]
    fn capsule_plane_two_points() {
        let (p, tp) = plane_y0();
        let cap = Shape::Capsule {
            half_height: 0.5,
            radius: 0.2,
        };
        let t = Transform::new(
            vec3(0.0, 0.15, 0.0),
            Quat::from_rotation_z(core::f32::consts::FRAC_PI_2),
            Vec3::ONE,
        );
        let m = collide_shapes(&cap, &t, &p, &tp).expect("应接触");
        assert_eq!(m.len(), 2, "水平胶囊应有 2 点");
        for cp in &m.points {
            assert!((cp.penetration - 0.05).abs() < 1e-4);
        }
    }

    #[test]
    fn trimesh_floor_supports_sphere() {
        let mesh = Shape::TriMesh {
            vertices: vec![
                vec3(-10.0, 0.0, -10.0),
                vec3(10.0, 0.0, -10.0),
                vec3(10.0, 0.0, 10.0),
                vec3(-10.0, 0.0, 10.0),
            ],
            indices: vec![0, 1, 2, 0, 2, 3],
        };
        let s = Shape::Sphere { radius: 0.5 };
        let m =
            collide_shapes(&mesh, &Transform::IDENTITY, &s, &at(-5.0, 0.45, -8.0)).expect("应接触");
        assert!(!m.is_empty());
        // 法线由网格指向球（+Y）。
        assert!(m.points[0].normal.y > 0.9, "{:?}", m.points[0].normal);
        assert!(m.points[0].penetration > 0.0 && m.points[0].penetration < 0.1);
    }

    #[test]
    fn cylinder_plane_spreads_points() {
        let (p, tp) = plane_y0();
        let cyl = Shape::Cylinder {
            half_height: 0.5,
            radius: 0.5,
        };
        let m = collide_shapes(&cyl, &at(0.0, 0.45, 0.0), &p, &tp).expect("应接触");
        assert_eq!(m.len(), 4);
        let mut angles: Vec<f32> = m
            .points
            .iter()
            .map(|p| p.position.x.atan2(p.position.z))
            .collect();
        angles.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for w in angles.windows(2) {
            assert!((w[1] - w[0]).abs() > 1.0, "接触点过于集中：{angles:?}");
        }
    }
}
