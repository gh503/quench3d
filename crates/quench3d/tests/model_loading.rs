//! 模型加载：**从内存字节**加载（Web 端唯一的正路），以及报错的质量。
//!
//! 这里手搓一个最小 GLB（一个三角形）而不是塞二进制固件进仓库：
//! 格式的每个字节都在测试里看得见，改了格式也知道哪里断了。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use quench3d::assets::{load_model, load_model_from_bytes, AssetError, Interpolation};
use quench3d::math::{Mat4, Transform, Vec3};
use quench3d::render::context::with_gpu_tests;
use quench3d::EngineConfig;

/// 手搓一个 GLB：12 字节头 + JSON 块 + BIN 块。
fn glb_from_parts(json: &str, mut bin: Vec<u8>) -> Vec<u8> {
    let mut json_bytes = json.replace('\n', "").into_bytes();
    while !json_bytes.len().is_multiple_of(4) {
        json_bytes.push(b' '); // JSON 块用空格补齐
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0); // BIN 块用 0 补齐
    }
    let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
    let mut glb = Vec::with_capacity(total);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes()); // 版本
    glb.extend_from_slice(&(total as u32).to_le_bytes());
    glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes()); // "JSON"
    glb.extend_from_slice(&json_bytes);
    glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x004E_4942u32.to_le_bytes()); // "BIN\0"
    glb.extend_from_slice(&bin);
    glb
}

/// 手搓一个最小 GLB：12 字节头 + JSON 块 + BIN 块（单个三角形）。
fn minimal_glb() -> Vec<u8> {
    // 3 个 vec3 位置 = 36 字节
    let mut bin = Vec::new();
    for f in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    let json = r#"{"asset":{"version":"2.0"},"scene":0,"scenes":[{"nodes":[0]}],
"nodes":[{"mesh":0}],"meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
"accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3",
"min":[0.0,0.0,0.0],"max":[1.0,1.0,0.0]}],
"bufferViews":[{"buffer":0,"byteOffset":0,"byteLength":36,"target":34962}],
"buffers":[{"byteLength":36}]}"#;
    glb_from_parts(json, bin)
}

#[test]
fn glb_bytes_load_without_touching_the_filesystem() {
    let glb = minimal_glb();
    let model = load_model_from_bytes("triangle.glb", &glb).expect("GLB 字节流应当能解析");
    assert_eq!(model.name, "triangle");
    assert_eq!(model.meshes.len(), 1, "一个网格");
    let mesh = &model.meshes[0];
    assert_eq!(mesh.data.vertices.len(), 3);
    assert_eq!(mesh.data.indices, vec![0, 1, 2], "没写索引时按顺序生成");
    // 位置真的读出来了（不是全 0）
    let p1 = mesh.data.vertices[1].position;
    assert!((p1[0] - 1.0).abs() < 1e-6, "{p1:?}");
    // 没有法线数据 → 应当自动平滑法线，而不是留 0 让光照全黑
    let n = mesh.data.vertices[0].normal;
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    assert!((len - 1.0).abs() < 1e-3, "缺省法线必须补成单位向量：{n:?}");
    // 没有材质 → 补一个缺省材质，别让上传时下标落空
    assert_eq!(model.materials.len(), 1);
    assert_eq!(mesh.material, None);
}

/// 手搓一个「1 关节 + 1 蒙皮网格 + 1 秒平移动画」的 GLB。
///
/// 关节 `node 0` 从 y=0 平移到 y=2，网格顶点在绑定姿态 y=0；如果蒙皮真的生效，
/// `skinned_mesh` 在 t=1 时顶点 y 必须整体 +2，而 `mesh_matrices` 必须仍是单位矩阵
/// （否则物体变换会和蒙皮叠加，动两遍）。
fn skinned_glb() -> Vec<u8> {
    let mut bin = Vec::new();
    // POSITION：3 个 vec3
    for f in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    // JOINTS_0：3 × [u8;4]，全部绑到关节 0
    for _ in 0..3 {
        bin.extend_from_slice(&[0, 0, 0, 0]);
    }
    // WEIGHTS_0：3 × vec4，全部权重 1
    for _ in 0..3 {
        for f in [1.0f32, 0.0, 0.0, 0.0] {
            bin.extend_from_slice(&f.to_le_bytes());
        }
    }
    // 逆绑定矩阵：单位矩阵（列主序）
    for f in [
        1.0f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    // 动画输入：t=0,1
    for f in [0.0f32, 1.0] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    // 动画输出：y=0 → y=2
    for f in [0.0f32, 0.0, 0.0, 0.0, 2.0, 0.0] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    let json = r#"{"asset":{"version":"2.0"},"scene":0,
"scenes":[{"nodes":[1]}],
"nodes":[{"name":"joint"},{"name":"skin","mesh":0,"skin":0}],
"meshes":[{"name":"tri","primitives":[{"attributes":{"POSITION":0,"JOINTS_0":1,"WEIGHTS_0":2}}]}],
"skins":[{"joints":[0],"inverseBindMatrices":3}],
"accessors":[
 {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0.0,0.0,0.0],"max":[1.0,1.0,0.0]},
 {"bufferView":1,"componentType":5121,"count":3,"type":"VEC4"},
 {"bufferView":2,"componentType":5126,"count":3,"type":"VEC4"},
 {"bufferView":3,"componentType":5126,"count":1,"type":"MAT4"},
 {"bufferView":4,"componentType":5126,"count":2,"type":"SCALAR","min":[0.0],"max":[1.0]},
 {"bufferView":5,"componentType":5126,"count":2,"type":"VEC3"}],
"bufferViews":[
 {"buffer":0,"byteOffset":0,"byteLength":36,"target":34962},
 {"buffer":0,"byteOffset":36,"byteLength":12,"target":34962},
 {"buffer":0,"byteOffset":48,"byteLength":48,"target":34962},
 {"buffer":0,"byteOffset":96,"byteLength":64},
 {"buffer":0,"byteOffset":160,"byteLength":8},
 {"buffer":0,"byteOffset":168,"byteLength":24}],
"buffers":[{"byteLength":192}],
"animations":[
 {"name":"lift","samplers":[{"input":4,"output":5,"interpolation":"LINEAR"}],
  "channels":[{"sampler":0,"target":{"node":0,"path":"translation"}}]},
 {"name":"lift2","samplers":[{"input":4,"output":5,"interpolation":"LINEAR"}],
  "channels":[{"sampler":0,"target":{"node":0,"path":"translation"}}]}]}"#;
    glb_from_parts(json, bin)
}

/// 手搓一个 CUBICSPLINE 平移动画：x 从 0 到 1，出切线 2/s、入切线 0。
///
/// 中点 Hermite 值应是 .75（线性只会是 .5），用来钉住解析器真的读了
/// `[入切线, 值, 出切线]` 三块，而不是只取中间的值。
fn cubic_glb() -> Vec<u8> {
    let mut bin = Vec::new();
    for f in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    for f in [0.0f32, 1.0] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    // [入切线, 值, 出切线] × 2
    for f in [
        0.0f32, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, // key0
        0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, // key1
    ] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    let json = r#"{"asset":{"version":"2.0"},"scene":0,
"scenes":[{"nodes":[0]}],"nodes":[{"mesh":0}],
"meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
"accessors":[
 {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0.0,0.0,0.0],"max":[1.0,1.0,0.0]},
 {"bufferView":1,"componentType":5126,"count":2,"type":"SCALAR","min":[0.0],"max":[1.0]},
 {"bufferView":2,"componentType":5126,"count":6,"type":"VEC3"}],
"bufferViews":[
 {"buffer":0,"byteOffset":0,"byteLength":36,"target":34962},
 {"buffer":0,"byteOffset":36,"byteLength":8},
 {"buffer":0,"byteOffset":44,"byteLength":72}],
"buffers":[{"byteLength":116}],
"animations":[{"name":"cubic","samplers":[
 {"input":1,"output":2,"interpolation":"CUBICSPLINE"}],
 "channels":[{"sampler":0,"target":{"node":0,"path":"translation"}}]}]}"#;
    glb_from_parts(json, bin)
}

/// 手搓一个带 1 个 morph target 的 GLB：三角形所有顶点 y 方向 +0.5。
fn morph_glb() -> Vec<u8> {
    let mut bin = Vec::new();
    // 位置
    for f in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    // 法线
    for _ in 0..3 {
        for f in [0.0f32, 0.0, 1.0] {
            bin.extend_from_slice(&f.to_le_bytes());
        }
    }
    // 索引 u16 + 2 字节对齐
    for i in [0u16, 1, 2] {
        bin.extend_from_slice(&i.to_le_bytes());
    }
    bin.extend_from_slice(&[0, 0]);
    // morph 位置位移
    for _ in 0..3 {
        for f in [0.0f32, 0.5, 0.0] {
            bin.extend_from_slice(&f.to_le_bytes());
        }
    }
    // 动画时间 0,1
    for f in [0.0f32, 1.0] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    // morph 权重 0 → 1
    for f in [0.0f32, 1.0] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    let json = r#"{"asset":{"version":"2.0"},"scene":0,
"scenes":[{"nodes":[0]}],
"nodes":[{"name":"tri","mesh":0}],
"meshes":[{"name":"morph","weights":[0.0],"primitives":[{"attributes":{"POSITION":0,"NORMAL":1},"indices":2,"targets":[{"POSITION":3}]}]}],
"accessors":[
 {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0.0,0.0,0.0],"max":[1.0,1.0,0.0]},
 {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"},
 {"bufferView":2,"componentType":5123,"count":3,"type":"SCALAR"},
 {"bufferView":3,"componentType":5126,"count":3,"type":"VEC3"},
 {"bufferView":4,"componentType":5126,"count":2,"type":"SCALAR","min":[0.0],"max":[1.0]},
 {"bufferView":5,"componentType":5126,"count":2,"type":"SCALAR"}],
"bufferViews":[
 {"buffer":0,"byteOffset":0,"byteLength":36,"target":34962},
 {"buffer":0,"byteOffset":36,"byteLength":36,"target":34962},
 {"buffer":0,"byteOffset":72,"byteLength":6,"target":34963},
 {"buffer":0,"byteOffset":80,"byteLength":36,"target":34962},
 {"buffer":0,"byteOffset":116,"byteLength":8},
 {"buffer":0,"byteOffset":124,"byteLength":8}],
"buffers":[{"byteLength":132}],
"animations":[{"name":"pulse","samplers":[{"input":4,"output":5,"interpolation":"LINEAR"}],
 "channels":[{"sampler":0,"target":{"node":0,"path":"weights"}}]}]}"#;
    glb_from_parts(json, bin)
}

/// 手搓一个「1 关节 + morph target + 同一 clip 里平移与权重」的 GLB。
///
/// 用来钉住 **morph + skin 的混合路径**：绑定姿态先加 morph 位移（y+0.5），
/// 再乘关节矩阵（y+2）；GPU 蒙皮只应乘一次关节，不能把 CPU 已蒙皮的结果再乘一次。
fn skinned_morph_glb() -> Vec<u8> {
    let mut bin = Vec::new();
    // POSITION @0
    for f in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    // NORMAL @36
    for _ in 0..3 {
        for f in [0.0f32, 0.0, 1.0] {
            bin.extend_from_slice(&f.to_le_bytes());
        }
    }
    // JOINTS_0 @72
    for _ in 0..3 {
        bin.extend_from_slice(&[0, 0, 0, 0]);
    }
    // WEIGHTS_0 @84
    for _ in 0..3 {
        for f in [1.0f32, 0.0, 0.0, 0.0] {
            bin.extend_from_slice(&f.to_le_bytes());
        }
    }
    // indices @132（u16 + 2 字节对齐）
    for i in [0u16, 1, 2] {
        bin.extend_from_slice(&i.to_le_bytes());
    }
    bin.extend_from_slice(&[0, 0]);
    // inverse bind @140
    for f in [
        1.0f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    // morph 位移 @204：所有顶点 y +0.5
    for _ in 0..3 {
        for f in [0.0f32, 0.5, 0.0] {
            bin.extend_from_slice(&f.to_le_bytes());
        }
    }
    // 动画时间 @240
    for f in [0.0f32, 1.0] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    // 关节平移 @248：y 0 → 2
    for f in [0.0f32, 0.0, 0.0, 0.0, 2.0, 0.0] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    // morph 权重 @272：0 → 1
    for f in [0.0f32, 1.0] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    let json = r#"{"asset":{"version":"2.0"},"scene":0,
"scenes":[{"nodes":[1]}],
"nodes":[{"name":"joint"},{"name":"skin","mesh":0,"skin":0}],
"meshes":[{"name":"tri","weights":[0.0],"primitives":[{"attributes":{"POSITION":0,"NORMAL":1,"JOINTS_0":2,"WEIGHTS_0":3},"indices":4,"targets":[{"POSITION":5}]}]}],
"skins":[{"joints":[0],"inverseBindMatrices":6}],
"accessors":[
 {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0.0,0.0,0.0],"max":[1.0,1.0,0.0]},
 {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"},
 {"bufferView":2,"componentType":5121,"count":3,"type":"VEC4"},
 {"bufferView":3,"componentType":5126,"count":3,"type":"VEC4"},
 {"bufferView":4,"componentType":5123,"count":3,"type":"SCALAR"},
 {"bufferView":6,"componentType":5126,"count":3,"type":"VEC3","min":[0.0,0.0,0.0],"max":[1.0,0.5,0.0]},
 {"bufferView":5,"componentType":5126,"count":1,"type":"MAT4"},
 {"bufferView":7,"componentType":5126,"count":2,"type":"SCALAR","min":[0.0],"max":[1.0]},
 {"bufferView":8,"componentType":5126,"count":2,"type":"VEC3"},
 {"bufferView":9,"componentType":5126,"count":2,"type":"SCALAR"}],
"bufferViews":[
 {"buffer":0,"byteOffset":0,"byteLength":36,"target":34962},
 {"buffer":0,"byteOffset":36,"byteLength":36,"target":34962},
 {"buffer":0,"byteOffset":72,"byteLength":12,"target":34962},
 {"buffer":0,"byteOffset":84,"byteLength":48,"target":34962},
 {"buffer":0,"byteOffset":132,"byteLength":6,"target":34963},
 {"buffer":0,"byteOffset":140,"byteLength":64},
 {"buffer":0,"byteOffset":204,"byteLength":36,"target":34962},
 {"buffer":0,"byteOffset":240,"byteLength":8},
 {"buffer":0,"byteOffset":248,"byteLength":24},
 {"buffer":0,"byteOffset":272,"byteLength":8}],
"buffers":[{"byteLength":280}],
"animations":[{"name":"combo","samplers":[
 {"input":7,"output":8,"interpolation":"LINEAR"},
 {"input":7,"output":9,"interpolation":"LINEAR"}],
 "channels":[
 {"sampler":0,"target":{"node":0,"path":"translation"}},
 {"sampler":1,"target":{"node":1,"path":"weights"}}]}]}"#;
    glb_from_parts(json, bin)
}

/// 与 `morph_glb` 相同的三角形，但权重轨道是 CUBICSPLINE：
/// 关键帧 0 = (入 0, 值 0, 出 2)，关键帧 1 = (入 0, 值 1, 出 0)。
fn morph_cubic_glb() -> Vec<u8> {
    let mut bin = Vec::new();
    for f in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    for _ in 0..3 {
        for f in [0.0f32, 0.0, 1.0] {
            bin.extend_from_slice(&f.to_le_bytes());
        }
    }
    for i in [0u16, 1, 2] {
        bin.extend_from_slice(&i.to_le_bytes());
    }
    bin.extend_from_slice(&[0, 0]);
    for _ in 0..3 {
        for f in [0.0f32, 0.5, 0.0] {
            bin.extend_from_slice(&f.to_le_bytes());
        }
    }
    for f in [0.0f32, 1.0] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    // [入切线, 值, 出切线] × 2 个关键帧（每个 target 1 个权重）
    for f in [0.0f32, 0.0, 2.0, 0.0, 1.0, 0.0] {
        bin.extend_from_slice(&f.to_le_bytes());
    }
    let json = r#"{"asset":{"version":"2.0"},"scene":0,
"scenes":[{"nodes":[0]}],
"nodes":[{"name":"tri","mesh":0}],
"meshes":[{"name":"morph","weights":[0.0],"primitives":[{"attributes":{"POSITION":0,"NORMAL":1},"indices":2,"targets":[{"POSITION":3}]}]}],
"accessors":[
 {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0.0,0.0,0.0],"max":[1.0,1.0,0.0]},
 {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"},
 {"bufferView":2,"componentType":5123,"count":3,"type":"SCALAR"},
 {"bufferView":3,"componentType":5126,"count":3,"type":"VEC3"},
 {"bufferView":4,"componentType":5126,"count":2,"type":"SCALAR","min":[0.0],"max":[1.0]},
 {"bufferView":5,"componentType":5126,"count":6,"type":"SCALAR"}],
"bufferViews":[
 {"buffer":0,"byteOffset":0,"byteLength":36,"target":34962},
 {"buffer":0,"byteOffset":36,"byteLength":36,"target":34962},
 {"buffer":0,"byteOffset":72,"byteLength":6,"target":34963},
 {"buffer":0,"byteOffset":80,"byteLength":36,"target":34962},
 {"buffer":0,"byteOffset":116,"byteLength":8},
 {"buffer":0,"byteOffset":124,"byteLength":24}],
"buffers":[{"byteLength":148}],
"animations":[{"name":"pulse","samplers":[{"input":4,"output":5,"interpolation":"CUBICSPLINE"}],
 "channels":[{"sampler":0,"target":{"node":0,"path":"weights"}}]}]}"#;
    glb_from_parts(json, bin)
}

#[test]
fn cubic_morph_animation_uses_tangents() {
    let model = load_model_from_bytes("cubic-morph.glb", &morph_cubic_glb()).expect("CUBICSPLINE");
    let clip = model.animation("pulse").expect("pulse");
    let track = &clip.morph_tracks[0];
    assert_eq!(track.interpolation, Interpolation::CubicSpline);
    assert_eq!(track.values, vec![vec![0.0], vec![1.0]]);
    assert_eq!(track.in_tangents, vec![vec![0.0], vec![0.0]]);
    assert_eq!(track.out_tangents, vec![vec![2.0], vec![0.0]]);
    let mid = model.morph_weights_at("pulse", 0.5).expect("t=.5");
    assert!(
        (mid[0].1[0] - 0.75).abs() < 1e-6,
        "Hermite 中点应为 .75，实际 {:?}",
        mid[0].1
    );
    let base = model.morphed_mesh(0, None).expect("base");
    let deformed = model.morphed_mesh(0, Some(&mid[0].1)).expect("mesh");
    assert!(
        (deformed.vertices[0].position[1] - base.vertices[0].position[1] - 0.375).abs() < 1e-6,
        "权重 .75 × 位移 .5 = .375"
    );
}

#[test]
fn morph_targets_displace_vertices_by_weight() {
    let model = load_model_from_bytes("morph.glb", &morph_glb()).expect("morph GLB 应当能解析");
    assert!(model.has_morph_targets(), "应当识别出 morph target");
    assert_eq!(model.meshes[0].morph_targets.len(), 1);
    assert_eq!(model.meshes[0].morph_weights, vec![0.0]);
    let base = model.morphed_mesh(0, None).expect("基础网格");
    assert!((base.vertices[0].position[1] - 0.0).abs() < 1e-6);
    let one = model.morphed_mesh(0, Some(&[1.0])).expect("权重 1");
    for (i, v) in one.vertices.iter().enumerate() {
        let want = base.vertices[i].position[1] + 0.5;
        assert!(
            (v.position[1] - want).abs() < 1e-6,
            "顶点 {i} 应当相对基础上移 0.5（期望 {want}）：{:?}",
            v.position
        );
    }
    let half = model.morphed_mesh(0, Some(&[0.5])).expect("权重 0.5");
    assert!((half.vertices[0].position[1] - 0.25).abs() < 1e-6);
}

#[test]
fn morph_animation_interpolates_weights() {
    let model = load_model_from_bytes("morph.glb", &morph_glb()).expect("morph GLB 应当能解析");
    let clip = model.animation("pulse").expect("应当有 pulse 动画");
    assert_eq!(clip.morph_tracks.len(), 1, "应当解析出 1 条 morph 权重轨道");
    assert!(clip.tracks.is_empty(), "只有 morph 轨道，没有 TRS 轨道");
    let start = model.morph_weights_at("pulse", 0.0).expect("t=0");
    assert_eq!(start.len(), 1);
    assert_eq!(start[0].0, 0, "权重轨道在节点 0 上");
    assert!((start[0].1[0] - 0.0).abs() < 1e-6);
    let mid = model.morph_weights_at("pulse", 0.5).expect("t=0.5");
    assert!((mid[0].1[0] - 0.5).abs() < 1e-6, "{mid:?}");
    let end = model.morph_weights_at("pulse", 1.0).expect("t=1");
    assert!((end[0].1[0] - 1.0).abs() < 1e-6);
    // 采样出的权重直接喂给 morph 网格：顶点相对基础上移 0.25。
    let base = model.morphed_mesh(0, None).expect("基础网格");
    let deformed = model.morphed_mesh(0, Some(&mid[0].1)).expect("t=0.5 网格");
    assert!((deformed.vertices[0].position[1] - base.vertices[0].position[1] - 0.25).abs() < 1e-6);
}

/// 双 clip 的 morph 权重按 blend 插值：主 clip t=.5 是 .5，第二 clip 恒 1，
/// blend=.5 时权重 .75（形变位移 .375）。
#[test]
fn blended_morph_weights_interpolate_both_clips() {
    let mut model = load_model_from_bytes("morph.glb", &morph_glb()).expect("morph GLB 应当能解析");
    let mut second = model.animations[0].clone();
    second.name = "pulse2".into();
    for track in &mut second.morph_tracks {
        for value in &mut track.values {
            value[0] = 1.0;
        }
    }
    model.animations.push(second);

    let primary_only = model
        .blended_morph_weights_at("pulse", "pulse2", 0.0, 0.5, 0.5)
        .expect("主 clip 有轨道");
    assert!(
        (primary_only[0].1[0] - 0.5).abs() < 1e-6,
        "{primary_only:?}"
    );
    let second_only = model
        .blended_morph_weights_at("pulse", "pulse2", 1.0, 0.5, 0.5)
        .expect("第二 clip 有轨道");
    assert!((second_only[0].1[0] - 1.0).abs() < 1e-6, "{second_only:?}");
    let half = model
        .blended_morph_weights_at("pulse", "pulse2", 0.5, 0.5, 0.5)
        .expect("两条 clip 都有轨道");
    assert!((half[0].1[0] - 0.75).abs() < 1e-6, "{half:?}");

    let base = model.morphed_mesh(0, None).expect("基础网格");
    let deformed = model.morphed_mesh(0, Some(&half[0].1)).expect("混合网格");
    assert!(
        (deformed.vertices[0].position[1] - base.vertices[0].position[1] - 0.375).abs() < 1e-6,
        "权重 .75 × 位移 .5 = .375"
    );
}

#[test]
fn skinned_glb_deforms_vertices_from_joint_animation() {
    let model = load_model_from_bytes("skinned.glb", &skinned_glb()).expect("蒙皮 GLB 应当能解析");
    assert!(model.has_skinning(), "应当识别出蒙皮网格");
    let mesh = &model.meshes[0];
    assert_eq!(mesh.skin, Some(0), "片段应当绑定到 skin 0");
    assert_eq!(mesh.joints.len(), 3, "每个顶点都要有 JOINTS_0");
    assert_eq!(mesh.weights.len(), 3, "每个顶点都要有 WEIGHTS_0");
    assert_eq!(model.skins[0].joints, vec![0]);
    assert_eq!(model.skins[0].inverse_bind.len(), 1);

    let player = model.player("lift").expect("动画名应当能查到");
    let worlds = player.node_worlds(&model).expect("应当能采样节点世界矩阵");
    // 蒙皮网格不再走刚性矩阵，否则会和顶点蒙皮叠加。
    assert_eq!(player.mesh_matrices(&model), vec![Mat4::IDENTITY]);
    assert_eq!(
        model.mesh_matrix(0, &worlds),
        Mat4::IDENTITY,
        "直接调用 mesh_matrix 也必须跳过蒙皮片段"
    );

    let bind = model
        .skinned_mesh(0, &worlds)
        .expect("蒙皮网格应当返回变形数据");
    assert!((bind.vertices[1].position[0] - 1.0).abs() < 1e-5);
    assert!((bind.vertices[1].position[1] - 0.0).abs() < 1e-5);

    let mut at_one = player.clone();
    at_one.time = 1.0;
    let worlds = at_one.node_worlds(&model).expect("t=1 也要能采样");
    let posed = model.skinned_mesh(0, &worlds).expect("t=1 应当有变形数据");
    for (i, v) in posed.vertices.iter().enumerate() {
        let bind_y = bind.vertices[i].position[1];
        assert!(
            (v.position[1] - (bind_y + 2.0)).abs() < 1e-5,
            "顶点 {i} 应当被关节抬高 2：{:?}",
            v.position
        );
    }
    // 法线不跟随平移变化，必须仍是单位向量（不能变成 NaN/0）。
    for v in &posed.vertices {
        let n = Vec3::from_array(v.normal);
        assert!(
            (n.length() - 1.0).abs() < 1e-4,
            "法线必须归一化：{:?}",
            v.normal
        );
    }
}

/// 蒙皮不是只改 CPU 数据：它必须把新顶点写回 GPU，画面真的动。
///
/// 同一条 `lift` 动画，t=0 与 t=0.25 各渲染一张 64×64；如果只改了 `SceneObject`
/// 的变换、或 `update_mesh_vertices` 没接上，两张图会逐字节相同（这条会红）。
/// 两条 clip 的局部 TRS 混合：primary y=1、secondary y=2，w=0.5 → y=1.5。
///
/// 用一个派生 clip（同一条动画的平移翻倍）而不是再手搓 GLB：这里要验的是采样/混合
/// 数学，不是解析；解析已由 `skinned_glb_deforms_vertices_from_joint_animation` 钉住。
#[test]
fn blended_clips_interpolate_skinned_pose() {
    let mut model = load_model_from_bytes("skinned.glb", &skinned_glb()).expect("蒙皮 GLB");
    let mut second = model.animations[0].clone();
    second.name = "lift4".into();
    for track in &mut second.tracks {
        if track.property == quench3d::assets::AnimatedProperty::Translation {
            for value in &mut track.values {
                value[1] *= 2.0;
            }
        }
    }
    model.animations.push(second);

    let mut primary = model.player("lift").expect("primary");
    primary.time = 0.5;
    let mut secondary = model.player("lift4").expect("secondary");
    secondary.time = 0.5;

    let at = |weight: f32| {
        let worlds = model
            .blended_node_worlds(&primary, &secondary, weight)
            .expect("两条 clip 都存在");
        model.skinned_mesh(0, &worlds).expect("蒙皮片段")
    };
    let half = at(0.5);
    let bind = &model.meshes[0].data.vertices;
    for (i, vertex) in half.vertices.iter().enumerate() {
        let dy = vertex.position[1] - bind[i].position[1];
        assert!(
            (dy - 1.5).abs() < 1e-5,
            "w=0.5 应当在 y=1 与 y=2 中间（顶点 {i} 的位移 {dy}）：{:?}",
            vertex.position
        );
    }
    let only_primary = at(0.0);
    assert!((only_primary.vertices[0].position[1] - 1.0).abs() < 1e-5);
    let only_secondary = at(1.0);
    assert!((only_secondary.vertices[0].position[1] - 2.0).abs() < 1e-5);
}

#[test]
fn skinned_mesh_changes_rendered_pixels() {
    let rendered = with_gpu_tests(
        "蒙皮顶点写回 GPU（真的创建 wgpu 上下文）",
        || {
            let model = Arc::new(
                load_model_from_bytes("skinned.glb", &skinned_glb()).expect("蒙皮 GLB 应当能解析"),
            );
            let config = EngineConfig {
                width: 64,
                height: 64,
                msaa_samples: 1,
                ..Default::default()
            };
            let render_at = |time: f32| {
                quench3d::render_offscreen(64, 64, &config, |engine| {
                    let spawned = model.spawn(
                        &mut engine.renderer,
                        &engine.gpu,
                        &mut engine.scene,
                        Transform::default(),
                    );
                    let mut player = model.player("lift").expect("动画存在");
                    player.time = time;
                    engine.scene.play_model_animation(
                        Arc::clone(&model),
                        spawned.objects,
                        spawned.base,
                        player,
                    );
                    engine.scene.lighting.sun = None;
                    engine.scene.lighting.ambient_color = Vec3::ONE;
                    engine.scene.lighting.ambient_intensity = 2.0;
                    engine.orbit.focus = Vec3::new(0.5, 0.75, 0.0);
                    engine.orbit.distance = 3.0;
                    engine.orbit.yaw = 0.0;
                    engine.orbit.pitch = 0.0;
                    engine
                        .scene
                        .refresh_animations_with_renderer(&mut engine.renderer, &engine.gpu);
                })
            };
            let a = render_at(0.0);
            let b = render_at(0.25);
            (a, b)
        },
    );
    let Some((a, b)) = rendered else {
        return;
    };
    // CI 的 ubuntu runner 可能没有 Vulkan/GL 适配器；这时显式跳过，而不是把
    // "找不到 GPU" 当像素回归失败。其它错误仍然 panic（那才是真的渲染坏了）。
    let (a, b) = match (a, b) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            if e.contains("GPU 适配器") || e.contains("GPU adapter") {
                eprintln!("跳过：没有可用的 GPU（{e}）");
                return;
            }
            panic!("蒙皮渲染失败：{e}");
        }
    };
    let changed = a
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.rgba.as_chunks::<4>().0)
        .filter(|(x, y)| x != y)
        .count();
    assert!(
        changed > 20,
        "蒙皮后画面至少要有几十个像素变化，实际 {changed}（顶点没有写回 GPU？）"
    );
}

#[test]
fn gpu_skinning_fills_the_joint_palette() {
    let palette = std::sync::Arc::new(AtomicUsize::new(0));
    let probe = std::sync::Arc::clone(&palette);
    let rendered = with_gpu_tests(
        "GPU 蒙皮调色板（真的创建 wgpu 上下文）",
        move || {
            let model = Arc::new(
                load_model_from_bytes("skinned.glb", &skinned_glb()).expect("蒙皮 GLB 应当能解析"),
            );
            let config = EngineConfig {
                width: 32,
                height: 32,
                msaa_samples: 1,
                ..Default::default()
            };
            quench3d::render_offscreen(32, 32, &config, move |engine| {
                let spawned = model.spawn(
                    &mut engine.renderer,
                    &engine.gpu,
                    &mut engine.scene,
                    Transform::default(),
                );
                let mut player = model.player("lift").expect("动画存在");
                player.time = 0.5;
                engine.scene.play_model_animation(
                    Arc::clone(&model),
                    spawned.objects,
                    spawned.base,
                    player,
                );
                engine
                    .scene
                    .refresh_animations_with_renderer(&mut engine.renderer, &engine.gpu);
                engine.scene.submit(&mut engine.renderer);
                probe.store(engine.renderer.skin_palette_len(), Ordering::SeqCst);
            })
        },
    );
    let Some(result) = rendered else {
        return;
    };
    match result {
        Ok(_) => {}
        Err(e) if e.contains("GPU 适配器") || e.contains("GPU adapter") => return,
        Err(e) => panic!("蒙皮渲染失败：{e}"),
    }
    assert!(
        palette.load(Ordering::SeqCst) > 0,
        "纯蒙皮片段应当在提交时把关节矩阵放进 GPU 调色板"
    );
}

#[test]
fn gpu_skinning_matches_cpu_skinning_pixels() {
    let rendered = with_gpu_tests(
        "GPU/CPU 蒙皮像素对照（真的创建 wgpu 上下文）",
        || {
            let model = Arc::new(
                load_model_from_bytes("skinned.glb", &skinned_glb()).expect("蒙皮 GLB 应当能解析"),
            );
            let config = EngineConfig {
                width: 64,
                height: 64,
                msaa_samples: 1,
                ..Default::default()
            };
            let render_at = |time: f32, gpu: bool| {
                let model = Arc::clone(&model);
                quench3d::render_offscreen(64, 64, &config, move |engine| {
                    let spawned = model.spawn(
                        &mut engine.renderer,
                        &engine.gpu,
                        &mut engine.scene,
                        Transform::default(),
                    );
                    let mut player = model.player("lift").expect("动画存在");
                    player.time = time;
                    engine.scene.gpu_skinning = gpu;
                    engine.scene.play_model_animation(
                        Arc::clone(&model),
                        spawned.objects,
                        spawned.base,
                        player,
                    );
                    engine.scene.lighting.sun = None;
                    engine.scene.lighting.ambient_color = Vec3::ONE;
                    engine.scene.lighting.ambient_intensity = 2.0;
                    engine.orbit.focus = Vec3::new(0.5, 0.75, 0.0);
                    engine.orbit.distance = 3.0;
                    engine.orbit.yaw = 0.0;
                    engine.orbit.pitch = 0.0;
                    engine
                        .scene
                        .refresh_animations_with_renderer(&mut engine.renderer, &engine.gpu);
                })
            };
            (render_at(0.25, false), render_at(0.25, true))
        },
    );
    let Some((cpu, gpu)) = rendered else {
        return;
    };
    let (cpu, gpu) = match (cpu, gpu) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            if e.contains("GPU 适配器") || e.contains("GPU adapter") {
                return;
            }
            panic!("蒙皮对照渲染失败：{e}");
        }
    };
    let mut differing = 0usize;
    let mut max_diff = 0i32;
    let mut total = 0i64;
    for (a, b) in cpu
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(gpu.rgba.as_chunks::<4>().0)
    {
        let mut pixel_diff = 0i32;
        for k in 0..3 {
            let d = (a[k] as i32 - b[k] as i32).abs();
            total += d as i64;
            pixel_diff = pixel_diff.max(d);
        }
        max_diff = max_diff.max(pixel_diff);
        if pixel_diff > 3 {
            differing += 1;
        }
    }
    let pixels = (cpu.width * cpu.height) as usize;
    let mean = total as f64 / (pixels * 3) as f64;
    eprintln!("GPU/CPU 蒙皮像素：{differing}/{pixels} 差>3，最大差 {max_diff}，平均 {mean:.4}");
    assert!(
        differing * 100 <= pixels,
        "GPU 与 CPU 蒙皮差得太多：{differing}/{pixels}（最大 {max_diff}）"
    );
    assert!(mean < 1.0, "GPU 与 CPU 蒙皮平均像素差 {mean:.4}");
}

#[test]
fn gpu_skinning_with_morph_matches_cpu_pixels_and_uses_palette() {
    let palette = std::sync::Arc::new(AtomicUsize::new(0));
    let probe = std::sync::Arc::clone(&palette);
    let rendered = with_gpu_tests(
        "morph + GPU 蒙皮的 CPU 像素对照（真的创建 wgpu 上下文）",
        move || {
            let model = Arc::new(
                load_model_from_bytes("skinned-morph.glb", &skinned_morph_glb())
                    .expect("morph + skin GLB 应当能解析"),
            );
            let config = EngineConfig {
                width: 128,
                height: 128,
                msaa_samples: 1,
                ..Default::default()
            };
            let render_at = |gpu: bool| {
                let model = Arc::clone(&model);
                let probe = std::sync::Arc::clone(&probe);
                quench3d::render_offscreen(128, 128, &config, move |engine| {
                    let spawned = model.spawn(
                        &mut engine.renderer,
                        &engine.gpu,
                        &mut engine.scene,
                        Transform::default(),
                    );
                    let mut player = model.player("combo").expect("动画存在");
                    player.time = 1.0;
                    engine.scene.gpu_skinning = gpu;
                    engine.scene.play_model_animation(
                        Arc::clone(&model),
                        spawned.objects,
                        spawned.base,
                        player,
                    );
                    engine.scene.lighting.sun = None;
                    engine.scene.lighting.ambient_color = Vec3::ONE;
                    engine.scene.lighting.ambient_intensity = 2.0;
                    engine.orbit.focus = Vec3::new(0.5, 3.0, 0.0);
                    engine.orbit.distance = 3.5;
                    engine.orbit.yaw = 0.0;
                    engine.orbit.pitch = 0.0;
                    engine
                        .scene
                        .refresh_animations_with_renderer(&mut engine.renderer, &engine.gpu);
                    engine.scene.submit(&mut engine.renderer);
                    if gpu {
                        probe.store(engine.renderer.skin_palette_len(), Ordering::SeqCst);
                    }
                })
            };
            let empty = quench3d::render_offscreen(128, 128, &config, move |engine| {
                engine.scene.lighting.sun = None;
                engine.scene.lighting.ambient_color = Vec3::ONE;
                engine.scene.lighting.ambient_intensity = 2.0;
                engine.orbit.focus = Vec3::new(0.5, 3.0, 0.0);
                engine.orbit.distance = 3.5;
                engine.orbit.yaw = 0.0;
                engine.orbit.pitch = 0.0;
            });
            (render_at(false), render_at(true), empty)
        },
    );
    let Some((cpu, gpu, empty)) = rendered else {
        return;
    };
    let empty = match empty {
        Ok(image) => image,
        Err(e) if e.contains("GPU 适配器") || e.contains("GPU adapter") => return,
        Err(e) => panic!("空场景对照渲染失败：{e}"),
    };
    let (cpu, gpu) = match (cpu, gpu) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => {
            if e.contains("GPU 适配器") || e.contains("GPU adapter") {
                return;
            }
            panic!("morph + 蒙皮对照渲染失败：{e}");
        }
    };
    assert!(
        palette.load(Ordering::SeqCst) > 0,
        "morph + 蒙皮片段也应该用 GPU 关节调色板"
    );
    let mut differing = 0usize;
    let mut max_diff = 0i32;
    let mut total = 0i64;
    for (a, b) in cpu
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(gpu.rgba.as_chunks::<4>().0)
    {
        let mut pixel_diff = 0i32;
        for k in 0..3 {
            let d = (a[k] as i32 - b[k] as i32).abs();
            total += d as i64;
            pixel_diff = pixel_diff.max(d);
        }
        max_diff = max_diff.max(pixel_diff);
        if pixel_diff > 3 {
            differing += 1;
        }
    }
    let pixels = (cpu.width * cpu.height) as usize;
    let mean = total as f64 / (pixels * 3) as f64;
    let center = ((cpu.height / 2 * cpu.width + cpu.width / 2) * 4) as usize;
    let covered = cpu
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(empty.rgba.as_chunks::<4>().0)
        .filter(|(a, b)| (0..3).any(|k| (a[k] as i32 - b[k] as i32).abs() > 10))
        .count();
    eprintln!(
        "morph + GPU/CPU 蒙皮像素：{differing}/{pixels} 差>3，最大差 {max_diff}，平均 {mean:.4}，覆盖 {covered}，中心 {:?}",
        &cpu.rgba[center..center + 3]
    );
    assert!(
        covered > 100,
        "morph + 蒙皮三角形必须真的出现在画面里：{covered} 像素"
    );
    assert!(
        differing * 100 <= pixels,
        "morph + GPU 与 CPU 蒙皮差得太多：{differing}/{pixels}（最大 {max_diff}）"
    );
    assert!(mean < 1.0, "morph + GPU/CPU 蒙皮平均像素差 {mean:.4}");
}

#[test]
fn obj_bytes_load_geometry() {
    let obj = b"# \xe4\xb8\x89\xe8\xa7\x92\xe5\xbd\xa2\nv 0 0 0\nv 2 0 0\nv 0 2 0\nf 1 2 3\n";
    let model = load_model_from_bytes("quad.obj", obj).expect("OBJ 字节流应当能解析");
    assert_eq!(model.name, "quad");
    assert_eq!(model.meshes.len(), 1);
    assert_eq!(model.meshes[0].data.indices.len(), 3, "一个三角形");
    assert_eq!(model.vertex_count(), 3);
}

#[test]
fn byte_loading_reports_actionable_errors() {
    // 未知扩展名：要说清楚支持什么
    let err = load_model_from_bytes("chair.ply", b"solid x").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("glb") && msg.contains("obj") && msg.contains("stl"),
        "{msg}"
    );

    // 不是 glTF 的字节：报解析错误，并提示 .glb 才是自包含的
    let err = load_model_from_bytes("broken.glb", b"not a glb at all").unwrap_err();
    match err {
        AssetError::Parse(msg) => assert!(msg.contains("glb"), "{msg}"),
        other => panic!("应当是解析错误：{other:?}"),
    }

    // OBJ 不是合法 UTF-8
    let err = load_model_from_bytes("bad.obj", &[0xff, 0xfe, 0xfd]).unwrap_err();
    assert!(err.to_string().contains("UTF-8"), "{err}");
}

#[test]
fn native_path_loading_still_reports_missing_files() {
    // Web 端的守卫不能改变原生行为：路径不存在时依旧是 NotFound（列出候选路径）
    let err = load_model("definitely-missing-model-xyz.glb").unwrap_err();
    match err {
        AssetError::NotFound(paths) => assert!(!paths.is_empty(), "要列出试过的路径"),
        other => panic!("应当是 NotFound：{other:?}"),
    }
}

/// 手搓的「4 节点 + 2 轨道」GLB：平移、旋转、父子累乘、同一网格多实例，外加一条 1 秒动画。
// 旋转关键帧里要写 0.7071068（sin45°），clippy 会建议用 `FRAC_1_SQRT_2`；但那段是
// **JSON 字符串**，塞常量进去要把整个 JSON 改成 format!（引号/大括号全要转义）。
// 这里明确允许近似常量，比为了一个测试把夹具搞得不可读更划算。
#[allow(clippy::approx_constant)]
fn nodes_with_animation_glb() -> Vec<u8> {
    // BIN：位置(36) + 法线(36) + 索引(6，u16 补齐到 8)
    let mut bin = Vec::new();
    for f in [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0] {
        bin.extend_from_slice(&f.to_le_bytes()); // 三角形 (0,0)-(1,0)-(0,1)，平面 Z+
    }
    for _ in 0..3 {
        for f in [0.0f32, 0.0, 1.0] {
            bin.extend_from_slice(&f.to_le_bytes()); // 法线都朝 +Z
        }
    }
    for i in [0u16, 1, 2] {
        bin.extend_from_slice(&i.to_le_bytes());
    }
    // 动画数据：2 个时间点 + 2 个平移 + 2 个旋转（绕 Z 从 0° 到 90°）
    // 先把位置/法线/索引补到 80 字节（索引只有 6 字节）—— 否则后面的数据错位 2 字节，
    // 读出来是 2.3e-23 这种 denormal，报错信息只会说"时长不对"。
    while bin.len() < 80 {
        bin.push(0);
    }
    for f in [0.0f32, 1.0] {
        bin.extend_from_slice(&f.to_le_bytes()); // 80..88
    }
    for v in [[2.0f32, 0.0, 0.0], [5.0, 0.0, 0.0]] {
        for f in v {
            bin.extend_from_slice(&f.to_le_bytes()); // 88..112
        }
    }
    for q in [[0.0f32, 0.0, 0.0, 1.0], [0.0, 0.0, 0.7071068, 0.7071068]] {
        for f in q {
            bin.extend_from_slice(&f.to_le_bytes()); // 112..144
        }
    }
    let json = r#"{"asset":{"version":"2.0"},"scene":0,
"scenes":[{"nodes":[0,1,2]}],
"nodes":[
  {"mesh":0,"name":"右","translation":[2.0,0.0,0.0],"children":[3]},
  {"mesh":0,"name":"左","translation":[-2.0,0.0,0.0]},
  {"mesh":0,"name":"转过的","rotation":[0.7071068,0.0,0.0,0.7071068]},
  {"mesh":0,"name":"子节点","translation":[0.0,1.0,0.0]}
],
"meshes":[{"name":"三角","primitives":[{"attributes":{"POSITION":0,"NORMAL":1},"indices":2}]}],
"accessors":[
  {"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0.0,0.0,0.0],"max":[1.0,1.0,0.0]},
  {"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"},
  {"bufferView":2,"componentType":5123,"count":3,"type":"SCALAR"},
  {"bufferView":3,"componentType":5126,"count":2,"type":"SCALAR","min":[0.0],"max":[1.0]},
  {"bufferView":4,"componentType":5126,"count":2,"type":"VEC3"},
  {"bufferView":5,"componentType":5126,"count":2,"type":"VEC4"}
],
"bufferViews":[
  {"buffer":0,"byteOffset":0,"byteLength":36,"target":34962},
  {"buffer":0,"byteOffset":36,"byteLength":36,"target":34962},
  {"buffer":0,"byteOffset":72,"byteLength":6,"target":34963},
  {"buffer":0,"byteOffset":80,"byteLength":8},
  {"buffer":0,"byteOffset":88,"byteLength":24},
  {"buffer":0,"byteOffset":112,"byteLength":32}
],
"animations":[{"name":"移动",
  "channels":[
    {"sampler":0,"target":{"node":0,"path":"translation"}},
    {"sampler":1,"target":{"node":2,"path":"rotation"}}
  ],
  "samplers":[
    {"input":3,"output":4,"interpolation":"LINEAR"},
    {"input":3,"output":5,"interpolation":"LINEAR"}
  ]}],
"buffers":[{"byteLength":144}]}"#;
    glb_from_parts(json, bin)
}

/// 节点变换必须真的烘进几何：同一网格被多个节点引用 → 多个实例，各在各的位置。
///
/// 【曾经是 bug】加载器直接遍历 `doc.meshes()`、完全不看节点，于是所有部件堆在原点
/// （单网格模型看不出来，两个轮子的车一眼就看出来），同一网格被 3 个节点引用也只画一次。
/// 这条测试同时盖住：平移、旋转（法线跟着转）、父子累乘、同名网格的多实例。
#[test]
fn gltf_node_transforms_are_baked_into_geometry() {
    let glb = nodes_with_animation_glb();
    let model = load_model_from_bytes("nodes.glb", &glb).expect("该能解析");
    assert_eq!(
        model.meshes.len(),
        4,
        "同一个网格被 4 个节点引用，就该有 4 份实例：{:?}",
        model.meshes.iter().map(|m| &m.name).collect::<Vec<_>>()
    );
    let by = |prefix: &str| {
        model
            .meshes
            .iter()
            .find(|m| m.name.starts_with(prefix))
            .unwrap_or_else(|| {
                panic!(
                    "没有 {prefix} 开头的网格：{:?}",
                    model.meshes.iter().map(|m| &m.name).collect::<Vec<_>>()
                )
            })
    };
    // 右移 2 米：位置整体 +2
    let right = by("右/");
    let xs: Vec<f32> = right.data.vertices.iter().map(|v| v.position[0]).collect();
    assert!(
        xs.iter().any(|x| (x - 2.0).abs() < 1e-5),
        "右该在 x=2：{xs:?}"
    );
    assert!(xs.iter().all(|x| *x >= 1.999), "右不该还留在原点：{xs:?}");
    // 左移 2 米
    let left = by("左/");
    let xs: Vec<f32> = left.data.vertices.iter().map(|v| v.position[0]).collect();
    assert!(xs.iter().all(|x| *x <= -0.999), "左该在 x=-2：{xs:?}");
    // 绕 X 转 +90°：法线 (0,0,1) → (0,-1,0)，顶点 (0,1,0) → (0,0,1)
    let rotated = by("转过的/");
    for v in &rotated.data.vertices {
        assert!(
            (v.normal[1] + 1.0).abs() < 1e-4 && v.normal[2].abs() < 1e-4,
            "旋转节点的法线该是 (0,-1,0)：{:?}",
            v.normal
        );
    }
    let max_z = rotated
        .data
        .vertices
        .iter()
        .map(|v| v.position[2])
        .fold(f32::NEG_INFINITY, f32::max);
    assert!((max_z - 1.0).abs() < 1e-5, "该转到 z=1，实际 {max_z}");
    // 父子累乘：子节点自己 +1 米，父节点 +2 米 ⇒ 世界 x=2、y=1
    let child = by("子节点/");
    let ys: Vec<f32> = child.data.vertices.iter().map(|v| v.position[1]).collect();
    assert!(
        ys.iter().any(|y| (y - 1.0).abs() < 1e-5) && ys.iter().all(|y| *y >= -1e-6),
        "子节点该被父节点带到 x=2、自己上移 1：{ys:?}"
    );
    let xs: Vec<f32> = child.data.vertices.iter().map(|v| v.position[0]).collect();
    assert!(
        xs.iter().any(|x| (x - 2.0).abs() < 1e-5),
        "子节点该继承父节点的 x=2：{xs:?}"
    );

    // ── 节点表与动画（gap #23 第一刀：解析 + CPU 采样）────────────────────
    assert_eq!(model.nodes.len(), 4, "4 个节点都要在表里");
    assert_eq!(model.nodes[3].parent, Some(0), "子节点的父是「右」");
    assert_eq!(model.animation_names(), vec!["移动"]);
    let clip = model.animation("移动").expect("该有动画");
    assert_eq!(clip.tracks.len(), 2, "一条平移 + 一条旋转");
    assert!(
        (clip.duration - 1.0).abs() < 1e-6,
        "时长 1 秒：{}",
        clip.duration
    );

    // 绑定姿态 = 初始状态：几何是烘好的，所以"动画矩阵"必须是单位矩阵
    let bind = model.bind_pose();
    let right_idx = model
        .meshes
        .iter()
        .position(|m| m.name.starts_with("右/"))
        .unwrap();
    let m = model.mesh_matrix(right_idx, &bind);
    let identity = [
        1.0f32, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ];
    for (a, b) in m.to_cols_array().iter().zip(identity.iter()) {
        assert!(
            (a - b).abs() < 1e-5,
            "绑定姿态下 mesh_matrix 必须是单位矩阵：{:?}",
            m.to_cols_array()
        );
    }

    // 采样：t=0 → 2，t=0.5 → 3.5，t=1 → 5（LINEAR）
    let at = |t: f32| model.sample_animation("移动", t).expect("名字对就该有结果");
    for (t, want) in [(0.0f32, 2.0f32), (0.5, 3.5), (1.0, 5.0)] {
        let w = at(t);
        assert!(
            (w[0].to_cols_array()[12] - want).abs() < 1e-4,
            "t={t} 时「右」该在 x={want}，实际 {}",
            w[0].to_cols_array()[12]
        );
    }
    // 子节点跟着父节点走（世界矩阵）
    assert!(
        (at(0.5)[3].to_cols_array()[13] - 1.0).abs() < 1e-4,
        "子节点该仍在父节点上方 1 米"
    );
    // 烘过的几何 + mesh_matrix = 动画后的位置
    let baked = right.data.vertices[1].position; // x ≈ 3（三角的 (1,0) 顶点）
    let anim = at(0.5);
    let moved = model
        .mesh_matrix(right_idx, &anim)
        .transform_point3(Vec3::new(baked[0], baked[1], baked[2]));
    assert!(
        (moved.x - (baked[0] + 1.5)).abs() < 1e-4,
        "t=0.5 时整体该右移 1.5（2→3.5）：{} vs {}",
        moved.x,
        baked[0] + 1.5
    );

    // 旋转轨道：t=1 时绕 Z 转 90°，(1,0) 方向 → (0,1)
    let rot = at(1.0);
    let rotated_dir = rot[2].transform_vector3(Vec3::new(1.0, 0.0, 0.0));
    assert!(
        rotated_dir.x.abs() < 1e-3 && (rotated_dir.y - 1.0).abs() < 1e-3,
        "90° 后 (1,0) 该变成 (0,1)：{rotated_dir:?}"
    );
    // 不存在的动画名 → None（不是 panic、也不是静默给绑定姿态）
    assert!(model.sample_animation("没有这条", 0.0).is_none());
    assert!(model.player("没有这条").is_none());

    // ── 播放器：时间推进 / 倍速 / 循环 / 播完 ────────────────────────────────
    let mut p = model.player("移动").expect("该有播放器");
    assert!((p.duration - 1.0).abs() < 1e-6 && p.time == 0.0 && p.looping);
    assert_eq!(p.mesh_matrices(&model).len(), model.meshes.len());
    let x_of_right = |mats: &[quench3d::math::Mat4]| mats[right_idx].to_cols_array()[12];
    p.advance(0.25).advance(0.25);
    assert!(
        (p.time - 0.5).abs() < 1e-5,
        "两次 0.25 秒后该是 0.5：{}",
        p.time
    );
    let mats = p.mesh_matrices(&model);
    assert!(
        (x_of_right(&mats) - 1.5).abs() < 1e-4,
        "t=0.5 时「右」该右移 1.5（2→3.5），实际 {}",
        x_of_right(&mats)
    );
    // 循环：再走 1 秒 → 1.5 对 1.0 取模 = 0.5
    p.advance(1.0);
    assert!(
        (p.time - 0.5).abs() < 1e-4 && !p.finished,
        "循环播放该绕回 0.5 且不 finished：time={} finished={}",
        p.time,
        p.finished
    );
    // 倍速：speed=2 走 0.25 秒 = 0.5 秒的动画时间
    let mut fast = model.player("移动").unwrap();
    fast.speed = 2.0;
    fast.advance(0.25);
    assert!(
        (fast.time - 0.5).abs() < 1e-4,
        "倍速 2 该走到 0.5：{}",
        fast.time
    );
    // 不循环：夹到时长并置 finished
    let mut once = model.player("移动").unwrap();
    once.looping = false;
    once.advance(5.0);
    assert!(
        (once.time - 1.0).abs() < 1e-6 && once.finished,
        "不循环该夹到 1.0 且 finished：time={} finished={}",
        once.time,
        once.finished
    );
    // 非法 dt / 非法速度不 panic、不产生 NaN
    let mut weird = model.player("移动").unwrap();
    weird.speed = 0.0;
    weird.advance(f32::NAN);
    assert!(weird.time.is_finite(), "时间不该变 NaN：{}", weird.time);
}

/// 从**解析**到**场景物体真的动了**的整条链（CLI 的 `models[].animation` 与编辑器的
/// 帧循环共用同一条）：
/// GLB 字节 → `LoadedModel` → `player(name)` → `Scene::play_model_animation`
/// → `advance_animations` → 物体变换。
#[test]
fn scene_animation_moves_the_spawned_meshes() {
    use quench3d::math::{Quat, Transform};
    use quench3d::scene::Scene;

    let model = std::sync::Arc::new(
        load_model_from_bytes("nodes.glb", &nodes_with_animation_glb()).expect("该能解析"),
    );
    let mut scene = Scene::new();
    let base = Transform::new(Vec3::new(10.0, 0.0, 0.0), Quat::IDENTITY, Vec3::ONE);
    // 真实 `spawn` 需要 GPU；这里用同一份"片段 → 物体下标、基础变换"的映射，
    // 场景层的语义（base × 动画矩阵）完全一致。
    let objects: Vec<usize> = (0..model.meshes.len())
        .map(|i| scene.add_named(i as u32, 0, base, &format!("片段{i}")))
        .collect();
    let right = model
        .meshes
        .iter()
        .position(|m| m.name.starts_with("右/"))
        .expect("「右」片段该在");
    let left = model
        .meshes
        .iter()
        .position(|m| m.name.starts_with("左/"))
        .expect("「左」片段该在");

    let player = model.player("移动").expect("该有播放器");
    scene.play_model_animation(
        model.clone(),
        objects.clone(),
        vec![base; model.meshes.len()],
        player,
    );

    // t = 0：保持基础变换（绑定姿态下动画矩阵是单位矩阵）。
    assert_eq!(
        scene.advance_animations(0.0),
        objects.len(),
        "每个片段都该算更新"
    );
    assert!(
        (scene.objects[objects[right]].transform.position.x - 10.0).abs() < 1e-4,
        "t=0 该保持基础位置"
    );

    // t = 0.5：节点 0 的平移从 2 走到 3.5，所以相对基础位置右移 1.5。
    scene.advance_animations(0.5);
    let x = scene.objects[objects[right]].transform.position.x;
    assert!(
        (x - 11.5).abs() < 1e-4,
        "t=0.5 时「右」该在 11.5（基础 10 + 1.5），实际 {x}"
    );
    // 没有轨道覆盖的节点（「左」）一分不动。
    let x = scene.objects[objects[left]].transform.position.x;
    assert!((x - 10.0).abs() < 1e-4, "「左」不该动，实际 {x}");

    // 循环：走满 1 秒会绕回 t=0 —— 位置回到基础变换（不是继续往前漂）。
    scene.advance_animations(0.5);
    let x = scene.objects[objects[right]].transform.position.x;
    assert!(
        (x - 10.0).abs() < 1e-4,
        "循环走满 1 秒该绕回基础位置，实际 {x}"
    );
}

/// 编辑器拖动时间轴：直接改 `player.time` 再 `refresh_animations`，位姿必须
/// 精确落在该时间点，且**不会**再推进时间（拖动不是播放）。
#[test]
fn refresh_animations_applies_current_time_without_advancing() {
    use quench3d::math::{Quat, Transform};
    use quench3d::scene::Scene;

    let model = std::sync::Arc::new(
        load_model_from_bytes("nodes.glb", &nodes_with_animation_glb()).expect("该能解析"),
    );
    let mut scene = Scene::new();
    let base = Transform::new(Vec3::new(10.0, 0.0, 0.0), Quat::IDENTITY, Vec3::ONE);
    let objects: Vec<usize> = (0..model.meshes.len())
        .map(|i| scene.add_named(i as u32, 0, base, &format!("片段{i}")))
        .collect();
    let right = model
        .meshes
        .iter()
        .position(|m| m.name.starts_with("右/"))
        .expect("「右」片段该在");
    scene.play_model_animation(
        model.clone(),
        objects.clone(),
        vec![base; model.meshes.len()],
        model.player("移动").expect("该有播放器"),
    );

    scene.advance_animations(0.5);
    let x = scene.objects[objects[right]].transform.position.x;
    assert!((x - 11.5).abs() < 1e-4, "t=.5 该在 11.5，实际 {x}");

    // 拖回 t=.25：线性插值 2→3.5 的中点是 2.75，所以世界 x = 10.75。
    scene.animations[0].player.time = 0.25;
    assert_eq!(scene.refresh_animations(), objects.len());
    let x = scene.objects[objects[right]].transform.position.x;
    assert!((x - 10.75).abs() < 1e-4, "t=.25 该在 10.75，实际 {x}");
    // 再刷一次不推进时间，也不累积位移。
    scene.refresh_animations();
    assert!((scene.animations[0].player.time - 0.25).abs() < 1e-6);
    let x = scene.objects[objects[right]].transform.position.x;
    assert!((x - 10.75).abs() < 1e-4, "重复刷新不该漂移，实际 {x}");
}

/// 场景层也真的推进**两条**时间线：primary 在 t=.5 位移 1.5，secondary 位移 3.5，
/// weight=0.5 时物体应落在 base + 2.5。CLI 的 `dt=0` 只能证明起始姿态，这里证明
/// `Scene::advance_animations` 会同时 `advance` 两条 clip 并走混合采样。
#[test]
fn scene_blended_animation_advances_both_clips() {
    use quench3d::math::{Quat, Transform};
    use quench3d::scene::Scene;

    let mut model =
        load_model_from_bytes("nodes.glb", &nodes_with_animation_glb()).expect("该能解析");
    let mut second = model.animations[0].clone();
    second.name = "移动2".into();
    for track in &mut second.tracks {
        if track.property == quench3d::assets::AnimatedProperty::Translation {
            for value in &mut track.values {
                value[0] += 2.0;
            }
        }
    }
    model.animations.push(second);
    let model = std::sync::Arc::new(model);

    let mut scene = Scene::new();
    let base = Transform::new(Vec3::new(10.0, 0.0, 0.0), Quat::IDENTITY, Vec3::ONE);
    let objects: Vec<usize> = (0..model.meshes.len())
        .map(|i| scene.add_named(i as u32, 0, base, &format!("片段{i}")))
        .collect();
    let right = model
        .meshes
        .iter()
        .position(|m| m.name.starts_with("右/"))
        .expect("「右」片段该在");

    scene.play_model_animation_blend(
        model.clone(),
        objects.clone(),
        vec![base; model.meshes.len()],
        model.player("移动").expect("primary"),
        model.player("移动2").expect("secondary"),
        0.5,
    );

    scene.advance_animations(0.5);
    let x = scene.objects[objects[right]].transform.position.x;
    assert!(
        (x - 12.5).abs() < 1e-4,
        "t=.5 混合后该在 10 + (1.5+3.5)/2 = 12.5，实际 {x}"
    );
}

/// 淡入：`fade=0.5` 时 t=.25 的权重是 smoothstep(.5)=0.5，t=.5 到 1。
///
/// 用与 `scene_blended_animation_advances_both_clips` 同一对 clip（第二条整体 +2），
/// 这样断言能同时钉住"时间前进"和"权重曲线"：t=.25 位移 1.75，t=.5 位移 3.5。
#[test]
fn scene_blend_fades_in_over_time() {
    use quench3d::math::{Quat, Transform};
    use quench3d::scene::Scene;

    let mut model =
        load_model_from_bytes("nodes.glb", &nodes_with_animation_glb()).expect("该能解析");
    let mut second = model.animations[0].clone();
    second.name = "移动2".into();
    for track in &mut second.tracks {
        if track.property == quench3d::assets::AnimatedProperty::Translation {
            for value in &mut track.values {
                value[0] += 2.0;
            }
        }
    }
    model.animations.push(second);
    let model = std::sync::Arc::new(model);

    let mut scene = Scene::new();
    let base = Transform::new(Vec3::new(10.0, 0.0, 0.0), Quat::IDENTITY, Vec3::ONE);
    let objects: Vec<usize> = (0..model.meshes.len())
        .map(|i| scene.add_named(i as u32, 0, base, &format!("片段{i}")))
        .collect();
    let right = model
        .meshes
        .iter()
        .position(|m| m.name.starts_with("右/"))
        .expect("「右」片段该在");

    scene.play_model_animation_blend_with_fade(
        model.clone(),
        objects.clone(),
        vec![base; model.meshes.len()],
        model.player("移动").expect("primary"),
        model.player("移动2").expect("secondary"),
        quench3d::scene::BlendOptions::with_fade(1.0, 0.5),
    );

    scene.advance_animations(0.25);
    let x = scene.objects[objects[right]].transform.position.x;
    assert!(
        (x - 11.75).abs() < 1e-4,
        "t=.25 权重 0.5，位移应为 (0.75+2.75)/2=1.75，实际 {x}"
    );

    scene.advance_animations(0.25);
    let x = scene.objects[objects[right]].transform.position.x;
    assert!(
        (x - 13.5).abs() < 1e-4,
        "淡入结束权重 1，t=.5 应完全取第二条（位移 3.5），实际 {x}"
    );
}

/// 状态机：按停留时间从 `移动` 淡入 `移动2`，淡入结束后把新 clip 提为主 clip。
#[test]
fn scene_animator_transitions_and_promotes() {
    use quench3d::scene::{ModelAnimatorState, ModelAnimatorTransition, Scene};

    let mut model =
        load_model_from_bytes("nodes.glb", &nodes_with_animation_glb()).expect("该能解析");
    let mut second = model.animations[0].clone();
    second.name = "移动2".into();
    for track in &mut second.tracks {
        if track.property == quench3d::assets::AnimatedProperty::Translation {
            for value in &mut track.values {
                value[0] += 2.0;
            }
        }
    }
    model.animations.push(second);
    let model = std::sync::Arc::new(model);
    let states = vec![
        ModelAnimatorState {
            name: "idle".into(),
            animation: "移动".into(),
            speed: 1.0,
            looping: true,
            start: 0.0,
        },
        ModelAnimatorState {
            name: "run".into(),
            animation: "移动2".into(),
            speed: 1.0,
            looping: true,
            start: 0.0,
        },
    ];
    let transitions = vec![ModelAnimatorTransition {
        from: "idle".into(),
        to: "run".into(),
        after: 0.5,
        on_finished: false,
        fade: 0.5,
        conditions: Vec::new(),
        on_event: None,
    }];
    let mut scene = Scene::new();
    let idx = scene
        .play_model_animator(
            model.clone(),
            Vec::new(),
            Vec::new(),
            states,
            transitions,
            0,
        )
        .expect("两个 clip 都存在");
    assert_eq!(scene.animations[idx].player.animation, "移动");
    assert!(scene.animations[idx].secondary.is_none());

    scene.advance_animations(0.25);
    let a = &scene.animations[idx];
    assert_eq!(a.player.animation, "移动", "0.25 秒还没到过渡时间");
    assert!(a.secondary.is_none());

    scene.advance_animations(0.25);
    let a = &scene.animations[idx];
    assert_eq!(
        a.player.animation, "移动",
        "停留时间到 0.5 秒的这一帧还没开始过渡"
    );
    assert!(a.secondary.is_none());

    scene.advance_animations(0.25);
    let a = &scene.animations[idx];
    assert_eq!(a.player.animation, "移动", "过渡期间旧 clip 仍是主 clip");
    assert_eq!(
        a.secondary.as_ref().map(|p| p.animation.as_str()),
        Some("移动2"),
        "第二条 clip 应当开始淡入"
    );
    assert!(
        a.blend_weight > 0.4 && a.blend_weight < 0.6,
        "过渡开始后的这一帧应走到半程：{}",
        a.blend_weight
    );

    scene.advance_animations(0.25);
    let a = &scene.animations[idx];
    assert_eq!(a.player.animation, "移动2", "淡入结束应提为主 clip");
    assert!(a.secondary.is_none());
    let animator = a.animator.as_ref().expect("状态机还在");
    assert_eq!(animator.current, 1);
    assert_eq!(animator.pending, None);
    assert!(animator.elapsed < 1e-6, "新状态应重新计时");
}

/// `on_finished`：非循环 clip 播完后的**下一帧首**才切换，避免同一帧既结束又切换。
#[test]
fn scene_animator_transitions_when_clip_finishes() {
    use quench3d::scene::{ModelAnimatorState, ModelAnimatorTransition, Scene};

    let mut model =
        load_model_from_bytes("nodes.glb", &nodes_with_animation_glb()).expect("该能解析");
    let mut second = model.animations[0].clone();
    second.name = "移动2".into();
    model.animations.push(second);
    let model = std::sync::Arc::new(model);
    let states = vec![
        ModelAnimatorState {
            name: "once".into(),
            animation: "移动".into(),
            speed: 1.0,
            looping: false,
            start: 0.0,
        },
        ModelAnimatorState {
            name: "after".into(),
            animation: "移动2".into(),
            speed: 1.0,
            looping: true,
            start: 0.0,
        },
    ];
    let transitions = vec![ModelAnimatorTransition {
        from: "once".into(),
        to: "after".into(),
        after: 0.0,
        on_finished: true,
        fade: 0.0,
        conditions: Vec::new(),
        on_event: None,
    }];
    let mut scene = Scene::new();
    let idx = scene
        .play_model_animator(model, Vec::new(), Vec::new(), states, transitions, 0)
        .expect("两个 clip 都存在");

    scene.advance_animations(0.6);
    assert_eq!(scene.animations[idx].player.animation, "移动");
    assert!(!scene.animations[idx].player.finished);

    // 这一帧把时间推到 clips 末尾并置 finished；帧首判定时还没 finished，所以不切。
    scene.advance_animations(0.6);
    assert_eq!(scene.animations[idx].player.animation, "移动");
    assert!(scene.animations[idx].player.finished);

    // 下一帧首看到 finished，立即切到循环的「移动2」。
    scene.advance_animations(0.1);
    let a = &scene.animations[idx];
    assert_eq!(a.player.animation, "移动2");
    assert!(!a.player.finished);
    assert!(a.secondary.is_none());
    let animator = a.animator.as_ref().expect("状态机还在");
    assert_eq!(animator.current, 1);
    assert!(animator.elapsed < 1e-6, "新状态应重新计时");
}

/// CUBICSPLINE 的切线必须从解析器一路传到采样：t=.5 是 .75 而不是 .5。
#[test]
fn cubic_spline_tangents_survive_parsing_and_sampling() {
    let model = load_model_from_bytes("cubic.glb", &cubic_glb()).expect("CUBICSPLINE GLB");
    let clip = model.animation("cubic").expect("cubic clip");
    let track = &clip.tracks[0];
    assert_eq!(
        track.interpolation,
        quench3d::assets::Interpolation::CubicSpline
    );
    assert_eq!(
        track.values,
        vec![[0.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]]
    );
    assert_eq!(track.in_tangents.len(), 2);
    assert_eq!(track.out_tangents.len(), 2);
    assert!((track.out_tangents[0][0] - 2.0).abs() < 1e-6);
    let worlds = model.sample_animation("cubic", 0.5).expect("采样");
    let x = worlds[0].transform_point3(Vec3::ZERO).x;
    assert!((x - 0.75).abs() < 1e-5, "Hermite 中点 x 应为 .75，实际 {x}");
}
