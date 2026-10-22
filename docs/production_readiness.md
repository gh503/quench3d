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
| 前向 PBR | 全部 | 金属度/粗糙度工作流 + IBL 环境贴图（等距柱状 HDR/PNG/JPEG，手工预滤波 cubemap）；直接光照手感正确 | ✅ |
| HDR 管线 | three.js / Unity | `rgba16float` 中间目标（`renderer.rs:41`）+ ACES 色调映射（`shaders/post.wgsl:26`，Narkowicz 近似）+ 可选半分辨率 bloom（`renderer.rs:253`、`shaders/bloom.wgsl`）+ 标量曝光（`light.rs:177`） | ✅ |
| 色调映射可选性 | 全部 | 算子**不可选不可关**，只有一条 ACES 路径（→ H1） | 🟠 |
| 阴影 | 全部 | 方向光 1~4 级 CSM（2×2 图集，视空间深度选级，`lighting.shadow_quality.cascades`）+ 3×3 PCF；聚光灯一张透视 2D 阴影图、点光源立方体阴影（各自整帧一盏，缺省关闭）。分辨率 512–4096（缺省 2048） | ✅ |
| 阴影可调参数 | Blender / Unity | bias 是编译期常量（`renderer.rs:795`），**没有公开字段**；只有 `DirectionalLight.cast_shadows` + 全局开关 | 🟠 |
| 后处理链 | 全部 | 固定链路（阴影 → PBR →[深度解析]→ bloom → post(SSAO) →[FXAA]），可按开关增减；**无 TAA、无 DOF、无 AO 去噪、无自定义 pass / render graph**。**GL/WebGL2 上 SSAO 与深度解析两趟按能力整体关闭**（§12.7） | 🟡 |
| 抗锯齿 | 全部 | MSAA 可配 `clamp(1,8)`（`context.rs:69`），场景 `render.msaa` 缺省 4；**GL/WebGL2 上按能力夹到 1**（多采样纹理在 WebGL2 的 glow 里是 `panic!`、在原生 GL 上出黑帧，§12.7） | 🟡 |
| 网格与顶点属性 | 全部 | position/normal/uv/tangent + **GPU 蒙皮**（glTF `JOINTS_0/WEIGHTS_0` 顶点属性，≤4 关节/顶点，128 关节 uniform 调色板，8 KB；只满足 WebGL2 最低 4 KB uniform 的设备需 CPU 回退；纯蒙皮走 GPU，带 morph 仍走 CPU）+ **morph target**（静态 `models[].morph_weights` + glTF `weights` 动画轨道；双 clip 只取主 clip）；**无顶点色** | 🟡 |
| 实例化 | 全部 | 不透明有（每实例 model + 颜色 + 材质覆盖，`renderer.rs:1032`） | ✅ |
| 透明排序 | 全部 | 半透明按实例 AABB 中心到相机的距离**全局由远到近**逐实例绘制，不同材质/网格之间也正确；代价是透明物体不合批 | ✅ |
| LOD | 引擎级 | 手工 LOD：`models[].lods` 按相机距离换静态粗模，10% 滞回；无自动减面/自动生成、动画模型不支持、无遮挡剔除 | 🟡 |
| 相机 | 全部 | 透视 + 正交（`camera.rs:7`）；**一次 render 只吃一个相机**，无多视口 | 🟡 |
| 天空 / 环境反射 | 全部 | 程序化天空 + 常量环境项 + **IBL 环境贴图**（`lighting.environment_map`，64px 7 级预滤波 cubemap）；无图时退回解析式天空；**无精确 PMREM/GGX、无 SSR** | 🟡 |
| 调试叠加层 | 全部 | 网格 / 坐标轴 / 接触点 / 包围盒线框；**动捕 3D 骨骼**也走调试线段 | 🟡 |
| 摄像头/动捕 | Unity + MediaPipe/SMPL-X | **实验**：普通 RGB → MediaPipe Pose 33 点（2D/3D）+ 分割轮廓 + 可选 MiDaS 相对深度；编辑器显示画面/轮廓/骨架并叠 3D 骨骼；深度可按内参反投影成相对尺度点云并在视口显示。单帧、单人、无体型拟合、无米制深度、无 TSDF、Web 未接 | 🟡 |

### 1.2 光照

| 能力 | 对标 | quench3d 现状 | 等级 |
|---|---|---|---|
| 方向光 | 全部 | 有（含阴影开关，`light.rs:165`） | ✅ |
| 点光 | 全部 | **有，最多 8 个**（`light.rs:103`、`common.wgsl:30`），半径为平滑衰减（`pbr.wgsl:214`） | ✅ |
| 聚光 / 面光 | 全部 | **聚光有**（内外锥角平滑过渡 + 可选聚光阴影）；面光没有 | 🟡 |
| 环境光 / 曝光 | 全部 | 常量环境项 + 标量曝光（`light.rs:167/177`） | 🟡 |
| 每灯光参数可调 | 全部 | 引擎侧有颜色/强度/半径/锥角/投影；编辑器「光照」面板可增删点光与聚光并调参 | ✅ |
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
| 关键帧动画 | 全部 | glTF 节点级 TRS 动画（STEP/LINEAR）+ GPU 皮肤蒙皮 + 双 clip 混合（一次性淡入）+ **参数/事件驱动状态机**（浮点参数、6 种比较、`on_event` 离散事件）；编辑器有**暂停可拖的时间检查器**、参数滑杆和事件按钮；无参数曲线、事件队列、完整时间轴/混合 UI | 🟡 |
| 骨骼 / 蒙皮 | 全部 | **GPU 蒙皮可用**（纯蒙皮：`JOINTS_0/WEIGHTS_0` 顶点属性 + 每帧关节调色板；`Scene::gpu_skinning=false` 退回 CPU）；**morph target 可用**（静态场景权重 + glTF `weights` 权重动画；双 clip 按 `blend_weight` 插值 morph 权重）；**两骨 IK 可用**（`models[].ik`，每帧采样后解目标、CPU/GPU 蒙皮与 F8 骨架一致）；**morph+skin 的 GPU 路径仍无** | 🟡 |
| 音频 | Unity / Godot | 无 | ❌ |
| UI / 输入映射 / 存档 | Unity / Godot | 无 | ❌ |

### 1.5 编辑器交互

| 能力 | 对标 | quench3d 现状 | 等级 |
|---|---|---|---|
| 变换 gizmo（移动/旋转/缩放） | 全部 | **本轮补上**（世界/局部、Ctrl 吸附、Shift 精细、Esc 取消、屏幕定长、10 px 抓取、命中优先于拾取） | ✅（本轮） |
| 撤销 / 重做 | 全部 | **本轮补上**（Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y，工具栏带可用态） | ✅（本轮） |
| 数值输入（拖拽改值的输入框） | 全部 | 有（egui `DragValue` + 滑杆） | ✅ |
| 多选 / 框选 | 全部 | **Shift/Ctrl 点选 + 组变换**（平移/绕中心旋转/缩放、批量复制/删除）+ **Shift/Ctrl 拖动框选**（按屏幕中心、追加、空框清空）+ **父子同选只动最上层祖先**（第 106 轮） | ✅（框选按中心，不做遮挡判断） |
| 复制粘贴（Ctrl+C/V） | 全部 | **内部剪贴板第 105 轮**（Ctrl+C 存世界变换+刚体形状，Ctrl+V 偏移粘贴/自动重名/重建刚体/根物体/可撤销）；不碰系统剪贴板 | 🟡 |
| 视口导航（轨道/平移/缩放 + 聚焦） | 全部 | 有（左键/中键/Alt 旋转、右键平移、滚轮缩放），**本轮补 `F` 聚焦选中** | ✅ |
| 网格与吸附 | 全部 | 1 单位网格 + gizmo 吸附；**网格不随尺度自适应** | 🟠 |
| 场景树 / 属性面板 / 状态栏 | 全部 | 有 | ✅ |
| 文件对话框 | 全部 | 无（桌面用路径文本，Web 用上传/下载） | 🟠 |
| 崩溃恢复 / 自动保存 / 布局持久化 | 全部 | **自动保存 + 崩溃恢复 ✅ 第 92 轮，Web localStorage ✅ 第 103 轮**（桌面 30s、原子写、正常退出清理；Web 30s、`localStorage` 版本化键、F9/F10 恢复/丢弃、恢复不覆盖原场景）；布局持久化仍无 | 🟡 |
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
| 错误模型（可机读错误码） | 生产集成 | **本轮**：校验诊断每条都带稳定 `code`（**第 60 轮：18 个码**，文档表 + schema + 系统提示三处同步，有对账测试）；顶层 `error.kind` 仍是粗粒度（`report.rs` 不在本轮围栏内）。详见 §8.15 | 🟡 |
| 文档完整度 | 全部 | README 1300+ 行、10 个示例；缺"色彩管理 / 单位轴向 / 确定性边界"三类**声明**（→ H1/H5/H6） | 🟡 |
| 测试规模 | 全部 | 337 → **本轮 352**（+历史 6、+gizmo 单元 7、+往返 2），0 失败 | ✅（本轮） |
| CI 矩阵 | 全部 | `test`（ubuntu / macOS / Windows）+ `wasm32-unknown-unknown` 四个 job，另有 Pages 工作流 | ✅ |
| API 文档门禁 | 库类项目 | CI 无 `cargo doc` 步骤 | 🟠 |

### 1.11 建模（网格创作）

这一域是 quench3d 与"内容创作软件"之间**最硬的一道墙**：它消费别人做好的模型，自己不产出网格。

| 能力 | 行业门槛（对标） | quench3d 现状（证据） | 验收标准 | 投入量级 | 能否替代 |
|---|---|---|---|---|---|
| 图元创作 | Blender / Maya | 7 种形状（球/盒/胶囊/圆柱/**圆锥**/平面/凸包，`scene_doc.rs` 的 `ShapeKind`）+ 5 种预设网格 | 有 | **已达标** | 部分 |
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
| 稳定错误码 | 生产集成 | **本轮**：**校验诊断**的 252 个出错点全部有稳定 `code`（**第 60 轮复核**：18 个码，`docs/error_codes.md` + `quench3d schema --json` 的 `error_codes` + 系统提示三处同步，逐格对账测试）；运行期错误（`RunError`）与 `error.kind` 仍未细分 | 每个失败路径有稳定 code 且文档化 | 校验路径**已完成**；运行期错误剩余约 0.5 人月 | 🟡 |

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
| 模型**导出** | Blender / 引擎 | **部分**：CLI `quench3d export` 写 OBJ + MTL，或按扩展名 `.gltf` 写**单文件 glTF 2.0**；编辑器顶部「导出 glTF」按钮走同一个导出器（Web 下载、桌面写同名 `.gltf`）。几何/材质标量/alpha/自发光/四类贴图（PNG/JPEG 内嵌）；base64 内嵌；凸包、`models[]` 跳过并报告 → 都能被本引擎重新导入 | `quench3d export scene.json model.gltf` 后再 `info model.gltf`，三角形数与包围盒对得上；编辑器导出后 JSON 节点数 = 物体数 | 已落地 OBJ + glTF（CLI + 编辑器，含贴图与节点树）；动画未导出 | ✅（OBJ/glTF 几何） |
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
| S2 场景可视化 | **不能替代** | IBL 只支持等距柱状 HDR/PNG/JPEG（无 `.exr`/精确 PMREM）；LOD 只有静态手工层级；输出色彩管线单一不可选、无 EXR | three.js 应用里的白模/静态演示 + 批量出图 + 回归门禁 |
| S3 物理仿真 | **有条件替代（窄）** | 无行业基准 V&V；确定性未划跨平台边界；无接触力导出/接触 ID | 确定性刚体参数扫描 / CI 回归 / 教学 |
| S4 游戏 | **不能替代** | 动画、音频、UI、输入映射、存档四件全无；无存档迁移 | 刚体玩法原型的物理核心 |
| S5 Web 分发 | **不能替代** | wasm 11 MB；真机 WebGPU 未验证（只有 SwiftShader）；H1 色彩；无无障碍 | 内部演示嵌入 |
| S6 自动化流水线 | **有条件替代（最接近）** | 可复现构建缺口（本轮已补 3 项，仍缺 SBOM/许可审计）；无迁移器；错误码**校验路径已细化**（第 55 轮，`diagnostics[].code`），运行期错误仍粗 | Blender 无头脚本中"纯仿真/出图/回归"那部分 |

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
| 替代 Blender 无头**批量出图/仿真** | CLI 稳定、可复现构建、像素 golden、分辨率/编码可控、错误可机读 | **最接近**：CLI 与确定性强，**校验诊断已可机读分流**（第 55 轮 `code`），仍缺像素 golden（本轮补了构建可复现） |
| 替代**物理仿真工具** | 行业基准 V&V（如标准落球/堆叠/摩擦锥用例比对）、接触力/接触 ID 导出、确定性边界声明、单位约定 | 只有自洽回归。**不能作为验收依据**，只能作为"没退化"的门禁 |

---

## 4. 隐蔽硬指标（H）逐条与关闭成本

这一节是整份文档最重要的部分：它决定"能不能进生产"，而不只是"好不好用"。

| ID | 硬指标 | 现状 | 关闭成本 | 具体动作 |
|---|---|---|---|---|
| **H1** | **色彩管理**：输入/输出色彩空间、色调映射算子、曝光是否可声明与可选 | 只有一条固定 ACES 路径，README 未声明色彩约定 | **低（文档）+ 中（可选算子）** | ① 在文档里写清：工作空间线性、base color sRGB、输出 ACES；② 增加"tonemap 算子可选 + 曝光"两个开关即可进入 H1 门槛 |
| **H2** | **无损往返**：存一次读一次之后场景不变 | **本轮已关闭编辑器侧的主要漏洞**：以前导出会把 `lighting`/`simulate`/`render`/`models`/`probes` 与材质 4 个贴图槽 + 7 个开关静默清零；现在以原文档为底稿，只覆盖编辑器管得着的字段，并有"存→读→再存字节相同"的固定点测试。**仍缺**：导入的 glTF 模型在编辑器里被移动后，位置改动写不回 `models` 引用 | **中** | 已做：固定点测试 + 底稿合并 + 悬空关节/动作清理。待做：模型实例的"位置回写引用"或"展开为 objects"二选一并测试 |
| **H3** | **可复现构建**：别人拿到 commit 能构建出同样东西 | 无 `rust-toolchain.toml`、CI 未 `--locked`、无 `deny.toml`/SBOM | **低**（本轮做掉大半） | `rust-toolchain.toml` + CI `--locked`（本轮）→ 许可审计与 SBOM 留待下一轮 |
| **H4** | **真机设备矩阵**：真浏览器/真 GPU 上跑过 | 只有 headless Chrome + SwiftShader（软件光栅）。**第 57 轮之前这一行其实是不成立的**：WebGL2 上编辑器**启动即 panic**（SSAO 读深度、MSAA 多采样纹理两条 GL 不支持的路），而门禁与 CI 都跑在 Vulkan 上 —— "矩阵里有一行"不等于"那一行绿"。现在有一条可复现的真 WebGL2 探针（`tools/probe_webgl2.sh`，判据 = 第一帧已渲染 + 遮罩收起 + 没有 `INDEPENDENT_BLEND`/Cube 启发式日志），且 GL 上会真正降级的能力都有关卡（§12.7）；**仍然全是软件光栅**（llvmpipe / SwiftShader），没有真 GPU 记录 | **中** | 至少一条"真机 WebGPU/真机 WebGL2"人工验证记录 + 一张矩阵表（Chrome/Firefox/Safari × 独显/核显），每行都要有一条能重跑的命令（`tools/probe_webgl2.sh` 是 WebGL2 那行的模板） |
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
- 边界（诚实）：不支持拖拽中键入数值；无轴心切换（单选是物体原点、多选是选中中心）；不支持框选；
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

- 框选、灯光编辑面板、材质贴图槽界面、glTF 导入进文档层；
  （场景层级与父子变换已在第 16 轮补上，见 §8.2）；
- 像素 golden 进 CI、场景格式迁移器、许可审计与 SBOM；
  （~~错误码~~ **已在第 55 轮补上**：校验诊断的稳定 `code`，见 §8.15；运行期错误的码仍未做）
- 色彩管理（H1）、单位/轴向（H6）、确定性边界（H5）的正式声明 —— 三者是**纯文档缺口**，列入第二晚。

## 6. 三晚路线（按 ROI 排序）

**第一晚（本轮）**：撤销/重做、变换 gizmo、工程卫生三项 + 本文档。

**第二晚（P1，把"S6 能替代"钉死）**：
1. 像素 golden 进 CI（单帧固定场景 → PNG → 与基线比对，容差上限给定）；
2. 往返测试断言"贴图引用不丢" + 场景格式补贴图字段 + 导不出时报错；
3. 场景格式迁移器 + "v1 文件读进 v2"测试；
4. 色彩管理（H1）与确定性边界（H5）、单位约定（H6）写成正式声明文档；
5. ~~错误码：CLI 错误加稳定 `code` 字段。~~ ✅ **2026-10-19 第 55 轮**：**校验诊断**的
   `diagnostics[].code` 已落地（第 60 轮：18 个码 + 文档表 + schema + 系统提示 + 对账/稳定性测试 + 变异验证，
   见 §8.15）。**仍缺**：顶层 `error.kind` 与运行期 `RunError` 的细分（要动 `report.rs`）。

**第三晚（P2，编辑器体验闭环）**：
1. 框选（Shift 加选与组变换已在第 101 轮落地）；
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
模型**导出**已补 OBJ（图元 → OBJ/MTL）与单文件 glTF 2.0（第 107 轮，几何/材质标量/内嵌 base64；第 109 轮补 PNG/JPEG 贴图内嵌），都能被本引擎重新导入；仍缺动画导出。
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

### 8.15 「校验诊断的稳定错误码」怎么落的（第 55 轮，已跑门禁）

> **状态：已实施**（`diagnostics[].code`）。验证：
> `cargo test -p quench3d-cli --lib -- error_code`、
> `cargo test -p quench3d-cli --lib -- diagnostic_codes_are_stable`、
> `cargo test -p quench3d-cli --lib -- serde_errors_classify`、
> `cargo test -p quench3d-cli --lib -- schema_error_codes`、
> `cargo test -p quench3d-cli --test cli_e2e -- diagnostics_carry_stable`。

这一轮补的是 §1.12 与 S6-Q2 上那条"错误码只有 4 类"的欠账：agent 以前只能按中文
`message` 分支，文案一改分支就烂。现在**每条校验诊断都带一个稳定的 `code`**。

| 层 | 改动 | 关键点 |
|---|---|---|
| 数据 | `Diagnostic.code: ErrorCode`（`scene_doc.rs`） | `code` 说**性质**、`path` 说**位置**、`message` 只给人读。码表故意窄（第 60 轮：18 个），不是"一条诊断一个码"——后者既不稳定也没法穷举。老 JSON 没有 `code` 时反序列化成兜底码 `invalid`（`#[serde(default)]` 向后兼容） |
| 生成 | `define_error_codes!` 宏 | **一处声明**同时生成枚举、`ALL`、`TABLE` 与 `as_str`/`meaning`/`agent_action`。这样"加了变体忘了加总表"在结构上不可能（`as_str` 的 `match` 穷尽）；线上名字来自同一个字面量，不手写第二份 |
| 分类 | `classify_serde_code` 挂在 `check_at` 这个**唯一出口** | 未知字段/类型不对/枚举拼错/缺字段全部从这里出去，所以字段级诊断不可能漏码。散在几十个 `check_*` 里各写一遍必漏 |
| 调用点 | 232 个 `Diagnostic::error/warn` 全部改成 `error_code`/`warn_code` | 两参数的老构造器**保留**（围栏外的 `generate.rs` 测试夹具还在用），它给兜底码 `invalid` |
| 文档 | `docs/error_codes.md` | 码表由 `quench3d schema --json` **生成**（文件里有 BEGIN/END 标记），不是手抄；文档开头给了一条能重算整表的命令 |
| schema | `schema()["error_codes"]` + 系统提示 | agent 的自描述入口能查到"码 → 含义 → 该做什么"；系统提示里也写了"按 code 分支、别按文案" |

**测试（无显卡，全部本机跑过）**：

| 测试 | 盯什么 |
|---|---|
| `error_code_table_is_self_consistent` | 码名唯一、snake_case、无 `|`（会破坏文档表格）、serde 名字与表一致、缺省是兜底码 |
| `error_codes_doc_matches_the_table` | `docs/error_codes.md` 的码/含义/动作与 `ErrorCode::TABLE` **逐格**对账 |
| `diagnostic_codes_are_stable_and_specific` | **稳定契约**：16 个非兜底码**各有真实触发场景**，钉住 `code` + `path`；末尾反向断言"覆盖集 == 码表 − `invalid`"，所以"加了码却没有场景会发出它"本地就红 |
| `every_code_in_the_table_is_emitted_by_the_source` | **死码守卫**：码表里每个码在 `mod tests` 之前的源码里至少有一个发出点（用 `include_str!` 读自己） |
| `serde_errors_classify_into_stable_codes` | 结构检查唯一出口的分类规则 |
| `diagnostics_without_a_code_deserialize_to_invalid` | 老 JSON 的向后兼容 |
| `schema_error_codes_match_the_table` / `system_prompt_mentions_diagnostic_codes` | schema 与系统提示没漏码 |
| `diagnostics_carry_stable_machine_readable_codes`（e2e） | 真跑二进制：每条诊断都有 `code`、具体问题给具体码、码表能从 `schema --json` 查到、**文本输出里也带码** |

**代码 ↔ 文档的一一对应怎么复核**（两条命令，17 行对 17 行，`diff` 为空）：
`schema --json` 的 `data.error_codes[].code` 排序 vs `docs/error_codes.md` 生成表第一列排序
（完整命令在 `docs/error_codes.md` 的"覆盖情况"）。注意两个**看着自然但数错**的 grep：
`code: "..."` 在源码里出现 **0** 次（码写在宏声明的 `Variant => "wire",` 里，每码只有这一处），
而文档里反引号包裹的小写标识符有 **53** 个（只有表格行第一列才是码）。

**变异验证（故意改坏 → 测试必须红 → 改回来 → 绿）**：

| 变异 | 变红的测试 |
|---|---|
| 把 `asset_not_found` 改名成 `asset_missing` | `error_codes_doc_matches_the_table`、`diagnostic_codes_are_stable_and_specific`、e2e `diagnostics_carry_stable_machine_readable_codes`（3 条独立守卫） |
| 把 `models[0].path` 的码误改成 `OutOfRange` | `diagnostic_codes_are_stable_and_specific`（文档对账**正确**地保持绿：表没变） |
| 手改 `docs/error_codes.md` 的一格文案 | `error_codes_doc_matches_the_table` |
| 摘掉 `ReservedName` 的唯一发出点（人为造一个死码） | `every_code_in_the_table_is_emitted_by_the_source` |
| 从稳定契约里删掉 `suspicious_value` 的触发场景 | `diagnostic_codes_are_stable_and_specific`（报"缺场景的码 = [suspicious_value]"），而死码守卫正确地保持绿 |
| 停掉 `classify_serde_code` 的 unknown-field 分支 | `serde_errors_classify_into_stable_codes` |

**没做（诚实边界）**：

- **顶层 `error.kind` 仍然是粗粒度**（`scene`/`invalid`/`environment`/`network`/`io`/`runtime`）。
  细分只发生在 `diagnostics[].code` 上。`ErrorInfo` 定义在 `crates/quench3d-cli/src/report.rs`，
  本轮围栏不含该文件，所以没动它 —— 也因此 `RunError`（渲染/仿真阶段失败）仍然没有码。
- 兜底码 `invalid` 仍可能出现：围栏外的 `generate.rs` 两处测试夹具用两参数构造器，
  以及以后新加的检查忘了给码。它不是"正常码"，出现即代表该补。
- 没有做 `--json-schema` 里的 per-code 断言（码表只进 `schema`，不进 JSON Schema）。

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
（引擎只有指数雾）；~~没有聚光灯与点光阴影（#24）~~ IBL/环境贴图（#26）第 100 轮已补（手工预滤波，无精确 PMREM）、
聚光灯（第 49 轮）与聚光/点光阴影（第 54/55 轮）已落地、没有基于日期的天光（#24/#26）。

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
| 编辑器 | F7「查询探针」、F8「模型骨骼」（关节线段 + 关节点） | `center_probe(camera, world, max)` 与 `skeleton_edges` 是纯函数（离线用例钉住边数/端点/世界变换）；画线本身没法离线验证（诚实边界） |

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
没有 TAA / CSM；FXAA 是 console 档（无子像素锯齿消除）。

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

**没做**：场景树里拖放改父（只有下拉框）；框选与父子同选的层级抑制；空物体（没有网格的节点）；
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

**部分做了（第 90 轮）**：**整场景重载**会调用 `Renderer::clear_scene_assets()` 丢掉旧
网格/材质/贴图并重建 `AssetCache`，反复"打开"同一份场景不再累加显存；但**单次场景内**
的 `load_model` / `add_mesh` 仍是只增不减（没有按句柄的 `remove_*`/引用计数）。要做真正的
逐资源回收，需要引用计数 +
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
在"小场景 + far=500"的位形下单级反而清晰约 33 倍）。
~~点光源/聚光灯阴影仍未做~~ —— **第 54 轮**补上了聚光灯阴影（整帧一盏，见 §12.6），
**第 55 轮**补上了点光源的立方体阴影（6 面深度数组，整帧一盏，见 §12.7）；
两套阴影图互相独立，可以同时各有一盏。

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

### 12.7 GL/downlevel 能力矩阵与"不建不出来的管线"这条不变量（第 57 轮）

#### 12.7.1 现场

用户在浏览器里实测到的原文（后端 = `gl`：`navigator.gpu` 存在但没有适配器时的 WebGL2 回退，
限制档位 `downlevel_webgl2`）：

```
[ERROR] Shader translation error for stage ShaderStages(FRAGMENT): WGSL `textureLoad` from depth textures is not supported in GLSL
panicked at wgpu-30.0.1/src/backend/wgpu_core.rs:1414:
  In Device::create_render_pipeline, label = 'quench3d-post'
    Internal error in ShaderStages(FRAGMENT) shader: WGSL `textureLoad` from depth textures is not supported in GLSL
```

**wasm 上管线校验错误是致命的**（wgpu 把它交给未捕获错误处理器 → panic），
所以这不是"某个效果没画出来"，而是**编辑器启动即死**。

修完上面那一段之后，同一个真实 WebGL2 环境里**紧接着撞上第二条 panic**
（说明"只修看得到的那一条"是不够的）：

```
panicked at glow-0.17.0/src/web_sys.rs:4307:9:
  Tex storage 2D multisample is not supported
```

即 12.7.2 里的 MSAA 那条 —— 编辑器缺省 `msaa_samples = 4`，它在**任何**浏览器上都会踩到。

#### 12.7.2 能力矩阵（以本机实测 + naga 源码为准）

| 后端 | 读深度纹理<br>（`textureLoad` / 采样） | SSAO | MSAA > 1 | 依据 |
|---|---|---|---|---|
| Vulkan / Metal / D3D12 / WebGPU | ✅ | ✅ | ✅ | 本机 Vulkan（llvmpipe）出图正常 |
| GL / GLES / **WebGL2** | ❌ **硬限制** | ❌ 按能力关闭 | ❌ 不支持 → **已按能力夹到 1** | 见下 |

"读深度纹理"在 GL 上断在 naga 的 GLSL 后端（`naga-30.0.1/src/back/glsl/writer.rs:4109`），
它把 `texture_depth_2d` 一律映射成 **`sampler2DShadow`**（`writer.rs:599`），于是三条路都不通：

| 写法 | GLSL 后端 | 结果 |
|---|---|---|
| `textureLoad(depth, coord, 0)` | 直接 `Error::Custom` | **建不出管线**（本次事故） |
| `textureSampleLevel(depth, sampler, uv, 0.0)`（非比较采样器） | naga 的 WGSL 校验先拒（`InvalidSampleLevelExactType`） | 建不出管线 |
| `textureSampleCompareLevel(depth, sampler_comparison, uv, ref)` | ✅ 生成 `textureLod(sampler2DShadow, vec3, 0.0)` | 能建，但**返回 0/1 比较结果，拿不到深度值** |

所以"改成采样读深度"这条首选路在 GL 上**不存在**；能做的只有**按能力关掉依赖深度的效果**。

**MSAA（`sample_count > 1`）在 GL 上同样是断的，而且更凶**（这一条是修完上面那个 panic 之后
在**真实 WebGL2** 里紧接着撞上的）：wgpu-hal 的 gles 后端给多采样纹理调
`gl.tex_storage_2d_multisample`（`wgpu-hal-30.0.1/src/gles/device.rs`），而 `glow-0.17`
的 WebGL2 实现里那个函数是 `panic!("Tex storage 2D multisample is not supported")`
（`glow-0.17.0/src/web_sys.rs` 的 web_sys 后端）—— **与浏览器、显卡都无关，是代码里的 panic**。
原生 GL（llvmpipe）不 panic，但解析出来的画面**全黑**（`rgba8` 与 `rgba16float` 一样），
根因在 wgpu-hal 的多采样解析路径或驱动。两条路都指向同一个结论：
**GL 上 `sample_count` 只能是 1**，编辑器缺省 `msaa_samples = 4`（`crates/quench3d-editor/src/app.rs:57`）。

#### 12.7.3 落地：能力判定 + 着色器变体（三处必须同时）

| # | 落点 | 作用 |
|---|---|---|
| 1 | `context::supports_depth_texture_read(backend)`（只有 `Gl` = false） | **唯一判据**，运行期不会变，`Renderer::new` 里定一次 |
| 2 | `renderer::post_wgsl_for(ssao)`：把 `post.wgsl` 里 `//@gl-depth-begin`…`//@gl-depth-end` 圈出的**三段**（深度绑定、`ssao_factor` 一族函数、`fs_main` 的调用点）整段删掉 | 交给驱动**之前**就没有那段代码 |
| 3 | `depth_resolve_pipeline` 变成 `Option`（GL 上 `None`）；`Renderer::supports_ssao()` 报 false，`ensure_targets` 不建解析深度 | 少一条"读多采样深度"的管线、少一张全屏深度贴图 |
| 4 | `context::effective_msaa_samples(backend, requested)`：GL 上一律返回 1（`GpuContext::build` 里唯一调用点，并且**打一条 `log::warn!`**，让"为什么我的 MSAA 没生效"在日志里有答案） | 多采样纹理根本不建，于是 glow 那条 `panic!` 永远走不到 |

**为什么不是"把 `ssao.x` 置 0 让它不执行"**：GLSL 的翻译发生在 `create_render_pipeline`，
naga 会把**整个模块**翻一遍（不理会运行期分支）—— WGSL 里留着那段代码，管线就建不出来。
"运行时判断"在这里救不了场，必须在"交给驱动之前"删掉。删掉后 tonemap / bloom / FXAA
一行不动（在 Vulkan 上强制走 GL 变体渲同一场景，输出**逐字节相同**，因为 SSAO 本来就关着）。

#### 12.7.4 不变量（这一节真正要留下的东西）

> **不变量：任何会在后端 X 上建的管线，其 WGSL 必须能被 X 的语言后端翻译出来。**
> "本机渲得出正确的图"证明的是**运行时行为**，"门禁全绿"证明的是**本机后端能编译** ——
> 两者都不覆盖别的后端（第 54 轮的 X3570 是 FXC，这次是 GLSL，同一个形状的第 2 次）。

守住它的东西是 **一条不需要显卡的测试**：直接调 **naga 的 GLSL 后端**
（`wgpu::naga`，wgpu 30 在 native 上无条件重导出）把"GL 上会建的每一条管线"的每个入口点
真翻一遍，WebGL2 GLSL ES 3.00 与桌面 3.30 两档都过。

> 这条守卫**只在有 GL 后端的平台上**存在（windows / linux / android / freebsd / emscripten）：
> `naga/glsl-out` 是跟着 wgpu 的 gles 后端一起被编进来的，`wgpu-core/Cargo.toml` 的 target 表
> 只在那几个平台拉起它 —— **macOS 上 wgpu 走 Metal，连 GL 后端都不编**，所以那里既没有
> 这个模块、也没有"GL 上建管线"这件事（那里由文本级守卫
> `post_gl_variant_drops_every_depth_texture` 兜底）。
> 这不是理论：第 57 轮推上去之后 CI 的 **macOS 腿**就是这么红的
> （`error[E0432]: unresolved import wgpu::naga::back::glsl`），
> 修法是按 wgpu 的 target 表把守卫收窄到有 GL 后端的平台，**不是**把它删掉或降级成 grep。

```
cargo test -p quench3d-render --lib gl_pipelines_all_translate_through_the_glsl_backend
```

变异验证（把 `post_wgsl_for` 的 GL 分支改回"返回原文"）：测试变红并打印
`Custom("WGSL \`textureLoad\` from depth textures is not supported in GLSL")`；改回来 → 绿。
另两条反向守卫（`full_post_wgsl_is_rejected_by_the_glsl_backend`、
`depth_resolve_wgsl_is_rejected_by_the_glsl_backend`）钉死"这个限制现在真的存在" ——
哪天 naga 支持了，它们会红，那时可以删掉整个变体机制。

**新增后端/新增效果的规矩**：只要 WGSL 里出现"读深度"或别的后端相关写法，
① 先看它属于哪一档能力；② 在 `supports_*` 里判；③ 让**不支持的那一档**在建管线之前就拿不到这段源码；
④ 补一条 naga 翻译守卫。**不允许**"先建了再说，反正运行时不走那条分支"。
**资源类的能力（MSAA、纹理格式、采样数）同理**：判定要落在一个**纯函数**上
（`effective_msaa_samples`），这样它不需要显卡就能被单元测试钉住；判定的**唯一调用点**是
`GpuContext::build`，用户看到的 `msaa_samples` 与真正建的纹理一致 ——
不允许"context 报 4、渲染器偷偷用 1"。

#### 12.7.5 怎么验（三层，从最便宜到最像真机）

```bash
# ① 不需要显卡：naga 的 GLSL 后端翻译 + 能力判定的纯函数
cargo test -p quench3d-render --lib
# ② 本机原生 GL（Mesa llvmpipe）：真建管线、真出图
WGPU_BACKEND=gl cargo run -p quench3d-render --example offline_render
WGPU_BACKEND=gl cargo test -p quench3d-render --no-fail-fast
# ③ 真实 WebGL2（headless Chrome + SwiftShader，要先用 tools/build_web.sh 重建 wasm）
tools/probe_webgl2.sh
```

三层各自的判据：

| 层 | 修前 | 修后 |
|---|---|---|
| ① | 4 条守卫红（`textureLoad` 那段一进 GLSL 就报错） | 全部绿 |
| ② | `panicked ... label = 'quench3d-post'`（exit 101）；就算跳过它，`msaa = 4` 也出**全黑**帧（13 KB PNG，逐像素 mean = 0.0） | exit 0；`msaa = 4` 与 `msaa = 1` 的输出**逐字节相同**（都被夹到 1），非黑像素 100%，整幅均值 `[156.41,165.44,179.80]` 对 Vulkan 的 `[156.48,165.53,179.84]`（差 < 0.1/255） |
| ③ | 真实 WebGL2 里 `panicked ... label = 'quench3d-post'`，遮罩停在「正在创建渲染面…」 | 日志 `编辑器第一帧已渲染：1280x800`，遮罩被收，`tools/probe_webgl2.sh` 判 PASS（0 次 panic） |

#### 12.7.6 明确没做的（**不要**把这一节读成"GL 后端已经无所不能"）

1. **SSAO 在 GL 上是"没有了"，不是"修好了"**。深度-only 的 SSAO 依赖"按像素读深度"，
   而 GL 上这条路是断的（12.7.2 三行表）。想恢复只能换实现（例如把深度**渲染进一张颜色目标**
   再读 —— 需要多一趟全屏 pass，且要重新验证精度），这不在本轮范围。
2. **MSAA 在 GL 上是"关掉"，不是"修好"**：`msaa_samples` 被夹到 1，用户设 4 也没用（会打警告）。
   GL/WebGL2 上想要抗锯齿，得换 FXAA（已有）或以后做后处理 AA。
3. **真机覆盖仍然只有一台**：真实 WebGL2 那一层跑在 headless Chrome + **SwiftShader**
   （软件光栅）上，不是用户的 ANGLE/Intel HD 630。能确定的：**建管线不再失败、第一帧画出来了**
   （SwiftShader 与 ANGLE 都走 wgpu 的同一个 gles 后端 + 同一个 glow 代码路径，
   MSAA 那条 `panic!` 更是与驱动无关）；不能确定的：真机上会不会有别的驱动级差异。
   用户那台机器上的最终确认只能由用户刷新页面给出。
4. 还有一处**没查**的：原生 GL 上 MSAA 解析出黑帧的根因（wgpu-hal 的 gles 解析路径 vs llvmpipe）
   —— 现在 GL 上不建多采样纹理了，这条路径在项目里再也走不到，本轮**没有**继续往下挖
   （挖它要去改 `wgpu-hal`，不在围栏内）。

#### 12.7.7 2026-10-21 线上事故：WebGL2 的 `INDEPENDENT_BLEND` 与 Cube 启发式

用户真实 ANGLE/Intel HD 630 上报了两条错误，第一条直接 panic：

1. `quench3d-alpha` 的 `create_render_pipeline` 报
   `DownlevelFlags(INDEPENDENT_BLEND) are required but not supported`。
   根因是主 pass 有两个颜色目标：颜色 0（HDR）在 alpha 管线用 `ALPHA_BLENDING`，
   颜色 1（水面 mask）却是 `blend: None`。wgpu 把"两个目标 blend 不同"翻译成
   independent blend；WebGL2 没有这个 downlevel 能力，于是建管线失败。
   **修法**：`main_pass_color_targets` 让两个目标共用同一个 blend；alpha 管线的
   mask 也走 `ALPHA_BLENDING`，同时 `pbr.wgsl` 把 mask 的 alpha 写成覆盖
   （非水面 0、水面 1）。这样非水面在 alpha 混合下保持目标值，水面仍然覆盖，
   语义与原来的"mask 不混合"一致。不透明管线两个目标都是 `None`，不受影响。
   守卫：`main_pass_targets_share_blend_for_webgl2`（不需要显卡）。
2. GL 后端把"正方形 + 6 层"的 D2 点光源阴影纹理猜成 `Cube`，而我们按
   `D2Array` 建视图，日志报 `heuristics assumed that the view dimension will be
   equal to Cube rather than D2Array`。**修法**：纹理分配 **7 层**（6 个面 + 1 个
   填充层），数组视图明确 `array_layer_count: Some(6)`；7 不是 6 的倍数，
   wgpu-hal 的 `is_cube_compatible()` 不再成立，GL 会建 `TEXTURE_2D_ARRAY`。
   代价是一张面大小的深度图（默认 512×512×4 ≈ 1 MB）。
   守卫：`point_shadow_texture_avoids_the_gl_cube_heuristic`。

**验证**：

* `WGPU_BACKEND=gl QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test
  post_pipeline water`——原生 GL 上水 mask 两条测试通过；
* `tools/probe_webgl2.sh` 新增两条判据：日志里出现 `INDEPENDENT_BLEND` 或
  `heuristics assumed` 即 FAIL。本机 SwiftShader 实测 PASS（第一帧已渲染、遮罩已收，
  两条错误都没有出现）；
* 用户那台 ANGLE/Intel HD 630 仍需刷新页面确认，因为探针是软件光栅、不是同一块 GPU。

### 12.6 聚光灯阴影（第 54 轮）：一张图、一盏灯、为什么

到第 53 轮为止，场景里唯一会投影的是太阳。聚光灯会把光**穿过**一切：箱子底下、墙根、
桌子下面全是亮的。这一轮让**一盏**聚光灯能投影。

#### 12.6.1 设计取舍

| 取舍 | 选择 | 为什么 |
|---|---|---|
| 阴影图数量 | **整帧一张**，只给一盏聚光灯 | 每盏灯一张图 = 每盏一次投影渲染 + 一张深度图 + 一次采样。8 盏就是 8 次额外渲染；而且 `FrameUniforms` 要变成数组（每盏一套矩阵/参数），改动从"加三个字段"变成"改布局协议"。先做一盏，把**坐标、偏移、验证**这条链走通 |
| 选哪一盏 | **第一盏 `cast_shadows == true` 的聚光灯**，其余勾了也不生效 | 规则要能被作者预测。校验器在 >1 盏时**警告并写明实际用的是哪一盏**（`lighting.spot_lights[i].cast_shadows`），不允许"静默挑一个" |
| 复用哪条路 | **复用平行光那条路的全部零件**：同一个 `SHADOW_FORMAT`（`Depth32Float`）、同一个比较采样器、同一个 `shadow_pipeline`（正面剔除 + 深度偏移）、同一份 3x3 PCF 代码 | 阴影这东西"看着差不多、错了很难查"。零件越少，需要验证的假设越少；PCF 与比较采样器已经在平行光那条路上被跑过几十轮了 |
| 投影方式 | **透视**（fov = 2 × 外锥角 × 1.01 余量） | 聚光灯是点光源，只有透视投影能同时覆盖"近处宽、远处窄"的锥体。正交投影要么浪费分辨率、要么漏掉锥的边缘 |
| 近平面 | `radius × 1%` 夹到 `[0.05, 0.5]`，**绝不为 0** | 见 12.6.2 |
| 深度偏移 | 偏移量在**世界空间**算（固定纹素数 × 该距离上的纹素世界尺寸），再按 `dz/dd` 折算成归一化深度 | 透视下纹素足迹随距离**线性增长**：同一个归一化偏移在 1 米处够用、在 40 米处就太小（或反过来）。所以偏移的"单位"必须是世界长度，不是深度值 |

复用的前提是"两边要的东西一样"。这里有一处**必须**与平行光不同：PCF 里用的是
`textureSampleCompareLevel`（= HLSL 的 `SampleCmpLevelZero`），而不是 `textureSampleCompare`。
后者要算隐式梯度，而它既在循环里、又被聚光灯那段的 `if` 包着 ——
**D3D/FXC 对"非一致控制流里的隐式梯度采样"是硬报错**（X3570 → `D3DCompile 0x80004005`），
整个不透明管线在 Windows 上建不出来（第 54 轮 CI 的 windows 腿就是这么红的 12 个测试）。
深度图没有 mip，LOD 0 与隐式 LOD 结果完全相同，所以这不是功能上的让步。
细节与教训见 `docs/overnight_report.md` §47。

#### 12.6.2 近平面为什么不能是 0（有单元测试钉住）

`near = 0` 时两件事同时发生：

1. `Mat4::perspective_rh` 的两个元素退化成 `0`（`near - far = -far` 仍在，但
   `near * far` 恒为 0），矩阵**不可逆**，`project_point3` 得到的深度没有意义；
2. 更直接的是：`clip.z = -z_view`、`clip.w = -z_view`，所以 `z_ndc = clip.z / clip.w ≡ 1` ——
   **不管物体多远，深度都是 1**。深度图里一个比特的信息都没有，"比较"永远得到同一个结果。

第 2 条是纯 CPU 可以断言的（`near_zero_destroys_every_depth`）：把同一盏灯的 `near` 换成 0，
`spot_shadow_camera` 算出的 5 个不同距离的点深度全是 `1.0`（实测 `[1,1,1,1,1]`），
而正常 `near` 下这 5 个深度严格递增、跨度 0.39。所以"近平面不能是 0"不是经验之谈，
是这条测试在拦。

#### 12.6.3 无显卡也能跑的不变量（CPU 单元测试）

`crates/quench3d-render/src/spot_shadow.rs` 是**纯函数**模块（只依赖数学），所以下面这些
在 CI 上（没有显卡）也真的会跑：

| 不变量 | 为什么它值得一条测试 |
|---|---|
| 光轴上的点 → NDC 中心（`x ≈ y ≈ 0`） | 矩阵的"对准"错了，影子就整体偏；这是最便宜的一条 |
| 锥边界（`outer_cos` 对应的角度）→ `x = ±1 / 1.01` | 钉住"fov = 2 × 外锥角"与那 1% 余量。余量做在 **tan 空间**（`tan(outer) × 1.01`），否则边界比例会随锥角变化 |
| 超出 `radius` 的点 → `z_ndc > 1`（在远平面外） | 远平面必须**真的**截断：灯照不到那么远的地方不该出现"影子" |
| `near = 0` ⇒ 所有深度 ≡ 1 | 见 12.6.2 |
| 偏移折算成世界长度后**与距离无关** | 透视偏移最容易写错的地方；这条把"纹素世界尺寸 × 纹素数"的换算钉死 |
| 挑灯规则与 `pack_local_lights` 的槽位**逐位一致** | 着色器里的 `i` 是"局部光在 uniform 数组里的下标"，点光源排在前。两条路径的下标算法不一致 = 影子长在**别的灯**下面（画面看着"有影子"，但长错了地方） |

#### 12.6.4 本机 GPU 探针：验证方法本身也被验证

CI 没有显卡，所以"画出来对不对"这件事只能用本机探针 + 把结论写进文档：

```bash
cargo build -p quench3d-cli
python3 tools/probe_spot_shadow.py --cli target/debug/quench3d --keep
```

它渲 10 张图（同一套场景，每次只改一个变量），用**纯 python** 读 PNG 做统计（没有 numpy/PIL
依赖），14 条断言全部是量化判据：

| 判据 | 实测 |
|---|---|
| **不放遮挡物**时开投影必须逐像素相同 | 0 / 102400 个像素变（这条同时证明阴影相机与偏移是对的） |
| 放遮挡物时出现暗区 | 3479 个像素（3.40%），最大变暗 65 灰阶 |
| 差异**只有变暗** | 最大变亮 0.0 |
| 暗区重心在遮挡物**朝光的一侧** | +35.7 px（光源 `x=-3`、遮挡物 `x=0`） |
| 遮挡物左移 2 米，暗区跟着左移 | 重心 195.2 → 138.0（遮挡物 159.5 → 119.8） |
| 偏移随遮挡物远离光轴而变小（解析解 1.5 m → 0.5 m） | 35.7 px → 18.1 px |
| 光源镜像到 +x 侧，偏移必须**严格**互为相反数 | +35.7 px vs −35.7 px（和 0.0） |
| 聚光灯前面插一盏点光源（占掉第 0 个槽位）后，暗区位置与面积不变 | 重心 (195.2, 161.6) 两处完全相同；3470 px vs 3469 px |

倒数第二条（镜像对称）比"面积差不多"强得多：场景与相机都关于 `x = 0` 对称，所以镜像图的偏移
必须是原图的相反数 —— V 翻转、uv 镜像、矩阵转置，任何一处镜像错都会破坏它。
最后一条钉的是**槽位**：着色器里的灯下标是"点光源在前、聚光灯在后"的紧凑数组下标，
两条路径的算法一旦不一致，影子就会长在**别的灯**下面（画面仍然"有影子"，但位置是错的）。

**这条探针第一次跑就抓到了一个真 bug**（细节见 `docs/overnight_report.md` §44）：
`pcf_3x3` 里把贴图写死成了平行光的 `shadow_map`，于是聚光灯拿自己的 uv 去采**另一张**图。
表现是"无遮挡物时 79% 的受光像素被压暗"——只看图会以为"阴影函数写错了"，实际上是采错了图。
把它改成把贴图当参数传之后，同一套断言全绿。这正好说明为什么第 1 条判据（**无遮挡物 = 零差异**）
必须存在：它是唯一一条不依赖"影子长在哪"的判据。

#### 12.6.5 诚实边界（这一轮没做到的）

- **只有一盏**聚光灯能投影；第二盏勾上只是被警告。
- ~~**点光源不投影**：那需要立方体阴影图（6 张图 + 面选择）~~ —— **第 55 轮补上了**
  （`texture_depth_2d_array` 的 6 层 + 面/uv 约定做成纯函数，见 §12.7）。
- 没有级联 / 多层聚光阴影；`shadow_strength`（`params.w`）对聚光阴影同样生效（这是刻意的，
  但"聚光阴影的强度"没有独立旋钮）。
- 半透明物体照旧不参与阴影（整个阴影系统都是不透明的）。
- 编辑器里那套"投影"勾选框是**按代码与 CPU 往返测试**验收的：这一轮没有人在真实窗口里点过它。
- 阴影图的**自阴影阈值**（`SPOT_SHADOW_DEPTH_BIAS_TEXELS` 等 4 个常量）是按探针那套场景调的，
  没有在整个场景空间里扫参。

### 12.7 点光源立方体阴影（第 55 轮）：6 个面、一张数组、为什么不用硬件 cube

第 54 轮把聚光灯的阴影做完了（一张 2D 图 = 一个视锥），点光源仍然是"光穿透一切"。
聚光灯解决的是"手电筒/舞台灯"，点光源解决的是"台灯/灯泡"——**同一张桌子旁边放一盏台灯，
桌腿的影子应该往四面八方铺开**，而不是只有一个锥形里的影子。这一轮补上它。

点光源与聚光灯的关系：**同一套零件**（同一个 `Depth32Float`、同一条只写深度的
`shadow_pipeline`、同一份"世界空间固定纹素数 × `dz/dd`"的深度偏移、同一份 3×3 PCF），
唯一的区别是"一张图"变成"六张图"：点光源朝四面八方照，所以是六个 90° 的面，
存成 `texture_depth_2d_array` 的 6 个 layer。

#### 12.7.1 取舍：为什么是深度数组，而不是硬件的 `texture_depth_cube`

硬件 cube 采样会把方向向量映射到某一面 + 面内 uv，**映射规则由图形 API 规定**
（Vulkan/D3D/GL 的表格互有细节差异：面序、u/v 的镜像、±Y 面的朝向）。
本仓库的 CI **没有显卡**，写错一处符号只有"影子在某个面里镜像了"这一种症状，
本地门禁（naga 只做语法与类型检查）抓不到，只能等某台机器的某个后端上看出画面不对。

所以这一轮的约定是"把约定变成等式"：

| 谁 | 做什么 |
|---|---|
| `point_shadow::cube_face_uv(dir)`（**纯函数**） | 方向 → `(面号, u, v)`，六面 `+X, -X, +Y, -Y, +Z, -Z` 编号 0..5 |
| `point_shadow::face_view(layer, ...)` | 第 `layer` 面是一台朝该轴的 90°（+1% 余量）透视相机，`up` 按同一份约定选 |
| `pbr.wgsl::cube_face_uv` | **逐字镜像**上面那个纯函数（6 个分支、同样的符号、面余量从 uniform 取） |
| CPU 测试 `every_direction_projects_to_the_uv_the_shader_will_use` | 2500+ 个方向 × 6 个面：公式给的 `(面, uv)` 与矩阵投出来的 `(面, uv)` 必须相等 |
| CPU 测试 `wgsl_cube_face_uv_mirrors_the_cpu_convention` | 把 WGSL 里那 6 个分支的表达式抠出来逐个比对（面序、符号、层号都钉住） |

代价有两个，都写在代码注释里：
1. PCF 必须用**数组重载** `textureSampleCompareLevel(tex, samp, uv, layer, depth)`
   （HLSL 的 `SampleCmpLevelZero`）。深度数组没有 mip，LOD 0 与隐式 LOD 结果完全相同；
   顺带避开了第 54 轮踩过的坑：非一致控制流里带隐式梯度的采样在 D3D/FXC 上是**硬报错**。
2. 面边界没有硬件接缝过滤，靠"每面 fov 多 1%"（`POINT_SHADOW_FACE_MARGIN = 1.01`，
   在 `tan` 空间上乘）让相邻面在边界处轻微重叠，3×3 PCF 的采样点因此仍落在本面图内。
   边界方向落在 `1/1.01` 而不是 `1.0`——有单元测试钉住这条（`the_face_boundary_lands_just_inside_the_tile`）。

分辨率：单面边长取聚光那张的**一半**（默认 `shadow_map_size = 2048` → 聚光 1024 → 点光每面 512），
6 个面合计 `6 × 512² = 1.5 × 1024²`，即"六面都要清晰"与"每帧多出来的显存"之间的折中
（`Depth32Float` 每面 1 MB、6 面 6 MB）。

#### 12.7.2 深度只跟"主分量"有关（与聚光那一套的关键差别）

面的深度写的是 `z_ndc(d) = far / (far - near) × (1 - near / d)`，其中

```text
d = max(|dx|, |dy|, |dz|)        # 垂直于被选中那个面的距离（"主分量"）
```

——**与朝向无关**，这正是 cube 阴影能只用一个 `near`/`far`/一套偏移常量的原因。
单元测试 `depth_depends_only_on_the_perpendicular_distance` 断言：同样的垂直距离在
`-Y`、`+X`、`+Z` 三个面上给出**同一条深度曲线**，而且严格递增、`radius` 之外越过远平面。

世界空间的偏移同理：一个纹素在该"轴深"上的世界大小是 `2 × tan(半面角) × d / 边长`，
换算到归一化深度要乘 `dz/dd = far × near / ((far - near) d²)`——与聚光灯那条公式一模一样，
只是把"视空间深度"换成"主分量"（`texel_size_grows_linearly_and_the_bias_is_a_constant_world_offset`）。

#### 12.7.3 无显卡也能跑的不变量（CPU 单元测试，10 条）

| 不变量 | 测试 |
|---|---|
| 公式算的面/uv == 面矩阵投出来的面/uv（2500+ 方向） | `every_direction_projects_to_the_uv_the_shader_will_use` |
| 六个轴向落到六个面的中心、面内符号（镜像）逐面正确 | `the_six_axes_map_to_the_expected_face_centers` |
| 45° 边界落在 `1/1.01`（PCF 余量），47° 已交给邻面 | `the_face_boundary_lands_just_inside_the_tile` |
| 深度只与主分量有关、严格递增、`radius` 外越界 | `depth_depends_only_on_the_perpendicular_distance` |
| `near = 0` ⇒ 所有深度塌成 1（与本模块的矩阵对账） | `a_zero_near_collapses_every_depth_in_the_face_matrix` |
| `near` 是半径的 1% 且夹在 `[0.05, 0.5]`；非法半径不产生退化矩阵 | `near_is_a_positive_small_fraction_and_bad_radii_do_not_degenerate` |
| 偏移是"世界空间常数个纹素" | `texel_size_grows_linearly_and_the_bias_is_a_constant_world_offset` |
| 第一盏勾了投影的点光源胜出；名额之外的灯不算 | `the_first_casting_point_light_wins` |
| 选中的下标 == `pack_local_lights` 里的槽位（下标错了影子会长在别的灯下面） | `the_picked_index_matches_where_pack_local_lights_puts_the_light` |
| 投射体球测（灯照得到的盒子都收进来） | `the_shadow_caster_sphere_test_behaves` |

另外三条在渲染器里（不需要 GPU）：`FrameUniforms` 的字节偏移（`point_shadow_params` = 1312、
`point_shadow_texel` = 1328、总大小 1344）、`point_shadow_uniforms` 的"没有投影灯就写负下标"
哨兵、以及上面那条 WGSL 漂移探测。

#### 12.7.4 本机 GPU 量化：探针 + 逐面对账

两件工具，回答两个不同的问题：

**(`a`) `tools/probe_point_shadow.py`**（20 张图 / 23 条判据，CI 没有显卡所以不进 CI）——
回答"影子在不在正确的位置、四个侧面有没有搞混、两套阴影图有没有串"：

| 判据 | 实测 |
|---|---|
| 无遮挡物时开投影**逐像素相同**（零差异基线） | **0 / 102400** 像素变化（这条同时证明 near/far 与偏移是对的：偏移不够，地面自己就会满屏噪点） |
| 有遮挡物时出现暗区、**只有变暗** | 差异像素 7112（6.95%），最大变暗 89 灰阶，最大变亮 **0** |
| 暗区在遮挡物**远离灯**的一侧、且落在地上 | 暗区重心 (263.5, 161.9) vs 遮挡物重心 (212.5, 97.3) |
| 遮挡物离光轴更近 → 暗区也更靠画面中间 | 重心 x 263.5 → 218.6 |
| ±X 面镜像 | 偏移 **+103.5 / −104.5 px**，和 = −1.00 px；两侧暗区面积 7047 = 7047 px |
| ±Z 面：换成 z 方向遮挡物，暗区必须换位置 | 重心 (159.5, 260.3) vs x 方向 (263.5, 161.9)（y 差 98 px） |
| ±Z 面：世界 z 翻转 = 画面**上下**翻转 | +z 在中心之下（260.3 > 160）、−z 在之上（108.3 < 160），x 重心一致（159.5 / 159.5） |
| 透视：靠相机那侧的暗区更大更靠下 | +z 偏移 100.3 px vs −z 偏移 51.7 px |
| 前面多一盏不投影的点光源 | 重心逐位相同（263.5, 161.9）、面积 7047 = 7047 px（`point_shadow_params.x` 是 `point_lights` 里的下标） |
| 只有点光投影 | 差异像素 6745，最大变暗 89 |
| 只有聚光投影 | 差异像素 755，最大变暗 4 |
| 点光 + 聚光**同时**投影 | both vs only-点光：790 px / 最大变暗 27；both vs only-聚光：7063 px / 最大变暗 89（两张暗区都在） |
| 两张暗区各自的"重心半径 20 px 内"都有像素 | 点光 1255 px、聚光 929 px |
| 三张"都不投影"的对照图 | 逐像素相同（0 个像素差，证明上面那些差异不是渲染抖动） |

**(`b`) `cargo run -p quench3d-render --example point_shadow_diag`**——回答"六个面各自的
内容对不对、图上的深度是不是 CPU 算的那个坐标系"。它把 +Y 面也覆盖到了（探针的场景里
灯上方没有东西，那三个面只能靠读回深度来验）：

```text
[基线] 不投影连渲两次：逐位最大差 0（必须 0），差异像素 0
[效果] 打开投影：差异像素 26915 / 102400（26.28%），点光阴影绘制调用 12，单面边长 512
[面 +X] 有内容的纹素   32847 / 262144（12.53%），最小深度 0.810444，重心 uv (0.499, 0.802)
[面 -X] 有内容的纹素   32847 / 262144（12.53%），最小深度 0.810444，重心 uv (0.499, 0.802)
[面 +Y] 有内容的纹素       0 / 262144（ 0.00%），最小深度 1.000000，重心 uv (0.500, 0.500)
[面 -Y] 有内容的纹素   30256 / 262144（11.54%），最小深度 0.808081，重心 uv (0.499, 0.499)
[面 +Z] 有内容的纹素   32847 / 262144（12.53%），最小深度 0.810444，重心 uv (0.499, 0.802)
[面 -Z] 有内容的纹素   32847 / 262144（12.53%），最小深度 0.810444，重心 uv (0.499, 0.802)
[对账] +X 盒子背面 36 个点（深度跨 0.0631）：平均（图上 - CPU）= 0.004596，极差 0.002206，最大 |差| = 0.005809
```

怎么读这几个数字（这几条同时是断言，不满足就 panic）：

* `+Y` 面必须是**空的**（灯上方什么都没有），其余五面各有那个盒子；五个盒子离灯的
  **主分量都是 1.5 米**，所以五面的最小深度都应当≈ `normalized_depth(1.5) = 0.808081`。
  `±X / ±Z` 四个面给出**逐位相同**的统计量（32847 个纹素、重心 `(0.499, 0.802)`），
  这是六面矩阵互为 90° 旋转的一个强证据。
* `-Y` 面为什么是 0.808081 而四个侧面是 0.810444：0.0024 的差来自阴影管线的
  `slope_scale = 2.0` 多边形偏移（`-Y` 面里那个盒子的顶面在视线上是正对的，斜率≈0）。
* **对账**（`+X` 面里盒子背面那片，深度跨 0.0631）：图和 CPU 的差在 0.006 以内、极差 0.0022
  ——残余的那点常数偏移就是上面那条多边形偏移。如果 uv 镜像、面序或 `far/near` 有一处错，
  这批点会横跨那 0.0631（差 10 倍以上）或者直接读到空值 `1.0`。
* 重心 v = 0.802 > 0.5：五个盒子都在灯的**下方**，所以它们在 ±X/±Z 四个面里必须出现在
  贴图下半 —— 这是 v 翻转的探测器。

#### 12.7.5 诚实边界（这一轮没做到的）

- **只有一盏**投影点光源；多盏要"阴影图数组 + 逐灯索引"，这一轮没做（第二盏勾上只是被警告）。
- **面接缝**靠 1% 的 tan 余量 + 3×3 PCF；极端斜面上可能有极轻微接缝，没有扫参验证。
- 单面分辨率是"聚光那张的一半"这个固定关系，**不能单独调**（要调只能调 `shadow_map_size`）。
- 投射体名单用"球与包围盒相交"（点光源照亮的是一个球，没有单一视锥）；但 `shadow_batches`
  仍然只收**镜头看得见**的物体——这是整条阴影链路的既有前提，见 §12.6.5。
- 偏移常量（纹素数、斜率因子、面余量）按探针/对账那套场景调的，没有在整个场景空间扫参。
- 编辑器里那个"投影"勾选框仍然是按代码 + CPU 往返测试验收的：这一轮没有人在真窗口里点过它。

### 12.5 编辑器可达性：局部光列表（第 53 轮）

点光源与聚光灯是**一组**（不是像太阳那样的单例），所以这轮补的是"列表编辑"：
位置/颜色/强度/半径 + 聚光灯的方向与内外锥角，每盏可删，底部可加。
存盘同样从**引擎**写回，由两个**纯函数**完成（`point_light_docs` / `spot_light_docs`），
所以这条链路在 CI 里有 CPU 测试，不需要显卡：

```bash
cargo test -p quench3d-editor --lib -- local_lights_round_trip
```

两个容易踩的点，都写进了代码注释与测试：
1. **"没写"不等于"写了 `[]`"**：点光源的 `[]` 是显式清空（连预设补光也清掉），
   所以在 UI 里删掉最后几盏灯时导出的是"不写这一节"。这类"空 vs 缺省"的区别，
   是场景格式里最容易把作者意图弄丢的地方。
2. **角度是从余弦反解出来的**：引擎只存 `inner_cos/outer_cos`，所以存回文件的角度
   与作者原来写的小数可能差 1e-5 —— 语义相同，但必须写在注释里，否则以后有人会
   以为是浮点 bug 去"修"它。

到这一轮为止，"预览 == 渲染"覆盖了：阴影级联、bloom/fxaa/ssao、局部光。
编辑器仍然**不做**的事（诚实边界）：局部光没有视口 gizmo/选中高亮（要靠坐标数字调），
也没有"从视口点一下放灯"的交互。~~聚光灯与点光源都不投阴影~~ —— 聚光灯的阴影在第 54 轮
补上了（整帧一盏，编辑器里有勾选框，见 §12.6）；**点光源的立方体阴影在第 55 轮补上了**
（6 面深度数组，同样整帧一盏、同样有勾选框，见 §12.7）。

**第 53 轮顺手修掉的一个不对称**：编辑器默认场景以前吃 `Lighting::default()`，而载入/撤销
路径用 CLI 的 `apply_lighting`（没写 `preset` = `Lighting::studio()`，带两盏补光），于是
"存盘再载入"或"撤销一次"会凭空多出那两盏点光源。为什么以前看不见？因为 `scene_to_json`
对局部光"沿用底稿"——**"沿用底稿"是分叉的掩体**：它让三条路径（预览用的默认值 / 载入用的预设 /
存盘写回的来源）各有一套默认值，而表面上一切正常。现在默认场景与载入路径调同一个
`apply_lighting`（改 `pub`），并补了 `default_scene_survives_save_and_reload_byte_for_byte`
（存盘 → 载入 → 再存盘必须逐字节相同）。这类问题只有"逐字节往返"的断言能抓住；
"看起来一样"的截图对比抓不住两盏多出来的补光。

### 12.4 编辑器可达性：级联 UI 与"预览 == 渲染"（第 51 轮）

到第 50 轮为止，`lighting.shadow_quality.cascades` 与 `split_lambda` 是**只有 CLI 能用**的
字段：编辑器既不显示也不应用它们，只会在存盘时"沿用底稿"，连载入 `cascades: 4` 的场景
预览出来的也是引擎默认的单级阴影。这轮补上：

| 环节 | 以前 | 现在 |
|---|---|---|
| 载入 | 不应用（预览 ≠ 渲染） | `install_doc` 把 `shadow_quality` 写进 `RendererOptions` |
| 编辑 | 没有 UI | 阴影一行两个控件（1..=4 / 0..=1），带说明 |
| 存盘 | 沿用底稿（值对但没人知道为什么） | 从 `engine.renderer.options` 写回 |

关键做法是**共用同一份映射**：`runner::shadow_cascades` / `shadow_split_lambda` 改成 `pub`，
编辑器直接调用。两份各自解析是"预览和成品悄悄分叉"的经典来源，而这种分叉没有任何测试会红。

```bash
cargo test -p quench3d-cli --lib -- shadow_quality_survives
cargo check -p quench3d-editor            # 编辑器是 wasm+桌面双目标，门禁里两个 clippy 都会过
```

**第 52 轮补齐**：`lighting.bloom` / `fxaa` / `ssao` 当时**也**不被编辑器应用（预览里没有泛光
与 SSAO，勾掉 FXAA 存盘却还开着）。现在同样走"共用映射 + 载入应用 + 从渲染器选项写回"，
并给泛光加了 开/强度/阈值 三个控件。

**仍然没接的（诚实边界）**：`render` 节里与输出有关的字段（分辨率、输出路径）在编辑器里
本来就是窗口/导出概念，不走 `RendererOptions`；`lighting.preset` 由 CLI 装配代码在**载入时**
应用、编辑器存盘时清掉（见 `scene_io` 的注释：留着 preset 等于"存了但不算数"）。

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

---

## 10. 第 56 轮：关节局部轴按端指定（gap #15 的第二段）—— 取舍 / 不变量 / 边界

### 10.1 这一轮修的是什么（一句话）

格式里只有一根 `axis`，装配时被**同时**喂给 `local_axis_a` 与 `local_axis_b`。这只在
**两端刚体初始朝向相同**时才等于"同一个世界轴"：门板转了 90° 时，世界 Y 在门框局部是 Y、
在门板局部是 Z。两端共用一根局部轴时，求解器的角度约束会先**把门板扭平**再当铰链用
（现象是"门装上去自己弹回原位"），而且**没有任何诊断**。

### 10.2 取舍（为什么这样做 / 为什么不那样做）

| 选择 | 为什么 | 为什么不选另一条 |
|---|---|---|
| 新增 `axis_a`/`axis_b`，**不写 = 回落 `axis`** | 老场景逐位不变（e2e 里钉了"只写 `axis`"与"两端显式抄同一根轴"的 `states` **逐字节相同**）；只写一端也合法，另一端回落 | "必须两端都写"会破坏所有已有场景；"`axis` 是两端缺省值 + 两端可覆盖"其实就是现在这条，只是文档要写清楚 |
| **不**为"同时写了 `axis` 与 `axis_a/axis_b`"报 `ineffective` | `axis` 的语义就是"两端缺省轴"；两端都显式给时它自然不参与，不是"写了不生效" | 报了会误伤"两端都写了、另外留一根缺省轴"的合理写法 |
| 新诊断**复用**三个已有码（`out_of_range` / `ineffective` / `suspicious_value`），不加新码 | 码表故意做得窄：码说**性质**、`path` 说位置。"零向量"就是 `out_of_range`，"写了不生效"就是 `ineffective`，"合法但很可能不是本意"就是 `suspicious_value` | 为"轴"单独造码会让码表膨胀、agent 也没法穷举；`docs/error_codes.md` 的覆盖计数因此不变（第 60 轮复核：252 个调用点 / 18 个码；这一轮新增的 18 个调用点全在 `include` 合并里，见 §14（第 60 轮）） |
| 朝向差用各端物体的 `rotation_deg` 直接算，**不沿父链累乘** | 关节两端**必须有刚体**，而"带刚体的物体不能挂 `parent`"（`objects[].parent` 的校验已经拦死）→ 它们的 `rotation_deg` 就是世界朝向 | 沿父链累乘要再实现一份层级求解（顺序、成环、局部/世界语义），多一份实现就多一个漂移点；真要支持"带父链的关节端"时再抽公共函数（那要先放开"带刚体不能有父物体"） |
| 阈值 **10°** | 两端朝向差 10° 以内，共用一根局部轴带来的轴间夹角误差在求解器增益下几乎看不出来；超过就该吵 | 阈值太小会对"故意轻微错开"的场景刷警告；太大（如 45°）会漏掉 20° 这种真错 |

### 10.3 不变量（钉住的、可复核的）

1. **缺省不落盘**：`axis_a`/`axis_b` 没写 → 存盘文本里不出现这两个字段（`skip_serializing_if`）。
   "不写 = 老行为"因此在**文件层**也成立（单元测试断言文本；e2e 断言物理 `states` 逐字节相同）。
2. **端到端同一根轴**：`joints[].axis_a/axis_b`（文档，各自刚体的局部坐标）→
   `Joint::Hinge{local_axis_a, local_axis_b}`（引擎）→ 两个世界轴 `q_a·axis_a` / `q_b·axis_b`。
   判据是**门板薄轴与铰链轴的夹角**：按端给轴 `|n·Y| = 0.0002`，两端共用 `|n·Y| = 1.0000`。
3. **点约束与轴无关**：换轴不改变"锚点被按住"——两种写法的质心到锚点距离都 = 1.000 米（误差 < 0.02）。
4. **编辑器与 CLI 同源**：编辑器 `install_doc` 走 `runner::build_world`，预览与 `quench3d render`
   用同一份按端取轴逻辑，不会分叉；编辑器存盘不许丢这两个字段（`scene_roundtrip` 钉住）。
5. **schema 与结构体逐字段一致**：`$defs.joint` 少一个字段就红
   （`json_schema_matches_the_source_structs`，本轮变异验证过）。

### 10.4 边界（明确没做的）

- ~~**`local_frame_a/b` 仍然不生效**~~ **第 59 轮补上了**（见**本文末尾的** `## 12.`，第 59 轮）：引擎的固定关节现在可以按
  参考姿态驱动（`frame_a`/`frame_b`）。上面这条保留下来是因为它准确记录了第 56 轮当时的边界。
- **关节可视化没有**（画转轴 / 锚点 gizmo）：编辑器里仍然只能靠数字调。
- **球窝的锥形限位本来就有按端轴**（`cone_limit.axis_a/axis_b`，第 23 轮），这轮没动它；
  它和顶层的 `axis_a`/`axis_b` 是两套字段，别混。
- **只对铰链与滑块有意义**：`fixed` / `ball` / `distance` 没有轴，写 `axis_a`/`axis_b` 会得到
  `ineffective` 提醒（不是错误，场景照跑）。
- **`limits` 的单位跳变照旧**（铰链弧度 / 滑块米）：这轮没碰，`unit_ambiguity` 仍只在大数值时提醒。
- **没查"两端都写了轴但世界轴仍然不一致"**：新诊断只在"只写了 `axis`"时触发。两端都显式给了
  `axis_a`/`axis_b` 时，如果算出来的世界轴依然差很多（同样会把 B 扭一下），现在**不报**——
  可能是作者有意为之（预扭的铰链定义），先不猜；要报得先想清楚"什么算错"。

## 11. 第 58 轮：CI 可靠性 —— 无 GPU 的 Windows runner 上的原生崩溃

### 11.1 这一轮修的是什么（一句话）

CI 的 **windows 腿不再被"创建真实 wgpu 上下文"的测试打死**（`0xc0000005`）：
这类测试改成**显式 opt-in**（`QUENCH3D_GPU_TESTS=1`），CI 只在**能建出上下文**的腿
（ubuntu / macOS）上打开它 —— 假红不再出现、覆盖不丢、跳过有可读原因、开关本身有守卫与变异证据。

### 11.2 可靠性条目（可复核）

| # | 条目 | 判据 / 证据 | 现状 |
|---|---|---|---|
| 1 | **"没 GPU 就返回 `Err`"接不住原生崩溃** | 失败形态是 `process didn't exit successfully (exit code: 0xc0000005)` 而不是 `test result: FAILED` + 断言 → 故障点在 Rust 可观测范围之外（`Result` / `catch_unwind` / panic hook 全拦不住） | 已定性（两次 CI：run 38935053883 `model_instances`、run 38931539230 `backend_fallback`，同 sha 重跑即绿） |
| 2 | **"重跑才是判据"不作为首选** | 它会把真红一起放过去、不可复核，且掩盖"windows 上这类测试覆盖率本来就是 0" | 已明确拒绝（`docs/overnight_report.md` §52.3） |
| 3 | **开关是显式 opt-in，且只认字面量 `1`** | `context::GPU_TESTS_ENV = "QUENCH3D_GPU_TESTS"`；未设 / `""` / `"0"` / `"true"` 一律为关 | 已落地（`crates/quench3d-render/src/context.rs`） |
| 4 | **跳过 = 整条测试体不执行**，不是"删断言 / 体内 `if false`" | 测试把整条测试体（含 `pollster::block_on(...)`）放进 `with_gpu_tests(原因, ‖…)` 的闭包；关着时闭包一次都不调用 | 已落地（`backend_fallback.rs`、`model_instances.rs`） |
| 5 | **守卫测试不需要显卡、也不碰进程环境变量** | `gpu_gate_builds_nothing_unless_explicitly_enabled` 直接喂判定内核的 flag（改环境变量会污染同进程的其它测试） | 已落地，且**变异验证**过 2 条（开关恒 `true` → 红；内核去掉拦截 → 红；改回 → 绿） |
| 6 | **按腿给开关，不写死平台** | `ci.yml`：ubuntu / macOS = `"1"`，windows = `"0"`；`tools/gate.sh` 本机默认 `1`（`QUENCH3D_GPU_TESTS=0 tools/gate.sh` 可关） | 已落地；`cfg!(windows)` 这类写法被明确拒绝（会让本地 Windows 开发者丢覆盖） |
| 7 | **开关不下沉进 `GpuContext::headless`** | 那是 CLI `render` / `movie` 的生产路径，被测试开关影响等于让所有人渲染不出来（默认关） | 已明确拒绝 |
| 8 | **上线实测**：run `38955064316` @ `2a0f0db`，`attempt = 1` | 五腿全绿；`test (windows-latest)` **首跑绿**（同一 target 可执行文件名 `model_instances-8259af2638c9b300.exe` / `backend_fallback-8cfe002d8085f3e7.exe` 上一轮报 `0xc0000005`，这一轮 `ok. 5 passed` / `ok. 7 passed`；全日志 `STATUS_ACCESS_VIOLATION` 0 次）。ubuntu 两 target `0.00s`（内层"没有可用的 GPU"分支），macOS `0.22s` / `0.17s`（真建出 Metal 上下文） | 已落地 |

### 11.3 不变量（钉住的、可复核的）

1. **没设 `QUENCH3D_GPU_TESTS=1` 时**，`backend_fallback` 与 `model_instances` 里"要真建上下文"
   的测试体**一次都不执行**，并各打印一句可读原因（`-- --nocapture` 可见；cargo 默认吞掉通过用例的输出）。
2. **设了 `QUENCH3D_GPU_TESTS=1` 时它们是真跑**：本机 1.99.0 工具链下
   `backend_fallback` 7 passed（0.14s vs 关掉时 0.00s）、`model_instances` 5 passed（0.96s），
   且 `--nocapture` 下**没有**"没有可用的 GPU"那行。
3. **开关的判定与拦截是两段独立的守卫**：解析（只有 `"1"` 是开）+ 拦截（关着不调用闭包），
   任一段被改坏，守卫测试立刻红。
4. **跳过不许是"放宽断言"**：断言原样保留在闭包里；能证明它真的在跑的证据是
   "打开开关时把断言改坏 → 红"这一条变异。

### 11.4 边界（明确没做的）

- **只围了两个测试二进制**（`backend_fallback.rs` / `model_instances.rs`，本轮围栏允许改的文件）。
  仓库里其它"真建 wgpu 上下文"的测试二进制（`post_pipeline.rs`、`shadow_cascades.rs`、
  `material_pipeline_traits.rs`、`fog_and_shadow_quality.rs`、`point_lights.rs`、
  编辑器侧 `hierarchy.rs` / `scene_roundtrip.rs` / `history.rs`）**仍然在 windows 腿上直接建上下文**，
  同类原生崩溃在它们身上依然可能发生 —— 这是**明确记下的欠账**（helper 已就位，逐个文件包一层即可）。
- **本机没有无 GPU 的 Windows 环境**：无法在本机复现那次原生崩溃，
  "windows 不再崩"只能由 CI 的 windows 腿首跑结果证明（本机只能证明"开关真的控制着建不建"）。
- **windows 腿这几条测试的覆盖本来就是 0**（runner 没有 GPU）：本轮是把"0 覆盖"写成
  "0 覆盖 + 可读原因"，不是把 0 变成有。要真有覆盖只能换带 GPU 的 Windows / 自托管 runner。
- **ubuntu 腿上的这几条断言实际也是空的**（实测 `0.00s`；同一腿的 CLI 冒烟打印
  `本机没有可用 GPU，渲染按约定返回退出码 3`）：ubuntu-latest 没有适配器，内层的
  "没有可用的 GPU → `return`"会接住它。CI 里**真的**在断言的是 macOS 腿（Metal，0.22s / 0.17s）
  与本机。要补 ubuntu 的覆盖得在 CI 装软件光栅（llvmpipe / lavapipe）**并另行验证**
  （适配器能不能拿到、断言在软件光栅上是否成立）—— 本轮目标只是"不再制造无法诊断的假红、
  不丢已有覆盖"，没把两件事混在一起。
- **没有加"下次自动检查其它二进制"的门禁**：那需要一份"哪些测试会建上下文"的清单，
  而清单本身会腐烂（第 50 轮"白名单是有毒的"就是这个教训）。

## 12. 第 59 轮：固定关节的参考姿态（gap #15 的第三段、也是最后一段）—— 取舍 / 不变量 / 边界

> **编号说明（很容易看错，先读这段）**：本文件有两个层次的编号并存 ——
> `## N.` 是**轮次章节**（0~9，然后是第 56 轮的 `## 10.`、第 58 轮的 `## 11.`、本节的 `## 12.`），
> 而 `### 11.x` / `### 12.x` / `### 13.x` 是一份**更早的流水记录**，它们挂在 `## 9.` 底下，
> 编号与父章节无关（`### 12.2` 讲级联阴影的量化结论，第 43 轮）。
> 所以 `docs/production_readiness.md §12.2` 这个引用在 `CHANGELOG.md:454` 与
> `crates/quench3d-render/tests/shadow_cascades.rs:4` 里指的是**级联阴影那一节**（`## 9.` 底下），
> **不是**本节。这一节是**顶层的 §12**（第 59 轮，关节参考姿态）。
> 想让引用不含糊，请写「§12（第 59 轮）」或「§9 底下的 12.2（第 43 轮）」。

### 12.1 这一轮修的是什么（一句话）

引擎里 `Joint::Fixed` 一直是**速度级**角约束（锁「创建那一刻」的相对姿态），格式里的
`local_frame_a/b` 从来没写出来过、写了也不生效；这一轮把它变成**参考姿态**约束
（驱动 `q_a⊗frame_a == q_b⊗frame_b`），并在格式里给出 `frame_a`/`frame_b` —— 于是
「把 B 扭到指定参考姿态」第一次可以**写出来**、也**真的会发生**。

### 12.2 取舍（为什么这样做 / 为什么不那样做）

| 选择 | 为什么 | 为什么不选另一条 |
|---|---|---|
| **「两端参考姿态都是单位四元数」= 老行为**（锁定创建时的相对姿态），只要有一端不是单位旋转才切到参考姿态约束 | ① 这是唯一能同时满足「新字段不写 = 老行为逐位不变」与「写了就生效」的判据；② 显式写 `[0,0,0]` 与不写逐位相同，e2e 用 `f32::to_bits()` 直接钉住；③ 与 `axis_a` 的「不写 = 回落 `axis`」是同一种约定（显式默认 = 缺省） | 若把「单位参考姿态」也解释成「扭到两端同朝向」，**所有**已有 fixed 场景（两块朝向不同的板焊在一起）会当场散架（引擎测试里那条变异就是这个：差 89.95°）；改用显式开关字段（如 `frame_mode`）则要新造一个「只在另一个字段非空时才有意义」的字段 |
| 偏置放在**速度层**（Baumgarte 式，一阶，每步收 20%），不加位置层角修正 | 与已有的铰链轴对齐偏置（`angular_rhs` 的 `0.2/dt`）同源同量级；`solve_position` 里本来就只做平移（伪速度），角误差走速度层是这个求解器既有的一致性 | 位置层角修正要新写「用伪速度转刚体」的路径（`apply_pseudo_impulse` 只作用于质心平移），等于给求解器加第二种角修正机制，风险与收益不成比例 |
| 误差取**最短路径**（`w < 0` 时翻转四元数符号） | 190° 的误差应当理解成「往回转 170°」而不是「继续转 190°」；不翻转时 `to_axis_angle` 会给出去 190° 的解，关节绕远路 | 保持 `to_axis_angle` 原样输出：行为仍是「能到位」，但慢、反直觉，且误差长度会超过 180°（单元测试里「永不超半圈」那条不变式就是钉这个） |
| 速度上限用 **`clamp_length`**，不用逐分量夹紧 | 逐分量夹紧会**改变方向** —— 关节就会朝错误的方向扭 | 不夹：180° 误差下 `0.2/dt·e` 能到 37.7 rad/s，一步甩过去不物理 |
| 把「不许睡」的判据从 `has_active_motor()` 换成新的 `drives_bodies()` | 睡眠体在求解器里**完全冻结**（`BodyView::from_body` 把 `inv_m` / `inv_I` 清零）→ 睡着就会把修正停在半路，且期间被推歪也不会再被拉回来 | 直接改 `has_active_motor()` 的返回值：那会偷换「有马达」这个公开语义；或按「误差大于阈值才醒」：要为每个关节每步回报误差，复杂度大得多 |
| 参考姿态用**欧拉角（度）**，不用四元数 | 与 `rotation_deg` / `motor_target_deg` / `cone_limit.half_angle_deg` 同一套（格式里的角度都是度；唯一例外是铰链 `limits` 与马达速度，且有 `unit_ambiguity` 兜着）；手写场景时 `90` 比 `0.707` 好写 | 四元数要额外解释归一化与 `-q == q`，而 agent 写错一点就得到另一个旋转；欧拉角的万向锁在这里无所谓（它只描述一个固定参考姿态，不做插值） |
| 校验**复用** `out_of_range` / `ineffective`，不加新码 | 码表故意窄：非有限是 `out_of_range`，「写了不生效」是 `ineffective`（`frame_*` 写在非 `fixed` 上、或两端都写 0） | 新码要动 `docs/error_codes.md` 的双向漂移守卫与「每个码一条真实场景」的覆盖断言；而这两类问题都不是新性质 |
| 编辑器**不加面板**，只加「不许分叉」的结构守卫 | 编辑器本来就不建模关节（`scene_to_json` 只按名字保留 `joints`；预览走 `runner::build_world`），加面板是另一件事（要 UI + 撤销/重做 + 选择集） | 造一个只读关节面板收益低；真正会出事的是「日后有人在编辑器里再抄一份关节装配」，所以守卫盯的是这个 |

### 12.3 不变量（钉住的、可复核的）

1. **不写 = 老行为，而且是逐位的**：`frame_a`/`frame_b` 都不写（或两端都写 0）时，求解器走
   与改动前**同一条表达式**（`-wrel`，代码里是显式分支，不是"加个零"）；e2e 断言
   「写全 0」与「不写」跑 120 步后每个刚体的位置/朝向按 `f32::to_bits()` 逐位相同；
   物理 crate 的 `digest_tests` 也全绿。
2. **参考姿态的语义**：收敛后 `q_a⊗frame_a == q_b⊗frame_b`（`fixed_frame_error_deg < 2°`），
   而**不是**「两端刚体同朝向」：`frame_a=[0,90,0]` 的门板最后停在绕 Y 90°（出生时 0°），
   只写 `frame_b=[0,90,0]` 则停在 -90°。
3. **收敛有界**：1 秒内 < 5°、3 秒内 < 2°、不过冲（误差不超过初始值）、点约束不受影响
   （锚点位移 < 0.05 米）。数值由 `crates/quench3d-physics/tests/fixed_reference_frame.rs` 钉住。
4. **两端都是动态体时也成立**：冲量同时作用在两侧（A 也跟着转），不是「只扭 B」。
5. **持续性**：参考姿态是每步都在修的约束 —— 被 3 rad/s 推歪之后能自己扭回来；两端在 10 秒内
   不会睡着（睡眠会把修正冻住），这条由 CLI 集成测试单独钉住。
6. **一份换算**：`runner::joint_frame_quat`（`pub` 纯函数）是度 → 四元数的唯一入口，
   CLI 装配、格式校验、编辑器守卫都调它；编辑器预览本来就走 `runner::build_world`。
7. **agent 自描述三处同步**：`scene_grammar()["joint"]` ↔ `$defs.joint.properties` ↔ 系统提示词；
   本轮新增 `joint_field_table_covers_every_joint_field`（人读表 ↔ `JointDoc` 字段集**逐项**对账），
   并补上人读表里**一直缺的** `motor_max_impulse`。

### 12.4 边界（明确没做的）

- **关节可视化仍然没有**（轴 / 锚点 / 参考姿态的 gizmo）：编辑器里还是只能靠数字调；
  渲染侧不在本轮围栏内。
- **编辑器没有关节编辑面板**：本轮编辑器侧只加了 3 条**纯 CPU** 的结构守卫
  （`crates/quench3d-editor/tests/joint_frames.rs`）—— 它们证明「只有一份关节装配」，
  **不**证明「预览画面正确」（那要真窗口 + 真显卡；编辑器已有的往返测试在没有 GPU 时仍然 skip）。
- **参考姿态只作用于 `fixed`**：`ball` 的 `cone_limit.axis_a/axis_b` 早已在求解器里生效（第 23 轮），
  铰链/滑块用 `axis_a`/`axis_b` —— 这三套字段互不相同，别混（写错关节类型会得到 `ineffective`）。
- **报告里没有「参考姿态残余误差」字段**：`simulate` 不会告诉你还差多少度；要查只能自己按
  `states` 算（或跑引擎那组测试）。这是明确的欠账。
- **没有专门覆盖 180° 这个奇点**：`to_axis_angle` 在 `w≈0` 时轴的方向由浮点符号决定
  （两条路都能到，方向可能不同）。单元测试扫了 10°/90°/179°/181°/190°/270°/350°，
  但没有「正好 180°」的专项用例。
- **无 GPU 环境下的一致性只能靠数值断言逼近**：物理本来就是纯 CPU 的同一份代码，
  所以「测试绿 = 解算对」这一步可信；但「渲染出来的画面与物理一致」这类问题**本轮没验证**
  （不需要显卡的部分我都跑了，需要显卡的部分我没跑）。

## 13. 第 58 轮（下半场）：把"裸建 wgpu 上下文"的测试一次清干净 —— 判据 / 不变量 / 边界

### 13.1 一句话

仓库里**全部 10 个**"真的创建 wgpu 上下文"的测试二进制现在都走显式 opt-in
（`QUENCH3D_GPU_TESTS=1` 才执行，否则整条测试体不执行 + 打印可读原因），
并有一条**不需要显卡**的守卫在每条 CI 腿上扫描 `crates/*/tests/*.rs`：
凡是真的建上下文的地方都必须落在某个 `with_gpu_tests(...)` 的区间里 —— 包括**将来新增**的文件。
**没有删掉或放宽任何断言**。

### 13.2 判据（为什么是这套做法）

| # | 判据 | 为什么不选别的 | 结论 |
|---|---|---|---|
| 1 | 逐个文件把建上下文那一步包进 `with_gpu_tests` 的闭包 | 体内 `if !enabled { return }` 会让"跳过"与"断言没跑"再次混在一起；`cfg!(windows)` 会连本地有显卡的 Windows 开发者也一起关掉 | 已落地（8 个文件） |
| 2 | 加一条**不需要显卡**的守卫，而不是只修这一次 | 只修当下 8 个 = 下次新写的测试照样裸奔（这正是本次欠账的成因）；守卫必须能在没有 GPU 的 windows 腿上跑，所以它只能读文件、不能建上下文 | 已落地（`post_pipeline.rs` 末尾） |
| 3 | 守卫必须**自动**覆盖新文件 | 白名单式（"检查这 8 个文件"）会在第 9 个文件出现时静默失效 | 扫描 `crates/*/tests/*.rs` 全目录 |
| 4 | 守卫必须**剥注释/字符串**再判定 | 真实反例：`crates/quench3d-editor/tests/joint_frames.rs:3` 的文档注释里就写着 `GpuContext::headless`；不剥就是第一次运行即误报，而会乱叫的守卫最终会被加白名单 | 已落地（`code_only`），并用变异 ③ 证明它载重 |
| 5 | 守卫要有"非空转"断言 | 词法守卫最危险的失效模式是**静默空转**（wgpu 入口改名后一处都扫不到 → 永远绿） | `sources.len() >= 5` + `gated > 0` |
| 6 | CI 要能把"真跑"与"跳过"分开 | 整树 `cargo test` 会吞掉通过测试的输出：没有显卡的腿上"跳过"与"没跑"日志一模一样（上半场就因此误判过一次） | `ci.yml` 增加只跑这 10 个二进制并加 `-- --nocapture` 的步骤 |

### 13.3 不变量

* **建上下文的调用永远不在闭包外面**：10 个二进制的每一处入口都在 `with_gpu_tests(...)`
  的括号区间内；新增文件若不这么做，守卫红。
* **跳过 ≠ 弱化**：断言文本、数量、阈值一个都没改；没设开关时是"整条体不执行 + 打印原因"，
  设了之后跑的是**同一份**断言（本机两态实测：关 = 10 个二进制全 `0.00s`，
  开 = 全真跑且绿，`post_pipeline` 4.09s、`scene_roundtrip` 2.29s…）。
* **守卫本身不需要显卡**：它只读文件，所以 windows 腿也跑它
  （这一点已经在上半场的 windows 腿日志里见到过同类守卫通过）。
* **门禁是整树的**：共享工作树时，一个写者"未提交且未格式化"的文件会让另一个写者的
  `GATE=0` 变红（第 1 段 fmt）。所以同一棵工作树同一时刻只允许一个写者跑门禁 —— 见
  `docs/overnight_report.md` §54.5。

### 13.4 边界（诚实清单）

* **本机没有"无 GPU 的 Windows"**：`0xc0000005` 无法在本地复现，"windows 腿不再崩"
  只能由 CI 证明。本地能证明的是"闭包不执行"与"守卫不需要显卡"。
* **守卫是词法的**：给 `GpuContext` 起别名（`use ... as G`）再建上下文会漏；把建上下文
  藏进间接包装也会漏。要更强需要 `syn` 解析（加依赖，围栏不允许）。它拦的是"写法常规的
  裸奔"，而那正是当下 100% 的实际情况。
* **只扫 `crates/*/tests/*.rs`**：`src/**` 里 `#[cfg(test)]` 的单元测试不在扫描范围
  （目前它们不建上下文）。
* **ubuntu 腿上的这些断言仍然是空的**（§11.4）：本轮没装软件光栅，`0.00s` + 内层返回；
  CI 里真正的断言覆盖来自 macOS 腿（Metal）。
* **windows 腿的这部分覆盖永久是 0**：开关关掉 = 不执行。这不是本轮引入的损失
  （本来也建不出上下文），但不要让"CI 全绿"被读成"windows 上渲染路径测过了"。

### 13.5 上线实测（CI）

`ab728a6`（run `38961249990`，`attempt = 1`）五腿全绿：

* **macOS 腿开关 `1`：10 个二进制全部真跑**（0.19s ~ 0.88s，Metal），
  `backend_fallback` 7p/0.19s、`post_pipeline` 8p/0.88s、`scene_roundtrip` 10p/0.29s …；
* **ubuntu 腿开关 `1` 但 runner 没有适配器**：全部 0.00s + 46 条内层
  "没有可用的 GPU"原因 → 这些断言在 ubuntu 上**仍然是空的**（本轮没装软件光栅）；
* **windows 腿开关 `0`**：全部 0.00s、`7/2/2/2/8/1/8/7/5/10 passed; 0 failed`、
  **首跑绿**、47 条可读的"未打开开关"原因；
  `process didn't exit successfully` 与 `exit code: 0xc0000005` 各 **0** 次。

`70829d2` 那一轮在 macOS 红过一次，红点是**我新写的 shell 行**
（macOS 的 bash 3.2 把紧跟 `$VAR` 的全角 `（` 算进变量名 → `set -u` 报 unbound），
不是任何断言；同一轮 windows/ubuntu 都是绿的。修法与完整证据见
`docs/overnight_report.md` §54.8。**这条也说明：跨腿矩阵要留，一行只在一个平台上错的脚本
单腿 CI 抓不到。**

## 14. 第 60 轮：跨文件组合 `include`（gap #13 最后一件大件）—— 取舍 / 不变量 / 边界

### 14.1 这一轮修的是什么（一句话）

场景格式第一次能**拆成多个文件**：`include: ["parts/wall.json"]` 在装载时把片段合并成
一份自包含文档（片段还能再 `include`），于是「一份材质/模板/关节组被多个场景复用」
不必再复制粘贴 JSON，agent 生成大场景时也不用把同一段 token 反复吐出来。

### 14.2 取舍（为什么这样做 / 为什么不那样做）

| 选择 | 为什么 | 为什么不选另一条 |
|---|---|---|
| **装载期**合并成一份文档，而不是运行期"两份世界拼起来" | 合并之后所有既有路径（校验、`build_world`、探针、报告、编辑器导出）看到的都是一份普通文档，**没有第二套语义**；引擎侧一行都不用改 | 运行期拼接要在物理世界、渲染器、报告、探针各写一遍"来自哪份文件"，一处漏写就是静默丢内容 |
| 片段只贡献**可合并的段**；具名表重名 = `duplicate_name` 错误 | 静默覆盖是最坏的形态（引用会挂到错的东西上，报告里还看不出）；错误信息给出**两份文件**的路径 | 后写覆盖先写：写场景的人看不到自己覆盖了什么；"重名给警告、后者赢"同样静默 |
| 根独有的段（`ground`/`camera`/`lighting`/`simulate`/`render`/`gravity`/`physics`/`name`/`assets`）写在片段里 = `invalid_value` 错误 | 这些段只有一个实例，"谁赢"没有正确答案；宁可当场报错，也不做含糊的合并 | 让根的赢：片段作者以为自己的光照生效了（跑起来却不生效），与"写了不生效"是同一类坑 |
| 数组按「**本文件自己的内容在前**，被包含的在后」追加 | 重名冲突永远报在"后并进来的那一份"（片段）上，读错误的人先看到 `include[0].materials.木` 而不是怀疑根文件；与自己写在前面的直觉一致 | 片段在前（我第一版就是）：同一个冲突会报在根文件的 `$.materials.木` 上并说"已在 parts/… 定义"——指向的却是根文件那一行，误导 |
| 片段的**资源**相对路径按**片段自己的目录**重定基一次，`include` 路径本身不重定基 | 片段搬到别的目录时贴图跟着走；而 `include` 的路径语义是"相对于写这一行的文件"，重定基反而会指错 | 全部按根目录解析：片段不能自带资源目录（`parts/wall.json` + `parts/tex/…` 会找不到）；两套解析规则并存则是漂移之源 |
| 同一个文件在一次解析里**只并入一次**（菱形包含），并且给一条 `ineffective` 警告 | 两次并入同一份片段必然重名报错（对用户是噪音），但静默跳过又违反"不静默"原则 → 跳过 + 说出来 | 并入两次：重名错误把"合法写法"变成"不可用"；静默跳过：用户以为摆了两份 |
| 编辑器存盘时**内联**片段并清掉 `include`（而不是保留 `include` 或拒绝存盘） | 编辑器编辑的是合并后的世界，`objects` 会按引擎重建：保留 `include` → 下次载入重复并入；只清不合并 → 片段里的 `models`/`joints`/材质被丢掉。内联是唯一两条错法都不犯的解 | 拒绝存盘会毁掉「改一改再存」这条链；"另存为片段"是更大的功能（见 14.4 欠账） |

### 14.3 不变量与守着它们的测试

| 不变量 | 判据（都在 CI 上、都不需要显卡） |
|---|---|
| **不写 `include` = 行为逐字节不变** | `scene_doc.rs::include_tests::scenes_without_include_are_byte_identical`（`to_json_pretty` 字符串相等）；`cli_e2e.rs::scenes_without_include_are_unaffected`（退出码/诊断全同） |
| **合并 == 手写一份单文件** | `cli_e2e.rs::include_split_scene_simulates_bit_identically_to_a_single_file`：多文件版与单文件版的 `states` 逐物体比 `position`/`rotation`（f32 位相同），外加 `lowest_y`/`max_speed`/`bodies` |
| **片段内容不会在存盘时丢掉** | `quench3d-editor/tests/include_export.rs`：`export_base_doc` 合并后材质/物体/关节都在、`include` 为空、有一句"内联了谁"的说明；坏片段 → `Err`（拒绝写出一份丢内容的文件） |
| **编辑器导出不会绕过合并** | 同文件的源码结构守卫：`scene_to_json` 必须走 `export_base_doc`，材质继承不许查未合并的底稿（两条都由变异红-绿验证：把导出改回 `loaded.clone()` → 红；材质继承改回 `doc.base_doc` → 红） |
| **片段的资源路径按片段目录解析** | 单元测试断言重定基后的**路径字符串**正好是 `parts/tex/brick.png`；e2e 里贴图**只**放在片段目录（空操作重定基 → `asset_not_found`，变异验证过） |
| **每个诊断都有稳定码** | `diagnostic_codes_are_stable_and_specific` 的覆盖集里加了 `include_cycle` 的真实触发（两个互相包含的文件），反向断言"覆盖集 == 码表 − `invalid`"（18 个码） |

**变异验证**（都是"改坏 → 红 → 改回 → 绿"，实测）：① 重定基改空操作 → 单元 + e2e 各红一条；
② 重名静默覆盖 → 单元红；③ 让 `merge_own` 搬运 `include`（模拟"将来顺手加了它"）→ 3 条红；
④ 编辑器绕过 `export_base_doc` / 材质继承查未合并底稿 → 各红一条。

### 14.4 边界与欠账（这一轮**没有**做的）

- **`include` 是装载期的组合，不是运行期链接**：合并后引擎/报告里没有"片段"这个概念，
  也没有 `variant`（同一份片段按场景覆盖字段）、没有条件包含、没有"覆盖根段"的写法。
- **只有"资源路径"会重定基**，`include` 自己写的路径不重定基（它本来就是相对写这一行的文件的）。
- **编辑器只能"装 + 存（内联）"**：没有"另存为片段"、没有片段面板；存盘会把片段内容复制进根文件
  （片段文件本身不动）——文件会变长、之后的修改要在根文件里做，这一点会写进存盘报告。
- **纯内存入口 `from_json`（浏览器/wasm）没有场景文件目录**：那里的 `include` 相对路径按工作目录
  解析，浏览器里基本会失败。**这条没在真浏览器里验证过**（本轮只跑了 `check_web` 的构建/导入检查）。
- **没有资源去重报告**：两份片段引同一张 4K 贴图，装载时会各自解析、渲染器也各上传一次；
  "合并后一共用了哪些资源"没有报告（gap #11 的尾巴）。
- **没有"片段里 include 深度/文件数"的规模报告**：只有 16 层上限与 `limit_exceeded`。
- **跨文件重名的"物体"报在合并后的下标上**：`materials`/`templates`/`cameras` 重名会点名两份文件
  （`include[0].materials.木`），但 `objects` 是数组、合并后没有来源信息，重名物体的诊断是
  `objects[3].name`（有名字、没有文件）—— 见 `docs/error_codes.md` 的 `duplicate_name`。
  想精确定位只能自己按 `include` 顺序数（这是明确的欠账）。
- **`include` 与 `assets.search_paths` 的关系只覆盖到"候选顺序"**：片段自己的
  `assets.search_paths` 是**错误**（根独有的段），所以片段不能声明自己的搜索路径 ——
  它靠"资源路径按自己目录重定基"覆盖同一个需求。

## 15. 第 61 轮：Wayland 上关窗口即 SIGSEGV —— 一个只有真跑 GUI 才看得见的析构顺序 bug

### 15.1 症状（用户实测，本机真 NVIDIA 卡 + Wayland）

`./target/debug/quench3d-editor` 启动一切正常（Vulkan 初始化成功、第一帧渲染成功），
**用户主动关闭窗口**时进程 `SIGSEGV` 死掉：`segmentation fault (core dumped)`，退出码 **139**。
**与 GPU、场景内容无关** —— 在 llvmpipe（软件 Vulkan）下同样复现。

这条崩溃最讨厌的地方是：它落在**"正常退出"**这条路上。所以"跑起来没事、第一帧也对"
这一整套既有验证完全骗过了它 —— 退出路径以前从未被真正走过。

### 15.2 回溯（gdb，本机复现）

```text
Thread 20 "smithay-clipboa" received signal SIGSEGV, Segmentation fault.
#0  libwayland-client.so.0
#1  wl_proxy_destroy
#2  wayland_backend::sys::client_impl::InnerBackend::destroy_object_inner  (src/sys/client_impl/mod.rs:561)
#3  wayland_backend::sys::client_impl::InnerBackend::send_request           (src/sys/client_impl/mod.rs:774)
#4  wayland_client::conn::Connection::send_request<ZwpPrimarySelectionDeviceManagerV1>
#5  ZwpPrimarySelectionDeviceManagerV1::destroy
#6  core::ptr::drop_glue<smithay_client_toolkit::primary_selection::PrimarySelectionManagerState>
#8  core::ptr::drop_glue<smithay_clipboard::state::State>
#9  smithay_clipboard::worker::worker_impl                                  (src/worker.rs:101)
```

### 15.3 根因：不是剪贴板坏了，是**析构顺序**错了

每一环都能在源码里逐行核对，没有一处是猜的：

| # | 事实 | 出处 |
|---|---|---|
| 1 | `EventLoop::run_app(self, app)` **按值吃掉** EventLoop（不是 `&self`） | `winit-0.30.13/src/event_loop.rs:264` |
| 2 | 连接是 `EventLoop` 的字段 → winit 自己的 Wayland 连接在 `run_app` **内部**就析构了 | `platform_impl/linux/wayland/event_loop/mod.rs:69` |
| 3 | 调用点 `event_loop.run_app(&mut EditorApp::new(config))?` 里的 `EditorApp` 是**临时值** → 要到整条语句结束才析构 → **晚于**连接 | `crates/quench3d-editor/src/app.rs` 的 `run()` |
| 4 | `egui-winit` 只在 `RawDisplayHandle::Wayland` 时才建 `smithay-clipboard`，且用 `Backend::from_foreign_display(display)` 包住 winit 的裸 `wl_display*`（**不持有所有权、也不负责释放**） | `egui-winit-0.37.0/src/clipboard.rs`（`init_smithay_clipboard`）、`smithay-clipboard-0.7.3/src/lib.rs`（`Clipboard::new`） |
| 5 | worker 线程析构 `PrimarySelectionManagerState` 时发 `destroy` 请求，并在 `wl_proxy_destroy` 里碰 `proxy->display` | `smithay-client-toolkit` + `wayland-backend` 的 `destroy_object_inner` |

1+3 让"剪贴板析构"跑在"`wl_display` 已被 winit 释放"**之后**；4+5 让这一步真的去碰那个
已经没了的 `wl_display` → 释放后使用。**崩溃点是 `worker.rs:101`（`State` 的析构），
不是任何一行业务代码**，这也是为什么"看代码找 bug"很难找到它 —— 错的不是代码，是顺序。

### 15.4 取舍

| 选择 | 为什么 | 为什么不选另一条 |
|---|---|---|
| 在 `ApplicationHandler::exiting()` 里 `self.inner.take()`，把窗口/GPU/剪贴板**按序释放** | `exiting` 是 winit **唯一**保证"事件循环即将退出、但 Wayland 连接仍然有效"时回调的地方：后端在 `pump_events` 里派发 `Event::LoopExiting`（`platform_impl/linux/wayland/event_loop/mod.rs:231`），此刻 `self.connection` 仍在作用域内；其后紧跟一次 `roundtrip()`，winit 自己的注释就写着「如果应用刚丢掉窗口，得确保最后的请求发给了合成器」—— 在 `exiting` 里丢窗口**正是它设计的用法** | 不释放（什么都不做）：就是本 bug。放在 `run()` 返回之后释放：那时 EventLoop 已被吃掉、连接已死，做不到 |
| 释放**只在 Wayland 触发**，判据 = `RawDisplayHandle::Wayland` | 判据与 `egui-winit` 决定"要不要建 `smithay-clipboard`"的判据**是同一个**，所以"要释放"与"有这个对象"严格等价；X11 下没有这个对象，也就没有这个顺序问题 | 无条件释放：实测会把 winit 0.30.13 X11 **既有的** `BadWindow` panic 从"多数 101"变成"必然 134(SIGABRT)"（基线 7 次 = 101×6/134×1；无条件释放 3 次 = 134×3）。既然 X11 上修不到任何东西，就不该去动它 |
| 用**显示句柄**而不是环境变量判断后端 | `WAYLAND_DISPLAY`/`WAYLAND_SOCKET` 只反映 winit 的**选路**，嵌套合成器等场景下未必与实际句柄一致；显示句柄才是权威 | 读环境变量：多一条"判据与实际后端不一致"的失败路径，且与 `egui-winit` 的判据不同源 |
| **不升级依赖**（否决 brief 的优先级 (a)） | 本仓库永远 `--offline`。离线能用的**只有** cargo 缓存里已有的 `.crate`：`smithay-clipboard` 只有 **0.7.3**（就是 `Cargo.lock` 里这个），`wayland-client` 只有 0.31.15、`wayland-backend` 只有 0.3.17、`smithay-client-toolkit` 只有 0.19.2/0.20.0。而且本地 registry **索引**里 `smithay-clipboard` 的最大版本就是 0.7.3（`wayland-client` 0.31.15 / `wayland-backend` 0.3.17 同样是索引里的最大版本）—— **索引里根本不存在更新的版本可升**。（本机无法联网复核上游最新版，`web_search` 端点不可用；但"缓存里没有"这一条已经足够判死 (a)。） | 改 `Cargo.toml`/`Cargo.lock` 指向一个缓存里不存在的版本：`--offline` 下直接构建失败 |
| 不用 `std::process::exit(0)` 跳过析构（否决 brief 的 (c)） | winit 给了一个**正确的钩子**，就该用它。`process::exit` 是**绕过**这个顺序问题而不是修好它：它会让 GPU/Vulkan 一起失去正常拆解路径（连 `wgpu` 的 `Drop` 都不跑）；而且 `std::process::exit` 在 **wasm32 上不存在**，要多一层 `#[cfg(not(target_arch = "wasm32"))]` 门控（本 crate 的 `web_client` 就是 wasm 腿，门禁会编它） | 跳过析构："进程马上就结束了"听着有理，但它把"顺序错了"这件事**藏起来**了；下次再加一个持有裸 `wl_display*` 的依赖，同样的崩溃会以新的形状回来 |
| **不捕获 / 不忽略 SIGSEGV** | 那是掩盖手段：真出了别的越界（渲染、物理、驱动）就再也看不见了，而"退出时崩"恰恰是最容易用 `signal(SIGSEGV, SIG_IGN)` 糊过去、也最不该糊的一类。本 bug 有明确的、可验证的正确修法，没有任何理由去掩盖 | 装 `SIGSEGV` handler：会把"内存错误"变成"随机行为"；而且 SIGSEGV 之后进程状态已不可信，继续跑等于掩盖 bug |

### 15.5 不变量与守着它们的测试

| 不变量 | 判据 |
|---|---|
| **关窗口 → 退出码 0，且没有 SIGSEGV** | 真跑 GUI（本机 Wayland）：同一配方修复前 139 / 修复后 0（复跑 4/4 全 0）；gdb 下修复后 `[Inferior 1 (process N) exited normally]`、`SIGSEGV` 出现 **0** 次 |
| **自动退出钩子默认关闭** | `crates/quench3d-editor/tests/exit_hook.rs`（6 条，**不需要显卡**）：不设 / 空白 / `0` / 非数字 → 一律"不自动退出"；只有正整数才是毫秒数；解析是纯函数、可重复。另有真跑证据：不设 / `=0` / `=abc` 三种情况都跑满 `timeout`（退出码 124）= 确实没有自动退出 |
| **钩子走的是"关窗口"那条路，不是另一条** | 钩子投的是**真的 `WindowEvent::CloseRequested`**（`handle_window_event` 里与用户点关闭按钮**同一个 match 分支**），不是直接调 `event_loop.exit()` |
| **X11 的既有行为没被本次改动扰动** | 真关窗口（X11/XWayland，`xdotool windowclose` = 真的 `WM_DELETE_WINDOW`）：基线 7 次 = 101×6/134×1；加门控后 4 次 = **101×4**，且 `exiting` 的释放日志一次都没出现（门控确实为假） |
| **变异红-绿** | 钩子解析两条变异：`0` 不再等于关闭 → `zero_means_off` **红**；默认不再关闭（`raw?` → `unwrap_or("3000")`）→ `unset_or_blank_means_off` **红**；还原 → **绿**（6 passed）。修复本身也做过变异：把 `exiting` 体摘成空实现 → 同一配方 **139 + SIGSEGV**；还原 → **0** |

### 15.6 边界（没做的、没验证的）

- **CI 覆盖不到这条路径**：所有 CI runner 都没有显示器/合成器，`cargo test` 里也拿不到
  `WindowEvent::CloseRequested`。CI 能证的只有"wasm32 腿编得过、原生腿编得过、
  不引入 clippy/测试回归"。**"关窗口 → 退出码 0"这件事只有真跑 GUI 能证** ——
  这也是为什么要留下那个默认关闭的钩子（配方见 `docs/overnight_report.md` §56）。
- **X11/XWayland 的关窗口 panic 没修，也没查根因**：`winit-0.30.13` X11 的
  `BadWindow` panic（`event_processor.rs:1324` "Failed to unfocus input context"，
  偶尔先在 `util/geometry.rs:188` 的 `TranslateCoordinates` 上炸），退出码 101 或 134。
  这是**上游 winit 的缺陷**，与本次修复无关（基线就有），属于**明确的欠账**。
  本次只做到"不去扰动它"，没有做到"修好它"。
- **没验 X11 下"用户的真实点关闭按钮"之外的形式**：X11 侧我用的是
  `xdotool windowclose`（发真的 `WM_DELETE_WINDOW`，与点关闭按钮同一条 X 协议），
  没有在真 X11 会话里用鼠标手点。Wayland 侧用的是钩子投递的真 `CloseRequested`
  （合成器发的就是这个事件），**没有**用鼠标手点 —— 手点在自动化里做不到。
- **`smithay-clipboard` 的"裸 `wl_display*` 生命周期契约"仍然靠自觉**：
  本次只是把析构挪到连接还活着的时候。`smithay-clipboard 0.7.3` 的 `Clipboard::new`
  至今仍是 `unsafe fn` + "display 必须比 Clipboard 活得久"的调用方义务；
  上游若哪天改了这个契约，这里需要跟着复核。
- **没做**：给退出路径加一条"析构顺序"的通用守卫（比如断言 `exiting` 一定被调用过）。
  现在的守卫是"真跑 + 日志行"，对自动化不够强 —— 但比"假装 CI 能覆盖"要好。


## 15. 第 61 轮：圆锥形状（gap #18 的第一件）—— 取舍 / 不变量 / 边界

### 15.1 一句话

格式多了一种形状 `shape.type = "cone"`（底面半径 `radius`、半高 `half_height`），
**物理、渲染、校验、CLI、编辑器五处一次接齐**，且与既有形状走同一套路径
（GJK/EPA、半空间接触、射线、单位网格 + 缩放），不是"格式认了但引擎里没有"的半成品。

### 15.2 取舍表

| 选择 | 备选 | 为什么选它 |
| --- | --- | --- |
| 用**支持函数**接进已有的 GJK/EPA | 给每一对形状写专用的锥-盒/锥-球解析路径 | 支持函数是闭式的（顶点与底面圆周二选一），写一次就让"锥 vs 任何凸体"都能算；专用路径要写 $O(n)$ 对，且每加一种形状就要再补一轮 |
| 本地朝向与圆柱/胶囊**同族**（轴沿 +Y，底面在 `-h`、顶点在 `+h`） | 把顶点放原点（像很多渲染库那样） | 场景里的 `position` 指的是**形状原点**：如果顶点在原点，同一份 `position` 换成圆锥就会整根扎进地里；同族朝向让"换成圆锥试试"是可预期的 |
| **如实报质心偏移** `(0, -h/2, 0)` | 假装质心在原点（按原点算惯量与重力） | 实心圆锥的重心在底面以上 1/4 高处，离几何中心 `h/2`。假装在原点 → 重力作用点错、静置时会自己倒、惯量也不对。引擎本来就用 `MassProperties::center` 表达这件事（凸包也这么报），所以代价是 0 |
| 半空间接触给**顶点 + 底圈 8 点** | 只给顶点（最省） | 只给顶点的话立着的圆锥只有一个接触点，会像陀螺一样抖/滑；底圈 8 点与圆柱同套做法，静置稳定 |
| 射线侧面解**二次方程**、底面单独当圆盘 | 把圆锥三角化成凸包逐面求交 | 二次解是闭式的、精确、无三角化误差；底面单独处理是因为它是平面（顶点处退化，二次解会丢） |
| 单位网格**中心在原点**（`assets.unit_cone`） | 直接用 `MeshData::cone`（底面在 y=0） | 场景图元一律中心在原点，`parts[].rotation` 才是绕几何中心转；否则"转一下圆锥"会绕脚底转 |
| 编辑器工具栏加一个按钮、复用同一个单位网格 | 只在格式层支持、编辑器不管 | "格式里有但界面里加不出来"等于没做（本仓库对"可达性"的要求） |

### 15.3 不变量与守卫测试

* **网格 == 形状**：`crates/quench3d/tests/unit_primitives.rs` 断言
  「单位网格 AABB == 物理 `local_aabb`」——圆锥**以及**盒/球/圆柱/胶囊一起对（后四个以前
  没人盯着）。变异：把 `unit_cone_mesh` 的平移从 `-0.5` 改成 `0` → **红**。
* **物理闭式解**：`shape::tests::cone_mass_properties_match_closed_form`（体积/质量/两个惯量/
  质心偏移/`is_finite`）、`cone_support_and_bounds`（顶点 vs 底圈、AABB、包围球、膨胀只动半径）。
  变异：支持函数永远返回顶点 → **红**。
* **真的会落下来**：`sim_tests::cone_base_down_rests_on_ground`（原点停在 `y=h`、速度趋 0、
  局部 Y 仍朝上、`com_world` 在原点下方 `h/2`）、`cone_apex_down_tips_over`
  （顶点朝下必然翻倒，且用支持函数算出的"世界最低点"贴着地面 —— 不穿地也不浮空）。
  变异：半空间接触只剩顶点、没有底圈 → **红**。
* **射线三段**：`query::tests::raycast_hits_cone_lateral_base_and_apex`（侧面 4.75 / 底面 4.5 /
  顶点 4.5，全是手算值；法线方向也断言）。
* **映射表**：`runner::tests::every_shape_kind_maps_to_physics_and_render_consistently`
  —— 遍历**全部** `ShapeKind`，逐形状断物理形状，再断「单位图元尺寸 × `render_scale` ==
  物理 AABB 尺寸」。变异：圆锥的渲染缩放丢掉半高 → **红**。
* **端到端**：`cli_e2e::cone_shape_simulates_validates_and_reports`（立着的停在 `y≈0.5`、
  顶点朝下的翻倒、静态锥不动、轨迹报告里 `shape == "cone"`、半径 0 与半高 -1 各给
  `out_of_range` + 精确 JSON 路径）。变异：校验里给圆锥留个空分支 → **红**。
* **格式枚举**：把 `ShapeKind::Cone` 删掉**编译就过不去**（映射表与 e2e 都引用它），
  不会出现"悄悄不支持"。
* **编辑器**：`doc::tests` 两条（`ALL` 里必须有 5 个图元且每个都有名字/正体积/合法 AABB；
  圆锥的标签与物理尺寸）。变异：工具栏去掉圆锥 → **红**。

### 15.4 边界与欠账

* **单点流形**：锥-锥、锥-盒走 GJK+EPA 的**单点**接触，静置稳定性弱于盒-盒（与圆柱、凸包同一档）；
  堆圆锥会互相滑。要稳定堆叠得给这些组合加多面裁剪路径（未做）。
* 没有**锥台/截头圆锥**、没有"倒角半径"（圆角盒）。
* `inflated`（接触余量）对圆锥只胀底面半径，顶点处不是严格的 Minkowski 膨胀 —— 与圆柱同族近似，
  余量量级远小于形状尺寸时才无害。
* **gap #18 还剩两件**：圆角盒（`rounded_box`）与高度场（`heightfield`）。高度场可以先用
  "静态三角网"的方式落地（格式紧凑、物理复用 `TriMesh`），但那会继承 `TriMesh` 的两条限制：
  只能静态、内存与三角形数同级（没有专用网格加速结构）。

## 16. 第 61 轮：高度场（gap #18 的第二件）—— 取舍 / 不变量 / 边界

### 16.1 一句话

场景里可以直接写地形了：`shape.type = "heightfield"`（`heights` 行主序 + `cell_size`）。
**物理与渲染共用同一份几何**（`MeshData::heightfield`），只能静态，校验把坏输入挡在门外，
编辑器存盘不会把地形丢掉 —— 但窄相是逐三角形的，所以这是"能用地形"而不是"能用地形做大地图"。

### 16.2 取舍表

| 选择 | 备选 | 为什么选它 |
| --- | --- | --- |
| 展开成已有的 `Shape::TriMesh` | 新写一个 `Shape::HeightField`（带自己的网格加速结构） | TriMesh 的窄相/射线/复合形状/接触全套已经跑通，展开一次就能用；新形状意味着新窄相 + 新射线 + 新 AABB + 新质量路径，而高度场**天然只能用静态**，收益只在性能上。性能留作下一步（见 16.4） |
| 高度场是**格式层**概念（CLI 展开），引擎不新增形状 | 引擎层新增形状 | 保证"引擎 API 不变"：老代码、老 wasm、老测试都不受影响；代价是引擎里看到的是 TriMesh（`--json` 报告的 `shape` 名由格式层给出，所以轨迹报告仍是 `heightfield`） |
| 一份几何（`MeshData::heightfield`）同时供物理与渲染 | 物理/渲染各写一遍展开 | 各写一遍就会出现"看得见、踩不到"（网格与碰撞面错开半个格子）。第 61 轮的 `unit_primitives`/映射表测试已经在盯同类问题 |
| 行主序 `heights[行][列]`，行沿 Z、列沿 X | 列主序 / 平铺一维数组 + `cols` | 二维数组在 JSON 里是**看得见的形状**（缩进就是地形），agent/人都不容易写反；平铺数组省不了多少 token 却多一个 `cols` 要对上 |
| 网格以**物体原点**为中心、高度相对原点 | 以左下角为原点 | 与其它形状一致（`position` 就是形状原点），于是"把地形抬到 y=2"是写 `position`；左下角约定会让地形在原点处"只有一个角" |
| UV 覆盖整块地形 `[0,1]²` | UV 用世界坐标（每米一次平铺） | 与其它图元一致（`uv_scale` = **网格 UV 空间**里的平铺次数），文档里 `uv_scale` 的说明不用改；要"每米一次"就写 `uv_scale = 边长` |
| 平法线 | 平滑法线 | 地形的面片感更清楚，且避免"顶点法线取平均"在大落差处糊掉；要平滑可以后续加开关 |
| **只能 `static`**（校验报错） | 允许 dynamic，引擎静默降级成静态 | 引擎 `BodyBuilder::build` 会把含静态专用形状的刚体强制成静态 —— 静默降级是"写对了不生效"的典型（同族的平面已经有这个坑，见 gap_analysis §3.3）。宁可报错 |
| 编辑器存盘**按名字从底稿恢复** | 存盘时把它报成 skipped 丢掉 | "编辑器没有形状编辑面板"⇒"形状没被改过"成立，恢复是安全的；丢整块地形是静默的数据损失 |
| 不给工具栏加"添加高度场" | 加一个按钮 | 高度场需要几十个高度值，点一下按钮得到的只能是 2×2 的平地（没有意义）；它是**写 JSON 的地形**，不是摆件图元 |

### 16.3 不变量与守卫测试

* **物理与渲染同源**：`runner::tests::heightfield_expands_to_a_trimesh_matching_the_render_mesh`
  —— 三角网顶点与 `MeshData::heightfield` 的顶点**逐位相同**、索引相同、两边 AABB 相同。
  变异：让物理用自己的一套展开（改一处）→ 红。
* **网格本身**：`render::mesh::tests::heightfield_mesh_is_centered_and_faces_up`
  —— 格点数 `行×列`、三角形数 `2·(行-1)·(列-1)`、XZ 居中、每个三角形几何法线朝 +Y、
  UV ∈ `[0,1]²`、退化输入（空/一行/行长不一/`cell ≤ 0`/NaN）一律空网格不 panic。
* **校验**：`scene_doc::tests::heightfield_diagnostics_are_specific` —— 合法 2×3 不报；
  只有一行/行长不一/缺 `heights`/`cell_size ≤ 0`/`dynamic`/`kinematic`/超上限/NaN 高度
  各自的码与路径。**超上限与 NaN 在内存里构造**（校验是纯函数，不为测上限落盘几十万个数）。
* **端到端**：`cli_e2e::heightfield_terrain_simulates_and_queries` —— 两个球停在 `y≈1.8`、
  `lowest_y ≈ 1.5`（同时证明 `position` 抬起了地形）、射线距离 3.5 且法线朝上、
  轨迹报告 `shape == "heightfield"`（静态体也能 trace）、`--overlap heightfield:...`
  退出码 2 带人话、`body: "dynamic"` 被拦。
* **编辑器兜底**：`include_export::triangulated_objects_survive_save_via_the_base_doc`
  （按名字恢复 `heights`/`cell_size`，查不到给 `None` 不编造）+
  `scene_to_json_falls_back_to_the_base_doc_for_unknown_shapes`
  （**两条**"反推不出来"的路径都接了 —— 只补一条的话另一种地形照样丢）。
* **格式枚举**：`ShapeKind::HeightField` 加进 JSON Schema 的 `shapeKind` 枚举、语法表、
  系统提示与映射表测试；删掉它就编译不过（映射表与 e2e 都引用）。

### 16.4 边界与欠账

* **窄相逐三角形**：`trimesh_vs_shape` 遍历所有三角形（每个先做 AABB 早退），没有 BVH/网格层次。
  65536 格点 ≈ 13 万三角形是"能跑但吃力"；大地形请**切块**（多个物体各一块），
  或者等"高度场专用加速结构"（下一步：按格子范围筛选候选三角形，等于给高度场一个隐式网格索引）。
* **只能静态**：会动的地形（滑坡、升降台）做不到 —— 要么用 box 拼，要么等动态网格。
* **不能当 `--overlap` 查询形状**：三角网没有解析的"点在内"判据（要做得先有"点在三角网内"的
  判定），`--overlap heightfield:...` 会明确报错并建议改用 `raycast`。
* **没有高度图/贴图导入**：`heights` 必须写在 JSON 里（没有"从 PNG 读高度"的入口）。
* **复合形状里的高度场**：可以写、能跑（整个刚体会被撑成静态），但编辑器存盘时该**部件**
  会报 skipped（按名字整体套用会串形状）。
* **没有 `flip`/法线翻转、没有多材质分层**：一块地形一个材质。

## 17. 第 61 轮：圆角盒（gap #18 收尾）—— 取舍 / 不变量 / 边界

### 17.1 一句话

`shape.type = "rounded_box"`（`size` 外接边长 + **必填** `radius`）：支持函数、AABB、膨胀都是
精确的闭式；体积闭式、**惯量是确定性网格采样**（误差 < 1%）；射线用解析 SDF 球体步进；
渲染网格按尺寸生成。至此 gap #18「形状集合固定 6 种」补齐（6 → 9 种）。

### 17.2 取舍表

| 选择 | 备选 | 为什么选它 |
| --- | --- | --- |
| 新物理形状 `Shape::RoundedBox` | 用复合形状拼（1 个盒子 + 12 个胶囊 + 8 个球） | 复合拼法**体积重复**（胶囊有一半埋在内盒里），质量会多算 4~5%，而且每个刚体 21 个部件、窄相成倍变慢；新形状的支持函数只有 6 行，GJK/EPA/射线/平面接触全套自动可用 |
| 支持函数 = `box(inner) + r·d̂` | 采样表面点做凸包 | 支持函数是精确的 Minkowski 和，凸包近似会让圆角变成多边形棱面（`r` 小时看不出来，`r` 大时很明显） |
| 惯量用**网格采样** | 闭式：6 面片 + 12 段 1/4 圆柱 + 8 个 1/8 球逐块套平行轴定理 | 闭式要处理 21 块各自的方向与质心偏移，写错一位只表现为"滚起来有点怪"——没人看得出来。采样 20 行、误差 < 1%、**完全确定**（固定顺序/分辨率），而且和本仓库凸包的既有做法一致。用球（`r = min(he)`）与盒（`r = 0`）两个已知解对账 |
| 体积仍用 Steiner 闭式 | 也用采样 | 采样有 1% 误差，而体积直接决定质量 —— 质量误差会变成"重力对不上"的可见偏差；Steiner 是三行 |
| 射线用**解析 SDF** 球体步进 | 复用 `ray_convex_hull`（GJK 最近点步进） | 实测踩坑：GJK 的"点-形状最近点"在点位于**内部**时不返回 0（返回单纯形顶点的距离），球体步进踩进内部后会跳过表面（实测把 2.5 的命中报成 3.07）。圆角盒的 SDF 是闭式且 1-Lipschitz，步进永不穿透 |
| 网格从球的方向网格**径向投影** | 按 6 面片 + 12 段 1/4 圆柱 + 8 个 1/8 球拼接 | 20 行 vs 200 行；拓扑直接沿用球（封闭、朝向一致），法线取 SDF 梯度（解析精确）。代价是疏密不均（见 17.4） |
| `radius` **必填** | 给个缺省值（0.05 / 最短边×10%） | 圆角半径是这个形状的**全部意义**：缺省值在大物体上小到看不见、在小物体上大到变成胶囊。要求显式写出最省心（校验的提示里给了建议值） |
| `radius > 最短半边长` 只报错、不钳制 | 静默钳制到最短半边长 | 静默钳制 = "写对了不生效"（和高度场动态刚体、平面 kinematic 同一类坑）。报错并提示"再大就滚成胶囊/球了" |
| 编辑器工具栏**不加**"圆角盒"按钮 | 加按钮（固定 `radius`） | 半径逐物体不同，固定值会渲染成错的圆角；写 JSON 或用底稿恢复才是对的（和高度场同一决策） |

### 17.3 不变量与守卫测试

* **支持函数与 AABB 一致**：`shape::tests::rounded_box_support_bounds_and_volume`
  —— 沿 +X 的支持点 x == 外接半长；对角支持点离内盒顶点恰好 `r`；AABB == 外接盒；
  `r = 0` 的体积 == 长方体；`r = min(he)`（三边相等）== 球；`radius` 超限不产生负内盒。
* **惯量对已知解**：`rounded_box_mass_properties_match_closed_forms` —— `r = 0` 与
  `Shape::Box` 的闭式惯量差 < 1%；`r = min(he)` 与 `Shape::Sphere` 差 < 2%；
  中间值落在两者之间且三轴相等（立方体对称）。
* **膨胀是精确 Minkowski 和**：`rounded_box_inflates_exactly` —— 外沿与半径同时 + `m`，
  沿任意方向的支持点整体外移 `m`。
* **静置与斜落**：`sim_tests::rounded_box_rests_flat_on_ground`（原点停在半长 ±2 cm、
  `up.y > 0.98` 不翻倒）、`rounded_box_settles_after_tilted_drop`（45° 落下能躺平、速度收敛）。
* **窄相与射线**：`rounded_box_pairs_produce_finite_contacts` —— 与球/盒/圆柱/圆锥/圆角盒
  五组：接触点有限、流形 ≤ 4 点、分离时无接触；射线从 3 米打到顶面距离 2.5、法线朝上。
* **网格 == 形状**：`render::mesh::tests::rounded_box_mesh_lies_on_the_surface` ——
  每个顶点 SDF ≈ 0、法线为单位向量且 == SDF 梯度、网格 AABB == 外接盒（`sectors` 是 4 的倍数
  时 ±X/±Z 面心正好被采到）；`r = 0` 时顶点都贴在盒面上；退化输入空网格不 panic。
  CLI 侧 `every_shape_kind_maps_to_physics_and_render_consistently` 再对一次
  "网格 AABB == 物理 AABB"（含 `radius` 被钳制的路径）并断言 `render_scale == ONE`。
* **格式**：`scene_doc::tests::rounded_box_diagnostics_are_specific` —— 缺 `radius`
  → `missing_field`；`radius ≤ 0` / 超最短半边长 → `out_of_range`；`size` 非正 → `out_of_range`。
  `rounded_box` 是**蛇形名**，而 `ShapeKind` 是 `rename_all = "lowercase"`，
  所以变体上单独写了 `#[serde(rename = "rounded_box")]`（否则会被序列化成 `roundedbox` ——
  这条是这轮真踩到的坑，只有 e2e 的校验路径才会暴露）。
* **编辑器往返**：`scene_io::tests::shape_export_round_trips_every_reversible_kind`
  —— 盒/球/胶囊/圆角盒写进去 → 引擎形状 → 反推回来还是同一份。
* **e2e**：`cli_e2e::rounded_box_behaves_like_a_box_and_validates_radius` —— 与同尺寸箱子
  并排落地都停在 `y ≈ 0.5` 且不翻倒、轨迹形状名 `rounded_box`、重叠查询命中、
  `radius` 超限的校验错误与 `--overlap` 的人话错误。

### 17.4 边界与欠账

* **惯量是采样值**（32³ 格心，误差 < 1%）：三轴各自独立采样，所以理论上可能出现
  微小不对称（立方体实测 < 2%，测试盯住了）；需要精确值时再补闭式。
* **网格疏密不均**：径向投影让面片中心附近三角形大、圆角处密。视觉上没问题
  （面片是平的），但如果要做"圆角处的高光很准"一类的事，应该改成拼接式网格。
* **`sectors` 必须是 4 的倍数**才保证网格 AABB == 外接盒（默认 32 ✓）；否则 ±X/±Z
  方向可能没被采到，AABB 会略小（差 < 1%，不影响物理）。
* **没有"圆角盒专用"的窄相**：走通用 GJK/EPA（和球/盒/圆锥一样），也就是每次接触解算
  要几下支持函数调用。这是所有凸体共享的成本，不是圆角盒特有的问题。
* **半空间接触候选是采样出来的**（14 个支持点 + 4 个平面面片角）：斜面/棱接触时
  流形可能不如解析解那么"整齐"，但平放是严格 4 点流形（不会摇晃）。

## 18. 第 61 轮：接触余量（gap #20 收尾）—— 一个静默的串线 bug，和为什么测试没抓到

### 18.1 一句话

`physics.speculative_margin` 现在**真的**生效（复合刚体也算）、有数值范围校验、有观察物理结果的
专门 e2e；同时修掉了"写 `static_friction_speed` 会顺手打开接触余量"的静默串线 bug。

### 18.2 bug 长什么样

```rust
if let Some(v) = doc.physics.static_friction_speed {
    world.config.static_friction_speed = v;
    world.config.speculative_margin = v;   // ← 黏在同一个 if 里
}
// ……而 `doc.physics.speculative_margin` 从来没有被读过
```

两个后果都是静默的：

1. 只调了摩擦阈值（`static_friction_speed: 0.05`）的场景，被悄悄加了 5 cm 的接触余量 ——
   物理结果变了，没有任何诊断指出"你的场景多了个余量"。
2. 真正写 `physics.speculative_margin` 的场景**毫无效果**（字段被解析、被校验、被写进 JSON
   Schema、被编辑器保留 —— 就是没接到引擎上）。

### 18.3 为什么之前的测试全是绿的

* 单测 `speculative_margin_defaults_off_and_inflates_shapes` 验的是**引擎 API**（直接设
  `SolverConfig`），不经过文档 → 世界这条线。
* 默认路径的 digest 测试证明"缺省 0 时逐位不变" —— 而那恰恰是这个 bug **不**影响的情形。
* 没有任何一条 e2e 观察**物理结果**（轨迹、穿透深度）与 `physics.*` 的关系。
* gap 表自己就写着"余量的数值范围校验与专门 e2e 用例没加" —— 欠账清单是对的，只是没还。

### 18.4 修法与守卫

* **字段各归各**：`static_friction_speed` 只写自己，`speculative_margin` 单独接线。
* `world_config_wiring_is_field_for_field`：四个世界级物理字段逐一对照 `SolverConfig` 同名
  字段 + 三条"只写 A 时 B 必须保持默认"。这是抓"接错人"最直接的办法 ——
  变异（旧代码原样放回 / 删掉余量接线）都会红。
* 校验：负数 / 非有限 / > 1 m → `out_of_range`；0.25~1 m → `suspicious_value` 告警
  （"物体之间会还没碰到就被推开"）。
* 复合刚体：逐部件 `inflated(margin)`。**不需要**重算刚体局部质心或聚合 AABB ——
  那些只跟装配有关，跟窄相选哪个几何无关（旧注释里的顾虑是错的）。
* e2e `speculative_margin_reduces_penetration_on_impact`：同一个球掉到地上，开 6 cm 余量的
  轨迹**低点更高**。这是"余量真的生效"的唯一观测口径（并且它对"没接线"是致命的：
  两个场景的低点会逐位相同）。

### 18.5 边界（诚实）

* **流形本身仍然不跨帧复用**（跨帧只有接触身份与冲量）—— 这条是 #20 唯一剩下的欠账。
* 余量的作用范围是"每对形状的窄相膨胀"，不是真 TOI：极高速的物体依然可以一步跳过整个
  障碍（那是 #13 的扫掠 CCD 的职责）。
* 余量对**三角网/高度场**同样逐三角形膨胀（每个三角形都是它的 Minkowski 和），
  但巨大地形上这会放大窄相的工作量（本来就在逐三角形）。

## 19. 第 61 轮：glTF 节点变换（gap #23 的前置）—— 现在的状态与下一步的草图

### 19.1 这轮修了什么

`assets::model_from_gltf` 原来遍历 `doc.meshes()`，**不看节点**：节点上的 TRS 全丢，
同一网格被多个节点引用也只画一次。现在：

* 从 `scenes` 的根节点往下递归，累乘世界变换（glTF 没有 `parent()`，只能自己建父子链；
  递归深度上限 64，防御坏文件的环 —— 递归爆栈不可恢复）。
* 顶点烘焙：位置按点变换、法线/切线按方向变换后归一化；非均匀缩放的严格做法（逆转置）
  没做，见 §19.3。
* 每份实例一个 `LoadedMesh`，名字带节点标签（`右/轮`）。未被任何节点引用的网格按单位
  变换兜底输出。
* 测试：手搓 4 节点 GLB（平移 / 绕 X 转 90° / 父子累乘 / 同网格 4 实例）。

### 19.2 gap #23 现在到底缺什么（诚实的清单）

| 环节 | 状态 | 说明 |
| --- | --- | --- |
| glTF 节点层级 / 节点变换 | ✅ 本轮 | 静态几何正确了 |
| glTF `animations` 解析 | ✅ 第 61 / 76 / 81 轮 | `LoadedModel.animations`（TRS 轨道、STEP/LINEAR、旋转 slerp；CUBICSPLINE 用 Hermite 入/出切线）；morph 权重轨道也解析并保留入/出切线 |
| glTF `skins` 解析（joints / inverse bind matrices / JOINTS_0 / WEIGHTS_0） | ✅ 第 70 轮（CPU） | `LoadedSkin` + `LoadedMesh.skin/joints/weights`；缺任何顶点数据时整体退回刚性路径（不撕裂网格） |
| CPU 蒙皮变形 + 顶点回写 | ✅ 第 70 轮 | `LoadedModel::skinned_mesh` 算 `Σ w·world(joint)·IBM·bindInverse`；`Scene::advance_animations_with_renderer` 调 `Renderer::update_mesh_vertices` 原地更新同一个 `MeshId`。GPU 像素回归 t=0/t=0.25 |
| GPU 蒙皮（顶点着色器关节矩阵） | ✅ 第 94 轮 | `Vertex.joints/weights` + 128 关节 uniform 调色板；`Scene` 每帧只上传关节矩阵，纯蒙皮片段不再回写顶点缓冲。像素对照：CPU 与 GPU 渲染 0/4096 像素差；调色板填充有独立断言。带 morph 的蒙皮仍走 CPU |
| 两骨 IK | ✅ 第 96 轮 | `models[].ik`（root/mid/tip 节点名 + 局部 target/pole）；每帧 CPU 采样后解三角形，只改 root/mid 朝向，长度不变；不可达夹取；`Scene::set_model_ik` 落在 `animation_worlds`，CPU/GPU 蒙皮、刚性片段、F8 骨架同源。**仍缺**：FK/IK 权重混合、多骨链、目标约束 |
| CPU 侧**采样**（给定 t 求节点世界矩阵） | ✅ 第 61 轮 | `sample_animation(name, t)`；循环/时间推进仍由调用方做（`t.rem_euclid(duration)`） |
| CPU 侧播放状态（循环/夹取/倍速） | ✅ 第 61 轮 | `ModelAnimationPlayer::advance(dt)` + `mesh_matrices(&model)`；**纯 CPU 可测**（循环用 `rem_euclid`、`duration <= 0` 不除零、非法 dt 不出 NaN） |
| 场景层应用（`Scene::apply_animation`） | ✅ 第 61 轮 | 纯 CPU、可测：`base × 额外矩阵`（不累乘 → 不漂）、只动有轨道的片段、越界跳过 |
| 渲染循环里的调用点 | ✅ 第 62 轮 | `Scene::advance_animations(dt)`；`Engine::tick`（窗口）与 `render_sequence_with_game`（无头 `movie`）每帧在 `sync_from_physics` 之后调一次，暂停不推进；单帧 `render` 用 `dt = 0` 摆起始姿态 |
| 与场景格式的接口 | ✅ 第 62 / 71 / 72 轮 | `models[].animation = { name, start?, speed?, loop?, blend? }`（**向后兼容**：不写 = 静止，老场景逐字节不变）；`blend = { name, start?, speed?, loop?, weight?, fade? }`，`weight` 缺省 0.5、`fade` 缺省 0；`LoadedModel::spawn` 返回 `SpawnedModel { objects, base }`，两份映射同源 |
| 名字不存在的诊断 | ✅ 第 62 轮（口径变了） | **不在 `validate`**：`validate` 不读资产，读不到 glTF 里的动画名。结构错误（空名字 / `speed <= 0` / `start < 0`）在 `validate`；"名字在不在"在 `simulate`/`render`/`movie` 报告的 `models[]`（`found`/`duration`/`tracks`/`clips`）+ `warnings`/`notes`，写错名字不是致命错误 |
| 编辑器可达 | ✅ 第 62 / 79 / 82 轮 | 编辑器载入即登记播放（共用 `setup_visuals` + `Engine::tick`），存盘保留 `models[].animation`；第 79 轮加「动画」面板：列出 clip/时长/倍速/循环/状态机，拖动时间轴调用 `refresh_animations_with_renderer` 当场重算位姿与蒙皮/morph；第 82 轮把倍速/循环写回 `models[i].animation` 或状态机当前 state（`Doc::set_animation_params`，可撤销）；第 83 轮普通动画可从资产里换 clip 并写回 `animation.name`（状态机片段仍由 states 管）；第 84 轮可改每条状态机过渡的 `after`/`fade`/`on_finished` 并写回 `transitions[i]`；第 85 轮可勾选「手动 morph 覆盖」并用滑杆写回 `morph_weights`（多网格共用一组，与场景格式一致）；第 86 轮可直接改状态 clip/起始时间并写回 `states[i]`；第 87 轮可增删状态和过渡（新状态自动选第一个 clip，新过渡从当前状态指向下一个状态，删除状态会连带删除引用它的过渡并改写 `initial`）；第 88 轮可改过渡的 `from/to` 与状态机初始状态；第 96 轮可直接改每条 IK 链的 target XYZ 并写回 `models[i].ik[j].target`；第 97 轮状态机参数有滑杆，拖动后 `dt=0` 立即判一次过渡（只改会话值，不写 default）；第 98 轮为 `on_event` 画「发送事件」按钮。**仍不是**完整时间轴（不能加事件、改状态名或增删 IK 链） |
| 参数/事件驱动状态机 | ✅ 第 75 / 80 / 97 / 98 轮 | `models[].animator`：`states` + `parameters` + `transitions`，按状态停留时间 `after`、`on_finished`、`conditions`（参数比较）或 `on_event`（离散事件，优先于时间过渡）切换；`fade` 秒旧→新淡入，完成后把新 clip 提为主 clip；`Scene::set_animator_parameter` / `fire_animator_event` 是宿主输入入口；报告 `models[].animator`。**仍缺**：参数曲线、事件队列/输入缓冲 |
| 混合（多轨道 / 权重） | ✅ 第 71~72 轮（双 clip + 一次性淡入） | `animation.blend`：两条 clip 各自推进时间，局部 TRS 插值（旋转 slerp），统一累乘父子链；`fade` 秒内从 0 smoothstep 升到 `weight`。报告 `models[].blend`（`found/weight/fade/duration/tracks`）。**仍缺**：任意权重曲线、>2 条 clip、事件/根运动 |
| `skins` + CPU 蒙皮 | ✅ 第 70 轮 | `LoadedMesh.skin/joints/weights` + `skinned_mesh`，每帧写回顶点缓冲；刚性片段仍走 `mesh_matrix`，绑定姿态逐位不变 |
| `skins` + GPU 蒙皮 | ✅ 第 94 轮 | `JOINTS_0/WEIGHTS_0` 顶点属性 + 128 关节 uniform 调色板（8 KB）+ `pbr.wgsl`/`shadow.wgsl` 分支；场景提交时按 `(base,count)` 绑定，纯蒙皮不再 CPU 重算。`gpu_skinning=false` 可退回 CPU；WebGL2 的最低 4 KB uniform 设备只能用 CPU |
| glTF `targets` / 静态 morph | ✅ 第 73 轮 | `LoadedMorphTarget` 解析 position/normal/tangent 位移（按节点世界方向烘过）；`LoadedModel::morphed_mesh` 按权重叠加，`deformed_mesh` 保证**先 morph 再蒙皮**；`models[].morph_weights` 覆盖 + GPU 静态回写；`info` 报 `morph_targets` |
| morph 权重动画 | ✅ 第 74 轮（双 clip 第 77 轮，CUBICSPLINE 第 81 轮） | `ReadOutputs::MorphTargetWeights` 解析为 `MorphTrack`（按 target 数切帧；CUBICSPLINE 保留 `[入切线, 值, 出切线]` 三块），`LoadedModel::morph_weights_at` 采样；场景每帧按节点应用到网格；双 clip 用 `blended_morph_weights_at` 按 `blend_weight` 逐 target 插值。**仍缺**：morph 事件；编辑器滑杆见第 85 轮（手动覆盖，多网格共用一组权重） |
| `CUBICSPLINE` 真值 | ✅ 第 76 轮（TRS）/ 第 81 轮（morph） | TRS 与 morph 权重轨道都按 Hermite 基函数求值，切线来自 glTF 的 `[入切线, 值, 出切线]`；旋转分量求值后归一化；切线不齐时退化为线性 |
| 动画 vs 物理 | ⚠️ 有意 | 模型动画是**视觉**的：`models[].collide` 的静态三角网碰撞体保持绑定姿态，不会跟着动 |
| `LoadedModel` 的节点树 | ✅ 第 61 轮 | `nodes` + `bind_pose()` + `bind_inverse`（每网格）——静态烘焙与动画并存：`world(t) × bind_inverse` 在绑定姿态下是单位矩阵 |

### 19.3 实现顺序（第 61~63 轮，已走完前三步）

1. ✅ **保留节点树**：`LoadedModel.nodes`（name / parent / local TRS）+ 由它算世界变换。
   静态路径继续烘焙（零回归），树留着给动画用。
2. ✅ **`animations` 解析 + CPU 播放**：TRS 轨道、STEP/LINEAR（旋转 slerp）；
   `ModelAnimationPlayer` 的循环/夹取/倍速是**纯 CPU 可测**的，不需要显卡。
3. ✅ **接进场景格式**：`models[].animation` + 每帧推进 + 编辑器保留 + 三处报告对账。
   与最早设想的一处偏差：**"动画名不存在"不做成 `unknown_reference` 致命错误** ——
   `validate` 的契约是"不读资产也能校验"，让它去装载每个 glTF 才能判断名字，会把
   一次纯结构校验变成 N 次文件解析（Web 端还没有文件系统）。现在结构错误仍然致命，
   名字错误降级为报告里的 `found: false` + 警告，退出码 0；这比"validate 忽然变慢/联网"
   更符合既有契约。
4. ✅ **`skins` + 蒙皮**（第 70 轮 CPU；第 94 轮 GPU）：解析 + 逐帧变形；GPU 路径只上传调色板，t=0/t=0.25
   的 GPU 像素回归；t=0 与绑定姿态一致。✅ **双 clip 混合**（第 71 轮）+
   **一次性 smoothstep 淡入**（第 72 轮）：`animation.blend` +
   `Scene::play_model_animation_blend_with_fade`。✅ **静态 morph target**（第 73 轮）：
   `targets` 解析 + `morph_weights` 场景覆盖 + 先 morph 后蒙皮。✅ **morph 权重动画**
   （第 74 轮）：`weights` 轨道 + 场景逐帧采样；双 clip 的 morph 权重按 `blend_weight`
   逐 target 插值。✅ **时间驱动状态机**（第 75 轮）：
   `animator.states/transitions` + 淡入。**GPU 蒙皮第 94 轮已补**（纯蒙皮）；
   **两骨 IK 第 96 轮已补**（`models[].ik`）；仍缺 morph+skin 的 GPU 路径、
   任意权重曲线、事件队列/输入缓冲、FK/IK 权重混合。
5. ✅ **`skins` + GPU 蒙皮**（第 94 轮）：顶点属性扩展 + 128 关节 uniform 调色板 +
   `pbr.wgsl`/`shadow.wgsl` + 阴影管线绑定；带 morph 的蒙皮仍走 CPU，
   `gpu_skinning=false` 可整体退回。像素对照与调色板填充都有测试。

## 20. 第 63 轮：`waters` —— 解析水域 + 浮力（M1）

### 20.1 这轮交付了什么

* 场景格式：`waters: [{ center: [x,z], size: [x,z], level, density, drag }]`，
  不写 = 没有水（物理结果与以前**逐位相同**；`Shape` 的采样/浮力代码只在 `waters` 非空时执行）。
* 物理：`PhysicsWorld::add_water` + 每个子步在速度积分**之前**施加浮力；作用点是**排开体积的
  形心**（不是质心），所以倾斜的箱子有扶正力矩。`submerged_fraction()` 是只读观测值。
* 渲染：`setup_visuals` 生成一块单位水面网格（16×16 细分，为将来的波浪网格留位置），
  按水域尺寸缩放，alpha 混合。没有波浪/折射；渲染器本来就会把 alpha 批次排到最后。
* 报告：`simulate` 顶层 `waters`（没水域不出现）+ `states[].submerged`（没浸没不出现），
  人读输出多一行"水域"与"浸没 %"；`mcp` 的 simulate outputSchema 同步。
* 编辑器：载入即装进世界并出现"水域"物体；存盘原样保留（`waters` 在底稿里），有 GPU 测试。

### 20.2 算法与边界（诚实清单）

| 项 | 状态 | 说明 |
| --- | --- | --- |
| 浸没体积 | 🟡 采样近似 | 每个形状部件在**本地 AABB** 上取 10³ 个格点，`Shape::contains_local` 判内部、变换到世界判水面；体积 = 格体积 × 命中数。误差约几个百分点（球体半浸没实测 < 5%），因此平衡水位有约一格（尺寸/10）的量化误差 |
| 凸包 | 🟡 按 AABB | 凸包只存点集、没有面表；`contains_local` 对凸包退化为 AABB 判定（与 `Shape::volume` 的口径一致），文档已标注 |
| 水面 | 半空间 | `y <= level` 且在 XZ 范围内就算浸没；**没有池底**（要池底就把地面放水下） |
| 浮力 | ✅ | `F = -g · ρ · V_sub`；`g = world.gravity()`，与 `gravity_scale` 无关 |
| 阻尼 | 🟡 简化 | 浸没部分按 `drag × submerged` 的比例缩速度（1/s），不是粘性应力；缺省 6 是"辐射阻尼"的替身 |
| 睡眠 | ✅ 可睡 | 浮力直接累加 `force/torque`、**不调 `wake_up`**，睡眠体连采样都跳过（`submerged` 停在最后醒着的值）；水面变化后要显式唤醒 |
| 多重水域 | ⚠️ 叠加 | 重叠水域会叠加浮力（不做优先级/去重）；文档建议不要重叠 |
| 粒子/流动/波浪/折射 | 🟡 | PBF 粒子流体（M2/M3）、CPU 公告板粒子（第 102 轮）与可配置屏幕空间水面后处理（M4.2：mask.g 水面高度 + 波面法线/高光）已做；真实波浪/流动/光线折射/焦散、GPU/软粒子仍无 |
| 格式校验 | ✅ | 尺寸/密度/阻尼非法在 `validate` 报 `waters[i].*`；`build_world` 再挡一次（不静默忽略） |

### 20.3 测试与证据

* `quench3d-physics`：`water_contains_respects_bounds_and_level`、
  `half_submerged_sphere_reports_half_its_volume`、`fully_above_water_has_no_buoyancy`、
  `fully_submerged_box_reports_its_volume`、`body_volume_sums_parts`、
  `half_density_sphere_floats_at_the_surface`、`dense_body_sinks_and_rests_on_the_ground`、
  `buoyancy_is_deterministic`。
* CLI e2e：`water_buoyancy_floats_light_bodies_and_sinks_dense_ones`（半密度箱停在水面、
  两倍密度箱沉底、报告有 `waters` / `submerged`、非法尺寸报 `waters[0].size`）。
* 编辑器（需 `QUENCH3D_GPU_TESTS=1`）：`waters_survive_editor_roundtrip_and_render_a_surface`。
* 像素证据：同一场景有/无 `waters` 的 `render` 对比 **25.9% 像素变化**，差异 y 范围
  154~419（画面下半部，即水面），平均绝对差 6.24；GIF（48 帧，640×420）帧间差异经 2px
  高斯模糊后集中在箱子区域（2.6%），排除 GIF 调色板抖动造成的假差异。

## 21. 第 64 轮：`fluids` —— CPU PBF 粒子流体（M2）

### 21.1 这轮交付了什么

* **物理**：`quench3d-physics/src/fluid.rs` 的 `Fluid` / `FluidDesc` / `FluidStats`。
  长方体晶格发射、空间哈希邻域、poly6 密度、spiky 梯度、密度约束迭代、边界投影、
  XSPH 粘性、速度限幅。粒子质量按**晶格静止密度**校准（`m = rho0 / ΣW`），
  不是 `rho0·spacing³`——后者只有 0.5~0.9 倍静止密度，会在"只压不拉"的 PBF 里
  直接互相穿过、摊成一层。
* **格式**：顶层 `fluids: [{ center, size, spacing, radius, rest_density, viscosity,
  iterations, max_particles }]`；`deny_unknown_fields`；缺省值见 `FluidDoc`；校验覆盖
  尺寸/间距/半径/密度/粘性/迭代/上限，`max_particles` 截断给警告。
* **报告**：`simulate` / MCP 输出 `fluids[]`（`particles`、`avg_density_ratio`、
  `max_speed`、`min_y`、`max_y`）；人读输出每个流体一行。
* **编辑器/CLI 可达**：`build_world` 转成 `FluidDesc` 并 `add_fluid`；编辑器
  `scene_roundtrip` 保留 `fluids`，`world.fluid_count()` 可见；`examples/fluid_pool.json`
  可直接 `simulate`/`render`。
* **文档/语法**：`schema.rs` 的 grammar、JSON Schema（`$defs/fluid`）、system prompt、
  include 可合并段列表；README §6.3。

### 21.2 算法与边界（诚实清单）

* PBF（Macklin & Müller 2013）的**压缩**形式：`C = max(rho/rho0 - 1, 0)`，1% 死区；
  `lambda` **每次迭代重算**（只算一次再反复加同一个 delta 会过冲，无重力也会炸）。
* 边界投影放在**迭代内部**：粒子不会先陷进地面再被一次性弹出来；`Plane` 的 AABB
  无效，不能拿它做拒绝测试。
* **单向（M2）**：刚体 → 流体。M3 已补上冲量反推 + 自由表面浮力；这一节保留 M2 当时的边界。
* **没有表面张力 / 涡量 / 粒子间碰撞**。敞开的平地上水会摊成薄层（密度约束允许二维
  密排），这是"液体在无限平面上的解"，不是穿地；围池（`examples/fluid_pool.json`）
  才能堆出三维水堆。测试 `pool_holds_a_three_dimensional_pile` 覆盖这一点。
* `TriMesh` 碰撞体不挡粒子（`Shape::closest_surface` 返回 `None`）；凸包/圆锥用 AABB
  近似。粒子渲染在 M4 已补（见 §23）。
* 统计里的 `avg_density_ratio` 是**求解后**密度（报告口径）；`max_speed` 限幅到
  `MAX_SPEED`。睡眠概念不适用；多流体之间不互相作用。

### 21.3 测试与证据

* `quench3d-physics`：`fluid_spawns_a_deterministic_lattice`、`invalid_parameters_are_rejected`、
  `particles_fall_and_stop_on_the_ground`、`wall_and_ground_keep_particles_out`、
  `pool_holds_a_three_dimensional_pile`、`fluid_is_deterministic`；以及
  `closest_surface_matches_primitive_geometry`。
* CLI：`fluids_parse_validate_and_roundtrip`（缺省值/往返/8 条非法路径）、
  `fluid_particles_settle_and_are_reported`（4000 粒子围池、报告字段、`fluids[0].spacing` 错误）。
* 编辑器：`fluids_survive_editor_roundtrip`。
* 示例：`examples/fluid_pool.json` 240 步 → 4000 粒子、密度比 1.01、最高速度 0.33 m/s、
  y 0.05..1.25。

## 22. 第 65 轮：流体 ↔ 刚体双向耦合（M3）

### 22.1 这轮交付了什么

* **冲量反推**：`Fluid::step` 记录粒子与刚体的接触（法线/接触点/刚体下标/边界速度）；
  速度求解后，对**动态**刚体按 `J = m_p·v_n·n` 反推法向冲量并 `wake_up`。只反推 20%：
  粒子的"压力"由浮力项承担，全额反推会让成千上万颗粒子把刚体当鼓面敲（实测浮球被砸进池底）。
* **粒子自由表面浮力**：`Fluid::surface_near(aabb, margin)` 取刚体 XZ 附近粒子的最高 y
  作为近似水面；用现成的 `water::submerged_in` 算浸没体积/形心，施加 `-g·ρ·V` 与
  `drag` 阻尼。`fluids[].drag` 缺省 6，0 = 无阻尼。
* **格式/报告**：`fluids[].drag` 进 grammar、JSON Schema、system prompt、`FluidDoc`；
  `simulate` 的 `fluids[].contacts`（0 时省略）；`states[].submerged` 取 `waters` 与
  `fluids` 的较大值。
* **示例**：`examples/fluid_pool.json` 加 300 kg/m³ 浮球；480 步后 y≈0.96（水面 1.25，
  浸没 26%），流体最高速度 0.38 m/s。

### 22.2 边界与不变量

* 浮力水面是**瞬时最高粒子**，飞溅会抬高读数；不是平滑水平集，也没有表面张力。
* 多流体/水域只取**最大**浸没占比，浮力不叠加（与 M1 `waters` 的"浸没体积求和"不同，
  这是有意的：两套水体描述同一个池子时不该双倍托举）。
* 反推只作用于动态刚体；静态刚体只挡粒子。睡眠体被粒子撞到会醒；只有浮力（没有新接触）
  不会叫醒它，和 M1 的取舍一致。
* 粒子之间仍无碰撞，敞开平地上会摊成薄层；`TriMesh` 仍不挡粒子。渲染见 §23。

### 22.3 测试与证据

* `quench3d-physics`：`fluid_pushes_a_heavy_dynamic_body_downward`、
  `fluid_buoyancy_keeps_a_light_body_afloat`、`fluid_wakes_a_sleeping_body_on_impact`，
  加上 M2 的 6 条流体测试。
* CLI e2e：`fluid_buoyancy_floats_a_light_body`（300 kg/m³ 球 > 0.5 m、报告 `contacts` > 0）、
  `fluid_particles_settle_and_are_reported`（围池接触数）。
* 示例：`examples/fluid_pool.json` 240 步浮球已在水面，480 步收敛到 26% 浸没。

## 23. 第 66 轮：粒子/水面渲染（M4）

### 23.1 这轮交付了什么

* **粒子可视**：`SceneObject` 新增 `fluid_particle: Option<FluidParticleVisual>`；
  `Scene::sync_from_physics` 每帧从 `world.fluids()` 读粒子位置/速度，写回物体的
  平移与缩放，并按速度给颜色加一点青白。每颗粒子一个场景物体，网格是 8×6 的低模球
  （80 三角形；4000 颗约 32 万三角形，软件光栅化可接受）。
* **水面可视**：`fluid_surface: Option<FluidSurfaceVisual>`；每帧把半透明平面抬到该
  流体粒子的最高 y，尺寸取 `fluids[].size` 的 XZ。走渲染器已有的 alpha 混合批次
  （`depth_write=false`、排在 opaque 之后）。
* **接入**：`runner::add_fluid_visuals` 在 `setup_visuals` 里建粒子/水面物体；CLI
  `render`/`movie`、编辑器共用 `Scene::sync_from_physics`，所以三条路径同一套。
* **规模**：`examples/fluid_pool.json` 960×600：4909 实例、47.2 万三角形、约 10 s；
  相对无流体基线 **7.4% 像素变化**（差异包围盒覆盖水池）。640×420、48 帧 GIF 约 39 s，
  帧间平均 7.6% 像素变化。

### 23.2 边界（诚实清单）

* **M4 当时不是屏幕空间水**：M4 先落地 alpha 平面；M4.1 补了基于 mask 的屏幕空间
  折射/吸收/菲涅耳/边缘泡沫；M4.2 又补了 `mask.g` 水面高度、程序化波面法线与太阳
  高光（见 §23.4）。仍**没有**真实波浪模拟/焦散/水下体积光，吸收厚度固定，
  折射是屏幕空间偏移而不是光线追踪。
* 粒子球是**视觉代理**：可视半径 `radius * 1.4`，比物理半径大，为了少留缝；它不改变
  碰撞/浮力。
* 4000 个场景物体对编辑器 UI/拾取不是免费午餐：这是"先能看见"的实现，不是最终
  GPU instancing 方案；渲染器已经按 `(mesh, material)` 合批，绘制调用只有 5 个。
* 水面高度取**最高粒子**，飞溅时会被抬高；和 M3 浮力用的是同一个近似。

### 23.3 测试与证据

* `quench3d`：`scene::tests::fluid_visuals_follow_the_particle_cloud`（不建 GPU，
  直接验证粒子位置/缩放与水面高度同步）。
* 像素：`render examples/fluid_pool.json` 对照无流体场景，7.4% 像素变化、平均绝对差
  4.06；相对 M4.1（无波面法线）2.55% 像素、平均 0.59 变化；相对 M4 alpha 水面
  2.55% 像素、平均 0.57 变化；同场景把 `wave_strength/foam/refraction` 设为 0
  另有 1.75% 像素、平均 0.25 变化（证明风格参数真的进了后处理）。
* 报告：`render` 的 `instances` 4909、`triangles` 472434，证明粒子确实进了提交列表；
  有水域时 `water_post: true`。

### 23.4 M4.1/M4.2：屏幕空间水面

**为什么不是读深度**：第一版想用后处理读主 pass 深度，从世界坐标判"这是不是水面"。
问题是水面走 alpha 混合、`depth_write=false`，深度纹理里根本没有水面；改用 SSAO 的
单采样深度解析又受绑定/重建路径影响（实测读回常量远平面）。这条路不可靠，回滚。

**最终方案：主 pass 第二颜色附件（MRT）**：

* 所有主 pass 管线（pbr/sky/grid/line）都输出 `@location(1)`；非水面写 0，水面材质
  （`MaterialDesc.water_mask = true`）写 `r=1, g=世界高度`。mask 格式 `Rgba16Float`
  （8 位会把水面高度量化到几十厘米），MSAA 时同样解析。
* mask 经过主 pass 深度测试：墙/粒子挡住水面的像素不会写 1，所以后处理不会"透过墙"上色。
* `post.wgsl` 读 `@binding(5)` 的 mask：用屏幕射线与 `g`（水面高度）求交重建水面
  世界坐标，据此生成程序化波面法线（两个八度），再做屏幕空间折射、固定厚度吸收、
  Schlick 菲涅耳、太阳高光（用 `lighting.sun` 的方向/强度）与波峰/边缘泡沫。
  不依赖深度纹理，GL/WebGL2 也能建管线。
* 顶层 `water`（根独有段）：`color` / `clarity` / `wave_strength` / `foam` /
  `refraction`，经 `water_options_for` 映射到 `RendererOptions.water`；所有水域共用
  一份风格。它只影响画面，不改变浮力/粒子结果；`include` 片段里写会报错。
* `RendererOptions.water` 默认关闭；`runner::apply_water_options` 在 `waters`/`fluids`
  非空时打开。关闭时水面仍是 M4 的 alpha 平面，像素与旧版一致。

**验证**：

* CPU：material `params2.w`、`PostUniforms` 352 字节布局、GL 变体保留 mask 绑定、
  `water` 风格解析/范围校验/往返、`water_options_for` 映射、编辑器存盘保留、`include`
  片段写 `water` 报错。
* GPU（`QUENCH3D_GPU_TESTS=1`）：`water_post_uses_the_mask_and_changes_pixels`——
  全屏 alpha=0 水面写 mask，开后处理中心/角落红通道必须升高；不开时不变；再把
  `wave_strength/foam/refraction` 调大，中心像素必须继续变化（波面法线/高光真的生效）。
  `water_mask_height_reconstructs_the_screen_ray`——水平面抬高 5 m 后，偏中心像素
  必须变化，证明 `mask.g` 真的参与射线-平面求交。
* 静态图 / GIF 指标见 §23.3；`water_post` 字段证明 CLI 真的接上了。

**边界**：

* 吸收用固定厚度，没有按水深/视线长度积分；折射是屏幕空间偏移，不是真实光线偏折。
* 水面几何仍是平面；法线是程序化波面法线（水平面扰动），斜坡/瀑布不适用；
  泡沫是 mask 边缘 + 波峰近似，不是白水模拟。
* 没有焦散、水下体积光、真实波浪；粒子本身不写 mask（只有水面平面写）。
* 射线-平面求交假设相机在水面上方；相机潜入水下时水面重建不成立。
* 编辑器「屏幕空间水」面板只调**全局**风格，不能逐水域上色；颜色/波浪/泡沫/折射
  存盘会写回顶层 `water` 段。
* mask 附件始终分配（即使 `water.enabled=false`），约 8 B/像素（`Rgba16Float`），
  MSAA 4× 时更高；
  这是为了不改主 pass 管线布局（所有管线必须同一 attachment 数量）。

### 23.5 IBL 环境贴图 + 解析式天空（第 100 轮）

**做了什么**：`lighting.environment_map`（等距柱状 `.hdr`/`.png`/`.jpg`）在装载时解码成
线性 RGB，CPU 生成 64×64×6、7 级 mip 的 cubemap：mip 0 直接采样，之后每级在方向锥内
按粗糙度做 12~84 个方向的平均。运行时 `pbr.wgsl` 的 `environment_radiance(dir, roughness)`
按 `roughness * max_mip` 采样，金属/光滑材质得到环境反射，漫反射用最高 mip 的平均照度；
`environment_intensity` 与 `environment_rotation_deg` 控制亮度与绕 Y 旋转。没有贴图
（`env_params.y < 0.5`）时走原来的解析式天空，**旧场景像素不变**。

环境贴图挂在 **group 3**（蒙皮调色板所在的全局 bind group），因为 WebGL2 只保证 4 个
bind group；纹理固定 64px/7 mip，换图只 `write_texture` 覆写内容，bind group 不重建。

**验证**：`cargo test -p quench3d-render --lib environment`（Radiance 平铺/新 RLE/旧 RLE、
6 面/mip 递减）；`cargo test -p quench3d-cli --test cli_e2e environment_map_changes_pixels`
（无太阳/环境光时，黑球在 HDR 下中心亮度比无图高 >60）。

**边界**：

* 只支持标准 `-Y height +X width` 朝向；不含 `.exr`；LDR 图只做 sRGB→线性。
* 预滤波是锥形平均，不是精确 GGX/PMREM，没有 BRDF LUT；没有从环境图提取太阳/局部光。
* 反射里不会出现场景物体（没有 SSR/平面反射）。

### 23.6 CPU 公告板粒子（第 102 轮）

**做了什么**：顶层 `particles[]`（`count`/`position`/`radius`/`velocity`/`spread`/
`lifetime`/`size`/`gravity`/`color`）在装载时创建 N 个隐藏代理物体，每个用一个面向
相机的 `MeshData::quad`（白色 unlit + alpha 混合 + 双面共享材质，颜色来自实例色）。
`Scene::update_particles(camera, dt)` 每帧推进年龄、重力与速度，重生时回到发射器并
**跳过该帧积分**（否则新粒子会凭空多飞一帧）；公告板用
`Quat::from_rotation_arc(Vec3::Z, dir)` 转向相机，alpha 按 `(1-age/lifetime)^2` 淡出。
粒子物体由 `Scene::is_particle_object` 标识，场景树/拾取/场景导出都跳过它们；编辑器
保存时 `particles` 段从底稿原样带出，重新载入会重新建发射器。暂停时 `dt=0`，只更新
朝向，不继续飞。

**验证**：`cargo test -p quench3d --lib particle`（4 条：代理/隐藏、运动+朝向+淡出+重生、count/寿命/尺寸 clamp、删除前方物体后代理下标前移）；`cargo test -p quench3d-cli --lib particles_parse`
（缺省/往返/8 条非法路径）；`cargo test -p quench3d-cli --test cli_e2e
particle_billboards_change_pixels`（同相机、无太阳/环境光，中心红色分量 +40 以上）；
`cargo test -p quench3d-editor --test history particles_survive_editor_roundtrip_and_reload`
（存盘保留 + 重载建 3 个代理）。

**边界**：没有贴图/软粒子/粒子间深度排序（按物体距离排序，不按粒子深度）、GPU 粒子、
发射器形状/曲线；编辑器没有粒子创建/参数面板，只显示并原样保存 JSON 里的段。

### 23.7 编辑器多选 + 成组变换（第 101 轮，框选第 104 轮，层级抑制第 106 轮）

**做了什么**：`Doc.extra_selected` 保存主选中之外的物体；场景树与视口都支持
`Shift`/`Ctrl` 点选加减选，场景树标题显示「已选 N 个」。多选时 gizmo 的轴心是
`Doc::selection_center`（所有选中物体世界位置的平均值）；拖拽时把主物体的
`Δ平移 / Δ旋转 / Δ缩放` 通过 `group_member_transform` 套到 `Doc::group_selection`
里的附加物体上：旋转和缩放绕轴心做刚体变换，平移直接加位移；主物体自己也绕轴心
旋转/缩放，否则只有其它人在动。附加物体仍用**未绕心**的主变换算增量，避免把主物体的
绕心位移又当平移加一次。同一模型展开的其它网格不进入组列表（主物体的
`apply_transform_to_model_group` 已经把整组模型带走，重复套用会双倍位移）。
「复制」「删除」按整组执行；一次拖拽仍走历史事务，只算一步。

**验证**：`cargo test -p quench3d-editor --lib multi_selection_toggle`（主选中提级/去重）、
`--lib group_member`（平移/绕 Y 90°/2 倍缩放都绕轴心）；GPU 历史测试
`delete_selected_removes_every_selected_object`、`duplicate_selected_copies_every_member`
（批量删除/复制后的数量、下标、选中与组轴心）；`ui_smoke` 第二帧断言场景树出现
「已选 2 个」。

**边界**：没有框选；父物体与子物体同时选中时两者各自参与变换（不做"只动父"的层级抑制）；
多选复制出来的模型网格是普通物体，不会重建 `models[i]` 引用；轴心固定在选中中心，
没有手动轴心/3D 光标。

---

### 23.8 Web 自动保存 + 恢复快捷键（第 103 轮）

**做了什么**：`AutoSave` 从"桌面专用"改成两端可用。桌面仍写
`$XDG_STATE_HOME/quench3d/autosave.scene.json`（临时文件 + rename）；Web 写
`localStorage["quench3d.autosave.scene.v1"]`，启动时 `read_recovery()` 读回并挂出
「发现自动保存 / 恢复 / 丢弃」。恢复/丢弃快捷键为 `F9` / `F10`（只在恢复点存在且键盘没被输入框占用时生效，
且在 `Engine::tick` 之前读按键，避免被 `begin_frame` 清掉）。只有编辑过的场景
（`history.can_undo()`）或刚恢复过的场景才写，刚打开的默认场景不写。Web 手动下载场景后清掉
恢复点，避免下次刷新又提示。Web 的 `std::time::Instant::now()` 会 panic（std 在 wasm
上未实现），所以 `AutoSave` 的计时在 Web 分支改用 `quench3d::clock::now_seconds()`
（`performance.now()`），间隔仍是 30 秒；`localStorage` 写入失败（配额/隐私模式）走
`defer` 延后重试，不打断编辑。

**验证**：原生 `cargo test -p quench3d-editor --lib autosave`（2 条文件语义）；
`tools/build_web.sh web_client` 后再跑 `tools/probe_web_ui.sh`：向
`localStorage` 写入一份带「自动恢复箱」的场景 → `Page.reload` → 断言
`data-quench3d-ui` 里 `recovery=true` 且默认场景物体数 >1 → 先断言 localStorage 为空（未编辑不写）→ 按 `F9` → 断言
`recovery=false`、`objects=1`、截图哈希变化 → 断言 localStorage 立刻又写出包含
「自动恢复箱」的新恢复点（写路径）→ 清键。探针输出
`PASS 自动保存写入：... 字节` 与 `PASS 自动保存 F9`。

**边界**：Web 没有"正常退出"钩子，所以恢复点只会在手动保存/下载或用户丢弃时清；
布局（面板宽度/窗口位置）仍不持久化；localStorage 约 5MB，场景过大时写失败只记日志。

### 23.9 编辑器框选（第 104 轮）

**做了什么**：`Doc::select_in_rect(engine, viewport, min, max, additive)` 把可见物体的
**世界位置投影到屏幕**，中心点落在矩形内就命中；跳过粒子/流体代理。UI 在
`Shift`/`Ctrl` + 左键拖动时画半透明选框（`Viewport.box_select_start`），松开后追加选择；
空框在非追加模式下清空。app 层在按下时检查 `Shift/Ctrl`，有修饰键就不把左键拖动当转视角，
框选因此不会顺手把相机转走。

**验证**：`QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test history
box_select_uses_screen_centers_and_adds_or_clears`（左/右两个箱子、只框左、Shift 追加、
空框清空）；`tools/probe_web_ui.sh` 用 CDP 发 Shift+拖动，断言 `data-quench3d-ui` 的
`selected` 从 1 变 7、中央视口截图哈希变化。

**边界**：按中心点命中（大物体边缘在框内但中心在外不算）；不做深度遮挡/透视裁剪；
框选只处理场景物体，不处理粒子/流体代理；没有"框选减选"。

### 23.10 编辑器内部剪贴板（第 105 轮）

**做了什么**：`Doc.clipboard` 保存选中物体的 `SceneObject` 快照 + **世界变换** + 刚体
`Shape`/静态动态标记。`Ctrl+C` 只写剪贴板（不记历史、不改场景）；`Ctrl+V` 给每份物体
加 `(1.2,0.2,0)` 偏移、自动重名、按原形状重建刚体、`parent=None`，并把粘贴体作为一组
选中；粘贴调用 `record_edit`，所以可撤销/重做。打开/恢复场景时清空剪贴板，避免旧 mesh
下标落到新场景。`Ctrl+C`/`Ctrl+V` 在 app 的 Ctrl 分支里先于引擎处理。

**验证**：`QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test history
copy_and_paste_clipboard_rebuilds_objects_and_is_undoable`（复制不改场景/不进历史、粘贴
+2、自动重名、根物体、刚体存在、撤销/重做数量、空选择不清剪贴板）；浏览器
`tools/probe_web_ui.sh` 在框选 7 个后发 Ctrl+C/V，`objects 7 → 14`，截图变化。

**边界**：只保存视觉字段和刚体形状/静态动态，不保存密度/摩擦/材质贴图覆盖；粘贴体
不继承父子关系；剪贴板不跨进程/不碰系统剪贴板；粘贴到已删除 mesh 的场景不可能，因为
打开新场景会清空。

### 23.11 父子同选的层级抑制（第 106 轮）

**问题**：父物体和子物体同时被选中时，gizmo 既给父写变换（子随层级移动），又把子当成
一个独立组成员再写一次，结果是子物体走两倍。

**做了什么**：`Doc::drag_driver(engine, index)` 沿 `parent` 链找**最上层被选中的祖先**；
`Doc::group_selection` 只保留"选中集合里的根"（有被选中祖先的物体不进组），并排除
驱动者本人。gizmo 的投影/开始拖拽/Esc 还原/每帧写回/冻结物理全部改用 `driver`，所以
父+子同选时只动父，子跟着走；同层其它选中物体仍按组变换。父+子+旁人时，组里只有旁人。

**验证**：`QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test history
selected_parent_suppresses_child_in_the_transform_group`（父+子组为空、旁人进组、
只选子时驱动者回到子）。

**边界**：不做"选择时自动去重"（树上仍显示两个都选中）；旋转/缩放父物体时绕父自身
原点（组为空），不再绕"父子平均位置"；`selection_center` 仍按全部选中项算（gizmo 的
屏幕锚点用驱动者世界位置）。

### 23.12 单文件 glTF 2.0 导出（第 107 轮）

**做了什么**：新增 `crates/quench3d-cli/src/gltf_export.rs`。`quench3d export scene.json
model.gltf` 按扩展名走 glTF 分支；复用 OBJ 导出的形状→网格与父级世界变换逻辑，把每个
部件的顶点/法线/UV/索引烘进世界空间，写成一个 `data:application/octet-stream;base64,...`
buffer（所以 `.gltf` 单文件、不额外落 `.bin`）。材质写 `pbrMetallicRoughness`
（baseColor/metallic/roughness）、`emissiveFactor`、alpha<1 时 `alphaMode=BLEND`。
`models[]` 外部资产、凸包按既有边界跳过并写进 `skipped`。

**验证**：`cargo test -p quench3d-cli --lib gltf_export`（合法 JSON、2 节点/2 mesh、
base64 buffer、父位移烘进子几何的包围盒、凸包跳过）；`cargo test -p quench3d-cli --test
cli_e2e export_writes_single_file_gltf`（CLI 真的落盘 → `info model.gltf` 读回三角形数
与包围盒一致）。

**边界**：父子关系写成节点树（局部 translation/rotation），不导出动画/骨骼/morph；
四边形等非三角图元会三角化；这是"能在别的工具里看到同样几何/颜色/贴图/层级"的导出，
不是无损编辑往返。

### 23.13 编辑器导出 glTF（第 108 轮）

**做了什么**：顶部工具栏加「导出 glTF」。`scene_io::scene_to_gltf` 先调用已有的
`scene_to_json`（保证编辑器懂的部分写回底稿、无法表达的物体进 `skipped`），再把 JSON
解析回 `SceneDoc` 交给 CLI 的 `gltf_export`，所以编辑器和 CLI 看到的是同一份场景语义。
桌面写到当前场景同名 `.gltf`；Web 走 `download_text` 下载。状态栏报告物体/三角形/字节数
与跳过项。

**验证**：`QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test history
editor_gltf_export_is_valid_and_contains_the_scene`（合法 glTF、`nodes.len()==objects`、
三角形 >0）；CLI 导出与 `info` 往返见 §23.12。

**边界**：按钮不弹文件对话框（桌面用当前场景路径换扩展名，Web 下载）；外部模型/
凸包/层级/动画仍不导出；导出失败只改状态栏，不改场景、不进历史。

### 23.14 glTF 贴图内嵌（第 109 轮）

**做了什么**：`export_gltf_with_base(doc, base_dir)` 按场景文件目录解析
`base_color_texture` / `normal_texture` / `metallic_roughness_texture` /
`emissive_texture`，把 PNG/JPEG 原字节追加进同一个 buffer，登记 `images` +
`textures` + 一个 REPEAT/LINEAR 采样器；材质写 `baseColorTexture` /
`metallicRoughnessTexture` / `normalTexture`（含 `normal_scale`）/ `emissiveTexture`，
并把 `alpha_cutoff` 映射成 `alphaMode=MASK` + `alphaCutoff`、`double_sided` 映射成
`doubleSided`、`unlit` 映射成 `KHR_materials_unlit`。同一路径按内容去重；缺文件/
非 PNG/JPEG 只进 `skipped`，几何照常导出。CLI 用场景所在目录，编辑器用
`doc.file_path` 的目录；Web 无文件系统，贴图报告未嵌入。

**验证**：`cargo test -p quench3d-cli --lib gltf_export` 新增
`embeds_texture_bytes_and_maps_material_fields` / `missing_texture_is_reported_without_blocking_geometry`；
CLI e2e `export_gltf_embeds_texture_from_scene_directory` 真的写 PNG 到场景目录、跑
`quench3d export`，断言 `data.textures==1`、`images[0].mimeType=="image/png"`、
材质挂到 `baseColorTexture`、`alphaMode=="MASK"`、图片字节进了 base64 buffer。
总测试 758 → 761。

**边界**：只支持 PNG/JPEG；不写 `uv_scale` 的平铺（core glTF 无此字段，会进 `skipped`）；
Web 导出不带贴图；贴图不重编码、不压缩、不生成 mipmap。

### 23.15 glTF 节点树导出（第 110 轮）

**做了什么**：不再把父位移烘进子顶点。每个场景物体对应一个 glTF 节点，节点写局部
`translation` + 四元数 `rotation`，`children` 按 `parent` 串起来，`scenes[0].nodes`
只列根节点；物体 `scale` 与部件局部位姿仍烘进顶点，所以世界位置不变。父链成环时
断开成根并在 `skipped` 里报告。空/跳过的物体也保留空节点，避免孙物体失去父链。

**验证**：`cargo test -p quench3d-cli --lib gltf_export` 的
`exports_valid_gltf_with_embedded_buffer_and_transforms` 断言 `scenes[0].nodes=[0]`、
`nodes[0].children=[1]`、父子局部 translation 与**局部**几何包围盒；
`nested_nodes_keep_three_levels_of_translation` 断言三级链
`根→中→孙` 的 children/translation；CLI e2e 仍用 `info model.gltf` 读回三角形数
与包围盒（导入器会应用节点链）。总测试 761 → 762。

**边界**：动画/骨骼/morph 仍不导出；`scale` 不在节点上（烘进顶点），所以外部工具
里不能单独改节点缩放；节点名可能重复。

## 24. 第 67 轮：摄像头动捕第一阶段 —— 文件协议 + 2D 轮廓 + 3D 骨骼

**做了什么**：

* `tools/camera_mocap/camera_mocap.py`：OpenCV 采集（Linux V4L2）、MediaPipe Pose
  33 点（2D 归一化 + 以髋部为原点的 3D 世界点）、Pose 分割 mask 的最大外轮廓、
  可选 MiDaS small ONNX 单目相对逆深度；OpenCV 预览与深度叠加。
* 输出 `latest.json` + `latest.rgb` + `latest.depth`，原子写、rgb 先 json 后。
* `quench3d-editor/src/camera.rs`：后台线程轮询最新帧；egui 浮动窗口显示画面、
  2D 骨架、轮廓、可选深度；`add_lines` 把 33 个 world 点画成 35 根骨 + 关节小球，
  叠加到 3D 视口。停更 2 秒自动消失。
* 协议版本 `quench3d.mocap/1`；桌面端读文件，Web 端空实现（浏览器不走这条路）。

**为什么用文件而不是 socket/stdout**：Python 进程可以独立起停/崩溃，不影响编辑器；
一帧目录可以直接拷走做回归；代价是最多约一帧的轮询延迟。编辑器只读，不修改任何
摄像头数据。

**边界（诚实）**：

* 单帧、单人；MediaPipe Pose 不是多人体跟踪。
* 3D 世界点是 MediaPipe 的近似，不是精确骨骼角；没有 SMPL/SMPL-X 体型/蒙皮。
* 单目深度是**相对逆深度**，每帧归一化，不是米制；不能直接用于物理/碰撞。
* 固定放在 `(2.2, 1.0, 0.0)`，没有放置/缩放 UI；没有时间同步/IMU/多相机。
* 没有点云/TSDF/网格重建；下一步才是深度抬升与融合。
* 只能桌面 Linux 采集；Web 端是空实现，浏览器摄像头需要另做 getUserMedia 管线。

**验证**：

* Rust：`cargo test -p quench3d-editor --lib camera`——协议解析、rgb 长度校验、
  完整帧读入、坐标翻转、35 根骨 + 33 个关节的线段数量、低可见度不画。
* Python：`python -m unittest discover -s tools/camera_mocap -p 'test_*.py'`——
  深度归一化、NaN、轮廓提取、关键点连接表。
* 没有摄像头时用 `--source synthetic` 生成 2D 骨架/轮廓/假深度，验证端到端协议。


---

## 25. 第 68 轮：输入/网格/阴影小修 + 正向验证入口

**做了什么**：

* 网格管线真正写深度（`grid_wgsl` 的 `frag_depth` 不再被管线丢弃）；地面下方的调试
  线段会被遮住。
* 编辑器快捷键只在**按下事件**（非 auto-repeat）里处理一次，`Game::update` 不再重复；
  F3/F4 不再"开→关"抵消。
* 单级阴影纹素吸附改用 `RendererOptions.shadow_map_size`（实际纹理尺寸），与 PCF 一致。
* `render` 报告新增 `camera_name`（含 `--camera`、`default` 别名、每张 `render.outputs[].camera`）。
* 新增 `ui_smoke`（整块 `ui::panels` 真跑一帧）与 `tools/probe_web_ui.sh`（CDP 按键 +
  截图 + `<body data-quench3d-ui>` 状态双判据），以及 `tools/verify_all.sh` /
  `docs/feature_verification.md`。

**验证**：

* `CARGO_NET_OFFLINE=true tools/gate.sh`：**758** 个测试，0 失败；clippy 原生/wasm、doc、
  wasm 构建、`check_web.mjs` 全绿。
* `tools/probe_web_ui.sh`：暂停态按 F3/F4/R/F6/F7/F8/P，UI 状态必须变化；有几何的项截图也必须变化
  （F3=`grid`、F4=`shadows` 且 `shadow_instances` 7→0、R=`gizmo` Translate→Scale、
  F6=`bounds`、F7=`query`、P=`paused` true→false）；F5 只断言 `contacts`，Tab 断言
  `camera` Orbit→Fly。
* `tools/probe_webgl2.sh`：第一帧 + 遮罩收起 + 无 panic/`INDEPENDENT_BLEND`/Cube 启发式。

**仍未做**：morph+skin 的 GPU 路径（纯蒙皮已 GPU 化）、离散事件驱动的动画状态机、
自动 LOD/遮挡剔除、音频/脚本层、Web 摄像头、单帧 golden 基线、真机 GPU 矩阵。清单与命令见 `docs/feature_verification.md` §6。

---

## 26. 第 69 轮：单帧黄金图回归（`render --golden`）

**做了什么**：

* `render --golden <参考.png> [--golden-tolerance 0..255]`：单张渲染后逐像素比对
  RGBA8（参考图允许 RGB8），报告 `golden.matched / changed_pixels / total_pixels /
  max_channel_diff / tolerance`；超容差退出码 1，并保留本次 PNG 供直接看差异。
* 拒绝在碰 GPU 前：`--golden-tolerance` 单独出现、`--dry-run` + `--golden`、
  `render.outputs` 多输出 + `--golden`。
* 参考图缺失/尺寸不一致/色型不支持/位深不是 8 位都给出具体原因，不猜转换。

**验证**：`cargo test -p quench3d-cli --test cli_e2e golden_png_comparison`——
同场景重渲 0 差异、换材质颜色报 `golden_mismatch`、缺图退出 1、三种参数错误退出 2。

**边界**：仓库不提交跨平台"官方参考图"；不同 GPU/驱动/抗锯齿下的基线要各自生成。
逐像素比对是**严格**的（默认容差 0），对有意改画面需要更新参考图或显式放宽容差。
