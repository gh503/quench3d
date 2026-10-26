//! 把摄像头深度点云固化成可编辑的三角网。
//!
//! 点云本身只能看，不能选中/移动/导出。这里提供两种固化方式：
//! * **体素表面**：每个点落进一个体素，只为朝向空气的六个面生成四边形，棱角分明；
//! * **平滑表面**：复用流体的 Surface Nets 等值面提取，得到连续、带法线的表面。
//!
//! 纯 CPU、无第三方库；同样的点 + 体素尺寸逐位可复现。摄像头可选**时间累积**
//! （[`PointCloudAccumulator`]）：最近若干帧按体素平均，补遮挡/降噪声。

use std::collections::HashSet;

use quench3d::render::{MeshData, Vertex};
use quench3d_math::{Vec2, Vec3};
use quench3d_physics::extract_surface;

/// 摄像头针孔投影：把引擎坐标顶点映射回原图 UV，用于给重建网格上色。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraProjection {
    /// 图像宽度（像素）。
    pub width: u32,
    /// 图像高度（像素）。
    pub height: u32,
    /// 水平焦距（像素）。
    pub fx: f32,
    /// 垂直焦距（像素）。
    pub fy: f32,
    /// 主点 x（像素）。
    pub cx: f32,
    /// 主点 y（像素）。
    pub cy: f32,
    /// 点云原点（引擎坐标 = 相机坐标 + origin）。
    pub origin: Vec3,
    /// 图像是否水平镜像。
    pub mirrored: bool,
}

impl CameraProjection {
    /// 内参/尺寸是否足以投影。
    pub fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.fx.is_finite()
            && self.fy.is_finite()
            && self.fx > 0.0
            && self.fy > 0.0
            && self.cx.is_finite()
            && self.cy.is_finite()
            && self.origin.is_finite()
    }

    /// 把引擎坐标点投到图像 UV（0..1，左上原点）；不在画面前方/画面外返回 `None`。
    pub fn uv(&self, p: Vec3) -> Option<Vec2> {
        if !self.is_valid() || !p.is_finite() {
            return None;
        }
        let z = self.origin.z - p.z;
        if !(z.is_finite() && z > 1e-6) {
            return None;
        }
        let mut x = (p.x - self.origin.x) / z * self.fx + self.cx;
        let y = -(p.y - self.origin.y) / z * self.fy + self.cy;
        if self.mirrored {
            x = (self.width.saturating_sub(1)) as f32 - x;
        }
        if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 {
            return None;
        }
        if x >= self.width as f32 || y >= self.height as f32 {
            return None;
        }
        Some(Vec2::new(
            (x + 0.5) / self.width as f32,
            (y + 0.5) / self.height as f32,
        ))
    }
}

/// 给网格顶点写摄像头原图 UV；返回成功投影的顶点数。
///
/// `rgb` 必须是 `width*height*3` 的 RGB24；长度不符时不改网格并返回 0。
/// 投影不到的顶点用画面中心 UV（0.5, 0.5），避免采样到贴图角落的黑色。
pub fn colorize_mesh_uvs(mesh: &mut MeshData, rgb: &[u8], projection: &CameraProjection) -> usize {
    if !projection.is_valid()
        || rgb.len() != projection.width as usize * projection.height as usize * 3
    {
        return 0;
    }
    let mut colored = 0usize;
    for vertex in &mut mesh.vertices {
        let p = Vec3::from_array(vertex.position);
        if let Some(uv) = projection.uv(p) {
            vertex.uv = uv.to_array();
            colored += 1;
        } else {
            vertex.uv = [0.5, 0.5];
        }
    }
    colored
}

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
    // 实时建模时深度点可能上百万；三轴排序前先等距抽样到 20 万点，
    // 把每帧重建的排序开销压住（采样是确定性的：按下标取模）。
    const MAX_POINTS: usize = 200_000;
    let stride = if trim > 0.0 {
        points.len().div_ceil(MAX_POINTS).max(1)
    } else {
        1
    };
    let finite: Vec<Vec3> = points
        .iter()
        .enumerate()
        .filter(|(i, _)| i % stride == 0)
        .map(|(_, p)| *p)
        .filter(|p| p.is_finite())
        .collect();
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
    fn projection_maps_engine_points_back_to_camera_pixels() {
        let p = CameraProjection {
            width: 100,
            height: 50,
            fx: 100.0,
            fy: 100.0,
            cx: 50.0,
            cy: 25.0,
            origin: Vec3::ZERO,
            mirrored: false,
        };
        assert!(p.is_valid());
        let center = p.uv(Vec3::new(0.0, 0.0, -1.0)).unwrap();
        assert!((center.x - 0.505).abs() < 1e-6, "{center:?}");
        assert!((center.y - 0.51).abs() < 1e-6, "{center:?}");
        // 相机右侧（+X）→ 像素右；相机上方（+Y）→ 像素上（y 变小）。
        let right = p.uv(Vec3::new(0.1, 0.0, -1.0)).unwrap();
        assert!(right.x > center.x);
        let up = p.uv(Vec3::new(0.0, 0.1, -1.0)).unwrap();
        assert!(up.y < center.y);
        // 背后/画面外/非有限值不猜。
        assert_eq!(p.uv(Vec3::new(0.0, 0.0, 1.0)), None);
        assert_eq!(p.uv(Vec3::new(10.0, 0.0, -1.0)), None);
        assert_eq!(p.uv(Vec3::splat(f32::NAN)), None);

        let mut mirrored = p;
        mirrored.mirrored = true;
        let m = mirrored.uv(Vec3::new(0.1, 0.0, -1.0)).unwrap();
        assert!((m.x - (1.0 - right.x)).abs() < 1e-6, "{m:?} {right:?}");
    }

    #[test]
    fn colorize_sets_uvs_and_rejects_wrong_rgb_length() {
        let mut mesh = voxel_surface(&[Vec3::new(0.0, 0.0, -1.0)], 0.5);
        let projection = CameraProjection {
            width: 16,
            height: 16,
            fx: 4.0,
            fy: 4.0,
            cx: 8.0,
            cy: 8.0,
            origin: Vec3::ZERO,
            mirrored: false,
        };
        assert_eq!(colorize_mesh_uvs(&mut mesh, &[0u8; 767], &projection), 0);
        assert_eq!(
            colorize_mesh_uvs(&mut mesh, &[128u8; 16 * 16 * 3], &projection),
            24
        );
        assert!(mesh
            .vertices
            .iter()
            .all(|v| { v.uv[0] >= 0.0 && v.uv[0] <= 1.0 && v.uv[1] >= 0.0 && v.uv[1] <= 1.0 }));
        assert!(mesh.vertices.iter().any(|v| v.uv != [0.5, 0.5]));
    }

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
    fn huge_clouds_are_sampled_before_sorting() {
        let pts: Vec<Vec3> = (0..300_000)
            .map(|i| Vec3::new(i as f32 * 1e-4, (i % 73) as f32 * 1e-4, 0.0))
            .collect();
        let trimmed = trim_outliers(&pts, 0.005);
        assert!(trimmed.len() <= 200_000, "{}", trimmed.len());
        assert!(
            trimmed.len() > 100_000,
            "抽样不应把点云削得太少：{}",
            trimmed.len()
        );
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

/// 摄像头点云的**时间累积**：把最近若干帧的点按体素去重/平均，弥补单帧遮挡和噪声。
///
/// 纯 CPU、确定性：体素键用 `BTreeMap` 排序，`points()` 输出顺序固定；同一输入序列
/// 逐位可复现。旧的单帧重建路径不受影响（开关缺省关）。
#[derive(Clone, Debug)]
pub struct PointCloudAccumulator {
    voxel: f32,
    max_cells: usize,
    ttl_frames: u64,
    cells: std::collections::BTreeMap<(i32, i32, i32), AccumCell>,
    last_purge: u64,
}

#[derive(Clone, Copy, Debug)]
struct AccumCell {
    sum: Vec3,
    count: u32,
    last_frame: u64,
}

impl PointCloudAccumulator {
    /// 创建一个累积器。
    ///
    /// * `voxel`：去重体素边长（米），夹到 `1e-3..=0.5`；
    /// * `max_cells`：最多保留多少个体素（至少 1）；
    /// * `ttl_frames`：多少帧没再被观测就丢弃（0 = 只保留当前帧）。
    pub fn new(voxel: f32, max_cells: usize, ttl_frames: u64) -> Self {
        let voxel = if voxel.is_finite() {
            voxel.clamp(1e-3, 0.5)
        } else {
            0.02
        };
        Self {
            voxel,
            max_cells: max_cells.max(1),
            ttl_frames,
            cells: std::collections::BTreeMap::new(),
            last_purge: 0,
        }
    }

    /// 合并一帧点云；返回实际插入/更新的体素数。
    pub fn merge(&mut self, points: &[Vec3], frame: u64) -> usize {
        if points.is_empty() {
            return 0;
        }
        // 每 32 帧清一次过期体素；比每帧 retain 便宜，同时限制内存。
        if frame > self.last_purge.saturating_add(32) {
            self.cells
                .retain(|_, c| frame.saturating_sub(c.last_frame) <= self.ttl_frames);
            self.last_purge = frame;
        }
        // 满容量时先清过期体素：摄像头扫过新区域时，不能因为旧区域占满而永远进不来。
        if self.cells.len() >= self.max_cells {
            let cutoff = frame.saturating_sub(self.ttl_frames);
            self.cells.retain(|_, c| c.last_frame >= cutoff);
        }
        let mut touched = 0usize;
        for p in points {
            if !p.is_finite() {
                continue;
            }
            let key = (
                (p.x / self.voxel).floor() as i32,
                (p.y / self.voxel).floor() as i32,
                (p.z / self.voxel).floor() as i32,
            );
            if let Some(cell) = self.cells.get_mut(&key) {
                cell.sum += *p;
                cell.count = cell.count.saturating_add(1);
                cell.last_frame = frame;
                touched += 1;
            } else if self.cells.len() < self.max_cells {
                self.cells.insert(
                    key,
                    AccumCell {
                        sum: *p,
                        count: 1,
                        last_frame: frame,
                    },
                );
                touched += 1;
            }
        }
        self.last_purge = frame;
        touched
    }

    /// 当前累积点（每个体素取平均；按体素键排序，确定性）。
    pub fn points(&self) -> Vec<Vec3> {
        self.cells
            .values()
            .map(|c| c.sum / c.count as f32)
            .collect()
    }

    /// 当前体素数。
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// 清空。
    pub fn clear(&mut self) {
        self.cells.clear();
        self.last_purge = 0;
    }
}

#[cfg(test)]
mod accumulator_tests {
    use super::*;
    use quench3d_math::vec3;

    #[test]
    fn accumulator_merges_equal_cells_and_orders_points() {
        let mut a = PointCloudAccumulator::new(0.1, 16, 120);
        assert_eq!(a.merge(&[vec3(0.01, 0.0, 0.0)], 0), 1);
        assert_eq!(a.merge(&[vec3(0.02, 0.0, 0.0)], 1), 1);
        assert_eq!(a.len(), 1, "同一体素应合并，而不是长两个点");
        let p = a.points()[0];
        assert!((p.x - 0.015).abs() < 1e-6, "{p:?}");
        assert_eq!(a.merge(&[vec3(1.01, 0.0, 0.0)], 2), 1);
        let pts = a.points();
        assert_eq!(pts.len(), 2);
        assert!(pts[0].x < pts[1].x, "点必须按体素键排序：{pts:?}");
    }

    #[test]
    fn accumulated_cloud_reconstructs_both_sides_of_a_sweep() {
        let mut acc = PointCloudAccumulator::new(0.05, 1024, 120);
        acc.merge(&[vec3(-0.5, 0.0, 0.0)], 0);
        acc.merge(&[vec3(0.5, 0.0, 0.0)], 1);
        let mesh = voxel_surface(&acc.points(), 0.1);
        assert!(!mesh.indices.is_empty());
        let xs: Vec<f32> = mesh.vertices.iter().map(|v| v.position[0]).collect();
        let lo = xs.iter().copied().fold(f32::INFINITY, f32::min);
        let hi = xs.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert!(lo < -0.45, "左侧点应进入网格：lo={lo}");
        assert!(hi > 0.45, "右侧点应进入网格：hi={hi}");
    }

    #[test]
    fn accumulator_caps_cells_ignores_non_finite_and_expires() {
        let mut a = PointCloudAccumulator::new(0.1, 2, 4);
        let n = a.merge(
            &[
                vec3(0.0, 0.0, 0.0),
                vec3(1.0, 0.0, 0.0),
                vec3(2.0, 0.0, 0.0),
                Vec3::splat(f32::NAN),
            ],
            0,
        );
        assert_eq!(n, 2, "只应接受前两个体素，NaN 跳过");
        assert_eq!(a.len(), 2, "不能超过 max_cells");
        // 过了 TTL，旧体素被清掉；新点仍可进入。
        assert_eq!(a.merge(&[vec3(3.0, 0.0, 0.0)], 100), 1);
        assert_eq!(a.len(), 1, "过期体素应被保留策略清掉");
        assert!(a.points()[0].x > 2.9);
    }

    #[test]
    fn accumulator_is_deterministic_and_clearable() {
        let frame = [
            vec3(0.0, 0.0, 0.0),
            vec3(0.5, 0.5, 0.5),
            vec3(0.51, 0.49, 0.5),
        ];
        let mut a = PointCloudAccumulator::new(0.05, 100, 30);
        let mut b = PointCloudAccumulator::new(0.05, 100, 30);
        a.merge(&frame, 7);
        b.merge(&frame, 7);
        assert_eq!(a.points(), b.points());
        a.clear();
        assert!(a.is_empty());
        assert_eq!(a.points(), Vec::<Vec3>::new());
    }
}
