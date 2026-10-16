//! 形状定义、支持函数与质量属性。
//!
//! 所有形状都以「质心在本地原点」为约定，`MassProperties::center` 描述
//! 实际质心相对本地原点的偏移（对规则体恒为零，凸包由采样得到）。

use core::f32::consts::PI;

use quench3d_math::*;

use crate::gjk::point_in_convex;

/// 凸包质量属性采样的每轴网格分辨率下限/上限。
const HULL_SAMPLES_MIN: usize = 8;
/// 凸包质量属性采样的每轴网格分辨率上限。
const HULL_SAMPLES_MAX: usize = 16;

/// 碰撞形状。
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// 球体。
    Sphere {
        /// 半径。
        radius: f32,
    },
    /// 长方体（本地轴对齐）。
    Box {
        /// 各轴半长。
        half_extents: Vec3,
    },
    /// 胶囊（沿本地 Y 轴）。
    Capsule {
        /// 圆柱段半高。
        half_height: f32,
        /// 半径。
        radius: f32,
    },
    /// 圆柱（沿本地 Y 轴）。
    Cylinder {
        /// 半高。
        half_height: f32,
        /// 半径。
        radius: f32,
    },
    /// 凸包（点集已包含质心）。
    ConvexHull {
        /// 顶点集。
        points: Vec<Vec3>,
    },
    /// 无限半空间（只能作为静态体）。
    Plane {
        /// 本地法线（指向实体外部）。
        normal: Vec3,
    },
    /// 静态三角形网格。
    TriMesh {
        /// 顶点。
        vertices: Vec<Vec3>,
        /// 三角形索引。
        indices: Vec<u32>,
    },
}

/// 质量属性：总质量、绕质心的本地对角惯量、质心（本地坐标）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MassProperties {
    /// 质量（静态形状为 0，表示无限大）。
    pub mass: f32,
    /// 绕质心的本地对角惯性张量。
    pub inertia: Vec3,
    /// 质心在本地坐标系中的位置。
    pub center: Vec3,
}

impl Default for MassProperties {
    fn default() -> Self {
        Self::ZERO
    }
}

impl MassProperties {
    /// 零质量属性（无限质量、零惯量）。
    pub const ZERO: Self = Self {
        mass: 0.0,
        inertia: Vec3::ZERO,
        center: Vec3::ZERO,
    };

    /// 是否可参与动力学。
    #[inline]
    pub fn is_finite(&self) -> bool {
        self.mass.is_finite() && self.inertia.is_finite() && self.center.is_finite()
    }
}

/// 长方体绕质心的对角惯量。
fn box_inertia(mass: f32, he: Vec3) -> Vec3 {
    Vec3::new(
        mass / 3.0 * (he.y * he.y + he.z * he.z),
        mass / 3.0 * (he.x * he.x + he.z * he.z),
        mass / 3.0 * (he.x * he.x + he.y * he.y),
    )
}

impl Shape {
    /// 形状沿法线方向"长胖" `margin` 米（配合 `SolverConfig::speculative_margin`）。
    ///
    /// 用法是**接触余量**：拿膨胀后的形状做窄相，再把穿透量减去 `margin`，于是
    /// "距离小于 margin 但还没碰上"也会得到接触点（穿透量为负 = 分离）。
    /// 球/盒/胶囊/柱按半径或半长膨胀；平面、凸包、三角网**原样返回**
    /// （平面余量没有意义；凸包/三角网要重算表面——都是明确边界，不是漏做）。
    pub fn inflated(&self, margin: f32) -> Shape {
        if margin <= 0.0 {
            return self.clone();
        }
        match self {
            Shape::Sphere { radius } => Shape::Sphere {
                radius: radius + margin,
            },
            Shape::Box { half_extents } => Shape::Box {
                half_extents: *half_extents + Vec3::splat(margin),
            },
            Shape::Capsule {
                half_height,
                radius,
            } => Shape::Capsule {
                half_height: *half_height,
                radius: radius + margin,
            },
            Shape::Cylinder {
                half_height,
                radius,
            } => Shape::Cylinder {
                half_height: *half_height,
                radius: radius + margin,
            },
            other => other.clone(),
        }
    }

    /// 体积（半空间/网格返回 0）。
    pub fn volume(&self) -> f32 {
        match self {
            Shape::Sphere { radius } => 4.0 / 3.0 * PI * radius * radius * radius,
            Shape::Box { half_extents } => 8.0 * half_extents.x * half_extents.y * half_extents.z,
            Shape::Capsule {
                half_height,
                radius,
            } => {
                PI * radius * radius * 2.0 * half_height + 4.0 / 3.0 * PI * radius * radius * radius
            }
            Shape::Cylinder {
                half_height,
                radius,
            } => PI * radius * radius * 2.0 * half_height,
            Shape::ConvexHull { points } => {
                let aabb = Aabb::from_points(points);
                if !aabb.is_valid() {
                    0.0
                } else {
                    let v = aabb.volume();
                    // 凸包体积用 AABB 体积乘填充率近似（采样得到）。
                    v * hull_fill_ratio(points)
                }
            }
            Shape::Plane { .. } | Shape::TriMesh { .. } => 0.0,
        }
    }

    /// 体积/质量/本地对角惯量。
    ///
    /// `Plane` 与 `TriMesh` 返回零质量（视为无限质量静态体）。
    pub fn mass_properties(&self, density: f32) -> MassProperties {
        let d = if density.is_finite() && density > 0.0 {
            density
        } else {
            0.0
        };
        match self {
            Shape::Sphere { radius } => {
                let r = radius.abs();
                let mass = 4.0 / 3.0 * PI * r * r * r * d;
                let i = 0.4 * mass * r * r;
                MassProperties {
                    mass,
                    inertia: Vec3::splat(i),
                    center: Vec3::ZERO,
                }
            }
            Shape::Box { half_extents } => {
                let he = half_extents.abs();
                let mass = 8.0 * he.x * he.y * he.z * d;
                MassProperties {
                    mass,
                    inertia: box_inertia(mass, he),
                    center: Vec3::ZERO,
                }
            }
            Shape::Capsule {
                half_height,
                radius,
            } => {
                let h = half_height.abs();
                let r = radius.abs();
                let v_cyl = PI * r * r * 2.0 * h;
                let v_sph = 4.0 / 3.0 * PI * r * r * r;
                let volume = v_cyl + v_sph;
                let mass = volume * d;
                let (mc, ms) = if volume > 1e-12 {
                    (mass * v_cyl / volume, mass * v_sph / volume)
                } else {
                    (0.0, 0.0)
                };
                // 圆柱 + 两个半球（半球质心距底面 3r/8）。
                let i_axis = 0.5 * mc * r * r + 0.4 * ms * r * r;
                let i_perp =
                    mc * (h * h / 3.0 + r * r / 4.0) + ms * (h * h + 0.4 * r * r + 0.75 * r * h);
                MassProperties {
                    mass,
                    inertia: Vec3::new(i_perp, i_axis, i_perp),
                    center: Vec3::ZERO,
                }
            }
            Shape::Cylinder {
                half_height,
                radius,
            } => {
                let h = half_height.abs();
                let r = radius.abs();
                let mass = PI * r * r * 2.0 * h * d;
                let i_axis = 0.5 * mass * r * r;
                let i_perp = mass * (h * h / 3.0 + r * r / 4.0);
                MassProperties {
                    mass,
                    inertia: Vec3::new(i_perp, i_axis, i_perp),
                    center: Vec3::ZERO,
                }
            }
            Shape::ConvexHull { points } => hull_mass_properties(points, d),
            Shape::Plane { .. } | Shape::TriMesh { .. } => MassProperties::ZERO,
        }
    }

    /// 本地空间轴对齐包围盒。
    pub fn local_aabb(&self) -> Aabb {
        match self {
            Shape::Sphere { radius } => {
                let r = radius.abs();
                Aabb::new(Vec3::splat(-r), Vec3::splat(r))
            }
            Shape::Box { half_extents } => {
                let he = half_extents.abs();
                Aabb::new(-he, he)
            }
            Shape::Capsule {
                half_height,
                radius,
            } => {
                let h = half_height.abs() + radius.abs();
                let r = radius.abs();
                Aabb::new(vec3(-r, -h, -r), vec3(r, h, r))
            }
            Shape::Cylinder {
                half_height,
                radius,
            } => {
                let r = radius.abs();
                let h = half_height.abs();
                Aabb::new(vec3(-r, -h, -r), vec3(r, h, r))
            }
            Shape::ConvexHull { points } => Aabb::from_points(points),
            Shape::Plane { .. } => Aabb::new(Vec3::NEG_INFINITY, Vec3::INFINITY),
            Shape::TriMesh { vertices, indices } => {
                let mut b = Aabb::EMPTY;
                for tri in indices.as_chunks::<3>().0 {
                    for &i in tri {
                        if let Some(v) = vertices.get(i as usize) {
                            b.expand_point(*v);
                        }
                    }
                }
                if b.is_valid() {
                    b
                } else {
                    Aabb::from_points(vertices)
                }
            }
        }
    }

    /// 是否只能作为静态形状（半空间 / 三角网格）。
    #[inline]
    pub fn is_static_only(&self) -> bool {
        matches!(self, Shape::Plane { .. } | Shape::TriMesh { .. })
    }

    /// 是否为无限半空间。
    #[inline]
    pub fn is_plane(&self) -> bool {
        matches!(self, Shape::Plane { .. })
    }

    /// 包围球半径。
    pub fn bounding_radius(&self) -> f32 {
        match self {
            Shape::Sphere { radius } => radius.abs(),
            Shape::Box { half_extents } => half_extents.abs().length(),
            Shape::Capsule {
                half_height,
                radius,
            } => half_height.abs() + radius.abs(),
            Shape::Cylinder {
                half_height,
                radius,
            } => (half_height * half_height + radius * radius).sqrt(),
            Shape::ConvexHull { points } => points
                .iter()
                .map(|p| p.length_squared())
                .fold(0.0f32, f32::max)
                .sqrt(),
            Shape::Plane { .. } => f32::INFINITY,
            Shape::TriMesh { vertices, .. } => vertices
                .iter()
                .map(|p| p.length_squared())
                .fold(0.0f32, f32::max)
                .sqrt(),
        }
    }

    /// 支持函数（本地空间）：返回形状上沿 `dir` 的最远点。
    ///
    /// `Plane` 是无界半空间，没有有限支持点，正规碰撞路径不会调用它；
    /// 这里返回零向量仅作防御。
    pub fn support(&self, dir: Vec3) -> Vec3 {
        match self {
            Shape::Sphere { radius } => dir.normalize_or_zero() * radius.abs(),
            Shape::Box { half_extents } => Vec3::new(
                if dir.x >= 0.0 {
                    half_extents.x
                } else {
                    -half_extents.x
                },
                if dir.y >= 0.0 {
                    half_extents.y
                } else {
                    -half_extents.y
                },
                if dir.z >= 0.0 {
                    half_extents.z
                } else {
                    -half_extents.z
                },
            ),
            Shape::Capsule {
                half_height,
                radius,
            } => {
                let y = if dir.y >= 0.0 {
                    half_height.abs()
                } else {
                    -half_height.abs()
                };
                vec3(0.0, y, 0.0) + dir.normalize_or_zero() * radius.abs()
            }
            Shape::Cylinder {
                half_height,
                radius,
            } => {
                let radial = vec3(dir.x, 0.0, dir.z).normalize_or_zero() * radius.abs();
                let y = if dir.y >= 0.0 {
                    half_height.abs()
                } else {
                    -half_height.abs()
                };
                vec3(radial.x, y, radial.z)
            }
            Shape::ConvexHull { points } => max_dot_point(points, dir),
            Shape::TriMesh { vertices, .. } => max_dot_point(vertices, dir),
            // 半空间无界，不存在有限支持点；正规碰撞路径（解析半空间接触）不会
            // 调用它，这里返回零向量仅作防御，避免 NaN 扩散。
            Shape::Plane { .. } => Vec3::ZERO,
        }
    }
}

fn max_dot_point(points: &[Vec3], dir: Vec3) -> Vec3 {
    let mut best = Vec3::ZERO;
    let mut best_d = f32::NEG_INFINITY;
    for p in points {
        let d = p.dot(dir);
        if d > best_d {
            best_d = d;
            best = *p;
        }
    }
    if best_d == f32::NEG_INFINITY {
        Vec3::ZERO
    } else {
        best
    }
}

/// 用固定网格采样估计凸包在 AABB 中的填充率。
fn hull_fill_ratio(points: &[Vec3]) -> f32 {
    if points.len() < 4 {
        return 0.0;
    }
    let aabb = Aabb::from_points(points);
    if !aabb.is_valid() {
        return 0.0;
    }
    let size = aabb.size();
    if size.min_element() <= 1e-9 {
        return 0.0;
    }
    let n = sample_resolution(points.len());
    let tol = size.max_element() * 1e-3;
    let mut inside = 0usize;
    for i in 0..n {
        for j in 0..n {
            for k in 0..n {
                let p = aabb.min
                    + vec3(
                        (i as f32 + 0.5) * size.x,
                        (j as f32 + 0.5) * size.y,
                        (k as f32 + 0.5) * size.z,
                    ) / n as f32;
                if point_in_convex(points, p, tol) {
                    inside += 1;
                }
            }
        }
    }
    inside as f32 / (n * n * n) as f32
}

fn sample_resolution(point_count: usize) -> usize {
    if point_count <= 32 {
        HULL_SAMPLES_MAX
    } else if point_count <= 128 {
        12
    } else {
        HULL_SAMPLES_MIN
    }
}

/// 凸包的质量属性（**近似**：用确定性网格采样求体积、质心与对角惯量）。
///
/// 说明：仅凭顶点集无法在无面信息时精确积分，这里在 AABB 内做固定分辨率
/// 的规则网格采样（每个格心用 GJK 判定是否在凸包内），把命中格点当作等质量
/// 微元累加。采样完全确定，因此不影响引擎的可复现性。
fn hull_mass_properties(points: &[Vec3], density: f32) -> MassProperties {
    if points.len() < 4 || density <= 0.0 {
        return MassProperties::ZERO;
    }
    let aabb = Aabb::from_points(points);
    if !aabb.is_valid() {
        return MassProperties::ZERO;
    }
    let size = aabb.size();
    if size.min_element() <= 1e-9 {
        return MassProperties::ZERO;
    }
    let n = sample_resolution(points.len());
    let tol = size.max_element() * 1e-3;
    let mut samples: Vec<Vec3> = Vec::with_capacity(n * n * n);
    for i in 0..n {
        for j in 0..n {
            for k in 0..n {
                let p = aabb.min
                    + vec3(
                        (i as f32 + 0.5) * size.x,
                        (j as f32 + 0.5) * size.y,
                        (k as f32 + 0.5) * size.z,
                    ) / n as f32;
                if point_in_convex(points, p, tol) {
                    samples.push(p);
                }
            }
        }
    }
    if samples.is_empty() {
        // 退化兜底：用 AABB 当长方体。
        let mass = aabb.volume() * density;
        return MassProperties {
            mass,
            inertia: box_inertia(mass, aabb.half_extents()),
            center: aabb.center(),
        };
    }
    let inv_count = 1.0 / samples.len() as f32;
    let mut center = Vec3::ZERO;
    for s in &samples {
        center += *s;
    }
    center *= inv_count;
    let volume = aabb.volume() * samples.len() as f32 * inv_count;
    let mass = volume * density;
    let cell_mass = mass * inv_count;
    let mut inertia = Vec3::ZERO;
    for s in &samples {
        let r = *s - center;
        inertia += vec3(
            r.y * r.y + r.z * r.z,
            r.x * r.x + r.z * r.z,
            r.x * r.x + r.y * r.y,
        ) * cell_mass;
    }
    MassProperties {
        mass,
        inertia,
        center,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_mass_properties() {
        let s = Shape::Box {
            half_extents: Vec3::splat(0.5),
        };
        let mp = s.mass_properties(1000.0);
        assert!((mp.mass - 1000.0).abs() < 1e-2, "质量 {}", mp.mass);
        let expected = 1000.0 / 6.0;
        assert!(
            (mp.inertia.x - expected).abs() < 1e-1,
            "惯量 {} 期望 {}",
            mp.inertia.x,
            expected
        );
        assert_eq!(mp.center, Vec3::ZERO);
    }

    #[test]
    fn sphere_mass_properties() {
        let s = Shape::Sphere { radius: 1.0 };
        let mp = s.mass_properties(1.0);
        let v = 4.0 / 3.0 * PI;
        assert!((mp.mass - v).abs() < 1e-4);
        assert!((mp.inertia.x - 0.4 * v).abs() < 1e-4);
    }

    #[test]
    fn capsule_and_cylinder_inertia_positive() {
        let cap = Shape::Capsule {
            half_height: 0.5,
            radius: 0.3,
        };
        let mp = cap.mass_properties(1000.0);
        assert!(mp.mass > 0.0);
        assert!(mp.inertia.min_element() > 0.0);
        assert!(mp.inertia.y < mp.inertia.x, "轴向惯量应小于横向");

        let cyl = Shape::Cylinder {
            half_height: 0.5,
            radius: 0.3,
        };
        let mc = cyl.mass_properties(1000.0);
        assert!(mc.mass > 0.0);
        assert!(mc.inertia.y < mc.inertia.x);
    }

    #[test]
    fn support_functions() {
        let b = Shape::Box {
            half_extents: vec3(1.0, 2.0, 3.0),
        };
        assert_eq!(b.support(vec3(1.0, 1.0, 1.0)), vec3(1.0, 2.0, 3.0));
        assert_eq!(b.support(vec3(-1.0, -1.0, -1.0)), vec3(-1.0, -2.0, -3.0));

        let c = Shape::Capsule {
            half_height: 1.0,
            radius: 0.5,
        };
        let s = c.support(Vec3::Y);
        assert!((s.y - 1.5).abs() < 1e-5);
        let s2 = c.support(Vec3::NEG_Y);
        assert!((s2.y + 1.5).abs() < 1e-5);

        let cy = Shape::Cylinder {
            half_height: 1.0,
            radius: 0.5,
        };
        let s3 = cy.support(Vec3::X);
        assert!((s3.x - 0.5).abs() < 1e-5, "径向支持点 {s3:?}");
        assert!((s3.y.abs() - 1.0).abs() < 1e-5, "端盖上的点 {s3:?}");
    }

    #[test]
    fn hull_mass_properties_cube() {
        let mut pts = Vec::new();
        for &x in &[-1.0f32, 1.0] {
            for &y in &[-1.0f32, 1.0] {
                for &z in &[-1.0f32, 1.0] {
                    pts.push(vec3(x, y, z));
                }
            }
        }
        let s = Shape::ConvexHull { points: pts };
        let mp = s.mass_properties(1.0);
        // 边长 2 的立方体：体积 8，I = m/3*(1²+1²) = 16/3 ≈ 5.333
        assert!((mp.mass - 8.0).abs() < 0.6, "质量 {}", mp.mass);
        assert!(
            (mp.inertia.x - 16.0 / 3.0).abs() < 0.5,
            "惯量 {}",
            mp.inertia.x
        );
        assert!(mp.center.length() < 0.05);
    }

    #[test]
    fn static_shapes_have_no_mass() {
        let p = Shape::Plane { normal: Vec3::Y };
        assert!(p.is_static_only());
        assert_eq!(p.mass_properties(1000.0).mass, 0.0);
        assert!(p.is_plane());
        let m = Shape::TriMesh {
            vertices: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
            indices: vec![0, 1, 2],
        };
        assert!(m.is_static_only());
        assert_eq!(m.mass_properties(1000.0).mass, 0.0);
        assert!(m.local_aabb().is_valid());
    }
}
