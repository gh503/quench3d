//! 模型加载：**从内存字节**加载（Web 端唯一的正路），以及报错的质量。
//!
//! 这里手搓一个最小 GLB（一个三角形）而不是塞二进制固件进仓库：
//! 格式的每个字节都在测试里看得见，改了格式也知道哪里断了。

use quench3d::assets::{load_model, load_model_from_bytes, AssetError};

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
    let err = load_model_from_bytes("chair.stl", b"solid x").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("glb") && msg.contains("obj"), "{msg}");

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

/// 节点变换必须真的烘进几何：同一网格被多个节点引用 → 多个实例，各在各的位置。
///
/// 【曾经是 bug】加载器直接遍历 `doc.meshes()`、完全不看节点，于是所有部件堆在原点
/// （单网格模型看不出来，两个轮子的车一眼就看出来），同一网格被 3 个节点引用也只画一次。
/// 这条测试同时盖住：平移、旋转（法线跟着转）、父子累乘、同名网格的多实例。
#[test]
fn gltf_node_transforms_are_baked_into_geometry() {
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
  {"bufferView":2,"componentType":5123,"count":3,"type":"SCALAR"}
],
"bufferViews":[
  {"buffer":0,"byteOffset":0,"byteLength":36,"target":34962},
  {"buffer":0,"byteOffset":36,"byteLength":36,"target":34962},
  {"buffer":0,"byteOffset":72,"byteLength":6,"target":34963}
],
"buffers":[{"byteLength":80}]}"#;
    let model = load_model_from_bytes("nodes.glb", &glb_from_parts(json, bin)).expect("该能解析");
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
}
