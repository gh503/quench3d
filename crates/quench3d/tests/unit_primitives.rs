//! 渲染用的「单位图元」与物理形状必须描述**同一个几何**。
//!
//! 这是最容易静默出错的地方：网格中心与形状原点差半个高度，画面上看起来"差不多"，
//! 但物理接触、旋转轴、`parts[].position` 全都会偏。以前没有测试盯着这件事，
//! 第 61 轮补圆锥时把这条对账补上（纯 CPU，不需要显卡）。

use quench3d::prelude::*;

/// 网格包围盒与物理形状的本地 AABB 逐轴相等（容差 1e-4）。
fn assert_matches_shape(mesh: &MeshData, shape: &Shape, what: &str) {
    let aabb = mesh.aabb();
    let expected = shape.local_aabb();
    assert!(
        (aabb.min - expected.min).length() < 1e-4,
        "{what}: 网格 min {:?} vs 物理 min {:?}",
        aabb.min,
        expected.min
    );
    assert!(
        (aabb.max - expected.max).length() < 1e-4,
        "{what}: 网格 max {:?} vs 物理 max {:?}",
        aabb.max,
        expected.max
    );
}

#[test]
fn unit_cone_mesh_matches_the_physics_cone() {
    let mesh = quench3d::scene::unit_cone_mesh();
    let shape = Shape::Cone {
        half_height: 0.5,
        radius: 0.5,
    };
    assert_matches_shape(&mesh, &shape, "圆锥");
    // 顶点朝 +Y、底面朝 -Y：最高的顶点只有一个，所以 AABB 顶面中心处应有顶点，
    // 而底面是一整圈（顶面中心之外的位置不该有顶点顶到 0.5）。
    let top = mesh
        .vertices
        .iter()
        .filter(|v| (v.position[1] - 0.5).abs() < 1e-4)
        .count();
    assert_eq!(top, 1, "顶点应该只有一个（实际 {top} 个顶点在 y=+0.5）");
    let bottom = mesh
        .vertices
        .iter()
        .filter(|v| (v.position[1] + 0.5).abs() < 1e-4)
        .count();
    assert!(bottom >= 3, "底面应有一圈顶点（实际 {bottom}）");
    // 底面：一圈半径 0.5 的底圈 + 一个圆心（扇面中心），都在 y=-0.5 上
    let mut ring = 0;
    for v in mesh
        .vertices
        .iter()
        .filter(|v| (v.position[1] + 0.5).abs() < 1e-4)
    {
        let r = (v.position[0] * v.position[0] + v.position[2] * v.position[2]).sqrt();
        assert!(r <= 0.5 + 1e-4, "底面顶点不该超出半径 0.5（{r}）");
        if (r - 0.5).abs() < 1e-4 {
            ring += 1;
        }
    }
    assert!(ring >= 3, "底面圆周上的顶点太少（{ring} 个）");
}

/// 其它单位图元也一起对账（它们早就存在，但同样没有测试盯着"网格 == 形状"）。
#[test]
fn other_unit_primitives_match_their_shapes() {
    assert_matches_shape(
        &MeshData::cube(1.0),
        &Shape::Box {
            half_extents: Vec3::splat(0.5),
        },
        "立方体",
    );
    assert_matches_shape(
        &MeshData::sphere(0.5, 32, 20),
        &Shape::Sphere { radius: 0.5 },
        "球",
    );
    assert_matches_shape(
        &MeshData::cylinder(0.5, 0.5, 28),
        &Shape::Cylinder {
            half_height: 0.5,
            radius: 0.5,
        },
        "圆柱",
    );
    assert_matches_shape(
        &MeshData::capsule(0.5, 0.5, 28, 20),
        &Shape::Capsule {
            half_height: 0.5,
            radius: 0.5,
        },
        "胶囊",
    );
}
