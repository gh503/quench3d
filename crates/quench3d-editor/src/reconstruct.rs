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
use quench3d_math::{Quat, Vec2, Vec3};
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

/// 两帧点云的刚体配准结果：把**当前帧**点映射到**上一帧**坐标系。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointCloudRegistration {
    /// 旋转。
    pub rotation: Quat,
    /// 平移（米）。
    pub translation: Vec3,
    /// 参与最小二乘的对应点数。
    pub pairs: usize,
    /// 对应点残差均方根（米）。
    pub rms: f32,
    /// 实际迭代次数（收敛时会小于上限）。
    pub iterations: u32,
}

/// 固定半径最近邻的稠密网格：每个格一条链表，查询 27 个格子。
///
/// 旧实现用 `HashMap<(i32,i32,i32), Vec<usize>>`，每轮每个点 27 次 SipHash；
/// 4k 参考点 × 1.5k 当前点 × 多轮时哈希成了主成本。网格直接把格子映射到
/// `heads` 下标，链表节点复用，构建一次、查询零分配。格子边长至少 8 cm，
/// 且按点云范围自适应放大到每轴最多约 120 格，避免大场景分配爆炸。
struct GridIndex {
    min: Vec3,
    cell: f32,
    dims: [i32; 3],
    heads: Vec<i32>,
    next: Vec<i32>,
}

impl GridIndex {
    fn new(points: &[Vec3]) -> Self {
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for p in points {
            min = min.min(*p);
            max = max.max(*p);
        }
        if !min.is_finite() || !max.is_finite() {
            min = Vec3::ZERO;
            max = Vec3::ZERO;
        }
        // 查询点可能被配准增量挪出参考点范围；向外扩一个最近邻半径。
        min -= Vec3::splat(0.12);
        max += Vec3::splat(0.12);
        let extent = (max - min).max(Vec3::splat(1e-4));
        let longest = extent.x.max(extent.y).max(extent.z);
        let cell = longest / 120.0;
        let cell = cell.max(0.08).max(1e-4);
        let dims = [
            ((extent.x / cell).floor() as i32 + 1).clamp(1, 128),
            ((extent.y / cell).floor() as i32 + 1).clamp(1, 128),
            ((extent.z / cell).floor() as i32 + 1).clamp(1, 128),
        ];
        let cells = (dims[0] * dims[1] * dims[2]) as usize;
        let mut heads = vec![-1i32; cells];
        let mut next = vec![-1i32; points.len()];
        for (i, p) in points.iter().enumerate() {
            if let Some([x, y, z]) = grid_cell(*p, min, cell, dims) {
                let slot = (x + dims[0] * (y + dims[1] * z)) as usize;
                next[i] = heads[slot];
                heads[slot] = i as i32;
            }
        }
        Self {
            min,
            cell,
            dims,
            heads,
            next,
        }
    }

    fn nearest(&self, points: &[Vec3], target: Vec3, max_distance: f32) -> Option<(usize, f32)> {
        let [bx, by, bz] = grid_cell(target, self.min, self.cell, self.dims)?;
        let max_d2 = max_distance * max_distance;
        let mut best: Option<(usize, f32)> = None;
        for dz in -1..=1 {
            let z = bz + dz;
            if z < 0 || z >= self.dims[2] {
                continue;
            }
            for dy in -1..=1 {
                let y = by + dy;
                if y < 0 || y >= self.dims[1] {
                    continue;
                }
                for dx in -1..=1 {
                    let x = bx + dx;
                    if x < 0 || x >= self.dims[0] {
                        continue;
                    }
                    let slot = (x + self.dims[0] * (y + self.dims[1] * z)) as usize;
                    let mut i = self.heads[slot];
                    while i >= 0 {
                        let index = i as usize;
                        let d2 = (points[index] - target).length_squared();
                        if d2 <= max_d2 {
                            let better = match best {
                                None => true,
                                Some((best_index, best_d2)) => {
                                    d2 < best_d2 || (d2 == best_d2 && index < best_index)
                                }
                            };
                            if better {
                                best = Some((index, d2));
                            }
                        }
                        i = self.next[index];
                    }
                }
            }
        }
        best
    }
}

fn grid_cell(p: Vec3, min: Vec3, cell: f32, dims: [i32; 3]) -> Option<[i32; 3]> {
    let local = (p - min) / cell;
    let x = local.x.floor() as i32;
    let y = local.y.floor() as i32;
    let z = local.z.floor() as i32;
    (x >= 0 && x < dims[0] && y >= 0 && y < dims[1] && z >= 0 && z < dims[2]).then_some([x, y, z])
}

/// 两帧点云刚体配准：点到点 ICP + 小角度线性化，返回把当前帧映射到上一帧的
/// `R, t`。
///
/// 这是**实时增量**配准，不是全局 SLAM：初始猜测是恒等变换，适合 30/60 Hz
/// 相邻帧的小运动；大位移或完全不同的场景会因为没有对应点而返回 `None`。
/// 用空间哈希找最近邻、固定采样与固定遍历顺序，结果确定；不依赖 GPU，也不
/// 需要外部点云库。精度上限由体素/采样间距和实际重叠区决定。
pub fn register_point_clouds(
    previous: &[Vec3],
    current: &[Vec3],
) -> Option<PointCloudRegistration> {
    const MAX_PREVIOUS: usize = 4000;
    const MAX_CURRENT: usize = 1500;
    const MAX_DISTANCE: f32 = 0.12;
    const ITERATIONS: usize = 24;

    fn sample(points: &[Vec3], max: usize) -> Vec<Vec3> {
        let step = points.len().div_ceil(max).max(1);
        points
            .iter()
            .step_by(step)
            .filter(|p| p.is_finite())
            .copied()
            .collect()
    }

    let previous = sample(previous, MAX_PREVIOUS);
    let current = sample(current, MAX_CURRENT);
    if previous.len() < 6 || current.len() < 6 {
        return None;
    }
    let grid = GridIndex::new(&previous);
    let nearest_pairs = |rotation: Quat, translation: Vec3| -> Vec<(Vec3, Vec3)> {
        let mut pairs = Vec::with_capacity(current.len());
        for p in &current {
            let q = rotation.mul_vec3(*p) + translation;
            if let Some((i, _)) = grid.nearest(&previous, q, MAX_DISTANCE) {
                pairs.push((previous[i], q));
            }
        }
        pairs
    };

    let mut rotation = Quat::IDENTITY;
    let mut translation = Vec3::ZERO;
    let mut iterations = 0u32;
    for _ in 0..ITERATIONS {
        iterations += 1;
        let pairs = nearest_pairs(rotation, translation);
        if pairs.len() < 6 {
            return None;
        }
        // 小角度线性化：q' ≈ q + ω×q + v。对每个对应点把 3 行雅可比累加进
        // 6×6 正规方程，解出增量 [ω, v]。
        let mut normal = [[0.0f64; 6]; 6];
        let mut rhs = [0.0f64; 6];
        for (target, q) in &pairs {
            let (qx, qy, qz) = (q.x as f64, q.y as f64, q.z as f64);
            let rows = [
                [0.0, qz, -qy, 1.0, 0.0, 0.0],
                [-qz, 0.0, qx, 0.0, 1.0, 0.0],
                [qy, -qx, 0.0, 0.0, 0.0, 1.0],
            ];
            let residual = [
                (target.x - q.x) as f64,
                (target.y - q.y) as f64,
                (target.z - q.z) as f64,
            ];
            for row in 0..3 {
                for i in 0..6 {
                    rhs[i] += rows[row][i] * residual[row];
                    for j in 0..6 {
                        normal[i][j] += rows[row][i] * rows[row][j];
                    }
                }
            }
        }
        let step = solve_six(normal, rhs)?;
        let omega = Vec3::new(step[0] as f32, step[1] as f32, step[2] as f32);
        let delta = Vec3::new(step[3] as f32, step[4] as f32, step[5] as f32);
        let angle = omega.length();
        let dq = if angle > 1e-7 {
            Quat::from_axis_angle(omega / angle, angle)
        } else {
            Quat::IDENTITY
        };
        translation = dq.mul_vec3(translation) + delta;
        rotation = (dq * rotation).normalize_or_identity();
        if omega.length() < 1e-6 && delta.length() < 1e-6 {
            break;
        }
    }

    let pairs = nearest_pairs(rotation, translation);
    if pairs.len() < 6 {
        return None;
    }
    let rms = (pairs
        .iter()
        .map(|(target, q)| (*target - *q).length_squared())
        .sum::<f32>()
        / pairs.len() as f32)
        .sqrt();
    if !rms.is_finite() {
        return None;
    }
    Some(PointCloudRegistration {
        rotation,
        translation,
        pairs: pairs.len(),
        rms,
        iterations,
    })
}

/// 6×6 线性方程的高斯消元（部分主元）；奇异时返回 `None`。
#[allow(clippy::needless_range_loop)] // 行列消元必须同时用列号与行号。
fn solve_six(mut a: [[f64; 6]; 6], mut b: [f64; 6]) -> Option<[f64; 6]> {
    for column in 0..6 {
        let mut pivot = column;
        for row in column + 1..6 {
            if a[row][column].abs() > a[pivot][column].abs() {
                pivot = row;
            }
        }
        if a[pivot][column].abs() < 1e-12 {
            return None;
        }
        a.swap(column, pivot);
        b.swap(column, pivot);
        let scale = a[column][column];
        for j in column..6 {
            a[column][j] /= scale;
        }
        b[column] /= scale;
        for row in 0..6 {
            if row == column {
                continue;
            }
            let factor = a[row][column];
            if factor == 0.0 {
                continue;
            }
            for j in column..6 {
                a[row][j] -= factor * a[column][j];
            }
            b[row] -= factor * b[column];
        }
    }
    Some(b)
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

    /// 一条不平移对称的起伏面：给 ICP 足够几何约束，又不用外部点云文件。
    fn wavy_patch() -> Vec<Vec3> {
        let mut out = Vec::new();
        for i in 0..24 {
            for j in 0..24 {
                let x = (i as f32 - 11.5) * 0.04;
                let y = (j as f32 - 11.5) * 0.04;
                let z = 0.08 * (x * 5.0).sin() + 0.04 * (y * 7.0).cos();
                out.push(vec3(x, y, z));
            }
        }
        out
    }

    /// 摄像头 30/60 Hz 实时预算：4k 参考点 + 1.5k 当前点的 ICP 单次耗时。
    #[test]
    #[ignore = "慢：ICP 性能基准，显式 --ignored --release 运行"]
    fn icp_4k_points_fits_a_camera_frame() {
        let mut previous = Vec::new();
        for i in 0..70 {
            for j in 0..70 {
                let x = (i as f32 - 34.5) * 0.02;
                let y = (j as f32 - 34.5) * 0.02;
                previous.push(vec3(x, y, 0.6 + 0.02 * (x * 3.0).sin()));
            }
        }
        let rotation = Quat::from_axis_angle(vec3(0.0, 1.0, 0.0), 0.01);
        let current: Vec<Vec3> = previous
            .iter()
            .map(|p| rotation.mul_vec3(*p) + vec3(0.005, 0.0, 0.0))
            .collect();
        let mut best = f64::INFINITY;
        let mut used = 0u32;
        for _ in 0..5 {
            let start = std::time::Instant::now();
            let found = register_point_clouds(&previous, &current).expect("应配准");
            best = best.min(start.elapsed().as_secs_f64() * 1000.0);
            used = found.iterations;
            assert!(found.pairs > 500);
        }
        println!("ICP 4k 参考 + 1.5k 当前：{best:.2} ms，{used} 轮");
        assert!(best < 2.5, "ICP 单帧 {best:.2} ms 超过 2.5 ms 实时目标");
    }

    #[test]
    fn grid_nearest_matches_a_brute_force_search() {
        // 网格索引是 ICP 的性能路径；最近邻必须与暴力搜索逐点一致，
        // 包括“半径外没有点”和相同距离取小下标两种边界。
        let mut points = Vec::new();
        for i in 0..500 {
            let x = (i % 10) as f32 * 0.07 + (i as f32 * 0.013).sin() * 0.02;
            let y = (i / 10 % 10) as f32 * 0.06 + (i as f32 * 0.017).cos() * 0.02;
            let z = (i / 100) as f32 * 0.09 + (i as f32 * 0.019).sin() * 0.02;
            points.push(vec3(x, y, z));
        }
        let grid = GridIndex::new(&points);
        for i in 0..240 {
            let q = vec3(
                (i % 12) as f32 * 0.06 - 0.1,
                (i / 12 % 12) as f32 * 0.05 - 0.1,
                (i / 144) as f32 * 0.1 - 0.1,
            );
            let mut want: Option<(usize, f32)> = None;
            for (index, p) in points.iter().enumerate() {
                let d2 = (*p - q).length_squared();
                if d2 > 0.12 * 0.12 {
                    continue;
                }
                let better = match want {
                    None => true,
                    Some((best_index, best_d2)) => {
                        d2 < best_d2 || (d2 == best_d2 && index < best_index)
                    }
                };
                if better {
                    want = Some((index, d2));
                }
            }
            assert_eq!(
                grid.nearest(&points, q, 0.12),
                want,
                "网格最近邻与暴力搜索不一致：q={q:?}"
            );
        }
    }

    #[test]
    fn point_cloud_registration_recovers_a_known_rigid_motion() {
        let previous = wavy_patch();
        let rotation = Quat::from_axis_angle(vec3(0.0, 1.0, 0.0), 0.02);
        let translation = vec3(0.004, -0.003, 0.002);
        let current: Vec<Vec3> = previous
            .iter()
            .map(|p| rotation.mul_vec3(*p) + translation)
            .collect();
        let found = register_point_clouds(&previous, &current).expect("同形点云应当配准");
        assert!(found.pairs > 400, "对应点太少：{}", found.pairs);
        assert!(
            found.iterations < 24,
            "小运动应在迭代上限前收敛，实际 {} 轮",
            found.iterations
        );
        assert!(found.rms < 0.002, "rms 应小于 2 mm：{}", found.rms);
        let mut worst = 0.0f32;
        for (p, q) in previous.iter().zip(&current) {
            let mapped = found.rotation.mul_vec3(*q) + found.translation;
            worst = worst.max((mapped - *p).length());
        }
        assert!(worst < 0.004, "最坏点误差应小于 4 mm：{worst}");
    }

    #[test]
    fn point_cloud_registration_reports_no_match_instead_of_faking_identity() {
        assert!(register_point_clouds(&[], &[Vec3::ZERO]).is_none());
        assert!(register_point_clouds(&[Vec3::ZERO], &[]).is_none());
        // 两组相距 10 m 的点：没有邻域对应，不能返回一个“恒等变换”冒充成功。
        assert!(register_point_clouds(
            &[vec3(0.0, 0.0, 0.0), vec3(0.1, 0.0, 0.0)],
            &[vec3(10.0, 0.0, 0.0), vec3(10.1, 0.0, 0.0)]
        )
        .is_none());
    }

    #[test]
    fn point_cloud_registration_is_deterministic() {
        let previous = wavy_patch();
        let rotation = Quat::from_axis_angle(vec3(1.0, 0.0, 0.0), -0.015);
        let translation = vec3(-0.003, 0.002, 0.001);
        let current: Vec<Vec3> = previous
            .iter()
            .map(|p| rotation.mul_vec3(*p) + translation)
            .collect();
        let a = register_point_clouds(&previous, &current).unwrap();
        let b = register_point_clouds(&previous, &current).unwrap();
        assert_eq!(a, b, "顺序/哈希遍历不能让配准漂移");
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
