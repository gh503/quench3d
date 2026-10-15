//! 模型加载：**从内存字节**加载（Web 端唯一的正路），以及报错的质量。
//!
//! 这里手搓一个最小 GLB（一个三角形）而不是塞二进制固件进仓库：
//! 格式的每个字节都在测试里看得见，改了格式也知道哪里断了。

use quench3d::assets::{load_model, load_model_from_bytes, AssetError};

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
"buffers":[{"byteLength":36}]}"#
        .replace('\n', "");
    let mut json_bytes = json.into_bytes();
    while !json_bytes.len().is_multiple_of(4) {
        json_bytes.push(b' '); // JSON 块用空格补齐
    }
    let mut bin_bytes = bin;
    while !bin_bytes.len().is_multiple_of(4) {
        bin_bytes.push(0); // BIN 块用 0 补齐
    }
    let total = 12 + 8 + json_bytes.len() + 8 + bin_bytes.len();
    let mut glb = Vec::with_capacity(total);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes()); // 版本
    glb.extend_from_slice(&(total as u32).to_le_bytes());
    glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x4E4F_534Au32.to_le_bytes()); // "JSON"
    glb.extend_from_slice(&json_bytes);
    glb.extend_from_slice(&(bin_bytes.len() as u32).to_le_bytes());
    glb.extend_from_slice(&0x004E_4942u32.to_le_bytes()); // "BIN\0"
    glb.extend_from_slice(&bin_bytes);
    glb
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
