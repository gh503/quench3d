# 贡献指南

> 这份文件的目标只有一个：**让"改完之后别人能验证"变成默认动作。**
> 没有验证方式的变化 = 没做完。

## 0. 一分钟上手

```bash
git clone https://github.com/gh503/quench3d && cd quench3d

# 桌面编辑器（需要 GPU；Wayland 下如果窗口不出现，见 §5）
cargo run -p quench3d-editor

# 命令行：渲染一帧 / 一段视频
cargo run -p quench3d-cli -- render scene.json -o out.png
cargo run -p quench3d-cli -- --help
```

## 1. 提交前必须过的门（本地一把梭）

```bash
cargo fmt --all
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --target wasm32-unknown-unknown --all-targets -- -D warnings
cargo test --workspace
```

**任何 Rust 改动都要重建 Web 产物**（`web/pkg/` 是入库的，浏览器端直接吃它）：

```bash
tools/build_web.sh web_client
node tools/check_web.mjs        # 最后跑：校验产物与页面接线
```

改 `web/index.html` 或任何 wasm 导出函数后，`check_web.mjs` 必须仍然通过。

## 2. 三条不能碰的红线

1. **`web/pkg/` 与 `Cargo.lock` 是入库的**，不要加进 `.gitignore`，也不要手工编辑。
2. **物理与仿真视频的摘要 digest 不能变**：物理 `0xbcc0bccf037b867f`、
   视频 `fnv1a64:4243836d6b378594`。它们由测试钉住——如果某个改动真的改变了结果，
   必须在 PR 描述里说清"为什么应该变"，并同步更新摘要。
3. **`--locked` 是 CI 的默认**：改了依赖就要提交 `Cargo.lock`。

## 3. 代码约定

- 语言：注释、文档、提交信息**都用中文**（面向的读者是中文使用者）。
  标识符用英文。
- 注释解释**为什么**，不复述代码在做什么。凡是"这里为什么不能那样写"的地方，
  写清踩过的坑（历史原因最有价值）。
- 公开项都要有文档注释；crate 级的 `#![warn(missing_docs)]` 已开。
- 不引入 `unsafe`：workspace 已 `#![forbid(unsafe_code)]`。
- 新增依赖前先问三个问题：能不能自己写 50 行？会不会拖垮 wasm 体积？
  维护状态如何？——体积敏感（Web 端产物要能下载）。

## 4. 加一个新能力的完整流程

1. **写清"验收判据"**：什么命令、什么数字、什么现象算做完。没有判据就别开工。
2. 实现 + 单元测试（纯逻辑的部分不要依赖 GPU，CI 机器没有软件光栅驱动）。
3. 需要 GPU 的测试**没有适配器就跳过**，写法照抄
   `crates/quench3d-editor/tests/history.rs` 里的 `headless_engine()`。
4. 跑 §1 的全部门；改了 UI/交互就去 §5 做一次真机验证并留下证据（截图/数字）。
5. 更新 `CHANGELOG.md`；能力边界写进 `README.md` 的"限制"一节与
   `docs/production_readiness.md` 的对应条目。
6. 提交信息写"做了什么 + 怎么验证的"，不要只写"fix bug"。

## 5. 验证配方（跑不起来时照这个查）

**桌面窗口（Wayland 下合成器可能不给窗口，用 X11 显式跑）**：

```bash
env -u WAYLAND_DISPLAY DISPLAY=:0 RUST_LOG=info cargo run -p quench3d-editor
# 截图（需要 xdotool + ImageMagick）
xdotool search --name Quench3D
import -window <id> /tmp/editor.png
```

**浏览器（无头 Chrome + SwiftShader 软光栅）**：

```bash
python3 -m http.server 8231 --bind 127.0.0.1 --directory web &
google-chrome --headless=new --no-sandbox --disable-dev-shm-usage \
  --enable-unsafe-swiftshader --use-gl=angle --use-angle=swiftshader \
  --window-size=1280,800 --remote-debugging-port=9333 \
  --user-data-dir=/tmp/cdp-profile http://127.0.0.1:8231/
```

**无 GPU 的 CI 上不要写"必须有 GPU"的测试**。物理/格式/编辑器历史的验证都应该是纯 CPU 的。

## 6. 平台事实（写代码前先知道）

- 工具链：CI 钉 `1.99.0`；workspace MSRV `1.90`（编辑器 crate 单独 `1.95`，egui 0.37 的要求）。
- 四平台 CI：ubuntu / macOS / Windows / wasm32。Windows 上的 Python 脚本要以 UTF-8 运行
  （CI 已设 `PYTHONUTF8=1`）。
- Web 端：一份 wasm 产物 11 MB 左右；改动渲染/物理会直接放大它，提交前看体积。
- 场景格式 `quench3d.scene` 由 CLI 与编辑器**共用**，改字段要同时考虑两端与
  `tests/scene_roundtrip.rs`。

## 7. 报告问题

请带上：复现命令、期望与实际、平台与 GPU/后端（`RUST_LOG=info` 的第一行有适配器信息）、
场景文件（能贴上就贴）、以及 `cargo test --workspace` 的结果。

## 一条命令跑门禁

```bash
tools/gate.sh          # fmt + clippy(原生) + clippy(wasm32) + 全部测试 + wasm 构建 + check_web
tools/gate.sh --fast   # 跳过 wasm 构建与 check_web（只改纯 Rust 时够用）
```

顺序有讲究：`check_web.mjs` **必须最后跑**（它检查的是刚构建出来的 wasm 产物）。
脚本会把每个阶段的完整日志写到 `target/tmp/gate-*.log`，失败时自动打印末尾若干行。

`gate.sh` 只做 CI 做的那几件事，不引入任何新依赖；`--fast` 少的那两项在 CI 里仍然会跑。
