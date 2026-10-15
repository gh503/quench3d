//! Web 端自检探针（只给 `tools/check_web.mjs` 用，仅 wasm 目标编译）。
//!
//! 为什么这些探针要住在引擎里而不是某个 example 里：它们是「Web 产物真的能用」
//! 的**硬证据**，而 Web 入口（example）以后还会换。放在这里，任何 Web 入口
//! 都能一行转发出去，检查脚本不必跟着入口改名。
//!
//! 每条探针都对应一个真踩过的坑：
//!
//! | 探针 | 挡住的坑 |
//! |---|---|
//! | [`clock_probe`] | `std::time::Instant::now()` 在 `wasm32-unknown-unknown` 上直接 panic，而 `cargo check` 看不出来 |
//! | [`model_path_probe`] | wasm 上没有文件系统，按路径加载模型必须给出「请改用 `load_model_from_bytes`」这种能照着做的报错 |
//! | [`model_bytes_probe`] | 从字节加载模型这条路必须真的跑得通（编译通过 ≠ 能跑） |
//!
//! 这里只有普通函数，**不带** `#[wasm_bindgen]`：wasm-bindgen 会把依赖库里带该
//! 属性、又被链进来的函数一并导出成 JS 符号，Web 入口再转发一次就会撞名
//! （`clock_probe is exported by multiple crates`）。导出交给入口 example 做。

/// 时钟探针：读一次引擎时钟（秒）。
///
/// `cargo check --target wasm32` 只能证明编译通过，而时间路径是在**运行期**炸的。
/// 这个探针让 CI 在 Node 里真的跑一遍时间路径。
#[cfg(target_arch = "wasm32")]
pub fn clock_probe() -> f64 {
    crate::clock::now_seconds()
}

/// 路径探针：Web 端**按路径**加载模型，必须给出能照着做的报错。
///
/// 断言的是错误文本里必须提到 `load_model_from_bytes` —— 否则用户只会看到
/// 「找不到 chair.glb，已尝试：…」，完全不知道 wasm 上根本没有文件系统。
#[cfg(target_arch = "wasm32")]
pub fn model_path_probe() -> String {
    match crate::assets::load_model("assets/chair.glb") {
        Ok(m) => format!("unexpected-ok meshes={}", m.meshes.len()),
        Err(e) => e.to_string(),
    }
}

/// 字节探针：Web 端**从字节**加载模型（内联一个三角形 OBJ）必须真的能跑通。
#[cfg(target_arch = "wasm32")]
pub fn model_bytes_probe() -> String {
    const OBJ: &[u8] = b"v 0 0 0
v 1 0 0
v 0 1 0
f 1 2 3
";
    match crate::assets::load_model_from_bytes("triangle.obj", OBJ) {
        Ok(m) => {
            let tris: usize = m
                .meshes
                .iter()
                .map(|mesh| mesh.data.indices.len() / 3)
                .sum();
            format!("ok meshes={} triangles={tris}", m.meshes.len())
        }
        Err(e) => format!("err {e}"),
    }
}
