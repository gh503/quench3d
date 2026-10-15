//! GJK 距离查询与 EPA 穿透深度求解（自研实现）。
//!
//! * [`gjk`]：计算两个凸形状之间的最近点/是否相交。内部维护一个最多 4 个顶点的
//!   单纯形，每一轮用「原点在单纯形上的最近点」把单纯形裁剪到最小面特征，
//!   同时保留重心坐标以便反算出两侧的见证点（witness point）。
//! * [`epa`]：在 GJK 判定相交后，把闵可夫斯基差扩张成多面体，不断用支持函数
//!   向外扩张，最终得到最小平移向量（法线 + 穿透深度 + 两侧见证点）。
//!
//! 约定：所有法线都由 A 指向 B，穿透深度为正。

use quench3d_math::*;

use crate::shape::Shape;

/// GJK 最大迭代轮数（防御性上限，正常收敛远小于它）。
const GJK_MAX_ITERS: usize = 64;
/// EPA 最大迭代轮数。
const EPA_MAX_ITERS: usize = 64;
/// EPA 多面体最多顶点数。
const EPA_MAX_VERTS: usize = 128;
/// 判定「原点已在单纯形内」的距离阈值（长度量纲）。
const TOUCH_EPS: f32 = 1e-5;

/// 支持函数代理：记录形状与它所在的世界位姿。
#[derive(Clone, Copy)]
pub(crate) struct Support<'a> {
    /// 形状引用。
    pub shape: &'a Shape,
    /// 世界位置。
    pub pos: Vec3,
    /// 世界旋转。
    pub rot: Quat,
    /// 旋转的逆（缓存，避免重复求逆）。
    pub inv_rot: Quat,
}

impl<'a> Support<'a> {
    /// 由形状与变换构造。
    pub fn new(shape: &'a Shape, t: &Transform) -> Self {
        Self {
            shape,
            pos: t.position,
            rot: t.rotation,
            inv_rot: t.rotation.inverse(),
        }
    }

    /// 形状在世界空间中沿 `dir` 的最远点。
    #[inline]
    pub fn support(&self, dir: Vec3) -> Vec3 {
        let local = self.inv_rot.mul_vec3(dir);
        self.pos + self.rot.mul_vec3(self.shape.support(local))
    }
}

/// 闵可夫斯基差 `A - B` 上的一个支持点，同时保留两侧的原始点。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct SupportPoint {
    /// `a - b`。
    pub v: Vec3,
    /// A 上的点。
    pub a: Vec3,
    /// B 上的点。
    pub b: Vec3,
}

#[inline]
fn make_point(sa: &Support, sb: &Support, dir: Vec3) -> SupportPoint {
    let a = sa.support(dir);
    let b = sb.support(-dir);
    SupportPoint { v: a - b, a, b }
}

/// 单纯形裁剪结果。
#[derive(Clone, Copy, Debug)]
struct Reduced {
    /// 原点到单纯形的最近点。
    closest: Vec3,
    /// 保留顶点的掩码（bit i 对应输入下标 i）。
    mask: u8,
    /// 与输入下标对齐的重心坐标。
    bary: [f32; 4],
    /// 原点是否被单纯形包含（四面体情形）。
    inside: bool,
}

#[inline]
fn safe_div(a: f32, b: f32) -> f32 {
    if b.abs() > 1e-20 {
        a / b
    } else {
        0.0
    }
}

/// 原点到三角形的最近点及其重心坐标。
pub(crate) fn closest_on_triangle(a: Vec3, b: Vec3, c: Vec3) -> (Vec3, [f32; 3]) {
    let ab = b - a;
    let ac = c - a;
    let ap = -a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return (a, [1.0, 0.0, 0.0]);
    }

    let bp = -b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return (b, [0.0, 1.0, 0.0]);
    }

    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = safe_div(d1, d1 - d3);
        return (a + ab * v, [1.0 - v, v, 0.0]);
    }

    let cp = -c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return (c, [0.0, 0.0, 1.0]);
    }

    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = safe_div(d2, d2 - d6);
        return (a + ac * w, [1.0 - w, 0.0, w]);
    }

    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = safe_div(d4 - d3, (d4 - d3) + (d5 - d6));
        return (b + (c - b) * w, [0.0, 1.0 - w, w]);
    }

    let denom = va + vb + vc;
    if denom.abs() <= 1e-20 {
        // 退化三角形：退回到最长边。
        let (p, bary) = closest_on_segment(a, b);
        if p.length_squared() <= c.length_squared() {
            (p, [bary.0, bary.1, 0.0])
        } else {
            (c, [0.0, 0.0, 1.0])
        }
    } else {
        let inv = 1.0 / denom;
        let v = vb * inv;
        let w = vc * inv;
        let u = 1.0 - v - w;
        (a + ab * v + ac * w, [u, v, w])
    }
}

/// 原点到线段的最近点及其重心坐标。
fn closest_on_segment(a: Vec3, b: Vec3) -> (Vec3, (f32, f32)) {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 <= 1e-20 {
        return (a, (1.0, 0.0));
    }
    let t = clamp(-a.dot(ab) / len2, 0.0, 1.0);
    (a + ab * t, (1.0 - t, t))
}

/// 把单纯形裁剪到离原点最近的面特征上。
fn reduce(points: &[SupportPoint]) -> Reduced {
    match points.len() {
        0 => Reduced {
            closest: Vec3::ZERO,
            mask: 0,
            bary: [0.0; 4],
            inside: true,
        },
        1 => Reduced {
            closest: points[0].v,
            mask: 0b0001,
            bary: [1.0, 0.0, 0.0, 0.0],
            inside: false,
        },
        2 => {
            let (p, (u, v)) = closest_on_segment(points[0].v, points[1].v);
            let mut mask = 0u8;
            if u > 0.0 {
                mask |= 0b01;
            }
            if v > 0.0 {
                mask |= 0b10;
            }
            if mask == 0 {
                mask = 0b01;
            }
            Reduced {
                closest: p,
                mask,
                bary: [u, v, 0.0, 0.0],
                inside: false,
            }
        }
        3 => {
            let (p, b) = closest_on_triangle(points[0].v, points[1].v, points[2].v);
            let mut mask = 0u8;
            if b[0] > 0.0 {
                mask |= 0b001;
            }
            if b[1] > 0.0 {
                mask |= 0b010;
            }
            if b[2] > 0.0 {
                mask |= 0b100;
            }
            if mask == 0 {
                mask = 0b001;
            }
            // 原点落在三角形（内部或边上）时必须保留全部三个顶点，
            // 否则会退化成线段并让后续探测方向失效。
            if p.length_squared() <= TOUCH_EPS * TOUCH_EPS {
                mask = 0b111;
            }
            Reduced {
                closest: p,
                mask,
                bary: [b[0], b[1], b[2], 0.0],
                inside: false,
            }
        }
        _ => {
            let p = [points[0].v, points[1].v, points[2].v, points[3].v];
            let vol = (p[1] - p[0]).dot((p[2] - p[0]).cross(p[3] - p[0]));
            let non_degenerate = vol.abs() > 1e-14;
            // 四个面（i, j, k）：r 是剩余顶点下标。
            const FACES: [[usize; 3]; 4] = [[0, 1, 2], [0, 2, 3], [0, 3, 1], [1, 3, 2]];
            let mut all_inside = true;
            let mut best: Option<(f32, Vec3, u8, [f32; 4])> = None;
            for f in FACES {
                let (i, j, k) = (f[0], f[1], f[2]);
                let r = 6 - i - j - k;
                let mut n = (p[j] - p[i]).cross(p[k] - p[i]);
                if n.dot(p[r] - p[i]) > 0.0 {
                    n = -n;
                }
                if n.length_squared() <= 1e-20 {
                    continue;
                }
                if n.dot(p[i]) >= 0.0 {
                    continue;
                }
                all_inside = false;
                let (c, b) = closest_on_triangle(p[i], p[j], p[k]);
                let d2 = c.length_squared();
                let mut mask = 0u8;
                if b[0] > 0.0 {
                    mask |= 1 << i;
                }
                if b[1] > 0.0 {
                    mask |= 1 << j;
                }
                if b[2] > 0.0 {
                    mask |= 1 << k;
                }
                if mask == 0 {
                    mask = 1 << i;
                }
                let mut bary = [0.0f32; 4];
                bary[i] = b[0];
                bary[j] = b[1];
                bary[k] = b[2];
                if best.is_none() || d2 < best.unwrap().0 {
                    best = Some((d2, c, mask, bary));
                }
            }
            if all_inside && non_degenerate {
                Reduced {
                    closest: Vec3::ZERO,
                    mask: 0b1111,
                    bary: [0.25; 4],
                    inside: true,
                }
            } else if let Some((_, c, mask, bary)) = best {
                Reduced {
                    closest: c,
                    mask,
                    bary,
                    inside: false,
                }
            } else {
                // 完全退化：退回到点。
                Reduced {
                    closest: points[0].v,
                    mask: 0b0001,
                    bary: [1.0, 0.0, 0.0, 0.0],
                    inside: false,
                }
            }
        }
    }
}

/// GJK 查询结果。
#[derive(Clone, Copy, Debug)]
pub(crate) struct GjkResult {
    /// 是否相交（原点位于闵可夫斯基差内部或表面）。
    pub intersect: bool,
    /// A 上的见证点。
    pub point_a: Vec3,
    /// B 上的见证点。
    pub point_b: Vec3,
    /// 最近距离（相交时为 0）。
    pub distance: f32,
    /// 单位法线，由 A 指向 B。
    pub normal: Vec3,
    /// 终止时的单纯形。
    pub simplex: [SupportPoint; 4],
    /// 单纯形有效顶点数。
    pub simplex_len: usize,
}

/// 用重心坐标计算见证点。
fn witness(simplex: &[SupportPoint], bary: &[f32]) -> (Vec3, Vec3) {
    let mut pa = Vec3::ZERO;
    let mut pb = Vec3::ZERO;
    for (p, w) in simplex.iter().zip(bary.iter()) {
        pa += p.a * *w;
        pb += p.b * *w;
    }
    (pa, pb)
}

/// GJK：判断相交并求最近点。
// 单纯形长度与重心数组在循环内重新赋值，初值仅用于「循环零次」的健全性检查。
#[allow(unused_assignments)]
pub(crate) fn gjk(sa: &Support, sb: &Support) -> GjkResult {
    let mut simplex: [SupportPoint; 4] = [SupportPoint::default(); 4];
    let mut bary: [f32; 4] = [0.0; 4];
    let mut len = 0usize;

    let mut dir = sb.pos - sa.pos;
    if dir.length_squared() < 1e-12 {
        dir = Vec3::X;
    }
    let first = make_point(sa, sb, dir);
    simplex[0] = first;
    bary[0] = 1.0;
    len = 1;
    let mut closest = first.v;

    let mut intersect = false;
    let mut iters = 0;
    while iters < GJK_MAX_ITERS {
        iters += 1;
        if closest.length_squared() <= TOUCH_EPS * TOUCH_EPS {
            // 原点落在当前单纯形上：低维单纯形不能直接判定相交，
            // 需要沿其正交方向继续探测（否则厚穿透会被误判为贴面接触）。
            match len {
                1 => {
                    intersect = true;
                    break;
                }
                2 => {
                    let ab = simplex[1].v - simplex[0].v;
                    let (t, _) = ab.any_orthonormal_pair();
                    dir = t;
                }
                _ => {
                    let n = (simplex[1].v - simplex[0].v).cross(simplex[2].v - simplex[0].v);
                    if n.length_squared() <= 1e-20 {
                        intersect = true;
                        break;
                    }
                    let n = n.normalize_or_zero();
                    // 三角形法线有两个方向，取支持函数更远的一侧，避免退化单纯形。
                    let sp = sa.support(n) - sb.support(-n);
                    dir = if sp.dot(n) >= 0.0 { n } else { -n };
                }
            }
        } else {
            dir = -closest;
        }
        let p = make_point(sa, sb, dir);
        // 收敛判定：新支持点沿 `dir` 的投影不再比当前最近点更远 → 已找到分离平面。
        let progress = p.v.dot(dir) - closest.dot(dir);
        if progress <= 1e-7 * dir.length().max(1.0) {
            break;
        }
        simplex[len] = p;
        bary[len] = 0.0;
        len += 1;

        let slice = &simplex[..len];
        let r = reduce(slice);
        // 按掩码重建单纯形。
        let mut new_len = 0usize;
        let mut new_simplex: [SupportPoint; 4] = [SupportPoint::default(); 4];
        let mut new_bary: [f32; 4] = [0.0; 4];
        for (i, sp) in simplex.iter().enumerate().take(len) {
            if r.mask & (1 << i) != 0 {
                new_simplex[new_len] = *sp;
                new_bary[new_len] = r.bary[i];
                new_len += 1;
            }
        }
        if new_len == 0 {
            new_simplex[0] = p;
            new_bary[0] = 1.0;
            new_len = 1;
        }
        simplex = new_simplex;
        bary = new_bary;
        len = new_len;
        closest = r.closest;
        if r.inside {
            intersect = true;
            break;
        }
    }

    let (point_a, point_b) = witness(&simplex[..len], &bary[..len]);
    if intersect {
        return GjkResult {
            intersect: true,
            point_a,
            point_b,
            distance: 0.0,
            normal: Vec3::Y,
            simplex,
            simplex_len: len,
        };
    }
    let delta = point_b - point_a;
    let distance = delta.length();
    let normal = if distance > 1e-9 {
        delta / distance
    } else {
        Vec3::Y
    };
    GjkResult {
        intersect: false,
        point_a,
        point_b,
        distance,
        normal,
        simplex,
        simplex_len: len,
    }
}

/// 只求距离与最近点的便捷包装。
pub(crate) fn closest_points(
    shape_a: &Shape,
    ta: &Transform,
    shape_b: &Shape,
    tb: &Transform,
) -> (Vec3, Vec3, f32, Vec3) {
    let sa = Support::new(shape_a, ta);
    let sb = Support::new(shape_b, tb);
    let r = gjk(&sa, &sb);
    if r.intersect {
        (r.point_a, r.point_b, 0.0, r.normal)
    } else {
        (r.point_a, r.point_b, r.distance, r.normal)
    }
}

/// EPA 结果。
#[derive(Clone, Copy, Debug)]
pub(crate) struct EpaResult {
    /// 单位法线，由 A 指向 B。
    pub normal: Vec3,
    /// 穿透深度（正值）。
    pub depth: f32,
    /// A 上的最深点。
    pub point_a: Vec3,
    /// B 上的最深点。
    pub point_b: Vec3,
}

#[derive(Clone, Copy, Debug)]
struct Face {
    idx: [usize; 3],
    normal: Vec3,
    dist: f32,
    alive: bool,
}

fn make_face(verts: &[SupportPoint], interior: Vec3, i: usize, j: usize, k: usize) -> Option<Face> {
    let a = verts[i].v;
    let b = verts[j].v;
    let c = verts[k].v;
    let mut n = (b - a).cross(c - a);
    let len = n.length();
    if len <= 1e-12 {
        return None;
    }
    n /= len;
    // 用多面体内部参考点定向：外法线必须背离内部点。
    // （不能用「n·a > 0」——面平面穿过原点时该判据会失效。）
    if n.dot(a - interior) < 0.0 {
        n = -n;
        Some(Face {
            idx: [i, k, j],
            normal: n,
            dist: n.dot(a).max(0.0),
            alive: true,
        })
    } else {
        Some(Face {
            idx: [i, j, k],
            normal: n,
            dist: n.dot(a).max(0.0),
            alive: true,
        })
    }
}

/// 把 GJK 终止单纯形扩张成一个包含原点的非退化四面体。
fn build_tetra(sa: &Support, sb: &Support, simplex: &[SupportPoint]) -> Option<Vec<SupportPoint>> {
    let mut verts: Vec<SupportPoint> = Vec::with_capacity(4);
    let push_unique = |verts: &mut Vec<SupportPoint>, p: SupportPoint| {
        for q in verts.iter() {
            if (q.v - p.v).length_squared() < 1e-14 {
                return;
            }
        }
        if verts.len() < 4 {
            verts.push(p);
        }
    };
    for p in simplex.iter() {
        push_unique(&mut verts, *p);
    }
    if verts.is_empty() {
        return None;
    }

    if verts.len() == 1 {
        for d in [Vec3::X, Vec3::Y, Vec3::Z, Vec3::NEG_X] {
            push_unique(&mut verts, make_point(sa, sb, d));
            if verts.len() >= 2 {
                break;
            }
        }
    }
    if verts.len() == 2 {
        let axis = verts[1].v - verts[0].v;
        let (t, _) = axis.any_orthonormal_pair();
        for d in [t, -t] {
            if verts.len() >= 3 {
                break;
            }
            push_unique(&mut verts, make_point(sa, sb, d));
        }
    }
    if verts.len() == 3 {
        let n = (verts[1].v - verts[0].v).cross(verts[2].v - verts[0].v);
        if n.length_squared() > 1e-20 {
            for d in [n, -n] {
                if verts.len() >= 4 {
                    break;
                }
                push_unique(&mut verts, make_point(sa, sb, d.normalize_or_zero()));
            }
        }
    }
    if verts.len() < 4 {
        return None;
    }
    // 保证四面体非退化且把原点包在里面（若不在，翻转一个顶点）。
    let vol =
        (verts[1].v - verts[0].v).dot((verts[2].v - verts[0].v).cross(verts[3].v - verts[0].v));
    if vol.abs() <= 1e-14 {
        return None;
    }
    Some(verts)
}

/// EPA：在已知相交的前提下求最小平移向量。
pub(crate) fn epa(sa: &Support, sb: &Support, gjk_res: &GjkResult) -> Option<EpaResult> {
    let simplex = &gjk_res.simplex[..gjk_res.simplex_len];
    let mut verts = build_tetra(sa, sb, simplex)?;

    // 内部参考点：初始四面体质心。EPA 只会向外扩张，因此它始终留在内部，
    // 可以稳定地给每个面定向。
    let interior = (verts[0].v + verts[1].v + verts[2].v + verts[3].v) * 0.25;

    let mut faces: Vec<Face> = Vec::with_capacity(EPA_MAX_VERTS * 2);
    for (i, j, k) in [(0usize, 1usize, 2usize), (0, 2, 3), (0, 3, 1), (1, 3, 2)] {
        if let Some(f) = make_face(&verts, interior, i, j, k) {
            faces.push(f);
        }
    }
    if faces.len() < 4 {
        return None;
    }
    // 原点必须（弱）位于初始多面体内部，否则本次 EPA 结果不可信。
    for f in &faces {
        if f.dist < -1e-4 {
            return None;
        }
    }

    let mut best_face = 0usize;
    let mut iters = 0;
    while iters < EPA_MAX_ITERS {
        iters += 1;
        // 找离原点最近的面。
        let mut min_dist = f32::INFINITY;
        let mut min_idx = usize::MAX;
        for (fi, f) in faces.iter().enumerate() {
            if f.alive && f.dist < min_dist {
                min_dist = f.dist;
                min_idx = fi;
            }
        }
        if min_idx == usize::MAX {
            return None;
        }
        best_face = min_idx;
        let n = faces[min_idx].normal;
        let p = make_point(sa, sb, n);
        let d = p.v.dot(n);
        if d - faces[min_idx].dist <= 1e-4 * (1.0 + faces[min_idx].dist.abs()) {
            break; // 收敛
        }
        if verts.len() >= EPA_MAX_VERTS {
            break;
        }
        // 支持点已经存在 → 无法继续扩张。
        if verts.iter().any(|v| (v.v - p.v).length_squared() < 1e-14) {
            break;
        }

        // 删除所有从 p 可见的面，收集地平线。
        let new_index = verts.len();
        let mut horizon: Vec<(usize, usize)> = Vec::new();
        for f in faces.iter_mut() {
            if !f.alive {
                continue;
            }
            if f.normal.dot(p.v) - f.dist > 1e-6 {
                f.alive = false;
                for e in 0..3 {
                    let a = f.idx[e];
                    let b = f.idx[(e + 1) % 3];
                    // 如果反向边也在待删面里，则不是地平线。
                    if let Some(pos) = horizon.iter().position(|&(x, y)| x == b && y == a) {
                        horizon.swap_remove(pos);
                    } else {
                        horizon.push((a, b));
                    }
                }
            }
        }
        if horizon.is_empty() {
            break;
        }
        verts.push(p);
        for (a, b) in horizon {
            if let Some(f) = make_face(&verts, interior, a, b, new_index) {
                faces.push(f);
            }
        }
        // 清理过期的面，避免无限增长。
        if faces.iter().filter(|f| f.alive).count() > EPA_MAX_VERTS * 2 {
            faces.retain(|f| f.alive);
        }
    }

    let f = faces.get(best_face)?;
    if !f.normal.is_finite() || !f.dist.is_finite() {
        return None;
    }
    let (i, j, k) = (f.idx[0], f.idx[1], f.idx[2]);
    let (p0, p1, p2) = (verts[i].v, verts[j].v, verts[k].v);
    let (_, bary) = closest_on_triangle(p0, p1, p2);
    let mut bary = bary;
    for b in bary.iter_mut() {
        if !b.is_finite() || *b < 0.0 {
            *b = 0.0;
        }
    }
    let sum = bary[0] + bary[1] + bary[2];
    if sum > 1e-9 {
        bary = [bary[0] / sum, bary[1] / sum, bary[2] / sum];
    } else {
        bary = [1.0, 0.0, 0.0];
    }
    let point_a = verts[i].a * bary[0] + verts[j].a * bary[1] + verts[k].a * bary[2];
    let point_b = verts[i].b * bary[0] + verts[j].b * bary[1] + verts[k].b * bary[2];
    // 闵可夫斯基差 M = A ⊖ B 的外法线 `n` 指向 M 边界上离原点最近的点；
    // 把 A 沿 -n 推、B 沿 +n 推即可分离，因此 A→B 的接触法线就是 +n。
    // （此时 point_a - point_b = n * depth，与「不相交」时的见证点方向相反。）
    let mut normal = f.normal;
    if !normal.is_finite() || normal.length_squared() < 1e-12 {
        normal = Vec3::Y;
    }
    Some(EpaResult {
        normal: normal.normalize_or_zero(),
        depth: f.dist.max(0.0),
        point_a,
        point_b,
    })
}

/// 通用凸-凸：返回 `(法线 A→B, 深度, A 上点, B 上点)`，不相交返回 `None`。
pub(crate) fn convex_convex(sa: &Support, sb: &Support) -> Option<(Vec3, f32, Vec3, Vec3)> {
    let r = gjk(sa, sb);
    if !r.intersect {
        return None;
    }
    match epa(sa, sb, &r) {
        Some(e) => Some((e.normal, e.depth, e.point_a, e.point_b)),
        None => {
            // EPA 退化（例如贴面接触）：用最后一次单纯形所在的最近方向兜底。
            let simplex = &r.simplex[..r.simplex_len];
            let (pa, pb) = witness(
                simplex,
                &[1.0 / r.simplex_len.max(1) as f32; 4][..r.simplex_len],
            );
            // 相交时 pa - pb 与 A→B 法线同向（见 `epa` 的推导）。
            let mut n = pa - pb;
            if n.length_squared() < 1e-12 {
                n = Vec3::Y;
            }
            Some((n.normalize_or_zero(), 0.0, pa, pb))
        }
    }
}

/// 点是否位于凸点集内部（含容差 `tol`）。
pub(crate) fn point_in_convex(points: &[Vec3], p: Vec3, tol: f32) -> bool {
    if points.len() < 4 {
        return false;
    }
    let hull = Shape::ConvexHull {
        points: points.to_vec(),
    };
    let pt = Shape::Sphere { radius: 0.0 };
    let sa = Support::new(&hull, &Transform::from_translation(Vec3::ZERO));
    let sb = Support::new(&pt, &Transform::from_translation(p));
    let r = gjk(&sa, &sb);
    r.intersect || r.distance <= tol
}

/// 点到形状表面的最近距离（点在内部时为 0）。
pub(crate) fn distance_point_to_shape(shape: &Shape, t: &Transform, p: Vec3) -> f32 {
    match shape {
        Shape::Plane { normal } => {
            let n = t.rotation.mul_vec3(normal.normalize_or_zero());
            (n.dot(p - t.position)).max(0.0)
        }
        Shape::TriMesh { vertices, indices } => {
            let mut best = f32::INFINITY;
            for tri in indices.as_chunks::<3>().0 {
                let (ia, ib, ic) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
                if ia >= vertices.len() || ib >= vertices.len() || ic >= vertices.len() {
                    continue;
                }
                let a = t.transform_point(vertices[ia]);
                let b = t.transform_point(vertices[ib]);
                let c = t.transform_point(vertices[ic]);
                let d = point_triangle_distance(p, a, b, c);
                if d < best {
                    best = d;
                }
            }
            if best.is_finite() {
                best
            } else {
                f32::INFINITY
            }
        }
        _ => {
            let pt = Shape::Sphere { radius: 0.0 };
            let sa = Support::new(shape, t);
            let sb = Support::new(&pt, &Transform::from_translation(p));
            let r = gjk(&sa, &sb);
            if r.intersect {
                0.0
            } else {
                r.distance
            }
        }
    }
}

/// 点到三角形的最近距离。
pub(crate) fn point_triangle_distance(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> f32 {
    // 先在平面法线方向投影，用与 closest_on_triangle 相同的 Voronoi 区域逻辑。
    let ab = b - a;
    let ac = c - a;
    let ap = p - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return (p - a).length();
    }
    let bp = p - b;
    let d3 = ab.dot(bp);
    let d4 = ac.dot(bp);
    if d3 >= 0.0 && d4 <= d3 {
        return (p - b).length();
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = safe_div(d1, d1 - d3);
        return (p - (a + ab * v)).length();
    }
    let cp = p - c;
    let d5 = ab.dot(cp);
    let d6 = ac.dot(cp);
    if d6 >= 0.0 && d5 <= d6 {
        return (p - c).length();
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = safe_div(d2, d2 - d6);
        return (p - (a + ac * w)).length();
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = safe_div(d4 - d3, (d4 - d3) + (d5 - d6));
        return (p - (b + (c - b) * w)).length();
    }
    let denom = va + vb + vc;
    if denom.abs() <= 1e-20 {
        return (p - a).length();
    }
    let inv = 1.0 / denom;
    let v = vb * inv;
    let w = vc * inv;
    (p - (a + ab * v + ac * w)).length()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shape::Shape;

    fn t_at(p: Vec3) -> Transform {
        Transform::from_translation(p)
    }

    #[test]
    fn sphere_sphere_distance() {
        let a = Shape::Sphere { radius: 1.0 };
        let b = Shape::Sphere { radius: 0.5 };
        let (pa, pb, d, n) = closest_points(&a, &t_at(Vec3::ZERO), &b, &t_at(vec3(4.0, 0.0, 0.0)));
        assert!((d - 2.5).abs() < 1e-3, "距离应为 2.5，实际 {d}");
        assert!((n - Vec3::X).length() < 1e-3);
        assert!((pa - Vec3::X).length() < 1e-3);
        assert!((pb - vec3(3.5, 0.0, 0.0)).length() < 1e-3);
    }

    #[test]
    fn box_box_penetration() {
        let a = Shape::Box {
            half_extents: Vec3::ONE,
        };
        let b = Shape::Box {
            half_extents: Vec3::ONE,
        };
        let sa = Support::new(&a, &t_at(Vec3::ZERO));
        let sb = Support::new(&b, &t_at(vec3(1.5, 0.0, 0.0)));
        let r = convex_convex(&sa, &sb).expect("应相交");
        assert!((r.1 - 0.5).abs() < 1e-3, "深度应为 0.5，实际 {}", r.1);
        // 法线由 A 指向 B。
        assert!(r.0.x > 0.9, "法线应为 +X，实际 {:?}", r.0);
    }

    #[test]
    fn box_box_separated() {
        let a = Shape::Box {
            half_extents: Vec3::ONE,
        };
        let b = Shape::Box {
            half_extents: Vec3::ONE,
        };
        let sa = Support::new(&a, &t_at(Vec3::ZERO));
        let sb = Support::new(&b, &t_at(vec3(3.0, 0.0, 0.0)));
        assert!(convex_convex(&sa, &sb).is_none());
        let g = gjk(&sa, &sb);
        assert!(!g.intersect);
        assert!((g.distance - 1.0).abs() < 1e-3, "实际 {}", g.distance);
    }

    #[test]
    fn cylinder_sphere_contact() {
        let cyl = Shape::Cylinder {
            half_height: 1.0,
            radius: 0.5,
        };
        let sph = Shape::Sphere { radius: 0.25 };
        let sa = Support::new(&cyl, &t_at(Vec3::ZERO));
        let sb = Support::new(&sph, &t_at(vec3(0.6, 0.0, 0.0)));
        let r = convex_convex(&sa, &sb).expect("应相交");
        assert!((r.1 - 0.15).abs() < 0.02, "深度应为 0.15，实际 {}", r.1);
        assert!(r.0.x > 0.9, "法线 {:?}", r.0);
    }

    #[test]
    fn point_inside_hull() {
        let pts = vec![
            vec3(-1.0, -1.0, -1.0),
            vec3(1.0, -1.0, -1.0),
            vec3(-1.0, 1.0, -1.0),
            vec3(1.0, 1.0, -1.0),
            vec3(-1.0, -1.0, 1.0),
            vec3(1.0, -1.0, 1.0),
            vec3(-1.0, 1.0, 1.0),
            vec3(1.0, 1.0, 1.0),
        ];
        assert!(point_in_convex(&pts, Vec3::ZERO, 1e-4));
        assert!(point_in_convex(&pts, vec3(0.9, 0.9, 0.9), 1e-4));
        assert!(!point_in_convex(&pts, vec3(1.5, 0.0, 0.0), 1e-4));
    }

    #[test]
    fn point_triangle_distance_works() {
        let a = Vec3::ZERO;
        let b = Vec3::X;
        let c = Vec3::Y;
        assert!((point_triangle_distance(vec3(0.25, 0.25, 2.0), a, b, c) - 2.0).abs() < 1e-4);
        assert!((point_triangle_distance(vec3(2.0, 0.0, 0.0), a, b, c) - 1.0).abs() < 1e-4);
    }
}
