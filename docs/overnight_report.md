# 夜间工作报告（等你验收）

> 时间：本轮连续作业（`main` 分支）。目标是你睡前那句话：
> **"能力细节上能满足生产需求吗？缺哪些、要做到什么程度就继续强化，直到目标完成。"**
>
> 结论先行：**"替代消费级内容生产软件"这个目标没有完成，也不该在这一晚声称完成。**
> 我把"最挡路的、最能在天亮前做成可验证事实的"两块补上了，并把"离生产还差什么"写成了一份
> 能逐条复核的评估（[production_readiness.md](production_readiness.md)）。
> 本文件是那一晚的"做了什么 / 为什么是这些 / 还没做什么 / 你怎么在 5 分钟内验证"。

---

## 1. 先回答你的问题：能满足生产需求吗？

**分场景答，不含糊：**

| 用途 | 现在能不能进生产 | 依据 |
|---|---|---|
| 无头批量物理仿真、参数扫描、回归门禁 | **能**（窄子集） | 确定性有逐位测试、并行/串行一致、线程数无关；`simulate`/`bench`/`movie` 可用 |
| CLI 生成场景 → **编辑器手工微调** → CLI 出图/视频 | **能**（本轮补齐了这条链的两处硬伤） | 手柄 + 撤销/重做 + 存盘不再丢段（见 §2） |
| 给 agent 的自动化入口（scene.json + MCP + CLI） | **能**（最接近） | schema/validate/doctor/ask/chat/mcp 齐全，格式可机读 |
| 替代 Blender（建模/UV/雕刻/材质节点） | **不能** | 没有编辑模式、没有 UV/雕刻、材质只有标量 + 4 贴图槽 |
| 替代 Unity/Unreal（游戏运行时） | **不能** | 动画、音频、UI、输入映射、存档**四件全无** |
| 替代 three.js editor（Web 内容/后处理生态） | **不能** | 后处理链固定三步、无 SSAO/DOF/TAA、无 IBL、wasm 11 MB |
| 替代 Houdini（程序化） | **不能** | 无节点图/程序化建模 |

**一句话**：它是"确定性刚体仿真内核 + agent 友好流水线"的**垂直切片**，不是一个通用 3D 内容创作软件。
差距**主要不在功能数量**，而在四类看不见的硬指标：色彩管理、无损往返、可复现构建、真机设备矩阵
（详见 [production_readiness.md](production_readiness.md) §4，其中"无损往返"本轮从**未过**推进到**编辑器侧已闭合**）。

---

## 2. 这一晚做了什么（每条都有可复核证据）

### 2.1 变换手柄（gizmo）—— 编辑器第一次能"直接拖"

* 代码：`crates/quench3d-editor/src/gizmo.rs`（新增，含注释与单元测试）
* 移动 / 旋转 / 缩放（`W`/`E`/`R`），世界 / 局部坐标系（`L`），工具栏可切换
* **屏幕定长 88 px**：贴脸和几公里外手柄一样大（世界长度由"投影一个单位轴"反推）
* 抓取容差 10 px，**手柄优先于物体拾取**；覆盖层不参与深度，被墙挡住的手柄照样能抓
* `Ctrl` 吸附（平移 0.25 / 旋转 15° / 缩放 0.1，作用于**整段拖拽的累计增量**），`Shift` 精细 ×0.1
* 一次拖拽 = **一步**历史；拖拽中 `Esc` 取消并还原；拖拽时清零刚体速度，避免"手里还被物理带走"

**验证（两条独立证据）**

```bash
# ① 纯几何/命中单元测试：不需要 GPU，任何机器都能跑
cargo test -p quench3d-editor --lib          # 7 个：命中优先级、相机后方不可抓、旋转按折线命中、
                                             # 吸附作用于累计量、平面 2×2 解算、正交基、点到线段
# ② 真机渲染（llvmpipe/Vulkan）逐像素统计
env -u WAYLAND_DISPLAY DISPLAY=:0 RUST_LOG=info ./target/debug/quench3d-editor /tmp/gizmo-verify.scene.json
#    → 截图中三根轴：X 红 163 px x[615,688]×y[534,560]；Y 绿 196 px x[599,610]×y[443,520]；
#      Z 蓝 194 px x[525,637]×y[501,570]；长度 ≈77–88 px，与 HANDLE_LEN_PX = 88 一致
# ③ 浏览器里同样渲染（headless Chrome + SwiftShader 截图，同一套判定）
#    → X 红 230 px、Y 绿 160 px、Z 蓝 175 px，三轴在选中物体处汇聚
```

**边界（不掩饰）**：不支持拖拽中键入数值、无轴心切换（只有物体原点）、**无多选成组变换**；
旋转手柄是真实投影椭圆折线，不是轨迹球。

### 2.2 撤销 / 重做 —— 手工操作的必要条件

* 代码：`crates/quench3d-editor/src/history.rs`（新增）+ `doc.rs` 的文档层接口
* 整场景快照式，上限 128 步；拖拽/滑杆整段合并为一步；`Ctrl+Z` / `Ctrl+Shift+Z` / `Ctrl+Y`
* **撤销不改变视角**；撤销后的场景与快照**逐字节相同**；新编辑清空重做链

```bash
cargo test -p quench3d-editor --test history   # 6 个：增删、变换、40 帧拖拽合并成 1 步、
                                               # 视角保持、逐字节一致、上限与重做链
```

**边界**：恢复的是场景内容（物体/变换/材质/刚体参数/灯光/相机文档字段），
**不**恢复物理步进状态（速度、睡眠、接触缓存）。

### 2.3 修掉一个真 bug：存盘会**静默丢段**

这是本轮最值得说的发现（也正是"生产可用"类问题）：

> 以前 `scene_to_json` 从 `SceneDoc::default()` 起头。于是
> **「CLI 生成带光照/渲染设置/模型的场景 → 编辑器改一改 → 存盘」**之后，
> `lighting`、`simulate`、`render`、`models`、`probes`，以及材质的 4 个贴图槽 + 7 个开关
> 会被**清零**——而文件看上去完全正常。这条链正是这个项目最核心的卖点。

修复方式：导出以**最近载入的原文档**为底稿，只覆盖编辑器真正管得着的字段
（物体、地面、材质标量、相机、重力、场景名）；材质按**同名物体**继承贴图与开关；
引用了已删除物体的关节/动作会被清掉并如实上报（否则文件自己读不回来）；撤销/重做会同步底稿。

```bash
cargo test -p quench3d-editor --test scene_roundtrip   # 4 个，其中两个是本轮新增：
#  ① 不建模的段必须保留 + 贴图引用/UV/法线强度不能丢 + 存→读→再存**字节相同**
#  ② 删掉物体后引用它的关节必须消失，且导出结果仍能读回来
```

### 2.4 光照编辑（场景级）

* 属性面板顶部新增「光照」折叠区：太阳方向/强度、投影开关、环境光、曝光、天空、参考网格
* 改动走同一套撤销历史（拖拽整段算一步）；**导出时从引擎写回**，所以"调完存盘"会生效
* 行为变更（有意）：导出会清掉 `lighting.preset` —— 预设会在载入时覆盖这些显式字段，留着等于"存了但不算数"
* 验证：`cargo test -p quench3d-editor --test scene_roundtrip`（4 个用例，其中
  "改了曝光/太阳强度必须出现在导出的 JSON 里" + "存→读→再存字节相同"直接钉住这条链）

### 2.5 工程卫生 + 一份"离生产还差什么"的评估

* CI / Pages 钉工具链 `@1.99.0`（原来是 `@stable`，等于每天换编译器）；所有 cargo 命令加 `--locked`
* 新增 `CHANGELOG.md`（含"未做"一节）、`CONTRIBUTING.md`（门禁命令、三条红线、验证配方）
* 新增 [production_readiness.md](production_readiness.md)：按**代码实测**逐域评估
  （渲染/光照/材质/场景图/编辑器/物理/资产/输出/平台/工程卫生 十张表，每行带 `文件:行`），
  六个使用场景的判定，以及四类"一票否决"的隐蔽硬指标 H1–H7
* 顺手更正了若干**文档与代码不一致**的地方（例如"没有点光源/没有实例化/没有 bloom"这类旧结论，
  经逐行核对后改为"有，且给出代码位置"）

**测试规模：337 → 354（0 失败）**；门禁全绿：`cargo fmt --check`、
`cargo clippy --workspace --all-targets -- -D warnings`、
`cargo clippy --workspace --target wasm32-unknown-unknown --all-targets -- -D warnings`、
`cargo test --workspace`、`tools/build_web.sh web_client` + `node tools/check_web.mjs`（wasm 11,370,401 字节）。

---

## 3. 为什么是这两块（而不是别的）

对标行业实践后，编辑器这条线上"缺得最明显、且用户每天都会碰到"的是两件事：
**没有手柄**（只能填数值）和**不能撤销**（点错一步只能重开）。它们挡住的不是"高级功能"，
而是"这个软件能不能被用来干活"本身。第三块（往返丢段）是**数据可靠性**问题——
功能少用户看得见、能绕过；数据被静默改坏，用户会在最不该出事的时候出事。

按同样的 ROI 排序，**下一批**最该做的是（已写进 [production_readiness.md](production_readiness.md) §6）：
多选/框选与层级、灯光与贴图面板、glTF 导入进文档层、场景格式迁移器、
单帧渲染的像素 golden 进 CI、真机 GPU 矩阵。

---

## 4. 这一晚**没有**做（逐条列出，避免误以为已有）

* 多选/框选、场景层级与父子变换、空物体、编组
* 灯光编辑面板、材质贴图槽界面（导出不丢，但**改不了**）
* glTF/OBJ 导入进编辑器的文档层（导入的模型仍只能作为静态物体，位置改动写不回 `models`）
* 动画、音频、UI、输入映射、存档（游戏运行时那一整块）
* 后处理扩展（SSAO/DOF/TAA/FXAA）、IBL、CSM、聚光灯、AO 贴图
* 场景格式迁移器、CLI 稳定错误码、SBOM / 许可审计、`cargo doc` 门禁
* 色彩管理（H1）、单位与 up-axis（H6）、确定性跨平台边界（H5）的正式声明文档
* **浏览器里"拖动某个轴并断言坐标变化"的端到端自动化验证**（见 §5 的诚实说明）

---

## 5. 诚实说明（三条必须写下来的）

1. **没能替代消费级生产软件**，也没做到"生产就绪"的全部门槛。本报告只声称：
   两条硬伤已修且有测试钉住、编辑器操作面补齐了两块、并且给出了一份可复核的差距评估。
2. **浏览器端到端拖拽验证没跑通**：Web 端"能起、能存出真实场景 JSON、手柄按设计渲染"都有证据；
   但本轮用的无头 Chrome 在重新加载后不再把 CDP 鼠标事件送进 winit
   （同一脚本在更早的实例上可以点到按钮并触发下载），**属于验证环境问题，但也没有排除应用侧原因**，
   因此按"未验证"记录。手柄命中/吸附本身由 7 个不需要 GPU 的单元测试覆盖，原生有逐像素证据。
3. **工具链取舍是刻意的**：CI 钉 `1.99.0`，但仓库**没有** `rust-toolchain.toml`
   （那会逼每个本地贡献者再下载一份工具链）。代价是"本地 stable 与 CI 不完全等价"。

---

## 6. 你早上怎么验收（5 分钟）

```bash
cd ~/Documents/infra-dev/quench3d
export CARGO_HOME=$PWD/../.cargo-home RUSTUP_HOME=$PWD/../.rustup PATH="$CARGO_HOME/bin:$PATH"

# ① 全套门禁（约 20 分钟；只想看结论可跳到 ②）
cargo fmt --all --check && \
cargo clippy --workspace --all-targets -- -D warnings && \
cargo test --workspace | tail -3 && \
tools/build_web.sh web_client && node tools/check_web.mjs

# ② 只看本轮新增的三块（约 1 分钟）
cargo test -p quench3d-editor --lib --test history --test scene_roundtrip

# ③ 眼睛验收：桌面上直接拖拽
cargo run -p quench3d-editor                    # 默认场景，球是选中的
#   左键拖轴箭头 = 移动；E = 旋转；R = 缩放；L 切世界/局部
#   Ctrl 吸附、Shift 精细、拖拽中 Esc 取消、Ctrl+Z 撤销、F 聚焦选中

# ④ 数据可靠性验收：带光照与贴图的场景，存一次再读一次
quench3d example demo > /tmp/demo.json && cargo run --release -p quench3d-editor -- /tmp/demo.json
#   在编辑器里随便改一改 → 保存 → 用编辑器再打开：光照与材质开关必须还在
#   （这条以前会静默清零；现在由 cargo test -p quench3d-editor --test scene_roundtrip 钉住）

# ⑤ Web 客户端
tools/build_web.sh web_client && python3 -m http.server 8231 --directory web
#   浏览器打开 http://127.0.0.1:8231/ —— 默认场景里能看到选中物体上的三轴手柄
```

CI：每次推送都会跑 4 个 job（ubuntu / macOS / Windows / wasm32）+ Pages 部署，
可在 <https://github.com/gh503/quench3d/actions> 查看；Pages 站点 <https://gh503.github.io/quench3d/>。

---

## 7. 如果你要我继续（下一晚的建议顺序）

1. **多选 + 层级**（一次拖一组、拖父带子）——手柄与撤销的地基已经就位，这是最自然的下一块；
2. **灯光与贴图面板**（能改，而不只是"不丢"）+ 单帧渲染的**像素 golden** 进 CI；
3. **场景格式迁移器**（版本升级路径）+ CLI 稳定错误码；
4. **H1/H5/H6 三份声明文档**（色彩管理、确定性边界、单位与 up-axis）——
   不需要写渲染代码，但它们是"能不能进生产"的一票否决项；
5. glTF 导入进文档层（导入的模型可编辑、可写回）。


---

## 8. 第 2–6 轮的增量（补充上文的初始版本）

第一版报告写于第 1 轮；之后又推进了 4 项并各自跑过门禁与 CI。**以本节为准**。

| 轮 | 内容 | 证据 | 提交 / CI |
|---|---|---|---|
| 2 | 差距分析补齐 **14 个域**：新增「建模 / 脚本与扩展 / 性能 / 互操作」四张表（每域含：行业门槛、现状证据（源码命中）、验收标准、投入量级、能否替代） | `docs/production_readiness.md` §1.11–1.14；关键证据：`csg/extrude/uv_unwrap/modifier/sculpt/bevel` 全仓 0 命中、`rhai/lua/wasmtime/plugin` 全仓 0 命中、无模型导出代码 | `5f8a080` CI ✅ |
| 3 | **材质贴图数据层**：`doc::TexturePaths` 按物体名记录 4 条贴图路径 + uv/normal/alpha/unlit/双面；导出时显式覆盖优先于"同名继承"；载入真的生效且**不串物体** | `cargo test -p quench3d-editor --test scene_roundtrip`（造真贴图 + 两个物体互不影响） | `d08bb02` CI ✅ Pages ✅ |
| 4 | 把"贴图界面"所需的 API 落点核清并写进仓库（含**真实阻塞点**：没有 `material_mut`、贴图 handle 烘在绑定组里、需要 `GpuContext`），避免下次重翻代码 | `docs/production_readiness.md` §8 | `265bbcc` CI ✅ |
| 5 | **材质缩放/开关面板**：UV 平铺 / 法线强度 / alpha 裁剪 / alpha 混合 / 无光照 / 双面，改的就是引擎材质（视口立刻变），并写进 `doc.textures` → 可撤销、可存盘 | `scene_roundtrip::update_material_changes_the_engine_material` | `230a5fa` CI ✅ Pages ✅ |

**当前测试总数 354（0 失败）**；本会话共推送 6 个提交（`15f906e` → `230a5fa`），
除最早一次因"钉工具链后未显式装 clippy"变红（已修复并复跑绿）外，其余 CI 全绿。

| 7 | **运行时换贴图**：渲染器新增 `set_material_texture` + `MaterialTextureSlot`（把绑定组构造抽成 `build_material_bind_group` —— 贴图句柄烘在绑定组里，换图必须重建）；编辑器"应用贴图（从磁盘读取）"；无效贴图 id 返回 false 而不是静默用错图 | `scene_roundtrip::set_material_texture_swaps_and_rejects_bad_ids` | `12ab634` CI ✅ Pages ✅ |
| 8 | **四个贴图槽全部接通**（基础色/法线/金属度-粗糙度/自发光），按槽位区分 sRGB（颜色贴图 vs 数据贴图）；Web 端按钮不出现并给提示，两条 cfg 分支都过 clippy | 门禁 fmt/clippy(native+wasm)/355 测试/wasm+check_web 全绿 | `6da3468` CI ✅ Pages ✅ |

**仍然没有做的**（按优先级，与 §4 的清单合并后最新的四项）：
1. glTF/OBJ 导入进**场景树/文档层**（现在导入的模型在编辑器里位置改动写不回 `models`）；
2. 场景**层级/父子**与场景格式 v2；
3. 后处理 FXAA/SSAO；
4. 物理碰撞过滤（层/掩码）、传感器事件、CCD。

已闭环：编辑器撤销重做 ✅ / 变换 gizmo ✅ / 灯光面板 ✅ / 材质贴图编辑（数据层 + 缩放开关 + 四槽换贴图）✅。

---

## 9. 已知问题（必须知道的两条）

1. **偶发测试**：`crates/quench3d-cli/tests/agent_tools.rs::agent_handles_multiple_tools_in_one_turn`
   在 **macOS** 上偶发失败（本地 mock HTTP 服务与客户端之间的竞态；同提交的 ubuntu / windows /
   wasm32 全绿，`gh run rerun --failed` 后转绿）。看到它红请先复跑再判断 —— 详见 `CHANGELOG.md` 的「已知问题」。
2. **Web 端仍需真机验证**：本会话所有浏览器验证跑在 headless Chrome + SwiftShader（`backend: "gl"`，
   即 WebGL2 回退），**真实设备的 WebGPU 路径没有验证过**（H4）。

---

## 10. 第 13–14 轮的增量

| 轮 | 内容 | 证据 | 提交 / CI |
|---|---|---|---|
| 11 | `tools/gate.sh`：一条命令跑完整门禁（fmt → clippy 原生 → clippy wasm32 → 全部测试 → wasm 构建 → `check_web.mjs`），顺序固定、日志落盘、失败自动摘要；`--fast` 跳过 wasm 两项 | 直接跑：全绿 | `ed8ec30` CI ✅（首次 macOS 红，见下） |
| 12 | 记录两条已知问题：macOS 偶发 flaky 测试、Web 端只有 SwiftShader 验证过 | `CHANGELOG.md` 「已知问题」+ 本文件 §9 | `4b0d88f` CI ✅ |
| 13 | **修掉那个 flaky**：假 LLM 服务在非阻塞 `accept()` 遇到 `Interrupted`/`ConnectionAborted` 时直接 `break`，服务线程中途死掉；改为可重试 | 本地连续 5/5 通过；**真正的验证是 macOS CI 连续绿**（目前 1 次） | `cacb14c` CI ✅ |
| 14 | **模型（glTF/OBJ 引用）变换面板**：位置/旋转/缩放写回导出底稿的 `models[i]` —— 补掉"导入模型的变换写不回场景文件" | `scene_roundtrip::model_transform_edits_are_exported`（真 OBJ；未改动不凭空多值；改后进文件；重载后值还在） | `d221f0e` + `3ae8c2a`(fmt 补救) CI ✅ Pages ✅ |

**当前测试总数 356（0 失败）**；本会话累计 14 个提交。
**一个流程教训（已修）**：`tools/gate.sh | tail` 会让管道吞掉退出码 —— 我第一次就这么推了一个 fmt 没过的提交，
下一提交修好；看门禁请 `tools/gate.sh > log 2>&1; echo $?`，别接管道。

**② 剩余的三组半**：① 模型引用接进场景树 + 视口实时拖动（需要"物体↔模型"映射）；② 场景层级/父子与格式 v2；
③ 后处理 FXAA/SSAO；④ 物理碰撞过滤/传感器事件/CCD。
