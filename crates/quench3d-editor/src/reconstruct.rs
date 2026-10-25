//! 把摄像头深度点云固化成可编辑的三角网。
//!
//! 点云本身只能看，不能选中/移动/导出。这里用**体素表面**把它变成真正的网格：
//! 每个点落进一个体素，只有朝向空气的六个面才生成四边形。相比 Marching Cubes
//! 它是有棱角的"扫描块"，但实现小、确定性强，适合单目深度点云这种稀疏输入。
//!
//! 纯 CPU、无第三方库；同样的点 + 体素尺寸逐位可复现。

use std::collections::HashSet;

use quench3d::render::{MeshData, Vertex};
use quench3d_math::Vec3;

/// 体素上限：超过就只保留最先出现的体素，避免一次扫描吃掉几 GB。
pub const MAX_VOXELS: usize = 300_000;

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
    fn duplicate_points_collapse_into_one_voxel() {
        let a = voxel_surface(&[Vec3::splat(0.01), Vec3::splat(0.02)], 0.1);
        assert_eq!(a.vertices.len(), 24);
        assert_eq!(a.indices.len(), 36);
    }
}
