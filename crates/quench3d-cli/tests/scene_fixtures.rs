//! 仓库里"写在代码之外"的场景夹具，格式版本必须跟着 `scene_doc::VERSION` 走。
//!
//! 为什么值得一条测试：格式从 v1 升到 v2 时，Rust 里的夹具会**立刻**被本机的测试
//! 抓出来（`cli_e2e` 当场红），但下面这两份夹具不在任何 crate 的测试路径里 ——
//! 本地门禁全绿，CI 冒烟测试直接红（`ask` 从假服务拿到的场景被校验器按"不支持的版本"拒掉）。
//! 这类漂移没有编译器帮忙、也没有规律可循，只能把"它们必须等于 `VERSION`"钉死在这里。
//!
//! 加新的外部夹具（又一个脚本 / workflow）时，请把它加进 [`OUTSIDE_FIXTURES`] ——
//! `found == 0` 的断言会提醒你：夹具不见了，或者这条测试该更新了。
#![cfg(not(target_arch = "wasm32"))]

/// 不在任何 crate 的测试路径里、却会被真的执行到的场景夹具（相对仓库根）。
const OUTSIDE_FIXTURES: [&str; 2] = [".github/workflows/ci.yml", "tools/mock_llm_server.py"];

/// 把 `"version": <数字>` 形式的版本号都揪出来。
fn versions_in(text: &str) -> Vec<u32> {
    let mut found = Vec::new();
    for (index, _) in text.match_indices("\"version\"") {
        let rest = text[index + "\"version\"".len()..].trim_start();
        let Some(rest) = rest.strip_prefix(':') else {
            continue; // `"version"` 出现在别处（比如 JSON 的键名比较），不管
        };
        let digits: String = rest
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !digits.is_empty() {
            // 上面已确认全是 ASCII 数字，解析不可能失败。
            found.push(digits.parse().expect("数字串"));
        }
    }
    found
}

#[test]
fn outside_scene_fixtures_use_the_current_version() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for relative in OUTSIDE_FIXTURES {
        let path = root.join(relative);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("读不了 {}：{e}", path.display()));
        let versions = versions_in(&text);
        assert!(
            !versions.is_empty(),
            "{relative} 里一个场景版本号都没找到：夹具被删了，还是这条测试该更新了？"
        );
        for version in versions {
            assert_eq!(
                version,
                quench3d_cli::scene_doc::VERSION,
                "{relative} 里的场景夹具是版本 {version}，当前格式版本是 {}",
                quench3d_cli::scene_doc::VERSION
            );
        }
    }
}

/// 这条测试自己的逻辑也得是对的：`versions_in` 不能被别的键带偏。
#[test]
fn versions_in_ignores_unrelated_keys() {
    // 只有紧跟冒号的才认；`"version": "0.1.0"`（字符串值）与别的键都该被跳过。
    let sample = r#"{"format": "quench3d.scene", "version": 2, "versionString": 7,
                     "tool": {"version": "0.1.0"}, "版本": 3}"#;
    assert_eq!(versions_in(sample), vec![2]);
}
