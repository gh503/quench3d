//! 编辑器存盘与 `include` 的接口守卫（纯 CPU，无 GPU，CI 每条腿都会跑）。
//!
//! 编辑器编辑的是**合并后**的世界，所以存盘必须把片段内联进根文件并清掉 `include`。
//! 两条错法都是静默的：
//!
//! * 留着 `include` 又按引擎重建 `objects` → 下次载入把同一批物体**再并一遍**（数据繁殖）；
//! * 只清 `include` 不合并 → 片段里的 `models` / `joints` / `actions` / 材质被**悄悄丢掉**。
//!
//! 现有的往返测试要真 GPU（`GpuContext::headless`），CI 上没有显卡时整条 skip ——
//! 所以这里用"纯 CPU 的导出底稿函数" + "源码结构断言"守住这条链。
//! 它不证明"真窗口里点存盘会得到什么"（那要真窗口 + 真显卡），见
//! `docs/production_readiness.md` 的诚实边界。

use quench3d_cli::scene_doc::SceneDoc;
use quench3d_editor::scene_io::export_base_doc;

/// 临时目录（每个用例一个，避免互相踩）。
fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "quench3d_editor_include_{tag}_{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn write(dir: &std::path::Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, text).unwrap();
}

/// 存盘底稿必须：片段内容并进来（否则丢掉）、`include` 清掉（否则写重）、并留下说明。
#[test]
fn export_base_doc_inlines_fragments_and_clears_include() {
    let dir = temp_dir("inline");
    write(
        &dir,
        "main.json",
        r#"{
          "format": "quench3d.scene", "version": 2,
          "include": ["parts/rig.json"],
          "ground": { "size": 30 },
          "objects": [ { "name": "底座", "shape": { "type": "box", "size": [2, 0.2, 2] },
                         "position": [0, 0.1, 0], "body": "static" } ],
          "joints": [ { "type": "fixed", "a": "底座", "b": "摆臂" } ]
        }"#,
    );
    write(
        &dir,
        "parts/rig.json",
        r#"{
          "format": "quench3d.scene", "version": 2,
          "materials": { "金属": { "color": [0.6, 0.6, 0.65], "metallic": 0.9 } },
          "objects": [ { "name": "摆臂", "shape": { "type": "box", "size": [0.2, 1.5, 0.2] },
                         "position": [0, 1, 0], "body": "dynamic", "material": "金属" } ]
        }"#,
    );
    let loaded = SceneDoc::from_file(dir.join("main.json")).expect("根场景要能解析");
    let mut notes = Vec::new();
    let base = export_base_doc(&loaded, &mut notes).expect("有 include 时必须能合并");

    // 片段的内容真的并进来了（只清 include 不合并的话，这几条会红）
    assert!(
        base.materials.contains_key("金属"),
        "片段里的材质必须并进底稿，否则存盘会把片段物体的贴图/材质清零"
    );
    assert!(
        base.objects
            .iter()
            .any(|o| o.name.as_deref() == Some("摆臂")),
        "片段里的物体必须并进底稿"
    );
    assert_eq!(base.objects.len(), 2, "根 + 片段各一个物体");
    assert_eq!(base.joints.len(), 1, "关节引用两端，必须保留");
    // `include` 必须清掉，否则下次载入再并一遍
    assert!(base.include.is_empty(), "内联之后必须清空 include");
    // 根独有的段不受影响
    assert_eq!(base.ground.as_ref().map(|g| g.size), Some(30.0));
    // 说明里要点出"内联了什么"，不能静默改文件结构
    assert_eq!(notes.len(), 1, "应当有一条说明：{notes:?}");
    assert!(
        notes[0].contains("parts/rig.json") && notes[0].contains("内联"),
        "说明里要点名被内联的片段：{notes:?}"
    );

    // 内联后的底稿必须是一份能独立解析的合法场景（否则编辑器存出来的文件 CLI 读不了）
    let text = base.to_json_pretty();
    let back = SceneDoc::from_json(&text).expect("内联后的底稿必须能被解析");
    assert!(!quench3d_cli::scene_doc::SceneDoc::has_errors(
        &back.validate()
    ));
    assert!(
        !text.contains("\"include\""),
        "内联后不该再有 include 键：{text}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 没有 `include` 时是**逐字节不变**的克隆（老场景存盘结果一个字都不该变）。
#[test]
fn export_base_doc_without_include_is_byte_identical() {
    let loaded = SceneDoc::from_json(
        r#"{ "name": "老场景", "ground": { "size": 10 },
            "objects": [ { "name": "箱", "shape": { "type": "box", "size": [1,1,1] },
                           "position": [0, 0.5, 0], "body": "dynamic" } ] }"#,
    )
    .unwrap();
    let mut notes = Vec::new();
    let base = export_base_doc(&loaded, &mut notes).expect("没有 include 时必然成功");
    assert!(notes.is_empty(), "没有内联就不该有说明：{notes:?}");
    assert_eq!(base.to_json_pretty(), loaded.to_json_pretty());
    // 空文档（新建场景）也不该 panic
    let mut notes = Vec::new();
    assert!(export_base_doc(&SceneDoc::default(), &mut notes).is_ok());
    assert!(notes.is_empty());
}

/// 片段坏了就**拒绝存盘**：宁可报错，也不要写出一份"看起来成功、其实丢了一半"的文件。
#[test]
fn export_base_doc_refuses_to_save_when_an_include_is_broken() {
    let dir = temp_dir("broken");
    write(
        &dir,
        "main.json",
        r#"{ "format": "quench3d.scene", "version": 2, "include": ["parts/nope.json"] }"#,
    );
    let loaded = SceneDoc::from_file(dir.join("main.json")).unwrap();
    let mut notes = Vec::new();
    let err = export_base_doc(&loaded, &mut notes).expect_err("片段缺失必须拒绝存盘");
    assert!(err.contains("include 解析失败"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 结构守卫：`scene_to_json` 必须走 [`export_base_doc`]，而且材质继承查的是**合并后的底稿**。
///
/// 这条盯的是"有人把导出改回 `doc.base_doc.clone()`"——那种改动在 CI 上不会红
/// （往返测试 skip），只会在真机上悄悄把片段内容写丢。
#[test]
fn scene_to_json_uses_the_merged_export_base() {
    let src = include_str!("../src/scene_io.rs");
    assert!(
        src.contains("export_base_doc(&loaded, &mut skipped)"),
        "scene_to_json 必须用 export_base_doc 得到导出底稿（合并 include + 清 include）"
    );
    assert!(
        !src.contains("doc.base_doc.as_ref().and_then"),
        "材质继承必须查**合并后的底稿**（`base`），否则片段物体的材质/贴图会在存盘时丢掉"
    );
}
