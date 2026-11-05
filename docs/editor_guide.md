# 编辑器与 Web 使用指南

[返回项目入口](../README.md)。本页包含详细操作与接口说明；机器可读参数以 `quench3d schema --json` 为准。

## 日常操作与保存

下载版解压后运行 `quench3d-editor`（Windows 为 `.exe`），无需 Rust。源码构建方法在后续章节。

- 工具栏提供六个标准视角、立体视角、透视／正交和聚焦选中按钮。
- 左右侧栏拖动内边界调宽，顶部“场景栏／右侧栏”开关和面板收起按钮控制展开状态；本地布局保存这些选择。
- “视图 → 坐标轴”默认关闭，菜单或 AI 修改后记住本机偏好；它不属于场景物体。物体的变换手柄和选中框是独立辅助显示。
- AI 入口在右侧“AI 助手”；配置和实际工具范围见[AI 助手说明](ai_agent.md)。
- 为已有轨道的选中天体勾选“显示轨道线”，或让 AI 调用 `set_orbit_path`。轨道圈跟随移动中心，不增加物体。
- 使用 AI 的 `set_environment` 切换宇宙／普通环境，光照面板也可开启宇宙星空。宇宙环境隐藏内置地面的画面，保留其物理碰撞。
- 保存 JSON 保留场景、材质、轨道、标签和环境；聊天记录、模型密钥、面板布局属于独立本机数据。打开场景会替换当前场景。撤销恢复场景编辑，不回滚完整仿真速度与接触缓存。
- 旧版本曾将实例发光倍率误写成材质发光强度；新版保存已修复，但旧文件中已经写入的异常发光需要手动校正，不能与有意设置的发光自动区分。

## 1. 快速开始（桌面端）

需要 Rust 1.90+（根 `Cargo.toml` 的 `rust-version`：由依赖图自己声明的 MSRV 推出，
下面几个依赖是决定项——`wgpu` 1.87、`image` 1.88、`ordered-float` 1.90；
源码与 CI 通过仓库工具链文件固定使用 Rust 1.99.0）。
**例外是编辑器**：`quench3d-editor` 单独声明 `rust-version = "1.95"`，因为 egui 0.37
要求 1.95，而 0.37 是唯一同时满足「MSRV 可用」与「依赖 `wgpu 30`」的版本
（0.35 还停在 `wgpu 29`，两个 wgpu 类型不兼容）。

```bash
cd quench3d

# 最小示例：一个自转的方块
cargo run --release --example hello_cube

# 物理沙盒：堆叠箱子 / 斜面 / 球体，空格发射小球，可射线拾取
cargo run --release --example physics_playground

# 3D 模型查看器：加载 glTF/GLB/OBJ（含 MTL）/STL，空格丢球验证模型碰撞体
cargo run --release --example model_viewer
cargo run --release --example model_viewer -- 你的模型.glb

# 无窗口集成验证：物理 + 场景 + 渲染跑 240 步，输出前后两张 PNG
cargo run --release --example headless

# 编辑器（桌面应用；同一套界面在浏览器里就是 Web 客户端，见 §2）
cargo run --release -p quench3d-editor
cargo run --release -p quench3d-editor -- 场景.json   # 启动即打开一个场景
```

> 开发期建议直接用 `--release`：debug 下 CPU 侧物理与场景同步会成为瓶颈
> （`Cargo.toml` 里已经把 `profile.dev` 设为 `opt-level = 1`、依赖设为 3）。

### 1.1 编辑器：桌面应用 = Web 客户端

`quench3d-editor` 是一个**手工操作界面**：不用写代码就能摆场景、调参数、跑物理、
存成场景文件。它和 CLI / MCP / SDK 是同一套引擎能力的第四个入口，
而且桌面与浏览器**跑的是同一套界面代码**（egui），不是两个实现。

| 区域 | 内容 |
| --- | --- |
| 顶部菜单栏 | **文件**（打开/保存/另存为/导出 glTF/GLB/截图）、**编辑**（撤销/重做/历史/复制/删除/清空）、**添加**（图元）、**变换**（移动/旋转/缩放、世界/局部、轴心、3D 光标）、**播放**（播放/暂停/单步/重置/唤醒；回放在常用工具条）、**视图**（网格/阴影/自动 LOD/物理关节/**场景探针（B）**/**选中包围盒 / 悬停高亮（H）**/坐标轴/灯光 gizmo/相机模式/摄像头）、**渲染**（级联/最大绘制距离/泛光/TAA/相机运动模糊（M）/FXAA/SSAO/DoF/水）、**布局**（左右栏宽度）、**帮助** |
| 常用工具条 | 播放/暂停、单步、**录制回放 / 播放回放 / 时间轴**、重置；**↶ 撤销 / ↷ 重做**；变换手柄模式（移动/旋转/缩放）+ 世界/局部；网格/阴影/关节开关；相机模式 |
| 右侧「相机与画中画」 | 轨道/飞行相机、相机防穿墙、命名视角（◀/▶ 或下拉）、画中画相机/角落/大小/分辨率/刷新间隔、透视/正交、视场角/近远裁剪面；面板整体可滚动 |
| 右侧「渲染 → 物理关节」 | 关节名/类型/当前角或位移/限位/马达力矩；铰链与滑块可改伺服目标并写回底稿；可用选中的两个刚体新建无马达铰链；可删除关节 |
| 左侧场景树 | 物体列表（点选；**Shift/Ctrl 点选多选**，再次点选取消）、`⌖` 聚焦到物体；多选时标题显示数量；**父子层级按缩进显示**（拖父物体时子物体跟着走）；导入模型（glTF/OBJ）展开出的子网格按 `模型 #i · 路径` **分组**显示，标题上有"整组聚焦" |
| 中央视口 | 3D 画面本身；**左键拖 = 环绕**、右键拖 = 平移、滚轮 = 缩放；单击拾取物体（逐三角形射线，包围盒只做粗筛；**Shift/Ctrl 单击加选/减选**）；**选中物体身上有变换手柄**（拖轴箭头/圆环/方块改位置/朝向/缩放；多选时绕组中心成组变换） |
| 右侧属性面板 | **光照（场景级：太阳方向/强度、投影、阴影强度/覆盖半径、环境光、曝光、天空与天顶/地平线颜色、雾、网格）**、**模型引用（位置/旋转/缩放，改完当场生效）**、名字、可见性、**父物体（下拉选择，改父不跳位）**、变换（位置/旋转/缩放数值框；有父物体时是**局部**量，并同时显示世界位置）、外观（颜色乘子、金属度/粗糙度覆盖、自发光）、物理（类型 动态/静态/视觉、质量/摩擦/弹性、停下、踢一脚） |
| 底部状态栏 | 帧率、绘制统计、当前文件名与操作反馈 |

* **多选**：场景树或视口里 `Shift`/`Ctrl` 点选加选/减选；多选时 gizmo 轴心是选中物体
  世界位置的平均值，拖动手柄对**整组**做平移/绕中心旋转/绕中心缩放，一次拖拽仍只算
  一步历史；「复制」「删除」也按整组执行。`Shift`/`Ctrl` + 左键拖动可**框选**（按物体
  屏幕中心命中，追加到现有选择）。边界：不做逐三角形/包围盒相交，也不做"框内可见"
  的遮挡判断。父物体与子物体同时选中时**只动最上层被选中的祖先**，子物体跟着层级走
  （不会一秒走两倍）；同层的其它选中物体仍按组变换。
* **变换手柄（gizmo）**：`W`/`E`/`R` 切移动/旋转/缩放，`L` 切世界/局部坐标系；
  手柄**屏幕定长 88 px**（贴脸和远处一样大），抓取容差 10 px 且**命中优先于物体拾取**
  （被墙挡住的手柄也能抓）；`Ctrl` 吸附（平移 0.25 / 旋转 15° / 缩放 0.1，作用在整段拖拽的累计增量上）、
  `Shift` 精细 ×0.1；拖拽中 `Esc` 取消并还原。一次拖拽只留**一步**历史。
* **撤销 / 重做**：`Ctrl+Z` / `Ctrl+Shift+Z` / `Ctrl+Y`。整场景快照式（上限 128 步），
  拖拽与滑杆整段算一步，**撤销不改变你的视角**，撤销后的场景与快照逐字节相同。
  边界：恢复场景内容（物体/变换/材质/刚体参数/灯光），不恢复"物理跑到哪一步"（速度、睡眠）。
* **崩溃恢复自动保存**：编辑后每 30 秒把当前场景写到
  `$XDG_STATE_HOME/quench3d/autosave.scene.json`（原子 rename；正常退出会删掉它）；
  Web 端写 `localStorage["quench3d.autosave.scene.v1"]`（刷新/崩溃后仍在）。
  只有**编辑过**的场景才写（刚打开的默认场景不写，所以不会每次刷新都弹恢复提示）。
  下次启动工具栏出现「发现自动保存 / 恢复 / 丢弃」（快捷键 `F9` / `F10`）；
  恢复只装进引擎，不覆盖原场景文件（Web 端也不自动下载）。Web 的间隔同样是 30 秒，
  写入失败（配额满/隐私模式禁用 localStorage）只记日志并延后重试，不弹窗打断编辑。
* **`F` 聚焦选中物体**（未选中时聚焦整个场景）。
* **导入的模型（glTF/OBJ）可整体编辑**：模型的每个子网格在引擎里是一个普通物体，
  编辑器靠"物体↔模型"映射（`runner::ModelInstance`，装载/撤销/重做时刷新）把它们当成一组——
  在视口里拖其中任何一个 = **整组联动**，新变换当场写回场景文件的 `models[i]`；
  右侧"模型"面板改位置/旋转/缩放同样**当场生效**（不再是"重开后生效"）。
  边界：一个模型只有一个变换，不能单独挪某个子网格；缩放对模型强制等比
  （场景格式里 `models[i].scale` 是一个数）。Z-up 资产（Blender/OBJ 常见）写
  `models[i].up_axis = "z"`：引擎在用户旋转**之前**绕 X 轴 -90° 转成 Y-up，
  位置/缩放不变，碰撞体与 LOD 用同一变换。
* **骨骼蒙皮（GPU，纯蒙皮）**：带 glTF `skins` / `JOINTS_0` / `WEIGHTS_0` 的角色把
  关节下标/权重放进顶点，顶点着色器按关节矩阵变形；场景每帧只上传关节调色板
  （最多 128 个矩阵），不再重算/重传整张顶点缓冲。场景里用 `models[].animation`
  选一条 clip；`Scene::gpu_skinning = false` 可退回 CPU 对照。
  边界：每顶点最多 4 关节；morph+skin 网格的 morph 位移仍在 CPU 叠加（每帧上传顶点），
  关节矩阵在 GPU 乘（只乘一次）；
  IK 见下；`info` 会列出 `skins` / `skinned_meshes`。
* **morph target（静态权重 + 权重动画）**：带 glTF `targets` 的网格解析逐顶点
  `POSITION/NORMAL/TANGENT` 位移；场景里用 `models[].morph_weights` 覆盖权重
  （0~1，缺项按 0）。glTF 的 `weights` 动画轨道会随 `models[].animation` 自动采样，
  按节点应用到对应网格；蒙皮模型按"先 morph、再乘关节矩阵"的顺序变形，静态模型
  在装载后立即写回 GPU 顶点缓冲。手工 `morph_weights` 优先于动画轨道。
  边界：双 clip 混合会按 `blend_weight` 逐 target 插值两条 clip 的 morph 权重；
  编辑器「动画」面板可勾选手动覆盖并拖动每个 target 的滑杆；`models[].collide` 的碰撞体仍是绑定姿态。`info` 会报 `morph_targets`，动画报告
  会报 `animation.morph_tracks`。
* **根运动（位移 + 偏航）**：`models[].animation.root_motion` 写一个 glTF 节点名，
  每帧把该节点相对**设置时**位置的三维位移与绕 Y 偏航从节点世界矩阵里扣掉、再整体加到
  模型物体上，所以蒙皮/刚性片段的可见结果不变，但物体真的往前/往上走了、也真的转向了
  （摄像机跟随、选择框、后续物理/导出都看到同一个位置）。要求单 clip、显式
  `"loop": false`、无 `blend`/`animator`/IK；只取位移与偏航，俯仰/滚转留在模型内部。边界：节点名不存在时报告
  `models[].animation.root_motion.found = false` 并保持原地；不提取转身/跳跃的根旋转，
  也不做根运动重定向/速度曲线。
* **参数驱动的动画状态机**：场景里用 `models[].animator` 声明 `states`、`parameters`
  与 `transitions`；当前状态停留 `after` 秒后切到目标状态，或写 `on_finished: true`
  让非循环 clip **播完**时切，`fade` 秒内从旧 clip 淡入新 clip（0 = 立即切），淡入完成后
  新 clip 提为主 clip。过渡可带 `conditions`（引用 `parameters`，支持
  `greater/greater_or_equal/less/less_or_equal/equal/not_equal`），全部满足且满足时间
  条件才切；宿主用 `Scene::set_animator_parameter(index, name, value)` 每帧写角色速度/
  是否着地等输入。参数是浮点，布尔用 0/1 表达。过渡还可写 `on_event`：宿主调用
  `Scene::fire_animator_event(index, name)` 发离散事件（跳一下/攻击命中），下一帧帧首
  **立即**过渡，事件过渡优先于同状态到期的时间过渡；需要 `conditions` 时同样要满足，
  条件不满足则事件被丢弃。状态机事件进入**有界 FIFO**（8 条，满时丢最旧），
  每帧帧首消费一条；可以在当前动作结束前先按下一招。`event_window`（0~10 秒，缺省 0 =
  不过期）让入队后超过窗口还没被接住的事件自动丢弃——淡入期间输入不被消费，长过渡后
  不会突然生效。clip 的动画事件标记另有队列
  （`models[].events` + `Game::on_animation_event`；标记可带 `payload` 字符串，
  原样随 `FiredAnimationEvent` 交给宿主，不解析、不执行）。输入有有界 FIFO 与
  `event_window` 过期，但仍不是**按时间戳匹配/缓冲重放**，也没有参数曲线。
  和 `models[].animation` 互斥。报告
  `models[].animator.{found,states,transitions,missing}`。
* **IBL / HDRI 环境光**：`lighting.environment_map` 指向等距柱状 `.hdr` / `.png` / `.jpg`，
  装载时解码成线性 RGB 并预滤波成 6 面 cubemap（64×64、7 级 mip）；着色器按粗糙度取
  对应 mip，金属/光滑表面能看到环境反射，漫反射也吃这张图的平均照度。可调
  `environment_intensity` 与 `environment_rotation_deg`。不写这个字段 = 继续用解析式天空，
  **旧场景像素不变**。边界：只支持标准 `-Y h +X w` 朝向；预滤波是锥形平均近似，不是
  精确 PMREM/GGX；没有从 HDRI 自动提取太阳/局部光。
* **手工 LOD（远处换粗模）**：`models[].lods` 给静态模型挂替换层：
  `[{ "path": "tree_low.glb", "min_distance": 25 }]`。基础模型用到第一个距离，之后每层
  用到下一层距离；所有层共用同一个位置/朝向/缩放。引擎每帧按相机距离切可见层级，
  阈值旁有 **10% 滞回**，不会在边界来回弹。边界：只支持**静态模型**（不能同时写
  `animation` / `animator` / `ik`），碰撞仍用基础模型。另有**渲染器级自动 LOD**
  （`render.auto_lod`）：高面数、非蒙皮网格在上传时用顶点聚类生成 1/2、1/4 两档，
  按批次包围盒到相机的距离切换；蒙皮/动画网格不自动生成。
* **两骨 IK（脚/手目标）**：场景里用 `models[].ik` 给角色加链：
  `{ "root": "LeftUpLeg", "mid": "LeftLeg", "tip": "LeftFoot", "target": [x,y,z], "pole": [0,0,1] }`。
  按 glTF 节点名引用，目标与极向量都在**模型局部空间**；每帧采样动画后解两骨三角形，
  只改 root/mid 的朝向，骨骼长度不变，`pole` 决定膝盖往哪边弯。求解在蒙皮之前，
  所以 CPU 蒙皮、GPU 关节调色板、刚性片段和 `F8` 骨骼叠加层看到同一份关节矩阵。
  边界：只支持两骨（大腿-小腿 / 上臂-前臂）；目标超出可达范围时夹到最长/最短，
  不拉伸；必须有 `animation` 或 `animator` 提供初始关节姿态。可选的 `weight`（0~1）
  在 FK 姿态与全 IK 解之间 slerp 混合：0 = 完全不动，1 = 完全到达目标，中间值骨骼
  长度仍不变但末端不保证精确到点；缺省 1。
* **动画面板**：右侧「动画」列出场景里每个播放实例的 clip、时长、倍速、循环/播完
  状态和状态机当前状态；**倍速/循环可改并写回 `models[i].animation`（或状态机当前
  state）**，可撤销，存盘生效。拖动「时间」滑杆时**暂停也立刻**重算位姿与蒙皮/morph
  顶点（`Scene::refresh_animations_with_renderer`），但时间本身只改当前会话。
  普通动画还可从资产里选择 clip（写回 `models[i].animation.name`）；状态机的片段由
  `states` 管，不能在这里单换。状态机的每条过渡可直接改等待秒数、淡入秒数和
  「播完触发」（写回 `transitions[i]`）。状态机有参数时，面板会列出参数滑杆；拖动后
  立刻按新值判一次过渡（暂停也生效），但只改当前会话、不写场景默认值。有 `on_event`
  过渡时，面板会为每个事件画「发送 xxx」按钮，点击后同样以 `dt=0` 当场判一次。
  clip 的**事件标记**也能在面板里增删、改名字/时间/作用 clip/负载（写回 `models[i].events`，
  会当场更新场景队列）；「在播放头加事件」用当前时间新建一条。模型有 `ik` 时，每条链的
  目标 XYZ 与 IK/FK 权重也可直接改并写回 `models[i].ik[j].target/weight`，视口里的蒙皮和
  `F8` 骨架当场跟着动。
  边界：仍不是完整时间轴（不能加参数曲线/多轨混合，不能增删 IK 链）。
* **CUBICSPLINE 真值**：glTF 动画的 TRS 轨道按 Hermite 基函数使用入/出切线
  （旋转在分量求值后归一化）；解析器保留三段 `[入切线, 值, 出切线]`，不再退化线性。
  morph 权重轨道的 CUBICSPLINE 仍只取关键帧值（按线性采样）。
* **父子层级**：场景树按缩进显示父子关系，属性面板里用"父物体"下拉改父（也可以提回根物体）。
  改父与**拖动父物体**都会**保持子物体的世界位姿**（改父不跳位、删父不跳位），
  一次改动只留一步历史。边界：子物体继承父物体的**位置与朝向，但不继承缩放** ——
  这个格式里 `scale` 就是图元尺寸（`shape.size` 折算而来），乘法继承会让"挂在大地板下的树"
  被压成一块饼；带刚体的物体不能有父物体（它的位姿由物理决定，校验器与引擎都会拒绝）。
* **相机**：环绕相机由编辑器接管（`Engine::auto_camera = false`），`Tab` 仍可切到自由飞行；
  其余引擎内置快捷键照旧（`R` 对准场景、`F3`/`F4` 网格/阴影、`F5`~`F8` 与 `F11` 调试、`T` 循环色调映射、`K` 3D 光标、`O` 自动 LOD、`M` 相机运动模糊、`P` 暂停、`Esc` 退出）；
  **例外**：轨道相机下 `W`/`E`/`R`/`L` 归变换手柄（飞行相机下它们仍归相机与引擎）；
  在输入框里打字时这些快捷键自动让位（`egui_wants_keyboard_input`）。
* **一帧是怎么拼出来的**：先跑界面（egui）→ 拿到这一帧的交互 → `Engine::tick` 推进物理与提交绘制 →
  3D 画进交换链纹理 → egui 在同一张纹理上叠一层（`LoadOp::Load`）→ 呈现。
  所以没有第二条渲染循环，也没有两份场景状态。
* **场景文件与 CLI 完全共用**（`quench3d.scene`，见 §6）：
  ```bash
  quench3d scene --prompt "三层堆叠的箱子" -o 场景.json   # 或手写 / 由别处生成
  cargo run --release -p quench3d-editor -- 场景.json      # 手工调整
  quench3d movie 场景.json --out 演示.gif                  # 渲成 GIF/APNG 或帧序列（见 §5）
  ```
  * 桌面端「打开/保存」走**路径输入框 + 真实文件系统**（不引入原生对话框依赖）；
  * Web 端走浏览器：页面提供文件选择与下载（`web/index.html` 里的
    `__quench3d_pick_scene_file` 与 wasm 导出的 `on_scene_file`），wasm 侧只收字符串。
* **中文界面**：egui 自带字体**一个中文字形都没有**（界面会变成一排豆腐块），
  所以编辑器内嵌了一份中文子集字体（`crates/quench3d-editor/assets/ui-font.ttf`，563 KB，
  源自 Droid Sans Fallback，Apache-2.0；字符集 = 界面文本 + GB2312 一级字 + 标点，共 3899 字），
  挂在字体栈末尾作为兜底。启动时会打一行日志把它变成可观测事实：
  `编辑器字体：中文子集可用 = true`。

## 2. 快速开始（Web 端）

需要 AI 时，从源码根目录运行 `python3 tools/agent_server.py`，打开 `http://127.0.0.1:8765`，再配置模型，详见[AI 连接说明](ai_agent.md#connect)。GitHub 的静态 Web 归档只包含页面资源，不能单独提供 AI 代理。下面的静态托管与构建步骤用于手工编辑或开发验证。

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129   # 必须与 wasm-bindgen crate 版本一致

# 可复现构建：仓库内 rust-toolchain.toml 钉 1.99.0；依赖许可/SBOM 用
# python3 tools/license_audit.py --check（CI 与门禁都会跑）。
tools/build_web.sh web_client                      # 默认就是它：编辑器（Web 客户端）
python3 -m http.server 8000 --directory web
# 打开 http://localhost:8000/
```

> **线上已经部署好了**：<https://gh503.github.io/quench3d/> —— 每次 push 到 `main`
> 由 `.github/workflows/pages.yml` 重新构建并发布（同一个 `tools/build_web.sh web_client`）。
> 首次打开需要下载 Wasm 与字体等资源；实际体积随版本变化。Web AI 应使用下述本地代理，而非仅静态托管。

页面里就是 **§1.1 那个编辑器**：工具栏、场景树、属性面板全在画布上由引擎自己画出来，
页面只负责三件事——给一张画布、在 `start()` 之前探好后端、把用户选中的场景文件读成文本交给 wasm。
（`physics_playground` 这个示例仍然可以构建成 Web 包：`tools/build_web.sh physics_playground`，
但 `web/index.html` 现在服务的是编辑器。）

> **时间 API 在 wasm 上要小心**：`std::time::Instant::now()` 在
> `wasm32-unknown-unknown` 上是**直接 panic**（"time not implemented on this platform"），
> 而 `cargo check --target wasm32` 看不出来——只会在浏览器里第一次跑到就白屏。
> 引擎的所有取时间都走 [`clock`](../crates/quench3d/src/clock.rs)（原生 `Instant`，
> wasm `performance.now()`，取不到就退 `Date.now()`）；物理的阶段计时走
> 可插拔时钟 `set_time_source()`（wasm 上由门面装入，不装也只是计时为 0，不 panic）。
> `tools/check_web.mjs` 会实例化 wasm 并真的调用示例导出的 `clock_probe()`
> 读一次时钟，还会检查产物是不是比源码旧（防止拿旧包自欺欺人）。
>
> **本机的工具链在仓库里，不在 `~/.rustup`**：`infra-dev/.rustup` 装了带 wasm32
> 目标的工具链，`infra-dev/.cargo-home` 里放了 `wasm-bindgen 0.2.129`。
> 系统默认的 `~/.rustup` 里**没有** wasm32 目标，所以直接跑脚本会报
> `can't find crate for 'core'` / `the wasm32-unknown-unknown target may not be installed`。
> `tools/build_web.sh` 现在会自己检测并切到仓库内那套（只在当前工具链确实缺
> wasm32 时才切，不覆盖你自己配好的 `RUSTUP_HOME`/`CARGO_HOME`），也会去
> `$CARGO_HOME/bin` 找 `wasm-bindgen`。要手动复现同样的环境：
>
> ```bash
> export RUSTUP_HOME=<infra-dev>/.rustup
> export CARGO_HOME=<infra-dev>/.cargo-home
> ```

产物 `web/pkg/quench3d_web_bg.wasm` 约 **14 MB**（本轮 `tools/gate.sh` 实测 14010301 字节；release、
`lto=thin`、`opt-level=3`、已 strip 调试信息；胶水 `quench3d_web.js` 约 144 KB）。
构成大致是：`wgpu` 的 WebGPU + WebGL2 两条后端、引擎与物理、glTF/OBJ 与图片解码
（自检探针要用）、egui 与内嵌的中文子集字体。
比物理示例包（5.4 MB）大了一倍，多出来的主要就是 egui 这一套 + 字体 + 模型解析。
真要压体积：`tools/package_release.sh` 已为 wasm 生成 Brotli `.br`（本机
14,010,301 → 3,087,644 字节），静态服务器对 `Accept-Encoding: br` 返回 `.br` 即可；
还能加 `wasm-opt -Oz`、把自检探针从发布包里去掉，或者只保留必需特性。
浏览器里 `Esc` 退出、`Tab` 切换相机、`F3`~`F8` 与 `F11` 是调试开关、`T` 循环色调映射、`K` 3D 光标、`O` 自动 LOD，和桌面端完全一致。

#### 后端怎么选（WebGPU → WebGL2 回退）

**先探再定**：页面在 `start()` 之前用 `navigator.gpu.requestAdapter()` 探一次
（**不碰画布**），把结论写进 `globalThis.__quench3d_backend`，引擎据此排候选后端：

| 探测结果 | 候选 | 说明 |
|---|---|---|
| 有 WebGPU 适配器 | WebGPU 优先，WebGL2 兜底 | 正常情况 |
| 没有 `navigator.gpu`，或 `requestAdapter()` 返回 null | **只**试 WebGL2 | `navigator.gpu` 存在 ≠ 有适配器（Linux Chrome 没开 Vulkan、软件光栅环境都这样） |
| 网址带 `?backend=gl` / `?backend=webgpu` | 按指定的来 | 强制后端，排查用 |

为什么不能"交给 wgpu 两个一起试"（这是踩过的坑，两道闸门都有回归测试）：
wgpu 只要看到请求里带 `BROWSER_WEBGPU` 且 `navigator.gpu` 存在，就**只**建 WebGPU
上下文、连 WebGL2 实例都不建，于是报 `No available adapters`；更要命的是**一旦对着
这张画布要过 `webgpu` 上下文，同一张画布的 `getContext("webgl2")` 会返回 null**
（一张画布只能有一种上下文，且不可逆）——事后回退在同一条路上根本走不通。

`tools/check_web.mjs` 会静态挡住两件事：wasm 里必须真的含 WebGL2 后端、
页面的后端探测必须发生在 `start();` 之前。**真正在浏览器里**的那一层用
[`tools/probe_webgl2.sh`](../tools/probe_webgl2.sh)（headless Chrome + SwiftShader，
`--enable-unsafe-swiftshader --use-angle=swiftshader`，缺省 `?backend=gl`）跑：
它在日志里要求「编辑器第一帧已渲染」且页面遮罩（`#overlay`）被收掉，
出现 `panicked at` 就判红。闸门是**遮罩**：Rust 侧只在第一帧真的画出来之后才收它，
所以"启动成功但白屏"过不了这一关。

> ⚠️ **第 57 轮的教训（为什么必须有一条真浏览器的探针）**：GL/WebGL2 是**另一套语言后端**
> （naga → GLSL）。`post.wgsl` 的 SSAO 对深度纹理用 `textureLoad` 时，naga 的 GLSL 后端
> 直接拒绝（`WGSL \`textureLoad\` from depth textures is not supported in GLSL`），
> 而 wasm 上管线校验错误是**致命**的 —— **编辑器启动即 panic**，本地门禁（Vulkan）与
> Linux CI 却全绿。现在有两层覆盖：这条浏览器探针（真机行为）
> + 一条不需要显卡的 naga GLSL 翻译守卫（可编译性）。
> GL 上的能力矩阵（SSAO 按能力关闭、MSAA > 1 的既有问题）见
> [docs/production_readiness.md](production_readiness.md) §12.7。

### 2.1 Web 端加载模型：走字节，不走路径

wasm 里没有文件系统，`load_model("assets/chair.glb")` 只会得到一句
"找不到资源，已尝试: chair.glb, assets/chair.glb, …"——对着这句话没人知道该怎么办。
所以 Web 端走 [`load_model_from_bytes`](../crates/quench3d/src/assets.rs)：

```rust
// 在你自己的 wasm 侧 Rust 代码里（例如一个 #[wasm_bindgen] 包装函数）
#[wasm_bindgen]
pub fn load_chair(bytes: &[u8]) -> Result<(), JsValue> {
    let model = quench3d::assets::load_model_from_bytes("chair.glb", bytes)?; // 由 JS fetch 得到
    for (mesh, material) in model.upload(&mut engine.renderer, &engine.gpu) {
        engine.scene.add(mesh, material, Transform::IDENTITY);
    }
    Ok(())
}
```

* **`.glb` 是自包含的**：任何来源的字节都能直接解析（Web 推荐的格式）；
* `.gltf` 若用 `uri` 引用外部 `.bin`/贴图，内存里无从解析 → 报错并让你改用 `.glb`
  （或把资源内联成 `data:` URI）；
* `.obj` 只解析几何：`mtllib` 不会被读取（没有目录可查），材质用缺省值；
* `load_model(path)` 在 wasm 上返回 `AssetError::Unsupported`，**错误文本里直接点出
  `load_model_from_bytes`**，不是一句没用的 NotFound。

`tools/check_web.mjs` 在 Node 里把这两条路都真的跑一遍（示例导出了两个探针）：
按路径加载的报错里必须出现 `load_model_from_bytes`，从字节加载必须真的解析出
`meshes=1 triangles=1`。这是"Web 端能加载模型"的硬证据——`cargo check --target wasm32`
只能证明它编译得过。

> 代价说清楚：这两个探针把 glTF/OBJ 解析器与图片解码器链进了示例 wasm。
> 实测（同一台机器，`--release`）：**去掉探针 4.56 MB，带上探针 5.39 MB**——
> 那 0.83 MB 就是模型解析本身。真要给演示包瘦身，去掉这两个探针即可；
> 但任何真的要加载模型的 Web 应用都需要这部分代码。

## 想读代码从哪里开始

**想先知道"它离生产还差什么"**：读 [docs/overnight_report.md](overnight_report.md)（本轮做了什么、你怎么验收）
与 [docs/production_readiness.md](production_readiness.md)（逐域差距矩阵、六个场景判定、四类一票否决项）。

机械臂（robot-modkit M1）的单位、推荐增益、`joint_target`、遥测与**未完成项**见
[docs/robot_arm_sim.md](robot_arm_sim.md)。

先看 **[docs/getting_started.md](getting_started.md)**（心智模型、三条开发路线、配方表、坑清单），
再打开 **[crates/quench3d/examples/starter_game.rs](../crates/quench3d/examples/starter_game.rs)**：
一个能跑、能改、能进 CI 的完整骨架（`cargo run --release --example starter_game`）。
想确认文档里的 API 没写错：`crates/quench3d/tests/api_snippets.rs` 把文档 §3 的每段配方
都编译一遍——文档写错名字，`cargo check --all-targets` 就会先炸。

