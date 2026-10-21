//! 模型加载：**从内存字节**加载（Web 端唯一的正路），以及报错的质量。
//!
//! 这里手搓一个最小 GLB（一个三角形）而不是塞二进制固件进仓库：
//! 格式的每个字节都在测试里看得见，改了格式也知道哪里断了。

use quench3d::assets::{load_model, load_model_from_bytes, AssetError};
use quench3d::math::Vec3;

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
