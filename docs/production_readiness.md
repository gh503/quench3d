# quench3d 生产能力评估与补齐计划

> 这份文档回答一个问题：**quench3d 离"生产可用"还差什么，差到什么程度，先补哪一段。**
>
> 它不做宣传。每一条结论都尽量落到代码位置、可复现命令或明确的"没有"上。
> 判定标准与场景折算见 [production_readiness_criteria.md](production_readiness_criteria.md)；
> 历史缺口清单见 [gap_analysis.md](gap_analysis.md)；本文件是两者的收敛与推进计划。
>
> 评估时间：2026 年 10 月（`main` 分支持续更新）。所有"现状"都以仓库当前代码为准。

---

## 0. 一句话结论

**quench3d 是"确定性刚体仿真内核 + agent 友好的无头流水线"的垂直切片，不是通用 3D 内容创作软件的替代品。**

| 结论 | 内容 |
|---|---|
| **已经能替代的窄子集** | ① 无头批量物理仿真与出图（参数扫描、回归门禁、教学演示）；② CI 里的确定性物理回归；③ "CLI 生成场景 → 编辑器手工微调 → CLI 渲染视频/图片"这条单机流水线；④ 给 agent 用的 scene.json + MCP + CLI 自动化入口 |
| **明确不能替代** | Blender（建模/UV/雕刻/修改器）、Unity/Unreal（游戏运行时、动画/音频/UI）、three.js editor（Web 端材质/后处理生态）、Houdini（程序化） |
| **阻断点不是功能数量** | 是四类**看不见的硬指标**：色彩管理（H1）、无损往返（H2）、可复现构建（H3）、真机设备矩阵（H4）。功能少用户看得见、能绕过；这四类缺失在生产里表现为"间歇、不可复现、无法归因"。见 §4 |

**判定规则**（沿用 criteria 文档）：`T` 门槛（主流程闭环）、`Q` 质量标准、`C` 规模化与协作、`H` 隐蔽硬指标；**`H` 任一未过 → 该场景直接判"不能替代"**，无论功能多全。证据分级 E1 自报 / E2 可复现命令 / E3 对比外部参考 / E4 真机矩阵；判"能替代"的关键项必须 ≥ E3，写不出验证方法的按**未验证 = 不达标**。

---

## 1. 逐域现状矩阵

差距等级：`✅ 达标`（有实现 + 有验证）、`🟡 可用但有明显边界`、`🟠 有雏形/不完整`、`❌ 没有`。
"对标"列写的是行业里**用户会拿来比的**那个基准，不是"应该做的所有事"。
括号里的 `文件:行` 是核对过的代码位置，便于复核。

### 1.1 渲染与着色

| 能力 | 对标 | quench3d 现状 | 等级 |
|---|---|---|---|
| 前向 PBR | 全部 | 金属度/粗糙度工作流，IBL 之外的直接光照手感正确 | ✅ |
| HDR 管线 | three.js / Unity | `rgba16float` 中间目标（`renderer.rs:41`）+ ACES 色调映射（`shaders/post.wgsl:26`，Narkowicz 近似）+ 可选半分辨率 bloom（`renderer.rs:253`、`shaders/bloom.wgsl`）+ 标量曝光（`light.rs:177`） | ✅ |
| 色调映射可选性 | 全部 | 算子**不可选不可关**，只有一条 ACES 路径（→ H1） | 🟠 |
| 阴影 | 全部 | 1 张方向光阴影贴图（`renderer.rs:339`，Depth32Float），3×3 PCF（`pbr.wgsl:105`），分辨率 512–4096（缺省 2048）；**无 CSM** | 🟡 |
| 阴影可调参数 | Blender / Unity | bias 是编译期常量（`renderer.rs:795`），**没有公开字段**；只有 `DirectionalLight.cast_shadows` + 全局开关 | 🟠 |
| 后处理链 | 全部 | 固定链路（阴影 → PBR →[深度解析]→ bloom → post(SSAO) →[FXAA]），可按开关增减；**无 TAA、无 DOF、无 AO 去噪、无自定义 pass / render graph** | 🟡 |
| 抗锯齿 | 全部 | MSAA 可配 `clamp(1,8)`（`context.rs:69`），场景 `render.msaa` 缺省 4 | ✅ |
| 网格与顶点属性 | 全部 | position/normal/uv/tangent（`mesh.rs:14`）；**无顶点色、无骨骼蒙皮、无 morph** | 🟡 |
| 实例化 | 全部 | 有（每实例 model + 颜色 + 材质覆盖，`renderer.rs:1032`） | ✅ |
| LOD | 引擎级 | 无 | ❌ |
| 相机 | 全部 | 透视 + 正交（`camera.rs:7`）；**一次 render 只吃一个相机**，无多视口 | 🟡 |
| 天空 | 全部 | 程序化天空 + 常量环境项（`light.rs:107`）；**无 HDRI/IBL/cubemap 反射** | 🟠 |
| 调试叠加层 | 全部 | 网格 / 坐标轴 / 接触点 / 包围盒线框 | 🟡 |

### 1.2 光照

| 能力 | 对标 | quench3d 现状 | 等级 |
|---|---|---|---|
| 方向光 | 全部 | 有（含阴影开关，`light.rs:165`） | ✅ |
| 点光 | 全部 | **有，最多 8 个**（`light.rs:103`、`common.wgsl:30`），半径为平滑衰减（`pbr.wgsl:214`） | ✅ |
| 聚光 / 面光 | 全部 | **没有**（`spot` / `cone` / 面光 0 命中） | ❌ |
| 环境光 / 曝光 | 全部 | 常量环境项 + 标量曝光（`light.rs:167/177`） | 🟡 |
| 每灯光参数可调 | 全部 | 引擎侧有颜色/强度/半径；**编辑器还没有灯光面板**（本轮未做） | 🟠 |
| 灯光 gizmo | Unity / Blender | 无（不能画半径/锥角并拖拽） | ❌ |
| 逐物体 投/受影 | Unity | 无 | ❌ |

### 1.3 材质与贴图

| 能力 | 对标 | quench3d 现状 | 等级 |
|---|---|---|---|
| PBR 通道 | 全部 | base color / metallic / roughness / metallic-roughness 贴图 / emissive(+色+强度+贴图) / normal(+normal_scale) / alpha cutoff / alpha blend / unlit / double_sided（`material.rs:43-77`） | ✅ |
| 贴图槽 | 全部 | **4 槽**：base color / normal / MR / emissive（`material.rs:57-63`） | ✅ |
| 遮挡贴图（AO 通道） | 全部 | 无（`params2.z` 硬编码 1.0 且 shader 不读，`material.rs:229`） | ❌ |
| UV 变换 | 全部 | 只有**全局标量** `uv_scale`（`material.rs:55`）；无 offset/rotation、无逐贴图变换 | 🟠 |
| 多材质槽（一个模型多材质） | 全部 | 无（物体 = 1 材质） | ❌ |
| 材质编辑 | 全部 | 颜色/金属度/粗糙度/自发光可在属性面板即时改（≤1 帧生效）；**贴图槽没有界面** | 🟠 |
| 材质节点图 / 材质球 | Blender / Unreal | 无 | ❌ |

### 1.4 场景图、动画、音频

| 能力 | 对标 | quench3d 现状 | 等级 |
|---|---|---|---|
| 扁平对象列表 | 全部 | 有（`Scene::objects`） | ✅ |
| 父子层级 / 编组 / 空物体 | 全部 | **有**：`objects[].parent` + 局部/世界变换（见 §8.2） | ✅（第 16 轮） |
| 拖拽改父级 | 全部 | 属性面板里用"父物体"下拉改（**保世界位姿**）；场景树内拖放还没有 | 🟡 |
| 重命名 / 删除 / 复制 | 全部 | 有（重名自动加后缀） | ✅ |
| 撤销 / 重做 | 全部 | **本轮补上**（快照式 + 事务合并，见 §5.1） | ✅（本轮） |
| 关键帧动画 | 全部 | 无（`animations()` 全仓 0 命中） | ❌ |
| 骨骼 / 蒙皮 | 全部 | 无 | ❌ |
| 音频 | Unity / Godot | 无 | ❌ |
| UI / 输入映射 / 存档 | Unity / Godot | 无 | ❌ |

### 1.5 编辑器交互

| 能力 | 对标 | quench3d 现状 | 等级 |
|---|---|---|---|
| 变换 gizmo（移动/旋转/缩放） | 全部 | **本轮补上**（世界/局部、Ctrl 吸附、Shift 精细、Esc 取消、屏幕定长、10 px 抓取、命中优先于拾取） | ✅（本轮） |
| 撤销 / 重做 | 全部 | **本轮补上**（Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y，工具栏带可用态） | ✅（本轮） |
| 数值输入（拖拽改值的输入框） | 全部 | 有（egui `DragValue` + 滑杆） | ✅ |
| 多选 / 框选 | 全部 | **只有单选**（本轮未做，已排入第二晚） | ❌ |
| 复制粘贴（Ctrl+C/V） | 全部 | 只有"复制"按钮（偏移复制 + 自动重名）；无剪贴板 | 🟠 |
| 视口导航（轨道/平移/缩放 + 聚焦） | 全部 | 有（左键/中键/Alt 旋转、右键平移、滚轮缩放），**本轮补 `F` 聚焦选中** | ✅ |
| 网格与吸附 | 全部 | 1 单位网格 + gizmo 吸附；**网格不随尺度自适应** | 🟠 |
| 场景树 / 属性面板 / 状态栏 | 全部 | 有 | ✅ |
| 文件对话框 | 全部 | 无（桌面用路径文本，Web 用上传/下载） | 🟠 |
| 崩溃恢复 / 自动保存 / 布局持久化 | 全部 | 无 | ❌ |
| 历史面板 / 快捷键自定义 / 命令搜索 | Blender / Godot | 无 | ❌ |
| 编辑模式（顶点/边/面） | Blender / Unity | 无 | ❌ |

### 1.6 物理

| 能力 | 对标 | quench3d 现状 | 等级 |
|---|---|---|---|
| 刚体动力学（岛 + 并行求解） | 全部 | 有；**并行与串行逐位一致**、**线程数 2/3/4 不影响结果**都有测试钉住（`api_tests.rs:412/562`） | ✅ |
| 碰撞形状 | PhysX / Jolt | 球 / 盒 / 胶囊 / 圆柱 / 平面 / 凸包 / 三角网 共 7 种（`shape.rs:19-61`）；**无复合形状** | 🟡 |
| 关节 | PhysX / Jolt | 固定 / 球 / 铰链 / 距离 / 滑块 共 5 种（`joints.rs:34-118`）；铰链有马达与角度限位、滑块有轴向限位；**滑块无马达、无独立弹簧类型**（距离关节用 stiffness/damping 兼作） | 🟡 |
| 查询 | 全部 | 射线（`world.rs:534`）、过滤射线、形状投射（`:553`）、球体重叠（`query.rs:60`） | ✅ |
| 接触信息 | Unity / Godot | 接触对可读、带法向冲量（`contact.rs:103`、`world.rs:972`）；**无接触事件/回调/接触 ID** | 🟠 |
| 触发器（sensor） | 全部 | `objects[].sensor` + `PhysicsWorld::sensor_events()`（进入/离开，每步轮询；不唤醒睡眠体、无回调总线） | 🟡 |
| 碰撞过滤（层/掩码） | 全部 | `objects[].collision_layer` / `collides_with`（对称匹配，宽相即过滤；射线/查询不看过滤） | 🟡 |
| CCD | PhysX / Jolt | `objects[].ccd`：每子步一次中心射线 + 只截法向位移（**非**精确 TOI、只扫中心线、无旋转扫掠） | 🟡 |
| 求解器可配 | 全部 | `SolverConfig`（速度/位置迭代、松弛、warm start、Baumgarte、slop）可配（`solver.rs:24-72`） | ✅ |
| 睡眠 | 全部 | 有 | ✅ |
| 确定性 | 高要求仿真 | 同机确定、跨线程数确定（有测试）；**跨平台/跨编译器边界未声明、无测试**（→ H5） | 🟠 |
| 行业基准 V&V | 仿真行业 | 只有自洽回归与黄金摘要（`digest_tests.rs:159`）——证明"没变"，不证明"对" | ❌ |

### 1.7 资产与场景格式

| 能力 | 对标 | quench3d 现状 | 等级 |
|---|---|---|---|
| 自有场景格式 | 全部 | `quench3d.scene`（JSON，`FORMAT`/`VERSION`，15 个顶层段，CLI/编辑器/MCP 共用，`scene_doc.rs:110-168`） | ✅ |
| 格式迁移器（版本升级） | 全部 | **没有**：版本不符直接报错（`scene_doc.rs:1325`） | ❌ |
| 模型导入 | 全部 | glTF/GLB（`assets.rs:261`）与 OBJ（`assets.rs:500`，**只解析几何、不读 mtllib**）；STL/FBX/USD/DAE/PLY 全无 | 🟠 |
| 贴图随模型导入 | 全部 | glTF 贴图能进来（4 槽 + 解码 + 上传，`assets.rs:341-352/394/102`）；OBJ 不带 | 🟡 |
| 场景里的贴图引用 | 全部 | 字符串路径 + 相对场景目录解析（`scene_doc.rs:1279`）；serde 往返保留 | ✅ |
| **编辑器存盘的无损性** | 全部 | **本轮修复**：以前 `scene_to_json` 从 `SceneDoc::default()` 起头，会把 `lighting`/`simulate`/`render`/`models`/`probes` 与材质的 4 个贴图槽 + 7 个开关**静默清零**（`scene_io.rs`）；现在以载入的原文档为底稿，只覆盖编辑器管得着的字段 | ✅（本轮） |
| 无损往返测试 | 全部 | 本轮加了"存→读→再存字节相同"的固定点测试 + "不建模的段必须保留"测试（`tests/scene_roundtrip.rs`） | ✅（本轮） |

### 1.8 输出（图片 / 视频）

| 能力 | 对标 | quench3d 现状 | 等级 |
|---|---|---|---|
| 单帧 PNG | 全部 | 有（`render` 子命令、编辑器截图） | ✅ |
| 序列帧 | Blender / ffmpeg | 有：`frame_%05d.png`（`runner.rs:2017`） | ✅ |
| 视频 | 全部 | **内置纯 Rust 的 GIF / APNG 编码器**（`runner.rs:1874-1990`）；**不调用 ffmpeg**，只在报告里给建议命令（`runner.rs:2326`）→ 无 MP4/WebM | 🟡 |
| 分辨率 / 帧率 / 步长控制 | 全部 | 有（`render` 段 + `movie --width/--height/--fps/--frames/--steps-per-frame`） | ✅ |
| 无头运行 | 全部 | 有（`GpuContext::headless`，CI 里跑得通） | ✅ |
| 多相机 / EXR / 线性输出 | 离线流程 | 无 | ❌ |
| 像素级回归 | 全部 | `post_pipeline.rs` 有 4 条逐像素断言、`digest_tests.rs` 有黄金摘要；但 **`render` 子命令没有 golden 图**（CI 只验 PNG 魔数 + 体积） | 🟠 |

### 1.9 平台与分发

| 能力 | 对标 | quench3d 现状 | 等级 |
|---|---|---|---|
| 桌面（Linux/macOS/Windows） | 全部 | 三平台 CI 编译 + 测试通过 | ✅ |
| Web（wasm） | three.js editor | 有（编辑器 wasm ≈11 MB，Pages 在线可玩） | 🟡 |
| 真机矩阵（浏览器/GPU 组合） | 全部 | 只有 headless Chrome + SwiftShader 软光栅；**真机 WebGPU 未验证**（→ H4） | ❌ |
| 后端回退（WebGPU → WebGL2） | 全部 | 有（页面预探测 + 候选过滤，有测试） | ✅ |
| 移动端 | Unity / Godot | 无 | ❌ |
| 可访问性 | 生产要求 | 无 | ❌ |

### 1.10 工程卫生（"看不见但会爆炸"的一类）

| 能力 | 对标 | quench3d 现状 | 等级 |
|---|---|---|---|
| 工具链固定 | 生产流水线 | **本轮**：CI / Pages 由 `@stable` 改为**钉 `@1.99.0`**；workspace 声明 MSRV `1.90`（编辑器 `1.95`）。刻意不加 `rust-toolchain.toml`：那会逼每个本地贡献者再下一份工具链，而 CI 的钉版才是可复现性的实际保证（见 §7 第 5 条） | ✅（本轮） |
| 锁定依赖构建 | 生产流水线 | **本轮**：CI 与 `tools/build_web.sh` 全部 `--locked` | ✅（本轮） |
| 许可与依赖审计 | 企业合规 | 无 `deny.toml` / SBOM / `cargo-audit` | ❌ |
| 变更日志 | 全部 | **本轮**：`CHANGELOG.md` | ✅（本轮） |
| 贡献指南 | 开源协作 | **本轮**：`CONTRIBUTING.md`（含门禁命令与验证配方） | ✅（本轮） |
| 错误模型（可机读错误码） | 生产集成 | CLI 错误文本可读，**无稳定错误码** | 🟠 |
| 文档完整度 | 全部 | README 1300+ 行、10 个示例；缺"色彩管理 / 单位轴向 / 确定性边界"三类**声明**（→ H1/H5/H6） | 🟡 |
| 测试规模 | 全部 | 337 → **本轮 352**（+历史 6、+gizmo 单元 7、+往返 2），0 失败 | ✅（本轮） |
| CI 矩阵 | 全部 | `test`（ubuntu / macOS / Windows）+ `wasm32-unknown-unknown` 四个 job，另有 Pages 工作流 | ✅ |
| API 文档门禁 | 库类项目 | CI 无 `cargo doc` 步骤 | 🟠 |

### 1.11 建模（网格创作）

这一域是 quench3d 与"内容创作软件"之间**最硬的一道墙**：它消费别人做好的模型，自己不产出网格。

| 能力 | 行业门槛（对标） | quench3d 现状（证据） | 验收标准 | 投入量级 | 能否替代 |
|---|---|---|---|---|---|
| 图元创作 | Blender / Maya | 6 种形状（球/盒/胶囊/圆柱/平面/凸包，`scene_doc.rs` 的 `ShapeKind`）+ 4 种预设网格 | 有 | **已达标** | 部分 |
| 外部模型导入 | 全部 | glTF/GLB + OBJ（`assets.rs:261/465/500`） | 有 | **已达标** | 部分 |
| 网格编辑（顶点/边/面） | Blender | **无**（`csg/extrude/subdivid/modifier/sculpt/bevel/uv_unwrap` 全仓 0 命中） | 半边长/移动顶点/挤出后能存盘并渲染 | **12–24 人月** | ❌ |
| UV 展开与编辑 | Blender / RizomUV | 无（只有全局 `uv_scale`，`material.rs:55`） | 展开一个盒子并贴图无拉伸 | **6–12 人月** | ❌ |
| 布尔 / 阵列 / 修改器栈 | Blender | 无 | 布尔求差后仍可编辑 | **6–12 人月** | ❌ |
| 雕刻 / 重拓扑 | ZBrush / Blender | 无 | — | **24+ 人月** | ❌ |
| 自动 LOD / 减面 | 引擎侧（Unity/Unreal） | 无（`renderer.rs:2072` 只有一句注释） | 同一模型 3 级 LOD 自动生成 | **2–4 人月** | ❌ |

**结论**：quench3d **不能**替代任何建模工具。它的正确位置是"在 Blender 做完模型 → glTF 导入 → 摆场景/跑物理/出图"这条链里的**下游**。

### 1.12 脚本与扩展（自动化接口）

| 能力 | 行业门槛（对标） | quench3d 现状（证据） | 验收标准 | 投入量级 | 能否替代 |
|---|---|---|---|---|---|
| 脚本语言内嵌 | Unity C# / Godot GDScript / Blender Python | **无**（`rhai/lua/mlua/wasmtime/wasmer/plugin/scripting` 全仓 0 命中） | 能注册"每帧/每步回调"并读改场景 | **4–8 人月** | ❌ |
| 命令行自动化 | 全部 | 11 个子命令（`main.rs` 手写解析，非 clap）：`schema/doctor/info/validate/render/movie/simulate/bench/example/ask/chat/mcp` | 有 | **已达标** | ✅ |
| 能力自描述（给 agent） | MCP 生态 | `schema` 输出能力清单、`doctor` 环境自检、`mcp` stdio 服务（工具含 `dry_run`/`valid` 字段） | 有 | **已达标** | ✅ |
| 库级 SDK | Unity/Unreal C++/C# | Rust crate 门面 `quench3d`（`Engine`/`World`/`Scene`），wasm 侧 `start()`/`on_scene_file()` | 有 | **已达标** | ✅ |
| 自定义渲染 pass / 插件 | Unity SRP / Unreal RDG | **无**（`renderer.rs` 固定三步链，无 Pass trait / render graph） | 能插入一个自定义后处理并出图 | **2–4 人月** | ❌ |
| 热重载（改脚本不重启） | Unity / Godot | 无 | 改脚本 1 秒内生效 | **1–2 人月** | ❌ |
| 稳定错误码 | 生产集成 | 无（错误是中文文本，无 code） | 每个失败路径有稳定 code 且文档化 | **0.5–1 人月** | 🟠 |

**结论**：**没有脚本层**是"给 agent 用"这条定位上的真实缺口 —— 目前 agent 只能通过 CLI/MCP 的**离散命令**驱动，不能写"每步都对接触力做判断"这种逻辑。这是下一批里性价比很高的一项（不需要图形学）。

### 1.13 性能与规模化

| 能力 | 行业门槛（对标） | quench3d 现状（证据） | 验收标准 | 投入量级 | 能否替代 |
|---|---|---|---|---|---|
| 物理并行 | PhysX / Jolt | 岛并行求解；**并行 vs 串行逐位一致**、线程数 2/3/4 不影响结果（`api_tests.rs:412/562`） | 有 | **已达标** | ✅ |
| 渲染批处理 | 全部 | 按 (mesh, material) 合批 + 实例化（`renderer.rs:964-1042`） | 有 | **已达标** | ✅ |
| 视锥剔除 | 全部 | 逐实例线性剔除（`renderer.rs:1215-1232`）；`occlusion_query_set: None` 是占位 | 有 | **部分** | 🟡 |
| 空间加速 / GPU 剔除 | Unreal Nanite / Unity | 无（无 BVH/八叉树、无 GPU-driven） | 数千物体 60 fps | **6–12 人月** | ❌ |
| 性能基准 | 全部 | `bench` 子命令（`crates/quench3d-cli/src/bench.rs`）；**无 criterion / tracy / 火焰图接入** | 每次 CI 产出可比数字并设阈值 | **0.5–1 人月** | 🟠 |
| 帧预算可观测 | 全部 | 编辑器状态栏有 FPS + 绘制统计；引擎有 `RenderStats` | 有 | **已达标** | ✅ |
| 大场景内存 | 全部 | 纹理只支持 `Rgba8Unorm(Srgb)`，无压缩纹理/KTX2（`texture.rs:284`） | 200 MB 贴图包能进显存 | **2–4 人月** | ❌ |

**结论**：**"单机中小场景"的性能没问题，"生产规模"没有证据** —— 缺的是可比基准与空间加速结构，而不是"现在很慢"（没有做过对照测试，所以也不能反过来说它慢）。

### 1.14 互操作（与外部工具交换）

| 能力 | 行业门槛（对标） | quench3d 现状（证据） | 验收标准 | 投入量级 | 能否替代 |
|---|---|---|---|---|---|
| 场景格式 | USD / glTF / FBX | 自有 `quench3d.scene`（JSON，15 段，CLI/编辑器/MCP 共用） | 有 | **已达标** | ✅ |
| 模型**导入** | 全部 | glTF/GLB + OBJ（OBJ 不带材质，`assets.rs:456`） | 有 | **部分** | 🟡 |
| 模型**导出** | Blender / 引擎 | **无**（无 gltf/obj 写出代码，0 命中）→ 在编辑器里改完只能存回自有 JSON | `quench3d export model.glb` 能被 Blender 打开 | **1–2 人月** | ❌ |
| 格式覆盖 | 全部 | 无 STL / FBX / USD / USDZ / DAE / PLY | 至少 STL（3D 打印）与 FBX（美术主流） | **2–3 人月/格式** | ❌ |
| 图像/视频交换 | 全部 | PNG 序列 + 内置 GIF/APNG；**不调用 ffmpeg**，只给建议命令（`runner.rs:2326`） | 直接产出 MP4/H.265 | **1 人月**（纯 Rust 编码器或 ffmpeg 调用） | 🟠 |
| 图像 I/O 色彩空间 | 全部 | 只按 sRGB 标志区分；无 EXR/HDR 读写 | 读写 32 位 EXR 不丢线性 | **1–2 人月** | ❌ |
| 版本升级路径 | 全部 | **无迁移器**（版本不符直接报错，`scene_doc.rs:1325`） | 旧文件能一键升到新版本并留备份 | **0.5–1 人月** | ❌ |

**结论**：互操作是**单向**的 —— 能把外面的模型拿进来，拿不出去（除 PNG）。要在流水线里当"下游"是够的；要当"中间站"不够。

## 2. 六个使用场景的判定

判定口径见 [production_readiness_criteria.md](production_readiness_criteria.md) §2–§6。

| 场景 | 结论 | 决定性阻断项 | 可以替换的子集 |
|---|---|---|---|
| S1 查看/检查模型 | **不能替代** | H1 色彩管理未声明、H2 导入单位/up-axis 未处理（贴图导出静默丢失**本轮已修**，见 §5.3） | 无头批量体检 + 缩略图 |
| S2 场景可视化 | **不能替代** | 无 IBL / CSM / SSAO / LOD；输出色彩管线单一不可选、无 EXR | three.js 应用里的白模/静态演示 + 批量出图 + 回归门禁 |
| S3 物理仿真 | **有条件替代（窄）** | 无行业基准 V&V；确定性未划跨平台边界；无接触力导出/接触 ID | 确定性刚体参数扫描 / CI 回归 / 教学 |
| S4 游戏 | **不能替代** | 动画、音频、UI、输入映射、存档四件全无；无存档迁移 | 刚体玩法原型的物理核心 |
| S5 Web 分发 | **不能替代** | wasm 11 MB；真机 WebGPU 未验证（只有 SwiftShader）；H1 色彩；无无障碍 | 内部演示嵌入 |
| S6 自动化流水线 | **有条件替代（最接近）** | 可复现构建缺口（本轮已补 3 项，仍缺 SBOM/许可审计）；无迁移器；错误码只有粗粒度 | Blender 无头脚本中"纯仿真/出图/回归"那部分 |

**一句话**：S6/S3 的窄子集已经可以进生产；S1/S2/S4/S5 在**任何**标的上都不构成替代，而这与"功能多不多"关系不大——是 §4 那四类硬指标在挡路。

### 本轮之后，判定变了多少

编辑器的**操作面**从"填数值"补到了"能直接拖"（变换手柄 + 撤销/重做，§5.1–5.2），
场景往返的**静默数据损坏**被修掉（§5.3），工程卫生补了 3 项（§5.4）。但六个场景的结论**没有一条因此翻盘**，原因很直白：

* S1/S2 卡在色彩管理与光照/后处理完整度 —— 手柄再好用，也解决不了"渲出来的颜色无法与外部工具对齐"；
* S4 卡在动画/音频/UI/存档 —— 手柄不是游戏运行时；
* S5 卡在包体与真机矩阵 —— 与编辑器功能无关；
* S3/S6 本来就可以用，本轮把它们的**编辑与往返**体验补齐，属于"让能用的那部分更好用"。

换句话：本轮补的是**"手工操作这一层缺得最明显的两块"**，而不是翻盘项。翻盘项在 §4 的 H1/H4/H5。

---

## 3. 什么程度算"能替代 X"（折算示例）

判定要落到可执行的必过项，不然"对标 Blender"毫无意义。

| 目标 | 必过项（摘） | quench3d 距离 |
|---|---|---|
| 替代 Blender 做**建模** | 编辑模式（点/边/面选择与拓扑编辑）、UV 展开（LSCM/ABF 级 + UDIM）、布尔（1000 病态用例 0 崩溃、结果水密）、修改器栈（≥20 个、非破坏、可确定性重算）、雕刻（1000 万面 ≥15 fps） | **差整整一个产品**（全无）。不做这个承诺 |
| 替代 Unity/Unreal 做**游戏** | 动画系统、音频、UI、输入映射、存档与迁移、脚本热重载、构建导出 | 四件核心全无。**不构成替代** |
| 替代 three.js editor 做**Web 演示** | Web 端材质/后处理/资源管线生态、真机矩阵、包体 ≤ 几 MB | 生态与体积都不在一个量级。只适合内部演示嵌入 |
| 替代 Blender 无头**批量出图/仿真** | CLI 稳定、可复现构建、像素 golden、分辨率/编码可控、错误可机读 | **最接近**：CLI 与确定性强，缺像素 golden 与错误码（本轮补了构建可复现） |
| 替代**物理仿真工具** | 行业基准 V&V（如标准落球/堆叠/摩擦锥用例比对）、接触力/接触 ID 导出、确定性边界声明、单位约定 | 只有自洽回归。**不能作为验收依据**，只能作为"没退化"的门禁 |

---

## 4. 隐蔽硬指标（H）逐条与关闭成本

这一节是整份文档最重要的部分：它决定"能不能进生产"，而不只是"好不好用"。

| ID | 硬指标 | 现状 | 关闭成本 | 具体动作 |
|---|---|---|---|---|
| **H1** | **色彩管理**：输入/输出色彩空间、色调映射算子、曝光是否可声明与可选 | 只有一条固定 ACES 路径，README 未声明色彩约定 | **低（文档）+ 中（可选算子）** | ① 在文档里写清：工作空间线性、base color sRGB、输出 ACES；② 增加"tonemap 算子可选 + 曝光"两个开关即可进入 H1 门槛 |
| **H2** | **无损往返**：存一次读一次之后场景不变 | **本轮已关闭编辑器侧的主要漏洞**：以前导出会把 `lighting`/`simulate`/`render`/`models`/`probes` 与材质 4 个贴图槽 + 7 个开关静默清零；现在以原文档为底稿，只覆盖编辑器管得着的字段，并有"存→读→再存字节相同"的固定点测试。**仍缺**：导入的 glTF 模型在编辑器里被移动后，位置改动写不回 `models` 引用 | **中** | 已做：固定点测试 + 底稿合并 + 悬空关节/动作清理。待做：模型实例的"位置回写引用"或"展开为 objects"二选一并测试 |
| **H3** | **可复现构建**：别人拿到 commit 能构建出同样东西 | 无 `rust-toolchain.toml`、CI 未 `--locked`、无 `deny.toml`/SBOM | **低**（本轮做掉大半） | `rust-toolchain.toml` + CI `--locked`（本轮）→ 许可审计与 SBOM 留待下一轮 |
| **H4** | **真机设备矩阵**：真浏览器/真 GPU 上跑过 | 只有 headless Chrome + SwiftShader（软件光栅） | **中** | 至少一条"真机 WebGPU"人工验证记录 + 一张矩阵表（Chrome/Firefox/Safari × 独显/核显） |
| **H5** | **确定性边界**：哪些维度上确定、哪些不确定 | 同机同线程数有测试；跨平台/跨线程数未声明 | **低** | 文档写清边界 + 一条"跨线程数一致"的用例（已有）作为对照 |
| **H6** | **单位与轴向约定** | 引擎内部 Y-up / 右手 / 1 单位 = 1 米（代码如此），文档未声明；导入不做轴向转换 | **低（声明）+ 中（导入选项）** | 文档声明 + 导入时的缩放/轴向选项 |
| **H7** | **存档/格式迁移** | 有 `VERSION` 字段，无迁移代码 | **中** | 加迁移函数表 + 一条"v1 文件能读进 v2"的测试 |

**结论**：H1/H3/H5/H6 里有一半是**纯文档缺口**（写清约定即可关闭），另一半是"两个开关 + 一个字段"级别的工程量。**这就是为什么说"离生产不远，但不是靠加功能解决的"。**

---

## 5. 本轮推进（夜间连续作业）

> 每条都给出"实现位置 + 验证方式 + 复核命令"。

### 5.1 编辑器：撤销 / 重做

- 实现：`crates/quench3d-editor/src/history.rs`（整场景快照 + 事务合并）；
  文档层接口 `Doc::record_edit` / `begin_history_transaction` / `end_history_transaction` /
  `undo` / `redo` / `reset_history`（`crates/quench3d-editor/src/doc.rs`）。
- 语义（逐条有测试）：一次拖拽/滑杆 = **一步**；撤销**不动视角**；撤销后的场景与快照**逐字节相同**；
  上限 128 步（可配）丢最旧；新编辑清空重做链；选区只在越界时收敛；
  撤销/重做同步"底稿文档"，所以撤销回到带光照的场景后再存盘不会丢光照。
- 快捷键：`Ctrl+Z` / `Ctrl+Shift+Z` / `Ctrl+Y`；工具栏"↶ 撤销 / ↷ 重做"带可用态。
- 边界（诚实）：恢复的是**场景内容**（物体、变换、材质、刚体参数、灯光与相机文档字段），
  **不**恢复物理步进状态（速度、睡眠、接触缓存）。运行中撤销 = "把场景重置成当时的样子，物理从这里继续"。
- 验证：`cargo test -p quench3d-editor --test history`（6 个用例：增删、变换、拖拽合并、视角保持、逐字节一致、上限与重做链）。

### 5.2 编辑器：变换 gizmo

- 实现：`crates/quench3d-editor/src/gizmo.rs`。
- 三模式（`W`/`E`/`R` + 工具栏）：移动 / 旋转 / 缩放；坐标系世界 / 局部（`L`）。
- **屏幕定长**：手柄 88 px，世界长度由"投影一个单位轴"反推 —— 贴脸与几公里外同一个大小。
- **抓取**：屏幕空间 10 px 容差，**gizmo 先于物体拾取**；被几何挡住的手柄照样能抓（覆盖层不参与深度）。
- **吸附与精细**：`Ctrl` 吸附（平移 0.25 / 旋转 15° / 缩放 0.1，作用在**本次拖拽累计增量**上），`Shift` 精细 ×0.1，可叠加。
- **拖拽语义**：按下开事务、抬起提交 → 一次拖拽 = 一步历史；`Esc` 中途取消会还原且不留历史；
  拖拽中把刚体速度清零，避免"手里还在被物理带走"。
- 边界（诚实）：不支持拖拽中键入数值；无轴心切换（只有物体原点）；无多选成组变换；
  旋转手柄是真实投影椭圆（画与抓同一条折线），不是轨迹球。
- 验证（两条独立证据）：
  1. 纯几何单元测试（不需要 GPU，任何 CI 机器都会跑）：`cargo test -p quench3d-editor --lib`
     —— 7 个：命中优先级、相机后方不可抓、旋转按折线命中、吸附作用于累计量、平面 2×2 解算、正交基、点到线段。
  2. 真机渲染证据（llvmpipe/Vulkan，1280×800 截图逐像素统计）：三根轴分别是
     X 红 `x[615,688] × y[534,560]`（163 px，近似水平）、Y 绿 `x[599,610] × y[443,520]`（196 px，竖直）、
     Z 蓝 `x[525,637] × y[501,570]`（194 px，斜向）；三者包围盒在轴心汇聚，长度 ≈77–88 px（与 `HANDLE_LEN_PX = 88` 一致）。

### 5.3 编辑器存盘：不再静默丢段（H2）

- 问题（本轮之前真实存在）：`scene_to_json` 从 `SceneDoc::default()` 起头，于是
  「CLI 生成带光照/渲染设置/模型的场景 → 编辑器改一改 → 存盘」之后，`lighting`、`simulate`、
  `render`、`models`、`probes` 以及材质的 4 个贴图槽与 7 个开关会被**清零**，而文件看上去完全正常。
  这条链正是本项目的核心卖点，属于"静默数据损坏"。
- 修复：导出以**最近载入的原文档**为底稿（`Doc::base_doc`），只覆盖编辑器真正管得着的字段
  （物体、地面、材质标量、相机、重力、场景名）；材质按**同名物体**继承贴图与开关；
  引用了已删除物体的关节/动作会被清掉并上报（否则文件自己读不回来）；撤销/重做同步底稿。
- 验证：`cargo test -p quench3d-editor --test scene_roundtrip`（4 个用例）。新增两个：
  ① 不建模的段必须保留 + 贴图引用/UV/法线强度不能丢 + **存→读→再存字节相同**；
  ② 删掉物体后引用它的关节必须消失、且导出结果仍能读回来。

### 5.4 工程卫生（H3）

- **CI / Pages 钉工具链**：`dtolnay/rust-toolchain@stable` → `@1.99.0`。
- **`--locked`**：CI 的全部 cargo 命令与 `tools/build_web.sh` 都加上。
- **`CHANGELOG.md`**（含"未做"一节）与 **`CONTRIBUTING.md`**（门禁命令、三条红线、验证配方、平台事实）。
- 刻意**没有**加 `rust-toolchain.toml`：那会逼每个本地贡献者再下载一份工具链（本机 stable 就是 1.99.0），
  而 CI 的钉版已覆盖"别人拿到 commit 能构建出同样东西"这个实际需求。这是**明确的取舍**，不是遗漏。

### 5.5 本轮**没有**做的（明确记录）

- 多选/框选、灯光编辑面板、材质贴图槽界面、glTF 导入进文档层；
  （场景层级与父子变换已在第 16 轮补上，见 §8.2）；
- 像素 golden 进 CI、场景格式迁移器、错误码、许可审计与 SBOM；
- 色彩管理（H1）、单位/轴向（H6）、确定性边界（H5）的正式声明 —— 三者是**纯文档缺口**，列入第二晚。

## 6. 三晚路线（按 ROI 排序）

**第一晚（本轮）**：撤销/重做、变换 gizmo、工程卫生三项 + 本文档。

**第二晚（P1，把"S6 能替代"钉死）**：
1. 像素 golden 进 CI（单帧固定场景 → PNG → 与基线比对，容差上限给定）；
2. 往返测试断言"贴图引用不丢" + 场景格式补贴图字段 + 导不出时报错；
3. 场景格式迁移器 + "v1 文件读进 v2"测试；
4. 色彩管理（H1）与确定性边界（H5）、单位约定（H6）写成正式声明文档；
5. 错误码：CLI 错误加稳定 `code` 字段。

**第三晚（P2，编辑器体验闭环）**：
1. 多选 + 框选 + Shift 加选，gizmo 支持成组变换（以选中中心为轴心）；
2. ~~场景层级：`parent` 字段 + 局部/世界变换 + 拖拽改父（默认保世界位置）+ 格式 v2~~
   —— **已实施（第 16 轮）**，但**不做** v1 迁移器（没有外部使用者，格式一次做到位），见 §8.2；
3. 灯光编辑（类型/颜色/强度/范围）+ 材质贴图槽（导入/清除/缩略图）；
4. glTF/OBJ 导入进文档层（导入为对象，带缩放/轴向选项）；
5. 网格随尺度自适应 + 视口导航轴 + 面板统计。

**明确不做（避免翻车）**：历史面板跳转、Prefab 覆盖与嵌套、FBX、材质节点图、雕刻模式、UV 展开、重复导入保实例、移动端、动画/音频/UI 运行时。每一项都是"周"级且会把上面三项的正确性拖下水。

---

## 7. 诚实声明

1. 本文档的"现状"以仓库当前代码为准；凡是**没有写验证方法**的条目，一律按"未验证 = 不达标"处理。
2. quench3d **不做**以下承诺：替代 Blender/Unity/Unreal/Houdini 的通用内容创作；提供行业级物理 V&V 结论；提供真机 WebGPU 认证；提供企业合规所需的许可审计报告（尚未有 SBOM）。
3. 物理结果**不能作为验收依据**，只能作为"没有退化"的回归门禁——除非按 §3 的必过项补齐基准用例。
4. 本文档由 agent 在连续作业中编写并自检；凡数字均给出来源（测试数、字节数、commit、CI run id），可复核。
5. **工具链取舍要说明**：CI/Pages 钉 `1.99.0`，但仓库**没有** `rust-toolchain.toml`。
   理由见 §5.4（避免逼本地贡献者再下一份工具链），代价是"本地 stable 与 CI 不完全等价"。
   这是一个**已知的、刻意的**取舍，不是遗漏。
6. **浏览器验证的边界**：Web 端"编辑器能起、能存出真实场景 JSON、变换手柄按设计渲染"都有像素/产物证据（§5.2 与 §5.3 的复核命令）；
   但**"在浏览器里用鼠标把某个轴拖走并断言物体坐标变化"这条链路没有被机器人跑通**——
   本轮用的无头 Chrome 实例在重新加载后不再把 CDP 鼠标事件送进 winit（同一脚本在更早的实例上可以点到按钮并触发下载），
   属于验证环境问题而非已定位的应用缺陷，但**没有排除应用侧原因**，因此按"未验证"记录。
   手柄的命中与吸附逻辑本身由 7 个不需要 GPU 的单元测试覆盖，原生（llvmpipe/Vulkan）下的渲染有逐像素证据。
7. 本轮的取舍清单（**没做**的）逐条写在 §5.5；如果某条正好是你最关心的，那它现在**还没有**。

---

## 8. 下一批的实施笔记（已核对的 API 落点，省得下次再翻）

**材质贴图界面**（数据层已在 `crates/quench3d-editor/src/doc.rs` 的 `TexturePaths` + `scene_io::apply_texture_overrides` 里就位，
只差控件与"应用"动作）：

| 需要什么 | 落点（本轮核过） |
|---|---|
| 读材质 | `Renderer::material(&self, MaterialId) -> Option<&MaterialDesc>`（`renderer.rs:1191`） |
| 改材质 | **没有 `material_mut`** —— 实施时要么加一个 `materials_mut()` / `material_mut()`，要么走"重建材质"的路径（先确认 `materials` 的可见性） |
| 贴图槽字段 | `MaterialDesc::{base_color_texture, normal_texture, metallic_roughness_texture, emissive_texture}: Option<TextureId>`（`material.rs:57-63`） |
| 上传贴图 | `Renderer::add_texture(&mut self, ctx: &GpuContext, image: &ImageData, srgb: bool) -> TextureId`（`renderer.rs:1081`）—— 需要拿到引擎里的 `GpuContext`（实施时先确认 `Engine` 暴露的字段名） |
| 从磁盘读图 | `ImageData::from_file(path)`（`texture.rs:103`）—— **原生专属**，wasm 侧要么禁用按钮、要么改成"上传字节"的入口 |
| 校验口径 | 场景校验器要求被引用的贴图**文件存在**（`texture.rs` 校验路径），所以"应用"失败要给出可读原因，而不是静默保留旧贴图 |

**其它两条已核过的落点**：glTF/OBJ 导入在 `assets.rs:261/465/500`（OBJ 不带材质）；
模型**导出**目前 0 命中，是互操作里唯一的"只进不出"。
版本检查（`scene_doc.rs` 的 `validate`）现在是**版本不符直接报错**，并且明确**不做**迁移器：
这个格式还没有外部使用者，与其维护一条"读旧版"的路径，不如让旧文件拿到一条清楚的诊断。
再加字段时请同步改 `VERSION`（当前 2）。

### 8.1 「模型引用接进场景树 + 视口实时拖动」要动哪里（本轮核过）

> **状态：已实施（第 15 轮）**。下面保留当时的分析（为什么必须动这两条路径），
> 实施结果写在最后一小节"实际改法"里；验证命令见
> `cargo test -p quench3d-editor --test model_instances`。

现状：模型的展开有**两条**路径，两边都只返回"成功/失败"，**不告诉调用方它加了哪些物体**：

| 路径 | 位置 | 行为 |
|---|---|---|
| 无头/CLI | `crates/quench3d-cli/src/runner.rs:977-988` | 遍历 `doc.models`，`load_model(&m.path)`，**每个 mesh 推一个物体** |
| 引擎 | `crates/quench3d-cli/src/runner.rs:1477-1483` | 每个模型一次 `engine.load_model(&m.path, transform)` |

因此编辑器拿不到"哪个物体属于哪个模型"的映射 —— 这是"视口里实时拖动模型"卡住的**唯一**原因
（写回场景文件的部分已经在第 14 轮做完：`models[i]` 的 position/rotation/scale 可改、可存、可撤销）。

最小改法（建议顺序）：

1. 在 `runner.rs` 的引擎路径上记录**物体索引区间**：调用 `load_model` 前取 `engine.scene.objects.len()`，
   调用后取一次，把 `(模型序号, start, end)` 存进 `SceneDoc` 的同级结构（或返回给调用方）；
2. 编辑器把这份映射存进 `Doc`，`models_section` 里改变换时**同时**把该区间内物体的
   `position/rotation/scale` 按模型变换的差分更新（引擎侧实时生效）；
3. 场景树里给这些物体显示分组标题（`模型 #i path`），使它们可选中、可高亮；
4. 测试：加载一个两 mesh 的 OBJ → 断言映射区间长度 = mesh 数；改模型变换 → 断言区间内物体真的动了；
   存盘 → 重开 → 位置仍然一致（与第 14 轮的导出测试互补）。

#### 8.1.1 实际改法（第 15 轮，已跑门禁）

比上面那个"最小改法"多做了一步：**反向映射也接上了**，否则"视口里拖模型"仍然会
把子网格拖散（拖的是单个物体，存盘时它又会被跳过）。落地如下：

| 改动 | 位置 | 说明 |
|---|---|---|
| 新增 `ModelInstance { model, path, objects }` | `crates/quench3d-cli/src/runner.rs` | `setup_visuals` 的返回值，记录 `models[i]` → 物体下标（直接用 `engine.load_model` 的返回值，不靠"前后各取一次长度"猜） |
| 装载时把映射带进编辑器 | `crates/quench3d-editor/src/scene_io.rs` | `install_doc` 返回 `Installed { objects, name, instances }`；`load_*_into_doc` 顺手 `doc.set_model_instances` |
| 撤销/重做后刷新（关键） | `crates/quench3d-editor/src/history.rs` | 快照是整场景重建，物体增删会让下标整体挪动；`Restored` 把新映射一起带出来，`Doc::undo/redo` 覆盖旧值。**不刷新就会拖到别的物体身上** |
| 双向变换 | `crates/quench3d-editor/src/doc.rs` | `apply_model_transform`（模型→物体，面板用）与 `apply_transform_to_model_group`（物体→模型，拖动用）；`forget_object` 维护删除后的下标 |
| 视口拖动 | `crates/quench3d-editor/src/gizmo.rs` | 每帧写完物体变换后调一次整组联动；`Esc` 还原整组；模型成员的缩放强制走等比手柄（格式里只有均匀 `scale`） |
| 面板 + 属性面板 | `crates/quench3d-editor/src/ui.rs` | 模型面板改完**当场应用**；选中模型的子网格时，属性面板的数值改动也走同一条整组路径 |
| 场景树分组 | `crates/quench3d-editor/src/ui.rs` | `模型 #i · 路径` 折叠组 + "整组聚焦"，成员照旧可点选 |

顺带改掉的两处**已经说不通的旧话**：`scene_to_json` 的跳过提示不再说"位置改动不会写回模型引用"；
README 的 §1.1 与 §9 里"编辑器没有贴图/灯光编辑界面""导入模型只能作为静态物体存在"两句也已按现状改写。

### 8.14 「接触特征 ID + 持久热启动」怎么落的（第 28 轮，🟡 部分）

> **状态：第一条腿已实施**。验证：`cargo test -p quench3d-physics -- contact_feature_ids`。

| 层 | 改动 | 关键点 |
|---|---|---|
| 数据 | `ContactPoint.feature_id: u32` | 由**两端局部坐标**按 0.5 mm 量化后哈希得到（`contact::feature_id_from_local`，0 = 未知）。局部坐标是关键：它随刚体一起运动，所以同一个几何特征在连续帧里得到同一个 ID |
| 生成 | `world::collide_bodies` 统一打标 | 包一层 `collide_bodies_raw`，在**唯一**的出口处给所有形状对的接触点打 ID——不散落在 20 多个窄相分支里（散开就一定会漏） |
| 使用 | `ContactCache::lookup` | **优先按 ID 精确匹配**；匹配不上才退回"两端局部坐标距离 < 2 cm"的历史行为。这一条解决了真问题：滑动/旋转时容差匹配会把冲量沿用到相邻特征点上 |
| 不变式 | 共用同一个哈希函数 | 宽相/窄相打的 ID 与求解器算的键必须逐位相同，所以两边调**同一个** `pub(crate)` 函数（各写一份迟早会漂） |

**没做（诚实边界，下一轮）**：
- **接触余量（speculative margin）**：分离的物体不会提前生成接触点。真做它需要窄相给出"最近特征距离"（不止是重叠时的穿透），并按 `separation/dt` 给法向速度目标——那是窄相 + 求解器偏置两处改动，不是加个参数。
- **流形本身跨帧复用**：现在流形每步重算，跨帧活下来的只有**身份（feature_id）与累积冲量**。这就是主流的"接触缓存"路线；更激进的"流形持久化 + 增量更新"收益主要在点数很多时，暂不做。

### 8.13 「机器可读 JSON Schema + 版本策略」怎么落的（第 27 轮，已跑门禁）

> **状态：已实施**。验证：`cargo test -p quench3d-cli --lib -- json_schema`、
> `cargo test -p quench3d-cli --test cli_e2e -- json_schema`。

| 层 | 改动 | 关键点 |
|---|---|---|
| CLI | `quench3d schema --json-schema` | draft 2020-12；`$defs` 覆盖 24 个类型 + 6 个枚举，全部 `additionalProperties: false`（与 `validate` 的"未知字段 = 错误"一致）。不带 `--json` 时直接打印 schema（好 pipe 给编辑器/CI） |
| 一致性 | `json_schema_matches_the_source_structs` | `include_str!("scene_doc.rs")` 读源码，抽 `pub` 字段（含 `#[serde(rename)]`、识别 `#[serde(skip)]`）与枚举变体（按 `rename_all` 折算），与 schema 的属性名/枚举值**双向**对账。加字段忘了改 schema、或 schema 里留了删掉的字段，都会红 |
| 自洽 | `json_schema_is_self_consistent` | 每个 `$ref` 必须指向存在的 def（写错名字 = 编辑器静默失去补全）；每个 object def 必须 `additionalProperties: false`（否则 schema 说合法、`validate` 说非法） |
| 版本 | `version` 诊断 + 文档 | 1 → 2 的唯一差别是 `objects[].parent` 生效，**没有 parent 的文件语义完全一样**，所以迁移 = 改版本号；未来版本给"请升级 quench3d"。**不做迁移器**：维护一份没人需要的重写逻辑比一条说清差别的诊断更贵 |

**没做（诚实边界）**：schema 里的值大多是"任意"（`true`）——**范围/互斥/交叉引用仍以
`quench3d validate` 为准**，别把"schema 校验通过"当成"场景能跑"；不生成 draft-07；
不支持外部 `$ref`（单文件自包含）；`format`/`minimum` 这类断言只有少数几处（`vec3` 的三元组、
枚举、`version` 的 `minimum`）；没有 `--json-schema --defs object` 之类的"只取一节"开关。

### 8.12 「模板 + 实例」怎么落的（第 25 轮，已跑门禁）

> **状态：已实施**。验证：`cargo test -p quench3d-cli --lib -- templates_and_instances`、
> `cargo test -p quench3d-cli --test cli_e2e -- templates_and_instances`、
> `cargo test -p quench3d-editor --test scene_roundtrip -- templates_and_instances`。

| 层 | 改动 | 关键点 |
|---|---|---|
| 格式 | `templates: { 名字: 物体字段 }` | 模板**复用 `ObjectDoc` 本身**承载：字段集自动一致（多一个字段就多一个拼写隐患）。代价是模板里能写 `name`/`parent`/`position`——那几项没有意义，所以给一条警告并忽略，而不是静默生效 |
| 格式 | `objects[].template` + `objects[].instances[]` | 实例只描述"每个副本差在哪"（位置/朝向/缩放/名字）。`instances` 的字段**覆盖**物体，物体**覆盖**模板 —— 一条规则贯穿到底 |
| 语义 | 每个副本 = 一个独立刚体 | 说得直白：这不是 GPU 实例化（渲染器那层本来就会按 (mesh, material) 合批），而是"把一段描述展开成 N 个物体"。所以物理、报告、关节引用都按独立物体算，`instances` 多了会有真实物理开销（>64 副本给警告） |
| 命名 | `本名#1`、`本名#2`…；只有一个副本且没写 name → 本名 | 后者保住老场景：给已有物体加一个"只放一个副本"的 `instances` 不会打断引用它的关节/动作 |
| 校验 | 结构层 + 展开后再验一遍 | 引用不存在的模板 / 空数组 / 展开后名字撞车 / 实例未知字段 = error；未用模板、模板带位置、副本数 > 64 = warning。**物体字段的检查一个字没改**：先把模板/实例展开，再把展开后的文档交给原来的 `validate()`，这样每条问题只报一次 |
| 展开点 | `SceneDoc::expanded()`，在 `build_world` / `setup_visuals` / 轨迹 / bench 各调一次 | 只有一处展开逻辑，CLI 与报告不可能出现"装载看到 3 个、报告说 1 个"的偏差 |

**没做（诚实边界）**：**外部场景引用**（跨文件 `include` / 组装多个场景文件）没做——它在
gap 表里与资产搜索路径（#11）同源，等那一项一起做；**编辑器存回时把 `instances` 摊平**成具体物体
（行为、数量、位置都不变，只是文件变长）——因为编辑器没有"模板面板"这个概念，宁可显式摊平
也不要假装保留了模板；模板不支持嵌套（`template` 里再写 `template`/`instances` 会被忽略并警告）；
`instances` 只覆盖位置/朝向/缩放/名字，别的字段要在模板或物体上写。

### 8.11 「复合形状：一个刚体多个 Shape」怎么落的（第 25 轮，已跑门禁）

> **状态：已实施**。验证：`cargo test -p quench3d-physics --lib -- compound`、
> `cargo test -p quench3d-cli --lib -- compound_shapes`、
> `cargo test -p quench3d-cli --test cli_e2e -- compound_shapes`、
> `cargo test -p quench3d-editor --test scene_roundtrip -- compound`。

| 层 | 改动 | 关键点 |
|---|---|---|
| 引擎 | `ShapePart { shape, offset, rotation }` + `BodyBuilder::part/part_at` | 部件**在建体那一刻**配置完；不提供 `build()` 之后加部件——"改了形状但要记得手动重算质量属性"是这类引擎最经典的静默 bug |
| 引擎 | 主形状仍单独存在，附加部件放 `extra_parts` | 单形状刚体（绝大多数）继续走**同一条老路径**：质量属性、AABB、窄相、查询都先看 `extra_parts.is_empty()`。这不是"应该一样"，是分支都没进 |
| 引擎 | 质量合成 `combine_mass_properties` | 质量相加、质心取一阶矩、惯量 = 零件惯量转轴 + 平行轴 `m·r²` 再挪回合成质心。**对角 + 无惯性积**，与引擎原本的惯量表示保持一致 |
| 引擎 | 窄相 `collide_bodies` 逐部件求交后合成一个流形 | 每对刚体最多 8 个点，超限按穿透深度取最深（稳定排序 → 确定性）。点多了对求解器没好处，还会让热启动缓存抖动 |
| 引擎 | AABB / 射线 / 距离 / 重叠 / 角色控制器逐部件 | 半空间的 AABB 是"无效包围盒"，语义是"不能粗筛掉，直接精确求交"——这条在复合形状里同样成立 |
| 引擎 | 任一部件是半空间/三角网 → 整体静态 | 一个可以推着走的"无限半空间"没有意义 |
| 格式 | `shapes` 与 `shape` 二选一 | 两个都写 = error（不猜"以谁为准"）；空数组 = error；部件未知字段 = error；半空间部件、只写一个部件 = warning |
| CLI | 刚体按所有部件装配；渲染每部件一个绘制物体 | 主形状进 builder 第一个参数、**其余**才 `part()`。第一版把主形状也当附加部件加了一遍 → 质量/惯量翻倍、接触点重复，被既存的静摩擦端到端测试当场抓住（"该滑走的箱子只走了 0.0336 m"） |
| 编辑器 | `SceneObject::owner_part` + 导出写 `shapes`、跳过部件伪物体 | 部件伪物体不是场景物体：不加这条归属线，**存一次场景就多出几个物体**（静默数据繁殖，比崩溃更难查） |

**没做（诚实边界）**：没有**部件级材质**（摩擦/恢复/传感器/CCD 仍是刚体级）；
部件形状不能是三角网（凸包也只能当整个刚体，不做"每个部件一个网格"）；
没有部件编辑面板（编辑器能原样往返，但不能在界面上增删部件、也看不到部件的选择框）；
每对刚体最多 8 个接触点（超出按深度截断）；惯量是对角近似（无惯性积）；
部件之间的自碰撞不单独过滤（同一刚体的部件本来就不互相碰撞）；接触的 `feature_id`
仍不存在（复合形状只影响"哪些部件参与"）。

### 8.10 「事件总线」怎么落的（第 24 轮，已跑门禁）

> **状态：已实施（生命周期钩子部分没做，见边界）**。验证：
> `cargo test -p quench3d-physics --lib -- event_bus`、
> `cargo test -p quench3d-cli --test cli_e2e -- simulate_report_includes_an_event_stream`。

| 层 | 改动 | 关键点 |
|---|---|---|
| 引擎 | `PhysicsEvent` + `PhysicsEventKind`（7 种） | 扁平 `Copy` 结构而不是带负载的枚举：队列能复用缓冲、报告能直接序列化、宿主按 `kind` 分支时字段含义固定 |
| 引擎 | 接触开始/结束 = 本子步与上一子步的**真实接触对**做集合差分 | 复用传感器那套已经测过的差分写法；`(a, b)` 先排序去重，所以事件顺序**与宽相顺序无关**（确定性） |
| 引擎 | 睡眠/唤醒 = 每步比对 `sleeping_prev` | 唤醒入口有五个（手动 `wake_up`、`wake_up_all`、接触唤醒、关节唤醒、手动 `sleep`）——逐个埋点迟早漏一个；状态差分是单一事实来源 |
| 引擎 | 传感器进入/离开**镜像**进总线 | 老的 `sensor_events()` 保留（有 `steps_inside`），新总线是统一入口，不逼调用方记两套 API |
| 引擎 | 队列上限 + `events_dropped()` | 事件是给宿主看的；宿主忘取时无上限队列就是内存泄漏。"丢事件"必须有痕迹，否则最难查 |
| 引擎 | 断裂事件带 `force`/`torque` 量级 | 断的那一刻的数值只在求解后存在一次，事后无法重算（关节已经被移除了） |
| 报告 | `events[]`（物体名 + 步号）+ `events_dropped`（永远输出） | 句柄对脚本没用；`events_dropped` 恒输出，脚本才能区分"没丢"与"字段不存在" |
| 编辑器 | 状态栏只读显示队列深度/丢弃数/最近一条 | 界面里看不见的东西等于不存在；但**不做**事件面板与"清空"（避免为了好看动 `&mut Engine` 的借用链） |

**没做（诚实边界）**：没有**生命周期钩子**（`on_scene_loaded` / `on_destroy`）与用户回调——
本引擎没有脚本层，宿主只能轮询事件队列，`Scene` 也没有自己的生命周期事件流；
没有事件过滤/订阅（要挑就自己遍历）；没有"每步清空"的开关（宿主自己 `clear_events()`）；
没有接触**持续**事件（只有开始/结束；`steps_inside` 在传感器报告里）；
没有事件优先级或背压（满了就丢新事件，不是丢最旧的）；编辑器不能清空队列、也没有时间线。

### 8.9 「关节补全：锥形限位 / 位置伺服 / 断裂 / 两端不碰撞」怎么落的（第 23 轮，已跑门禁）

> **状态：已实施**。验证：`cargo test -p quench3d-physics --lib -- ball_cone_limit hinge_servo
> servo_keeps joint_breaks jointed_bodies_stop`、
> `cargo test -p quench3d-cli --lib -- joint_completion_fields`、
> `cargo test -p quench3d-cli --test cli_e2e -- joint_servo_cone_limit_and_break_force`、
> `cargo test -p quench3d-editor --test scene_roundtrip -- export_preserves`。

| 层 | 改动 | 关键点 |
|---|---|---|
| 引擎（锥形限位） | `BallConeLimit { half_angle, local_axis_a, local_axis_b }` + `Joint::Ball { cone_limit }` | **不新写约束行**：把"两轴夹角"当成铰链的限位坐标（`coord`）、把"两轴叉积"当成修正轴（`axis`），直接复用铰链限位那套已经测过的收敛行为 |
| 引擎（锥形限位） | 叉积退化时跳过 | 两轴平行时没有确定的旋转平面（限位本来也没在限），硬算会得到 0/0；这与铰链在轴对齐时的处理一致 |
| 引擎（伺服） | `HingeMotor { target_angle, stiffness, damping }` + `HingeMotor::servo(..).gains(kp, kd)` | 位置环 `ω = kp·wrap_pi(目标-当前角)`，**不做显式 `-kd·ω` 阻尼** |
| 引擎（伺服） | `wrap_pi` 把误差绕回 `(-π, π]` | 少了它：从 179° 越过 180° 时坐标跳到 -179°，误差变成 +358° → 伺服"接着往前推"，门疯狂旋转 |
| 引擎（伺服） | 目标速度夹在 `MAX_SERVO_SPEED`（150 rad/s） | 误差大时比例项会要求几百度/秒，配上大 `max_impulse` 就是一次猛冲 |
| 引擎（伺服） | `damping` 语义改成"每次迭代吸收多少"（`1/(1+kd)`） | 原本的 `-kd·ω` 在序列冲量里会让 ω 在 0 和目标之间来回跳：迭代次数是偶数时原地不动、是奇数时猛冲（实测同样场景在 60 N·m·s 限幅下冲到 81° 停住）。改成松弛系数后收敛不过冲，且**不影响最终停在哪个角度** |
| 引擎（断裂） | `JointSlot.break_force / break_torque` + `set_joint_break` + `broken_joints()` | 力/力矩按"本步累积冲量 ÷ dt"估量级（点/标量约束算力，角度约束/限位/马达算力矩）。阈值 0/负/NaN = 不设 |
| 引擎（断裂） | 判定位置在 `self.joints = joints` **之后** | 求解期间槽位数组被 `mem::take` 借走了，那会儿 `self.joints` 是空的——第一版就是这样**静默失效**的（力算出来 627.84 N > 500 N，关节却不断） |
| 引擎（碰撞） | 关节两端缺省**不互相碰撞**（`collide_connected = false`） | 与 Bullet / PhysX 一致。铰链/球窝两端几何上就重合，让它们碰撞等于门和门轴互挤：实测"伺服停在 116°，目标是 90°"。窄相里按关节表过滤（宽相不管关节） |
| 引擎（睡眠，bug 修复） | 被马达驱动的刚体这一步不许睡（复用缓冲 `scratch_actuated`） | 伺服到位后 ω 正好是 0，只看速度的睡眠判定会冻结它，于是停在没到位的角度再也回不来。判据是"关节有马达"（含 `target_velocity = 0`） |
| 格式 | `cone_limit` / `motor_target_deg` / `motor_stiffness` / `motor_damping` / `break_force` / `break_torque` / `collide_connected` | 单位陷阱：`half_angle_deg` / `motor_target_deg` 是**度**（引擎里是弧度），校验里对"限位 > 6.5 rad""目标角 > 3600°"另有警告 |
| 校验 | 锥形限位给非球窝、半角越界、阈值非正、增益为负 = error；半角≈0、阈值 < 0.5、同时给伺服目标与速度、给错关节类型 = warning | "能跑但多半写错"的一律 warning，不升级成 error |
| CLI / 报告 | `simulate` 报告新增 `joints_broken` | 断裂是**运行时事实**，只写进日志就没法断言；报告里给出来，端到端测试就能钉住 |
| 编辑器 | 不做关节面板，但新字段必须原样留着 | 关节走 `base_doc` 直传；`scene_roundtrip` 里加了 needle 与类型化核对，防止"没界面就静默丢字段" |

**没做（诚实边界）**：没有 6DOF / 万向节 / 滑柱关节；没有角度弹簧-阻尼（只有刚性限位与伺服）；
断裂判定是**量级估计**（冲量 ÷ dt），不是精确约束力，也不是"累计损伤"（断了就断，没有疲劳）；
断裂不会产生事件回调（只进 `broken_joints()` 与报告计数）；关节没有可视化与 gizmo（编辑器/视口里
看不到关节，只能看效果）；`collide_connected` 只作用于"这一对"刚体，不递归到子物体；
伺服只有 1 个自由度（铰链），球窝没有伺服；锥形限位是速度级约束（半秒内压回，不是硬夹住）。

### 8.8 「物理材质：静摩擦 + 合成规则」怎么落的（第 22 轮，已跑门禁）

> **状态：已实施**。验证：`cargo test -p quench3d-physics --lib -- static_friction friction_combine`、
> `cargo test -p quench3d-cli --lib -- physics_materials`、
> `cargo test -p quench3d-cli --test cli_e2e -- static_friction_and_combine`、
> `cargo test -p quench3d-editor --test scene_roundtrip -- export_preserves`。

| 层 | 改动 | 关键点 |
|---|---|---|
| 引擎 | `CombineRule`（几何平均/平均/取小/取大/相乘）+ `RigidBody::static_friction` | 缺省规则必须是 `GeometricMean`（摩擦）/ `Max`（恢复），否则**现有场景的观感会变**（digest 回归测试就是这条的护栏） |
| 引擎 | 静/动摩擦的判定用**施力之前**的速度 | 第一版用施力后的速度：静止箱子被推的那一步速度已经是 Δt×a ≈ 0.08 m/s > 阈值，于是永远走"滑动"分支，"静摩擦"这项形同虚设。现在在 `step` 开头快照速度（复用缓冲，稳态零分配） |
| 引擎 | 每次接触只在**准备阶段**定一次摩擦锥上界 | 若在速度迭代里反复判定，上界会跳变，收敛变差且结果依赖迭代次数 |
| 规则解析 | 先显式、后缺省；两端都显式且不同时取 A（slot 靠前者） | 与 Unity 的 per-collider combine + 工程缺省一个思路。不做"两端的规则再合成一次"：`Average` 与 `GeometricMean` 混在一起没有物理意义 |
| 格式 | 顶层 `physics` 段 + 物体级覆盖字段 | 三段都可选、都不写就是引擎缺省 → 老场景逐位不变 |
| 编辑器 | 「静摩擦」滑块 + 两个合成规则下拉 | 下拉里必须有"（缺省）"项：少了它，用户点过一次就再也回不到"跟随世界缺省" |
| 校验 | 负静摩擦/负阈值 = error；静摩擦 < 动摩擦、阈值 > 1 m/s、没有刚体却写规则 = warning | 后三条都是"能跑但多半写错了"，所以是警告不是错误 |

**没做（诚实边界）**：没有各向异性摩擦/滚动摩擦；没有"摩擦-速度曲线"（只有静/动两档）；
两端都显式指定**不同**规则时只有一条确定性优先级（slot 靠前者），格式层没有额外警告；
`static_friction_speed` 是引擎级参数，没有按材质给不同阈值。

### 8.7 「睡眠 API 补全」怎么落的（第 21 轮，已跑门禁）

> **状态：已实施**。验证：`cargo test -p quench3d-physics --lib -- wake_up_all`、
> `cargo test -p quench3d-cli --test cli_e2e -- wake_action_with_star`。

| 层 | 改动 | 关键点 |
|---|---|---|
| 引擎 | `PhysicsWorld::wake_up_all() -> usize` | 返回"唤醒几个"是**可观测性**的一部分：调用方（报告/状态栏/测试）能确认它真干了事，而不是"调用了一个 void 函数然后假设它生效" |
| 引擎 | `RigidBody::sleep()` 由 `pub(crate)` 改为 `pub` | 文档里写清与**自动睡眠**的差别：手动是立即的、且**清零速度**；自动要看整岛计时器（逐体入睡会让堆叠渗下去） |
| 引擎（行为修正） | `update_sleeping` 第一遍加 `if !allow_sleep { sleeping = false; ... }` | 这是一个真 bug：`allow_sleep = false` 的刚体一旦被手动 `sleep()` 就永远醒不过来（那一遍只看速度）。在 `sleep()` 公开之前这个状态从 crate 外**无法构造**，所以修正它不会影响任何已有场景 |
| 场景格式 | `actions[]` 的 `wake` 接受 `target: "*"` | 别的动作写 `"*"` 报错（`actions[i].target`）：一个"看起来作用于全部、其实只作用于一个"的动作是最难查的静默失效 |
| CLI / 报告 | `PreparedAction.handle` 改成 `Option<BodyHandle>`（`None` = 全部） | 用类型表达"没有单个目标"，而不是拿一个假的 `BodyHandle{0,0}` 当哨兵 |
| 编辑器 | 工具栏「☀ 唤醒全部」+ 状态栏报数 | 场景级操作放工具栏（不属于任何物体）；不写历史——它不改场景内容，只改运行时状态 |

**没做（诚实边界）**：没有"睡着的物体列表"查询 API（要统计就自己遍历 `bodies()` / 报告里的
`sleeping` 计数）；`wake_up_all` 不重置 `sleep_timer` 之外的任何东西、也不 bump `version`
（位置没变，查询索引不受影响）；没有"按区域唤醒"（用 `probes` + 自己遍历即可）。

### 8.6 「光照补全：雾 / 阴影质量 / 天空颜色」怎么落的（第 20 轮，已跑门禁）

> **状态：已实施**。验证：`cargo test -p quench3d-cli --lib -- fog_shadow_quality`、
> `cargo test -p quench3d-cli --test cli_e2e -- fog_and_shadow_quality`、
> `cargo test -p quench3d-render --test fog_and_shadow_quality`（像素级，本机有 GPU 真跑）、
> `cargo test -p quench3d-editor --test scene_roundtrip -- export_preserves`。

| 层 | 改动 | 关键点 |
|---|---|---|
| 场景格式 | `lighting.fog` / `lighting.shadow_quality` / `lighting.sky_colors` | 三节都按"**不写 = 保持原样**"设计：fog 不写 = 关闭（逐位相同），shadow_quality 不写 = 引擎缺省，sky_colors 只覆盖写了的那几个通道（缺省跟随 preset） |
| 颜色空间 | fog / sky_colors 用**线性**（与 `point_lights.color` 一致） | 只有 `materials.*.color` 是 sRGB；同一个文件里两种约定，所以在 schema、README、字段注释三处都写明，并给"颜色 > 4 就警告"的兜底 |
| 校验 | 负颜色/负密度/强度越界/半径非正/天空颜色为负 = error；密度 > 1、半径 > 400、颜色 > 4 = warning | 路径指向具体字段（`lighting.fog.density`），NaN/inf 也挡（JSON 里没有 NaN，但代码路径能构造出来，校验不能指望 serde） |
| CLI | `apply_lighting` 应用三节；`schema --json` 的字段表与语法摘要同步 | `--dry-run` 不需要额外处理：这些字段只影响像素，不影响"会不会写盘" |
| 编辑器 | 光照面板加"天顶/地平线颜色、阴影强度/覆盖半径、雾色 + 密度"；导出从引擎写回三节 | 颜色控件改的是**传进去的数组**，必须像材质颜色那样"拷进局部数组 → 改 → 写回"，否则改了不生效还留一笔历史（第一版就踩了） |
| 阴影分辨率 | **故意不进格式** | 它是渲染器构造参数（纹理 + 绑定组在 `Renderer::new` 里建），运行时改需要重建。写进格式只能做一个"看起来能配、其实不生效"的字段——不如不写，把边界说清楚 |

**踩过的坑**：① 颜色控件写回（见上）；② 端到端测试里 `render.output` 是相对路径，`--out` 必须给绝对路径，
否则 PNG 会被写到 crate 目录里（第一次跑真的污染了仓库目录）；③ 阴影像素测试第一版的太阳几乎正上方，
影子被遮挡物自己挡住 → 只测到 65 个变化像素；把遮挡物放大 + 太阳斜射才稳定（说明"像素测试"失败的
原因经常是**取景**而不是功能）。

**没做（诚实边界）**：阴影贴图分辨率不在场景文件（改它要重建纹理与绑定组）；没有线性雾的起止距离
（引擎只有指数雾）；没有 IBL/环境贴图（#26）、没有聚光灯与点光阴影（#24）、没有基于日期的天光（#24/#26）。

### 8.5 「查询补全 + 查询加速结构」怎么落的（第 19 轮，已跑门禁）

> **状态：已实施**。验证：`cargo test -p quench3d-physics --lib`（6 个新用例，不需要显卡）、
> `cargo test -p quench3d-physics --doc`、`cargo test -p quench3d --lib center_probe`、
> `cargo test -p quench3d-cli --test cli_e2e query_`、`cargo test -p quench3d-cli --lib -- mcp`。

| 层 | 改动 | 关键点 |
|---|---|---|
| 引擎（补全） | `raycast_all_filtered` / `overlap_shape_filtered` / `distance_to` / `QueryFilter` | 排序规则钉死（距离升序、平局按槽位下标）→ 与"取第一个"的 `raycast` 逐位一致；`overlap_sphere` 改成 `overlap_shape` 的一层壳，不再有两份实现 |
| 引擎（加速） | `QueryIndex`（`build_query_index()`）+ `PhysicsWorld::version` | 网格策略与**宽相共用同一份** `pick_cell_size` / `spread_into_cells`；射线 3D DDA 只测经过的单元，重叠只扫探针 AABB 的单元；超大物体/半空间升级成"全局代理"（每次查询都测）保证不漏 |
| 引擎（正确性） | 版本号 + 自动回退 | `add_body` / `remove_body` / `body_mut` / `clear` / `step*` 都 `version += 1`；`QueryIndex` 记下建表时的版本，**过期就回退线性扫描**（结果永远一致，report 里 `fallback` / `is_stale()` 是信号） |
| 场景格式 | **没有新字段** | 查询不是场景内容；`--mask` 与 `objects[].collision_layer` 是同一套层号（单向 vs 对称的区别写进了 schema 与 README） |
| CLI | `query` 子命令（`--ray` / `--overlap` / `--all` / `--mask` / `--no-sensors` / `--max` / `--steps` / `--index`） | 报告里 `tested` 是"实际检查了几个刚体"（加速的可见证据）；`--dry-run` / `--no-clobber` / `--out` **明确报错**而不是悄悄忽略 |
| MCP | 新增 `quench3d_query`（只读 / 幂等 / 不出网） | 与 CLI 共用 `runner::parse_ray_query` / `parse_overlap_query`，语法不会漂移 |
| 编辑器 | F7「查询探针」（相机中心射线：命中画红箭头 + 绿色法线） | `center_probe(camera, world, max)` 是纯函数（离线用例钉住）；画线本身没法离线验证（诚实边界） |

**踩过的坑**：① `broadphase.compute_pairs` 在 `proxies.len() < 2` 或"没有可动体"时**直接 return**，
网格根本没建——想复用它当查询索引会踩到"静态场景查不到东西"，所以 `QueryIndex` 自带一份网格
（策略共用、状态独立）；② 一个刚体的 AABB 会覆盖多个单元，射线会重复测同一个刚体 →
命中要按 `(距离, 句柄)` 去重（排序后相邻，`dedup_by_key`），否则 `--all` 会出现重复条目；
③ 相机射线的**起点在近裁剪面上**（不是相机位置），断言"距离 = 8 - 0.5"会差 0.1，用例里
写成 `起点.z - 0.5`；④ `--dry-run` 这类开关如果不显式拒绝，就会被参数解析静默吞掉。

**没做（诚实边界）**：`shape_cast`（保守推进）仍然线性、没有加速；CLI 一次进程只做一次查询
（单次建索引是净亏，批量查询要自己写 API 循环）；索引是**快照**（世界一动就回退，
没有增量更新）；没有"射线穿透多个物体并返回所有命中点"的变体（`raycast_all` 每个刚体只给最近的一点）；
查询仍然**不看物理分组**（`QueryFilter::mask` 是单向判据，这是设计而非缺失）。

### 8.4 「碰撞过滤 + 传感器事件 + CCD」怎么落的（第 18 轮，已跑门禁）

> **状态：已实施**。验证：`cargo test -p quench3d-physics --lib`（6 个新用例，不需要显卡）、
> `cargo test -p quench3d-cli --lib collision_filter`、`--test cli_e2e simulate_reports_sensors`、
> `cargo test -p quench3d-editor --test hierarchy export_preserves`。

| 层 | 改动 | 关键点 |
|---|---|---|
| 引擎（过滤） | `InteractionGroups { memberships, filter }` + `Proxy.groups` | 过滤放在**宽相**而不是窄相：不配对的连候选对都不产生。判据对称 `A.mem & B.filter && B.mem & A.filter` |
| 引擎（传感器） | `RigidBody::sensor` + `sensor_pairs` / `sensor_events` | 传感器对**不进 `manifolds`**（于是求解器、热启动、island、并行路径全都不用改），只进 `contacts` 与事件；也**不唤醒睡眠体** |
| 引擎（CCD） | `RigidBody::ccd` + `apply_ccd(dt)`（位置积分前） | 中心射线（`raycast_filtered` + `Shape::support(dir)·dir`）→ 只把**法向**位移截到命中点，切向保留 → 位置积分照旧跑。不是精确 TOI |
| 场景格式 | `objects[].collision_layer` / `collides_with` / `sensor` / `ccd` | 缺省全不写 = 与以前逐位相同；层号越界/空掩码/传感器缺刚体/CCD 非 dynamic 都有校验 |
| CLI | `simulate` / `render` / `movie` 三条报告都新增 `sensors[]` | 复用探针的 `ProbeState` 语义（`enter_step` / `exit_step` / `steps_inside` / `entries`），按名字排序保证输出稳定 |
| 编辑器 | 传感器接触画青色 + 导出从刚体读回四个字段 | 没有编辑界面，但**不会静默丢字段**（往返用例钉住） |

**踩过的坑（都留在注释里）**：① `InteractionGroups::default()` 一开始是 `memberships = ALL`，
于是"只想碰第 5 层的墙"会把缺省物体当成第 5 层同类——缺省必须是"第 0 层"；
② CCD 的查询第一版**没查分组**，测试里那颗子弹被它"看不见"的墙拦住了（现在 `apply_ccd`
把分组、传感器、自己都排除）；③ CCD 的第一版实现是**保守推进扫掠**，在**贴着地面滑行**时
会被脚下的地面拖住——"最近物体"永远是地面，扫掠每步只推进地面那么远，而且它返回的命中
就是地面（法线与运动垂直，本不该拦），所以加了地面之后子弹照样穿墙。现在换成中心射线，
只报**沿运动方向**的命中，一次查询就够；④ 传感器的"停在里面"不能靠事件累加步数
（事件只在变化时出现），得靠"本步是否 inside"。

**没做（诚实边界）**：没有回调/事件总线（每步轮询 `sensor_events()`）；
传感器不唤醒睡眠体，所以"睡眠体被别人从触发区里搬走"不会报离开；
过滤只作用于物理（查询不看分组）；CCD 无旋转扫掠、动态×动态只有"两边都开才扫"的语义。

### 8.3 「后处理 FXAA + SSAO」怎么落的（第 17 轮，已跑门禁）

> **状态：已实施**。验证：`cargo test -p quench3d-render --test post_pipeline`
> （**像素级**，本机有 GPU 真跑；3 个新用例：FXAA 两条 + SSAO 一条）、
> `cargo test -p quench3d-cli --lib fxaa` / `ssao`（格式与校验）。

| 层 | 改动 | 关键点 |
|---|---|---|
| 渲染器（FXAA） | `lighting.fxaa` → `FxaaOptions{enabled, edge_threshold, edge_threshold_min}` | 在 tonemap **之后**做（阈值按感知亮度定）；中间目标 `rgba16float`，写 sRGB 目标时硬件编码一次；关闭时不建目标、不跑那一趟 |
| 渲染器（SSAO） | `lighting.ssao` → `SsaoOptions{enabled, radius, bias, strength, samples}` | 深度-only：法线由视空间位置的屏幕导数重建；半球采样黄金角螺旋 + **交错梯度噪声**旋转；判据 `bias < p.z - q.z < radius`（**上界不能省**，否则掠射角下同一个平面自己遮自己） |
| 深度 | `DEPTH_FORMAT` `Depth24Plus` → `Depth32Float` | 前者**不可采样**；SSAO 必须把深度当纹理读。MSAA > 1 时多一趟 `depth_resolve.wgsl`（取第 0 号采样；深度**不能**平均，边缘上的平均深度是虚构的） |
| 管线 | `post_ldr_pipeline`（写 LDR 中间目标）与 `fxaa_pipeline`（写最终目标） | 颜色附件格式不同 = 必须是不同的管线（wgpu 会当场拒绝复用） |
| 编辑器 | 视口面板两个勾选框（FXAA / SSAO） | 开关是**运行时**改 `options`，所以 `ensure_targets` 的提前返回要把"SSAO 深度是否就绪"算进去，否则勾了不生效 |

**踩过的两个坑（都留在注释里）**：
① `in.position.xy` 在片元着色器里**已经是像素中心**（+0.5），再 `+0.5` 会让 bloom 采样整体偏移半像素；
② 遮挡判据一开始只写"谁更靠近相机"，实测**远处平坦地面平均暗 3.5/255**（掠射角下切平面上的
采样点滑到旁边，投回屏幕落在更近的地面上）；加上半径上界后降到 0.45。中间试过"把偏移投影到
法线上"这个更漂亮的判据，结果因为掠射角下法线误差最大，反而恶化到 11/255——**越"正确"的公式越吃误差**，
最后选了不依赖法线精度的深度比较。

**没做（诚实边界）**：AO 没有单独的目标与双边模糊（所以接触处有轻微颗粒，16 抽样 + IGN）；
没有 TAA / 景深 / CSM；FXAA 是 console 档（无子像素锯齿消除）。

### 8.2 「场景层级（父子变换）+ 格式 v2」怎么落的（第 16 轮，已跑门禁）

> **状态：已实施**。验证：`cargo test -p quench3d --lib`（5 个纯逻辑用例，**不需要显卡**）、
> `cargo test -p quench3d-cli --lib`（7 个格式用例）、
> `cargo test -p quench3d-editor --test hierarchy`（7 个集成用例）。

| 层 | 改动 | 关键点 |
|---|---|---|
| 场景格式 | `objects[].parent: Option<String>`，`VERSION = 2` | 用**名字**引用（父物体必须显式 `name`），父物体可以排在子物体后面；校验器拦：悬空父名、自为空、成环（一个环只报一条）、"带刚体还挂父物体" |
| 引擎 | `SceneObject::parent` + `Scene::{world_transform, set_world_transform, set_parent, is_ancestor, children_of, remove}` | 绘制/包围盒/拾取/物理同步全部改走世界变换；`set_parent` 拒绝自环、成环与"带刚体的子物体"；`remove` 把子物体提到祖父、**保持世界位姿**、并把后面的父下标前移 |
| CLI | `setup_visuals` 里把 `parent` 名字解析成下标（两趟：先建物体再连线） | 用"文档下标 → 场景物体下标"的显式映射，不靠名字查找（地面排在物体前面，名字还可能撞车） |
| 编辑器装载/导出 | `scene_to_json` 写局部变换 + `parent` 名字；父物体没被写成物体（地面、模型网格）就丢掉关系并上报 | 悬空 `parent` 会让**整个文件**读不回来，宁缺勿滥 |
| 编辑器操作 | 场景树缩进显示、属性面板"父物体"下拉（`Doc::set_parent`）、gizmo 在世界空间算、写回时换算成局部 | 改父/删父都保持世界位姿；gizmo 的 `start_transform` 用世界变换，写回走 `set_world_transform` |
| 缩放语义 | 子物体**只继承位置与朝向，不继承缩放**（`scene::rigid_only`） | 这个格式里 `scale` 就是图元尺寸：乘法继承会把"挂在大地板下的树"压成一块饼。这是**刻意的**取舍，写进了格式文档、schema 与系统提示 |

顺带修掉一个**真 bug**：`to_euler_xyz_deg` 在万向锁（绕 Y ±90°）时用错了矩阵行，
把旋转反解成"另一个 90°"。单看该物体看不出来（都是转了 90°），但它的**子物体会跑到别处**；
已加穷举角度的回归测试。这条正是"给层级写集成测试"顺出来的 —— 父物体错一点，子物体错一大截。

**没做**：场景树里拖放改父（只有下拉框）；多选成组变换；空物体（没有网格的节点）；
Prefab / 实例（同一模型引用两次会展开成两批物体，不共享状态）。


## 9. 下一件大件：多相机 / 多输出（gap #16 剩余部分）的可执行设计

第 31 轮的核对把 #16 收窄成"只差多相机/多输出"。这里把它写成可以照着做的四步，
**先做第 1、2 步就能形成一个自洽的可用切片**（多个命名相机 + 选择渲染哪一台），
多输出（一次渲多张）留作第 3、4 步：

1. **格式**：`SceneDoc.cameras: BTreeMap<String, CameraDoc>`（可空，缺省空 = 行为不变）；
   `camera` 段保持原样（向后兼容：没写 `cameras` 的场景与今天逐位相同）。
   校验：名字不能与 `camera` 段的语义冲突；空 `cameras` 是警告不是错误；
   `--camera <name>` 指向不存在的名字 → 退出码 2（error.kind = unknown_camera，带 hint 列出可用名字）。
2. **选择渲染哪一台**：`quench3d render/simulate/movie --camera <name>`；不给就还是 `camera` 段。
   报告里加 `camera_name`（缺省 `"default"`）。这一步不需要改渲染循环，只改取相机的那一处。
3. **多输出**：`render.outputs: [{ name?, camera?, width?, height? }]`（可空）。
   给了就按顺序渲 N 张：`render.output` 视为**目录**（或前缀），文件名 = `name` 或 `output-<i>`；
   没给 `name` 且只有一个 output 时保持旧路径（与旧场景逐位一致）。
4. **报告与并行**：`render` 报告加 `outputs: [{ name, path, bytes, digest }]`，`output` 字段保留为第一张；
   多张时先各自渲完再统一写盘（避免半套文件），并在 `--no-clobber` 下先检查所有目标路径。

**测试（都不需要显卡）**：`--camera` 名字解析与错误路径；多输出场景 → N 个文件都存在且尺寸随各自
`width`/`height`；单相机/单输出场景的报告与旧版字段一致（向后兼容）；`--no-clobber` 在多输出下任一目标
存在就整体退出 2 且不写任何文件。

### 9.1 进度（第 34 轮）

**第 1 步已完成**（`cameras` 格式 + 校验 + 预检白名单 + `schema()`/`json_schema()`/系统提示同步 +
往返测试 `named_cameras_parse_validate_and_round_trip`）。**第 2 步未做**：`--camera <名字>`
选择（含 `unknown_camera` 退出码 2 与 hint）；**第 3、4 步未做**：`render.outputs[]` 多输出与
`outputs[]`/`--no-clobber` 语义。下一步从第 2 步开始。

### 9.2 进度（第 35 轮）

**第 2 步已完成**：`--camera <名字>`（`render`/`simulate`/`movie`），未知名 → 退出码 2 +
`error.kind = unknown_camera` + 可用名单；`default` 是 `camera` 段别名。实现取巧但正确的地方：
**只在 `load_scene` 里换一次 `doc.camera`**，下游 `apply_camera` 与渲染报告一个字都不用改。
测试：`camera_select_tests::select_camera_swaps_the_camera_and_reports_unknown_names`（单测）+
`camera_flag_reports_unknown_names_without_touching_the_gpu`（e2e，用 `--dry-run` 证明它不碰 GPU）。

**还没做**：报告里的 `camera_name`；**第 3、4 步**（`render.outputs[]` 多输出、
`outputs[]` 报告与 `--no-clobber` 的"先查全部目标再写"）。下一步从第 3 步开始。

### 11.1 进度（第 41 轮）：模型缓存（CPU 侧）做了什么、没做什么

**做了**：`quench3d::assets::ModelCache`。key 是**规范化路径**，失效判断是 `(文件长度, mtime)`——
文件被改过就当没缓存，**不拿旧几何骗人**。预算按估算字节（顶点 32 B/个 + 索引 4 B/个 + 贴图
RGBA 实际长度），超出按 LRU 淘汰；单个模型自己就超过预算时照常返回、只是不留在缓存里
（宁可每次重解，也不让预算形同虚设）。CLI 侧是一个**进程内**缓存（一次命令一个进程），
三个加载点（建碰撞体 / 上屏 / `info`）都走它，`Engine::add_model` 是为"先加载再上屏"拆出来的。

**可达性**：`simulate` 的报告里多出 `model_cache{hits,misses,evictions,entries,bytes,budget}`；
**没有任何模型参与时不出现这个字段**，所以老场景的报告逐字节不变。

**没做（诚实边界）**：**显存释放**。GPU 缓冲仍然是只增不减——反复 `load_model` 照样涨显存，
本轮只把**解析**（解压 / glTF / 贴图解码）省下来了。要做真正的显存回收，需要引用计数 +
渲染器侧的缓冲池（或者按场景重建整个渲染器），那是另一件大活，没在本轮假装做完。
另外缓存**不跨进程**：跑两次命令就是两次解析（要跨进程就得落盘，属于资源管线后续）。

### 12.1 进度（第 42 轮）：CSM 做完（点/聚光阴影仍然没做）

`lighting.shadow_quality.cascades`（1~4，缺省 **1**）与 `split_lambda`（0~1，缺省 0.5）。
缺省仍是单级、逐像素保持老行为——**加功能不改老场景**这条底线不能破。多级时按视锥做 PSSM 切分，
N 级画进同一张深度图的 2×2 图集，主 pass 按视空间深度选级；每级只有半张贴图，
所以"加级数"换的是近处的清晰度，超远场景还是该提高 `shadow_map_size`。

**验收分两层**：(1) **CPU 侧几何断言**（无显卡可跑）：每一级的矩阵必须把它自己那一段视锥的 8 个角点
全部送进单位立方体——这个不变量比"看着对"可靠得多，测试还把相机**转斜**（正对时错误会被掩盖）；
(2) **GPU 侧**（有卡时跑）：四种 `cascades` 值的校验行为 + 一级与四级渲染出的画面**确实不同** +
两种模式下都真的有影子（最暗像素明显暗过均值）。

**边界已量化**，见下一节 §12.2（结论：级联换到的是**覆盖**，不是"小场景里更清晰"——
在"小场景 + far=500"的位形下单级反而清晰约 33 倍）。点光源/聚光灯阴影仍未做。

### 12.2 级联的量化结论（第 43 轮，`examples/shadow_probe`）

判据不是肉眼：`shadow_probe` 只统计**地面像素**（用红蓝差排除投射体自身），
打印**半影比例**（边缘像素 / 核心像素，越小越陡）与**推导的纹素足迹**（世界单位/纹素）。
同一相机与几何下轮廓不变，所以边缘像素数正比于半影宽度。

**A. 小场景 + `far = 500`（近景：投射体在相机前几米）**

| cascades | 受影响像素 | 核心 / 边缘 | 半影比例 | 最差纹素足迹（世界单位） |
|---|---|---|---|---|
| 1 | 5.39% | 3452 / 506 | **0.147** | **0.0273** |
| 2 | 15.74% | 809 / 7760 | 9.59 | 0.923 |
| 3 | 10.93% | 697 / 4380 | 6.28 | 0.904 |
| 4 | 9.71% | 1508 / 3626 | 2.40 | 0.893 |

**单级胜出约 33 倍**（0.893 / 0.0273）。原因很直白：单级是**手调**的 28 米正交盒 + 整张 2048 贴图
（0.027 世界单位/纹素），而级联必须覆盖到 500 米、每级还只分到 2×2 图集里的半张贴图，
最好的那级也有 0.89 世界单位/纹素。随级数增加单调变好（9.59 → 6.28 → 2.40），但都追不上。

**B. 深景（投射体在 60 米外；单级半径钉 10 米）**

| cascades | 变暗的地面像素 | 落点与解析位置误差 |
|---|---|---|
| 1 | **0（完全照不到）** | — |
| 2 | 5 | 0.5 px |
| 3 | 3 | 0.6 px |
| 4 | 3 | 0.2 px |

这就是级联真正换到的东西：**覆盖**。数量只有几个像素，是因为那个投影在屏幕上本来就小
（占 0.03% 像素）——这也解释了第 42 轮"整幅图统计看不出差别"的困惑：不是没差别，是我在
76,800 个像素里找 3~24 个。

**C. 该说清楚的教训**：第 42 轮我写"多级近处更细、更干净"是**方向说反了**。多级在 A 那种位形下
是**更糊**：核心像素塌了、边缘像素暴涨（3452/506 → 809/7760），我描述为"散开/变糊"，
没有逐像素确认它到底是 PCF 把粗级的影子摊开、还是别的原因，所以不写"一定是摊开"。
用级联的场合是**场景本身纵深大**（B）；小场景要清晰，正确做法是把 `far` 收小或提高
`shadow_map_size`，而不是加级数。

### 13.1 资产解析（第 44 轮）：一个概念不能有三套规则

场景里的"资源路径"本来有三处解析：贴图走 `SceneDoc::asset_candidates`（场景目录 → 当前工作目录 →
引擎搜索路径），模型走加载器自己的规则（原路径、`assets/`、`quench3d/assets/`、crate 目录），
而 `validate` 检查模型时用的又是加载器那套。三者不一致的后果很具体：**模型就放在场景文件旁边**
（最自然的写法）在工作目录不等于场景目录时加载失败，而 `validate` 有可能说没问题。

这轮把三处合并成**一个**候选列表（`asset_candidates` / `model_candidates`），并加上场景级
`assets.search_paths`（排在场景目录之后、当前工作目录之前，相对路径以场景目录为基准）。
错误信息列出**找过哪些位置**（展开成绝对路径），因为"找不到文件"最贵的部分从来不是错误本身，
而是不知道该往哪儿放。

**测试**：`assets_search_paths_and_model_paths_use_the_same_candidates`（解析顺序 / 搜索路径展开 /
找不到时列出的位置 / 空项与不存在目录的诊断 / 不写 `assets` 时序列化里不出现这个键）；
e2e `scene_relative_model_paths_do_not_depend_on_the_working_directory` 全程**不需要显卡**
（用 `simulate` 走物理侧的模型加载）。注意 e2e 里"找不到"的退出码是 **2 而不是 1**：
`validate` 会先拦下来——所以 `resolve_model_path` 的报错路径在 CLI 上是第二道防线，
留着是为了让引擎 API 的调用方也拿到同样的信息。

### 13.5 白名单清空（第 50 轮）："已知欠账"清单要定期回看

第 47 轮为了让"旗标必须写进帮助"能立住，我挂了一张 `DEFERRED` 白名单，把当时没写的旗标
（`--scene/--device/--ray/--overlap/--mask/--max`）放进去，并规定"白名单只能变短"。
第 50 轮去清它，结果 6 项里 **4 项根本不该在里面**：`--ray/--overlap/--mask/--max` 早就写在
`schema` 的 `query` 命令参数里了（描述还相当详细），是我写白名单时只看 `--help` 看漏了。
另外 2 项 `--scene/--device` 更糟：**全仓没有任何代码读过它们**，是死旗标。

于是这轮做了两件事：
1. 规则收紧成"在 `--help` 里 **或** 在 `schema` 的命令参数里"，**白名单清空** —— 不再有
   "已知没写"的中间地带；
2. 死旗标直接删掉。`--device 1` 以前会被静默忽略，比报错更坏。

教训：**"已知欠账"清单是有毒的**——它让"没做"看起来像"已分类"。写它的时候必须记下
"为什么没写"，否则几轮之后连自己都分不清哪些是真缺、哪些是看漏。这轮的两类错误
（看漏 / 死代码）恰好是清单最常见的两种腐烂方式。

### 13.4 `schema.global_flags`（第 48 轮）

`schema` 是 agent 的自描述入口，但它以前只描述"命令"和"命令的参数"。`--json`、`--camera`、
`--asset-path` 这类**对所有命令都生效**的旗标在 schema 里一个字都没有——agent 想知道
"能不能指定资源目录"只能读 `--help`（人写的散文）或者试。这轮补上 `global_flags`
（8 条：`--json/--camera/--asset-path/--out/--width/--height/--base-url/--model`），
并用一条守卫测试把**三方**绑在一起：

| 说的 | 必须真的 |
|---|---|
| `kind=value` 的条目 | 在 `VALUE_FLAGS` 里（否则值会被当成位置参数） |
| 每个条目 | 出现在 `--help` 里（人和 agent 的说明书不能不一致） |
| `--json/--camera/--asset-path` | 必须留在表里（防止以后被顺手删掉） |
| 每个条目 | 有非空 `description`（无可读描述等于没有这条） |

```bash
cargo test -p quench3d-cli --bin quench3d -- schema_global_flags
```

**边界**：`global_flags` 不是全部旗标的清单（命令专属旗标仍在 `commands[].args`），
也不是从 `VALUE_FLAGS` 自动生成的——它是"给 agent 看的最小可用集"，多出来的部分靠
`--help` 与 `commands[].args` 补。自动生成会把内部/调试旗标也暴露出去，反而更难用。

### 13.3 帮助文本与守卫测试（第 47 轮）

`--help` 是人和 agent 唯一能看 CLI 旗标的地方（`schema` 只描述命令与场景字段，不描述旗标）。
第 45 轮加 `--asset-path` 时**忘了写帮助**，而且没有任何东西会提醒——直到这轮自查才发现
`--camera` 也一直没写。现在把 `print_help` 的字面量抽成 `help_text() -> String`（为了可测），
加一条守卫：每个 `VALUE_FLAGS` 里的旗标**要么在帮助里出现，要么进 `DEFERRED` 白名单**，
且白名单里不许留已经写进帮助的项（白名单只能变短）。

```bash
cargo test -p quench3d-cli --bin quench3d -- every_valued_flag
```

白名单在**第 50 轮被清空**了（见 §13.5）：`--ray/--overlap/--mask/--max` 本来就写在 `schema` 的 query 参数里（我当时只看 `--help`，看漏了），`--scene/--device` 则是**从来没有任何代码读过的死旗标**，已直接删掉 —— 与其给死旗标编说明，不如让它消失。

### 13.2 `--asset-path`（第 45 轮）：旗标要登记在**进程级**

搜索路径写进场景是"这份场景自己的规矩"；`--asset-path` 是"这一次运行"的补充（渲染农场、CI、
临时素材目录）。多个目录用 `:` 分隔（Windows `;`，与 `PATH` 一致），相对路径以场景目录为基准，
顺序排在 `assets.search_paths` 之后。

一个实现细节值得记：一开始我把旗标写进 `SceneDoc` 的字段、在两个装载点各调一次——结果
`simulate` 仍然报"找不到模型"。原因是**命令实现内部还会自己 `from_file` 一次**（多输出渲染、
内部校验），逐个传参总会漏。改成进程级 `OnceLock`（`set_cli_asset_paths`）后 `asset_candidates`
无条件读它，所有装载点都看得到。**"旗标有时不生效"是最难查的一类 bug**：能过的地方过得去，
过不去的地方看起来像路径写错了。

测试：`cli_asset_paths_are_tried_after_the_scene_ones`（候选顺序：场景目录 → 场景声明 →
CLI → 绝对路径原样；不写旗标时序列化里不出现该键）+ e2e
`cli_asset_path_flag_resolves_assets_without_touching_the_scene`（不加旗标必失败、加了必成功、
`:` 分隔列表里有一个命中即可、**场景文件逐字节未变**），两者都不需要显卡。

**第 47 轮的自我打脸**：我给"旗标必须写进帮助"写的守卫测试，第一版用 `help.contains(flag)`
判"提到了没有"——`--max` 是 `--max-rounds` 的前缀，于是白名单里的 `--max` 被判成
"已经写进帮助"，守卫自己报错。修法是按词匹配（把子串后面的字符也看一眼：后面跟 `-`
或字母数字就不算独立出现）。**守卫测试自己也会写错**，而它写错的方向通常是"过松"或"过严"：
过松等于没有，过严会在无关的改动上乱叫——两种都得靠立刻跑一遍才知道。

### 12.3 聚光灯（第 49 轮）：角度、余弦与"验证脚本自己也要被验证"

`lighting.spot_lights` 是引擎里第一类**有方向**的局部光。实现上做得最省的一件事是
**复用点光源那条 `array<PointLight, 8>`**：每盏从 32 字节变 64 字节，第三段 `dir_outer`
的 `w` 当开关（0 = 点光源，> 0 = 外锥角余弦），第四段 `cone_inner.x` 存内锥角余弦。
好处是着色器只有一个循环、一份衰减、一份 PCF；代价是 `FrameUniforms` 从 960 变 1216 字节
（布局测试与 WGSL 同步改，`frame_uniform_layout_matches_wgsl` 钉死）。

**量化验证**（本机 GPU；CI 无显卡，只跑 CPU 侧的打包/校验/布局测试）：顶视 120 米地面、
`sun_intensity = 0`、`ambient = 0`、`grid = false`，一盏 9 米高的聚光灯，256×256：

| 场景 | 高亮(>150)像素占比 | 按 tan²(外锥角) 的预期 |
|---|---|---|
| 无灯 | 0.00% | 0 |
| 内 8° / 外 12° | 0.53% | 0.50% |
| 内 8° / 外 45° | 11.06% | 11.0% |
| 内 8° / 外 12°，灯斜 30° | 0.83% | 光斑移开、面积随入射角变大 |

误差 6% 以内 —— 亮区面积随外锥角的平方增长，说明"角度 → 余弦 → `cos_angle` 比较"这条链路
是对的，而不是"看起来有个亮斑"。

**这轮最该记的教训是"验证脚本本身也要被验证"**，一共踩了四次：
1. 改 `common.wgsl` 时用正则截 `struct PointLight {…}`，正则把结尾的 `}` 吃掉，
   `old.replace('};', …)` 什么都没替换 —— **脚本照样打印 ok**。之后加了"写之前先断言内容真的变了"。
2. 探针场景把 `camera.target` 写成 `target`（真名是 `look_at`）、`ambient` 写成数组（它是 `f32`）、
   用 `sun_intensity: 0` 以为就黑了（**天空色贡献 65% 环境光**），于是"暗场景"根本不黑。
3. 用 `dir_outer` 之前忘了 `l2` 还没定义 → 着色器编译期报错（好在 wgpu 立刻报）。
4. 断言常量拍脑袋写（要求窄锥亮区 > 1%），而真实值是 0.53% —— **数据一直是对的，是我把尺子刻错了**。
