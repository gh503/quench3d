//! 把摄像头深度点云固化成可编辑的三角网。
//!
//! 点云本身只能看，不能选中/移动/导出。这里提供两种固化方式：
//! * **体素表面**：每个点落进一个体素，只为朝向空气的六个面生成四边形，棱角分明；
//! * **平滑表面**：复用流体的 Surface Nets 等值面提取，得到连续、带法线的表面。
//!
//! 纯 CPU、无第三方库；同样的点 + 体素尺寸逐位可复现。

use std::collections::HashSet;

use quench3d::render::{MeshData, Vertex};
use quench3d_math::{Vec2, Vec3};
use quench3d_physics::extract_surface;

/// 体素上限：超过就只保留最先出现的体素，避免一次扫描吃掉几 GB。
pub const MAX_VOXELS: usize = 300_000;

/// 平滑表面的最大格数（最长轴）；再高对摄像头点云没有意义，还会卡住 UI。
pub const MAX_SURFACE_CELLS: u32 = 160;

/// 去掉坐标两端各 `trim` 比例的离群点（默认 0.5%）。
///
/// 单目深度图经常有少量飞点；不剔掉会把整个包围盒撑大，Surface Nets 的格距
/// 随之变粗、主体糊成一团。按每轴独立分位数裁剪，保留同时落在三轴范围内的点；
/// 点数太少（<64）时原样返回，避免把稀疏轮廓裁没。
pub fn trim_outliers(points: &[Vec3], trim: f32) -> Vec<Vec3> {
    let trim = if trim.is_finite() {
        trim.clamp(0.0, 0.45)
    } else {
        0.0
    };
    let finite: Vec<Vec3> = points.iter().copied().filter(|p| p.is_finite()).collect();
    if trim <= 0.0 || finite.len() < 64 {
        return finite;
    }
    let n = finite.len();
    let lo = ((n as f32 * trim).ceil() as usize).clamp(1, n / 4);
    let hi = (n - 1).saturating_sub(lo);
    let mut axes: [Vec<f32>; 3] = [
        Vec::with_capacity(n),
        Vec::with_capacity(n),
        Vec::with_capacity(n),
    ];
    for p in &finite {
        axes[0].push(p.x);
        axes[1].push(p.y);
        axes[2].push(p.z);
    }
    let mut bounds = [(0.0f32, 0.0f32); 3];
    for (axis, vals) in axes.iter_mut().enumerate() {
        vals.sort_unstable_by(f32::total_cmp);
        bounds[axis] = (vals[lo], vals[hi]);
    }
    let filtered: Vec<Vec3> = finite
        .into_iter()
        .filter(|p| {
            p.x >= bounds[0].0
                && p.x <= bounds[0].1
                && p.y >= bounds[1].0
                && p.y <= bounds[1].1
                && p.z >= bounds[2].0
                && p.z <= bounds[2].1
        })
        .collect();
    if filtered.len() < 4 {
        points.iter().copied().filter(|p| p.is_finite()).collect()
    } else {
        filtered
    }
}

/// 体素表面 + 离群点裁剪（摄像头默认路径）。
pub fn voxel_surface_robust(points: &[Vec3], voxel: f32) -> MeshData {
    let filtered = trim_outliers(points, 0.005);
    voxel_surface(&filtered, voxel)
}

/// 从点云生成体素表面网格。
///
/// * `voxel`：体素边长（米），会夹到 `1e-4..=10`；
/// * 非有限点、坐标超出 `i32` 体素范围的点跳过；
/// * 没有任何有效点/没有体素时返回空网格。
///
/// 每个暴露面生成 4 个顶点、2 个三角形；相邻体素共享的面**不**生成。
pub fn voxel_surface(points: &[Vec3], voxel: f32) -> MeshData {
    if points.is_empty() || !voxel.is_finite() {
        return MeshData::default();
    }
    let voxel = voxel.clamp(1e-4, 10.0);
    let mut occupied: HashSet<(i32, i32, i32)> = HashSet::new();
    for p in points {
        if !p.is_finite() {
            continue;
        }
        let q = *p / voxel;
        if q.x < i32::MIN as f32 + 1.0
            || q.x > i32::MAX as f32 - 1.0
            || q.y < i32::MIN as f32 + 1.0
            || q.y > i32::MAX as f32 - 1.0
            || q.z < i32::MIN as f32 + 1.0
            || q.z > i32::MAX as f32 - 1.0
        {
            continue;
        }
        if occupied.len() >= MAX_VOXELS {
            break;
        }
        occupied.insert((q.x.floor() as i32, q.y.floor() as i32, q.z.floor() as i32));
    }
    if occupied.is_empty() {
        return MeshData::default();
    }

    // 每个方向：邻居偏移 + 从外侧看逆时针的 4 个局部角（0/1 坐标）。
    type Face = ([i32; 3], [[f32; 3]; 4], [f32; 3]);
    const FACES: [Face; 6] = [
        (
            [1, 0, 0],
            [[1., 0., 0.], [1., 1., 0.], [1., 1., 1.], [1., 0., 1.]],
            [1., 0., 0.],
        ),
        (
            [-1, 0, 0],
            [[0., 0., 0.], [0., 0., 1.], [0., 1., 1.], [0., 1., 0.]],
            [-1., 0., 0.],
        ),
        (
            [0, 1, 0],
            [[0., 1., 0.], [0., 1., 1.], [1., 1., 1.], [1., 1., 0.]],
            [0., 1., 0.],
        ),
        (
            [0, -1, 0],
            [[0., 0., 0.], [1., 0., 0.], [1., 0., 1.], [0., 0., 1.]],
            [0., -1., 0.],
        ),
        (
            [0, 0, 1],
            [[0., 0., 1.], [1., 0., 1.], [1., 1., 1.], [0., 1., 1.]],
            [0., 0., 1.],
        ),
        (
            [0, 0, -1],
            [[0., 0., 0.], [0., 1., 0.], [1., 1., 0.], [1., 0., 0.]],
            [0., 0., -1.],
        ),
    ];
    const UV: [[f32; 2]; 4] = [[0., 0.], [1., 0.], [1., 1.], [0., 1.]];

    let mut mesh = MeshData::default();
    for (x, y, z) in &occupied {
        for (dir, corners, normal) in FACES {
            if occupied.contains(&(x + dir[0], y + dir[1], z + dir[2])) {
                continue;
            }
            let n = Vec3::from_array(normal);
            let base = Vec3::new(*x as f32, *y as f32, *z as f32) * voxel;
            let start = mesh.vertices.len() as u32;
            for (corner, uv) in corners.iter().zip(UV) {
                let p = base + Vec3::from_array(*corner) * voxel;
                mesh.vertices
                    .push(Vertex::new(p, n, quench3d_math::Vec2::from_array(uv)));
            }
            // 先按表顺序写，再用第一条边的叉积确认朝向；表本身就是逆时针，
            // 这里仍做一次兜底，防止以后改表悄悄把面翻到里面。
            let p0 = Vec3::from_array(corners[0]);
            let p1 = Vec3::from_array(corners[1]);
            let p2 = Vec3::from_array(corners[2]);
            let winding = (p1 - p0).cross(p2 - p0).dot(n);
            if winding >= 0.0 {
                mesh.indices.extend_from_slice(&[
                    start,
                    start + 1,
                    start + 2,
                    start,
                    start + 2,
                    start + 3,
                ]);
            } else {
                mesh.indices.extend_from_slice(&[
                    start,
                    start + 2,
                    start + 1,
                    start,
                    start + 3,
                    start + 2,
                ]);
            }
        }
    }
    mesh
}

/// 从点云生成平滑的 Surface Nets 网格。
///
/// `voxel` 控制最长轴的格距（夹到 1e-3..=0.5 m）；`iso` 是等值面阈值。
/// 点太少/半径非正时返回空网格（调用方保留旧画面）。
pub fn smooth_surface(points: &[Vec3], voxel: f32, iso: f32) -> MeshData {
    if points.is_empty() || !voxel.is_finite() || !iso.is_finite() {
        return MeshData::default();
    }
    let voxel = voxel.clamp(1e-3, 0.5);
    let filtered = trim_outliers(points, 0.005);
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for p in &filtered {
        min = min.min(*p);
        max = max.max(*p);
    }
    if !min.is_finite() || !max.is_finite() {
        return MeshData::default();
    }
    let span = (max - min).max_element();
    if span <= 1e-6 {
        return MeshData::default();
    }
    let resolution = ((span / voxel).ceil() as u32).clamp(8, MAX_SURFACE_CELLS);
    let surface = extract_surface(&filtered, voxel * 2.0, resolution, iso.clamp(0.05, 1.5));
    if surface.is_empty() {
        return MeshData::default();
    }
    let mut mesh = MeshData::default();
    mesh.vertices.reserve(surface.positions.len());
    for (p, n) in surface.positions.iter().zip(&surface.normals) {
        let u = ((p.x - min.x) / span).clamp(0.0, 1.0);
        let v = ((p.z - min.z) / span).clamp(0.0, 1.0);
        mesh.vertices.push(Vertex::new(*p, *n, Vec2::new(u, v)));
    }
    mesh.indices = surface.indices;
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_degenerate_points_produce_no_mesh() {
        assert!(voxel_surface(&[], 0.03).indices.is_empty());
        assert!(voxel_surface(&[Vec3::splat(f32::NAN)], 0.03)
            .indices
            .is_empty());
        // 0 会被夹到最小体素，仍然产生一个立方体（不是空）。
        assert!(!voxel_surface(&[Vec3::ZERO], 0.0).indices.is_empty());
    }

    #[test]
    fn one_voxel_has_six_outward_faces() {
        let mesh = voxel_surface(&[Vec3::splat(0.01)], 0.1);
        assert_eq!(mesh.vertices.len(), 24);
        assert_eq!(mesh.indices.len(), 36);
        assert_eq!(mesh.indices.len() % 3, 0);
        // 六个面的法线必须覆盖 ±X/±Y/±Z。
        let mut dirs = std::collections::HashSet::new();
        for v in &mesh.vertices {
            dirs.insert(v.normal.map(|c| c as i32));
        }
        assert_eq!(dirs.len(), 6);
        let b = mesh.aabb();
        assert!((b.min.x - 0.0).abs() < 1e-6);
        assert!((b.max.x - 0.1).abs() < 1e-6);
    }

    #[test]
    fn adjacent_voxels_share_no_internal_face() {
        let mesh = voxel_surface(
            &[Vec3::new(0.01, 0.01, 0.01), Vec3::new(0.11, 0.01, 0.01)],
            0.1,
        );
        // 两个体素并排：10 个外露面，不是 12 个。
        assert_eq!(mesh.vertices.len(), 40);
        assert_eq!(mesh.indices.len(), 60);
        let b = mesh.aabb();
        assert!((b.max.x - 0.2).abs() < 1e-6);
    }

    #[test]
    fn smooth_surface_handles_empty_and_closed_blobs() {
        assert!(smooth_surface(&[], 0.03, 0.45).indices.is_empty());
        let mut pts = Vec::new();
        for z in -2..=2 {
            for y in -2..=2 {
                for x in -2..=2 {
                    if x * x + y * y + z * z <= 5 {
                        pts.push(Vec3::new(x as f32 * 0.04, y as f32 * 0.04, z as f32 * 0.04));
                    }
                }
            }
        }
        let mesh = smooth_surface(&pts, 0.04, 0.45);
        assert!(!mesh.indices.is_empty());
        assert_eq!(mesh.indices.len() % 3, 0);
        assert!(mesh.vertices.iter().all(|v| {
            let p = Vec3::from_array(v.position);
            let n = Vec3::from_array(v.normal);
            p.is_finite() && n.is_finite() && n.length() > 0.5
        }));
        // 点云在原点附近，平滑网格不能跑到 bounds 之外太远。
        let b = mesh.aabb();
        assert!(b.min.x > -0.3 && b.max.x < 0.3);
    }

    #[test]
    fn robust_trim_removes_a_far_outlier_before_reconstruction() {
        let mut pts = Vec::new();
        for z in -3..=3 {
            for y in -3..=3 {
                for x in -3..=3 {
                    if x * x + y * y + z * z <= 10 {
                        pts.push(Vec3::new(x as f32 * 0.04, y as f32 * 0.04, z as f32 * 0.04));
                    }
                }
            }
        }
        let clean = pts.len();
        pts.push(Vec3::new(1000.0, 1000.0, 1000.0));
        let trimmed = trim_outliers(&pts, 0.005);
        assert_eq!(
            trimmed.len(),
            clean - 1 + 1,
            "只应被裁掉 1 个点（另一颗是新加的）"
        );
        assert!(trimmed.iter().all(|p| p.x < 10.0));
        let smooth = smooth_surface(&pts, 0.04, 0.45);
        let b = smooth.aabb();
        assert!(b.max.x < 1.0, "飞点不能把包围盒撑到 1000 米：{b:?}");
        let voxel = voxel_surface_robust(&pts, 0.04);
        assert!(voxel.aabb().max.x < 1.0);
    }

    #[test]
    fn duplicate_points_collapse_into_one_voxel() {
        let a = voxel_surface(&[Vec3::splat(0.01), Vec3::splat(0.02)], 0.1);
        assert_eq!(a.vertices.len(), 24);
        assert_eq!(a.indices.len(), 36);
    }
}
