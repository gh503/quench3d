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
* 场景格式迁移器、~~CLI 稳定错误码~~（**第 55 轮已补校验路径**：`diagnostics[].code`，
  见 [docs/error_codes.md](error_codes.md) 与 production_readiness §8.15）、SBOM / 许可审计、`cargo doc` 门禁
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
3. **场景格式迁移器**（版本升级路径，仍未做）+ ~~CLI 稳定错误码~~
   ✅（第 55 轮补齐**校验路径**；顶层 `error.kind` / 运行期 `RunError` 仍未细分）；
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
（第 45 轮 --asset-path 之后是 **475** 个测试、wasm 11861855 字节，七段全绿。）
（第 47 轮帮助与旗标守卫之后是 **476** 个测试、wasm 11861855 字节，七段全绿。）
（第 48 轮 schema.global_flags 之后是 **477** 个测试、wasm 11861855 字节，七段全绿。）
（第 49 轮聚光灯之后是 **479** 个测试、wasm 11877801 字节，七段全绿。）
（第 50 轮清空白名单与删死旗标之后是 **479** 个测试、wasm 11877801 字节，七段全绿。）
（第 51 轮编辑器级联 UI + 修文档计数之后是 **480** 个测试、wasm 11879728 字节，七段全绿。）
（第 52 轮编辑器接上 bloom/fxaa/ssao 之后是 **481** 个测试、wasm 11882544 字节，七段全绿。）
（第 53 轮编辑器局部光 UI + 修默认场景/载入路径光照不对称之后是 **483** 个测试、wasm 11903880 字节，七段全绿。）
（第 54 轮聚光灯阴影之后是 **498** 个测试、wasm 11914308 字节，七段全绿。）
（第 57 轮 GL/downlevel：真实 WebGL2 探针 + naga GLSL 翻译守卫 + MSAA 按能力降级，与第 55 轮点光立方阴影合并之后是 **532** 个测试、wasm 11961808 字节，七段全绿。）
（第 58 轮 CI 假红：`QUENCH3D_GPU_TESTS` 显式 opt-in + 不需要显卡的守卫测试之后是 **533** 个测试、wasm **11961808** 字节（与上轮逐字节相同：新 helper 是死代码，被裁掉了），七段全绿。）

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

## 40. CI 抓到我自己的测试写错了（第 45 轮）

第 45 轮的 `--asset-path` 本地全绿、门禁全绿，推上去 **Windows 掉了一条**。原因不在功能，
在我自己写的 e2e：

```rust
let list = format!("{}:{}", bogus.display(), lib.display());   // 硬编码 `:`
```

功能本身是"多个目录用 `:` 分隔（Windows 用 `;`）"，测试却把 `:` 写死了。在 Windows 上
`C:\tmp\nope:C:\tmp\lib` 是一个**不合法的路径**，根本不会按分隔符切开，于是模型找不到、
退出码 2、断言失败。修法是测试里也用 `if cfg!(windows) { ';' } else { ':' }`。

两条教训：
1. **"平台相关的行为"必须让测试也走平台判断**，否则测试只证明了写测试的人那台机器。
   这不是"CI 太挑剔"，而是 CI 唯一一次真正跨平台的机会。
2. 我在写这条断言时想的是"验证分隔符能用"，实际写出来的却是"验证 `:` 能用"——
   **断言和意图不一致时，测试仍然会绿**（在 Linux 上）。跨平台 CI 是唯一能戳破它的东西，
   这也是为什么"推送并确认 CI 绿"必须是闭环的一部分，而不是可选项。

## 41. agent 的说明书里少了整整一类能力（第 48 轮）

`schema` 的定位是"agent 的自描述入口"，`commands[].args` 把每个命令的参数写得很细。
但 `--json`、`--camera`、`--asset-path` 这类**对所有命令都生效**的旗标，schema 里一个字都没有。
结果是：agent 想知道"能不能指定资源目录"，只能去读 `--help` 的散文，或者干脆试一下。
**工具的说明书漏了整整一类东西，而"漏"这件事本身没有任何症状**——json 还是合法的，
测试还是绿的。

这轮补了 `global_flags`（8 条），并且不让它成为"又一份会腐烂的清单"：守卫测试把三方绑起来
——`kind=value` 必须在 `VALUE_FLAGS`、每条必须在 `--help` 里、`--json/--camera/--asset-path`
必须留在表里、每条都要有非空描述。四个方向里**任何一个方向写错都会红**。

一个设计取舍：`global_flags` **不是**从 `VALUE_FLAGS` 自动生成的。自动生成看似"永不腐烂"，
但会把内部/调试/一次性旗标也塞进 agent 的视野，反而更难用；"给 agent 看的最小可用集 +
守卫测试防删"是更诚实的做法。边界写进了 readiness §13.4。

## 43. 白名单是有毒的（第 50 轮）

第 47 轮我挂了一张"已知没写帮助的旗标"白名单，规定它只能变短。第 50 轮去清，发现 6 项里
4 项（`--ray/--overlap/--mask/--max`）**本来就写在 `schema` 里**，是我当时只看 `--help` 看漏了；
另外 2 项（`--scene/--device`）**全仓没有任何代码读过**，是死旗标 —— `--device 1` 会被静默忽略。

两件事都值得记：
1. **"已知欠账"清单让"没做"看起来像"已分类"**。写它的时候不写"为什么没写"，几轮之后
   自己都分不清哪些是真缺、哪些是看漏。这两种腐烂方式（看漏 / 死代码）都在这轮出现了。
2. **静默忽略的旗标比报错更坏**：用户以为"指定了设备但没生效"是自己的用法问题，会去查别的地方。
   删掉之后至少会得到"多余的参数"。

所以这轮把规则从"help 或白名单"收紧成"help 或 schema"，把白名单清空（不再有中间地带），
并删掉两个死旗标。**清空一张诚实清单的过程，本身就是一次审计。**

## 42. 聚光灯：把"看不出来"变成"量出来"（第 49 轮）

聚光灯是引擎里第一类有方向的局部光。实现上最省的做法是**复用点光源那条
`array<PointLight, 8>`**：单盏 32 → 64 字节，第三段 `dir_outer.w` 当开关
（0 = 点光源，> 0 = 外锥角余弦）。一个循环、一份衰减、一份 PCF，代价是 uniform 从
960 变 1216 字节，用布局测试钉死。

真正花时间的不是功能，是**验证**。这轮验证脚本自己错了四次：
正则把结构体的 `}` 吃掉导致 `replace` 静默失败（脚本还打印了 ok）；探针场景字段名写错
（`target` vs `look_at`）、`ambient` 类型写错（是 `f32` 不是数组）、以为 `sun_intensity: 0`
就是黑场景（**天空色贡献 65% 环境光**）；WGSL 里在 `l2` 定义之前用它；最后断言常量拍脑袋
（要求窄锥亮区 > 1%，实测 0.53%）——**数据一直是对的，是尺子刻错了**。

修完尺子之后量出来的东西才有说服力：亮区占比 0.53%（外锥 12°）与 11.06%（外锥 45°），
与 tan² 的几何预期 0.50% / 11.0% 吻合到 6% 以内。**"亮区面积随外锥角平方增长"这种关系，
是眼睛看不出来的；而能看出来的是"这里有个亮斑"——后者根本不能证明锥角生效。**

一句话：给引擎加功能时，一半的力气要花在"怎么证明它真的按你想的那样工作"上，
而证明脚本本身也要被证明（断言先跑一遍已知答案的对照场景）。

## 44. 门禁只报了第一个失败（第 53 轮）

`cargo test --workspace` 缺省**在第一个失败的测试二进制就收工**，后面的 crate 根本不跑。
第 53 轮我修完第一个 crate 的问题、重跑门禁，才看见第二个 crate 也是红的 —— 两次门禁、
两次都以为"就这一处"。更阴的是底下那行测试计数：它把 `test result:` 行求和，
而没跑的 crate 不计入，所以失败那一轮的"483 个测试"是个**假数字**（真实含义是"跑到失败为止的测试数"）。

第 54 轮把 `gate.sh` 改成 `cargo test --workspace --no-fail-fast`：一次门禁把所有红点都摊开，
计数也才是真的。教训不是"cargo 不好"，而是**门禁的措辞会塑造我的判断**：
"门禁未过"这四个字听起来就像"一处没过"。

## 45. 子串断言既会误报也会漏报（第 53 轮）

第 53 轮我用 `!json.contains("3.0")` 断言"这个值没了"，结果被补光位置里的 `-3.0` 绊倒（**误报**）；
反过来，`json.contains("3.0")` 这种正向断言在值**真的消失**时也能通过 —— 只要别处出现同一个
子串（**漏报**）。一个断言同时具备"会冤枉"和"抓不到"两种毛病，等于没有断言。

改法：解析成类型，然后断言**字段**（`spot.cast_shadows == Some(true)`）。
第 54 轮新增的测试全部按这个写：`SpotLightDoc` 的往返测试在解析后逐字段断言，
"没勾的那一盏不许写出这个字段"则用 `text.matches("cast_shadows").count() == 1` 这种**有界**计数，
而不是"文件里没有这个词"。

## 46. 聚光灯阴影：一盏灯、一张图，和一条"零差异"的判据（第 54 轮）

聚光灯以前把光**穿过**一切：箱子底下、墙根全是亮的。这轮给**一盏**聚光灯加了阴影，
但真正花力气的是"怎么证明它长在对的地方"。

**验证分两层**。CPU 层是纯函数 `spot_shadow.rs`（透视阴影相机 + 偏移），CI 上就能跑：
光轴点落 NDC 中心、锥边界落 `1/1.01`、超出 `radius` 落远平面外、
**`near = 0` 时所有深度都塌成 1**（所以近平面不能是 0，这不是经验之谈）、
偏移折算成世界长度后与距离无关、挑灯下标与 `pack_local_lights` **逐位一致**。
GPU 层是 `tools/probe_spot_shadow.py`：10 张图、14 条断言，全部是量化判据。

**最重要的一条判据是"什么都不该变"**：不放遮挡物时，开投影必须**逐像素相同**
（实测 0/102400）。它不依赖"影子长在哪"，所以它是唯一能证明"相机与偏移本身没写错"的断言 ——
偏移不够时，地面会对自己投出满屏自阴影。第 1 条就抓到了 bug。

**探针抓到的那个 bug**：`pcf_3x3` 复用时把贴图写死成了平行光的 `shadow_map`，
于是聚光灯拿自己的 uv 去采**另一张**图。症状是"无遮挡物时 79% 的受光像素被压暗" ——
看画面只会得出"阴影比较写错了"这个错结论；把贴图改成参数传进去之后全绿。
**复用代码时，被复用函数里"隐含依赖的全局"就是最危险的地方**：签名上看不出它采的是哪张图。

**槽位也要在 GPU 上量一次**：CPU 侧有"挑选规则与 `pack_local_lights` 逐位一致"的测试，
但那只证明两个**函数**一致，不证明整条链路（Rust pack → uniform 下标 → 着色器里的 `i`）一致。
所以探针里多一组场景：在聚光灯**前面**插一盏不投影的点光源（占掉第 0 个槽位），
暗区必须还在原处（实测重心 (195.2, 161.6) 与没有点光源时完全相同，面积 3470 → 3469 px）。
下标算错时这一条会红，而"有个影子"这件事照样成立 —— 这正是它值得单独量的原因。

**尺子也错了一次**：`diff()` 里把"变暗/变亮"的标签写反了，于是"差异只有变暗"这条
被判成失败（打印"最大变暗 0.0、最大变亮 126.0"）。数字一直是对的，是读数字的方式错了 ——
与第 49 轮同一个坑。

**还有一条"物理上站不住"的判据**：我一开始要求"遮挡物挪到别处，暗区相对遮挡物的偏移不变"。
解析解是 `x_s - x_b = (h_floor - h_box)/(h_light - h_box) × (x_b - l_x)` ——
遮挡物离光轴越远，世界空间偏移**越小**，所以"偏移不变"是错的（实测 35.7 px → 18.1 px，
与 1.5 m → 0.5 m 的解析解方向一致）。换成两条能站住的：偏移随远离光轴而变小，
以及**镜像光源后偏移必须严格互为相反数**（场景与相机都关于 `x = 0` 对称，实测 +35.7 / −35.7 px）。
后一条比"面积差不多"强得多：V 翻转、uv 镜像、矩阵转置，任何一处镜像错都会破坏它。

**没做的**（同上，见 readiness §12.6.5）：只有一盏聚光灯能投影、点光源仍然不投影（要立方体阴影图）、
没有级联聚光阴影、半透明物体不参与阴影；编辑器里那个"投影"勾选框是按代码与 CPU 往返测试验收的，
**这一轮没有人在真实窗口里点过它**。

## 47. 只有 Windows 腿才会报的 WGSL 错误（第 54 轮）

聚光灯阴影提交并推上去之后，CI 的 `test (windows-latest)` 红了：12 个 CLI e2e 测试失败
（`bloom_is_wired_to_the_renderer`、`mcp_over_stdio_handshake_list_and_call`、
`movie_records_deterministic_frame_sequence`…… 看上去毫不相干）。真实原因只有一个：

```
In Device::create_render_pipeline, label = 'quench3d-opaque'
  Internal error in ShaderStages(FRAGMENT) shader: FXC D3DCompile error (Unspecified error (0x80004005)):
quench3d-pbr(168,34-84): warning X3570: gradient instruction used in a loop with varying iteration
```

`pcf_3x3` 里用的是 `textureSampleCompare`（要算**隐式梯度**），而它既在循环里、
又被聚光灯那段 `if (i == 投影灯)` 包着。**"非一致控制流里不许有隐式梯度采样"是 D3D/FXC 的硬限制**：
Windows 上 FXC 直接编译失败，于是整个不透明管线建不出来 —— 所有要渲一帧的 e2e 测试全部被带崩
（**"12 个测试红了"不等于"12 个 bug"**；CI 给你的失败清单只是"谁最先撞上这个管线"）。

修法：换成 **`textureSampleCompareLevel`**（naga 生成 HLSL 的 `SampleCmpLevelZero`，
不带梯度）。深度图只有 1 级 mip，LOD 0 与隐式 LOD 结果本来就应该完全相同 ——
实测也确实完全相同（探针数字一字未变：偏移仍 +35.7 px、暗区仍 3470 px）。
注释写在采样点旁边（`pbr.wgsl` 的 `pcf_3x3`），防止以后有人"顺手改回 `textureSampleCompare`"。

两条教训：

1. **本地渲得出正确的图 ≠ 这套 WGSL 在所有后端都编得过。** 第 49 轮立的规矩是
   "改过 WGSL 就必须真渲一遍"——它证明的是**本机 Vulkan 上的运行时行为**，
   证明不了**可编译性**。SPIR-V 与 MSL 对"隐式梯度在非一致控制流里"是宽容的（不诊断），
   只有 FXC 会拒绝。这两件事得分开验。
2. **五条腿里 windows 不是形式**。门禁（Linux）与本地 GPU 探针都抓不到这一类问题：
   门禁跑的是 Vulkan，wasm 腿只编 wasm32、不编译 D3D。**WGSL 一改，必须去看 windows 腿**；
   这也是为什么"推上去并确认五条腿全绿"是闭环的一部分，而不是可选项。

## 48. 点光源立方体阴影：把六个面变成一条等式（第 55 轮）

聚光灯阴影（第 54 轮）是**一张图**：一台相机、一个视锥、一套矩阵。点光源是**六个面**：
六个 90° 的相机、六个矩阵、六个 layer。多出来的工作量不在"渲染六次"（那是循环），
而在**面选择与面内 uv 的约定**：方向向量要先决定"落在哪一面"，再在那一面里算出 uv。

**为什么不用硬件的 cube 采样。** `texture_depth_cube` 一行就能采样，面序/u/v 镜像/±Y 的朝向
全由图形 API 规定。问题在于本仓库的 CI **没有显卡**：naga 只做语法与类型检查，
面内符号写反是**完全合法的 WGSL**，本地门禁全绿，只有某台机器上"某个面里的影子镜像了"
这一种症状。所以这一轮选了 `texture_depth_2d_array` + 手写约定，把"约定"变成**一条能在
CPU 上跑的等式**：

* `point_shadow::cube_face_uv(dir)` 是纯函数（面号 + uv），`face_view(layer, …)` 用**同一份**
  约定造六个面的相机；
* 主测试拿 2500+ 个方向 × 6 个面，断言"公式给的 `(面, uv)`" 与 "面矩阵投出来的 `(面, uv)`"
  逐个相等 —— 矩阵与公式任何一处不一致（转置、符号、上下翻转）都会红；
* 第二条测试从 WGSL 源里把六个分支的表达式**逐字抠出来**比对（面序、符号、层号），
  还断言面余量必须来自 uniform（WGSL 里出现 `1.01` 就红）。

**漂移探测器的红/绿是实测的**：把 `+X` 面里 `v` 的符号翻一下（`- dir.y` → `+ dir.y`），
两条防线同时红：

```
test renderer::tests::wgsl_cube_face_uv_mirrors_the_cpu_convention ... FAILED
  0 面的公式与 point_shadow.rs 不一致（找不到 0.5*(1.0+dir.z/ma/m),0.5*(1.0-dir.y/ma/m)）
```

GPU 探针也红（`±X` 两面本该互为镜像：偏移和 −1.00 px → **−7.77 px**，面积 7047 = 7047 → **6148 vs 7047**），
翻回来立刻全绿。**"有个影子"这件事在符号错的时候照样成立** —— 这就是为什么判据必须是
"镜像是否严格成对"，而不是"有没有暗区"。

**深度只跟主分量有关。** 六个面共用一组 `near`/`far`/偏移常量，因为写进去的是
`d = max(|dx|,|dy|,|dz|)`（垂直于被选中那一面的距离），不是到灯的距离。
有一条测试专门钉这个：同样的垂直距离在 `-Y`、`+X`、`+Z` 三个面上给出同一条深度曲线。

**探针照不到的面，用读回深度补。** 探针场景里灯的上方没有东西，`+Y` 面永远是空的，
所以"六个面"里探针实际只验了五个。另一件工具 `examples/point_shadow_diag` 直接读回六层
深度图：`+Y` 必须**一个纹素都没有**，`±X/±Z` 四面的统计量必须**逐位相同**
（32847 个纹素、重心 (0.499, 0.802)、最小深度 0.810444），`-Y` 是 0.808081 ——
差的 0.0024 正好是阴影管线那条 `slope_scale = 2.0` 多边形偏移（`-Y` 面里那个盒子的顶面
在视线上是正对的）。最后把 `+X` 面里盒子背面那 36 个点（深度跨 0.0631）与 CPU 对账：
平均差 0.004596、极差 0.002206。**如果 uv 镜像、面序或 `far/near` 有一处错，这批点会横跨
那 0.0631（差 10 倍以上）**——这比"看着对"强得多，也是这一轮唯一能验 `+Y` 面的手段。

**尺子又错了一次（第三次同一类）。** 探针第一版里"暗区重心在遮挡物远离灯那一侧"用的是
`occx_on.changed_mask(occx_off)` 当"遮挡物在屏幕上的位置"。但投影开关**只改变接收面的明暗**，
物体自己的位置不变 —— 那个掩膜就是影子本身，于是两个重心逐位相同（263.5 vs 263.4），
判据自己把自己判成了失败。正确做法是拿"有遮挡物 / 没有遮挡物"两张图比（第 54 轮就是这么写的）。
同一个场景里还有一条判据从一开始就"物理上站不住"：我要求 ±z 两面的暗区在画面里重叠，
但相机在 +z 侧，世界 z 翻转在画面里是**上下**翻转（+z 的重心 y=260.3、−z 是 108.3），
而且近的那一侧因为透视还大得多（偏移 100.3 px vs 51.7 px）。改成"落在画面中心上下两侧 +
x 重心一致 + 近侧更远"三条后，它才真的在量东西。

**共享 target 目录是个坑（新踩的）。** 这一轮同时在同一个仓库的另一个 worktree 上有人干活，
一开始两边共用 `target/`。症状是：渲染 crate 的测试全绿（新代码明明在），但 CLI 编译报
`no field cast_shadows on type quench3d::prelude::PointLight`（旧代码的字段列表）。
原因是**两个 worktree 的本地 crate 在共享 target 目录里被当成了同一个单元**
（同样的包名/版本/编译参数 → 同样的产物名与指纹），于是"我这边的 facade"实际链上了
"你那边的 render"。诊断依据是同一个测试二进制的哈希在两棵树里完全一样
（`quench3d_render-91f03bc8e2cda028`）—— 那一刻我跑的根本不是自己的代码。
隔离成 `.wt-r55/target` 后立刻恢复正常（CLI 的错误变回真错误、schema 测试也真的跑到我的源）。
**教训：并行开发时 `target/` 必须隔离；"测试绿了"在串味的构建里没有意义。**

**没做的**（见 readiness §12.7.5）：只有一盏投影点光源（多盏要阴影图数组 + 逐灯索引）；
面接缝靠 1% tan 余量 + 3×3 PCF，没有扫参；单面分辨率固定为聚光那张的一半，不能单独调；
半透明物体照旧不参与阴影；偏移常量按探针那套场景调的；编辑器那个勾选框仍然**没有人在真窗口点过**。

## 49. 房门装不上：一根轴在两端指向两个方向（第 56 轮）

**这一轮选的是 gap #15「关节锚点与局部轴」的第二段**（不是 #18 的形状集合）。依据：#15 在
§4 里是 **P0**（`docs/gap_analysis.md:619`），#18 只剩 Cone / 圆角盒 / HeightField，属 §4 的
P2 且要动物理内核（新 `Shape` 变体 → 支撑函数/惯量/窄相/渲染网格），本轮闭不了环；
#15 的引擎侧**早就齐了**（`Joint::Hinge { local_axis_a, local_axis_b }` 是两根独立的轴，
`hinge_angle` 也已经各用各的），缺的只是"格式没把它透出来"——典型的接线活。

### 48.1 现象：不是"缺能力"，是"格式逼你写错"

锚点（`anchor_a`/`anchor_b`）第 29 轮就接线了。剩下的坑是**轴只有一根**：装配时
`local_axis_a` 与 `local_axis_b` 都被赋成同一个 `axis`。这只在两端刚体**初始朝向相同**时
才等于"同一个世界轴"。门框平放、门板 `rotation_deg: [90,0,0]` 时，世界 Y 在门框局部是 Y、
在门板局部却是 Z —— 求解器的角度约束（`err = axis_a × axis_b`，增益 `0.2/dt` 夹到 60）
于是**先把门板扭平**再当铰链用。现象是"门装上去自己弹回原位"，而校验器一句话都不说。

### 48.2 做了什么（`axis_a` / `axis_b`）

* 格式：`joints[].axis_a` / `axis_b`（`Option<[f32;3]>`）。**不写 = 回落 `axis`**，
  所以老场景连"存盘后的字节"都不变（`skip_serializing_if`）。只写一端也合法。
* 校验三条（**复用**已有码，不新增码表条目）：零向量/非有限 → `out_of_range`（报在自己的
  `path` 上）；写在 `fixed`/`ball`/`distance` 上 → `ineffective`（球窝还额外指路
  `cone_limit.axis_a/b`）；**两端朝向相差 >10° 却只写 `axis`** → `suspicious_value` + 修法提示。
  最后这条是这轮的守门人：它把"静默装错"变成一条能 grep 的诊断。
* 三处 agent 自描述同步：`scene_grammar()["joint"]`、`$defs.joint.properties`、系统提示词。
* 编辑器**不用改代码**就跟着对了：`install_doc` 走 `runner::build_world`，预览与
  `quench3d render` 是同一份映射；存盘路径本来就会保留关节，这轮把 `axis_a/axis_b`
  也钉进 `scene_roundtrip` 的"编辑器不建模的段不许丢"清单里。

### 48.3 判据（怎么量出来的，而不是"看起来对了"）

`hinge_local_axes_can_differ_per_side`（无显卡，纯物理报告）：门板 `size [2,0.1,1]`、
`rotation_deg [90,0,0]`、铰链边在原点、初始状态**正好在约束流形上**（绕世界 Y 90°/s 的
圆周运动：`angular_velocity = [0,π/2,0]` 且 `velocity = [0,0,-π/2]`）：

| 写法 | 1 秒后质心 | 门板薄轴与世界 Y 的夹角 `\|n·Y\|` |
|---|---|---|
| `axis_a=[0,1,0]` + `axis_b=[0,0,1]`（按端） | 转过 **80.4°**、半径 1.000 m、y 漂移 <0.02 | **0.0002**（还是立着的门） |
| `axis_a=[0,1,0]` + `axis_b=[0,1,0]`（= 旧格式唯一能写的） | 半径仍 1.000 m | **1.0000**（门板被扭平） |

两者相差**三个数量级**，而且点约束在两种写法下都成立（半径都是 1.000 m）——
说明差别确实只出在"轴"上。同一条用例还断言：**只写 `axis`** 与"把 `axis` 显式抄到
`axis_a`/`axis_b`"的 `states` **逐字节相同**（"不写 = 老行为"的直接证据）。

### 48.4 变异验证（改坏 → 红 → 改回 → 绿）

| 变异 | 变红的测试 | 原文 |
|---|---|---|
| 铰链 B 端改回用 A 端的轴（回到"两端共用"） | e2e `hinge_local_axes_can_differ_per_side` | `panicked at cli_e2e.rs:2525`（门板薄轴断言） |
| `$defs.joint` 里删掉 `axis_a` | `json_schema_matches_the_source_structs` | `JSON Schema 的 joint 少了字段 axis_a（结构体改了要同步改 schema）` |
| 把朝向差阈值抬到 100000°（等于摘掉这条诊断） | `joint_local_axes_are_per_side_and_diagnosed` | `panicked at scene_doc.rs:6747: 朝向不同必须提醒` |

### 48.5 门禁与计数

**门禁**：`tools/gate.sh` **七段全绿**，**509 个测试（0 失败）**，wasm 11,920,909 字节。
本轮新增 **2** 条测试（`scene_doc` 单元 1 + e2e 1；编辑器那条是**并进已有用例**的断言，
不计入新增），所以改动前的总数应当是 507 —— 这个数没有直接量过，是从增量倒推的。
本会话新增的计数块：README 的 workspace 总数 498 → 509、CLI 152 → 163（lib 73 → 82、
`cli_e2e` 52 → 54、`agent_tools` 6 → 5、`steady_state_allocations` 2 → 1 —— 后两个是
**旧数字过期**，不是本轮改动的）；**没有重算**的 README 计数块：`api_tests.rs` 3 个、
`island.rs` 6 个、`digest_tests` 4 个、`ask_repair.rs` 3 个、`llm_mock.rs` 9 个、
以及物理/渲染/编辑器那几段的数字。

### 48.6 一个真实教训：`1.5707963` 过不了 clippy

第一次跑门禁时 `clippy -D warnings` 在**两条腿上都红**：
`error: approximate value of f{32,64}::consts::FRAC_PI_2 found`（`cli_e2e.rs:2463,2464`）。
测试直接 `cargo test` 是绿的 —— `approx_constant` 只在 clippy 下 deny。改成
`std::f64::consts::FRAC_PI_2` 后绿。**"测试跑过了"不等于"门禁会过"**，
这也再次说明为什么必须跑完整七段而不是只跑 `cargo test`。

### 48.7 诚实边界（没做的）

* `local_frame_a/b`（固定关节的"参考姿态"）**仍只存不用**：引擎的固定关节是速度级角约束、
  锁定创建时的相对姿态，要"把 B 扭到指定姿态"必须先改 `crates/quench3d-physics` 的角约束
  —— 不在本轮允许改动的接线范围内（写进了 `docs/gap_analysis.md` §3.2 #15 与
  `docs/production_readiness.md` §10.4）。
* 关节可视化（轴/锚点 gizmo）没有；编辑器里仍然只能靠数字调。
* 球窝的 `cone_limit.axis_a/b` 是另一套字段（第 23 轮就有），这轮只在"写错地方"时给了指路提示。
* 顺带发现但**没动**：README §6.3 那个大 JSON 例子的 `joints` 引用了 objects 里不存在的
  `门`/`框`/`闸门`/`躯干`/`手臂`（那段带 `//` 注释、本来就不是可校验的 JSON）；这是既有的
  文档漂移，改它要重写整段例子，不在本轮范围。

## 50. 两条会波及其他 agent 的环境教训（第 55 轮）

这一轮是在**同一个仓库的另一棵 worktree 正被另一个 agent 写入**的情况下做的，
踩到两条与代码无关、但会让人得出错误结论的坑。写在这里，因为它们比这一轮的功能更容易再犯。

### 50.1 多 worktree 必须各自独立 `target/`（或显式设 `CARGO_TARGET_DIR`）

实测症状：渲染 crate 的测试全绿（新代码明明在），但 CLI 编译报

```
error[E0609]: no field `cast_shadows` on type `quench3d::prelude::PointLight`
  available fields are: position, color, intensity, radius
```

也就是"我的 CLI 链上了**另一棵树**的 `quench3d-render`"。原因：两棵树共用同一个 `target/`，
而本地 crate 的产物名与指纹只由（包名、版本、编译参数）决定 —— 路径不参与，
于是两棵树里的同名 crate 在 cargo 眼里是同一个单元。

**判据（一眼可查）**：同一棵 crate 的测试二进制哈希在两棵树里完全相同
（当时是 `quench3d_render-91f03bc8e2cda028`）。看到这个就说明"你跑的不是自己的代码"。

**结论：共享 `target/` 时"测试绿了"没有意义。** 并行开发时每棵树自带 `target/`，
或者用 `CARGO_TARGET_DIR` 指到各自目录。这一轮把构建隔离到 `.wt-r55/target` 之后，
CLI 的错误立刻变回**真错误**（我自己的字段没接上），schema 测试也才真的跑到我的源。

### 50.2 共享工作树时的纪律：一个写者、显式路径、禁用 `reset --hard` / `clean`

* **同一时刻只有一棵树的主分支可以有一个写者**。这一轮开始时，主树 `quench3d/` 上
  另一条线正在提交 #15/#16 的工作；我先前在主树里的改动被一次 `git reset --hard` 抹掉，
  于是整轮工作改在 `.wt-r55` 里做完（结果证明这是对的）。
* **提交只用显式路径**（`git add <文件…>`，永不 `git add -A`）：主树里随时可能有别人的
  未提交产物（`web/pkg/*`、文档草稿）。
* **任何人在场时禁止 `git reset --hard` / `git clean -fd`**：本会话真的抹掉过别人 20 分钟的
  未提交工作（被抹的一方只看到"我的文件不见了"）。

这三条不是流程洁癖：它们造成的是**错误的绿色**（50.1）和**静默的数据丢失**（50.2），
两种都比一次构建失败贵得多。
## 51. GL/WebGL2：一条"建不出来"的管线，和一段本机抓不到的 bug（第 57 轮）

这一轮不是做功能，是**用户实测到的崩溃**：Web 编辑器在 GL/WebGL2 后端（`downlevel`）上
**启动即 panic** —— 一帧都没画出来。

### 51.1 症状与原文

```
[ERROR] Shader translation error for stage ShaderStages(FRAGMENT): WGSL `textureLoad` from depth textures is not supported in GLSL
panicked at wgpu-30.0.1/src/backend/wgpu_core.rs:1414:
  In Device::create_render_pipeline, label = 'quench3d-post'
    Internal error in ShaderStages(FRAGMENT) shader: WGSL `textureLoad` from depth textures is not supported in GLSL
后端选择：gl（navigator.gpu 存在但没有适配器）；渲染设备：ANGLE (Intel HD Graphics 630) OpenGL ES 3.2 / Gl（downlevel_webgl2）
```

wasm 上 wgpu 把管线校验错误交给未捕获错误处理器 → **panic**。所以这不是"效果没画出来"，
是编辑器直接死：所有 `Renderer::new` 之后的代码都没机会跑。

### 51.2 为什么本地和门禁都抓不到（和第 54 轮是同一个形状）

第 54 轮是 **FXC**（Windows）拒绝了 `textureSampleCompare`；这一次是 **GLSL**（GL/WebGL2）
拒绝了 `textureLoad`。两次的共性：

| | 第 54 轮 | 本轮 |
|---|---|---|
| 谁拒绝 | D3D/FXC（HLSL） | naga 的 GLSL 后端 |
| 本地（Vulkan） | 全绿 | 全绿（Vulkan 没有这条限制） |
| Linux 门禁 | 全绿 | 全绿（同上） |
| wasm 腿 | 只编、不跑 | 只编、不跑（**翻译发生在运行期建管线时**） |
| 谁先发现 | CI 的 windows 腿 | **用户** |

教训与第 54 轮同一条，但这次连"CI 五条腿"都没救下来 —— 因为浏览器里的 WebGL2 不在 CI 里。
**"本地渲得出正确的图"与"这套 WGSL 在某个后端编得过"是两件事，必须分开验。**

### 51.3 为什么"改成采样读深度"这条首选路走不通（三条都试过）

先按任务书的首选方案试了采样，一条条都被 naga 挡回来（`naga-30.0.1/src/back/glsl/writer.rs`）：

| 写法 | 结果 |
|---|---|
| `textureLoad(depth_texture, coord, 0)` | `Error::Custom("WGSL \`textureLoad\` from depth textures is not supported in GLSL")` |
| `textureSampleLevel(depth_texture, sampler, uv, 0.0)`（**非比较**采样器） | naga 的 WGSL 校验先拒：`InvalidSampleLevelExactType` |
| `textureSampleCompareLevel(depth_texture, sampler_comparison, uv, ref)` | 能翻（生成 `textureLod(sampler2DShadow, vec3, 0.0)`），但**返回 0/1 的比较结果**，拿不到深度值 |

根因是 GLSL 后端把 `texture_depth_2d` **一律**映射成 `sampler2DShadow`（`writer.rs:599`），
"非比较地读深度"在 naga 里根本没有出口。所以 GL 上**只能按能力关掉**依赖深度的效果 ——
这正是任务书里"若某个效果在 GL 上根本做不到，就优雅降级"的那条分支。

### 51.4 修法：能力判定 + **在建管线之前删源码**

| # | 落点 | 作用 |
|---|---|---|
| 1 | `context::supports_depth_texture_read(backend)` | 唯一判据（只有 `Gl` = false）；`Renderer::new` 里定一次 |
| 2 | `renderer::post_wgsl_for(ssao)` | 把 `post.wgsl` 里 `//@gl-depth-begin`…`//@gl-depth-end` 圈出的**三段**（深度绑定、`ssao_factor` 一族函数、`fs_main` 调用点）整段删掉 |
| 3 | `depth_resolve_pipeline: Option<_>` + `Renderer::supports_ssao()` | GL 上不建"读多采样深度"那条管线，也不建解析用的深度目标 |
| 4 | `context::effective_msaa_samples(backend, requested)`（GL 上恒为 1，唯一调用点是 `GpuContext::build`，并打 `log::warn!`） | 多采样纹理根本不建 —— 那条 `glow` 的 `panic!` 永远走不到；警告让"我的 MSAA 怎么没生效"在日志里有答案 |

关键判断：**"运行时把 `ssao.x` 置 0"救不了场**。GLSL 的翻译发生在 `create_render_pipeline`，
naga 会把整个模块翻一遍、不理会运行期分支；只要 WGSL 里还留着 `textureLoad(depth_texture, …)`，
`quench3d-post` 就建不出来。**必须在"交给驱动之前"删掉那段源码。**
删掉之后 tonemap / bloom / FXAA 一行不动（在 Vulkan 上强制走 GL 变体渲同一场景，
输出**逐字节相同** —— 因为默认场景本来就没开 SSAO，这也顺带证明变体只删了该删的）。

第 4 行是**半路杀出来的**（见 51.5 的 ⑤）：第一条修掉之后，真实 WebGL2 里立刻死在 MSAA 上。
它们的形状完全一样 —— **"GL 后端不支持这个能力，而引擎缺省就在用它"**，
区别只在一条是语言后端拒绝翻译、一条是库里的 `panic!`。所以修法也一样：
判定做成**纯函数**（不需要显卡就能测）、调用点**只有一个**、
并且让用户看得见降级（警告日志）而不是偷偷降。

四个开关必须**同时**成立：变体（源码里没有）、能力判定（不建管线）、绑定（绑占位深度视图）、
MSAA 夹到 1。少任何一个都会回到"要么建不出来、要么读了没绑的资源、要么直接 panic"。

### 51.5 判据

**① 本地复现（同一台机器、同一条命令，修前/修后）**

```bash
WGPU_BACKEND=gl cargo run -p quench3d-render --example offline_render
```

* 修前：`panicked at ... In Device::create_render_pipeline, label = 'quench3d-post'`
  —— 与用户浏览器里**同一个 label、同一句 naga 原文**（exit 101）
* 修后：exit 0，`target/screenshots/offline_render.png` 落盘

这条路能走通是因为本机有 Mesa 的 GL（llvmpipe，`WGPU_BACKEND=gl` 能拿到适配器）。

**② 不需要显卡的守卫（CI 的 linux/windows 腿能跑；macOS 腿不行，见本节末）**：直接调 naga 的 GLSL 后端翻译
——`wgpu::naga`（wgpu 30 在 native 上无条件重导出），
`gl_pipelines_all_translate_through_the_glsl_backend` 把"GL 上会建的每一条管线"的每个入口点
在 **WebGL2 GLSL ES 3.00** 与**桌面 3.30 core** 两档上真翻一遍。
它挡的是"这条管线在 GL 上建不出来"，而不是"这段代码看起来像有问题"。
（**平台门槛**：`naga/glsl-out` 跟着 wgpu 的 gles 后端走，只在 windows / linux / android /
freebsd / emscripten 上被编进来。第一版我没加这个 cfg，推上去之后 CI 的 **macOS 腿**红了 ——
`error[E0432]: unresolved import wgpu::naga::back::glsl`。这不是"误报"，是**守卫写到了
没有这个后端的平台上**：macOS 上 wgpu 走 Metal，`naga::back::glsl` 那个模块根本不存在。
修法是按 wgpu 的 target 表收窄，而不是删掉它 —— 删掉等于把这类事故的本地守卫全丢了。）
同一层里还有 `gl_backend_is_declared_unable_to_use_msaa`：MSAA 的判定被做成了**纯函数**
（`context::effective_msaa_samples`），所以它也不需要显卡 —— GL 上任何请求值（0/1/2/4/8/64）
都必须落在 1，而 Vulkan/Metal/DX12/WebGPU 上必须原样保留用户的设置（4 就是 4）。
**能力判定要能被穷举地测**，这是它半年后还靠得住的原因。

**③ 变异红-绿**（把 `post_wgsl_for` 的 GL 分支改回"返回原文"）：

```
post.wgsl(gl 变体) 在 webgl2 上建不出管线：入口点 fs_main（Fragment）翻译失败：
Custom("WGSL `textureLoad` from depth textures is not supported in GLSL")
```

改回来 → 3 条守卫全绿。**守卫会在"把 bug 改回来"时准确地叫出来**，这是它可信的前提。

**④ GL 上的整套渲染测试**：`WGPU_BACKEND=gl cargo test -p quench3d-render --no-fail-fast`
→ **82 passed / 0 failed**（修前：任何建 `Renderer` 的测试都在 `quench3d-post` 上 panic）。

**⑤ 真实 WebGL2（这一层是修完前四条之后才补上的，也最关键）**：
`tools/probe_webgl2.sh`（headless Chrome + SwiftShader + `?backend=gl`，
判据 = 日志出现「编辑器第一帧已渲染」且页面遮罩被收掉）。

| | 修前（基线 wasm） | 修后 |
|---|---|---|
| 后端 | `gl（URL 指定 backend=gl）` / `Gl（限制档位 downlevel_webgl2）` | 同左 |
| 日志 | `[ERROR] Shader translation error ... textureLoad from depth textures ...` → `panicked ... label = 'quench3d-post'` | `[WARN] 后端 Gl 不支持多采样纹理：MSAA 从 4 降到 1` → `[INFO] 编辑器第一帧已渲染：1280x800` |
| 页面 | 遮罩停在「正在创建渲染面…」 | 遮罩收起，1280x800 截图均值 `[122.3,127.4,134.9]`，中央视口 `[158.3,162.5,174.5]`（与离屏渲染同量级） |
| 探针 | FAIL（0 帧 + panic） | **PASS**（0 次 panic） |

**这一步抓到了第二条命**（也说明"只修看得见的那一条"是不够的）：深度那条修掉之后，
真实 WebGL2 里紧接着变成

```
panicked at glow-0.17.0/src/web_sys.rs:4307:9: Tex storage 2D multisample is not supported
```

—— 附录：编辑器缺省 `render.msaa = 4`，而 wgpu-hal 的 gles 后端对 `sample_count > 1`
的纹理就调 `gl.tex_storage_2d_multisample`，`glow` 的 WebGL2 实现里那个函数是 `panic!`
（**与浏览器、显卡都无关**；原生 GL 上不 panic，但画面全黑）。
于是它和深度那条一样是一条"GL 上必死"的路，只是**排在后面**：修了第一条才露出来。
修法与判据见 51.4 的第 4 行与 51.5 的 ②⑤。

### 51.6 边界（这一节别读成"GL 后端已经无所不能"）

1. **SSAO 在 GL 上是"没有了"，不是"修好了"**：深度-only SSAO 依赖"按像素读深度"，
   而 GL 上这条路是断的。要恢复得换实现（例如把深度渲染进一张颜色目标再读，多一趟全屏 pass）。
2. **MSAA 在 GL 上是"关掉"，不是"修好"**：`msaa_samples` 夹到 1，用户设 4 也会打警告后降级。
   GL/WebGL2 上想要抗锯齿只能用 FXAA。
3. **真机仍然只有两台（Vulkan 的 llvmpipe + WebGL2 的 SwiftShader），都是软件光栅**。
   用户那台是 ANGLE/Intel HD 630：能确定的是**管线建得出来、第一帧画得出来**
   （同一套 wgpu gles 后端 + 同一个 glow 代码路径；MSAA 那条 `panic!` 更是与驱动无关），
   不能确定的是真机驱动上的其它差异。**最终确认只能由用户刷新页面给出。**
4. **没查的**：原生 GL 上 MSAA 解析出黑帧的根因（在 wgpu-hal 的 gles 路径或 llvmpipe 里）。
   现在 GL 上不建多采样纹理，这条路径在项目里再也走不到；要挖它得改 `wgpu-hal`（不在围栏内）。

---

## 52. CI 的 windows 假红：一条接不住原生崩溃的 `Err`（第 58 轮）

### 52.1 症状原文（先钉原文，别再重新猜）

这类假红在最近 12 次 CI 里出现过 **2 次**，两次是**不同的测试二进制**、同一个退出码：

```
run 38935053883 @ cf52cd8
error: test failed, to rerun pass `-p quench3d-editor --test model_instances`
Caused by:
  process didn't exit successfully: `...model_instances-8259af2638c9b300.exe`
  (exit code: 0xc0000005, STATUS_ACCESS_VIOLATION)
```

```
run 38931539230 @ 1c873ad
error: test failed, to rerun pass `-p quench3d-render --test backend_fallback`
Caused by:
  process didn't exit successfully: `...backend_fallback-8cfe002d8085f3e7.exe`
  (exit code: 0xc0000005, STATUS_ACCESS_VIOLATION)
```

已有的旁证（不是猜的）：两次都是 `0xc0000005`；`quench3d-render` **不依赖** `quench3d-cli`
（排除"某个共享依赖在 windows 上炸"）；**同 sha 重跑即绿**；最近 12 次 CI 里 windows 腿只红过
这 2 次（另一次是真的 FXC 编译错误，不是这一类）。两个二进制的共同点只有一个：
**它们都在测试里"真的创建 wgpu 上下文"**（`GpuContext::headless` / `from_candidates_with`）。

结论：**在没有 GPU 的 Windows runner 上创建真实 wgpu 上下文会原生崩溃**。

### 52.2 为什么 `Result::Err` 接不住它

这两个文件里原本就写着"没 GPU 就跳过"：

```rust
let ctx = match pollster::block_on(GpuContext::from_candidates_with(/* … */)) {
    Ok(ctx) => ctx,
    Err(e) => { eprintln!("跳过：没有可用的 GPU（{e}）"); return; }
};
```

这条路的前提是"没有 GPU"会**返回** `Err`。而在无 GPU 的 Windows 上，创建真实上下文
（DX12/DXGI 那条路）是**原生访问违例**：进程直接被操作系统打死，Rust 侧**没有机会执行
任何代码** —— 既没有 `Err`，也没有 `panic`，连析构都跑不到。测试框架看到的因此不是
"一条断言失败"，而是 `process didn't exit successfully ... (exit code: 0xc0000005)`。

**判据**：失败形态是"进程退出码 + `process didn't exit successfully`"而不是
"`test result: FAILED` + 一条断言"，就说明故障点在 Rust 能观测到的范围**之外** ——
`Result` / `catch_unwind` / `panic::set_hook` 一个都拦不住。

### 52.3 为什么"重跑才是判据"不作为首选

同 sha 重跑确实会绿（这类崩溃是环境相关的偶发），但把重跑当判据有三个问题：

1. **它把"真红"也一起放过去了**：判据一旦是"重跑一次"，任何偶发红都能被消掉，包括真 bug
   造成的偶发红。这两次红已经害我们两次把真绿当真红、或把真红当假红来回折腾。
2. **它不可复核**：CI 历史里只留下一条红，没有任何人能证明"它为什么红"。
3. **它掩盖了"覆盖率本来就是 0"**：windows runner 没有 GPU，那几条测试在 windows 上
   本来就只能走"没 GPU 就 return"的假绿分支 —— 它们**从来没在 windows 上测过任何东西**。
   重跑只是把"什么也没测"标成绿色。

所以本轮改的是"把事实写进代码"：这几条测试**默认不跑**，只在显式打开时跑。

### 52.4 采用的手段：显式 opt-in + 按腿给开关

`crates/quench3d-render/src/context.rs` 加了一个小 helper（1 个常量 + 4 个函数）：

| 名字 | 作用 |
|---|---|
| `GPU_TESTS_ENV` | 环境变量名 `QUENCH3D_GPU_TESTS` |
| `gpu_tests_enabled_from(Option<&str>)` | 纯函数：**只有字面量 `"1"` 算打开**（未设 / `""` / `"0"` / `"true"` 都是关） |
| `gpu_tests_enabled()` | 读一次环境变量 |
| `run_if_gpu_tests_enabled(开/关, 原因, f)` | 判定内核：**关着就根本不调用 `f`**，打印可读原因并返回 `None` |
| `with_gpu_tests(原因, f)` | 真实调用点 = `run_if_gpu_tests_enabled(gpu_tests_enabled(), …)` |

用法是"把**整条测试体**放进闭包"，而不是在体里加个 `if`：

```rust
let _ = with_gpu_tests("空候选列表要退回默认候选（真的创建 wgpu 上下文）", || {
    // …原来的整条测试体（含 pollster::block_on(...) 那一步）…
});
```

`model_instances.rs` 里 `headless_engine(原因)` 把**创建上下文那一步**放进了闭包内部，
所以"没打开开关"时 wgpu 那边**一个函数都不会被调用**。

CI（`.github/workflows/ci.yml`）按腿给这个变量：

| 腿 | `QUENCH3D_GPU_TESTS` | 为什么 |
|---|---|---|
| `test (ubuntu-latest)` | `1` | 覆盖不能丢；开关按"**这条腿的属性**"给，而不是按平台。**实测 ubuntu-latest 没有适配器**，所以这两条断言在该腿上是空的（见 52.6 ⑦）—— 这与改动**之前**完全一样，没有丢覆盖，而是"本来就是 0" |
| `test (macos-latest)` | `1` | 同上；实测真的建出了上下文（Metal，0.22s / 0.17s）—— 这两条测试在 CI 里的实际断言覆盖来自这条腿 |
| `test (windows-latest)` | `0` | 没有 GPU，建上下文 = 原生崩溃 |

`tools/gate.sh`（本地门禁）默认 `1`（本机有显卡，覆盖不能丢）；没有显卡的机器用
`QUENCH3D_GPU_TESTS=0 tools/gate.sh` 关掉，步骤标题里会印出当前取值。

### 52.5 为什么不那样做

- **不用 `cfg!(windows)` 硬编码跳过**：那会让**本地 Windows 开发者**也失去这部分覆盖 ——
  他们的机器是有显卡的。这里要区分的从来不是"平台"，而是"这条腿能不能建出上下文"。
- **不在"检测 GPU"上做文章**：崩溃就发生在检测/创建那一步（DXGI 枚举适配器），
  Rust 侧接不住；"先探测有没有 GPU 再决定要不要建"只是把崩溃点往前挪一点，问题不变。
- **不记成 known-flake / 靠重跑**：理由见 52.3。
- **不把开关下沉进 `GpuContext::headless`**：那会让 CLI 的 `render` / `movie` 在用户机器上
  依赖一个测试开关（默认关 → 所有人都渲染不出来）。开关只能留在测试里。

### 52.6 证据（本机 Linux + 有 GPU，工具链 = CI 钉的 1.99.0）

**① 不设变量**（题目要求的命令，`--offline`）

```
$ env -u QUENCH3D_GPU_TESTS cargo test -p quench3d-render --test backend_fallback --offline
running 7 tests
test empty_candidate_list_does_not_panic_and_uses_defaults ... ok
test gpu_gate_builds_nothing_unless_explicitly_enabled ... ok
test hint_gl_means_gl_only ... ok
test hint_webgpu_prefers_webgpu_but_keeps_gl_as_backstop ... ok
test hinted_candidates_are_compiled_in ... ok
test unknown_or_missing_hint_falls_back_to_default_candidates ... ok
test unusable_first_candidate_falls_through_to_the_next ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

跳过原因在同一条命令加 `-- --nocapture` 后可见（**cargo 默认会把通过用例的输出吞掉**，
这是测试框架的行为，不是我们藏起来）：

```
跳过：空候选列表要退回默认候选（真的创建 wgpu 上下文） —— 未打开 QUENCH3D_GPU_TESTS=1。…
跳过：第一个候选不可用时回退到下一个（真的创建 wgpu 上下文） —— 未打开 QUENCH3D_GPU_TESTS=1。…
```

**② 设了变量**（同一条命令）：真的跑，0.14s（对比关掉时的 0.00s）

```
$ QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test backend_fallback --offline
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.14s
```

`--nocapture` 下**没有**"没有可用的 GPU"那行 → 断言真的在真上下文上跑了
（打开开关时唯一一条"跳过"是守卫测试自己故意喂 `false` 的那个反例）。

**③ 编辑器那两个态**（`model_instances`，5 条）

```
$ env -u QUENCH3D_GPU_TESTS cargo test -p quench3d-editor --test model_instances --offline
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
$ QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test model_instances --offline
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.96s
```

关掉时 `--nocapture` 能看到 **5 条各自的原因**（"未打开 `QUENCH3D_GPU_TESTS=1`"），
打开时 **0 条**跳过。

**④ 守卫测试**（`gpu_gate_builds_nothing_unless_explicitly_enabled`，**不需要显卡**）：
它**不读写进程级环境变量** —— `cargo test` 的测试跑在同一进程的多个线程里，改环境变量会污染
同进程的其它测试（在无 GPU 的 Windows 上，那等于把原生崩溃放出来）—— 而是直接喂
`run_if_gpu_tests_enabled` 的 flag：关着的时候闭包（= "建上下文"的替身）**一次都不执行**，
打开时恰好一次且结果透传；另外还有"只有字面量 `1` 算打开"的 5 条断言。

**⑤ 变异红-绿**（改坏 → 红 → 改回 → 绿，三条独立）

| 变异 | 结果 |
|---|---|
| `gpu_tests_enabled_from` 改成恒 `true` | 红：`没设变量必须是关`（`backend_fallback.rs:43`） |
| 判定内核 `if !enabled` 改成 `if false`（关着也执行闭包） | 红：`关着的时候必须返回 None（= 跳过）`（`backend_fallback.rs:62`） |
| 开关**打开**时把 `assert_eq!(ctx.size(), (32, 32))` 改成 `(1, 1)` | 红：`assertion left == right failed`（`backend_fallback.rs:157`）→ 证明打开时测试体真的在跑 |
| 同上那条坏断言、但**不设**开关 | **绿** → 这就是"跳过会藏住坏断言"的机理本身 |

最后一行是本轮唯一要承认的代价：**跳过就是跳过，它不可能抓到断言写错**。所以开关必须在
"能建出上下文"的腿上保持打开（ubuntu / macOS / 本机门禁默认）—— 而"能建出上下文"
是**这条腿的属性**，不是平台的属性。

**⑥ 门禁（`tools/gate.sh`，七段）**：七段全 `PASS`，`测试总数：533（0 失败）`
（本轮新增 1 条不需要显卡的守卫测试：532 → 533），`wasm 字节：11961808`
—— **与上一轮逐字节相同**（新 helper 在 wasm 侧是死代码，被裁掉了）：这个开关没有给
Web 产物带来任何体积代价。门禁步骤标题现在会印出 `QUENCH3D_GPU_TESTS=1`。

**⑦ 上线实测：CI run `38955064316` @ `2a0f0db`，`attempt = 1`，五条腿全绿**

| 腿 | 结果 | 这两条测试在日志里的实测原文 |
|---|---|---|
| `test (windows-latest)` | ✅ **首跑绿** | `Running tests\model_instances.rs (target\debug\deps\model_instances-8259af2638c9b300.exe)` → `test result: ok. 5 passed; 0 failed; … finished in 0.00s`；`Running tests\backend_fallback.rs (…\backend_fallback-8cfe002d8085f3e7.exe)` → `test result: ok. 7 passed; 0 failed; … finished in 0.00s`。两个**可执行文件名与上一轮崩溃的那两个逐字符相同**（`model_instances-8259af2638c9b300.exe` / `backend_fallback-8cfe002d8085f3e7.exe`），全日志里 `process didn't exit successfully` 与 `STATUS_ACCESS_VIOLATION` 出现 **0 次** |
| `test (ubuntu-latest)` | ✅ | `Running tests/model_instances.rs` → `ok. 5 passed … 0.00s`；`Running tests/backend_fallback.rs` → `ok. 7 passed … 0.00s`。**0.00s 说明走的是内层"没有可用的 GPU"分支** —— 同一条腿的 CLI 冒烟也打印 `本机没有可用 GPU，渲染按约定返回退出码 3`。即：目标**真的在跑**（5 条 / 7 条都执行），但这几条 GPU 断言在 ubuntu 上仍是空的（改动前也一样） |
| `test (macos-latest)` | ✅ | `ok. 5 passed … 0.22s`（`model_instances`）、`ok. 7 passed … 0.17s`（`backend_fallback`）→ **真的建出了上下文**（Metal），断言真的跑了 |
| `wasm32-unknown-unknown` | ✅ | — |
| `msrv (1.90)` | ✅ | — |

另外：那条**不需要显卡的守卫测试在 windows 腿上也真的跑了**且绿
（`test gpu_gate_builds_nothing_unless_explicitly_enabled ... ok`）—— 机制本身在没有 GPU 的平台上也被验证过一次。

### 52.7 边界（没做的、没验证的）

1. **本机没有无 GPU 的 Windows 环境，复现不了那次原生崩溃**。本机是 Linux + 有 GPU；
   52.6 的 ①②③ 都是在**能建出上下文**的机器上做的，它们证明的是"开关真的控制着建不建"，
   不是"windows 上不再崩"。后者只能由 CI 的 windows 腿首跑结果来证。
2. **只围了这两个测试二进制**（`backend_fallback` / `model_instances`）—— 它们是两次假红的
   当事者，也是本轮围栏允许改动的文件。仓库里还有其它"真建 wgpu 上下文"的测试二进制：
   `post_pipeline.rs`、`shadow_cascades.rs`、`material_pipeline_traits.rs`、
   `fog_and_shadow_quality.rs`、`point_lights.rs`，编辑器侧 `hierarchy.rs` /
   `scene_roundtrip.rs` / `history.rs` —— 它们**仍在 windows 腿上直接建上下文**，
   同样的原生崩溃在它们身上依然可能发生，只是最近 12 次 CI 恰好没轮到。
   这是**明确记下的欠账**，不是"已经普遍修好"。（同一条 helper 已经就位，
   后续每个文件只要把 `headless()` 那一步包进 `with_gpu_tests` 即可。）
3. **windows 腿上这几条测试的覆盖永久是 0**（runner 没有 GPU，本来也测不了）。
   本轮做的是"把 0 覆盖写成 0 覆盖并留下可读原因"，不是"把 0 覆盖变成有覆盖"。
   要真有覆盖只能换一台带 GPU 的 Windows（或自托管 runner）。
4. **ubuntu 腿上的这几条断言实际上也是空的**（52.6 ⑦ 实测 0.00s + 同腿 CLI 冒烟报"没有可用 GPU"）。
   本轮**没有**在 ubuntu 腿装软件光栅（llvmpipe / lavapipe）—— 那是"把 0 覆盖变成有覆盖"的
   下一步，要单独验证（装上之后 wgpu 真的拿得到适配器吗？断言在软件光栅上成立吗？）。
   本轮的目标只是"别再制造无法诊断的假红、别丢已有覆盖"，所以不把两件事混在一起。
   CI 里真有断言覆盖的是 macOS 腿（Metal）与本机（Linux + GPU）。

## 54. 一次把同类风险清干净：8 个裸奔的测试二进制，和一条会自己发现新裸奔者的守卫（第 58 轮·下半场）

§52 只修了**一个**假红（两个测试二进制）。这一节处理**同类**的另外 8 个，并加一条
**不需要显卡**的守卫，让"将来再写一个裸奔测试"这件事在 CI 上**自动变红** ——
只把这次的 8 个补上，几个月后还会长回来（这正是欠账的成因：第 58 轮上半场按围栏只改了 2 个）。

### 54.1 清单：仓库里"真的建 wgpu 上下文"的测试二进制一共 10 个

| # | 文件 | 建上下文的入口 | 本轮前 |
|---|---|---|---|
| 1 | `crates/quench3d-render/tests/backend_fallback.rs` | 2 处 `from_candidates_with` | 上半场已包 ✅ |
| 2 | `crates/quench3d-editor/tests/model_instances.rs` | `headless_engine()` | 上半场已包 ✅ |
| 3 | `crates/quench3d-render/tests/post_pipeline.rs` | `setup()` + 4 处内联 | 裸奔 ❌ → 本轮包 |
| 4 | `crates/quench3d-render/tests/shadow_cascades.rs` | `Scene::new()` | 裸奔 ❌ → 本轮包 |
| 5 | `crates/quench3d-render/tests/material_pipeline_traits.rs` | `setup()` | 裸奔 ❌ → 本轮包 |
| 6 | `crates/quench3d-render/tests/fog_and_shadow_quality.rs` | `setup()` | 裸奔 ❌ → 本轮包 |
| 7 | `crates/quench3d-render/tests/point_lights.rs` | `setup()` | 裸奔 ❌ → 本轮包 |
| 8 | `crates/quench3d-editor/tests/hierarchy.rs` | `headless_engine()` | 裸奔 ❌ → 本轮包 |
| 9 | `crates/quench3d-editor/tests/history.rs` | `headless_engine()` | 裸奔 ❌ → 本轮包 |
| 10 | `crates/quench3d-editor/tests/scene_roundtrip.rs` | `headless_engine()` | 裸奔 ❌ → 本轮包 |

**断言一条都没删、没放宽**：改的只是"建上下文那一步在不在闭包里面"。
`post_pipeline.rs` 里 4 处内联的建上下文抽成了一个 `context(msaa, what)` 小函数（同一个文件里
5 处建上下文，跳过原因要能分辨是哪一处，所以 `what` 是参数）；其余 7 个文件都是把
`setup()` / `Scene::new()` / `headless_engine()` 里那一句包进 `with_gpu_tests`，**函数签名不变**，
调用点不动。没设 `QUENCH3D_GPU_TESTS=1` 时**闭包一次都不执行**（不是体内 `if false`），
并打印可读原因（"跳过：点光源的像素级验证（真的创建 wgpu 上下文） —— 未打开 QUENCH3D_GPU_TESTS=1。"）。

### 54.2 守卫：`every_real_gpu_context_creation_in_tests_goes_through_the_gate`

放在 `post_pipeline.rs` 末尾（本文件建上下文的地方最多，改它的人一定会看到这条守卫），
**不需要显卡**：它只读文件，所以 5 条 CI 腿**每条**都会跑它，包括没有 GPU 的 windows 腿。

判定规则（词法，不是语法分析）：

1. 扫描 `crates/*/tests/*.rs`（自动包含**将来新增**的文件 —— 这是关键）；
2. 先把**注释与字符串字面量**换成空格，只留代码；
3. 找出每个 `with_gpu_tests(...)` 的**配对括号区间**；
4. 对 `GPU_CREATION_CALLS` 里每个入口（`GpuContext::headless` / `from_candidates`（含 `_with`）/
   `create_instance_with` / `wgpu::Instance::new`（含 `new_without_display_handle`））的每处出现，
   断言它落在某个区间里；不在就红，并打印 `文件:行  入口`。

两个防"空转"的断言，避免守卫变成摆设：`assert!(sources.len() >= 5)`、
`assert!(gated > 0, "一个调用点都没扫到 → wgpu 入口改名了，更新清单")`。

**为什么必须剥注释（这条是载重的，不是洁癖）**：另一位写者新增的
`crates/quench3d-editor/tests/joint_frames.rs` 第 3 行的**文档注释**里就写着
"编辑器现有的往返测试都要 `GpuContext::headless`"。纯 `grep` 会把这句散文判成一处裸奔的
建上下文 —— 守卫在**真正有意义的第一次运行**里就会误报，而会乱叫的守卫最后一定被人加白名单。
§54.4 的变异 ③ 把这件事证明成了红-绿，而不是我嘴上一句"我剥了注释"。

### 54.3 按腿的证据：**真跑**（秒数）与**显式跳过**（原因行）必须能分开

上半场在这里栽过一次：当时把"日志里有 `Running tests/...` 行"当成了"断言真的在跑"，
其实 ubuntu 腿是 `0.00s` + 内层"没有可用的 GPU"分支（见 §52.6 ⑦）。所以本轮的证据按
**这一轮的判据**给：秒数（真跑）与跳过原因行（跳过）。

本机（有显卡，工具链 1.99.0 = CI 钉的版本）：

| target | 开关**关**（`env -u QUENCH3D_GPU_TESTS`） | 开关**开** |
|---|---|---|
| `post_pipeline` | 8 passed / **0.00s** / 6 条原因行 | 8 passed / **4.09s** |
| `shadow_cascades` | 1 passed / **0.00s** / 1 条 | 1 passed / **2.34s** |
| `scene_roundtrip` | 10 passed / **0.00s** / 10 条 | 10 passed / **2.29s** |
| `material_pipeline_traits` | 2 passed / **0.00s** / 2 条 | 2 passed / **1.72s** |
| `hierarchy` | 8 passed / **0.00s** / 7 条 | 8 passed / **1.42s** |
| `fog_and_shadow_quality` | 2 passed / **0.00s** / 2 条 | 2 passed / **1.12s** |
| `history` | 7 passed / **0.00s** / 7 条 | 7 passed / **1.10s** |
| `point_lights` | 2 passed / **0.00s** / 2 条 | 2 passed / **1.07s** |
| `model_instances` | 5 passed / **0.00s** / 5 条 | 5 passed / **0.98s** |
| `backend_fallback` | 7 passed / **0.00s** / 3 条 | 7 passed / **0.12s** |

开关**开**那一列：整轮 **0 条**"未打开开关"、**0 条**"没有可用的 GPU" —— 也就是说这些秒数
是**真的在真上下文上**跑出来的，不是内层提前返回。开关**关**那一列：10 个二进制全部
`0.00s` 且逐条打印原因（render 16 条 / editor 29 条）。

CI 侧新增一段 `GPU 上下文测试（显式开关；--nocapture 保留跳过原因）`：只跑这 10 个二进制并加
`-- --nocapture`。**为什么必须加**：上面那步整树 `cargo test` 会把**通过了的测试**的
stdout/stderr 吞掉，"跳过"和"没跑"在日志里长得一模一样 —— 那正是本轮要消灭的"无法诊断"。
加了这段之后，没有显卡的 windows 腿会留下一行行可读原因，有显卡的腿留下真实耗时。

### 54.4 变异红-绿（三条，全部实跑）

| # | 变异 | 期望 | 实测 |
|---|---|---|---|
| ① | 把 `point_lights.rs` 的包裹拆掉（退回裸建） | 守卫红并**点名那一行** | 退出码 101，`crates/quench3d-render/tests/point_lights.rs:28  GpuContext::headless`；改回 → 退出码 0 |
| ② | 新建一个裸奔测试文件（模拟"将来新增"） | 守卫红并点名**新文件** | 退出码 101，`crates/quench3d-render/tests/zz_mutation_probe.rs:4  GpuContext::headless`；删除 → 退出码 0 |
| ③ | 把 `code_only` 退化成"不剥注释" | 守卫红（且在**别人的文档注释**上误报） | 退出码 101，误报点：`crates/quench3d-editor/tests/joint_frames.rs:3`（还有我自己守卫的文档注释 3 处）；改回 → 退出码 0 |

变异 ② 是这一节最要紧的一条：它证明守卫**自己会发现**"未来新增的裸奔测试"，
而不是只覆盖我手动列出来的这 8 个文件。变异 ③ 证明"剥注释"这一步是载重的。

### 54.5 给下一个在这棵树上干活的人：**门禁是整树的，所以同一棵树只允许一个写者**

本轮真发生了：我的 8 个文件写完包裹、**还没跑 `rustfmt`** 的那几分钟里，另一位写者
（第 59 轮那一路）跑了完整门禁 → **第 1 段 `fmt` 全红，红点全在我未提交的文件上**。
后果不是"两个人都慢一点"，而是：**他既不能提交，也无法自证**（`cargo fmt --all --check`
是整树的），我当时还卡在一次 4 分钟的编译等待里。

结论（写在这里给下一个人）：

* **同一棵工作树、同一时刻只允许一个写者在跑门禁**；扩围阶段先把文件 `rustfmt` 干净再跑，
  否则你的未完成状态会把别人的 `GATE=0` 直接变红；
* 反过来也一样：看到"红点不是我改的文件"，先看 `git status`，很可能是另一位写者的
  未提交/未格式化文件，**不要**替他 format、更不要 `checkout --` / `stash` 去"清干净"
  （那会吞掉他正在写的代码）；
* 本轮我**没有**改动另一位写者的任何文件（`crates/quench3d-cli/**`、`crates/quench3d-physics/**`、
  `docs/gap_analysis.md`、他们那几段 `docs/*.md` / `CHANGELOG.md` / `README.md` 全部原样保留）；
* `web/pkg/*` 我**没有提交**：门禁重建出来的那份 wasm 里含的是**他们未提交**的代码
  （`11968465` 字节，比上一轮 +6657 —— 那是他们的参考姿态偏置进了 wasm），
  提交进去会让 main 的产物与源码对不上。按"原样留着别管"处理，工作树里保持脏。

### 54.6 边界（没做的、没验证的）

* **本机没有"无 GPU 的 Windows"**：原生崩溃（`0xc0000005`）**无法在本地复现**，
  windows 腿"不再崩"这件事**只能由 CI 证明**。本机能证明的是：开关关掉时**闭包不执行**
  （0.00s + 原因行）、守卫不需要显卡也能跑、以及开关打开时真跑且绿。
* **守卫是词法的，不是语法分析**：如果有人在测试里给 `GpuContext` 起别名再建上下文
  （`use ... as G; G::headless(...)`），或把建上下文藏进一个名字里没有 `GpuContext` 的
  间接包装，这条守卫会**漏**。它拦的是"写法常规的裸奔"（本次 10 个二进制全是这种写法，
  这正是当下 100% 的实际情况）。要更强得用 `syn` 解析 —— 那要加依赖，围栏不允许。
  非空转断言只能挡住"清单全失效"（0 处）这种整体失效。
* **`run_if_gpu_tests_enabled` 不算"过闸"**：只有这样命名（`with_gpu_tests`）才算。
  这是有意的 —— 那个参数化的内核允许 `true`/`false` 硬编码，硬编码 `true` 就是"绕过开关"。
* **ubuntu 腿仍然是空的**（§52.6 ⑦ / §52.7 第 4 条）：本轮**没有**在 CI 装软件光栅，
  所以这几条断言在 ubuntu 上照旧是 `0.00s` + 内层返回。CI 里的真断言覆盖来自 macOS 腿。
* **只覆盖 `crates/*/tests/*.rs`**：`crates/*/src/**` 里 `#[cfg(test)]` 的单元测试不扫。
  目前那些单元测试不建 wgpu 上下文（建上下文的都在 tests/ 下），但这不是守卫保证的。
* **本轮没动 `context.rs`**：守卫用的 `with_gpu_tests` / `run_if_gpu_tests_enabled` 是上半场
  已经提交的（`2a0f0db`），所以本轮的围栏只落在 8 个测试文件 + `ci.yml` 上。

### 54.7 收尾数字

* 门禁（**整树**，含另一位写者未提交的 19 条测试）：**553 个测试（0 失败）**，
  `wasm 字节：11968465`；`fmt` / `clippy-native` / `clippy-wasm` / `doc` / `tests` /
  `web-build` / `check-web` **七段全绿**。
* **本提交自己那棵树 = 533 + 1 = 534**（533 是上一轮的数；+1 是新增的裸奔守卫）。
  这个 534 由**本提交的 CI 日志**核实（把各 target 的 `test result:` 行加起来，与
  `tools/gate.sh` 同一口径）——不是从工作树那个 553 里减出来的推断值。
* 553 与 534 差 19 = 另一位写者的 19 条；他们的 `§53.8` 写的 552 = 533 + 19（他们跑门禁时
  我的守卫还不存在）。**合并后正确的数是 553**，README 的计数块我留给他们或下一条提交回填
  （理由见 §54.5：同一行上有他们未提交的改动，我去改会覆盖他们的工作）。
