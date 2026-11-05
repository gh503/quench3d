# 测试与验证参考

[返回项目入口](../README.md)。本页包含详细操作与接口说明；机器可读参数以 `quench3d schema --json` 为准。

> 测试数量、耗时和证据路径是记录时的快照；当前结果以运行命令及 CI 输出为准。文档调整可只检查链接和示例命令，渲染／物理变更应运行对应测试。

## 7. 测试与验证

```bash
cargo test --workspace         # 922 个测试通过（另 3 个 ignored；物理/CLI/渲染/数学/门面/编辑器 + doctest），0 失败
cargo clippy --workspace --all-targets     # 0 警告（含 examples / tests）
cargo fmt --all --check                    # 0 差异
cargo test -p quench3d-physics  # 只看物理（含 200 体宽相、5 秒长跑稳定性、确定性）
cargo test -p quench3d-editor   # 编辑器：场景存读往返（"要真建 GPU 上下文"的那些见下面）
```

**有一类测试要真的创建 wgpu 上下文**（`backend_fallback`、`model_instances`）：它们默认
**不执行**，设 `QUENCH3D_GPU_TESTS=1` 才跑（不设时每条都会打印跳过原因，加 `-- --nocapture`
可见）。原因：在没有 GPU 的 Windows runner 上创建上下文会**原生崩溃**（`0xc0000005`），
进程直接死掉、`Result::Err` 接不住，CI 只能报 `process didn't exit successfully`。
所以 CI 只在**能建出上下文**的腿（ubuntu / macOS）上打开它，windows 腿关掉；
`tools/gate.sh` 在本机默认打开，没有显卡的机器用 `QUENCH3D_GPU_TESTS=0 tools/gate.sh`

**一条命令跑完可自动化的正向验证**（门禁 + WebGL2 启动 + Web UI 按键/截图 + 摄像头）：

```bash
tools/verify_all.sh
```

每类功能的可复现命令与明确未覆盖项见 [`docs/feature_verification.md`](feature_verification.md)。
（细节与判据见 `docs/overnight_report.md` §52）。

`crates/quench3d/tests/readme_api.rs` 是**文档守卫**：它把 README 第 3 节的代码
片段原样编译一遍，所以「文档里写的 API」与真实 API 不会漂移。

### 7.1 编辑器的验证（桌面 + 浏览器）

编辑器的两条测试

* `scene_json_roundtrip_keeps_objects_and_ground`：默认场景 → 存成 `quench3d.scene` →
  用 **CLI 的解析器与校验器**读回来 → 装回引擎，逐项对账（物体名不丢、位置漂移 < 0.01、
  地面高度与物理半空间一致、刚体数量 = 物体数 + 1）。
  这条测试第一次跑就抓到一个真 bug：默认场景的地面是「压扁的立方体」，
  它的**视觉表面**在原点上方半个板厚，而物理半空间在原点平面上 —— 物体看起来半埋在地里，
  存读一轮还会掉下去 0.5。已改成 `unit_plane`（与 CLI 的地面表示一致）。
* `scene_to_json_reports_unrepresentable_objects`：塞一个场景格式表达不了的物体
  （三角网碰撞体）进去，导出时必须**报告物体名**，而不是静默丢掉。

两条都不依赖窗口；没有可用 GPU 时打印一行「跳过」并返回（CI 机器通常没有软件光栅驱动）。

手工验过的两条实机路径（本机软件光栅 llvmpipe / SwiftShader）：

| 路径 | 证据 |
| --- | --- |
| 桌面（X11 窗口 1280×800） | 日志 `编辑器字体：中文子集可用 = true`、`编辑器第一帧已渲染：1280x800`；窗口截屏分区统计：工具栏平均色 (46,48,52)、左面板 (29,29,30)、视口 (77,92,119)/985 种颜色、右面板 (33,34,34) |
| Web（headless Chrome + SwiftShader，走 WebGL2） | 控制台 `后端选择：gl（navigator.gpu 存在但没有适配器）`、`渲染设备就绪：… / Gl（限制档位 downlevel_webgl2）`、`编辑器字体：中文子集可用 = true`、`编辑器第一帧已渲染：1280x800` |

而且**桌面与浏览器的画面是同一张图**：两边各截一张 1280×800，
16×16 平均亮度网格的平均差 **0.2/255**（最大差 13，出现在状态栏文字那一行），
3D 视口区域的平均像素差 **0.0**；连工具栏按钮的横向像素位置都一一对上。
这比"两边都能启动"强得多——它说明两个入口用的是同一条渲染与界面路径。

**界面能不能真的操作**（这不能靠"截图像界面"证明，必须点下去看结果）：
在浏览器里用 DevTools 协议（`Input.dispatchMouseEvent`）真的按坐标点击画布，
再比对点击前后的像素：

| 操作 | 结果（平均像素差，0~255） |
| --- | --- |
| 点工具栏第一个按钮（播放） | 工具栏 **6.76**（按钮文字 播放 → 暂停）；3D 视口 **4.19** —— 方块真的掉下来了 |
| 再点一次（暂停） | 工具栏 **3.26**（文字变回 播放）；此后 8 秒视口 **0.00** —— 暂停真的停住了 |
| 在视口里点一下 | 左面板（场景树选中高亮）**0.90**、右面板（属性面板内容）**3.13** —— 射线拾取真的选中了物体 |

> 诚实说明：桌面那条路**没能**用合成事件验完。X11 侧用 `xdotool` 打进去的
> `CursorMoved` 能到（日志里有 3 条），但 `MouseInput` / `KeyboardInput` **0 条** ——
> XWayland 下 XTest 的按键/按钮事件没有转发给客户端。窗口缩放（`Resized`）能到，
> 且缩放后界面真的按新尺寸重排（900×620 时右面板真的挪到 x=640）。
> 所以"点击操作界面"这条证据是在**浏览器**里取的，用的还是同一套界面代码。

物理测试覆盖：自由落体、静置接触、三层堆叠、恢复系数、高/低摩擦斜面、静/动摩擦分离、
**复合形状**（质量/质心/惯量合成、AABB 并集、附加部件真的参与碰撞与查询）、**模板/实例**
（展开语义、名字规则、覆盖规则，以及"一份描述 → N 个独立刚体"的端到端）、**事件总线**（接触开始/结束、传感器进出、睡眠/唤醒、断裂、队列上限、确定性）、
射线检测、铰链摆与马达、**位置伺服与锥形限位**、5 种关节、角度限位、**关节断裂**、
**关节两端不互相碰撞**、确定性（逐位相等，含槽位复用）、
200 体宽相、睡眠/唤醒、三角网格与凸包（含**薄板**：模型网格当碰撞体的典型情况）、
病态输入、长时间稳定性。

资源层测试覆盖：OBJ 四边形三角化、负索引、MTL 解析（`Kd/Ks/Ns/d/map_Kd`）。

命令行工具的 **197** 个测试（`cargo test -p quench3d-cli`：lib 99 + bin 6 + 7 个集成测试二进制共 91 + 1 个 doctest），按目的分这几块：

* **契约测试**（`tests/cli_e2e.rs`，64 个）真的去 spawn `quench3d` 子进程，断言
  **单行 JSON 信封、退出码（0/2/3/4）、诊断带路径**、渲染出的确实是 PNG；
  其中 4 条是「能力真的接线了」的像素/数值证据：**材质贴图**（同一箱子带/不带贴图的
  中心色差）、**点光源**（一盏红灯必须把箱子照红，且显式清空预设补光必须改变画面）、
  **相机**（`far` 把 6 米外的箱子裁成 0 像素；`orthographic_height` 让它占满 14400 像素）、
  **刚体参数**（`gravity_scale=0` 悬停、阻尼减速、`allow_sleep=false` 不睡、
  `mass` 改变碰撞结果、`angular_velocity` 真的改朝向）、**关节锚点**（球关节带偏置锚点时
  质心被约束在 1 米外）、**关节按端局部轴**（门板转 90° 时两端共用一根局部轴会被扭平：
  薄轴与世界 Y 的夹角 `|n·Y|` 从 0.0002 变成 1.0000；只写 `axis` 与两端显式同轴的 `states`
  逐字节相同）、**定时动作**（第 100 步的冲量把静止的球重新打上天；
  `at_time: 1.0` 与 `at_step: 60` 的输出逐字节相同）、**区域探针**
  （`enter_step`/`exit_step`/区内最高速符合自由落体，且加探针不改变物理）、
  **三条路径一致**（simulate / render / movie 都要认动作与探针）、
  **`--dry-run` 一个字节都不写**（渲染/录制/帧目录全都不存在，而物理是真跑的）、
  **`--no-clobber` 拒绝覆盖**（退出码 2 + `error.kind = output_exists` + `path`，旧文件字节不变）、
  **无意义的参数组合会报错**（`simulate --dry-run` 不带 `--render` → 退出码 2，而不是悄悄忽略）；
  `movie` 那条还会断言帧序列的**像素摘要两次录制一致**（确定性）与断点续录，
  内置编码器那条则断言 GIF 魔数/结束标记、APNG 的 `acTL` 帧数与循环次数，
  以及「换编码器 digest 不变」；
  MCP 那一条还会走完整的 stdio 握手 → `tools/list` → `tools/call`。
* **大模型测试**（`tests/llm_mock.rs`，9 个）起一个**假 OpenAI 服务**（`TcpListener`），
  覆盖 Content-Length 与 chunked 两种响应、Markdown 代码块里抠 JSON、
  HTTP 4xx 报错、连接被拒时的建议、环境变量优先级——不需要真的模型。
  还有一个**假 `curl`**：它抄下自己的 argv，断言 `https` 路径的 API key 不在命令行参数里、
  且配置文件权限是 `-rw-------`（把 key 塞回 argv，这条测试会真的变红）。
* **agent 循环测试**（`tests/agent_tools.rs`，5 个）用假模型发出真实的 `tool_calls`，
  验证「调用 → 执行 → 结果回喂 → 收尾」全链路、同一回复里的多个工具、
  工具失败时把诊断回喂、以及往返上限不会死循环。
* **零分配回归**（`tests/steady_state_allocations.rs`，1 个）：计数分配器断言物理稳态
  每步堆分配为 0、活动内存不增长。
* **并行 == 串行（逐位）**（`api_tests.rs`，3 个）：无关节快速路径、有关节交错路径、
  以及**换线程数**（2/3/4 线程）结果不变；每个测试都先断言「并行真的启用了」，
  再逐位比较全部刚体状态。
* **像素级能力验证**（`crates/quench3d-render/tests/`）：alpha 混合与双面渲染
  必须改变画面；**点光源必须真的亮**（单盏红灯：旧 SoA/AoS 布局错位时它完全不亮）；
  另有 `frame_uniform_layout_matches_wgsl` 把 Rust 与 WGSL 的结构体偏移量钉死。
* **整岛休眠/唤醒**（`api_tests.rs`，2 个）：一摞箱子必须同步入睡且不来回醒；
  睡着的一摞被推动后整岛一起醒。
* **island 划分**（`island.rs`，6 个）：独立簇/静态体不粘连/关节合并/编号确定性/退化输入；
  公开 API 契约测试还验证了「两个独立的球 = 2 个岛」「叠三层箱子 = 1 个 3 体的岛」。
* **宽相暴力对照**（`broadphase.rs::pairs_match_brute_force_on_a_dense_scene`）：
  400 个代理（含静态/睡眠/全局）的宽相输出必须恰好等于 O(n²) 暴力枚举的 AABB 相交集合。
* **物理行为锁**（`quench3d-physics` 的 `digest_tests`，4 个）：60 步紧容差参考、
  400 步物理不变量、400 步摘要警报；外加 `world_inv_inertia_tests` 证明
  「预计算世界逆惯量」与原来的两次四元数旋转数学等价（最大相对偏差 1.03e-6）。
* **自动修复测试**（`tests/ask_repair.rs`，3 个）验证「模型第一次给错 → 诊断回喂 →
  第二次修好」这条闭环，以及修不好时按 `--repair-rounds` 停下。
* **单元测试**（`src/`，82 个）覆盖场景解析与校验、规模上限诊断、结构诊断路径、生成与修复提示、
  信封与退出码映射、MCP 协议形状与轨迹、agent 工具定义、base64、参数解析
  （含「schema 里每个带取值的参数都必须登记进 VALUE_FLAGS」这条守卫）。

渲染侧的数值验证不靠肉眼：`offline_render` / `shadow_probe` 会打印
「天空占比 / 网格占比 / 阴影像素占比 / 阴影质心与解析落点误差」等指标。

下面这些自检都可以直接放进 CI（前三个带断言，失败即非零退出）：

```bash
cargo run -p quench3d --example headless                     # 240 步物理 + 出图 + 断言不穿地/无 NaN
F3D_JOINTS_HEADLESS=1 cargo run -p quench3d --example joints  # 关节自检 + 出图
F3D_MODEL_HEADLESS=1 cargo run -p quench3d --example model_viewer  # 模型加载 + 碰撞体 + 丢球断言
cargo run -p quench3d-render --example shadow_probe           # 自阴影 + 阴影方向（误差 < 1px）+ 级联 sweep
cargo run -p quench3d-render --example offline_render         # 天空/网格/阴影/地面逐项开关比对
node tools/check_web.mjs                                     # Web 产物自检（页面 import + wasm 实例化）
```

命令行工具也有可直接进 CI 的冒烟（见 `.github/workflows/ci.yml` 的「CLI 冒烟测试」）：

```bash
quench3d example --out scene.json
quench3d simulate scene.json --steps 300 --json   # 不需要 GPU，断言无 NaN、不穿地
quench3d validate bad.json --json                 # 断言退出码 2 且诊断带路径
quench3d render scene.json --out out.png --json   # 有 GPU 出图，无 GPU 必须是退出码 3
quench3d render scene.json --out out.png --golden 参考.png   # 单帧像素回归；不匹配退出码 1
```

