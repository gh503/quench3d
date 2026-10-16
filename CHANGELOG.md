# 变更日志

本文件记录**用户看得见的变化**（新能力、行为变化、破坏性改动、性能与体积）。
格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循[语义化版本](https://semver.org/lang/zh-CN/)。

> 约定：每条都写"发生了什么 + 怎么验证"。没有验证方式的变化不要写进来。

## [Unreleased]

### 新增

- **编辑器接上后处理三项：`lighting.bloom` / `fxaa` / `ssao`**。以前编辑器**完全不应用**
  它们——载入一个开了泛光与 SSAO 的场景，预览里两样都没有，而 `quench3d render` 渲出来是有的；
  在编辑器里勾掉 FXAA 存盘，文件里却还开着（三项都是"沿用底稿"）。
  现在载入时用 `runner::bloom_options / fxaa_options / ssao_options`（改成 `pub`）写进
  `RendererOptions`，UI 直接改这份选项（泛光新增 开/强度/阈值 三个控件），存盘从渲染器选项写回。
  **预览与 CLI 渲染因此共用同一份映射**，不会在"默认值"上悄悄分叉。
- 往返测试 `post_processing_survives_the_round_trip_through_renderer_options`（CPU）：
  显式值的往返守恒、不写 = 三项都关、写了空节 `{}` = "就是想开"且其余取缺省。
- 已知副作用（沿用既有约定）：编辑器存盘会把 `bloom/fxaa/ssao` 三节显式写进文件，
  即使作者从未调过它们。语义不变（`enabled: false` 是显式的），但文件会变长一点。

### 新增

- **编辑器：阴影级联有了 UI（级联数 1..=4、切分系数 0..=1）**，并且载入场景时把
  `lighting.shadow_quality` **应用进** `RendererOptions`（以前编辑器完全没应用：
  载入 `cascades: 4` 的场景，预览仍是引擎默认的单级），存盘时从渲染器选项写回
  （不再"沿用底稿"）。级联映射函数 `runner::shadow_cascades` / `shadow_split_lambda`
  改成 `pub`，编辑器与 CLI **共用同一份** —— 预览与 `quench3d render` 因此不会在级联数上分叉。
- 往返测试 `shadow_quality_survives_the_round_trip_through_renderer_options`（CPU）：
  1..=4 × 4 个 λ 的"场景 → 渲染器 → 存回"守恒；不写 = 单级 + 0.5；手改 JSON 越界时
  映射函数自己再夹一次范围。
- **新发现的边界**：编辑器目前只有"阴影级联"这条链路通到渲染器选项，
  **`lighting.bloom` / `fxaa` / `ssao` 仍然不被编辑器应用**（预览与 CLI 渲染会不一致）。
  已记入 gap 列表，是下一项。

### 变更

- **删掉两个从未被读取的旗标：`--scene`、`--device`**。全仓搜索只有 `VALUE_FLAGS` 与
  "已知没写"白名单提到过它们，没有任何代码读过 —— 也就是说 `--device 1` 会被**静默忽略**。
  一个"看着能传值、其实不起作用"的旗标比没有这个旗标更坏：用户会以为问题出在别处。
  删掉之后传它们会得到"多余的参数"错误（比静默忽略诚实）。GPU 适配器选择仍然可以用
  `quench3d doctor` 查看；真要支持"指定设备"是一个独立功能（见 gap 列表），不该用一个空壳旗标占位。
- 守卫测试 `every_valued_flag_is_documented_in_help_or_schema`：规则从"在 `--help` 里 **或**
  在已知没写的白名单里"收紧为"在 `--help` 里 **或** 在 `schema` 的命令参数里"，
  **白名单已清空**。清空时发现原白名单 6 项里有 4 项（`--ray/--overlap/--mask/--max`）
  本来就写在 `schema` 的 `query` 参数里 —— 是白名单看漏了，不是文档缺失。

### 新增

- **聚光灯（spot lights）**：`lighting.spot_lights` —— 有方向的局部光，照亮一个圆锥。
  引擎侧 `SpotLight`（位置 / 方向 / 颜色 / 强度 / 半径 / 内锥角 / 外锥角），
  与点光源**共享**每帧 8 盏的局部光名额（点光源优先，超额的被忽略并在校验里给警告）。
  WGSL 里两者共用同一条 `array<PointLight, 8>`：`dir_outer.w > 0` 就是聚光灯
  （w = 外锥角余弦），内外锥之间按余弦平滑过渡。
- 校验：锥角必须落在 `(0, 90)` 度、**内锥必须小于外锥**、半径为正、方向非零；
  `--asset-path` 那种"能解析但看着不对"的坑这里也堵住了（都有明确的 hint）。
- 测试：`pack_local_lights_puts_spots_after_points_with_the_cone_intact`（纯函数：顺序、
  方向归一化、零方向兜底、角→余弦、名额耗尽时点光优先）、
  `spot_light_validation_catches_the_ways_cones_go_wrong`（缺省可用 / 四类非法值 /
  超预算警告）、`frame_uniform_layout_matches_wgsl` 更新到 64 字节/盏、FrameUniforms 1216 字节。
- **GPU 侧量化验证（本机跑的，CI 无显卡不跑）**：顶视 120 米地面、关天空与网格、
  只留一盏 9 米高的聚光灯，256×256 渲染后统计高亮像素占比：

  | 场景 | 高亮(>150)占比 | 预期（按 tan² 外锥角） |
  |---|---|---|
  | 无灯（对照） | 0.00% | 0 |
  | 8/12 度 | 0.53% | 0.50% |
  | 8/45 度 | 11.06% | 11.0% |
  | 8/12 度、斜 30 度 | 0.83% | 光斑移开且变大 |

  实测与几何预期吻合到 6% 以内，说明"角度 → 余弦 → 圆锥判断"这条链路是对的。

### 新增

- **`schema` 新增 `global_flags`**：`--json`、`--camera`、`--asset-path`、`--out`、`--width`、
  `--height`、`--base-url`、`--model`。`commands[].args` 只描述"某个命令的参数"，
  这些"对所有命令都生效"的旗标以前在 schema 里**完全不存在**——agent 只能靠 `--help` 或猜。
  守卫测试 `schema_global_flags_are_registered_and_documented` 钉死三方一致：
  `kind=value` 必须在 `VALUE_FLAGS`、每条都必须在 `--help` 里、并且 `--json/--camera/--asset-path`
  必须留在表里（避免以后再被删掉）。

### 修复

- **`--help` 补上 `--camera` 与 `--asset-path`**，并加守卫测试
  `every_valued_flag_is_in_help_or_explicitly_deferred`：每个带值旗标要么写进帮助，
  要么进"已知没写"白名单（白名单只允许变短；`--scene/--device/--ray/--overlap/--mask/--max`
  仍在里面）。`schema` 目前不描述 CLI 旗标，帮助是人和 agent 唯一能看的地方，
  之前"加了旗标忘记写帮助"没有任何东西拦得住。

### 新增

- **`--asset-path <目录>`：一次运行用的额外资源搜索路径**（gap #11 的运行半）。多个目录用
  `:` 分隔（Windows 用 `;`，与 `PATH` 一致）；相对路径以场景文件所在目录为基准，顺序排在
  `assets.search_paths` **之后**（场景文件才是这份场景的约定，旗标是这一次运行的补充）。
  只影响这一次运行，**不改场景文件**（e2e 里逐字节比对过）。旗标登记在进程级：
  命令实现内部还会自己 `from_file` 一次（多输出渲染、内部校验），逐个传参必漏一处，
  漏掉的表现是"旗标有时不生效"——这种最难查。
- `assets.extra_asset_paths` 与 `set_cli_asset_paths` 都不参与序列化，不写旗标的老场景逐字节不变。

### 新增

- **`assets.search_paths`：场景自己的资源搜索路径**（gap #11）。不写 = 老行为完全不变；
  写了就在"场景文件所在目录"之后、"当前工作目录"之前依次尝试，相对路径以场景目录为基准。
  校验会报空项（错误）与不存在的目录（警告，避免打错一个字静默失效）。
- **修掉一个真实的不一致：模型路径与贴图路径不是同一套解析规则**。
  贴图按"场景目录 → 当前工作目录 → 引擎搜索路径"；模型却只按加载器自己的规则
  （原路径、`assets/`、`quench3d/assets/`）——**不含场景目录**。后果：
  `models[].path: "chair.glb"` 这种最自然的写法，在工作目录不等于场景目录时加载失败，
  而 `validate` 用第三套规则可能还说没问题。现在贴图与模型共用同一个候选列表
  （`SceneDoc::asset_candidates` / `model_candidates`），validate 与渲染也一致。
  错误信息会列出**找过哪些位置**（含搜索路径展开后的绝对路径），而不是一句 CWD 相对路径的 IO 错误。
- 测试：`assets_search_paths_and_model_paths_use_the_same_candidates`（解析顺序、搜索路径
  展开、找不到时列出的位置、空/不存在搜索路径的诊断、不写 assets 时不出现该键）+
  e2e `scene_relative_model_paths_do_not_depend_on_the_working_directory`（**不需要显卡**，
  走物理侧的模型加载；含"模型就在场景旁边""靠 search_paths 找""找不到时的报错"三种情形）。
- **级联阴影的量化验证**（不改渲染行为）：`examples/shadow_probe` 从"只诊断自阴影与方向"
  扩成**级联 sweep**（`cascades = 1/2/3/4` × 远景/近景/深景三组位形），并给出
  **半影比例**（地面边缘像素 / 核心像素）与**推导的纹素足迹**（世界单位/纹素）。
  实测结论（写进 `docs/production_readiness.md` §12.2）：
  * 级联换来的是**覆盖**：60 米外的投射体在"单级半径 10 米"下**一个影子像素都没有**，
    2/3/4 级都有，且落点与太阳方向推导的解析位置一致（误差 0.5 / 0.6 / 0.2 px）；
  * 在"小场景 + `far = 500`"的位形下，**单级反而清晰约 33 倍**（纹素足迹 0.0273 vs 0.893
    世界单位；半影比例 0.147 vs 2.40）。手调的 28 米正交盒 + 整张 2048 贴图，
    是"必须覆盖到 500 米、每级还只分半张贴图"的级联比不了的。
    所以第 42 轮那句"近处更清晰"是**说错了方向**的一张账，这里改正。
- **级联能力回归测试** `crates/quench3d-render/tests/shadow_cascades.rs`（有 GPU 才跑，
  无 GPU 自动跳过）：单级照不到 60 米外的投射体（≤1 个像素变暗），4 级照得到（≥4 个），
  且落点落在解析位置的 2 米内。
- **级联阴影（CSM，gap #24 的第一件）**：`lighting.shadow_quality.cascades`（1~4，缺省 1）
  与 `split_lambda`（0~1，缺省 0.5）。`cascades = 1` 时**逐像素保持老行为**（还是围绕相机前方的
  单级正交盒，覆盖范围由 `radius` 决定）；2~4 时按相机视锥做 PSSM 切分，**近处细、远处粗**。
  - **CPU 侧数学单独成模块**（`quench3d-render/src/cascade.rs`）：`splits`（log/uniform 混合，含
    荒谬输入收敛）、`frustum_corners`（反投影 NDC，注意本引擎深度是 `[0,1]`）、`slice_corners`
    （沿视锥角点线性插值——视空间深度是世界位置的线性函数，所以这个插值是对的）、
    `matrices`（包围球拟合 + 按分块纹素对齐，避免相机旋转时阴影抖动）。
  - **验收判据不是"看着对"**：把某一级的 8 个角点用那一级的矩阵变过去，必须全部落在单位立方体内
    （`each_cascade_box_contains_its_own_slice`，还特意把相机**转斜**，正对时会掩盖旋转相关的错误）。
    另有切分单调性/端点、角点与投影一致、切段线性等断言。
  - **渲染侧**：N 级画进同一张深度图的 **2x2 图集**（一级一个视口 + 一级一个帧常量 buffer——
    同一个 buffer 一帧内写多次只有最后一次生效，所以一级一个最省事）；主 pass 的 uniform 里带
    4 个矩阵 + 切分点 + 级联数，着色器按**视空间深度**选级并把 uv 映射到分块（越界判据用分块内 uv，
    否则 PCF 会跨界采到隔壁那一级的深度、出现接缝）。阴影投射体的剔除改成"落在任意一级里就画"。
  - **代价写清楚**：N 级 = 画 N 遍阴影 pass，且每级只分到半张贴图（有效分辨率 = `贴图边长 / 2`）；
    `cascades > 1` 时 `shadow_quality.radius` 不再起作用（覆盖范围由视锥决定）。
  - 校验：`cascades` 不在 1~4、`split_lambda` 不在 0~1 → 错误（e2e 覆盖 0/5/1.5 三种坏值）。
  - **诚实边界**：GPU 侧只验证到"字段接通 + 画面确实变化 + 两种模式都真有影子"（e2e，有卡时跑）；
    **"近处确实更清晰"没有独立量化**（用远处的投射体做过一次对比，但无天空背景占了 42% 像素、
    统计量分不出来，没得出结论）。点光源/聚光灯阴影仍未做。
- **模型缓存（CPU 侧，gap #27）**：`quench3d::assets::ModelCache` —— 进程内、以**规范化路径**为 key、
  按 `(文件长度, mtime)` 失效（文件改了就重新解析，不拿旧几何骗人）、256 MiB 估算预算 + LRU 淘汰、
  单个模型超预算时"照常返回但不留缓存"。CLI 的三个加载点（建碰撞体 / 上屏 / `info`）都走它，
  为此把 `Engine::load_model` 拆出 `Engine::add_model`（先加载、再上屏）。
  - **可达的观测口径**：`simulate` 报告新增 `model_cache{hits,misses,evictions,entries,bytes,budget}`；
    **场景里没有模型时该字段不出现**，老报告逐字节不变。e2e：同一路径写两条 `models[]`
    （"两把相同的椅子"）= **1 次解析 + 1 次命中 + 1 个条目**；两个不同文件 = 2 次解析 0 命中；
    没模型的场景没有这个字段。
  - 单测（`assets::cache_tests`）：命中/未命中、**文件改动后失效**、预算 0 不留东西、预算只够一个时
    LRU 淘汰且不超预算、`clear()` 归零。**文件失效只能在进程内观测**——每个 CLI 调用都是新进程，
    所以这条由单测钉住，e2e 里写明了原因。
  - **没做**：**显存释放**（GPU 缓冲仍只增不减，反复 `load_model` 照样涨显存）；缓存不跨进程。
- **动画混合权重 `animations[].weight`（gap #23 第 2 件：clip 混合）**：`weight`（0~1，缺省 1）
  的语义是"**从当前姿态**朝这条动画的姿态走多远"——0 = 完全不生效，1 = 完全生效（与第 38/39 轮逐位一致），
  中间值就是混合比例。同一个物体挂两条动画时，后一条的权重天然给出 `lerp(前一条的姿态, 后一条的姿态, w)`：
  两条 clip 的混合不需要额外概念。纯函数 `animation::blend_position` / `blend_rotation`（位置 lerp、朝向 slerp，
  权重夹到 [0,1]）带端点/中点断言。
  - **实现上踩到的坑（已修，并写进注释）**：不能"边算边写"——`body.transform()` 读到的是**上一步结束时**的
    姿态，同一帧里刚 `set_transform` 过的值读不回来。于是"先 A（权重 1）再 B（权重 0.5）"里，B 会拿 A **之前**
    的姿态做起点：应当得 3，实测得到 1。修法是**先在内存里把同一刚体的多条动画按顺序叠完，再一次性写世界**。
  - 校验：`weight` 必须在 0~1（超范围 → 错误）。e2e：两条 clip 权重 0.5 → y 落在中点 3；权重 0 → 姿态不动。
  - **没做**：clip 之间的时间对齐/同步（现在按同一个世界时间采样）、叠加（additive）层、过渡曲线。
- **动画的角速度推算（gap #23 补第一步的边界）**：`rotation` 轨道不再只设姿态——每步用
  `dq = to * from⁻¹` 取轴角再 `/dt` 写成**角速度**（最短弧：先按 `w ≥ 0` 归一符号）。
  没有它，被动画转起来的球在接触点没有切向速度，**摩擦使不上劲、球原地空转**（实测位移恒为 0）。
  现在绕 +Z 转的球会被摩擦推向 -X：60 步约 -0.38 m。纯函数 `animation::angular_velocity_between`
  有 4 条断言（数值/方向/最短弧/非法 dt），e2e 用 `simulate` 的 `states` 钉住"真的滚起来了"。
  - **同时暴露并写明一条语义边界**：旋转关键帧按**姿态**存、段内走 slerp，所以**一段最多转 180°**；
    跨度超了走最短弧，两个等效姿态（0° 与 360°/720°）之间甚至会退化成"完全不动"。
    场景校验对此给**警告**（不是错误：场景仍合法），提示在中间插关键帧——不能让作者对着
    "我写了 720° 它没转"发呆。恰好 180° 时"往哪边转"本身是歧义的（q 与 -q 同一姿态），
    两种解读都合法，单测里也写明了。
- **关键帧动画 `animations[]`（gap #23 第一步）**：`{ target, loop?, start?, speed?, position?: {times, values}, rotation?: {times, values} }`，
  按时间把物体推着走（**运动学**：每步直接设变换）。`position` 段内线性插值，`rotation` 段内**四元数 slerp**
  （关键帧写欧拉角 XYZ 度，所以不会出现绕远路的中间姿态）；`start` 之前保持第一帧，不循环时末尾截断，
  循环按轨道时长取模（`speed` 在取模之前生效）。缺省空 = 行为与今天逐位一致。
  - 引擎实现放在 `crates/quench3d/src/animation.rs`（**纯计算**，可在无显卡环境完整单测：插值/截断/循环/slerp/倍速/起始保持共 4 条测试）。
  - 播放接在 CLI 脚本层：`Script::step` 里 **先动画、再动作、最后步进物理**（顺序写在注释里，反了动作会被动画抹掉）；
    位置轨道用**有限差分**补线速度（`Δ位置/dt`），让接触与摩擦看到"它在动"而不是"它每步瞬移"。
  - 校验：轨道长度一致、时间非负且严格递增、取值有限、至少一条轨道、`speed > 0`、目标名不能为空。
  - e2e（无显卡）：0.5 秒时 y 到 5、循环 1.5 秒后回到顶部附近、目标不存在 → 退出码 2 且错误里带目标名、
    时间不递增 → `validate` 判 `valid: false` 并指出"严格递增"。
  - **顺手修掉一个真 bug**：三组早退检查（命名相机 / `render.outputs` / 动画）的 guard 原先夹在中间，
    导致**后面追加的诊断被静默丢掉**——动画时间不递增时 `validate` 报 `valid: true`。guard 挪到三组检查之后，
    并把这条教训写进注释。
  - **没做**：glTF 动画/蒙皮导入、顶点蒙皮（skin）、两条 clip 之间的混合、编辑器的播放/时间轴 UI；
    `rotation` 只设姿态不推角速度（滚动/摩擦耦合不精确）。
- **MSRV 1.90 由 CI 验证（gap #30 收尾）**：新增 `msrv (1.90)` 腿，用 1.90.0 工具链对
  `quench3d-math` / `-physics` / `-render` / `quench3d` / `quench3d-cli` 跑
  `cargo check --locked --all-targets`（首次即全绿）。此前 `rust-version = "1.90"` 只是
  "从依赖元数据推出的下界"，声明与事实之间没有防线——**声明没被验证就等于没有声明**。
  范围有意排除 `quench3d-editor`（它单独声明 1.95，egui 0.37 要求），写进 CI 注释与 gap 行，
  不含糊说成"整个工作区都过了"；`msrv` 腿只在 ubuntu 跑，Windows/macOS 由 stable 腿覆盖。
- **`--camera <名字>`（gap #16 第二步）**：`render` / `simulate` / `movie` 都能用命名相机，
  `default` 保留给 `camera` 段那台；未知名 → 退出码 **2**、`error.kind = unknown_camera`、
  消息里带那个名字并列出可用相机（`report.rs::exit_code` 把 `unknown_camera` 归到
  「输入有问题，改参数再跑」这一类）。实现上只在**一处**换相机（`SceneDoc::select_camera`
  被 `load_scene` 调用），下游 `apply_camera`/报告只看 `camera` 段，不必改渲染路径。
  测试：单测覆盖选择与错误消息，e2e `camera_flag_reports_unknown_names_without_touching_the_gpu`
  用 `--dry-run` 钉住"取相机阶段就失败、不该碰 GPU"（CI 无显卡）。
  **没做**：报告里的 `camera_name` 字段、`render.outputs[]` 多输出。
- **命名相机 `cameras`（gap #16 第一步）**：`cameras: { 名字 → 相机对象 }`，缺省空 = 行为与
  今天完全一致（只渲 `camera` 段那台）。校验只拦两种真会出事的名字：空名（没法被引用）与保留名
  `default`（它是 `camera` 段的别名）；手写 JSON 预检的白名单、`schema()` 语法表、系统提示、
  `json_schema()` 的属性表全部同步（一致性测试会盯着，漏一个就红）。**没做**：`--camera <名字>`
  的选择与 `render.outputs[]` 多输出——下一步（设计见 `docs/production_readiness.md §9`）。
- **`cargo doc -D warnings` 进门禁与 CI（gap #30 的一部分）**：rustdoc 的「文档链到私有项 /
  拼错路径」以前没人管，现在 `tools/gate.sh` 多一段 `PASS doc`、CI 多一个 `cargo doc` 步骤；
  为此修掉全部 16 处既有告警（私有模块、私有函数、字面量被当成链接），改成纯代码字体，
  **没有用 `#[allow]` 掩过去**（掩掉等于废掉这段门禁）。
- 无新增（第 31 轮是**核对轮**）：确认 gap #16 的"正交投影 / `near`·`far` / fov / 距离 /
  偏航俯仰 / 自动取景"都已落地并有用例钉住（`render/camera.rs:58/275/310`、
  `cli_e2e.rs:2015`）；把该行收窄成只剩**多相机 / 多输出**没做，并写明它要动哪三处
  （场景格式、渲染循环、CLI 报告）。
- **接触余量 `physics.speculative_margin`（gap #20 第二条腿）**：分离但距离小于余量的
  形状对也生成接触点（穿透量为**负**），求解器对这类接触按 `separation/dt` 给法向速度目标，
  于是高速物体"即将接触"时就进入约束求解，少一帧穿透突跳。**缺省 0 = 只在重叠时生成接触**，
  没写这一项的场景结果与历史**逐位相同**（digest 测试当场验证过：一开始没给这道门，
  窄相本来就会给少量负穿透，整场结果漂移，`digest_tests` 立刻红了）。
  形状膨胀只对球/盒/胶囊/柱（半径或半长 +margin）；平面/凸包/三角网原样返回。
  **边界**：只作用于单形状刚体（复合形状不膨胀）；没有加数值范围校验（写超大值会全场互撞，
  未拦截）；没有专门的 e2e 用例，验证靠物理套件 + digest 不变。
- 无新增（第 29 轮是**核对轮**）：逐条验证 gap #15（`actions[]` + `probes[]`）与
  gap #18（角速度 / 阻尼 / `gravity_scale` / `mass` / 睡眠）确实是"格式 + 校验 + 引擎 +
  CLI + 编辑器"全程可用，把两行从"待做"改成 ✅ 并写明**边界**（探针没有可视化绘制、
  动作没有时间线 UI、`mass` 与 `density` 同时写时密度优先、没有 `sleep_threshold`
  这类更细的睡眠参数）。证据：`cli_e2e.rs:2345/2500/2577`、`scene_doc.rs:158/164/195-205/890-907`、
  `scene_io.rs:37/386`。判据写进 `docs/production_readiness_criteria.md` 附表。
- **模板 + 实例：一段描述摆出很多物体**（gap #14）。以前堆 1000 个箱子就要在 JSON 里
  写 1000 份重复描述——agent 生成大场景时 token 直接爆。
  * `templates`: `{ "箱子": { "shape": {...}, "body": "dynamic", "material": "木头" } }`
    （字段集与物体**完全相同**，包括复合形状 `shapes`）。
  * `objects[].template`: 引用模板；**物体自己写的字段覆盖模板**。形状整体二选一
    （物体写了 `shape`/`shapes` 就不取模板的形状）；模板里的
    `name`/`parent`/`position`/`rotation_deg` 没有意义 → 警告并忽略。
  * `objects[].instances`: `[ { name?, position?, rotation_deg?, scale? } ]` —— 每个副本是
    一个**独立物体/刚体**（不是共享刚体，也不是 GPU 实例化），名字缺省 `本名#1`、`本名#2`…
    （只有一个副本且没写 `name` 时保持本名，老场景的关节/动作引用不受影响）。
  * 校验：模板不存在 / `instances` 空数组 / 展开后名字撞车 / 实例字段未知 = error；
    未使用的模板、模板里写位置、副本数 > 64 = warning。展开后的下标才是
    `objects[k]`（错误路径以展开后为准）。
  验证：`cargo test -p quench3d-cli --lib -- templates_and_instances`、
  `cargo test -p quench3d-cli --test cli_e2e -- templates_and_instances`、
  `cargo test -p quench3d-editor --test scene_roundtrip -- templates_and_instances`。
  **没做**：外部场景引用（`include` / 跨文件组装）——那属于资产路径（#11）；
  编辑器装载能展开，但**存回时会摊平成具体物体**（行为一致、文件变长，测试钉住）。

- **复合形状：一个刚体多个 Shape**（gap #12）。桌子的"板 + 腿"、载具的底盘 + 车厢、
  角色的躯干 + 四肢碰撞体，以前只能拿多个刚体 + 关节硬拼（会晃、会漏、要调关节参数）。
  现在一个刚体就能带多个形状：
  * 场景格式：`objects[].shapes = [ { shape, position?, rotation_deg? } ]`，与 `shape`
    **二选一**（两个都写报错、空数组报错、部件里的未知字段报错）。部件的
    `position` / `rotation_deg` 是**相对刚体原点**的局部位姿（度）。
  * 引擎：`BodyBuilder::part(ShapePart)` / `.part_at(shape, offset)`，
    `RigidBody::parts()` / `part_count()` / `extra_parts()` / `part_world_transform()`。
  * 质量属性按所有部件合成：质量相加、质心是一阶矩的加权平均、惯性张量是
    "零件惯量转到刚体坐标系 + 平行轴（`m·r²`）"再挪回合成质心。**单形状刚体走原路径**，
    数值与以前逐位一致（不是"应该一样"，是同一个分支没动）。
  * 碰撞、AABB、射线/距离/重叠查询、角色控制器全部**逐部件**处理；每对刚体最多保留
    **8 个接触点**（超限按穿透深度取最深的，稳定排序保证确定性）。任一部件是
    半空间/三角网 → 整个刚体强制静态。
  * 渲染：每个部件一个绘制物体（附加部件挂成主物体的子物体，跟着刚体走）。
    `SceneObject::owner_part` 标记"这个绘制物体是我的第几个部件"——**编辑器的场景树、
    导出、选择都把它当主物体的一部分**。少了这条线，场景每存一次就会多出几个物体。
  * 编辑器：不做部件编辑面板，但复合形状**原样往返**（导出写 `shapes`，导入重建部件），
    `tests/scene_roundtrip.rs` 里连"伪物体不许被写成独立物体"一起钉住。
  验证：`cargo test -p quench3d-physics --lib -- compound`、
  `cargo test -p quench3d-cli --lib -- compound_shapes`、
  `cargo test -p quench3d-cli --test cli_e2e -- compound_shapes`、
  `cargo test -p quench3d-editor --test scene_roundtrip -- compound`。

- **事件总线：一步里"发生了什么"有地方问了**（gap #29）。以前只有
  `sensor_events()` 与"当前接触表"，接触开始/结束、谁睡谁醒、哪个关节断了都只能自己
  逐帧比对。现在 `PhysicsWorld` 有一条跨步累积的事件队列：
  * 7 种事件：`contact_begin` / `contact_end` / `sensor_enter` / `sensor_exit` /
    `sleep` / `wake` / `joint_broken`（扁平 `PhysicsEvent`：`step` + `kind` + `a`/`b` +
    断裂时的 `joint`/`force`/`torque`）。
  * 取事件：`drain_events(&mut buf)`（把世界的缓冲留给自己复用，零分配）或 `take_events()`。
  * **队列有上限**（默认 4096）：到顶丢新事件但记在 `events_dropped()`——
    "宿主忘了取"不该变成内存泄漏，也不该静默无痕。`set_event_capacity(0)` 会退化成默认值。
  * 接触/传感器事件用**集合差分**（与上一子步比），所以不会重复报；睡觉/唤醒事件用
    "每步比对沉睡状态"，因为唤醒入口太多（手动、全唤醒、接触、关节），逐个埋点必漏。
    `sleep`/`wake` 每步最多一条；事件顺序按 (a, b) 排序后再差分，**与宽相顺序无关**。
  * `simulate` 的报告里多了 `events[]`（`{step, kind, a, b?, force?, torque?}`，物体名已解析，
    地面叫 `$ground`）与 `events_dropped`（永远输出，0 也输出，脚本要能区分"没丢"与"没这个字段"）。
  * 编辑器状态栏**只读**显示队列深度、丢弃数与最近一条事件（句柄显示成 `#下标`）；
    没有事件面板/时间线，也不能在界面上清空队列。
  验证：`cargo test -p quench3d-physics --lib -- event_bus`、
  `cargo test -p quench3d-cli --test cli_e2e -- simulate_report_includes_an_event_stream`。

- **关节补全：球窝锥形限位、铰链伺服马达、断裂阈值、关节两端不再互相碰撞**（gap #11）。
  以前关节只能"焊/转/滑"，做布娃娃、机械臂、可破坏门都缺零件。现在四件事都能配：
  * `joints[].cone_limit = { half_angle_deg, axis_a?, axis_b? }`（仅 `type: "ball"`）：
    两端各给一根局部轴，夹角被限制在 `half_angle_deg` 之内（2 个转动自由度被限，
    绕轴自转仍然自由）。实现上复用铰链限位那一行（坐标换成两轴夹角、修正轴换成两轴叉积），
    是**速度级**约束：半秒内压回锥面，不是瞬间夹住。
  * `joints[].motor_target_deg / motor_stiffness / motor_damping`（仅铰链）：**位置伺服**，
    转到目标角并停住（缺省 `stiffness = 8`、`damping = 1`）。给了目标角就按伺服走，
    同时给 `motor_velocity` 会警告"速度被忽略"。引擎侧是
    `HingeMotor::servo(弧度, 最大冲量).gains(kp, kd)`。
  * `joints[].break_force`（N）/ `break_torque`（N·m）：本步约束力量级超过阈值就把关节
    **移除**（不是禁用），断过的句柄进 `PhysicsWorld::broken_joints()`，`simulate` 报告里
    多了 `joints_broken`。阈值为 0/负/NaN 一律等于"不设"（静默的 NaN 阈值最难查）。
  * `joints[].collide_connected`（缺省 `false`）：**关节两端默认不再互相碰撞**，
    与 Bullet / PhysX 的 `collide_connected = false` 一致。铰链、球窝的两端几何上就是
    重合的（锚点相同），让它们参与碰撞等于让门和门轴互相挤——表现为"转到一半卡住"
    "伺服停在没到位的角度上"（调试这一项时真的踩到：门停在 116°，目标是 90°）。
    需要"连起来还能互相弹开"时显式写 `true`。
- **被马达驱动的刚体不再自动睡眠**（顺带修的引擎 bug）。伺服到位之后速度正好是 0，
  只看速度的睡眠判定会把它判成静止、清零速度并冻结——于是停在没到位的角度上再也回不来。
  现在只要关节带马达（含 `target_velocity = 0`），两端刚体这一步不许睡，同岛的其他刚体
  也跟着保持清醒；马达删掉之后恢复正常睡眠。
  验证：`cargo test -p quench3d-physics --lib -- ball_cone_limit hinge_servo servo_keeps
  joint_breaks jointed_bodies_stop`、`cargo test -p quench3d-cli --lib -- joint_completion_fields`、
  `cargo test -p quench3d-cli --test cli_e2e -- joint_servo_cone_limit_and_break_force`、
  `cargo test -p quench3d-editor --test scene_roundtrip -- export_preserves`。

### 修复

- **`rad()` 是"用弧度构造"，不做换算**：一度以为它会转换（`rad(90.0)` 得到 90 弧度）。
  本次在 runner 里连踩两次（伺服目标 30 度被当成 30 弧度，绕回后变成 -81.13°；
  锥形限位 25 度被夹到 π 而失效）。现在都用 `to_radians()`，注释里写明这个坑。

- **物理材质：静摩擦 + 摩擦/恢复的合成规则**（gap #22）。以前 `friction` / `restitution`
  各写一个，接触时到底听谁的**没有选择**：摩擦写死几何平均、恢复写死取大。
  现在两个都能选，而且"静摩擦"独立出来了：
  * `physics.{friction_combine, restitution_combine, static_friction_speed}`：世界级缺省；
    `objects[].{friction_combine, restitution_combine}` 按物体覆盖。规则有
    `geometric_mean`（摩擦缺省，= 老行为）/ `average` / `min` / `max` / `multiply`；
    恢复的缺省是 `max`（= 老行为）。
  * `objects[].static_friction`（缺省跟随 `friction`）：切向相对速度低于阈值时用静摩擦，
    高于则用动摩擦——"推得动、但自己不会慢慢滑"。判定用**施力之前**的速度
    （用施力后的速度会让静止的箱子永远走"滑动"分支，静摩擦形同虚设）。
  * 一切缺省都等于老行为：不写这些字段的场景，仿真结果**逐位不变**
    （digest 回归测试钉着）。
  * 编辑器：物理面板多了「静摩擦」滑块与「摩擦合成 / 弹性合成」下拉（含"（缺省）"
    一项，点了能回到跟随世界缺省），导出时从刚体写回。
  验证：`cargo test -p quench3d-physics --lib -- static_friction friction_combine combine_rule`、
  `cargo test -p quench3d-cli --lib -- physics_materials`、
  `cargo test -p quench3d-cli --test cli_e2e -- static_friction_and_combine`、
  `cargo test -p quench3d-editor --test scene_roundtrip -- export_preserves`。

- **睡眠 API 补全：`RigidBody::sleep()` 公开 + `PhysicsWorld::wake_up_all()`**。
  以前只能 `wake_up()`（叫醒一个），想让"体检式"地批量唤醒或手动让某个物体入睡，
  得自己遍历（而且 `sleep()` 是 crate 私有）。现在：
  * `RigidBody::sleep()`：**立即**入睡并清零线速度/角速度（与"自动睡眠"不同：自动睡眠
    要求整岛都慢够 0.5 秒，手动入睡不等）；`is_sleeping()` / `can_sleep()` 随时可查。
  * `PhysicsWorld::wake_up_all() -> usize`：一趟唤醒所有睡着的刚体，返回**唤醒了几个**
    （可观测，而不是"默默干了点什么"）。
  * `allow_sleep = false` 变成了**硬承诺**：以前它是"不主动入睡"，但一个被手动 `sleep()`
    的刚体会一直睡着（`update_sleeping` 只看速度）；现在这类刚体下一次 `step` 一定被
    置回清醒——"我说了不许睡"不该被一次 `sleep()` 推翻。
  * 场景格式：`actions[]` 的 `wake` 动作接受 `target: "*"`（唤醒全部），
    别的动作写 `"*"` 会**报错**（带 `actions[i].target` 路径）而不是悄悄只作用在一个物体上。
  * 编辑器：工具栏新增「☀ 唤醒全部」（状态栏报"已唤醒 N 个"）——搬动过一批物体、
    或从快照恢复之后，免得有东西以睡着的姿态停在半空。
  验证：`cargo test -p quench3d-physics --lib -- wake_up_all`（**不需要显卡**：零重力静止
  场景两个盒子入睡 → `wake_up_all()` 返回 2 且都醒；再唤返回 0（幂等）；`sleep()` 清零速度、
  睡着后位置不漂；`allow_sleep = false` 的刚体睡不住）、
  `cargo test -p quench3d-cli --test cli_e2e -- wake_action_with_star`（端到端：
  报告里 `sleeping` 2 → 0；只唤 `a` 时 `b` 仍然睡着；`impulse` 配 `"*"` 退出码 2）。

- **光照补全：雾、阴影质量、天空颜色**。引擎里早就有这三样，场景文件里却配不到
  （只能选 `preset` 和开关 `sky` / `shadows`），于是"夜景为什么没有雾""阴影边缘为什么
  糊/为什么没有影子"这类问题只能改引擎代码。现在都能从场景文件配：
  * `lighting.fog`（**不写 = 关闭**，像素与关闭时逐位相同）：`color`（**线性空间**，
    与 `point_lights.color` 同一约定，不是 `materials.*.color` 的 sRGB）+ `density`
    （每米指数衰减，缺省 0.02；0 = 关）。天空与参考网格也会跟着雾一起淡出。
  * `lighting.shadow_quality`（**不写 = 引擎缺省** 0.85 / 28 米）：`strength`（0~1；
    0 = 阴影不压暗，1 = 全黑——真实场景 0.6~0.9 更自然）+ `radius`（阴影相机覆盖半径，
    米；这是"覆盖范围 ↔ 精度"的旋钮，给大远处也能投上阴影但边缘更糊）。
    与开关 `lighting.shadows` 分开：开关管"投不投"，这一节管"投得好不好"。
  * `lighting.sky_colors`（只覆盖写了的那几个通道，缺省跟随 `preset`）：
    `zenith` / `horizon` / `ground`（**线性空间**）。
  * 校验：负颜色/负密度/强度越界/半径非正/天空颜色为负都是 error（带 JSON 路径）；
    明显不合常理的值（密度 > 1、覆盖半径 > 400、颜色 > 4）是 warning —— 能出画面但要提醒。
  * 编辑器：光照面板多了雾（颜色 + 密度滑块）、阴影强度/覆盖半径、天空天顶/地平线颜色，
    改完能撤销、能存盘（导出写回引擎值）。
  验证：`cargo test -p quench3d-cli --lib -- fog_shadow_quality`（格式与校验：合法值往返、
  五个非法路径各给 error、超范围给 warning、NaN/inf 也挡、拼错字段报错）、
  `cargo test -p quench3d-cli --test cli_e2e -- fog_and_shadow_quality`（端到端：合法场景
  校验通过、非法场景在打开 GPU 之前退出码 2、`render` 的像素证明「雾密度 0 与不写逐位相同」
  且浓雾把画面推向雾色、阴影强度 0→1 让画面变暗、自定义天空颜色改变画面）、
  `cargo test -p quench3d-render --test fog_and_shadow_quality`（**像素级**，本机有显卡真跑：
  浓红雾把远处像素染红、密度 0 与不开逐位相同、阴影强度只暗在影子里且 0.5 落在 0 与 1 之间）、
  `cargo test -p quench3d-editor --test scene_roundtrip -- export_preserves`
  （存盘往返：雾/阴影质量/天空颜色都不丢，编辑器里改完能写进文件）。

- **查询补全 + 查询加速结构**。以前只有"最近一条射线"和"球形重叠"，而且都是 O(N) 线性扫描。
  * 引擎：`raycast_all` / `raycast_all_filtered`（返回最远距离内的**所有**命中，按距离升序、
    平局按槽位下标，输出确定）、`overlap_shape` / `overlap_shape_filtered`（**任意形状**探针：
    盒 / 球 / 胶囊 / 圆柱 / 半空间，含旋转位姿）、`distance_to(shape, transform, handle)`
    给"查询形状到某个刚体的最近距离 + 两侧最近点 + 法线"。
  * `QueryFilter { mask, sensors }`：**单向**判据（只看被查方在不在掩码里，不像物理碰撞要
    双方都愿意），`sensors: false` 可把触发区排除在外。
  * `QueryIndex`（`PhysicsWorld::build_query_index()`）：与宽相同一套策略的均匀空间哈希网格
    （同一份 `pick_cell_size` / `spread_into_cells`），射线用 3D DDA 只走经过的单元、
    重叠只扫探针 AABB 覆盖的单元，超大物体与半空间升级成"全局代理"保证不漏。
    它是**快照**：世界动过（`PhysicsWorld::version()` 变了，增删刚体/`body_mut`/`step` 都会 +1）
    查询就**自动回退到线性扫描**，结果永远正确、`is_stale()` / 报告里的 `fallback` 会告诉你。
    已有用例逐条对比索引路径与线性路径的命中（句柄、距离、点），并断言确实少检查了刚体。
  * CLI `quench3d query`：`--ray ox,oy,oz,dx,dy,dz` / `--overlap "box:1,2,1@0,1,0~0,45,0"`、
    `--all`、`--mask`、`--no-sensors`、`--max`、`--steps`（先跑物理再查）、`--index`。
    报告里有 `hits[]`（名字/类型/距离/点/法线/层/是否传感器）、`bodies`、`tested`
    （**实际检查了几个刚体**，索引路径的证据）、`indexed`、`fallback`。
    `--dry-run` / `--no-clobber` / `--out` 一律**报错**（query 只读不写，悄悄忽略参数更糟）。
  * MCP 新增 `quench3d_query` 工具（只读、幂等、不出网），参数与 CLI 同一套语法。
  * 编辑器 F7 "查询探针"：相机中心那条射线打到什么就画红箭头 + 绿色法线
    （走的就是 `PhysicsWorld::raycast`；`center_probe` 是纯函数，有离线用例）。
  验证：`cargo test -p quench3d-physics --lib`（**不需要显卡**，6 个新用例：`raycast_all`
  的距离升序/上限截断/退化输入、任意形状重叠与 `overlap_sphere` 一致、单向层掩码与传感器开关、
  索引 vs 线性逐条一致且确实更省、过期自动回退且结果跟着世界走、横跨多单元的大物体与掩码）、
  `cargo test -p quench3d-physics --doc`（`QueryIndex` 文档里的例子真的跑）、
  `cargo test -p quench3d --lib center_probe`（编辑器探针：命中距离/法线/看旁边为空/距离上限）、
  `cargo test -p quench3d-cli --test cli_e2e query_`（端到端：最近命中、`--all` 两条按距离升序、
  `--no-sensors`、`--mask 8` 单向过滤、`--index` 与线性一致、重叠命中且距离为 0、
  旋转探针盒真的生效、`--max` 截断、`--steps` 先跑物理再查，以及 9 条错误路径都被拒）、
  `cargo test -p quench3d-cli --lib mcp`（工具清单 9 个、query 只读/幂等/outputSchema 有 hits）。

- **场景层级（父子变换）+ 场景格式 v2**。`objects[].parent` 用**父物体的名字**引用，
  给了它之后 `position` / `rotation_deg` 就是**相对父物体**的局部量：父物体一动，子物体跟着走。
  覆盖范围：引擎（`SceneObject::parent`、`Scene::world_transform` / `set_world_transform` /
  `children_of` / `is_ancestor` / `remove`，绘制、包围盒、拾取、物理同步全部走世界变换）、
  场景格式（校验器拦悬空父名、自环、成环、以及"带刚体还挂父物体"）、CLI（装载时按名字解析成下标，
  父物体可以排在子物体后面）、编辑器（场景树缩进显示层级、属性面板"父物体"下拉改父、
  **拖动父物体时子物体跟着走**、导出写回局部变换 + `parent`）。
  几处刻意的设计：
  ① 改父与**删除父物体**都**保持子物体的世界位姿**（不然物体一改父就跳，是最像 bug 的行为）；
  ② 子物体继承父物体的**位置与朝向，不继承缩放** —— 这个格式里 `scale` 就是图元尺寸
  （`shape.size` 折算而来），乘法继承会把"挂在大地板下的树"压成一块饼；
  ③ 带刚体的物体**不能**有父物体（位姿归物理管，否则父物体的运动会静默失效）；
  ④ 导出时父物体没被写成物体（地面、模型展开的网格）就**丢掉这条关系并上报**，
  不写悬空引用（那样整个文件都读不回来）。
  **破坏性改动**：格式版本升到 2，**不支持版本 1**（没有外部使用者，不做迁移器）——
  版本 1 的文件会拿到一条"当前支持 2"的诊断，而不是被静默按新语义（局部变换）解释。
  验证：`cargo test -p quench3d --lib`（5 个纯逻辑用例，无显卡也能跑）、
  `cargo test -p quench3d-cli --lib`（7 个格式用例：往返、悬空、自环、成环只报一次、
  带刚体拒绝、版本 1 被拒）、`cargo test -p quench3d-editor --test hierarchy`
  （7 个集成用例：父链装配、拖父带子、改父不跳位、成环被挡、删父不跳位、存盘往返、丢父上报）。

- **物理：碰撞过滤（层/掩码）、传感器事件、CCD**，三项都能从场景文件配、CLI 能验证。
  * `objects[].collision_layer`（0~31，缺省 0）+ `objects[].collides_with`（层号列表，
    缺省所有层；`[]` = 谁也不碰）。判断**对称**：`A` 愿意碰 `B` 且 `B` 也愿意碰 `A` 才碰。
    过滤在**宽相**就生效（不配对的连候选对都不产生），CCD 扫掠也遵守它。
    **只作用于物理**：射线/拾取/overlap 查询不看分组（边界写进 README §9）。
  * `objects[].sensor`（触发区）：照常进 `contacts()` 与进入/离开事件，但**不产生任何冲量**
    （不进求解器，也**不唤醒睡眠体**——一个巨大的触发区不该叫醒整个世界）。
    引擎侧 `PhysicsWorld::sensor_events()` / `sensor_pairs()`，`ContactPair` 新增 `sensor` 字段；
    CLI 的 `simulate` / `render` / `movie` 三条报告都新增 `sensors[]`（谁的 `enter_step` /
    `exit_step` / `steps_inside` / `entries`，按名字排序，输出稳定；`render` 里内嵌的
    `simulate` 报告同样带着）。事件是**每步轮询**式，没有回调/事件总线。
  * `objects[].ccd`（只对 dynamic 生效）：每子步从刚体中心沿这一步的位移方向打一条
    **中心射线**（排除自己、传感器、分组不匹配的刚体），命中距离减去"中心到表面在运动
    方向上的距离"（`Shape::support(dir)·dir`，凸形状上精确）就是中心还能走多远，
    于是只把这一步**朝表面**的法向位移截到那里（切向保留，所以沿墙滑行不受影响）。
    一开始用的是"保守推进扫掠"（`shape_cast`），**贴着地面滑行的子弹会被脚下的地面拖住**
    （每步只推进地面距离那么远，而且返回的"命中"是地面——法线与运动垂直，不该拦），
    加地面之后子弹照样穿墙；这条坑有专门的回归用例（`ccd_works_while_skimming_a_ground_plane`）。
    **不是**精确 TOI：只扫中心线（擦角可能漏）、无旋转扫掠，极快 + 极薄仍可能漏；
    代价是每子步一次射线查询，与速度无关。
  * 缺省语义：三个字段都不写 = 与加这个功能之前**逐位相同**（`InteractionGroups::default()`
    是"第 0 层 + 碰所有层"，不是 `memberships = ALL`——后者会让"只想碰第 5 层的过滤体"
    把缺省物体当成同类）。
  * 编辑器：没有编辑界面，但**导出不会吃掉这些字段**（从物理刚体读回来，有往返用例钉住）；
    F5 接触点里传感器接触画成**青色**，与真实碰撞区分。
  验证：`cargo test -p quench3d-physics --lib`（**不需要显卡**，8 个新用例：分组对称匹配与
  不匹配穿透、宽相在窄相之前就把不配对的对滤掉、传感器进入/离开且零冲量、传感器不唤醒睡眠体、
  CCD 停住 600 m/s 的小球、CCD 遵守分组、贴着地面滑行的 CCD 子弹仍被薄墙拦住）、
  `cargo test -p quench3d-cli --lib collision_filter`（格式与校验：层号越界/重复/空掩码、
  传感器缺刚体是 error、CCD 非 dynamic 是 warning）、
  `cargo test -p quench3d-cli --test cli_e2e simulate_reports_sensors`（端到端：报告里有
  传感器的进入/离开且落体穿过触发区、薄墙分组不匹配时子弹连 CCD 都拦不住、
  同一颗子弹不开 CCD 一步跨过薄墙而开了 CCD 停在墙前，以及 render / movie 两条路径
  也带上了 `sensors[]`）、
  `cargo test -p quench3d-editor --test hierarchy export_preserves`（存盘往返不丢字段）。

- **SSAO（屏幕空间环境光遮蔽）**。`lighting.ssao`（**不写 = 关闭**，像素与关闭时逐位相同；
  写 `{}` 就是打开），参数 `radius`（米，缺省 0.75）/ `bias`（缺省 0.025）/
  `strength`（缺省 1.0）/ `samples`（缺省 16，1~64）；CLI、`validate`、编辑器视口面板
  （"环境光遮蔽（SSAO）"）都能开。让物体与地面的接触缝隙、墙角变暗——廉价版接触阴影。
  实现是**深度-only**：没有法线缓冲，法线由视空间位置的屏幕导数重建；半球采样按黄金角螺旋
  分布、每像素用**交错梯度噪声**（不是白噪声：同样免费，颗粒感明显更小）旋转；
  遮挡判据是"真实几何比采样点更靠近相机，且距离落在 (bias, radius) 内"。
  结果乘在整幅 HDR 颜色上（不区分直接光/间接光）——后处理式 SSAO 的固有简化。
  **破坏性改动（内部）**：深度格式 `Depth24Plus` → `Depth32Float`（前者不可采样，
  SSAO 必须把深度当纹理读）；MSAA > 1 时多一趟 `depth_resolve.wgsl` 把多采样深度的
  第 0 号采样搬到单采样深度（深度**不能**平均：几何边缘上的平均深度是虚构的）。
  验证：`cargo test -p quench3d-render --test post_pipeline ssao`（像素级：msaa=4 走深度解析
  那条路，接缝窗口平均暗 7.4、最暗像素暗 30 且离接缝 5.7 像素、远处平坦地面只暗 0.45、
  天空逐位不变）；`cargo test -p quench3d-cli --lib ssao`（格式：半径非正/采样数越界是 error、
  半径过大与偏移≥半径与强度>1 是 warning、拼错字段报未知字段）。
  端到端：`target/ssao-demo/`（同一 480×320 场景开/关 + 全开 SSAO+FXAA+bloom+MSAA4），
  逐像素统计：天空带均值暗 0.03（标准差 0.48，即天空没被碰）、接触区均值暗 3.25、
  极值 51。

- **FXAA（后处理抗锯齿）**。`lighting.fxaa`（**不写 = 关闭**，像素与关闭时逐位相同；
  写 `{}` 就是打开），CLI 的 `render`/`shot` 与 `validate` 都认这个字段，编辑器视口面板
  也有"抗锯齿（FXAA）"勾选框。
  它在 tonemap **之后**的画面上工作（阈值是 0~1 的**感知**亮度，不是线性光强），
  中间目标是另一张 `rgba16float`：着色器给线性显示值，最后写 sRGB 目标时由硬件编码一次——
  所以"边缘判断在 sRGB 空间、混合在线性空间"（FXAA 原版整条都在 gamma 空间，这里有细微差别，
  见 `fxaa.wgsl` 顶部注释）；用 8 位 sRGB 中间目标会在混合处多一次量化，因此不用。
  算法是 FXAA 3.11 的 console 档：4 个对角取样估方向 + 沿方向两次混合 + 亮度越界回退，
  没有高质量档的子像素锯齿消除。**平坦区域逐位不变**（提前退出），
  与 `render.msaa` 不冲突也不重复。
  验证：`cargo test -p quench3d-render --test post_pipeline`（2 个像素级用例：
  ① 关掉时硬边中间值为 0、打开后 ≥8 个中间值，且物体内部 ±6 像素与四角背景**逐位不变**；
  ② 把阈值调到不可能触发，整幅图必须与关闭时逐位一致——防"阈值读了但没进判断"）；
  `cargo test -p quench3d-cli --lib fxaa`（格式：不写就不出现在输出里、空对象即打开、
  负阈值是 error、>1 是 warning、显式 enabled:false 不再警告、拼错字段报未知字段）。
  端到端：同一场景开/关各渲一张 480×320（`target/fxaa-demo/`），
  逐像素比对 3.03% 的像素被改动，其中**只有 3 个**离边缘 2 像素以外（3×3 邻域梯度 < 12），
  改动幅度中位数 4/255、最大 118（在最硬的轮廓上）——即"只动边缘"。

### 修复

- **macOS 上偶发红的假 LLM 服务（根因终于找到了）**：测试用的假服务把 listener 设成非阻塞，
  而 **BSD / macOS 的 `accept()` 会继承 `O_NONBLOCK`**（Linux 的不会，所以本地永远复现不出来）。
  于是 `read_request` 里的 `set_read_timeout` 在非阻塞 socket 上不生效：请求字节还没到就立刻
  `WouldBlock`，被当成"对端没发请求"，线程退出、连接被关 → 客户端读到
  `Connection reset by peer (os error 54)`。现在 accept 之后**显式设回阻塞**。
  这条以前被记成"偶发 flaky"（只在 accept 出错时补过重试，治的是另一个症状），
  实际是一个能解释清楚的平台差异；验证方式是 macOS CI 连续绿 + 本地 `llm_mock`/`agent_tools`
  连续 5 轮通过（本地是 Linux，复现不出来）。

- **编辑器：绕 Y 轴 ±90° 的物体存盘后会变成"另一个 90° 旋转"**（万向锁下的欧拉角反解写错了
  矩阵行）。单看这个物体看不出来（都是"转了 90°"），但它的**子物体会立刻跑到别的地方**；
  在没有层级之前这个 bug 表现为导出前后自身朝向不一致。现在改用矩阵第 2 行求解，
  并加了穷举角度的回归测试（`euler_extraction_round_trips_every_axis`，覆盖 ±90° 与 ±89.999°）。
  是"给层级写集成测试"时顺出来的——父物体的旋转错一点，子物体的位置就错一大截。
- **假模型服务与 CI 冒烟测试里的场景夹具升到格式版本 2**（`tools/mock_llm_server.py`、
  `.github/workflows/ci.yml`）—— 否则 `quench3d ask` 从假服务拿到的场景会被校验器
  按"不支持的版本"拒掉（本机实测：升级后 `ask` → `validate` 全绿）。
  这次漂移本地门禁看不出来（它们在任何 crate 的测试路径之外），所以补了一条守卫：
  `cargo test -p quench3d-cli --test scene_fixtures` 断言这两份外部夹具的版本号等于
  `scene_doc::VERSION`（故意改回 v1 会立刻变红，已实测）。

### 新增

- **编辑器：导入模型（glTF/OBJ）接进场景树，并可在视口里实时拖动**。
  `runner::setup_visuals` 现在返回**模型实例映射**（`models[i]` ↔ 它展开出来的物体下标，
  `runner::ModelInstance`），编辑器把它存进 `Doc`，装载 / 撤销 / 重做都会刷新。于是：
  ① 场景树里模型展开的物体收进 `模型 #i · 路径` 分组（成员照旧可点选、可 `⌖` 聚焦，
  标题上还有"整组聚焦"）；② 模型面板改变换**当场生效**（不再只是"重开后生效"）；
  ③ 在视口里拖模型的任何一个子网格 = **整组联动**，并把新变换写回 `models[i]`，
  存盘重开一致；模型成员的属性面板改变换走同一条路。
  诚实边界：一个模型只有**一个**变换（不能单独挪某个子网格）；缩放对模型成员强制等比，
  因为场景格式里 `models[i].scale` 就是一个数；删掉模型网格后 `models[i]` 仍在引用里，
  存盘重开它会随模型回来（状态栏会写明）。
  验证：`cargo test -p quench3d-editor --test model_instances`（5 个用例：两 mesh OBJ 的映射、
  面板改变换即时生效、拖动整组联动并写回且往返一致、删除物体后的下标维护、撤销后映射刷新）。
- **编辑器：撤销 / 重做**。整场景快照式历史（上限 128 步，可配），
  `Ctrl+Z` / `Ctrl+Shift+Z` / `Ctrl+Y`，工具栏带可用态的"↶ 撤销 / ↷ 重做"。
  一次拖拽（gizmo 或滑杆）合并为**一步**；撤销**不改变视角**；
  撤销后的场景与快照**逐字节相同**（不会"撤销一次就又变脏"）。
  边界：只恢复场景内容（物体/变换/材质/刚体参数/灯光/相机文档字段），
  不恢复物理步进状态（速度、睡眠、接触缓存）。
  验证：`cargo test -p quench3d-editor --test history`（6 个用例）。
- **编辑器：变换 gizmo**。移动 / 旋转 / 缩放三模式（`W`/`E`/`R`，工具栏同款按钮），
  世界 / 局部坐标系（`L`）。手柄屏幕定长 88 px、抓取容差 10 px、
  **命中优先于物体拾取**；`Ctrl` 吸附（0.25 / 15° / 0.1，作用于本次拖拽累计增量）、
  `Shift` 精细 ×0.1；一次拖拽 = 一步历史；拖拽中 `Esc` 取消并还原。
  验证：`cargo test -p quench3d-editor --lib`（7 个纯几何单元测试，不需要 GPU）。
- **编辑器：`F` 聚焦选中物体**（未选中时聚焦整个场景），与场景树里的 `⌖` 同效。
- `docs/production_readiness.md`：生产能力评估（逐域现状矩阵、六场景判定、
  隐蔽硬指标清单与关闭成本、三晚路线）。
- `docs/production_readiness_criteria.md`：判定标准框架（`T`/`Q`/`C`/`H` 四类门槛 +
  证据分级 E1–E4 + "什么程度算能替代 X"的折算流程）。

### 新增

- 编辑器**光照面板**（属性面板顶部，场景级）：太阳方向/强度、投影开关、环境光、
  曝光、天空与参考网格；改动同样走撤销历史（拖拽整段算一步）。
  导出时这几个字段从**引擎**写回，所以"调完光照存盘"真的会生效。

### 新增

- 材质**贴图与开关的显式覆盖**（编辑器数据层）：按**物体名**记录基础色/法线/金属度-粗糙度/
  自发光贴图路径与 UV、法线强度、alpha 裁剪、混合、无光照、双面等开关；导出时优先于
  "同名物体继承原文档"的规则，载入后真的生效，且不会串到别的物体。
  验证：`cargo test -p quench3d-editor --test scene_roundtrip`（用例
  `explicit_material_textures_are_exported` 会造一张真贴图并复核两个物体互不影响）。
  **界面还没有**：用户暂时不能点选贴图文件，只能用场景文件写这些字段。

### 新增

- 材质面板把**四个贴图槽**都接上了：基础色 / 法线 / 金属度-粗糙度 / 自发光，
  每个槽一个路径输入 + 「应用…（从磁盘读取）」按钮（原生）；Web 端提示"路径写场景文件里"。
  上传时按槽位区分 sRGB（基础色与自发光是颜色贴图，法线/MR 是数据贴图）。

### 工程卫生

- 新增 `tools/gate.sh`：一条命令跑完整门禁（fmt → clippy 原生 → clippy wasm32 → 全部测试 →
  wasm 构建 → `check_web.mjs`），顺序固定（`check_web` 必须在 wasm 构建之后），
  日志写到 `target/tmp/gate-*.log`，失败自动打印末尾若干行；`--fast` 可跳过 wasm 两项。

### 新增

- 编辑器新增**模型（glTF/OBJ 引用）变换面板**：列出场景里的模型引用，可改位置/旋转/缩放。
  改的是导出底稿里的 `models[i]` —— 因为导出时 `models` 段本来就原样带出、场景快照也包含它，
  所以"改位置 → 存盘 → 重开生效"成立，且撤销/重做能覆盖。
  **当时的诚实边界**（后来的"模型实例映射"已补齐，见本节第一条）：改完当场不会动
  （引擎里的模型实例没有跟着移动，编辑器拿不到"哪个物体属于哪个模型"的映射）。
- `Doc::base_doc()` / `Doc::base_doc_mut()`：访问载入时保留的原文档（编辑器不建模的段靠它带出去）。

### 变更

- 场景导出会**清掉 `lighting.preset`**：编辑器现在自己负责太阳/环境光/曝光等字段，
  留着预设会在载入时把它们再覆盖一遍（"存了但不算数"）。这是有意的行为变更，
  由 `cargo test -p quench3d-editor --test scene_roundtrip` 里的"光照编辑必须写进文件 + 存读存字节相同"钉住。

- CI：Rust 工具链由 `@stable` 改为**钉住 `@1.99.0`**；构建与测试命令加 `--locked`，
  杜绝"本地换了个依赖版本、CI 却过/不过"的漂移。
- 编辑器帮助窗口与状态栏补充手柄模式、坐标系、吸附与撤销说明。

### 修复

- **编辑器存盘不再静默丢段**（重要）：`scene_to_json` 过去从 `SceneDoc::default()` 起头，
  于是「CLI 生成带光照/渲染设置/模型的场景 → 编辑器改一改 → 存盘」之后，
  `lighting`/`simulate`/`render`/`models`/`probes` 与材质的 4 个贴图槽、
  7 个开关会被清零，而文件表面上完全正常。现在导出以最近载入的原文档为底稿，
  只覆盖编辑器真正管得着的字段（物体/地面/材质标量/相机/重力/场景名），
  材质按同名物体继承贴图与开关；引用了已删除物体的关节/动作会被清掉并如实上报。
  验证：`cargo test -p quench3d-editor --test scene_roundtrip`（4 个用例，含
  "存→读→再存字节相同"的固定点断言）。
- 编辑器：Web 端场景往返探针（`roundtrip_probe`）与桌面端行为对齐（此前只在浏览器路径覆盖）。

### 文档

- 新增 `docs/overnight_report.md`（本轮工作验收说明：做了什么、为什么、没做什么、5 分钟复核命令）。
- 新增 `docs/production_readiness.md`：按代码实测的逐域差距矩阵（十张表、带 `文件:行`）、
  六个使用场景判定、H1–H7 一票否决项与本轮推进记录。

### 未做（明确记录，避免误以为"已经有了"）

- gizmo 不支持拖拽中键入数值、不支持轴心切换、不支持多选成组变换
  （现在唯一的"成组"来源是模型实例，见上）。
- 撤销历史没有可视化面板（不能跳到第 k 步）。
- 场景格式仍无迁移器；模型实例只能**整体**变换 —— 不能单独挪某个子网格，
  也不能把非均匀缩放写回格式（`models[i].scale` 是单个浮点数）。

## 更早（0.1.0，独立仓库建立之前）

- 自研渲染：wgpu 30，HDR（`Rgba16Float`）+ ACES 色调映射、阴影贴图 + PCF、
  MSAA 可配、PBR（metallic/roughness/emissive）、调试线框（网格/坐标轴/接触点/包围盒）。
- 自研物理：岛划分 + 并行求解、7 种碰撞形状、关节（固定/铰链/滑块/球/距离/弹簧）、
  睡眠、确定性（跨线程数结果一致有测试钉住）。
- CLI：场景 JSON（`quench3d.scene`）渲染图片 / 序列帧 / 视频、无头运行、参数扫描。
- 编辑器：egui 0.37 面板 + 3D 视口合成、场景树 / 属性面板 / 状态栏、
  拾取、基线重置、存读场景、截图、中文字体子集。
- Web：wasm 编辑器客户端（Pages 在线可玩），后端 WebGPU → WebGL2 自动回退。
- 工具链：337 个测试 0 失败、四平台 CI 矩阵（ubuntu / macOS / Windows / wasm32）。

## 已知问题

- ~~**偶发（flaky）测试**：`crates/quench3d-cli/tests/agent_tools.rs` 的
  `agent_handles_multiple_tools_in_one_turn` 在 **macOS** 上偶发失败~~ —— **已修**，
  但前面记的根因**只对了一半**（第 16 轮才找到真正的那个）：
  1. 第 13 轮修的是"一条连接只答一个请求就丢 socket"（HTTP 客户端连接池会复用 socket，
     复用那一次拿到空响应/连接被关）—— 这是真问题，也确实修掉了；
  2. 但同类失败仍在 macOS 上偶发（第 16 轮又抓到一次，这次是
     `llm_mock::chat_reads_content_length_response_and_sends_expected_request`，
     报 `Connection reset by peer (os error 54)`）。真正的原因是**平台差异**：
     BSD / macOS 的 `accept()` 会**继承** listener 的 `O_NONBLOCK`，而 Linux 的 `accept()`
     不继承 —— 假服务把 listener 设成非阻塞（为了 accept 超时），于是 macOS 上连进来的
     socket 也是非阻塞的，`set_read_timeout` 对它无效：请求字节还没到就立刻 `WouldBlock`，
     被当成"对端没发请求"，线程退出、socket 关闭 → 客户端读到 RST。
     这也解释了为什么"本地怎么跑都不红"。
     修法：accept 之后**显式 `set_nonblocking(false)`**（`crates/quench3d-cli/tests/common/mod.rs`）。
  验证：本地（Linux，复现不出该平台差异）`agent_tools` + `llm_mock` 连续 5 轮全过；
  真正的验证是 **macOS CI 连续 3 次绿**（`38835476605` 原始跑 + 该 run 的 macOS 复跑 +
  `38835998073` 全平台，均 success）。
