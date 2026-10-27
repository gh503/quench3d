//! 从粒子流体提取连续表面（Surface Nets / 对偶等值面）。
//!
//! PBF 的解是粒子，直接画球会看到一颗颗珠子；真实水面/水体需要连续网格。
//! 这里用**对偶等值面**而不是 256 项 Marching Cubes 表：
//!
//! 1. 把粒子按平滑核撒到规则网格标量场；
//! 2. 每个跨越等值面的格子取一个顶点（12 条棱的交点平均）；
//! 3. 每条跨越等值面的网格棱连接周围 4 个格子成一个四边形（两个三角形）。
//!
//! 这样不需要查表、不会产生裂缝，代价是顶点位置略偏离真实等值面（对偶面）。
//! 结果不依赖随机数，粒子顺序固定时逐位可复现。

use quench3d_math::{Aabb, Vec3};

/// 提取出的流体表面网格（世界坐标）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FluidSurfaceMesh {
    /// 顶点位置。
    pub positions: Vec<Vec3>,
    /// 顶点法线（由标量场梯度给出，指向流体外部）。
    pub normals: Vec<Vec3>,
    /// 三角形索引。
    pub indices: Vec<u32>,
}

impl FluidSurfaceMesh {
    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty() || self.indices.is_empty()
    }

    /// 三角形数量。
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// 从粒子位置提取表面。
///
/// * `radius`：平滑核半径（通常取 `FluidDesc::smoothing_radius()`）；
/// * `resolution`：最长轴的目标格数（4..=128），越大越细也越慢；
/// * `iso`：等值面阈值；粒子中心附近场值约 1，取 `0.35~0.6` 比较合适。
///
/// 粒子少于 4 颗、半径非正或分辨率太小时返回空网格（调用方保留旧画面）。
pub fn extract_surface(
    particles: &[Vec3],
    radius: f32,
    resolution: u32,
    iso: f32,
) -> FluidSurfaceMesh {
    if particles.len() < 4
        || !radius.is_finite()
        || radius <= 1e-5
        || !iso.is_finite()
        || resolution < 4
    {
        return FluidSurfaceMesh::default();
    }
    let mut bounds = Aabb::EMPTY;
    let mut finite_count = 0;
    for p in particles {
        if p.is_finite() {
            bounds.expand_point(*p);
            finite_count += 1;
        }
    }
    if finite_count < 4 || !bounds.is_valid() {
        return FluidSurfaceMesh::default();
    }
    let margin = radius * 2.0;
    bounds = bounds.grown(margin);
    let extent = bounds.max - bounds.min;
    let longest = extent.max_element();
    if !longest.is_finite() || longest <= 1e-5 {
        return FluidSurfaceMesh::default();
    }
    // 每格边长：让最长轴正好有 `resolution` 个格；至少 2 个格。
    let cell = longest / resolution.clamp(4, 128) as f32;
    let nx = ((extent.x / cell).ceil() as usize + 1).clamp(3, 192);
    let ny = ((extent.y / cell).ceil() as usize + 1).clamp(3, 192);
    let nz = ((extent.z / cell).ceil() as usize + 1).clamp(3, 192);
    let total = nx.saturating_mul(ny).saturating_mul(nz);
    if total > 2_000_000 {
        // 拒绝病态输入（例如一颗粒子飞出去把包围盒拉得极大），不让一次提取吃掉几 GB。
        return FluidSurfaceMesh::default();
    }

    let mut field = vec![0.0f32; total];
    let index = |x: usize, y: usize, z: usize| (z * ny + y) * nx + x;
    let inv_h2 = 1.0 / (radius * radius);
    // 只扫每颗粒子周围 (2*radius/cell + 1)³ 个格点；固定粒子顺序 → 结果确定。
    let reach = (radius / cell).ceil() as isize;
    for p in particles {
        let ix = (((p.x - bounds.min.x) / cell).floor() as isize).clamp(0, nx as isize - 1);
        let iy = (((p.y - bounds.min.y) / cell).floor() as isize).clamp(0, ny as isize - 1);
        let iz = (((p.z - bounds.min.z) / cell).floor() as isize).clamp(0, nz as isize - 1);
        for dz in -reach..=reach {
            let z = iz + dz;
            if z < 0 || z >= nz as isize {
                continue;
            }
            for dy in -reach..=reach {
                let y = iy + dy;
                if y < 0 || y >= ny as isize {
                    continue;
                }
                for dx in -reach..=reach {
                    let x = ix + dx;
                    if x < 0 || x >= nx as isize {
                        continue;
                    }
                    let gp = Vec3::new(
                        bounds.min.x + x as f32 * cell,
                        bounds.min.y + y as f32 * cell,
                        bounds.min.z + z as f32 * cell,
                    );
                    let r2 = (gp - *p).length_squared() * inv_h2;
                    if r2 < 1.0 {
                        let w = 1.0 - r2;
                        field[index(x as usize, y as usize, z as usize)] += w * w;
                    }
                }
            }
        }
    }

    // 1. 每个跨等值面的格子一个顶点（棱交点平均）。
    let cx = nx - 1;
    let cy = ny - 1;
    let cz = nz - 1;
    let mut cell_vertex = vec![u32::MAX; cx * cy * cz];
    let cell_index = |x: usize, y: usize, z: usize| (z * cy + y) * cx + x;
    let mut mesh = FluidSurfaceMesh::default();
    let corner = |x: usize, y: usize, z: usize| field[index(x, y, z)];
    // 12 条棱的两端（格内局部坐标）。
    const EDGES: [(usize, usize); 12] = [
        (0, 1),
        (1, 3),
        (2, 3),
        (0, 2),
        (4, 5),
        (5, 7),
        (6, 7),
        (4, 6),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];
    // 局部角编号：bit0=x, bit1=y, bit2=z。
    let pos_local =
        |c: usize| Vec3::new((c & 1) as f32, ((c >> 1) & 1) as f32, ((c >> 2) & 1) as f32);
    for z in 0..cz {
        for y in 0..cy {
            for x in 0..cx {
                let mut vals = [0.0f32; 8];
                for (c, v) in vals.iter_mut().enumerate() {
                    *v = corner(x + (c & 1), y + ((c >> 1) & 1), z + ((c >> 2) & 1));
                }
                let inside = vals.iter().filter(|v| **v >= iso).count();
                if inside == 0 || inside == 8 {
                    continue;
                }
                let mut sum = Vec3::ZERO;
                let mut hits = 0u32;
                for (a, b) in EDGES {
                    let (va, vb) = (vals[a], vals[b]);
                    if (va >= iso) == (vb >= iso) {
                        continue;
                    }
                    let t = ((iso - va) / (vb - va)).clamp(0.0, 1.0);
                    sum += pos_local(a).lerp(pos_local(b), t);
                    hits += 1;
                }
                if hits == 0 {
                    continue;
                }
                let local = sum / hits as f32;
                let p = bounds.min + Vec3::new(x as f32, y as f32, z as f32) * cell + local * cell;
                let n = field_gradient(&field, nx, ny, nz, x, y, z, local);
                let id = mesh.positions.len() as u32;
                mesh.positions.push(p);
                mesh.normals.push(n);
                cell_vertex[cell_index(x, y, z)] = id;
            }
        }
    }
    if mesh.positions.is_empty() {
        return mesh;
    }

    // 2. 每条跨等值面的网格棱连接周围四个格子。
    //    局部角的 `f` 在棱两端之间的符号变化决定四边形朝向。
    let mut quad = |a: u32, b: u32, c: u32, d: u32, flip: bool| {
        if a == u32::MAX || b == u32::MAX || c == u32::MAX || d == u32::MAX {
            return;
        }
        if flip {
            mesh.indices.extend_from_slice(&[a, c, b, a, d, c]);
        } else {
            mesh.indices.extend_from_slice(&[a, b, c, a, c, d]);
        }
    };
    for z in 1..cz {
        for y in 1..cy {
            for x in 0..cx {
                let f0 = corner(x, y, z);
                let f1 = corner(x + 1, y, z);
                if (f0 >= iso) == (f1 >= iso) {
                    continue;
                }
                quad(
                    cell_vertex[cell_index(x, y - 1, z - 1)],
                    cell_vertex[cell_index(x, y, z - 1)],
                    cell_vertex[cell_index(x, y, z)],
                    cell_vertex[cell_index(x, y - 1, z)],
                    f0 < iso,
                );
            }
        }
    }
    for z in 1..cz {
        for y in 0..cy {
            for x in 1..cx {
                let f0 = corner(x, y, z);
                let f1 = corner(x, y + 1, z);
                if (f0 >= iso) == (f1 >= iso) {
                    continue;
                }
                quad(
                    cell_vertex[cell_index(x - 1, y, z - 1)],
                    cell_vertex[cell_index(x - 1, y, z)],
                    cell_vertex[cell_index(x, y, z)],
                    cell_vertex[cell_index(x, y, z - 1)],
                    f0 < iso,
                );
            }
        }
    }
    for z in 0..cz {
        for y in 1..cy {
            for x in 1..cx {
                let f0 = corner(x, y, z);
                let f1 = corner(x, y, z + 1);
                if (f0 >= iso) == (f1 >= iso) {
                    continue;
                }
                quad(
                    cell_vertex[cell_index(x - 1, y - 1, z)],
                    cell_vertex[cell_index(x, y - 1, z)],
                    cell_vertex[cell_index(x, y, z)],
                    cell_vertex[cell_index(x - 1, y, z)],
                    f0 < iso,
                );
            }
        }
    }
    mesh
}

/// 标量场在格内局部坐标处的梯度（中心差分 + 三线性权重）。
// 维度/坐标本来就是七个独立参数；打包成结构体只会让调用点更难读。
#[allow(clippy::too_many_arguments)]
fn field_gradient(
    field: &[f32],
    nx: usize,
    ny: usize,
    nz: usize,
    x: usize,
    y: usize,
    z: usize,
    local: Vec3,
) -> Vec3 {
    let idx = |x: usize, y: usize, z: usize| (z * ny + y) * nx + x;
    let sample = |fx: f32, fy: f32, fz: f32| {
        let xx = (x as f32 + fx).clamp(0.0, nx as f32 - 1.0);
        let yy = (y as f32 + fy).clamp(0.0, ny as f32 - 1.0);
        let zz = (z as f32 + fz).clamp(0.0, nz as f32 - 1.0);
        let x0 = xx.floor() as usize;
        let y0 = yy.floor() as usize;
        let z0 = zz.floor() as usize;
        let x1 = (x0 + 1).min(nx - 1);
        let y1 = (y0 + 1).min(ny - 1);
        let z1 = (z0 + 1).min(nz - 1);
        let tx = xx - x0 as f32;
        let ty = yy - y0 as f32;
        let tz = zz - z0 as f32;
        let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
        let c00 = lerp(field[idx(x0, y0, z0)], field[idx(x1, y0, z0)], tx);
        let c10 = lerp(field[idx(x0, y1, z0)], field[idx(x1, y1, z0)], tx);
        let c01 = lerp(field[idx(x0, y0, z1)], field[idx(x1, y0, z1)], tx);
        let c11 = lerp(field[idx(x0, y1, z1)], field[idx(x1, y1, z1)], tx);
        lerp(lerp(c00, c10, ty), lerp(c01, c11, ty), tz)
    };
    let e = 0.25;
    let gx = sample(local.x + e, local.y, local.z) - sample(local.x - e, local.y, local.z);
    let gy = sample(local.x, local.y + e, local.z) - sample(local.x, local.y - e, local.z);
    let gz = sample(local.x, local.y, local.z + e) - sample(local.x, local.y, local.z - e);
    // 梯度指向场值增大（流体内部）；表面法线朝外，所以取负。
    -Vec3::new(gx, gy, gz).normalize_or_zero()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blob(offset: Vec3) -> Vec<Vec3> {
        let mut p = Vec::new();
        for z in -3..=3 {
            for y in -3..=3 {
                for x in -3..=3 {
                    if x * x + y * y + z * z <= 9 {
                        p.push(
                            Vec3::new(x as f32 * 0.05, y as f32 * 0.05, z as f32 * 0.05) + offset,
                        );
                    }
                }
            }
        }
        p
    }

    #[test]
    fn empty_or_degenerate_input_produces_no_mesh() {
        assert!(extract_surface(&[], 0.1, 16, 0.5).is_empty());
        assert!(extract_surface(&[Vec3::ZERO, Vec3::X], 0.1, 16, 0.5).is_empty());
        assert!(extract_surface(&blob(Vec3::ZERO), 0.0, 16, 0.5).is_empty());
        assert!(extract_surface(&blob(Vec3::ZERO), 0.1, 2, 0.5).is_empty());
    }

    #[test]
    fn a_blob_produces_a_closed_surface_inside_its_bounds() {
        let p = blob(Vec3::ZERO);
        let m = extract_surface(&p, 0.12, 32, 0.45);
        println!(
            "verts={} tris={} pos0={:?} poslast={:?}",
            m.positions.len(),
            m.triangle_count(),
            m.positions.first(),
            m.positions.last()
        );
        assert!(!m.is_empty(), "应提取出表面");
        assert_eq!(m.positions.len(), m.normals.len());
        assert_eq!(m.indices.len() % 3, 0);
        assert!(m.indices.iter().all(|i| (*i as usize) < m.positions.len()));
        assert!(m.positions.iter().all(|v| v.is_finite()));
        assert!(m.normals.iter().all(|n| (n.length() - 1.0).abs() < 1e-3));
        let mut b = Aabb::EMPTY;
        for v in &m.positions {
            b.expand_point(*v);
        }
        assert_eq!(m.triangle_count() * 3, m.indices.len());
        assert!(b.max.x > 0.0 && b.min.x < 0.0, "表面应包住 blob：{b:?}");
        assert!(
            m.positions.len() > 100,
            "32 格应产生足够多的顶点，实际 {}",
            m.positions.len()
        );
    }

    #[test]
    fn two_separated_blobs_span_both_centers() {
        let mut two_points = blob(Vec3::new(-0.5, 0.0, 0.0));
        two_points.extend(blob(Vec3::new(0.5, 0.0, 0.0)));
        let two = extract_surface(&two_points, 0.12, 48, 0.45);
        assert!(!two.is_empty());
        let mut b = Aabb::EMPTY;
        for v in &two.positions {
            b.expand_point(*v);
        }
        assert!(
            b.max.x - b.min.x > 0.9,
            "两团应跨越两边：{b:?}（顶点 {}）",
            two.positions.len()
        );
        assert!(two.triangle_count() > 100);
    }

    #[test]
    fn nonfinite_particles_do_not_turn_an_insufficient_cloud_into_a_surface() {
        let valid = [Vec3::ZERO, Vec3::X * 0.05, Vec3::Y * 0.05];
        assert!(extract_surface(&valid, 0.12, 24, 0.45).is_empty());
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut points = valid.to_vec();
            points.push(Vec3::new(invalid, 0.0, 0.0));
            assert!(extract_surface(&points, 0.12, 24, 0.45).is_empty());
        }
    }

    #[test]
    fn nonfinite_particles_preserve_valid_surface_bits() {
        let points = blob(Vec3::ZERO);
        let expected = extract_surface(&points, 0.12, 24, 0.45);
        assert!(!expected.is_empty());
        let bits = |vectors: &[Vec3]| {
            vectors
                .iter()
                .map(|v| [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()])
                .collect::<Vec<_>>()
        };
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut mixed = points.clone();
            mixed.insert(0, Vec3::new(invalid, 0.0, 0.0));
            mixed.push(Vec3::new(0.0, invalid, 0.0));
            mixed.insert(mixed.len() / 2, Vec3::new(0.0, 0.0, invalid));
            let actual = extract_surface(&mixed, 0.12, 24, 0.45);
            assert_eq!(bits(&actual.positions), bits(&expected.positions));
            assert_eq!(bits(&actual.normals), bits(&expected.normals));
            assert_eq!(actual.indices, expected.indices);
        }
    }

    #[test]
    fn extraction_is_deterministic() {
        let p = blob(Vec3::ZERO);
        assert_eq!(
            extract_surface(&p, 0.12, 24, 0.45),
            extract_surface(&p, 0.12, 24, 0.45)
        );
    }
}
