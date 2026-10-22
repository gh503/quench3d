//! 网格数据与几何体生成。
//!
//! `MeshData` 是 CPU 侧的顶点/索引集合；`Renderer::add_mesh` 会把它上传到 GPU。
//! 内置了盒、球、胶囊、圆柱、圆锥、圆环、地面网格等常用图元，
//! 也支持法线/切线重算与网格变换。

use bytemuck::{Pod, Zeroable};
use quench3d_math::{Aabb, Mat4, Vec2, Vec3};

/// 顶点格式（位置 / 法线 / UV / 切线）。与着色器中的 `VertexInput` 一一对应。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
#[derive(Pod, Zeroable)]
pub struct Vertex {
    /// 位置。
    pub position: [f32; 3],
    /// 法线。
    pub normal: [f32; 3],
    /// 纹理坐标。
    pub uv: [f32; 2],
    /// 切线（`w` 为副切线手性符号）。
    pub tangent: [f32; 4],
    /// 关节下标（GPU 蒙皮；非蒙皮网格全 0）。
    pub joints: [u16; 4],
    /// 关节权重（GPU 蒙皮；全 0 = 保持原顶点）。
    pub weights: [f32; 4],
}

impl Vertex {
    /// 构造顶点。
    pub fn new(position: Vec3, normal: Vec3, uv: Vec2) -> Self {
        Self {
            position: position.to_array(),
            normal: normal.to_array(),
            uv: uv.to_array(),
            tangent: [1.0, 0.0, 0.0, 1.0],
            joints: [0; 4],
            weights: [0.0; 4],
        }
    }

    /// 位置。
    #[inline]
    pub fn pos(&self) -> Vec3 {
        Vec3::from_array(self.position)
    }

    /// 法线。
    #[inline]
    pub fn nrm(&self) -> Vec3 {
        Vec3::from_array(self.normal)
    }
}

/// CPU 侧网格数据。
#[derive(Clone, Debug, Default)]
pub struct MeshData {
    /// 顶点。
    pub vertices: Vec<Vertex>,
    /// 三角形索引。
    pub indices: Vec<u32>,
}

impl MeshData {
    /// 空网格。
    pub fn new() -> Self {
        Self::default()
    }

    /// 由顶点与索引构造。
    pub fn from_parts(vertices: Vec<Vertex>, indices: Vec<u32>) -> Self {
        Self { vertices, indices }
    }

    /// 三角形数量。
    #[inline]
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// 顶点数量。
    #[inline]
    pub fn vertex_count(&self) -> usize {
        self.vertices.len()
    }

    /// 是否为空。
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.vertices.is_empty() || self.indices.is_empty()
    }

    /// 局部包围盒。
    pub fn aabb(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        for v in &self.vertices {
            b.expand_point(v.pos());
        }
        b
    }

    /// 用矩阵变换全部顶点（法线用逆转置，切线做同样的线性变换）。
    pub fn transformed(&self, m: &Mat4) -> Self {
        let n = m.to_mat3().inverse().transpose();
        let l = m.to_mat3();
        let mut out = self.clone();
        for v in &mut out.vertices {
            let p = m.transform_point3(v.pos());
            v.position = p.to_array();
            v.normal = n.mul_vec3(v.nrm()).normalize_or_zero().to_array();
            let t = l
                .mul_vec3(Vec3::from_array([v.tangent[0], v.tangent[1], v.tangent[2]]))
                .normalize_or_zero();
            v.tangent = [t.x, t.y, t.z, v.tangent[3]];
        }
        out
    }

    /// 合并另一个网格（索引自动偏移）。
    pub fn merge(&mut self, other: &MeshData) {
        let base = self.vertices.len() as u32;
        self.vertices.extend_from_slice(&other.vertices);
        self.indices.extend(other.indices.iter().map(|i| i + base));
    }

    /// 翻转所有三角形绕序（配合 `cull_mode` 可用于内表面）。
    pub fn flip_winding(&mut self) {
        for tri in self.indices.as_chunks_mut::<3>().0 {
            tri.swap(1, 2);
        }
    }

    /// 按面重算法线（硬边，适合立方体等低模）。
    pub fn recompute_flat_normals(&mut self) {
        for v in &mut self.vertices {
            v.normal = [0.0; 3];
        }
        let verts = self.vertices.clone();
        for tri in self.indices.as_chunks::<3>().0 {
            let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            let n = (verts[b].pos() - verts[a].pos())
                .cross(verts[c].pos() - verts[a].pos())
                .normalize_or_zero();
            for &i in &[a, b, c] {
                self.vertices[i].normal = n.to_array();
            }
        }
    }

    /// 按面积加权重算平滑法线。
    pub fn recompute_smooth_normals(&mut self) {
        let mut acc = vec![Vec3::ZERO; self.vertices.len()];
        for tri in self.indices.as_chunks::<3>().0 {
            let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            let p = [
                self.vertices[a].pos(),
                self.vertices[b].pos(),
                self.vertices[c].pos(),
            ];
            let n = (p[1] - p[0]).cross(p[2] - p[0]); // 未归一化 → 天然带面积权重
            acc[a] += n;
            acc[b] += n;
            acc[c] += n;
        }
        for (v, n) in self.vertices.iter_mut().zip(acc) {
            v.normal = n.normalize_or_zero().to_array();
        }
    }

    /// 计算切线（需要位置/法线/UV，用于法线贴图）。
    pub fn recompute_tangents(&mut self) {
        let n = self.vertices.len();
        let mut tan = vec![Vec3::ZERO; n];
        let mut bit = vec![Vec3::ZERO; n];

        for tri in self.indices.as_chunks::<3>().0 {
            let (i0, i1, i2) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            let v0 = self.vertices[i0];
            let v1 = self.vertices[i1];
            let v2 = self.vertices[i2];
            let e1 = v1.pos() - v0.pos();
            let e2 = v2.pos() - v0.pos();
            let d1 = Vec2::from_array(v1.uv) - Vec2::from_array(v0.uv);
            let d2 = Vec2::from_array(v2.uv) - Vec2::from_array(v0.uv);
            let det = d1.x * d2.y - d2.x * d1.y;
            if det.abs() < 1e-12 {
                continue;
            }
            let r = 1.0 / det;
            let t = (e1 * d2.y - e2 * d1.y) * r;
            let b = (e2 * d1.x - e1 * d2.x) * r;
            for &i in &[i0, i1, i2] {
                tan[i] += t;
                bit[i] += b;
            }
        }

        for i in 0..n {
            let nn = self.vertices[i].nrm().normalize_or_zero();
            let mut t = tan[i];
            // Gram–Schmidt 正交化。
            t = (t - nn * nn.dot(t)).normalize_or_zero();
            if t.length_squared() < 1e-8 {
                t = nn.any_orthonormal_pair().0;
            }
            let w = if nn.cross(t).dot(bit[i]) < 0.0 {
                -1.0
            } else {
                1.0
            };
            self.vertices[i].tangent = [t.x, t.y, t.z, w];
        }
    }

    // -----------------------------------------------------------------
    // 图元构造
    // -----------------------------------------------------------------

    /// 轴对齐立方体（边长 `size`，每个面独立法线/UV）。
    pub fn cube(size: f32) -> Self {
        Self::box_sized(Vec3::splat(size))
    }

    /// 长方体（全尺寸 `size`，即包围盒尺寸）。
    pub fn box_sized(size: Vec3) -> Self {
        let h = size * 0.5;
        // 每个面的 `u × v = n`，配合下面的逆时针缠绕保证几何法线与面法线一致（否则会被背面剔除）。
        let faces: [(Vec3, Vec3, Vec3, Vec3); 6] = [
            // (法线, 面内 U 轴, 面内 V 轴, 面中心)
            (Vec3::X, Vec3::Y, Vec3::Z, Vec3::new(h.x, 0.0, 0.0)),
            (Vec3::NEG_X, Vec3::Z, Vec3::Y, Vec3::new(-h.x, 0.0, 0.0)),
            (Vec3::Y, Vec3::Z, Vec3::X, Vec3::new(0.0, h.y, 0.0)),
            (Vec3::NEG_Y, Vec3::X, Vec3::Z, Vec3::new(0.0, -h.y, 0.0)),
            (Vec3::Z, Vec3::X, Vec3::Y, Vec3::new(0.0, 0.0, h.z)),
            (Vec3::NEG_Z, Vec3::Y, Vec3::X, Vec3::new(0.0, 0.0, -h.z)),
        ];
        let axis_extent = |a: Vec3| -> f32 {
            if a.x.abs() > 0.5 {
                h.x
            } else if a.y.abs() > 0.5 {
                h.y
            } else {
                h.z
            }
        };
        let mut vertices = Vec::with_capacity(24);
        let mut indices = Vec::with_capacity(36);
        for (n, u, v, c) in faces {
            let su = axis_extent(u);
            let sv = axis_extent(v);
            let base = vertices.len() as u32;
            for (du, dv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let p = c + u * (du * su) + v * (dv * sv);
                let uv = Vec2::new((du + 1.0) * 0.5, 1.0 - (dv + 1.0) * 0.5);
                vertices.push(Vertex::new(p, n, uv));
            }
            // 逆时针缠绕：几何法线 = u × v = n
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        Self::from_parts(vertices, indices)
    }

    /// 由半尺寸构造长方体。
    pub fn box_half_extents(half: Vec3) -> Self {
        Self::box_sized(half * 2.0)
    }

    /// UV 球体。
    pub fn sphere(radius: f32, sectors: u32, stacks: u32) -> Self {
        let sectors = sectors.max(3);
        let stacks = stacks.max(2);
        let mut vertices = Vec::with_capacity(((sectors + 1) * (stacks + 1)) as usize);
        let mut indices = Vec::new();

        for i in 0..=stacks {
            let v = i as f32 / stacks as f32;
            // 从北极（+90°）扫到南极（-90°）：y = sin(phi)，水平半径 = cos(phi) ≥ 0
            let phi = core::f32::consts::FRAC_PI_2 - v * core::f32::consts::PI;
            let (sp, cp) = phi.sin_cos();
            for j in 0..=sectors {
                let u = j as f32 / sectors as f32;
                let theta = u * core::f32::consts::TAU;
                let (st, ct) = theta.sin_cos();
                let n = Vec3::new(cp * st, sp, cp * ct);
                let uv = Vec2::new(u, 1.0 - v);
                vertices.push(Vertex::new(n * radius, n, uv));
            }
        }
        let stride = sectors + 1;
        for i in 0..stacks {
            for j in 0..sectors {
                let a = i * stride + j;
                let b = a + stride;
                indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
            }
        }
        Self::from_parts(vertices, indices)
    }

    /// 沿 Y 轴的胶囊体（两端半球 + 中段圆柱）。
    pub fn capsule(radius: f32, half_height: f32, sectors: u32, rings: u32) -> Self {
        let sectors = sectors.max(3);
        let rings = rings.max(4);
        let mut vertices = Vec::with_capacity(((sectors + 1) * (rings + 1)) as usize);
        let mut indices = Vec::new();

        for i in 0..=rings {
            let t = i as f32 / rings as f32;
            let a = core::f32::consts::FRAC_PI_2 - core::f32::consts::PI * t;
            let (sa, ca) = a.sin_cos();
            let sign = if a >= 0.0 { 1.0 } else { -1.0 };
            let y = radius * sa + sign * half_height;
            let r = radius * ca;
            for j in 0..=sectors {
                let u = j as f32 / sectors as f32;
                let theta = u * core::f32::consts::TAU;
                let (st, ct) = theta.sin_cos();
                let n = Vec3::new(ca * st, sa, ca * ct);
                let p = Vec3::new(r * st, y, r * ct);
                vertices.push(Vertex::new(p, n, Vec2::new(u, t)));
            }
        }
        let stride = sectors + 1;
        for i in 0..rings {
            for j in 0..sectors {
                let a = i * stride + j;
                let b = a + stride;
                indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
            }
        }
        Self::from_parts(vertices, indices)
    }

    /// 沿 Y 轴的圆柱体（带平盖）。
    pub fn cylinder(radius: f32, half_height: f32, sectors: u32) -> Self {
        let sectors = sectors.max(3);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();

        // 侧面
        for j in 0..=sectors {
            let u = j as f32 / sectors as f32;
            let theta = u * core::f32::consts::TAU;
            let (st, ct) = theta.sin_cos();
            let n = Vec3::new(st, 0.0, ct);
            vertices.push(Vertex::new(
                Vec3::new(radius * st, half_height, radius * ct),
                n,
                Vec2::new(u, 0.0),
            ));
            vertices.push(Vertex::new(
                Vec3::new(radius * st, -half_height, radius * ct),
                n,
                Vec2::new(u, 1.0),
            ));
        }
        for j in 0..sectors {
            let a = j * 2;
            indices.extend_from_slice(&[a, a + 1, a + 2, a + 2, a + 1, a + 3]);
        }

        // 上下盖
        for (y, ny, flip) in [(half_height, 1.0f32, false), (-half_height, -1.0, true)] {
            let center = vertices.len() as u32;
            vertices.push(Vertex::new(
                Vec3::new(0.0, y, 0.0),
                Vec3::new(0.0, ny, 0.0),
                Vec2::new(0.5, 0.5),
            ));
            for j in 0..=sectors {
                let u = j as f32 / sectors as f32;
                let theta = u * core::f32::consts::TAU;
                let (st, ct) = theta.sin_cos();
                vertices.push(Vertex::new(
                    Vec3::new(radius * st, y, radius * ct),
                    Vec3::new(0.0, ny, 0.0),
                    Vec2::new(0.5 + 0.5 * st, 0.5 + 0.5 * ct),
                ));
            }
            for j in 0..sectors {
                let a = center + 1 + j;
                if flip {
                    indices.extend_from_slice(&[center, a + 1, a]);
                } else {
                    indices.extend_from_slice(&[center, a, a + 1]);
                }
            }
        }
        Self::from_parts(vertices, indices)
    }

    /// 圆锥（底面半径 `radius`、高 `height`，底面在 y=0，尖端在 y=height）。
    pub fn cone(radius: f32, height: f32, sectors: u32) -> Self {
        let sectors = sectors.max(3);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();

        let slope = radius / height.max(1e-4);

        // 尖端
        let apex = vertices.len() as u32;
        vertices.push(Vertex::new(
            Vec3::new(0.0, height, 0.0),
            Vec3::Y,
            Vec2::new(0.5, 1.0),
        ));

        // 侧面底部一圈
        let ring = vertices.len() as u32;
        for j in 0..=sectors {
            let u = j as f32 / sectors as f32;
            let theta = u * core::f32::consts::TAU;
            let (st, ct) = theta.sin_cos();
            let p = Vec3::new(radius * st, 0.0, radius * ct);
            let n = Vec3::new(st, slope, ct).normalize_or_zero();
            vertices.push(Vertex::new(p, n, Vec2::new(u, 0.0)));
        }
        for j in 0..sectors {
            let a = ring + j;
            // 从外侧看逆时针
            indices.extend_from_slice(&[apex, a, a + 1]);
        }

        // 底面
        let center = vertices.len() as u32;
        vertices.push(Vertex::new(Vec3::ZERO, Vec3::NEG_Y, Vec2::new(0.5, 0.5)));
        for j in 0..=sectors {
            let u = j as f32 / sectors as f32;
            let theta = u * core::f32::consts::TAU;
            let (st, ct) = theta.sin_cos();
            vertices.push(Vertex::new(
                Vec3::new(radius * st, 0.0, radius * ct),
                Vec3::NEG_Y,
                Vec2::new(0.5 + 0.5 * st, 0.5 + 0.5 * ct),
            ));
        }
        for j in 0..sectors {
            let a = center + 1 + j;
            // 底面朝 -Y，缠绕需相应反向
            indices.extend_from_slice(&[center, a + 1, a]);
        }
        Self::from_parts(vertices, indices)
    }

    /// 圆环（`major` 为主半径，`minor` 为管半径，位于 XZ 平面）。
    pub fn torus(major: f32, minor: f32, major_seg: u32, minor_seg: u32) -> Self {
        let major_seg = major_seg.max(3);
        let minor_seg = minor_seg.max(3);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();

        for i in 0..=major_seg {
            let u = i as f32 / major_seg as f32;
            let theta = u * core::f32::consts::TAU;
            let (st, ct) = theta.sin_cos();
            let center = Vec3::new(major * st, 0.0, major * ct);
            let radial = Vec3::new(st, 0.0, ct);
            for j in 0..=minor_seg {
                let v = j as f32 / minor_seg as f32;
                let phi = v * core::f32::consts::TAU;
                let (sp, cp) = phi.sin_cos();
                let n = radial * cp + Vec3::Y * sp;
                vertices.push(Vertex::new(center + n * minor, n, Vec2::new(u, v)));
            }
        }
        let stride = minor_seg + 1;
        for i in 0..major_seg {
            for j in 0..minor_seg {
                let a = i * stride + j;
                let b = a + stride;
                indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
            }
        }
        Self::from_parts(vertices, indices)
    }

    /// XZ 平面上的细分网格（法线朝 +Y）。`size` 为全尺寸，`subdivisions` 为每边分段数。
    pub fn plane(size: f32, subdivisions: u32) -> Self {
        let n = subdivisions.max(1);
        let h = size * 0.5;
        let mut vertices = Vec::new();
        for i in 0..=n {
            for j in 0..=n {
                let x = -h + size * (j as f32 / n as f32);
                let z = -h + size * (i as f32 / n as f32);
                vertices.push(Vertex::new(
                    Vec3::new(x, 0.0, z),
                    Vec3::Y,
                    Vec2::new(j as f32 / n as f32, i as f32 / n as f32),
                ));
            }
        }
        let mut indices = Vec::new();
        let stride = n + 1;
        for i in 0..n {
            for j in 0..n {
                let a = i * stride + j;
                let b = a + stride;
                // 法线朝 +Y：从上方看逆时针
                indices.extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
            }
        }
        Self::from_parts(vertices, indices)
    }

    /// 面向 +Z 的四边形（全屏后处理与公告板用）。
    pub fn quad(size: Vec2) -> Self {
        let h = size * 0.5;
        let vertices = vec![
            Vertex::new(Vec3::new(-h.x, -h.y, 0.0), Vec3::Z, Vec2::new(0.0, 1.0)),
            Vertex::new(Vec3::new(h.x, -h.y, 0.0), Vec3::Z, Vec2::new(1.0, 1.0)),
            Vertex::new(Vec3::new(h.x, h.y, 0.0), Vec3::Z, Vec2::new(1.0, 0.0)),
            Vertex::new(Vec3::new(-h.x, h.y, 0.0), Vec3::Z, Vec2::new(0.0, 0.0)),
        ];
        Self::from_parts(vertices, vec![0, 1, 2, 0, 2, 3])
    }

    /// 由高度场构造网格（行主序：`heights[行][列]`，行沿 Z、列沿 X）。
    ///
    /// 网格在 XZ 平面**以原点为中心**，`y = heights[行][列]`，`cell` 是格子的 X/Z 边长；
    /// 每格两个三角形，绕序保证法线朝 +Y。法线按三角形算（**平法线**）——地形面片的硬边比
    /// 平滑法线更不容易在大落差处糊掉。UV 覆盖整块地形 `[0, 1]²`，所以材质的 `uv_scale`
    /// 是"整块地形上的平铺次数"。
    ///
    /// 行数/列数不足 2、各行长度不一致、`cell` 非正时返回**空网格**：场景格式的校验层会先
    /// 拦下这些输入，这里只保证不 panic（不 panic 是给"直接调 API 的人"的底线）。
    pub fn heightfield(heights: &[Vec<f32>], cell: f32) -> Self {
        let rows = heights.len();
        let cols = heights.first().map(|r| r.len()).unwrap_or(0);
        if rows < 2
            || cols < 2
            || heights.iter().any(|r| r.len() != cols)
            || !cell.is_finite()
            || cell <= 0.0
        {
            return Self::new();
        }
        let (cx, cz) = ((cols - 1) as f32 * 0.5, (rows - 1) as f32 * 0.5);
        let (dx, dz) = ((cols - 1) as f32, (rows - 1) as f32);
        let mut vertices = Vec::with_capacity(rows * cols);
        for (r, row) in heights.iter().enumerate() {
            let z = (r as f32 - cz) * cell;
            let v = r as f32 / dz;
            for (c, h) in row.iter().enumerate() {
                let x = (c as f32 - cx) * cell;
                vertices.push(Vertex::new(
                    Vec3::new(x, *h, z),
                    Vec3::Y,
                    Vec2::new(c as f32 / dx, v),
                ));
            }
        }
        // 绕序：`(v00, v10, v11)` 与 `(v00, v11, v01)` 的法线都朝 +Y（见 mesh.rs 的测试）。
        let mut indices = Vec::with_capacity(6 * (rows - 1) * (cols - 1));
        for r in 0..rows - 1 {
            for c in 0..cols - 1 {
                let v00 = (r * cols + c) as u32;
                let v01 = v00 + 1;
                let v10 = ((r + 1) * cols + c) as u32;
                let v11 = v10 + 1;
                indices.extend_from_slice(&[v00, v10, v11, v00, v11, v01]);
            }
        }
        let mut m = Self::from_parts(vertices, indices);
        m.recompute_flat_normals();
        m
    }

    /// 圆角盒（"盒子 ⊕ 半径 `radius` 的球"）：`half_extents` 是**外接**半长。
    ///
    /// 生成方式是从球的**方向网格径向投影**到圆角盒表面：对每个单位方向 `n`，沿射线
    /// `t·n` 求"距内盒 `half_extents - radius` 恰好 `radius`"的那个 `t`（二分求根）。
    /// 凸体的径向投影是双射，所以顶点一定落在表面上、三角形不会自交；拓扑也直接沿用
    /// 球的（封闭、朝向一致）。法线取 SDF 的梯度（解析精确），不做事后平均。
    ///
    /// 代价是**网格疏密不均**（面片中心附近三角形大、圆角处密），这是径向投影的固有性质；
    /// 要均匀就得按"6 面片 + 12 段 1/4 圆柱 + 8 个 1/8 球"拼接，代码量翻几倍。
    /// 半长非正、半径非有限时返回空网格（不 panic）。
    pub fn rounded_box(half_extents: Vec3, radius: f32, sectors: u32, stacks: u32) -> Self {
        let he = Vec3::new(
            half_extents.x.abs(),
            half_extents.y.abs(),
            half_extents.z.abs(),
        );
        if !he.is_finite() || he.min_element() <= 0.0 || !radius.is_finite() {
            return Self::new();
        }
        let r = radius.abs().min(he.min_element());
        let inner = (he - Vec3::splat(r)).max(Vec3::ZERO);
        let mut m = Self::sphere(1.0, sectors, stacks);
        let sdf = |p: Vec3| (p - p.clamp(-inner, inner)).length() - r;
        for v in &mut m.vertices {
            let n = v.pos().normalize_or_zero();
            // 沿 n 找表面：从"球半径方向"出发，步进到 SDF 变号再二分。
            let mut lo = 0.0f32;
            let mut hi = he.length() + r + 1.0;
            for _ in 0..48 {
                let mid = 0.5 * (lo + hi);
                if sdf(n * mid) > 0.0 {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            let p = n * (0.5 * (lo + hi));
            let g = (p - p.clamp(-inner, inner)).normalize_or_zero();
            let normal = if g.length_squared() > 0.5 {
                g
            } else {
                // 半径 0 的退化情形：法线取最外侧轴
                let a = [p.x.abs(), p.y.abs(), p.z.abs()];
                let axis = if a[0] >= a[1] && a[0] >= a[2] {
                    0
                } else if a[1] >= a[2] {
                    1
                } else {
                    2
                };
                let mut nrm = Vec3::ZERO;
                nrm[axis] = if [p.x, p.y, p.z][axis] >= 0.0 {
                    1.0
                } else {
                    -1.0
                };
                nrm
            };
            let uv = v.uv;
            *v = Vertex::new(p, normal, Vec2::new(uv[0], uv[1]));
        }
        m
    }

    /// 由三角形汤（每 3 个顶点一个三角形）构造带平法线的网格。
    pub fn from_triangles(positions: &[Vec3]) -> Self {
        let mut vertices = Vec::with_capacity(positions.len());
        for p in positions {
            vertices.push(Vertex::new(*p, Vec3::Y, Vec2::ZERO));
        }
        let indices = (0..positions.len() as u32).collect();
        let mut m = Self::from_parts(vertices, indices);
        m.recompute_flat_normals();
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 圆角盒网格：所有顶点都在表面（SDF = 0）、法线朝外且与位置一致、AABB 等于外接盒。
    #[test]
    fn rounded_box_mesh_lies_on_the_surface() {
        let he = Vec3::new(1.0, 0.6, 0.35);
        let r = 0.12;
        let m = MeshData::rounded_box(he, r, 32, 16);
        assert_eq!(m.vertices.len(), 33 * 17, "沿用球的方向网格");
        assert_eq!(m.triangle_count(), 2 * 32 * 16);
        let inner = he - Vec3::splat(r);
        let sdf = |p: Vec3| (p - p.clamp(-inner, inner)).length() - r;
        for v in &m.vertices {
            let p = v.pos();
            assert!(sdf(p).abs() < 1e-4, "顶点该在表面：{p:?} sdf={}", sdf(p));
            let n = v.nrm();
            assert!((n.length() - 1.0).abs() < 1e-4, "法线该是单位向量：{n:?}");
            // 法线 == SDF 梯度（朝外）
            let g = (p - p.clamp(-inner, inner)).normalize_or_zero();
            assert!((n - g).length() < 1e-3, "法线该朝外：{n:?} vs {g:?}");
        }
        // AABB == 外接盒（sectors 是 4 的倍数时 ±X/±Z 面心正好被采到，极点是 ±Y）
        let aabb = m.aabb();
        assert!((aabb.min + he).length() < 1e-4, "{:?}", aabb.min);
        assert!((aabb.max - he).length() < 1e-4, "{:?}", aabb.max);

        // r = 0 退化成普通盒：顶点必须贴在盒面上，且三个方向都到边
        let boxy = MeshData::rounded_box(Vec3::splat(0.5), 0.0, 32, 16);
        for v in &boxy.vertices {
            let p = v.pos();
            let m = [p.x.abs(), p.y.abs(), p.z.abs()];
            assert!(
                (m[0] - 0.5).abs() < 1e-4 || (m[1] - 0.5).abs() < 1e-4 || (m[2] - 0.5).abs() < 1e-4,
                "顶点该在盒面上：{p:?}"
            );
        }
        // 退化输入：空网格，不 panic
        assert!(MeshData::rounded_box(Vec3::ZERO, 0.1, 8, 4).is_empty());
        assert!(MeshData::rounded_box(Vec3::splat(1.0), f32::NAN, 8, 4).is_empty());
        // 负半长按绝对值处理（防御性，和物理 `Shape` 一致），不是空网格
        let neg = MeshData::rounded_box(Vec3::splat(-1.0), 0.1, 8, 4);
        assert!(
            (neg.aabb().max - Vec3::splat(1.0)).length() < 0.2,
            "负半长取绝对值"
        );
    }

    /// 高度场网格：格点数、三角形数、居中性、绕序（法线朝 +Y）、UV 范围、退化输入不 panic。
    #[test]
    fn heightfield_mesh_is_centered_and_faces_up() {
        let heights = vec![
            vec![0.0, 0.5, 1.0],
            vec![0.25, 0.75, 1.5],
            vec![0.5, 1.0, 2.0],
            vec![0.75, 1.25, 2.5],
        ]; // 4 行 3 列
        let cell = 2.0;
        let m = MeshData::heightfield(&heights, cell);
        assert_eq!(m.vertices.len(), 12, "格点数 = 行 × 列");
        assert_eq!(m.triangle_count(), 2 * 3 * 2, "每格两个三角形");
        // 位置：x = (列-1)·cell（居中），z = (行-1)·cell（居中），y = 高度
        let pos = |r: usize, c: usize| m.vertices[r * 3 + c].pos();
        assert!((pos(0, 0).x + 2.0).abs() < 1e-5 && (pos(0, 2).x - 2.0).abs() < 1e-5);
        assert!((pos(0, 0).z + 3.0).abs() < 1e-5 && (pos(3, 0).z - 3.0).abs() < 1e-5);
        for (r, row) in heights.iter().enumerate() {
            for (c, h) in row.iter().enumerate() {
                assert!((pos(r, c).y - h).abs() < 1e-6);
            }
        }
        // 绕序：每个三角形的几何法线都朝 +Y（地形朝上），与顶点法线一致
        for tri in m.indices.as_chunks::<3>().0 {
            let (a, b, c) = (
                m.vertices[tri[0] as usize].pos(),
                m.vertices[tri[1] as usize].pos(),
                m.vertices[tri[2] as usize].pos(),
            );
            let n = (b - a).cross(c - a);
            assert!(n.y > 0.0, "三角形绕序反了（法线 {n:?}）");
            for i in tri {
                // 顶点法线来自"最后一个碰到它的那个面"，所以只在**上半球**上断言：
                // 地形有坡度，面法线本来就该歪开 +Y（陡坡上可以歪到接近水平）。
                assert!(
                    m.vertices[*i as usize].nrm().y > 0.0,
                    "顶点法线该朝上，实际 {:?}（第 {i} 个顶点，三角形 {tri:?}）",
                    m.vertices[*i as usize].nrm()
                );
            }
        }
        // UV 覆盖 [0,1]²（所以 uv_scale 是"整块地形上的平铺次数"）
        for v in &m.vertices {
            let (u, w) = (v.uv[0], v.uv[1]);
            assert!((0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&w));
        }
        assert_eq!(m.vertices[0].uv[0], 0.0);
        assert_eq!(m.vertices[2].uv[0], 1.0);
        assert_eq!(m.vertices[9].uv[1], 1.0);

        // 一张平地：AABB 的高度为 0，宽度/深度 = (列-1)·cell / (行-1)·cell
        let flat_heights = vec![vec![0.0f32; 3]; 4];
        let flat = MeshData::heightfield(&flat_heights, 2.0);
        let (lo, hi) = (flat.aabb().min, flat.aabb().max);
        assert!((hi.y - lo.y).abs() < 1e-6, "平地的高度必须是 0");
        assert!((hi.x - lo.x - 4.0).abs() < 1e-5 && (hi.z - lo.z - 6.0).abs() < 1e-5);
        assert!(
            (lo.x + 2.0).abs() < 1e-5 && (lo.z + 3.0).abs() < 1e-5,
            "必须以原点为中心"
        );

        // 退化输入：空、只有一行、行长不一、cell ≤ 0 —— 一律空网格，不 panic
        for (h, c) in [
            (vec![], 1.0),
            (vec![vec![0.0, 1.0]], 1.0),
            (vec![vec![0.0, 1.0], vec![0.0]], 1.0),
            (vec![vec![0.0, 1.0], vec![0.0, 1.0]], 0.0),
            (vec![vec![0.0, 1.0], vec![0.0, 1.0]], f32::NAN),
        ] {
            assert!(
                MeshData::heightfield(&h, c).is_empty(),
                "退化输入该是空网格"
            );
        }
    }

    #[test]
    fn primitives_are_valid() {
        for m in [
            MeshData::cube(1.0),
            MeshData::sphere(1.0, 16, 12),
            MeshData::capsule(0.5, 0.5, 16, 12),
            MeshData::cylinder(0.5, 1.0, 16),
            MeshData::cone(0.5, 1.0, 16),
            MeshData::torus(1.0, 0.25, 24, 12),
            MeshData::plane(10.0, 4),
            MeshData::quad(Vec2::new(2.0, 2.0)),
        ] {
            assert!(!m.is_empty());
            assert_eq!(m.indices.len() % 3, 0);
            assert!(m.indices.iter().all(|i| (*i as usize) < m.vertices.len()));
            for v in &m.vertices {
                assert!(v.pos().is_finite(), "顶点出现非有限值");
                assert!(
                    (v.nrm().length() - 1.0).abs() < 1e-3,
                    "法线未归一化: {:?}",
                    v.nrm()
                );
            }
            assert!(m.aabb().is_valid());
        }
    }

    #[test]
    fn cube_aabb_matches_size() {
        let m = MeshData::box_sized(Vec3::new(2.0, 4.0, 6.0));
        let b = m.aabb();
        assert!((b.size() - Vec3::new(2.0, 4.0, 6.0)).length() < 1e-5);
        assert!(b.center().length() < 1e-5);
    }

    #[test]
    fn capsule_aabb_height() {
        let m = MeshData::capsule(0.5, 1.0, 24, 16);
        let b = m.aabb();
        // 总高 = 2*half_height + 2*radius = 3.0
        assert!((b.size().y - 3.0).abs() < 0.05, "高度 {}", b.size().y);
    }

    #[test]
    fn primitive_winding_matches_normals() {
        let meshes = [
            ("cube", MeshData::cube(2.0)),
            ("box", MeshData::box_sized(Vec3::new(1.0, 2.0, 3.0))),
            ("sphere", MeshData::sphere(1.0, 24, 16)),
            ("capsule", MeshData::capsule(0.5, 0.5, 20, 12)),
            ("cylinder", MeshData::cylinder(0.5, 0.5, 20)),
            ("cone", MeshData::cone(0.5, 1.0, 20)),
            ("plane", MeshData::plane(1.0, 2)),
            ("torus", MeshData::torus(1.0, 0.3, 20, 12)),
        ];
        for (name, m) in meshes {
            for (t, tri) in m.indices.as_chunks::<3>().0.iter().enumerate() {
                let v = [
                    &m.vertices[tri[0] as usize],
                    &m.vertices[tri[1] as usize],
                    &m.vertices[tri[2] as usize],
                ];
                let geo = (v[1].pos() - v[0].pos()).cross(v[2].pos() - v[0].pos());
                if geo.length_squared() < 1e-14 {
                    continue;
                }
                let n = (v[0].nrm() + v[1].nrm() + v[2].nrm()).normalize_or_zero();
                assert!(
                    geo.normalize().dot(n) > 0.0,
                    "{name} 第 {t} 个三角形缠绕方向与法线相反（会被背面剔除掉）"
                );
            }
        }
    }

    #[test]
    fn flat_normals_point_outward_on_cube() {
        let m = MeshData::cube(2.0);
        for v in &m.vertices {
            assert!((v.nrm().length() - 1.0).abs() < 1e-5, "法线应为单位向量");
            // 顶点位置与面法线的点积为正 ⇒ 法线朝外
            assert!(v.pos().dot(v.nrm()) > 0.0, "立方体面法线应朝外");
            // 立方体的每个面法线必须是 ±X / ±Y / ±Z
            assert!(v.nrm().abs().max_element() > 0.999, "面法线应轴对齐");
        }
    }

    #[test]
    fn sphere_bounds_match_radius() {
        let m = MeshData::sphere(1.5, 32, 16);
        let b = m.aabb();
        let size = b.size();
        assert!(
            (size.x - 3.0).abs() < 0.02
                && (size.y - 3.0).abs() < 0.02
                && (size.z - 3.0).abs() < 0.02,
            "球体包围盒应为边长 2r 的立方体，实际 {size:?}"
        );
        assert!(b.center().length() < 0.02, "球体应以原点为中心");
    }

    #[test]
    fn tangents_are_orthogonal() {
        let mut m = MeshData::sphere(1.0, 24, 16);
        m.recompute_tangents();
        for v in &m.vertices {
            let t = Vec3::from_array([v.tangent[0], v.tangent[1], v.tangent[2]]);
            assert!(t.dot(v.nrm()).abs() < 0.05, "切线应与法线正交");
            assert!((t.length() - 1.0).abs() < 0.05);
        }
    }

    #[test]
    fn transform_moves_aabb() {
        let m = MeshData::cube(1.0);
        let t = m.transformed(&Mat4::from_translation(Vec3::new(5.0, 0.0, 0.0)));
        assert!((t.aabb().center() - Vec3::new(5.0, 0.0, 0.0)).length() < 1e-5);
    }

    #[test]
    fn merge_offsets_indices() {
        let mut a = MeshData::cube(1.0);
        let b = MeshData::sphere(1.0, 8, 6);
        let before = a.vertices.len();
        a.merge(&b);
        assert_eq!(a.vertices.len(), before + b.vertices.len());
        assert!(a.indices.iter().all(|i| (*i as usize) < a.vertices.len()));
    }
}
