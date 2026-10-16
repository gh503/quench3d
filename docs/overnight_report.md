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
| 替代 three.js editor（Web 内容/后处理生态） | **不能** | 后处理链可选（SSAO + bloom + FXAA，无去噪）、无 DOF/TAA/CSM/IBL、wasm 11 MB |
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
* 后处理扩展（DOF/TAA、AO 去噪）、IBL、CSM、聚光灯、AO 贴图
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

**仍然没有做的**（按优先级，与 §4 的清单合并后最新的四项）——**四项已全部闭环**，见 §17：
1. ~~glTF/OBJ 导入进**场景树/文档层**~~ ✅（第 15 轮，§12）；那行"位置改动写不回 `models`"
   的描述当时就已过时：§10 第 14 项（`d221f0e`）先把面板写回做了，第 15 轮又补了视口拖动联动；
2. ~~场景**层级/父子**与场景格式 v2~~ ✅（第 16 轮，§13）；
3. ~~后处理 FXAA/SSAO~~ ✅（第 17 轮，见 §14 与 §15）；
4. ~~物理碰撞过滤（层/掩码）、传感器事件、CCD~~ ✅（第 18 轮，见 §16）。

已闭环：编辑器撤销重做 ✅ / 变换 gizmo ✅ / 灯光面板 ✅ / 材质贴图编辑（数据层 + 缩放开关 + 四槽换贴图）✅。

---

## 9. 已知问题

1. ~~**macOS 偶发 flaky（假 LLM 服务）**~~ **已修（第 16 轮）**：
   `agent_tools.rs::agent_handles_multiple_tools_in_one_turn` 与
   `llm_mock.rs::chat_reads_content_length_response_and_sends_expected_request`
   都在这条线上偶发红（同提交的 ubuntu / windows / wasm32 全绿，`gh run rerun --failed` 曾转绿）。
   根因是 BSD / macOS 的 `accept()` 会**继承** listener 的 `O_NONBLOCK`（Linux 不继承），
   假服务的 `set_read_timeout` 因此不生效：请求字节没到时立刻 `WouldBlock` → 当成"没请求"
   关连接 → 客户端读到 `Connection reset by peer (os error 54)`。
   修法是 accept 之后**显式设回阻塞**（`crates/quench3d-cli/tests/common/mod.rs`）。
   验证方式是 **macOS CI 连续 3 次绿**（`38835476605` 原始跑 + 该 run 的 macOS 复跑 +
   `38835998073` 全平台，四个 job 全 success；本地是 Linux，复现不出来，
   只能验"没改坏"：`llm_mock` + `agent_tools` 连续 5 轮通过）。
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

**剩余清单 → 第 16 轮之后剩两组**：① ~~模型引用接进场景树 + 视口实时拖动~~（第 15 轮，§12）；
② ~~场景层级/父子与格式 v2~~（第 16 轮，§13）；③ ~~后处理 FXAA/SSAO~~（第 17 轮，§14/§15）；
④ ~~物理碰撞过滤/传感器事件/CCD~~（第 18 轮，§16）。

---

## 11. CLI 三连（本轮实测过，用法与输出都是真的）

命令行参数是**位置参数**，没有 `--scene`（`--help` 也不支持，用法靠错误提示给出）。实测：

```bash
# ① 生成示例场景（stdout，1503 字节）
./target/debug/quench3d example demo > /tmp/demo.json

# ② 校验
./target/debug/quench3d validate /tmp/demo.json
#   → 场景合法（0 条警告）

# ③ 出一帧 PNG（注意：场景路径是位置参数）
./target/debug/quench3d render /tmp/demo.json --out /tmp/demo.png
#   → 相机位置  : [7.0, 4.5, 8.0]
#      耗时      : 2372 ms
#      物理      : 180 步，5 个刚体，最大速度 0.00 m/s，NaN 0
#      /tmp/demo.png = 313,313 字节

# ④ 出视频（帧序列 + 内置 GIF/APNG；不调用 ffmpeg）
./target/debug/quench3d movie /tmp/demo.json --frames 120 --frames-dir frames
```

顺带一条**验收要点**：`render` 的输出里有"NaN 0"这一项 —— 用它比看画面更快发现物理炸掉。

---

## 12. 第 15 轮：导入模型接进场景树 + 视口实时拖动（§8.1 的工作项）

`docs/production_readiness.md` §8.1 里记下的卡点是"模型展开后**不给调用方物体下标**"，
于是编辑器只能改文件里的 `models[i]`、等重开才生效。这一轮把它补上，
并且**反过来也接**：在视口里拖模型 = 整组联动 + 写回 `models[i]`。

| # | 内容 | 落点 / 证据 |
|---|---|---|
| 1 | `runner::ModelInstance`；`setup_visuals` 返回映射（`models[i]` → 物体下标） | `crates/quench3d-cli/src/runner.rs`；直接用 `engine.load_model` 的返回值，不靠"前后各取一次长度"猜 |
| 2 | `scene_io::Installed` 把映射带进 `Doc`；**撤销/重做也带**（`history::Restored`） | 快照是整场景重建，物体增删会让下标整体挪动；不刷新映射就会拖到别的物体身上 |
| 3 | `Doc`：`apply_model_transform`（模型→物体）、`apply_transform_to_model_group`（物体→模型）、`forget_object`（删除后的下标维护） | `crates/quench3d-editor/src/doc.rs` |
| 4 | 视口拖动：整组联动 + 写回 `models[i]`；`Esc` 整组还原；模型成员缩放强制走等比 | `crates/quench3d-editor/src/gizmo.rs` |
| 5 | 模型面板改变换**当场生效**；选中模型子网格时属性面板的变换改动走同一条路 | `crates/quench3d-editor/src/ui.rs` |
| 6 | 场景树按 `模型 #i · 路径` 分组（成员可点选/聚焦，标题上有"整组聚焦"） | `crates/quench3d-editor/src/ui.rs` |

**验证**：`cargo test -p quench3d-editor --test model_instances` —— 5 个用例（无 GPU 自动跳过）：
两 mesh OBJ 的映射长度与下标、面板改变换**当场**作用到引擎物体、
拖动整组联动并写回 `models` 且存盘重开后一致、删掉前面的物体后映射下标整体前移、
撤销后模型变换与映射都刷新。
**当前测试总数 361（0 失败）**；`tools/gate.sh` 全绿（fmt / clippy 原生 / clippy wasm32 /
361 测试 / wasm 构建 11,415,654 字节 / `check_web.mjs`）。

**没有做（别误以为有）**：多选还没做，"成组"目前只有"模型实例"这一个来源；
不能单独挪模型的某个子网格；非均匀缩放写不回格式（`models[i].scale` 是一个数）。

---

## 13. 第 16 轮：场景层级（父子变换）+ 场景格式 v2

这一轮把"组合体"的地基放进去：物体可以有**父物体**，于是 `position` / `rotation_deg`
变成相对父物体的**局部**量，拖动父物体时子物体自动跟着走。格式版本升到 2。

| # | 内容 | 落点 / 证据 |
|---|---|---|
| 1 | 引擎：`SceneObject::parent` + 世界变换现算（`world_transform` / `set_world_transform` / `set_parent` / `is_ancestor` / `children_of` / `remove`）；绘制、包围盒、拾取、物理同步全走世界变换 | `crates/quench3d/src/scene.rs`；5 个纯逻辑用例（**不需要显卡**，CI 上也会真跑） |
| 2 | 格式：`objects[].parent`（用**名字**引用）+ `VERSION = 2`；校验器拦悬空父名、自为空、成环、带刚体挂父物体 | `crates/quench3d-cli/src/scene_doc.rs`；7 个格式用例（含"一个环只报一条诊断"） |
| 3 | CLI 装载：两趟连线（先建物体，再按"文档下标 → 场景物体下标"接父链） | `crates/quench3d-cli/src/runner.rs`；父物体可以排在子物体后面 |
| 4 | 编辑器：场景树缩进显示层级；属性面板"父物体"下拉改父（**保世界位姿**）；gizmo 在世界空间算、写回时换算成局部 | `ui.rs` / `doc.rs`（`Doc::set_parent`）/ `gizmo.rs`；7 个集成用例 |
| 5 | 导出：写局部变换 + `parent` 名字；父物体没被写成物体（地面、模型网格）就丢掉关系并**上报** | `scene_io.rs`；悬空 `parent` 会让整个文件读不回来 |
| 6 | 缩放语义：子物体**只继承位置与朝向，不继承缩放** | `scene::rigid_only`；格式文档 / schema / 系统提示都写明了理由 |

**顺带修掉一个真 bug**：`to_euler_xyz_deg` 在万向锁（绕 Y ±90°）时用错了矩阵行，
把旋转反解成"另一个 90°"。单看该物体看不出来（都是转了 90°），但子物体会跑到别处 ——
这正是给层级写集成测试时顺出来的（父物体错一点，子物体错一大截）。
已加穷举角度的回归测试 `euler_extraction_round_trips_every_axis`。

**验证**：`cargo test -p quench3d --lib`（5 个，纯逻辑）/ `cargo test -p quench3d-cli --lib`（7 个）/
`cargo test -p quench3d-editor --test hierarchy`（7 个，无 GPU 自动跳过）/
`cargo test -p quench3d-cli --test scene_fixtures`（2 个：外部夹具版本对齐）/
`cargo test -p quench3d-math`（`child_of` 与 `mul_transform` 互为逆）。
**当前测试总数 385（0 失败）**；`tools/gate.sh` 全绿（fmt / clippy 原生 / clippy wasm32 /
385 测试 / wasm 构建 **11,459,738 字节**（比上一轮 +44 KB）/ `check_web.mjs`）。

**没有做（别误以为有）**：场景树里拖放改父（只有下拉框）；多选成组变换；空物体（没有网格的节点）；
Prefab / 实例（同一个模型引用两次会展开成两批互不相干的物体）；版本 1 的迁移器（**刻意不做**）。

**这一轮顺带挖出了那个"偶发 flaky"的真根因**：BSD / macOS 的 `accept()` 会继承
listener 的 `O_NONBLOCK`（Linux 不继承）—— 假 LLM 服务为了 accept 超时把 listener 设成
非阻塞，于是 macOS 上连进来的 socket 也是非阻塞，`set_read_timeout` 无效，请求字节没到就
当成"对端没发请求"关连接，客户端读到 RST。修法是 accept 之后显式设回阻塞；
macOS CI 连续 3 次绿（含一次全平台 dispatch 跑）。

**这一轮的一个反面教材（已变成机制）**：格式升到 v2 时，仓库里两份**写在代码之外**的场景夹具
（`.github/workflows/ci.yml` 的冒烟测试、`tools/mock_llm_server.py` 的假场景）还是 v1 ——
本地门禁全绿，CI 的 ubuntu 与 macOS 直接红。Rust 里的夹具会当场被本机测试抓出来，
这两份不在任何 crate 的测试路径里。补救不是"下次记得改"，而是加了一条测试
（`cargo test -p quench3d-cli --test scene_fixtures`）：把这类外部夹具列进名单，
断言它们的版本号等于 `scene_doc::VERSION` —— 故意改回 v1 会立刻变红（已实测）。

---

## 14. 第 17 轮：后处理 FXAA（剩余项 ③ 的一半）

**做了什么**：`lighting.fxaa`（不写 = 关闭，写 `{}` = 打开）→ CLI、`validate`、编辑器视口面板
都能开。算法是 FXAA 3.11 的 console 档（4 个对角取样估边缘方向 → 沿方向两次混合 →
亮度越界回退），实现落在 `crates/quench3d-render/src/shaders/fxaa.wgsl`。

| # | 决定 | 理由 / 落点 |
| --- | --- | --- |
| 1 | 放在 tonemap **之后**，而不是在线性 HDR 上做 | FXAA 的阈值是 0~1 的**感知**亮度；线性 HDR 里太阳是 8~20，任何阈值都失效 |
| 2 | 中间目标用 `rgba16float`，不用 8 位 sRGB 贴图 | 8 位 sRGB 要"先编码再解码"才能混合，混合处会多一次量化；f16 线性存显示值，写 sRGB 目标时由硬件编码一次 |
| 3 | **边缘判断在 sRGB 空间、混合在线性空间** | 边缘是感知现象（判断要在 sRGB），混合是光强运算（要在线性）；与 FXAA 原版"整条都在 gamma 空间"有细微差别，已写在 `fxaa.wgsl` 顶部 |
| 4 | 关闭时**不建** LDR 目标、**不跑**那一趟 | 保证"不写 = 像素与加这个功能之前逐位相同"这条项目惯例 |
| 5 | 多了一条 `post_ldr_pipeline`（同着色器、不同颜色目标格式） | wgpu 的管线与附件格式绑定：写 LDR 中间目标与写 sRGB 目标必须是两条管线（第一次跑测试就撞上了这个校验） |
| 6 | 编辑器的开关放在视口面板（网格/阴影旁边） | `renderer.options.fxaa.enabled` 本来就在 `Engine` 上，3 行代码；与 `render.msaa` 分开，互不替代 |

**验证（这一轮的重点）**：

* `cargo test -p quench3d-render --test post_pipeline` —— 新增 2 个**像素级**用例
  （本机有 GPU，真跑；无 GPU 的 CI 会打印"跳过"）：
  ① `fxaa_smooths_edges_and_leaves_flat_areas_alone`：msaa=1 + 无光照材质下，
  **关掉时中间值必须为 0**（硬边），打开后**必须 ≥ 8 个中间值**，同时
  物体内部 ±6 像素与四角背景**逐位不变**——抗锯齿最常见的失败是"整幅画糊了一层"，
  这条断言就是钉这个的；
  ② `fxaa_threshold_actually_gates_the_filter`：阈值调到 100（不可能触发）后，
  整幅图必须与关闭时逐位一致——防"阈值读了但没进判断"。
* `cargo test -p quench3d-cli --lib fxaa`：格式层 6 条断言（不写就不出现在序列化结果里、
  空对象 = 打开、负阈值 error、>1 warning、显式 `enabled:false` 不再警告、字段拼错报未知字段）。
* **端到端 + 逐像素比对**：同一个 480×320 场景开/关各渲一张
  （`target/fxaa-demo/{on,off}.png`，命令见 CHANGELOG），
  比对结果：**3.03% 的像素被改动，其中只有 3 个**离边缘 2 像素以外（3×3 邻域梯度 < 12），
  改动幅度中位数 4/255、最大 118（出现在最硬的轮廓上）。
  也就是说它确实"只动边缘"，不是全局模糊。

**没有做（别误以为有）**：FXAA 高质量档的子像素锯齿消除与 3×3 全邻域方向估计；
TAA；SSAO（③ 的另一半，下一轮）；编辑器的 MSAA 切换（MSAA 仍然只能在场景文件里改）。

---

## 15. 第 17 轮（下半）：后处理 SSAO —— 剩余项 ③ 至此做完

**做了什么**：`lighting.ssao`（不写 = 关闭，写 `{}` = 打开），参数 `radius`（米）/`bias`/
`strength`/`samples`；CLI、`validate`、编辑器视口面板都能开。让物体与地面的接触缝隙、墙角变暗。

| # | 决定 | 理由 / 落点 |
| --- | --- | --- |
| 1 | **深度-only**（不加法线缓冲） | 法线由视空间位置的屏幕导数重建；少一张全屏 RT、少一趟几何 pass |
| 2 | 算在 **post 那一趟里**（tonemap 之前），不做单独的 AO 目标 | 少一张 RT 与一次读写；代价是没法对 AO 做双边模糊（见"没做"） |
| 3 | 遮挡判据 `bias < p.z - q.z < radius` 的**上界** | 只比"谁更近"会让掠射角下的同一个平面自己遮自己：实测远处平坦地面平均暗 3.5/255；加上界后 0.45 |
| 4 | 不用"把偏移投影到法线上"这个更漂亮的判据 | 它依赖重建法线，而掠射角下法线误差最大——实测反而恶化到 11/255。**越"正确"的公式越吃误差** |
| 5 | 旋转角用**交错梯度噪声**而不是白噪声 hash | 同样 0 成本；实测远处地面暗度标准差 2.89 → 2.32、接触区 4.55 → 4.16，颗粒感明显更小 |
| 6 | 深度格式 `Depth24Plus` → `Depth32Float` | 前者**不可采样**；SSAO 必须按像素读深度 |
| 7 | MSAA > 1 时加一趟深度解析 | 多采样深度不能当单采样纹理用；取**第 0 号采样**而不是平均（平均深度在几何边缘上是虚构值） |
| 8 | 后处理绑定组常驻一个 1×1 占位深度 | 布局固定；MSAA > 1 而 SSAO 关着时，主 pass 的深度视图是多采样的，不能绑上去 |
| 9 | 编辑器勾选必须当场生效 | `ensure_targets` 的提前返回条件里加了"SSAO 深度是否已就绪"，否则运行时勾选不会重建目标 |

**验证**：
* `cargo test -p quench3d-render --test post_pipeline ssao`（**msaa = 4**，因此这条用例同时是
  "深度解析那一趟"的唯一自动化覆盖）：接缝窗口平均暗 **7.4**、最暗像素暗 **30** 且离接缝
  **5.7 像素**、远离遮挡物的平坦地面只暗 **0.45**、天空**逐位不变**（远平面提前退出）。
* `cargo test -p quench3d-cli --lib ssao`：半径非正/采样数越界 = error；半径过大、偏移 ≥ 半径、
  强度 > 1 = warning；字段拼错报未知字段。
* 端到端 `target/ssao-demo/`（480×320，同一场景开/关 + 全开 SSAO+FXAA+bloom+MSAA4）逐像素统计：
  天空带均值暗 **0.03**（标准差 0.48）、中部接触区均值暗 **3.25**（极值 51）、
  改动 > 1 的像素 34%。全开那张也验证了整条链（深度解析 → bloom → post(SSAO) → FXAA）无校验错误。

**没做（诚实边界）**：AO 没有单独目标与**双边模糊去噪**（接触处有轻微颗粒，靠 16 抽样 + IGN 压着，
不是"干净"的工业级 SSAO）；没有 TAA / 景深 / CSM / IBL；SSAO 是屏幕空间的，
屏幕外与背面几何不参与（也就是没有"接触阴影"那种离屏保守性）；
远处平坦地面在掠射角下仍会暗 1 个色阶左右（已知边界，README §9 已写）。

---

## 16. 第 18 轮：物理碰撞过滤 / 传感器事件 / CCD —— 剩余项 ④

**做了什么**：`objects[].collision_layer` + `collides_with`（层/掩码）、`objects[].sensor`
（触发区，报进入/离开、不产生冲量）、`objects[].ccd`（高速物体不穿薄墙）。
三项都能配、能校验、CLI 的 `simulate` 报告能验证、编辑器不丢字段。

| # | 决定 | 理由 / 落点 |
| --- | --- | --- |
| 1 | 过滤放在**宽相**而不是窄相 | 不配对的一对连候选对都不产生；"过滤掉 90% 物体"于是真的省了窄相开销 |
| 2 | 判据**对称**（双方都愿意才碰） | 单向平台这类需求靠"上层单向过滤"表达，而不是靠"一边说了算"——后者在多人/AI 混用时会变成玄学 |
| 3 | 缺省 `memberships` 是**第 0 层**而不是 `ALL` | 一开始写成 `ALL`，结果"只想碰第 5 层的墙"把缺省物体当成同类（测试当场抓住）。`ALL` 要显式用 `InteractionGroups::all()` |
| 4 | 传感器对**不进 `manifolds`** | 求解器、热启动、island 划分、并行路径全都不用加分支；`contacts` 与事件照旧 |
| 5 | 传感器**不唤醒睡眠体** | 一个覆盖半个场景的触发区不该每个子步把整个世界叫醒（代价换语义，写进文档） |
| 6 | 事件是**每步轮询**（`sensor_events()`），不做回调总线 | 与"确定性、可回放"一致：事件只是这一子步的差分结果，没有隐藏状态；报告也是从它推出来的 |
| 7 | CCD 用**中心射线 + 只截法向位移** | 比"全局子步细分"便宜得多（每子步每个 CCD 刚体一次射线，与速度无关），且不像子步细分那样在极端速度下退化成慢动作 |
| 8 | CCD 查询**排除自己 / 分组不匹配 / 传感器** | 第一版忘了查分组，测试里子弹被"看不见"的墙拦住了 |
| 9 | 先写的是**保守推进扫掠**，被"贴着地面滑行的子弹"证伪后换成中心射线 | 地面永远在正下方、永远是"最近物体"，扫掠每步只推进地面距离；而且它返回的命中是地面（法线与运动垂直，不该拦）——加地面之后子弹照样穿墙。中心射线只报**沿运动方向**的命中，一次查询就够；代价是只扫中心线（擦角可能漏），写进边界 |
| 9 | 编辑器不做编辑界面，但导出**从刚体读回**这四个字段 | "载入 → 保存"不会静默丢数据（往返用例钉住）；F5 接触点里传感器画青色 |

**验证**：
* `cargo test -p quench3d-physics --lib`（**不需要显卡**，7 条）：分组对称匹配与不匹配穿透、
  传感器进入/离开且 `impulse == 0`、传感器不唤醒睡眠体、CCD 停住 600 m/s 的小球、
  CCD 遵守分组（子弹穿过"只想碰第 5 层"的墙）、**贴地面滑行的 CCD 子弹仍被薄墙拦住**。
* `cargo test -p quench3d-cli --test cli_e2e simulate_reports_sensors`：端到端跑二进制，
  报告里确实有 `sensors[]`（`enter_step`/`exit_step`/`steps_inside`/`entries`），落体穿过
  触发区继续下落，同一颗子弹**不开 CCD 一步跨过薄墙**、**开了 CCD 停在墙前**；
  `render --dry-run` 与 `movie --dry-run` 两条路径的报告也带上了同一个观测
  （报告是三条路各写一遍的，只接一条就等于"换个命令就看不见了"）。
* `cargo test -p quench3d-cli --lib collision_filter`：格式与校验（层号越界/重复/空掩码、
  传感器缺刚体是 error、密度/摩擦对传感器是 warning、CCD 非 dynamic 是 warning、拼错字段报错）。
* `cargo test -p quench3d-editor --test hierarchy export_preserves`：存盘往返后
  `sensor` / `collision_layer` / `collides_with` / `ccd` 都还在。

**没做（诚实边界）**：没有回调/事件总线（每步轮询；要在事件里改世界得自己在循环里做）；
传感器不唤醒睡眠体，所以"睡着的物体被别人搬出触发区"不会报离开；
碰撞过滤只作用于物理，射线/拾取/`overlap` 查询不看分组；
CCD 只扫**中心线**（球擦着墙角过去、或细长物旋转扫过薄壁都可能漏），动态×动态的 CCD 只有
"两边都开才互相拦"的语义，且极快 + 极薄（一步能跨过整个物体）仍可能漏。

---

## 31. 第 32 轮：rustdoc 进门禁与 CI（gap #30 的一部分）

门禁原本六段（fmt / clippy 原生 / clippy wasm / 测试 / wasm 构建 / web 产物检查），
这一轮加了第七段 `PASS doc`（`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace`），
CI 里同样加了一步。理由很直接：**rustdoc 告警是唯一"没人看就永远没人看"的一类**——
`cargo test`/`clippy` 都会碰到代码，文档链接不会。

代价是先还债：既有 16 处告警，全是「公开文档链到私有项」（`lib.rs` 的模块清单）与
「字面量被当成链接」（`[x,y,z]`）。修法是**改成纯代码字体**，不是
`#[allow(rustdoc::private_intra_doc_links)]`——那种掩法等于把这段门禁废掉。

`--locked` 与 MSRV 声明早就齐了（CI 每条 cargo 命令都带 `--locked`，Cargo.toml 有
`rust-version = "1.90"`），所以 #30 只剩「在 1.90 上真的跑一次 MSRV 构建」与两处措辞核对
没做——都写进了 gap 行，没有含糊过去。

**验证**：`tools/gate.sh` 七段全绿（448 测试 0 失败、wasm 11,742,106 字节、`PASS doc`）。

## 30. 第 31 轮：相机参数核对（gap #16 收窄成"只差多相机"）

用第 29 轮定下的四问核对 gap #16：**正交投影**不是"格式里有字段"而已——
`camera.orthographic_height` 真的被交给 `Projection::Orthographic`（`orthographic_rh`），
`near`/`far` 进投影矩阵，`fov_deg`/`distance`/`yaw_deg`/`pitch_deg`/`frame_scene`
也都齐；e2e `camera_clip_planes_and_orthographic_projection_are_wired` 同时钉住了
"参数被采纳"和"`near > far` 要报错"两条路径。所以这一行从"一堆待做"收窄成**只差多相机/多输出**。

**为什么不当场把多相机做了**：它不是加个字段——要同时动场景格式（`cameras[]` 或
`render.outputs[]`，还要跟现有单个 `camera` 保持向后兼容）、渲染循环（每台相机各渲一遍、
输出命名）、CLI 报告（多份 output/digest）三处，每一处都要有测试。塞进核对轮会变成
"改一半"，比不做更糟。收窄 + 写清三处改动点，下一轮单独做。

**验证**：本轮只动文档，仍跑满六段门禁（448 测试 0 失败、wasm 11,742,106 字节）。

## 29. 第 30 轮：接触余量（gap #20 第二条腿）

接触余量（speculative margin）解决的是"高速物体在还没接触的那一帧里直接穿进去半个身位"：
把窄相的形状沿法线膨胀 `margin`，穿透量减掉 `margin`（于是得到**负穿透** = 还差多少贴上），
求解器对负穿透的接触按 `separation/dt` 给法向速度目标——物体可以在"即将贴上"时就被约束住，
而不是等穿进去了才被弹出来。位置纠正那边不用改：它本来就 `pen <= slop` 就跳过，负穿透天然不推。

两个工程细节值得记：
* **形状膨胀做成 `Shape::inflated(margin)`**，且 `margin <= 0` 时原样返回——缺省路径连一次
  分支都没有，这是"缺省必须逐位不变"的实现方式。
* **必须给"负穿透"加一道门**：窄相本来就会给出少量负穿透的接触（贴而不叠），如果无条件
  按 `separation/dt` 放宽，整场结果会漂。第一版就踩了这个坑，`digest_tests` 两条当场变红，
  加上 `cfg.speculative_margin > 0.0` 这个前提才恢复。这是"golden digest 值得留"的最好例子。

**没做（诚实边界）**：只对单形状刚体生效（复合形状要逐部件膨胀 + 重算局部质心/聚合 AABB，
收益小）；平面/凸包/三角网不膨胀；没有数值范围校验（`physics.speculative_margin` 写 5 米
会全场互撞，只靠文档约束）；没有专门的 e2e 用例，回归靠物理套件 + digest 不变。

**验证**：`cargo test -p quench3d-physics` → 153 条全绿（含 `digest_tests` 的
"缺省行为逐位不变"），新增 `speculative_margin_defaults_off_and_inflates_shapes`。

## 28. 第 29 轮：核对轮（gap #15 / #18 从"待做"改成 ✅）

这一轮不改代码，专做**核对**——这两项在最近实现里"看起来已经做了"，而"看起来做了"
和"真的闭环"是两件事。判据是四问 + 一问：**场景格式有字段吗？校验管它吗？引擎真的
执行吗？CLI/编辑器能到吗？有测试钉住吗？**

* **#15 `actions[]` + `probes[]`**：正式字段；动作按 `at_step`/`at_time` 到点施力；
  探针**只读**记录进出区域（这点关键——探针不该改变被测实验）；render/movie/simulate
  三条路径都能用；编辑器保留 `probes` 并在删物体时清掉指向它的动作（否则留悬空引用）。
  **边界**：探针没有可视化绘制；动作没有时间线 UI，只能手写 JSON。
* **#18 物理参数透出**：物体级 `velocity`/`angular_velocity`/`linear_damping`/
  `angular_damping`/`gravity_scale`/`mass`/`allow_sleep`，世界级合成规则 +
  `static_friction_speed`。**边界**：`mass` 与 `density` 同写时密度优先；没有
  `sleep_threshold`/`sleep_time`（要透出得先想清楚"两个物体阈值不同"怎么合并）。

**为什么值得单开一轮**：把做完的事标成"待做"、把没做完的事标成"已完成"，是同一种错误
的两个方向——前者让人重复劳动，后者让人踩空。核对完这两行，剩下的 🟡/待做就是真没做
（#16 相机、#20 余量、#23 动画、#24 CSM/点光阴影、#26 IBL、#27 模型缓存）。

**验证**：这轮只改文档与 CHANGELOG，仍然跑满六段门禁：**447 测试 0 失败**、
wasm 11,739,868 字节，六段全绿。

## 27. 第 28 轮：接触特征 ID + 热启动精确匹配（gap #20，🟡 部分）

热启动（warm starting）本来就有：求解器把每步的累积法向/切向冲量存进 `ContactCache`，
下一步作为初始猜测。**问题在"怎么认得出这是同一个接触点"**——原来是拿两端局部坐标
比距离（2 cm 容差）。物体会滑动、会转，2 cm 容差于是会把冲量沿用到**相邻的另一个**
特征点上：慢速滑动的箱子因此忽软忽硬（真实的物理引擎里这是个经典 bug 来源）。

这一轮做的就是把"认点"这件事从**容差猜**换成**身份对**：接触点带一个 `feature_id`，
由两端局部坐标按 0.5 mm 量化后哈希得到。局部坐标随刚体运动，所以同一个几何特征连续帧
得到同一个 ID；与接触点的顺序、新增/消失无关。缓存查表**优先按 ID 精确匹配**，
匹配不上才退回旧的容差逻辑（保守：行为只会变好，不会突然"匹配不上"）。

工程上有两条值得记的：
* **打标只在一个出口做**：包一层 `collide_bodies_raw`，在唯一的返回点给所有形状对的
  接触点打 ID。窄相有 20 多个分支，逐个改必然漏。
* **两边必须用同一个哈希**：宽相打的 ID 就是求解器查表的键，各写一份迟早会漂。
  所以抽成一个 `pub(crate) fn feature_id_from_local`，两边都调它。

**没做**：接触余量（speculative margin）——分离的物体提前生成接触点，需要窄相给出
"最近特征距离"并按 `separation/dt` 给速度目标，是窄相 + 求解器两处改动；流形本身仍每步
重算（跨帧活下来的只有身份与冲量，即主流的接触缓存路线）。

**验证**：`cargo test -p quench3d-physics -- contact_feature_ids`（稳定性：0.5 mm 内抖动
同 ID、5 mm 不同 ID、NaN/∞ 退化为"未知"），以及整个物理套件 151 条保持绿
（说明热启动改动没有破坏既有行为）。

## 26. 第 27 轮：机器可读 JSON Schema + 版本策略（gap #19）

**为什么不是"加个 schemars 依赖自动生成"**：这个仓库有一条硬约束（离线、不加依赖），
而且自动生成的 schema 往往把 `Option` 展开成一堆 `anyOf`，编辑器补全反而更难用。
所以手写，但**用源码对账把"手写会腐烂"这个风险摁住**：测试 `include_str!("scene_doc.rs")`
直接读结构体定义，抽出字段名（含 `#[serde(rename)]`，识别 `#[serde(skip)]`）与枚举变体
（按 `rename_all` 折算），与 schema 里的属性名**双向**比对——少一个、多一个都红。
这样"加字段忘了改 schema"永远不会溜到用户那边，而 schema 本身保持人写得动的形状。

另外两条自洽检查：每个 `$ref` 必须指向存在的 def（写错名字是"静默失去补全"，
比报错更难查）；每个对象 def 必须 `additionalProperties: false`
（否则 schema 说合法、`validate` 说非法，用户会以为工具自相矛盾）。

**分工写进了文档和 README**：schema 管**结构**（哪些键、嵌套、不许额外字段），
`validate` 管**语义**（范围、互斥、交叉引用）。schema 里的值大多是"任意"——
"schema 过了"不等于"场景能跑"，这句话必须说在前面。

**版本策略**这一半，答案是"不改代码、把话说清"：1 → 2 的唯一差别是 `objects[].parent`
在 v2 才生效，没有 parent 的文件语义完全一样，所以迁移就是把 `version` 改成 2。
诊断里现在直接给出这句话；比版本新则给"请升级 quench3d"。**不做迁移器**——
一个只改一个整数的重写工具，维护成本比它解决的问题高。

**验证（不需要显卡）**：`cargo test -p quench3d-cli --lib -- json_schema`（源码对账 +
自洽 + 悬空 `$ref`）、`cargo test -p quench3d-cli --test cli_e2e -- json_schema`
（`--json-schema` 输出 draft/枚举/新字段；不带 `--json` 直接给 schema；不带 flag 仍是语法表）。

**没做**：语义约束（范围/互斥）不进 schema；没有 draft-07 兼容；不支持外部 `$ref`；
没有"只输出某一节"的开关。

## 25. 第 26 轮：模板 + 实例（gap #14）

**这一轮的目标只有一个：让"摆 1000 个箱子"不再需要 1000 份 JSON。** 做法是两条正交的复用：

- `templates` 把"这类东西长什么样"抽出来（字段集与物体**完全相同**——直接复用 `ObjectDoc`
  承载，少一个结构就少一处不一致），物体用 `template` 引用；
- `instances` 把"每个副本差在哪"列出来（位置/朝向/缩放/名字），一次摆多个。

**语义上仍然是 N 个独立物体**：不是 GPU 实例化，也不是共享刚体。这一点必须在文档里说死，
否则用户会以为"改一个副本的摩擦"能影响全部。代价是真实的物理开销，所以副本数 > 64 给一条警告。

三个设计选择值得记下来：

1. **覆盖规则只有一条**：实例 > 物体 > 模板。形状是"整体二选一"（物体写了 `shape`/`shapes`
   就不取模板的形状），不搞字段级深合并——"模板的 shape + 物体的 shapes"必然报错，没人想要。
2. **单副本保本名**：`instances` 只有一个且没写 `name` 时，展开后的物体用物体本名。
   这样给老场景加一个副本不会打断引用它的关节/动作（名字是跨文件引用，断一次很难查）。
3. **校验先展开再验**：结构层查模板引用/实例表/名字唯一，其余一切交给 `expanded()` 之后的
   文档去验——物体字段那几百行检查一个字都不用改，也不会重复报同一条问题。
   代价是错误里的 `objects[k]` 是**展开后**的下标，这一点写进了格式说明。

**验证（不需要显卡）**：
`cargo test -p quench3d-cli --lib -- templates_and_instances`（模板继承、物体覆盖模板、
实例覆盖物体、名字规则、3 条错误 2 条警告、往返保留 templates/instances）；
`cargo test -p quench3d-cli --test cli_e2e -- templates_and_instances`（一份描述 → 报告里
`dynamic == 3`；三层箱子各自停在 0.2/0.6/1.0；用动作按名字推动 `塔#3` 确实动了）；
`cargo test -p quench3d-editor --test scene_roundtrip -- templates_and_instances`
（装载展开成 3 个物体，存回时摊平且过 CLI 校验）。

**没做**：外部场景引用（跨文件 include，与 #11 资产路径同源）、模板嵌套、
编辑器存回摊平 instances（有意取舍）、GPU 实例化的"共享刚体"语义（本来就不该有）。

## 24. 第 25 轮：复合形状 —— 一个刚体多个 Shape（gap #12）

**这一轮把"桌子的板 + 腿"从"多个刚体 + 关节硬拼"变成一件事**。三条硬约束决定了实现方式：

1. **单形状必须逐位不变**。引擎里绝大多数刚体只有一个形状，所以主形状单独存在、
   附加部件放 `extra_parts`，所有路径先看 `extra_parts.is_empty()` 走老分支。
   质量属性也一样：合成函数在单形状时**原样返回** `shape.mass_properties()`。
   （把"零件惯量 → 平移 → 再平移回来"算一遍在浮点上不保证回到原值，而现有场景的
   digest 回归就是拿这些位钉着的。）
2. **惯量沿用对角表示**：零件惯量转到刚体坐标系取对角，加平行轴 `m·r²`，再挪回合成质心。
   没有惯性积——这是引擎原本就有的近似，不在这轮悄悄改口径。
3. **接触点要有界**：每对刚体最多 8 个（超限按穿透深度取最深的，稳定排序保证确定性）。
   部件一多，点会成倍涨，热启动缓存会被冲垮。

流程上这一轮是"引擎 → 格式 → CLI/渲染 → 编辑器"，其中**编辑器那一环差点埋雷**：
复合形状在渲染上要拆成多个绘制物体，如果导出时把它们也当成场景物体写进 `objects`，
场景每存一次就多出几个物体。所以加了 `SceneObject::owner_part`（"我是谁的部件"），
导出时跳过它们、把部件写进主物体的 `shapes`，测试里连"文档里不许出现 `名字#序号`"一起钉住。

**过程中抓到的真 bug**：CLI 里我把主形状既当 builder 的第一个参数、又当附加部件加了一遍
→ 单形状物体的质量与惯量翻倍、接触点重复。既存的静摩擦端到端测试立刻报
"该滑走的箱子只走了 0.0336 m"。修掉之后全绿——**这类"参数写重了"的错，只有端着
旧行为的回归测试能立刻发现**，光看新功能的测试是发现不了的。

**验证（不需要显卡）**：
`cargo test -p quench3d-physics --lib -- compound`（质量/质心/惯量逐项核对 + AABB 并集；
附加部件真的参与碰撞与射线；单形状质量属性与 `mass_properties` 逐位相等）；
`cargo test -p quench3d-cli --lib -- compound_shapes`（解析/往返/3 条错误/2 条警告）；
`cargo test -p quench3d-cli --test cli_e2e -- compound_shapes`（带腿的桌子站住、位移 ≈ 0；
裸顶板掉下去、位移 > 0.5；`dynamic == 1`）；
`cargo test -p quench3d-editor --test scene_roundtrip -- compound`（导出写 `shapes`、
伪物体不被写成独立物体、读回来仍是两个部件）。

**没做**：部件级材质（摩擦/恢复/传感器/CCD 仍是刚体级）、部件不能是三角网、
没有部件编辑面板与部件选择框、每对最多 8 个接触点、惯量对角近似。

## 23. 第 24 轮：事件总线（gap #29）

**这一轮给"一步里发生了什么"开了一个正式出口**。以前只有"当前接触表"和传感器进入/离开，
想在宿主里做音效、计分、伤害、断裂特效，只能自己逐帧比对状态——那是**把引擎的职责推给每个宿主**。

做的事：

* `PhysicsEventKind` 七种：`contact_begin` / `contact_end` / `sensor_enter` / `sensor_exit` /
  `sleep` / `wake` / `joint_broken`；`PhysicsEvent` 是**扁平 `Copy` 结构**（`step`/`kind`/`a`/`b`/
  `joint`/`force`/`torque`），队列 `Vec<PhysicsEvent>` 跨步累积、缓冲复用。
* 接触开始/结束用**集合差分**（本子步 vs 上一子步的真实接触对），排序去重后再比 →
  事件顺序与宽相顺序无关（端到端测试里"两次运行事件流逐条相同"钉住）。
* 睡眠/唤醒用**每步状态比对**，而不是在五个唤醒入口埋点：漏一个入口就会漏事件，
  而且那种 bug 只在特定路径上出现。
* 队列有上限（4096）+ `events_dropped()` 记账：事件是给宿主的，宿主忘取时无上限队列＝内存泄漏；
  "丢"必须留痕。
* `simulate` 报告输出 `events[]`（解析成物体名）与 `events_dropped`（0 也输出）。
* 编辑器状态栏只读显示队列深度/丢弃数/最近一条事件。

**这一轮的小坑**：`events_dropped` 一开始跟着"空就不输出"的习惯写了
`skip_serializing_if`，端到端测试立刻发现 `Null != 0`——脚本要能区分"没丢事件"与"没这个字段"，
所以它改成**永远输出**。

**验证（不需要显卡）**：`cargo test -p quench3d-physics --lib -- event_bus`（弹跳球的 begin/end、
穿过触发区的 enter/exit、静止入睡与冲量唤醒、断裂事件带力量级、队列上限与记账、
两次运行事件序列逐条相同）；`cargo test -p quench3d-cli --test cli_e2e --
simulate_report_includes_an_event_stream`（物体名、步号单调、sleep 只报一次、断裂量级 > 阈值、
两次运行 `events` 逐条相同）。

**没做**：生命周期钩子（`on_scene_loaded` / `on_destroy`）与用户回调（没有脚本层，宿主只能轮询）；
没有事件订阅/过滤；没有"持续接触"事件；满了丢新事件而不是丢最旧的；编辑器没有事件面板/时间线。

## 22. 第 23 轮：关节补全 —— 锥形限位 / 位置伺服 / 断裂 / 两端不碰撞（gap #11）

**这一轮补的是"想做布娃娃、机械臂、可破坏门"缺的那几块零件**，四项一次做完（都是同一个
`joints[]` 段，分开做反而要改四遍格式与文档）：

1. **球窝锥形限位**（`cone_limit`）：两端各一根局部轴，夹角不超过 `half_angle_deg`。
   实现上没有新写约束行——把"两轴夹角"塞进铰链限位的坐标位、把"两轴叉积"塞进修正轴，
   直接复用那一行已经测过的收敛行为。
2. **铰链位置伺服**（`motor_target_deg` + `motor_stiffness` / `motor_damping`）：
   `ω = kp·wrap_pi(目标 − 当前角)`，限幅后交给原有的马达行。**不做** `-kd·ω` 显式阻尼：
   那个写法在序列冲量求解器里会让 ω 在 0 与目标之间来回跳（迭代次数是偶数时原地不动）。
3. **断裂阈值**（`break_force` / `break_torque`）：按"本步累积冲量 ÷ dt"估量级，
   超阈值就把关节移除，`broken_joints()` 与报告的 `joints_broken` 留痕。
4. **关节两端缺省不互相碰撞**（`collide_connected`，缺省 `false`）：与 Bullet/PhysX 同款语义。

**这一轮踩到（并修掉）的三个坑，都写进了注释与文档**：

* **`rad()` 不做换算**：它只是"用弧度构造"。我在 runner 里把 30 度当成 30 弧度送进伺服，
  绕回后目标变成 −81.13°，门稳稳停在 −81°——看起来像"伺服不收敛"，其实是单位错。
  锥形限位也中过一次（25 度 → 25 弧度 → 被 clamp 到 π，限位静默失效）。
* **断裂判定放在 `mem::take` 借走关节表之后**：`self.joints` 那会儿是空的，判定**静默失效**
  （力算出来 627.84 N > 阈值 500 N，关节却不断）。移到 `self.joints = joints` 之后才对。
* **马达驱动的刚体会睡着**：伺服到位后 ω 正好是 0，睡眠判定会把它冻结在"没到位"的角度上。
  现在只要关节带马达，两端刚体这一步不许睡（同岛邻居也跟着清醒）。

**验证（全部不需要显卡）**：
`cargo test -p quench3d-physics --lib -- ball_cone_limit hinge_servo servo_keeps joint_breaks jointed_bodies_stop`
（锥形限位半秒内压回锥面、绕轴自转仍自由；伺服正/反向都能到位并 10 秒不漂；
64 kg 载荷挂 500 N 关节必断、1e9 N 必不断、0/负/NaN 阈值等于不设；缺省不碰撞 vs 显式打开对比）；
`cargo test -p quench3d-cli --lib -- joint_completion_fields`（解析 + 往返 + 5 条 error + 4 条 warning）；
`cargo test -p quench3d-cli --test cli_e2e -- joint_servo_cone_limit_and_break_force`
（端到端：门停在 30°±3°、自由摆仍 60° 而限位摆 < 33°、`joints_broken` 1→载荷落地 / 1e6 N 吊在原处）；
`cargo test -p quench3d-editor --test scene_roundtrip -- export_preserves`（新字段不被编辑器丢掉）。

**没做**：6DOF/万向节、角度弹簧-阻尼、疲劳/累计损伤、断裂事件回调、关节可视化与 gizmo、
球窝伺服、锥形限位的"硬夹"（现在是速度级）。

**门禁**：`tools/gate.sh` 六段全绿，**430 个测试（0 失败）**，wasm 11,601,438 字节。
（第 24 轮事件总线之后是 **434** 个测试、wasm 11,606,845 字节，同样六段全绿。）
（第 25 轮复合形状之后是 **440** 个测试、wasm 11,655,974 字节，六段全绿。）
（第 26 轮模板/实例之后是 **443** 个测试、wasm 11,737,533 字节，六段全绿。）
（第 27 轮 JSON Schema 之后是 **446** 个测试、wasm 11,738,498 字节，六段全绿。）
（第 28 轮接触特征 ID 之后是 **447** 个测试、wasm 11,739,868 字节，六段全绿。）
（第 30 轮接触余量之后是 **448** 个测试、wasm 11,742,106 字节，六段全绿。）
（第 34 轮命名相机之后是 **449** 个测试、wasm 11,757,550 字节，七段全绿。）
（第 35 轮 `--camera` 之后是 **451** 个测试、wasm 11,757,550 字节，七段全绿。）
（第 36 轮多输出之后是 **452** 个测试、wasm 11,776,781 字节，七段全绿。）
（第 38 轮关键帧动画之后是 **457** 个测试、wasm 11,806,896 字节，七段全绿。）
（第 39 轮角速度推算之后是 **460** 个测试、wasm 11,807,433 字节，七段全绿。）
（第 40 轮 clip 混合之后是 **462** 个测试、wasm 11,806,579 字节，七段全绿。）
（第 41 轮模型缓存之后是 **465** 个测试、wasm 11,816,396 字节，七段全绿。）
（第 42 轮 CSM 之后是 **470** 个测试、wasm 11845940 字节，七段全绿。）
（第 43 轮级联量化之后是 **472** 个测试、wasm 11845940 字节，七段全绿。）
（第 44 轮资产搜索路径之后是 **473** 个测试、wasm 11859839 字节，七段全绿。）

## 21. 第 22 轮：物理材质 —— 静摩擦 + 摩擦/恢复的合成规则（gap #22）

**做了什么**：`friction` / `restitution` 以前各写一个、接触时听谁的没得选（摩擦写死几何平均、
恢复写死取大）。现在合成规则可选（5 条）、世界级缺省 + 物体级覆盖、静摩擦独立于动摩擦。

| # | 决定 | 理由 / 落点 |
| --- | --- | --- |
| 1 | 缺省规则 = `geometric_mean`（摩擦）/ `max`（恢复） | 这两个就是**老行为**。换成教科书上更"标准"的值会让已有场景的球跳得不一样高——那是观感变化，不是 bug 修复，必须由用户显式选 |
| 2 | 静摩擦的判定用**施力前**的速度 | 用施力后的速度时，静止箱子被推的那一步速度已经是 Δt×a（1/60 × 5 ≈ 0.08 m/s）> 阈值 → 永远走"滑动"分支 → 静摩擦形同虚设。实测表现是"静摩擦 1.0 的箱子照样滑 2.4 m" |
| 3 | 判定在**准备阶段**做一次，不在速度迭代里反复做 | 反复判定会让摩擦锥上界在迭代中跳变：收敛变差，而且结果依赖迭代次数（同一个场景换迭代数就换答案） |
| 4 | 快的实现：`step` 开头把速度快照进复用缓冲 | 稳态零分配（这个项目对"每步分配"很敏感）；代价是每体多 24 字节的快照缓冲，换来静摩擦真的生效 |
| 5 | 规则解析：先显式、后缺省；两端都显式且不同时取 A | 与 Unity 的 per-collider combine + 工程缺省一致。`Average` 与 `GeometricMean` "再合成一次"没有物理意义，所以不做 |
| 6 | 编辑器下拉必须有"（缺省）"项 | 否则用户点过一次规则就再也回不到"跟随世界缺省"的状态（这是"用下拉框表达三态"的经典坑） |
| 7 | 静摩擦 < 动摩擦只给**警告** | 它物理上不常见但不是非法（有些情况确实想要"起步就要大劲"），校验不该拦 |

**验证**：
* `cargo test -p quench3d-physics --lib -- static_friction friction_combine combine_rule`
  （**不需要显卡**）：同样 5000 N 推力下"静摩擦 1.0 按住（位移 < 0.05 m）/ 静摩擦 0.05 滑走
  （> 0.5 m）"；`min` 与 `max` 规则让同一个箱子滑出不同距离；五条规则的数学、负数/NaN/inf
  的兜底、`from_name` 与 `name` 互逆、缺省规则 = 老行为；**digest 回归测试全绿**（默认路径逐位不变）。
* `cargo test -p quench3d-cli --lib -- physics_materials`：解析/往返/五个非法与告警路径。
* `cargo test -p quench3d-cli --test cli_e2e -- static_friction_and_combine`：端到端跑二进制，
  量 `simulate` 报告里的**位移**（按住 0.00 m / 滑走 3.22 m；`min` 比 `max` 远；物体级规则盖过世界缺省）。
  这里踩过一个坑：`states[0]` 是**地面**（静态，位移恒 0），按名字取才对——差点把"假证据"当通过。
* `cargo test -p quench3d-editor --test scene_roundtrip -- export_preserves`：`physics` 段与
  物体级材质字段"存→读→再存"字节不变，且编辑器里改静摩擦/规则能写进文件。

**没做（诚实边界）**：没有各向异性摩擦、滚动摩擦、摩擦-速度曲线（只有静/动两档）；
两端都显式指定不同规则时只有一条优先级（slot 靠前者），格式校验层不会为此报警；
`static_friction_speed` 只有引擎级一个值，不能按材质各给一个。

---

## 20. 第 21 轮：睡眠 API 补全（gap #9）

**做了什么**：`RigidBody::sleep()` 公开、`PhysicsWorld::wake_up_all()` 新增，
`allow_sleep = false` 从"不主动入睡"升级成"**永远清醒**"（顺手修掉一个只有新 API 才够得着的
真 bug），场景格式的 `wake` 动作支持 `target: "*"`（唤醒全部），编辑器工具栏加了「☀ 唤醒全部」。

| # | 决定 | 理由 / 落点 |
| --- | --- | --- |
| 1 | `wake_up_all()` 返回 `usize`（唤醒个数） | 调用方能确认"它真的干活了"。这个项目里"调了个 void 函数然后假设生效"已经出过太多次事 |
| 2 | `sleep()` 文档里把"手动 vs 自动"的差别写死 | 手动：立即、**清零速度**；自动：整岛计时 0.5 秒（逐体入睡会让堆叠渗下去）。语义不写清，用户会在"为什么我搬下来的箱子醒来是停着的"上卡住 |
| 3 | `allow_sleep = false` 变成硬承诺（**行为修正**） | 之前 `update_sleeping` 第一遍只看速度：一个被手动 `sleep()` 的 `allow_sleep=false` 刚体会永远睡着。在 `sleep()` 公开之前这个状态从 crate 外构造不出来，所以修正它不影响任何已有场景 |
| 4 | `wake` 动作的 `"*"` 用**类型**表达（`handle: Option<BodyHandle>`） | 拿 `BodyHandle{index:0,..}` 当"全部"的哨兵，会真的去动 0 号刚体——这类哨兵是静默失效的温床 |
| 5 | `"*"` 只对 `wake` 有效，别的动作报错 | 一个"看起来作用于全部、其实只作用于一个"的动作比报错难查得多 |
| 6 | 编辑器按钮不写撤销历史 | 它不改场景内容，只改运行时状态（位置/速度都没动）；写进历史会让"撤销"把物理状态倒回去，与"撤销=场景编辑"的语义打架 |

**验证**：
* `cargo test -p quench3d-physics --lib -- wake_up_all`（**不需要显卡**）：零重力静止场景
  两个盒子 0.5 秒后都入睡 → `wake_up_all()` 返回 **2** 且两个都醒；再唤一次返回 **0**（幂等）；
  `sleep()` 立即生效且线/角速度都清零；睡着之后 30 步位置不漂（不被积分）；
  `allow_sleep = false` 的刚体 `sleep()` 之后下一步一定醒。
* `cargo test -p quench3d-cli --test cli_e2e -- wake_action_with_star`（端到端跑二进制）：
  零重力场景报告里 `sleeping` 从 **2** 变成 **0**（`target: "*"`）；
  只唤醒 `a` 时 `b` 仍然 `asleep`（证明 `*` 真的是"全部"）；`impulse` 配 `"*"` 退出码 **2**
  且诊断路径是 `actions[0].target`。

**没做（诚实边界）**：没有"列出睡着的刚体"的 API（报告里的 `sleeping` 计数与 `states[].asleep`
够用）；`wake_up_all` 不 bump 查询索引版本（位置没变，索引不受影响）；
没有按区域唤醒（`probes` + 自己遍历 `bodies()` 可以做，但那是用户侧的组合）。

---

## 19. 第 20 轮：光照补全 —— 雾 / 阴影质量 / 天空颜色（gap #17）

**做了什么**：把引擎里早就有、但场景文件配不到的三样接到格式上：
`lighting.fog`（颜色 + 密度，不写 = 关闭）、`lighting.shadow_quality`（强度 + 覆盖半径）、
`lighting.sky_colors`（天顶/地平线/地面）。顺带把点光源/聚光灯那条过时记录对账了
（点光源 2026-10-15 就接线了，只是 gap 表没更新）。

| # | 决定 | 理由 / 落点 |
| --- | --- | --- |
| 1 | 三节都遵循"**不写 = 保持原样**" | fog 不写 = 关闭（像素逐位相同，有端到端断言）；shadow_quality 不写 = 引擎缺省；sky_colors 只覆盖写了的通道（缺省跟随 preset）。加字段不能悄悄改已有场景的观感 |
| 2 | 颜色空间跟 `point_lights` 走**线性** | 引擎内部就是线性；如果这里收 sRGB 再转，同一个文件里"材质色 sRGB、灯光色线性、雾色 sRGB"三种规则，谁都会写错。代价是文档必须三处写清（schema / README / 字段注释） |
| 3 | `shadow_quality` 与 `shadows` 开关**分开** | "投不投"和"投得好不好"是两个问题；混成一个字段会让"我想要柔和阴影"变成"我要关掉阴影" |
| 4 | 阴影贴图分辨率**不进格式** | 它是渲染器构造参数（`Renderer::new` 里建纹理 + 绑定组），运行时改要重建。能写但不生效的字段比没有更坏（这个项目的头号原则就是不做静默失效） |
| 5 | 校验分 error / warning 两档 | 负密度、强度 2.5、半径为 0 → **画不出来**，是 error；密度 2.0（两米外全白）、半径 900 米（糊成一片）、颜色 9.0（过曝）→ **能画但多半是写错了**，是 warning |
| 6 | 编辑器颜色控件要**写回** | egui 的颜色控件改的是传进去的 `[f32;3]`，`&mut light.sky.zenith.to_array()` 改的是临时值——改了不生效、还留下一笔撤销历史（第一版就这么写的，改代码时抓出来） |
| 7 | 顺带修正三处过时文档 | `gap_analysis` 的 §3.2 表达力边界表（点光源/天空与雾两行）与 §3.1 字段表、`production_readiness_criteria` 的 S1-T3/S2-T2/S2-T3/S2-C1 四条（贴图导出是否丢、相机 near/far/正交、撤销/重做与 gizmo）——这些都是**已经做完但文档还写着没有**的条目，留着会让下一轮重复劳动 |

**验证**：
* `cargo test -p quench3d-cli --lib -- fog_shadow_quality`（**不需要显卡**）：合法值解析 + JSON 往返、
  五个非法路径各给 error、超范围给 warning、NaN/inf 也挡（JSON 里没有 NaN，只能从代码路径构造）、
  拼错字段报未知字段、不写三节 = 全 `None`。
* `cargo test -p quench3d-cli --test cli_e2e -- fog_and_shadow_quality`（端到端跑二进制）：
  六个合法场景校验通过；非法场景退出码 2 且诊断路径指向 `lighting.fog.*` 等；
  渲染断言里 **"雾密度 0" 与 "不写 fog" 逐位相同**，浓红雾把画面整体推向红色（R−B 增大）、
  阴影强度 0→1 让整幅图变暗、自定义天空颜色确实改变画面。
* `cargo test -p quench3d-render --test fog_and_shadow_quality`（**像素级**，本机有显卡真跑，CI 自动跳过）：
  浓红雾让远处像素变红主导、密度 0 逐位相同、阴影强度只暗在影子里（两帧差异 > 150 像素且整体变暗）、
  强度 0.5 的暗化落在 0 与 1 之间（单调，不是"要么全无要么全黑"）。
* `cargo test -p quench3d-editor --test scene_roundtrip -- export_preserves`：雾/阴影质量/天空颜色
  存盘往返不丢，而且"在引擎里改 → 导出"确实写进文件（数值按类型核对，不做字符串匹配）。

**没做（诚实边界）**：阴影贴图分辨率不在场景文件里（要改得重建纹理与绑定组）；
只有指数雾（没有线性雾的起止距离、没有高度雾）；没有 IBL/环境贴图（#26）、
没有聚光灯与点光/聚光阴影（#24）、天光不按日期与时区（#24/#26）；
编辑器的贴图槽仍然只能读不能换（读得回来、改不了路径）。

---

## 18. 第 19 轮：查询补全 + 查询加速结构（gap #21）

**做了什么**：引擎的查询从"最近一条射线 + 球形重叠、全线性扫描"补成
`raycast_all` / 任意形状 `overlap_shape` / `distance_to` / 单向 `QueryFilter`，
再加一个**快照式**的 `QueryIndex`（均匀空间哈希网格 + 射线 3D DDA）；
CLI 新增 `query` 子命令、MCP 新增 `quench3d_query` 工具、编辑器 F7 加了查询探针。

| # | 决定 | 理由 / 落点 |
| --- | --- | --- |
| 1 | 查询过滤用**单向**判据（`QueryFilter::mask`），与物理的对称判据分开 | 物理是"双方都愿意才碰"，查询是"我问你答"，只有提问方有立场。把两者混成一个 API 会让人以为 `mask` 也能当过滤器用 |
| 2 | 索引复用宽相的**同一份** `pick_cell_size` / `spread_into_cells` | 两边的网格形状一致，"宽相里配得上对的两个物体在查询时也该在彼此候选里"就不需要额外推理；代价是宽相那两个函数泛型化了 |
| 3 | **没有**复用宽相实例（`Broadphase`）当查询索引 | `compute_pairs` 在 `proxies.len() < 2` 或"没有可动体"时**直接 return**，网格根本没建——静态场景查询会全部落空。自建一份网格（策略共用、状态独立） |
| 4 | 索引是**快照 + 自动回退**，不是增量维护 | `PhysicsWorld::version` 在增删刚体 / `body_mut` / `step*` 时 +1；过期查询回退线性扫描。正确性永远保住（不会有静默错答），加速只在"每帧重建一次、帧内查很多次"时体现 |
| 5 | 射线走 **3D DDA**，重叠扫探针 AABB 覆盖的单元 | 射线只测真正经过的格子；超大物体（铺格子超过上限）与半空间升级成"全局代理"，每次查询都测一遍，保证不漏 |
| 6 | 结果**必须与线性路径逐条一致**（距离升序、平局按槽位下标） | 有专门用例逐条对比句柄/距离/点，并断言索引路径确实少检查了刚体；不然"加速"会变成"换一套语义" |
| 7 | `--max` 对重叠查询是**距离上限**（相交的距离为 0，永远报） | 否则这个参数对 `--overlap` 就是个装饰；现在"这附近 2 米内有什么"也能问 |
| 8 | `query` 拒绝 `--out` / `--dry-run` / `--no-clobber` | 它只读不写；悄悄忽略参数是这个项目最不想要的失败模式（与 `simulate` 的处理一致） |
| 9 | 编辑器只加 **F7 探针**（画线），不做查询面板 | 查询的"结果"是数据不是编辑动作；探针让"我看的这块是什么"在屏幕上可见，`center_probe` 还是纯函数（离线可测） |
| 10 | MCP 工具的参数与 CLI **共用解析器** | `runner::parse_ray_query` / `parse_overlap_query` 一份实现两个入口，语法不会漂移 |

**验证**：
* `cargo test -p quench3d-physics --lib`（**不需要显卡**，6 条新用例）：`raycast_all` 的距离
  升序 / 上限截断 / 退化输入、任意形状重叠与老 `overlap_sphere` 一致、单向层掩码与传感器开关、
  **索引 vs 线性逐条一致且确实更省**、过期自动回退且结果跟着世界走、横跨多单元的大物体 + 掩码。
* `cargo test -p quench3d-physics --doc`：`QueryIndex` 文档里的例子真跑（顺带钉住 API 形状）。
* `cargo test -p quench3d --lib center_probe`：编辑器探针（命中距离 = 起点到盒面、法线朝相机、
  看旁边为空、`max_distance` 生效）。
* `cargo test -p quench3d-cli --test cli_e2e query_`（端到端跑二进制）：最近命中、`--all` 两条
  按距离升序、`--no-sensors`、`--mask 8`、`--index` 与线性一致、重叠命中距离为 0、
  45° 旋转探针真的生效、`--max` 截断、`--steps` 先跑物理再查（球落地前后各一次），
  以及 9 条错误路径（两种查询同时给 / 都不给 / `--all` 配 overlap / 数字个数不对 /
  零方向 / 未知形状 / 位置缺数 / 三个写盘开关 / 场景不存在）都被拒。
* `cargo test -p quench3d-cli --lib -- mcp`：工具清单 9 个、`quench3d_query` 只读 + 幂等 +
  `outputSchema` 有 `hits`。

**没做（诚实边界）**：`shape_cast`（保守推进）**仍然线性**，没进加速结构；CLI 一次进程只做一次
查询（单次建索引是净亏，要批量得走引擎 API 或 MCP 循环）；索引**是快照**，没有增量更新，
世界一动就回退（结果对、只是不快）；`raycast_all` 对每个刚体只给**最近的一个**命中点
（没有"进入/离开两个交点"的变体）；查询**不看物理分组**（这是设计：`mask` 单向）；
编辑器探针只是画线，我没法用图像验证它在屏幕上的样子（`read_image` 在这个模型上不可用），
能保证的是 `center_probe` 的数值与编译期检查。

---

## 17. 剩余清单对账（全部闭环）

`docs/overnight_report.md` 里四件事，逐条落到提交与 CI：

| # | 事项 | 提交 | 证据 | CI |
|---|---|---|---|---|
| ① | 模型引用接进场景树 + 视口实时拖动 | `d221f0e` / `3ae8c2a`（写回）+ 第 15 轮（§12） | `cargo test -p quench3d-editor --test model_instances`（5 个用例） | ✅ |
| ② | 场景层级/父子 + 场景格式 v2 | `4e4e82a` / `a9042b8` / `8d2a916` / `3c7b313`（§13） | 引擎侧纯逻辑用例（不需要显卡）+ 编辑器 `hierarchy` + 端到端 | ✅ |
| ③ | 后处理 FXAA / SSAO | `3d4a692`（FXAA）→ `a090075`（SSAO）（§14/§15） | `post_pipeline` 像素级用例（本机有显卡，CI 上自动跳过）+ `ssao-demo` 端到端 | ✅ 全绿 |
| ④ | 物理碰撞过滤 / 传感器事件 / CCD | `3a272f1`（§16） | 物理 7 条（**不需要显卡**，CI 上真跑）+ 格式校验 + `cli_e2e` + 编辑器往返 | ✅ 全绿 |

四项的共同要求（"引擎实现 + 场景格式字段 + 校验 + 编辑器/CLI 可达 + 测试 + 文档更新 +
门禁全绿 + 推送 + CI 绿"）都是逐条做完的；每一项的**没做**部分都写在自己的小节与
README 的边界清单里，没有含糊过去。

**收尾时的测试规模**：401 个测试（0 失败）；`tools/gate.sh` 六段全绿
（fmt / clippy 原生 / clippy wasm32 / 测试 / wasm 构建 11,531,134 字节 / `check_web.mjs`）。
（第 19 轮之后是 **411** 个测试、wasm 11,532,858 字节，门禁同样六段全绿。）

**这一夜最值得记住的两个坑**（都不是"写错了代码"，是"想错了模型"）：
1. SSAO 的遮挡判据加上界——少了上界，掠射角下同一个平面自己遮自己；
2. CCD 用保守推进扫掠——贴着地面滑行的子弹会被脚下的地面一直拖住，
   换成中心射线才干净。两个坑都留了回归用例，注释里写了"为什么不是另一个做法"。

## 32. MSRV 从"声明"变成"被验证"（第 37 轮）

`Cargo.toml` 里那句 `rust-version = "1.90"` 从第 26 轮起就写着注释："这是依赖元数据推出的
下界，没有用 1.90 真的编译验证过"。这是一句**诚实的自认**，但它同时也是一个风险：
声明与实际支持范围之间的差，谁都不会发现，直到某个用户拿 1.90 编译失败。

这一轮加了 `msrv (1.90)` 这条 CI 腿（1.90.0 工具链 + `cargo check --locked --all-targets`，
范围是工作区成员里除编辑器之外的全部）。**第一次跑就全绿**——也就是说这个数字一直是准的，
只是没人验证；现在它有了防线。范围有意写成"除编辑器"（编辑器单独 1.95，egui 0.37 要求），
并写进 CI 注释与 gap 行：宁可说清适用边界，也不要含糊地宣称"整个工作区都过了"。

顺手把 `Cargo.toml` 的注释从"没验证过"改成"由 CI 的 msrv 腿验证"——注释也是文档，
过期的注释比没有注释更坏。

## 37. 三次"没接上"是三种不同的原因（第 42 轮）

第 42 轮做级联阴影（CSM）。代码本身写完了，倒是三个不同层面的"没接上"值得记下来。

**一、诊断被静默丢掉（第二次犯同一类错）**：新加的 `cascades` 校验把诊断 push 进了 `cam_diags`，
而这个累加器在**光照检查之前**就已经并进输出了——于是 `cascades: 0` 校验结果是 `valid: true`。
e2e 当场抓住。教训不是"记得用对变量名"，而是：**一个函数里有两个输出累加器时，代码顺序本身就是陷阱**；
上一轮我刚在这里踩过一次（动画诊断被 early-return 丢掉），这次换了个形式又来一遍。

**二、相对输出路径会把文件丢进仓库**：第一版 e2e 用场景里的 `render.output: "single.png"`，
PNG 落到了**进程当前目录**（`crates/quench3d-cli/`）。这不是测试写错，而是真实行为：相对输出按 CWD 解析，
而测试的 CWD 就是仓库目录——`git add -A` 会顺手把这两张 PNG 提交上去。仓库里早就有先例
（另一条测试的注释写着"输出走 `--out` 绝对路径，别写到仓库目录里"），我没照做。
已清理散落文件，新测试改成绝对 `--out`。**每次 `git add -A` 之前先看一眼暂存清单。**

**三、想量化"更清晰"，结果测了个寂寞**：为了证明级联有用，我设计了一个"远投射体"场景
（单级覆盖不到、多级应当出现影子），比暗像素数量——两者几乎一样（10752 vs 10764）。
原因是无天空背景本身就占了 42% 的暗像素，统计量被淹没。**这次测量没得出结论，我就不写
"级联让阴影更清晰"这种话**；能站住的只有两层：CPU 侧几何不变量 + GPU 侧"画面确实变了且都有影子"。
更要紧的是：仓库里**本来就有**量化入口（`shadow_probe` 的落点误差、`offline_render` 的阴影占比/质心），
我先自己造了一套粗糙统计。**先找已有量尺，再自己发明指标。**

## 38. 一个错的假设、一条对的不变式，和 3 个像素（第 43 轮）

第 43 轮的任务是补上第 42 轮留下的窟窿：级联阴影到底值不值，只有"画面变了"这种不体面的证据。
我先把仓库里**已经有**的量尺（`examples/shadow_probe`：阴影质心 vs 解析落点）扩成级联 sweep，
再加一个"半影比例"（地面边缘像素 / 核心像素）。

**测量立刻打脸了我自己的实现解释。** 我看到近景的受影响像素从 5.4% 涨到 15.7%、半影比例
0.15 → 9.6，第一反应是"自阴影（acne）"，并推理出一条因果链：各级用自己的 2*radius 深度范围 →
归一化 bias 在世界里变小 → 麻点。我照着这条链改了 `cascade::matrices`（改成各级共用深度范围），
还写了个测试。

**结果渲染输出一位数都没变**，测试却红了。追下去才发现：正交远平面是 `2*radius + depth_range`，
所以**深度范围和半径是绑在一起缩放的**——归一化 bias 乘以 `4r` 得到的世界偏移，除以该级纹素
在世界里的大小 `2r/tile`，`r` 恰好消掉。也就是说"一个固定的 bias 常量在每一级都等于同一个
纹素数（约 1.5 个）"，这套代码本来就是对的；我那条改动不但理由错，还会把近处那一级的 bias
放大一个数量级。已回滚，并把**真正的不变式**写成了测试：
`one_normalized_bias_is_the_same_number_of_texels_in_every_cascade`（各级"每纹素对应的归一化
深度"必须一致，极限值 = `1/(2*tile)`，与切分/半径/相机无关）。

教训很具体：**先写判据，再写改动**。我当时是"看到数字不对 → 认定原因 → 改代码"，中间那一步
"原因"没有可验证的判据（如果我先算一下 `4r / (2r/tile)`，五分钟就知道两者无关）。
第二，**渲染数值一位不变**是非常强的信号：它说明我改的东西不在输出路径上（或者被别的缩放抵消），
比"测试通过"更值得追。

**真正的结论是另一个方向**：级联换到的**不是清晰度而是覆盖**。深景（投射体在 60 米外、
单级半径 10 米）里，单级的影像是**一个影子像素都没有**；2/3/4 级分别有 5/3/3 个像素变暗，
落点与解析位置误差 0.5/0.6/0.2 px。而近景（小场景 + far=500）里单级反而清晰约 33 倍。
第 42 轮我写"多级近处更细更干净"——**方向说反了**，这轮改了。

**为什么第 42 轮看不出来**：那个效应在 320x240 的图里只占 3~24 个像素（0.03%）。
我当时拿"整幅图的暗像素统计"去找它，被 42% 的无天空背景淹没，于是得出"测不出来"。
这轮换成只统计**地面像素**（用红蓝差排除投射体），并且**用解析落点当参照**，
3 个像素才变成一个结论。**量尺选对了，小效应也能测；量尺选错了，大效应也测不出来。**

## 39. 一个概念，三套规则（第 44 轮）

第 44 轮做 gap #11（资产引用与搜索路径）。动第一行代码之前先查现状，结果发现要修的不是"缺功能"，
而是**同一个概念有三套规则**：

| 谁 | 怎么找文件 |
|---|---|
| 贴图 | 场景目录 → 当前工作目录 → 引擎搜索路径 |
| 模型 | 原路径 → `assets/` → `quench3d/assets/` → crate 目录（**不含场景目录**） |
| `validate` 查模型 | 同"模型"那一套 |

后果是最自然的写法反而坏掉：把模型放在场景文件旁边、写 `"path": "chair.glb"`，
在工作目录不等于场景目录时加载失败——而贴图在同一份场景里是好的，`validate` 还可能说没问题。
**一个概念有两套规则时，一致性只能靠"共用同一个函数"来保证**，靠"我两边都记得改"保证不了。
所以这轮不是给模型加一段解析代码，而是把三处合并到一个候选列表上，再补 `assets.search_paths`。
错误信息也改成列出**找过哪些位置**：找不到文件最贵的部分从来不是错误本身，而是不知道该放哪儿。

**自己给自己挖的坑（同类错误的第二次）**：我用脚本替换 `pub struct ModelDoc {` 这行来插入新结构体，
结果替换文本里**没有把这行原样写回去**，`ModelDoc` 就丢了函数头——编译立刻报花括号不配对。
上一次同类事故是"用 sed 批量转义把测试字符串搞坏"。两次都是**锚点替换没有把锚点本身还回来**。
现在的做法固定成：替换文本里显式包含锚点（或明确知道自己在删它），并且每步之后立刻编译一次，
而不是攒一堆改动再跑门禁。

还有一个小发现值得记：这条 e2e 里"找不到模型"的退出码是 **2**（`validate` 先拦下来），
不是我一开始以为的 1。我按猜测写断言，测试立刻纠正了我——**断言写错比断言不写更危险**，
因为它会让人以为行为已经确认过了。
