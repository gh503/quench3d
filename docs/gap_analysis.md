# Quench3D 功能完整性审计（对标行业实践）

> 审计时间：2026-10-15 · 审计范围：`crates/` 全部源码 + `tools/` + `scene.json` 格式 + 文档
> 规则：每一条「有 / 没有」都必须给 `文件:行号` 证据；写不出证据的一律标「未验证」。
> 本文档由多路独立审计合并（物理 / 渲染 / 场景格式 / 引擎与平台层），冲突处已复核。

---

## 1. 一句话结论

Quench3D 目前的定位是**一个垂直切片**：自研物理 + 自研渲染 + agent 友好的 CLI 这条主线是**真的打通了**
（确定性物理、岛并行、整岛休眠、离屏渲染、MCP、场景 JSON、回归防线都在），
但**离"功能完整"还有明显距离**，缺的是"做真实项目必然马上撞上"的那一层：
动画、贴图材质、层级、实例化、触发器/事件、后处理。

如果按「能不能用来做东西」分档：

| 用途 | 现在能不能 |
| --- | --- |
| 物理仿真 / 参数扫描 / 教学演示 / CI 里出图 | ✅ 能，而且比多数自研引擎扎实（确定性 + 零依赖） |
| 静态模型展示、截图、关卡白模 | ✅ 能 |
| 小游戏原型（方块/球、刚体玩法） | 🟡 能，但要自己写引擎缺的那层（UI、音效、存档） |
| 带角色/骨骼动画的游戏 | ❌ 不能（无动画系统、无蒙皮） |
| 有贴图材质的场景（哪怕只是"木纹箱子"） | 🟡 引擎能，但 `scene.json` 表达不了（见 §3），只能靠 glTF 带进来 |
| 大场景（数千物体、LOD、剔除之外的优化） | 🟡 有实例化与视锥剔除，缺 LOD / 遮挡剔除 / 合批策略 |
| 商业化发布（桌面三平台 + Web 商店） | ❌ 还不能（打包、音频、UI、存档、崩溃上报都缺） |

---

## 1.1 修复进度（2026-10-15 第二轮）

第一轮审计列出的 P0 里，**"骗人"那一类已经清完**（每一条都有配套测试，不是"改了代码"就算）：

| 项 | 状态 | 证据 |
| --- | --- | --- |
| P0 #1 静默失效 12 条（§3.3 十条 + 渲染两条） | ✅ 全修 | `scene_doc.rs` 新增校验（重复名、scale、关节单位、preset 取值、ground 摩擦、模型路径按加载器搜索路径、凸包近似告警）；`runner.rs` 让 `render.enabled`/`preset:"none"` 真正生效；`material.rs`+`renderer.rs` 让 `alphaMode: BLEND` 与 `double_sided` 真的改变画面。测试：`silent_fallbacks_are_now_reported`、`unit_and_render_only_traps_are_warned_about`、`render_refuses_scene_that_disables_rendering`、`material_pipeline_traits.rs`（**像素级**：左右半幅对比 alpha 混合与背面剔除） |
| P0 #3 `LICENSE-MIT` / `LICENSE-APACHE` | ✅ 已补 | 仓库根两份全文（Apache 只改了附录版权行），README §11 说明双许可 |
| P0 #8 无头"推进 N 帧"API | ✅ 已补（第一轮） | `render_sequence` / `render_sequence_with_game`；`movie` 的 `digest` 已是端到端像素断言 |
| P0 #8 后半：单帧 `render` 的 golden 回归 | ⬜ 仍未做 | `render` 只断言"是合法 PNG"，没有像素基线 |
| P0 #9 运动学推不动睡眠刚体（真 bug） | ✅ 已修 + 回归用例 | 根因：宽相把"运动学体"当成不可动（`world.rs` 的 `movable` 注释里写了这段历史），候选对根本不会生成，唤醒逻辑走不到。用例 `moving_kinematic_body_wakes_and_pushes_a_sleeping_body`（**回退修复即变红**）与反向约束 `kinematic_body_at_rest_does_not_wake_sleepers` |
| P0 #9 后半：`wake_up_all` / 公开 `sleep()` | ⬜ 仍未做 | `world.rs` 里仍无这两个 API |
| P1 #30 文档数字与门禁对齐 | 🟡 大部分已做 | 测试数 285→**337**（物理 121、CLI 97、渲染 52、数学 32、门面 22、编辑器 2、doctest 11）、行数 3 万→**3.7 万（src）**、重复的 `## 3.` 标题已消、示例数已对（10 个）、MSRV 由 1.82 改为 **1.90**（编辑器单独 1.95，因为 egui 0.37 要求它）；本机与 CI 工具链**都升到了 1.99 并真跑过**——CI 第一次跑到"全部测试"这一步就抓出 4 个只在 CI 上暴露的问题（无 GPU 的 doctest、3 核机器的线程数断言、macOS 没有 GL 的候选断言、Windows 的 cp1252 读不了中文 JSON），全部已修。`cargo doc` / `--locked` 仍未进 CI |
| P2 "地面可配摩擦" | ✅ 已做 | `ground.friction` / `ground.restitution`，取值校验 + 测试 |
| P2 "`scale` 与碰撞体一致性诊断" | ✅ 已做 | `objects[i].scale` 现在会告警"只影响渲染" |
| 仿真视频的内置编码器（`docs/simulation_video.md` 第 4 项） | ✅ 已做 | `--encoder gif\|apng`：纯 Rust、流式写入、换编码器不改像素 `digest`；GIF 的 10 ms 延迟粒度会在 `data.notes` 里算给用户看 |
| 暂停语义（`paused` 与文档矛盾） | ✅ 已修 | `time` 暂停时不再增长（`frame_count`/`fps` 仍更新，因为它们描述渲染本身）；`advance_clock` 有单测 `paused_freezes_simulation_time_but_not_frame_stats` |

**仍然挡路的（不是"缺功能"，是"没接上"）**：

* ~~**P0 #2 项目仍未纳入 git**~~ ✅ **已解决（2026-10-16）**：`quench3d` 成了独立公开仓库
  <https://github.com/gh503/quench3d>（main 分支），CI 矩阵（ubuntu / macOS / Windows / wasm32）
  与 Pages 部署都真的跑过、结论可查（例如 CI run `38763387386`、Pages run `38763387263`）。
  第一次真跑就抓到一个只在 CI 上出现的问题：CI 用 stable（1.99）做 `clippy -D warnings`，
  而本机当时是 1.94 —— 1.99 新增的 `clippy::chunks_exact_to_as_chunks` / `drain_collect`
  让四个 crate 直接红。**本机工具链已升到 1.99 与 CI 对齐**，这类"本地绿、CI 红"的漂移随之消失。
* 剩余大件（都在 §4 P1/P2，不是本轮范围）：
  渲染大件（IBL/动画蒙皮/点光源阴影/景深与 TAA）、脚本层、音频。
  （~~复合形状~~ ✅ 第 25 轮；~~关节限位 / 马达 / 锥限位 / 断裂~~ ✅ 第 23 轮；
  ~~关节锚点 + **按端给的局部轴**~~ ✅ 第 29 / 56 轮；~~事件总线~~ ✅ 第 24 轮 —— 见 §4 P0 #10~#12 与 #29。）
  （~~查询补全 + 加速结构~~ ✅ 2026-10-17 已做，见 §4。）
  （~~父层级~~ ✅ 2026-10-17 第 16 轮已做，见 §3.2 与 §4。）


## 2. 能力矩阵（分域）

### 2.1 渲染（`crates/quench3d-render/`，~3.9k 行含 WGSL）

架构：**单 forward + uber-shader**，无 RenderGraph；后处理链**已有骨架**（线性 HDR 中间目标 +
`post.wgsl` 统一 tonemap + bloom），但还没到"可插拔 pass 图"的程度。定位是"单机预览 / 离线截图"，
不是"能跑内容的引擎"。

**扎实的部分**（证据已复核）：Cook–Torrance PBR 全套 BRDF 项（`shaders/pbr.wgsl:79-103,210-213`）、
切线空间法线贴图（手写 TBN，`:163-169`）、单平行光阴影（纹素对齐拟合 `light.rs:47-69` + 正面剔除 +
斜率偏移 + **3×3 PCF** `pbr.wgsl:116-135`）、8 个点光平滑衰减（`light.rs:103`、`common.wgsl:30`）、
程序化渐变天空（`sky.wgsl:12-42`）、指数距离雾、MSAA（1–8，缺省 4）、
**视锥剔除**（6 平面 Gribb–Hartmann `camera.rs:174-214` + 逐实例 AABB `renderer.rs:1222`）、
按 (mesh, material) 合批 + 实例化（`renderer.rs:964-985,992`）、调试线段 12 图元、
离屏渲染与读回（含行对齐去 padding、BGRA 交换）、表面生命周期处理完整（Suboptimal/Outdated/Lost、resize）。

**骨架级空缺**（决定"能不能做产品"）：

| 优先级 | 缺什么 | 证据 | 现实后果 |
| --- | --- | --- | --- |
| P0 | **GPU 蒙皮 + 骨骼动画（仍无）** | 顶点只有 pos/normal/uv/tangent（`mesh.rs:13-22`），没有 JOINTS_0/WEIGHTS_0；第 61~62 轮补的是**节点级 TRS 动画**（`animations` 解析 + CPU 采样 + `models[].animation` 播放），不是蒙皮 | 带骨骼的角色 GLB 仍只能**静止**；机械/节点动画已可播（见 #23）；人物/怪物动画还做不了 |
| P0 | **无 CSM 级联阴影** | `cascade`/`csm` 全仓 **0 命中**；单张 2048 正交深度图（`renderer.rs:474`） | 大场景（>100m）阴影直接糊掉；小了又浪费精度 |
| P1 | ~~无后处理链、无 HDR 中间目标~~ ✅ 2026-10-16 | `rgba16float` 中间目标 + `post.wgsl`（ACES 只此一份）+ 半分辨率 bloom；tonemap 从三个物体着色器里删掉 | **已修**：混合/Alpha/网格线现在发生在 tonemap **之前**（此前是"先 tonemap 再混合"，能量错）。仍缺景深 / TAA / 可插拔 pass 图、AO 去噪（FXAA ✅ + SSAO ✅ 2026-10-16） |
| P1 | **无 IBL / 环境贴图** | 只有解析式半球环境光（`pbr.wgsl:245-252`）；材质绑定组仅 4 张 2D 贴图（`renderer.rs:413-433`） | 金属/光滑材质没有反射，观感对不上 Blender 预览 |
| P1 | **无聚光灯、点光无阴影** | `spot` 全仓 **0 命中** | 室内、手电、舞台灯做不了 |
| P1 | 透明物体之间不排序 | 排序键 `(alpha_flag, material, mesh, idx)`（`renderer.rs:1240-1248`） | 玻璃叠玻璃、树叶穿插会出现错误遮挡 |
| P2 | 无 AO 贴图、无 HDR/浮点纹理、无 KTX2/BC/ASTC | `params2.z` 硬编码 1.0（`material.rs:217`）；只支持 `Rgba8Unorm(Srgb)`（`texture.rs:284-288`） | 大贴图包体积、缺少接触阴影细节 |
| P2 | 无粒子 / billboard / 贴花 | `particle`/`decal` 全仓 0 命中 | 火、烟、雨、命中特效、弹孔都没有 |
| P2 | 流体只有"半成品"：解析浮力 + 双向耦合的粒子求解器，**粒子还画不出来** | 第 63 轮 `waters`（排开体积浮力 + 半透明平面）+ 第 64 轮 `fluids`（CPU PBF：密度约束/迭代/XSPH + 冲量反推 + 自由表面浮力，`fluid.rs`）。没有表面张力、涡量、真正的自由表面网格 | 水花/波浪/烟仍做不了；M4 粒子渲染是最后一批 |
| P2 | 无 LOD / 遮挡剔除 / 空间加速 | 逐实例线性剔除（`renderer.rs:1215-1232`）；`occlusion_query_set: None` 是占位（`:1285`） | 数千物体时 CPU 剔除本身成瓶颈 |
| P2 | 无 UI / 文字渲染 | `font`/`glyph`/`sdf` 全仓 0 命中 | HUD、标签、菜单全部要靠宿主自绘 |
| P2 | 无多摄像机 / 多视口 | `set_viewport` 0 命中；单相机单 target（`renderer.rs:1182-1189`） | 分屏、小地图、后视镜、离线多角度批渲染做不了 |

**现存缺陷（第一轮复核时"做错了"的那些——1、2 已在 2026-10-15 修掉，3–6 仍在）**：

1. ~~**只靠贴图 alpha 的透明材质会被判成不透明**~~ ✅ **已修**：`MaterialDesc.alpha_blend` 显式开关 +
   glTF `AlphaMode::Blend` 置位，判据变成 `(alpha_blend || base_color.w < 0.999) && alpha_cutoff <= 0`；
   像素级测试 `alpha_blend_flag_makes_texture_alpha_show_through`（同一帧左右半幅对比，旧代码为红）。
2. ~~**`double_sided` 从未接到管线**~~ ✅ **已修**：不透明管线按 double_sided 分流到
   `cull_mode: None` 的双面管线（着色器本来就按 `front_facing` 翻转法线）；
   像素级测试 `double_sided_flag_renders_back_faces`（背面像素数 左≈0 / 右>200）。
3. **离屏读回阻塞渲染线程**：`map_async` 后 `poll(wait_indefinitely)` + `mpsc::recv()`（`renderer.rs:1486-1490`），
   且截图由渲染线程调用（`facade/app.rs:213-221`）→ 连续截图会卡帧。
4. **GPU 资源只增不减**：`meshes/materials/textures` 无任何 `remove_*`/`destroy` API（0 命中），资源表 append-only
   （`renderer.rs:262-267`）→ 反复 `load_model` 会持续涨显存（每帧不泄漏，但生命周期不可回收）。
5. **地面网格的 `frag_depth` 无效**：管线 `depth_write_enabled: false`（`renderer.rs:650-656`），
   所以 `grid.wgsl:22,80` 的深度写入被忽略，而注释声称"写入真实深度" → 网格不遮挡其后绘制的调试线段。
6. **阴影尺寸有两个来源**：`DirectionalLight.shadow_map_size`（`light.rs:19`，用于纹素吸附）与
   `RendererOptions.shadow_map_size`（`renderer.rs:474`，实际纹理尺寸）不一致时会算错纹素吸附。

**做得对、值得保留的取舍**：没有 shader 变体机制（所以不存在排列组合爆炸），代价是无条件采样 4 张贴图 + 8 光循环；
`ShadowMap` 用 `Depth32Float`、mipmap 用 CPU 盒式滤波（`texture.rs:165-202`，仅在纹理上传期，渲染目标无 mip）——
在 llvmpipe/WebGL2 这类受限环境上是合理选择。

### 2.2 物理（`crates/quench3d-physics/`，零第三方依赖 + `#![forbid(unsafe_code)]`）

**扎实的部分**：7 种形状（Sphere/Box/Capsule/Cylinder/ConvexHull/Plane/TriMesh）；窄相 = 平面/球解析
+ Box-Box SAT（15 轴 + 参考面裁剪，最多 4 点流形）+ 其余凸-凸 GJK/EPA + TriMesh 逐三角四级兜底；
宽相 = 自适应均匀空间哈希（13 前向邻居 + 超大物体升全局代理 + 排序去重 → 确定性）；
接触缓存热启动；顺序冲量 + split impulse 位置修正（Baumgarte 0.2 / slop 0.005 / 修正速度上限 3.0）+ 松弛 pass；
5 种关节（Fixed / Ball / Hinge 限位+速度马达 / Distance 弹簧 / Slider 限位）+ 关节热启动；
并查集岛划分 + **整岛同步休眠**；native 多岛并行且**与串行逐位相同**；
查询（raycast / raycast_filtered / shape_cast / overlap_sphere）；角色控制器 move_and_slide（台阶 / 贴地 / 坡度）；
黄金摘要确定性测试 + CLI 稳态零分配回归。

**骨架级缺失**：

| 优先级 | 缺什么 | 证据 | 做不了什么 |
| --- | --- | --- | --- |
| P0 | ~~**碰撞过滤 layer/mask/分组**~~ ✅ 2026-10-16（`objects[].collision_layer` / `collides_with`，宽相即过滤；查询不看过滤） | 队友互不碰撞、单向平台、碎屑分层、布娃娃自碰撞关闭 |
| P0 | ~~**传感器/触发器 + 碰撞事件**~~ ✅ 2026-10-16（`objects[].sensor`；`PhysicsWorld::sensor_events()` 报进入/离开，`simulate` 报告里也有；**不唤醒睡眠体**、没有回调/事件总线） | 拾取区、伤害区、检查点、门与音效 |
| P0 | ~~**CCD / 推测接触**~~ 🟡 2026-10-16（`objects[].ccd`：每子步一条中心射线 + 只截法向位移；**不是**精确 TOI、只扫中心线、无旋转扫掠） | 子弹穿薄墙、快速下落物穿三角网格 |
| P0 | ~~**复合形状**~~ ✅ 2026-10-18（`objects[].shapes` + `BodyBuilder::part`；质量/惯量合成、碰撞/查询逐部件、每对最多 8 个接触点；**没有**部件级材质与部件编辑面板） | 桌子、载具、分层角色碰撞体只能多刚体 + 关节硬拼 |
| P0 | ~~**关节限位与马达补全**~~ ✅ 2026-10-18（Ball 锥形限位、Hinge 位置伺服、break force/torque、关节两端缺省不互相碰撞；**仍缺** 6DOF / 万向节 / 角度弹簧-阻尼 / 关节可视化） | 布娃娃（要锥限位）、悬挂载具、机械臂、可破坏门 |
| P1 | 接触特征 ID / 持久流形 / 接触余量 | `ContactPoint` 无 id；缓存按刚体对 + 局部锚点 0.02 容差就近匹配 | 4 层以上堆叠下沉抖动、滑动接触丢热启动、静止接触在 1e-4 边界闪断 |
| P1 | 查询补全 + 加速结构 | 只有单命中 raycast、球 overlap、平移 sweep，且 O(N) 线性扫描 | 框选、爆炸范围、AI 视线批量；万体场景不可用 |
| P1 | ~~物理材质与组合规则~~ ✅ 2026-10-17（`friction_combine` / `restitution_combine` / `static_friction` / `static_friction_speed`；**没有**各向异性摩擦与滚动摩擦） | 冰面/橡胶/泥地手感与配对策略 |
| P2 | 快照/回放；渲染插值；地形高度场；多体/articulation | — | 存档、netcode 回滚、高刷平滑、开放世界地形 |

**细节级发现**（这类"看不见的正确性"直接决定手感）：

1. **摩擦不是锥、是正方形**：两切向各自 clamp 到 ±μ·N（`solver.rs:505-520`）→ 对角线方向等效摩擦可达 **√2·μ**。
   PhysX/Rapier 按切向冲量**向量模长** clamp，没有这个各向异性。
2. **热启动不按特征 ID**：流形每子步重建，跨步靠"局部锚点 < 0.02"就近匹配（`solver.rs:291-312,426`）
   → 滑动/滚动接触会配错特征点、丢累积冲量。
3. **restitution 阈值实现正确**（默认 1.0 m/s，用求解前 vn 判定，`solver.rs:51,413-420`），但不能按材质下放。
4. **凸包质量属性是采样近似**：AABB 内 8³~16³ 网格采样（`shape.rs:411-478`），自测容差放宽到
   质量 ±7.5% / 惯量 ±9.4%；盒/球/圆柱/胶囊是闭式解。
5. **非法质量静默降级**（已复核）：`safe_inv` 把质量 ≤1e-9 变成 `inv_mass = 0`（`body.rs:48-54`）
   → `density: 0` 或 <4 点凸包的**动态**体会变成"受重力下落但推不动"的僵尸体，**没有任何错误返回**。
6. ~~**运动学物体唤醒不了睡眠刚体——真 bug**~~ ✅ **已修**（2026-10-15）：根因是宽相把"可动"写成
   `body_type == Dynamic && !sleeping`，于是"运动学体 × 睡眠刚体"这一对在 `!pa.movable && !pb.movable` 时被剔除，
   候选对根本不生成 → `world.rs` 的 kinematic 唤醒分支不可达。修法：字段改名 `movable` 并改成
   "动态体或运动学体且未睡眠"。回归用例 `moving_kinematic_body_wakes_and_pushes_a_sleeping_body`
   （把修复回退即变红）与 `kinematic_body_at_rest_does_not_wake_sleepers`（反向约束，防止睡眠优化失效）。
7. **静/动摩擦未分离**；线/角阻尼有，但不分轴、不作用于伪速度。
8. **零分配承诺有例外**：圆柱-半空间产生 16 个候选、凸包-半空间与 TriMesh 路径超出 `SmallVec` 内联容量 8
   → 这些形状每步仍会 malloc/free（`narrowphase.rs:23,144-164,692-711`）。

### 2.3 引擎 / 平台层 / 交付链

**交付链（最刺眼的三条，都是 P0，且都不是代码问题）**：

1. ~~**项目没有被 git 跟踪**~~ ✅ **已解决（2026-10-16）**：独立公开仓库
   <https://github.com/gh503/quench3d>，三平台 + wasm 的 CI 矩阵**整条全绿**
   （run `38778798444`：ubuntu / macOS / Windows / wasm32 四个 job 全 success）。
   "CI 是绿的"从此是可查的事实，不再是纸面结论——而且它第一次真跑就抓出 4 个
   只在 CI 上暴露的问题（无 GPU 的 doctest、3 核机器的线程数断言、
   macOS 没有 GL 的候选断言、Windows 的 cp1252 读不了中文 JSON）。
2. ~~**声明了双许可却没有 LICENSE 文件**~~ ✅ **已补**：仓库根 `LICENSE-MIT` / `LICENSE-APACHE` 全文，
   README §11 说明双许可与第三方依赖许可（gif/png 等均为 MIT/Apache-2.0）。
3. ~~**crate 元数据**~~ ✅ **已补齐**：`readme`/`keywords`/`categories`/`description`/`repository`
   （`https://github.com/gh503/quench3d`）都在。**仍未做**：`cargo publish --dry-run`、
   `cargo doc` 与 `--locked` 进 CI。

**应用层缺失**：~~父子层级~~ ✅（2026-10-17 第 16 轮：`objects[].parent` + 局部/世界变换 + 格式 v2）、
存档/快照 ❌、事件总线 ❌、输入映射/手柄/触屏 ❌、音频 ❌、脚本层 ❌、
资源异步与热重载 ❌、资源释放 ❌（只增不减，反复 `load_model` 必涨显存）、确定性回放（物理有、帧循环无）🟡。
~~**其中一条对 Web 目标是硬伤**：`assets.rs` 里 `wasm32` 零命中且依赖 `Path::exists()`（`:231`）
→ **浏览器里加载 glTF/OBJ 一律 NotFound**（现在 web 演示只用内置图元，所以没人撞上）。~~
✅ **已修**（2026-10-15）：新增不依赖文件系统的 `assets::load_model_from_bytes(name, bytes)`
（`.glb` 自包含、`.gltf` 外部引用会报错指路、`.obj` 只解析几何），wasm 下 `load_model(path)`
改报 `AssetError::Unsupported` 并在文本里点出该调哪个 API。**在真 wasm 里验证过**：
`tools/check_web.mjs` 跑示例导出的两个探针，断言"按路径加载"的报错含 `load_model_from_bytes`、
"从字节加载"解析出 `meshes=1 triangles=1`（`cargo check --target wasm32` 只能证明编译得过）。
原生侧 4 条单测含**手搓的最小 GLB**（一个三角形，格式每个字节都在测试里可见）。
**仍未做**：显存释放（CPU 侧缓存已做，但 GPU 缓冲仍只增不减）。

**可观测性**：物理 crate 连 `log` 依赖都没有（病态输入、数值夹紧全部静默）；无帧阶段/GPU 计时
（`timestamp_writes: None`）；bench 的内存统计只支持 Linux；渲染/CLI **无 golden、无图像哈希**
（CI 只查 PNG 头 8 字节 + 大小 >2000）；帧循环在 CI 里曾经从未被执行
（`Engine::frame` 私有、`render_offscreen_stats` 不跑 update/fixed_update）——
**2026-10-15 已补**：`render_sequence_with_game` 提供固定步长的无头帧循环，
`quench3d movie` 的端到端契约测试会真的跑它并断言像素摘要（见 [simulation_video.md](simulation_video.md)）。
~~**已复核的实现缺陷**：`paused` 时物理确实停，但 `time`/`frame_count`/`fps` 照常推进~~
✅ **已修**（2026-10-15）：`time` 暂停时不再增长，`frame_count`/`fps` 保持更新（它们描述渲染本身），
单测 `paused_freezes_simulation_time_but_not_frame_stats`；`render_sequence` 同规则。

**agent 向缺口**（这是本项目的卖点，所以单列）：错误**分类**已有（`error.kind` = invalid/env/io/scene + 退出码 2/3/4），
~~细粒度错误码~~ ✅ **2026-10-19 第 55 轮**：**校验诊断**每条都带稳定 `code`（第 60 轮：18 个码；`docs/error_codes.md`
+ `quench3d schema --json` 的 `error_codes` + 系统提示三处同步，逐格对账/稳定性/e2e 守卫测试，变异验证过）。
现在 agent 能按 `diagnostics[].code` 而不是中文文案分支（`asset_not_found` vs `invalid_type` 一眼可分）。
**仍无**：顶层 `error.kind` 与运行期 `RunError` 的细分（`ErrorInfo` 在 `report.rs`）；
~~**不幂等**（默认输出名固定 `output.png`、`--out` 直接覆盖 → 多 agent 并行必然互踩）~~
✅ **已加保护**（2026-10-15）：`--no-clobber`（输出/帧目录已存在就退出码 2、`error.kind = output_exists`、
带 `path` 与 `hint`，一个字节都不动）与 `--dry-run`（只校验 + 跑物理 + 采探针，**不建 GPU、不写盘**，
报告给 `would_write`）。MCP 的 render/movie 工具也透出了这两个参数。缺省行为逐位不变（有基线比对）。
仍无 diff/patch/增量操作；
无 record/replay 与随机种子；MCP 工具缺 `annotations`/`outputSchema`，resources/prompts 永远为空；
LLM 链路无流式/重试/token 预算；
~~⚠️ **安全问题：API key 经 curl 命令行参数传递**（`llm.rs:680-682`），`/proc/<pid>/cmdline` 可读~~
✅ **已修**（2026-10-15）：https 路径把 url 与 `Authorization` 头写进 **0600** 临时配置文件、
用 `curl -K <文件>` 读入（请求体仍走 stdin，提示词不落盘），请求结束立刻 RAII 删除。
防线是"假 curl"测试：它抄下自己的 argv，断言里面搜不到密钥、且配置文件权限是 `-rw-------`——
把密钥塞回 argv 时该测试**确实会失败**（已实测反证）。

**文档漂移**（2026-10-15 已对齐）：README 测试数改为实测 **298**（口径分域同步）、行数改为实测 **3.6 万**、
示例清单 9 个与目录一致、重复的 `## 3.` 标题已消、"ab_bench 在 CI"与 island 并行/CSM 的失实描述已改，
MSRV 由 1.82 改为 **1.90**（依据依赖声明的 `rust-version`，未实编译验证）；`cli_e2e` 的条数改为实测值。
**仍未做**：`cargo doc` / `--locked` 进 CI、单帧 `render` 的像素 golden。

### 2.4 行业对标：还差什么（2026-10-15 第二轮复核）

> 对标物按域取行业事实标准：渲染对标 Filament / Godot 4 / three.js / Unity URP；物理对标
> PhysX 5 / Jolt / Rapier / Box2D / MuJoCo；内容与玩法对标 Unity / Godot / Bevy；场景与资产管线
> 对标 glTF 生态 / Blender；交付工程对标 Bevy / three.js。
> **证据边界**：本仓库每条状态都给 `文件:行号`（本轮全部重新 grep 复核，含"0 命中"这类否证）；
> 对标物的能力属行业惯例层面的判断，本文没有逐条引用其官方文档——请按这个边界采信。

#### 档 A：已经越过行业及格线（别妄自菲薄）

| 能力 | 我们的证据 | 对标位置 |
| --- | --- | --- |
| 确定性物理 + 可断言摘要 | 黄金摘要测试；并行 == 串行逐位一致（`api_tests.rs`） | 多数自研引擎没有；PhysX/Jolt 也只在特定配置下保证 |
| 物理零第三方依赖 + `forbid(unsafe_code)` | `quench3d-physics` | Rapier/Box2D 都是外部依赖；可审计性更强 |
| 无头逐帧 + 像素 `digest` + 内置 GIF/APNG | `render_sequence`、`movie --encoder` | 对标 Godot `--write-movie`、Blender 图像序列 |
| agent 自描述与机器可读输出 | `schema` 命令、单行 JSON 信封、`error.kind`+退出码、MCP stdio | three.js/Unity 无等价物，这是本项目的差异化 |
| 整岛同步休眠 + 多岛并行 + 稳态零分配回归 | `island.rs`、`steady_state_allocations.rs` | 与 Box2D/Bullet 同类机制同档 |
| island 并行结果逐位不变 | `api_tests.rs`（换线程数结果一致） | 少见；多数引擎并行后放弃逐位确定性 |

#### 档 B：差"接线"（引擎已有、格式/API 没透出）——性价比最高

| 缺口 | 引擎侧（已有） | 格式侧（缺） | 挡住什么 |
| --- | --- | --- | --- |
| 材质贴图 / alpha / 双面 / AO | `MaterialDesc` **18 个字段**（含 4 个贴图槽位：base_color/normal/metallic_roughness/emissive） | ✅ **已接线**（2026-10-15）：`alpha`、`emissive_color`、`uv_scale`、4 个贴图路径、`normal_scale`、`alpha_cutoff`、`alpha_blend`、`unlit`、`double_sided`。贴图路径相对**场景文件**解析（回退 assets 搜索路径），按路径+色彩空间缓存。AO **仍未接**——不是接线问题：`MaterialDesc` 里根本没有 AO 贴图槽（着色器 `params2.z` 预留了强度但无贴图寄存器），属 P2 需先加渲染能力 |
| 点光源与光照细节 | 8 个点光 + 平滑衰减（`light.rs:103`） | ✅ **已接线**（2026-10-15），且**顺带修掉一个真 bug**：`FrameUniforms` 把点光源写成两个并列数组（SoA），WGSL 的 `array<PointLight, 8>` 是结构体数组（AoS），布局错位 → **只给一盏灯完全不亮**、多盏灯时"颜色/强度"读到的是下一盏灯的位置。修复后 studio 预设两盏补光颜色才对，**示例场景画面因此变化（是修复，不是回归）**。回归防线：`frame_uniform_layout_matches_wgsl`（偏移量钉死）+ `tests/point_lights.rs`（单盏红灯必须照亮且变红） | 
| 相机 near/far/正交 | `Camera.near/far/projection`，含 `Projection::Orthographic`（`camera.rs:7-33`） | ✅ **已接线**：`near`/`far`/`orthographic_height`（缺省 `None` → 与以往逐位一致的透视）。**仍未做**：多相机/多视口（`set_viewport` 0 命中） |
| 关节锚点与局部轴 | `local_anchor_a/b`、`local_axis_a/b`、`local_frame_a/b`（`joints.rs:34-125`） | ✅ **已接线**：`anchor_a`/`anchor_b`（局部坐标，第 29 轮）+ `axis_a`/`axis_b`（**按端**的局部轴，不写 = 回落 `axis`，第 56 轮）+ `frame_a`/`frame_b`（固定关节的**参考姿态**，欧拉角度；第 59 轮起引擎真的按它把两端扭到参考姿态重合，实测数字见下表）。`axis` 的「世界轴」错误注释已改成局部轴，零轴报错。**仍未做**：关节可视化（轴 / 锚点 / 参考姿态的 gizmo） |
| 物理参数 | `BodyBuilder` 的 damping/gravity_scale/mass/allow_sleep/angular_velocity | ✅ **已接线**：`linear_damping`/`angular_damping`/`gravity_scale`/`mass`/`allow_sleep`/`angular_velocity`。非 `dynamic` 刚体写这些字段会**警告"会被忽略"**（静默忽略是最难查的坑）。顺带给 `simulate` 报告加了 `rotation`（四元数 xyzw）——否则"原地转起来"在报告里和静止完全一样 |
| 行为与探针 | `contacts()` 轮询 + 轨迹 `trace` | ✅ **已接线**：`actions[]`（impulse / impulse_at_point / torque_impulse / velocity / angular_velocity / force / teleport / wake，按 `at_step` 或 `at_time` 触发）+ `probes[]`（球形/盒形区域，报告谁在第几步进入、第几步离开、区内最高速度与进入次数）。探针**只读**：同一场景多挂探针，刚体状态逐字节不变（有回归测试钉着） | 无需宿主代码即可表达"第 2 秒推一下""碰到就记录" |

档 B **全部关闭**（含最后一条"行为与探针"）。每一条都有**端到端像素/数值证据**
（不是"代码写完了"就当完成）：

| 接线项 | 证据（实测数字） |
| --- | --- |
| 材质贴图 | 2×2 纯绿贴图 → 箱子中心 `(26.2, 228.9, 30.0)`；不带贴图 `(227.9, 228.9, 230.2)`；贴图路径写错 → 退出码 2 + 诊断路径 `materials.m.base_color_texture` |
| 点光源 | 同一个箱子：无灯时 `(39.6, 51.3, 90.6)`，一盏红灯 intensity=200 → `(241.2, 173.9, 199.3)` 且 R 通道主导；`point_lights: []` 与"不写字段"的图**必须不同**（预设补光） |
| 相机 | 透视下箱子 324 px；`far: 3`（箱子在 6 米外）→ **0 px**；`orthographic_height: 1` → **14400 px**（正好占满 120px 高 × 120px 宽） |
| 刚体参数 | `gravity_scale: 0` → y 停在 5.000；缺省 → y=-14.783。`linear_damping: 3` → x=3.32；无阻尼 → x=20.00。`allow_sleep: false` → sleeping=0；缺省 → sleeping=1。质量 1 → B 被推出 4.16 米；质量 50 → 8.15 米。`angular_velocity: [0,0,8]` 转 1 秒 → 四元数 `(0,0,-0.753,-0.658)`（= cos/sin(4 rad)，与 8 rad/s 吻合） |
| 关节锚点 | 球关节 `anchor_b=[1,0,0]` → B 的质心离 A 原点 **1.009 米**；不给锚点 → 0.009 米 |
| 关节局部轴（第 56 轮） | 门板（`rotation_deg:[90,0,0]`）绕世界 Y 的铰链：按端写 `axis_a=[0,1,0]`/`axis_b=[0,0,1]` → 1 秒后转过 **80.4°**、门板薄轴 `\|n·Y\|=0.0002`（还是立着的门）；两端共用同一根局部轴 → `\|n·Y\|=1.0000`（门板被扭平）；两种写法的薄轴相差三个数量级。另：只写 `axis` 与"把 axis 显式抄到 `axis_a`/`axis_b`"的 `states` **逐字节相同**（不写 = 老行为） |
| 关节参考姿态（第 59 轮） | 静态底座 + 动态门板（锚点都在原点、门板出生 0°）写 `frame_a: [0,90,0]` → 门板停在绕 Y **90°**（与目标差 < 2°，再用引擎自己的 `fixed_frame_error_deg` 复量也 < 2°）；只写 `frame_b: [0,90,0]` → 停在 **-90°**；**写全 0 与完全不写**跑 120 步后每个刚体的位置/朝向按 `f32::to_bits()` **逐位相同**；不给 `frame_*` 时门板出生就是 90°、跑 3 秒后**仍差 90°**（老行为没被改）。引擎侧另有一组纯数值用例：1 秒内误差 < 5°、3 秒 < 2°、不过冲、锚点位移 < 0.05 米；两端都是动态体时也收敛（冲量两侧都作用）；被 3 rad/s 推歪后自己扭回来；驱动中的两端跑 10 秒也不睡（睡眠会把求解器里的刚体完全冻结，修正会停在半路） |
| 定时动作 | 5 kg 的球静止后在第 100 步给 30 N·s 冲量 → 末态 y=1.92（基线 0.495）、速度 2.73（基线 0）；设速度后横向飞出 x=2.90；瞬移 → x=3.000 且四元数 `(0,0,0.383,0.924)`（正好 45°）；角速度 3 rad/s 跑 1 秒 → 转角 **171.9°**（=3 rad，误差 <0.1°）；`impulse_at_point` 打在盒子边缘 → 同时平移 1.000 m 与自转（力臂算对了）；**`at_time: 1.0` 与 `at_step: 60` 的输出逐字节相同** |
| 区域探针 | 5 米落下的球：落点探针 `enter_step=46`、区内最高速 9.32 m/s、74 步在内；途中 1³ 盒探针 `enter=32 / exit=50 / steps_inside=18`；瞬移场景里"落点"的 `exit_step=60`、新位置探针的 `enter_step=60`（正好是瞬移那一步）；**加探针与不加探针的 `states` 逐字节相同** |

**兼容性**：所有新字段都是可选的，**不写字段就不会改变已有场景的行为**（材质那一轮用同一场景录 GIF 后
`cmp` 逐字节相同来验证）。唯一的例外是点光源布局修复：那是把算错的修对，画面必然变——
同一个箱子在 studio 预设下，修复前中心是 `(198.3, 202.5, 175.2)`（偏暖），修复后 `(162.2, 179.9, 202.2)`，
因为补光的"颜色"以前读到的是下一盏灯的**位置**。
留个可对照的数字：示例场景 60 帧 400×250 的 `movie` 摘要 —— 修复后 `fnv1a64:5631b3d008aea093`，
显式写 `point_lights: []`（完全不要点光）是 `fnv1a64:827cad9fd35078a3`，
而修复前的基线 GIF 与两者都不同（逐字节比较从第 41 字节起就不一样）。
（这两个摘要值测于**点光源修复那一轮**，场景未随仓库发布；HDR 后处理改造之后摘要又变了，
它们只作为"画面确实变了"的历史证据，不是可复现的基线。）
物理黄金摘要不受影响（`golden_digest_is_stable` 仍是 `0xbcc0bccf037b867f`，本轮没碰物理）。

**HDR 后处理那一轮（2026-10-16）**的兼容性记录：不写 `lighting.bloom` 时只有一条后处理
pass（tonemap），且**不透明/未被混合的像素逐位不变**——用一块无光照 0.5 灰的铺屏方块验证过，
msaa 1 与 4 都精确等于手算的 `encode(aces(0.5)) = 154`。变化只出现在**混合像素**：
带地面网格的示例场景（200×120，msaa 4）有 59.7% 的像素变化，全部是网格线/半透明像素，
差值集中在 +1…+3 个色阶（因为混合从"tonemap 之后"搬到了"tonemap 之前"），天空/不透明区域
Δ=0。这条不变量由 `crates/quench3d-render/tests/post_pipeline.rs` 逐像素钉住。

**Web 后端回退那一轮（2026-10-16）**：浏览器里的 `No available adapters` 是**引擎侧**的
bug，不是浏览器不支持——旧代码把 `Backends::all()`（含 `BROWSER_WEBGPU`）一把交给 wgpu，
而 wgpu 只要看到 `BROWSER_WEBGPU` 且 `navigator.gpu` 存在就**只**建 WebGPU 上下文、连
WebGL2 实例都不建；`navigator.gpu` 存在 ≠ 有适配器（Linux Chrome 没开 Vulkan、软件光栅
环境都这样）。现在改成：页面先用 `navigator.gpu.requestAdapter()` 探（**不碰画布**）→
写 `globalThis.__quench3d_backend` → 引擎按 `[WebGPU]`（探测到适配器）或 `[GL]`（没探到）
排候选，`?backend=gl|webgpu` 可强制。**为什么必须"先探"而不是"失败再换"**：一旦对着画布
要过 `webgpu` 上下文，同一张画布的 `getContext("webgl2")` 就返回 null（一张画布只能有一种
上下文，不可逆），回退在同一条路上根本走不通——这一点也写进了 `from_candidates_with` 的注释。

实测（本机 headless Chrome + SwiftShader，`--enable-unsafe-swiftshader --use-angle=swiftshader`，
即"有 `navigator.gpu` 但没有 WebGPU 适配器"的环境）：

| 场景 | 结果 |
| --- | --- |
| 默认（自动探测） | 打印「后端选择：gl（navigator.gpu 存在但没有适配器）」→ `Web 后端候选：[Backends(GL)]` → **第一帧已渲染 1280x720** |
| `?backend=gl` | 与默认**逐像素相同**（天空 194/206/219、地面 64/68/80） |
| `?backend=webgpu` | 按预期失败，错误里同时列出 WebGPU 无适配器与"画布已被占用"，并提示加 `?backend=gl` 刷新 |
| 失败那次（修复前） | 画布是页面底色 + 遮罩（17/24/42），什么都没画出来 |

WebGL2 上的 HDR 路径**没有拦路虎**：`EXT_color_buffer_float`（半浮点渲染目标）、
`EXT_float_blend`（往浮点目标混合）、`MAX_SAMPLES=4` 三者都在（ANGLE/SwiftShader 已具备，
真实 GPU 只会更强）。回归测试：`crates/quench3d-render/tests/backend_fallback.rs`（候选解析 +
"第一个候选拿不到适配器时必须换下一个"）、`tools/check_web.mjs`（wasm 里必须真的含 GL 后端、
页面必须在 `start();` 之前探完后端）。
**诚实边界**：真实 GPU 的浏览器（有 WebGPU 的机器）本机测不了，只测了软件光栅的 WebGL2 路径
与失败的 WebGPU 路径。

#### 档 C：差"整层"（真骨架，不是接线）

| 层 | 缺什么（**本轮 grep 的命中数**） | 对标物 | 挡住了什么 |
| --- | --- | --- | --- |
| 内容层 | GPU 蒙皮/骨骼动画（`skin\|skinning\|joint_palette` = **0**，loader 不调 glTF `animations()` = **0**）、morph（0）、粒子（0）、贴花（0） | Unity / Godot / Bevy 全有；skins/animations 是 glTF 核心扩展 | 导入任何角色 GLB **只能静止**；HUD/菜单/伤害数字要宿主自绘 |
| 手工界面 | ✅ **已有**（2026-10-16）：`quench3d-editor` 用 egui 做了工具栏/场景树/属性面板/视口拾取，桌面与 Web 共用（§2.5）。**引擎本体仍不带** UI/文字系统（`font\|glyph\|sdf` 在 `quench3d-render` 里仍是 0）——GUI 由宿主（编辑器就是这样一个宿主）自己选库 | Unity/Godot 的 UI 是引擎内建；Bevy 走 `bevy_ui` + 第三方编辑器 | 想在自己游戏里画 HUD，得自己接一个 UI 库（本仓库演示了怎么和 wgpu 渲染器共存） |
| 表现层 | HDR 中间目标 + tonemap + bloom ✅ + FXAA ✅ + SSAO ✅、CSM（0）、聚光灯（0）、IBL/环境贴图（0）、景深/TAA（0）、透明物体距离排序、纹理压缩与浮点纹理（只有 `Rgba8Unorm(Srgb)`） | Filament 的 HDR→tonemap→post 链、Unity URP、Godot 4 | 金属没反射；大场景阴影糊；大贴图包体积下不来；少了 AO/景深/抗锯齿这些"最后一公里"的观感项 |
| 玩法层（物理） | 碰撞过滤 layer/mask ✅、传感器/触发事件 ✅（轮询式，无回调总线）、CCD 🟡（中心射线式）、复合形状（1 刚体 1 形状）、关节限位/马达补全、持久流形/特征 ID、物理材质与 combine（0）、快照回放、渲染插值（`interpolat` = 0）、地形高度场（0） | PhysX 5 / Jolt / Rapier / Box2D 的标配 | 队友互不碰撞、拾取/伤害区、子弹穿墙、载具悬挂、存档与回滚、高刷平滑 |
| 工程/平台层 | 存档、音频（0）、输入抽象（手柄/触屏/重绑定 = 0，只有键鼠 + `movement_axes`）、脚本层（0）、渲染资源释放（渲染器 `remove_*` = **0**，资源表 append-only）、**Web 端 glTF/OBJ 加载（`assets.rs` 用 `Path::exists()`，wasm 下必 NotFound）**、发布工程 | Unity/Godot 的 Asset/Input/Audio/Save 子系统；Bevy 的 asset server | 项目做完变不成产品；Web 端连模型都加载不了 |

#### 档 D：agent 向（差异化优势，但仍有缺口）

已有：`schema` 自描述、单行 JSON 信封、`error.kind` + 退出码、MCP stdio、像素 `digest` 可断言、
帧序列可续录。仍缺：

* ~~**不幂等**~~ ✅ **已加保护**：`--no-clobber`（`error.kind = output_exists` + 退出码 2 + `path`/`hint`）
  与 `--dry-run`（不建 GPU、不写盘，只跑物理与校验）；MCP 的 render/movie 也透出这两个参数（P1 #28 大部分完成）；
* ~~无 `--dry-run`~~ ✅ **已做**（同上）；仍无增量/diff 操作、无随机种子与 record/replay（物理确定性有，帧循环回放无）；
* ~~MCP 缺 `annotations` / `outputSchema`~~ ✅ **已补**（2026-10-15）：八个工具都声明了
  `readOnlyHint` / `destructiveHint` / `idempotentHint` / `openWorldHint` 与 `outputSchema`，
  并有单测把声明与事实对齐（只读工具不许标"会破坏数据"、出网工具必须不幂等）；
  resources/prompts 仍然永远为空；
* ~~⚠️ **API key 仍经 curl 命令行参数传递**~~ ✅ **已修**：走 0600 配置文件 + `curl -K`，
  密钥不进 argv；"假 curl"测试抄 argv 断言搜不到密钥（反证过：塞回 argv 测试就红）；
  ~~仍无细粒度错误码~~ ✅ 2026-10-19 第 55 轮：校验诊断的 `diagnostics[].code` 已落地（见上）；
  仍无 LLM 流式/重试/token 预算。

#### 跨过"玩具 → 能用"的 5 件事（按性价比排序）

1. **材质贴图槽位 + 点/聚光 + 相机 near/far/正交 进 `scene.json`**：纯接线，引擎全有，视觉上限立刻抬起来；
2. ~~**碰撞过滤 layer/mask + 传感器与 begin/end 事件**~~ ✅ 2026-10-16（还差事件总线/回调：现在是每步轮询 `sensor_events()`）；
3. ~~**父层级**~~ ✅（第 16 轮）**+ 复合形状 + 关节锚点/限位/马达补全**：
   组合体（车 / 机械臂 / 布娃娃）的地基，父层级这块已经落地；
4. **GPU 蒙皮 + 骨骼动画 + glTF `animations()` 导入**：内容化的门槛；
5. ~~**HDR 中间目标 + 后处理链骨架**~~ ✅：bloom 已挂上（`post.wgsl` + `bloom.wgsl`），
   TAA/景深/CSM 可以直接往同一条链上加（SSAO 与 FXAA 已经加上去了）。

#### 有意不做（不算缺陷，写下来免得反复讨论）

* 内嵌 H.264/HEVC（GPL + 专利池）→ 交给 ffmpeg；内置只做免专利的 GIF/APNG；
* 物理零第三方依赖 → 换来 `forbid(unsafe_code)`、可审计与确定性，代价是几何/求解全自己写；
* 无 shader 变体机制 → 没有排列组合爆炸，代价是无条件采样 4 张贴图 + 8 光循环；
* 不做所见即所得编辑器 → 定位是"给 agent 与代码用的引擎"，`scene.json` 就是接口。

#### 最刺眼的三个非代码缺口

1. ~~`quench3d/` **未被 git 跟踪**~~ ✅ 独立公开仓库 + CI 整条全绿（见 §2.3）；
2. ~~`Cargo.toml` **缺 `repository`**~~ ✅ 已补；
3. ~~无 `CHANGELOG.md` / `CONTRIBUTING.md` / `rust-toolchain.toml` / `deny.toml`~~
   🟡 **部分**：这四个文件仍没有；版本与依赖策略目前写在 README §10 与工作流里
   （`Cargo.lock` 与 `web/pkg/` 是有意入库的）。

---

### 2.5 编辑器：第四个入口（2026-10-16 新增）

对标物：Godot/Unity 的场景编辑器、Blender 的视口 + 大纲 + 属性面板、Bevy 生态里的
`bevy_editor_*` 系（仍在早期）。差距最大的那一档一直是「**手工操作界面**」——
CLI / MCP / SDK 都是给程序用的，人想摆个场景只能写 JSON。本轮补上：

* `quench3d-editor`：工具栏（播放/暂停/单步/重置/增删/复制/清空/显示开关/相机模式/文件）、
  场景树、属性面板（变换 + 外观 + 物理参数）、状态栏；视口内左键环绕、右键平移、滚轮缩放、
  单击拾取；桌面与 Web **同一套 egui 界面代码**（Web 端就是 `web/index.html`）。
* 与 CLI 共用同一份 `quench3d.scene`：编辑器存的场景 `quench3d render/movie` 直接吃，
  反过来也成立。往返有测试钉着（物体名、位置、地面高度、刚体数量），
  且**这条测试当天就抓到一个真 bug**（默认地面用"压扁的立方体"时，视觉表面与物理半空间
  差半个板厚，存读一轮地面下沉 0.5）。
* 尺寸：`src/` 约 2.4k 行（另有 0.16k 行测试）。
* **"能点"有像素证据**（这一档最容易自欺：截图像界面 ≠ 界面能用）：
  在浏览器里用 DevTools 协议的 `Input.dispatchMouseEvent` 按坐标真点，
  点「播放」→ 工具栏差 6.76（文字变「暂停」）+ 视口差 4.19（方块掉下来）；
  再点 → 3.26，之后 8 秒视口 0.00（真停住了）；视口里点一下 →
  左面板 0.90 / 右面板 3.13（射线拾取真的换选中物体）。
  桌面侧的 XTest 合成按键/按钮在 XWayland 下**到不了客户端**（`CursorMoved` 有 3 条、
  `MouseInput`/`KeyboardInput` 0 条），所以这条证据取自浏览器——同一套界面代码。

**仍然缺的（按差距大小排）**：拖拽式变换 gizmo、撤销/重做、原生文件对话框、
多选与成组、场景内嵌脚本或行为编辑。
（导入模型进场景树、材质贴图选择、~~层级/父变换~~ 都已补上：分别见 §2.5 的模型实例一节、
`CHANGELOG` 的贴图槽条目、以及 2026-10-17 第 16 轮的父子层级。）
换言之：**"能手工摆一个图元场景"这一档已过关，"能手工编辑真实项目"这一档还差得远。**

## 3. `scene.json` 专项审计

**结论先说：`scene.json` 目前是「够跑 demo 和参数扫描」的粒度，不是「能支撑真实小项目」的粒度。
最硬的证据是：引擎能力 > 格式表达力，格式落后于引擎。**

### 3.1 现有字段（完整清单，`crates/quench3d-cli/src/scene_doc.rs`）

> 下表是 2026-10-15 的快照（**行号会随代码漂移，认字段名**）；此后新增的字段就地补在对应行里：
> `objects[].parent`（2026-10-17 第 16 轮，格式升到 v2）。

全格式**只有 4 处必填**：`materials.<名字>.color`、`objects[].shape.type`、`models[].path`、`joints[].type/a/b`——
其余全部可省，`{}` 本身就是合法场景（scene_doc.rs:9-13）。硬上限：`objects` ≤ 20000（:41）、
`steps` ≤ 1000000（:43）、`dt` ∈ (0, 0.5]（:1132）、`msaa` ∈ {1,2,4,8}（:1150）。

| 层级 | 字段 | 类型 | 默认 | 证据 |
| --- | --- | --- | --- | --- |
| 顶层 | `format` | string | `quench3d.scene` | :105-108 |
| | `version` | u32 | **2**（2026-10-17 起；不匹配直接报错，不做旧版迁移） | :111, :871 |
| | `name` | string? | — | :114 |
| | `gravity` | `[f32;3]?` | — | :117 |
| | `ground` | `{size, material?, y}`? | — | :178 |
| | `materials` | `map<名字, MaterialDoc>` | 空 | :123 |
| | `objects` | `ObjectDoc[]` | 空 | :126 |
| | `models` | `ModelDoc[]` | 空 | :129 |
| | `joints` | `JointDoc[]` | 空 | :132 |
| | `camera` | `CameraDoc`（**必填**） | — | :135 |
| | `lighting` | `LightingDoc` | — | :138 |
| | `simulate` | `{steps, dt}` | 0 步 / 1/60 | :464-470 |
| | `render` | `{output, width, height, msaa, enabled}` | 960×600 / MSAA4 | :489-505 |
| 材质 | `color` | `[f32;3]` | — | :199 |
| | `metallic` / `roughness` | f32 | — | :202, :205 |
| | `emissive` | **f32（只有强度）** | — | :208 |
| 物体 | `name`/`shape`/`position`/`rotation_deg`/`scale`/`material`/**`parent`**（按父物体名字引用；有它时 `position`/`rotation_deg` 是局部量，**缩放不继承**） | — | — | :288-303 |
| | `body` | `none\|static\|dynamic\|kinematic` | `none` | :273-286 |
| | `density`/`restitution`/`friction`/`velocity`/`visible` | — | — | :312-324 |
| 形状 | `type` | `box\|sphere\|capsule\|cylinder\|cone\|plane\|convex`（7 种；圆角盒/高度场见 #18） | — | :218-232 |
| | `size`（**边长**，不是半长）/`radius`/`half_height`/`normal`/`points` | — | — | :236-255 |
| 模型 | `path`/`position`/`rotation_deg`/`scale`/`collide`/`friction` | — | — | :334-351 |
| 关节 | `kind` | `fixed\|ball\|hinge\|distance\|slider` | — | :357-370 |
| | `a`/`b`（按名字引用）/`axis`/`limits`/`rest_length`/`stiffness`/`damping`/`motor_velocity`/`motor_max_impulse` | — | — | :373-401 |
| 相机 | `position`/`look_at`/`fov_deg`/`distance`/`yaw_deg`/`pitch_deg`/`frame_scene` | — | — | :407-428 |
| 光照 | `preset`/`sun_direction`/`sun_intensity`/`ambient`/`exposure`/`sky`/`grid`/`shadows`/**`point_lights`**/**`fog`**/**`shadow_quality`**/**`sky_colors`**/`bloom`/`fxaa`/`ssao` | — | — | `scene_doc.rs` 的 `LightingDoc` |

做得好的地方（不是缺点的部分）：全层级 `deny_unknown_fields` + 未知字段诊断会**列出全部合法顶层字段**
（:595-604），版本不匹配会明确报「不支持的版本 X，当前支持 1」（:871）；单位与轴向约定写在 README:430
和 `quench3d schema` 的文字里（schema.rs:224, :360）——对 agent 是友好的。

### 3.2 表达力边界（按严重度）

| # | 表达不了的东西 | 证据（引擎有 vs 格式没有） | 用户会撞的墙 |
| --- | --- | --- | --- |
| 1 | ✅ | **任何贴图** | 引擎 `MaterialDesc` 有 `base_color_texture` / `normal_texture` / `metallic_roughness_texture` / `emissive_texture` / `uv_scale` / `normal_scale`（material.rs:55-65）；`MaterialDoc` 一个都没有 | 「地板贴瓷砖」「箱子贴木纹」这种最基本需求在 JSON 里做不到，只能把物体做成 glTF 带贴图进来 |
| 2 | ✅ | **半透明 / alpha** | 引擎 `base_color: Vec4`（含 alpha）+ `alpha_cutoff`（material.rs:45, :67）；格式是 `color: [f32;3]`（:199） | 玻璃、水、淡出效果无法表达 |
| 3 | ✅ | **自发光颜色** | 引擎 `emissive: Vec3` + `emissive_strength`（material.rs:51-53）；格式只有 `emissive: f32`（:208） | 只能做"白光发光"，做不了霓虹/指示灯颜色 |
| 4 | ✅ 2026-10-17 | ~~**点光源 / 聚光灯**~~ **点光源已接线**（`lighting.point_lights`，最多 8 盏，超出给警告）；**聚光灯仍然没有**（引擎里也没有，见 §4 #24） | `PointLight`（light.rs:79）、`MAX_POINT_LIGHTS = 8`（light.rs:103）；`spot` 全仓 0 命中 | 室内布光可以了；手电/舞台灯还做不了 |
| 5 | ✅ 2026-10-17 | ~~**天空与雾参数**~~ 已接线：`lighting.sky_colors`（天顶/地平线/地面，线性色）+ `lighting.fog`（颜色 + 密度，不写 = 关闭）+ `lighting.shadow_quality`（强度/覆盖半径）。**没做**：线性雾的起止距离（引擎只有指数雾）、阴影贴图分辨率、天空盒贴图/IBL | 黄昏、雾天、自定义天空都能配了 |
| 6 | ✅ | **阴影质量 / 范围** | 引擎 `DirectionalLight` 有 `shadow_radius`（light.rs:17）/ `shadow_map_size`（light.rs:19，缺省 28.0 / 2048 见 :30-31）；格式只有 `shadows: bool` | 大场景阴影糊、小场景阴影浪费 |
| 7 | ✅ | **层级 / 父子变换** | ~~`SceneObject` 只有 `transform`，无 parent~~ **已做**（2026-10-17 第 16 轮）：`objects[].parent`（按名字引用）+ 局部/世界变换 + 格式 v2；校验器拦悬空/成环/带刚体挂父物体 | 机械臂、车辆、角色挂点这类"组合体"现在能直接摆（仍缺：空物体、场景树内拖放改父；**Prefab/实例已补**：见 #8 的 `templates`/`instances` 与 #13 的跨文件 `include`） |
| 8 | ✅ | **实例 / 模板复用** | 渲染器支持实例化（`InstanceDesc`，renderer.rs:87；绘制入口 renderer.rs:992） | 堆 1000 个箱子 = JSON 里 1000 份重复描述；agent 生成大场景时 token 直接爆。**格式侧已补**（2026-10-18 第 26 轮）：`templates` + `objects[].template` + `objects[].instances[]`（2026-10-20 第 60 轮再补**外部场景引用**，见 #13） |
| 9 | ✅ | **行为 / 时间轴** | `SimulateDoc` 只有 `steps`/`dt`（:464-470）；而 CLI 有 `--trace` / `--bodies` / `--layout` / `--no-sleep` 等参数（runner.rs 里 trace 12 处命中，scene_doc.rs 里 0 处） | 「第 60 步给球一个冲量」「记录这个物体的轨迹」在场景文件里表达不了 → **agent 从 `schema` 看到的能力 < CLI 实际能力** |
| 10 | ✅ | **正交相机 / 多相机 / 裁剪面** | `CameraDoc` 只有 `fov_deg`（:416） | 平面图、CAD 视图、分屏、像素风做不了 |
| 11 | ✅ 2026-10-19（第 44~45 轮） | ~~`models[].path` 是裸路径~~ **已修 + 已加资源库**：`assets.search_paths`（场景级搜索路径，相对路径以场景目录为基准）；贴图与模型**共用同一套候选**（场景目录 → `assets.search_paths` → 当前工作目录 → 引擎搜索路径），validate 与渲染也一致；找不到时错误里列出找过的位置。**修掉的 bug**：模型以前不走场景目录，`models[].path: "chair.glb"` 在工作目录≠场景目录时直接失败（贴图却正常），validate 用的还是第三套规则。**运行时可加**：`--asset-path a:b`（只影响这一次运行，不改场景文件）；**已补**：跨文件 `include`/prefab（2026-10-20 第 60 轮，见 #13）；**仍缺**：资源索引与去重报告 | 素材复用要靠相对路径约定 |
| 12 | ✅ **机器可读 schema** | `quench3d schema` 输出的是手写示例 + 文字（schema.rs:16-42） | ~~agent 拿不到 JSON Schema~~ **已修**（第 27 轮）：`schema --json-schema`（draft 2020-12，见 P1 #19 与 readiness §8.13）。**第 55 轮补上配套的那半**：schema 表达不了的范围/互斥/交叉引用仍以 `validate` 为准，而 `validate` 的每条诊断现在都有稳定 `code`（见 §1 的 agent 向缺口），所以"schema 校验过了但跑不了"这件事至少能被程序分支处理 |
| 13 | ✅ 2026-10-20（第 60 轮） | ~~无（全仓无命中）~~ **已做 `include`**：跨文件组合 + 跨文件 prefab；相对路径以写这一行的文件为基准（与贴图/模型同一套候选）；片段只贡献可合并的段（具名表重名 = `duplicate_name`，数组按「自己的在前」追加），根独有的段写在片段里 = `invalid_value`；片段的资源相对路径按片段目录重定基；最多 16 层、同文件只并一次（菱形给 `ineffective` 警告）、成环 = `include_cycle`；CLI/MCP/编辑器（装载 + 存盘内联）/JSON Schema/语法表/系统提示全部可达。**没做**：variant（同一份片段按场景覆盖字段）、条件包含、运行期资源去重报告、编辑器侧的「另存为片段」 | 大场景可以拆开复用 |
| 14 | **单位与轴向写进文件** | 约定在 README:430 与 schema 文字里，文件本身没有 | 与外部工具交换时需要口头约定 |
| 15 | ✅ **已接线**（第 29 轮锚点 + 第 56 轮按端局部轴 + **第 59 轮参考姿态**） | 引擎 `Joint` 的 `local_anchor_a/b`、`local_frame_a/b`、`local_axis_a/b`（`joints.rs:34-125`）现在**全部有格式入口且全部生效**：`anchor_a`/`anchor_b`（局部坐标）、`axis_a`/`axis_b`（按端局部轴，不写 = 回落 `axis`）、`frame_a`/`frame_b`（固定关节的参考姿态，欧拉角度）。参考姿态这一段的落点在**引擎**：`Joint::Fixed` 多了参考姿态偏置（`fixed_frame_error` + `frame_target_velocity`，`joints.rs`），把两端扭到 `q_a⊗frame_a == q_b⊗frame_b`；两端参考姿态都是单位四元数时走**原来那条**角约束表达式，老场景逐位不变。诊断（全部复用已有码）：零向量 `out_of_range`；`axis_a/b` 写在无轴关节上 `ineffective`；两端朝向相差 >10° 却只写 `axis` → `suspicious_value`；`frame_a/b` 写在非 `fixed` 上、或**两端都写 0**（与不写逐位等价）→ `ineffective`。**仍缺**：关节可视化（轴 / 锚点 / 参考姿态的 gizmo —— 要动渲染侧）；参考姿态只作用于 `fixed`（球窝/铰链各有自己的轴与限位，不需要它） | 门上装合页、摆锤、两块板按「特征朝向」对齐——**都能做了**；只剩可视化 |
| 16 | 🟡 部分（第 31/34/35 轮）：**正交 / near·far / fov / 距离 / 偏航俯仰 / 自动取景已落地**（`render/camera.rs:58/275/310`，e2e `cli_e2e.rs:2015`）；**命名相机 `cameras` 已落地**（校验空名/保留名 `default`，预检白名单+schema+系统提示同步）；**`--camera <名字>` 已落地**（`render`/`simulate`/`movie` 通用；未知名 → 退出码 2 + `unknown_camera` + 可用名单；单测 + 无显卡 e2e）。**没做**：报告里的 `camera_name` 字段、`render.outputs[]` 多输出（一次渲多张）与 `outputs[]`/`--no-clobber` 语义 | 格式 |
| 17 | **地面全局唯一** | `ground` 是单个 `Option`；物理面无限大、摩擦**硬编码 0.85**（runner.rs:374-376），`size` 只管视觉 | 冰面 + 草地共存、可调摩擦——做不到 |
| 18 | ✅ **已补齐：形状集合 6 种 → 9 种**（第 61 轮补 `cone`、`heightfield`、`rounded_box`） | 引擎有 `TriMesh`（以前只能靠 `models[].collide` 间接用）。**`cone`（第 61 轮）**：物理 `Shape::Cone` 支撑函数/质量/惯量/质心偏移 + 渲染单位网格 + 格式/校验/编辑器工具栏 + 射线与半空间接触。**`heightfield`（第 61 轮）**：格式 `heights`（行主序，相对原点）+ `cell_size`，物理展开成**静态三角网**（`MeshData::heightfield` 是唯一一份几何，渲染与物理同源），校验拦下非矩形/超 65536 格点/动态刚体，`--overlap heightfield:...` 给人话错误；编辑器存盘靠**底稿兜底**保住地形（否则存一次就丢）。**`rounded_box`（第 61 轮）**：物理 `Shape::RoundedBox`（支持函数/体积 Steiner 闭式/精确膨胀/解析 SDF 射线）+ 按尺寸生成的网格 + 格式/校验（`radius` 必填、≤ 最短半边长）/`--overlap` 支持 + 编辑器往返。**没做**：HeightField 的专用加速结构（现在逐三角形）、圆角盒的闭式惯量（现在是确定性网格采样，误差 < 1%） | 地形高度场（小网格）、圆角盒、L 形刚体仍只能拆成多个物体用 fixed 关节凑 |
| 19 | ✅ **已基本齐**（第 59 轮核对，**本条曾经是错的、会误导下一个 agent 去重复实现**） | 引擎 `BodyBuilder` 的 `angular_velocity` / `linear_damping` / `angular_damping` / `gravity_scale` / `mass` / `allow_sleep`（`body.rs:336-351`）**都已在格式里**：`ObjectDoc` 字段在 `scene_doc.rs:1179-1196`，runner 装配在 `runner.rs:1429-1445`，校验在 `scene_doc.rs:3500-3545`（`mass` 非正、阻尼为负、非有限 → `out_of_range`）与 `scene_doc.rs:3545-3565`（非 `dynamic` 刚体写这些 → `ineffective` 并列出被忽略的字段名）；`templates` 里也能写（`scene_doc.rs:663-668` 逐字段继承）。**仍缺**（都不在引擎里，属于「以后再说」）：初速度只能给固定值（没有随机/分布）、重力只有**缩放**没有逐物体方向、没有逐物体的求解器迭代次数 | 陀螺自转（`angular_velocity`）、气球（`gravity_scale`）、按克数给质量（`mass`）、休眠控制（`allow_sleep`）——**都表达得了**；上表「刚体参数」那行有实测数字 |

### 3.3 静默失效与"写对了不生效"清单（**已逐条复核**）

> **状态（2026-10-15 第二轮）**：下表 10 条 + 渲染侧 2 条（`alphaMode: BLEND` 的贴图被当不透明、
> `double_sided` 存了不用）**全部已修复**，见 §1.1 第 1 行与 `scene_doc.rs` / `runner.rs` /
> `material.rs` / `renderer.rs` 的测试。下面的表格保留原始证据，方便回看"当时错在哪"。

这类缺陷比"缺功能"更糟：用户/LLM **写对了却得到错的结果，而且没有任何诊断**。按危害排序：

| # | 状态 | 现象 | 证据（已复核） | 后果 |
| --- | --- | --- | --- | --- |
| 1 | ✅ | **`scale` 只影响渲染，不影响碰撞** | 物理建体完全不读 `scale`（runner.rs:382-408，grep 无命中）；渲染读（runner.rs:862-865） | `size:[1,1,1]` + `scale:[2,2,2]` → 画面 2 米箱子、碰撞 1 米 → **物体陷进地面/悬空**，且无诊断 |
| 2 | 🟡 | **凸包渲染成地面大小的立方体** | runner.rs:339（`Vec3::splat(plane_size)`）+ :860（`unit_cube`） | 视觉完全错，物理解算却是对的，最容易让人以为是物理坏了 |
| 3 | ✅ | **`render.enabled` 是死字段** | 全 CLI 源码唯一的 `.enabled` 命中是 `lighting.sky.enabled`（runner.rs:939） | `"enabled": false` 不会阻止出图；`schema.rs:325` 还把它列为有效字段 |
| 4 | ✅ | **`preset: "none"` 得到的是"夜晚"** | `Some("night") \| Some("none") => Lighting::night()`（runner.rs:917） | 想关掉所有光照 → 实际是夜色 + 环境光 0.35 + 天空，与直觉相反 |
| 5 | ✅ | **`preset` 拼错静默回退** | 类型是裸 `String`（scene_doc.rs:437），`validate` 不校验取值，`_ => Lighting::studio()` 兜底（runner.rs:916-920） | `"stuidio"` 不报错，静默变 studio |
| 6 | ✅ | **光照文档默认值与实现差 2.9 倍** | schema 注释写 `sun_intensity` 缺省 1（scene_doc.rs:441）、`ambient` 0.35（:445）；实际 `Lighting::studio()` 是 2.9（light.rs:28）、0.38（light.rs:201） | 照文档推导画面亮度会完全对不上 |
| 7 | ✅ | **铰链 `limits` 单位随类型跳变** | 同一个 `[f32;2]`：hinge 用**弧度**、slider 用**米**（schema.rs:224,293；runner.rs:474,496），而 `rotation_deg` 用度 | LLM/用户写 `limits:[0,90]` 想表达 90° → 变成 90 弧度，物体瞬间转飞，无诊断 |
| 8 | ✅ | **模型路径校验是假阴性** | `validate()` 用裸 `m.path.exists()`（scene_doc.rs:1031），而加载器走 `assets::resolve_path`，会试 `assets/`、`quench3d/assets/`、`CARGO_MANIFEST_DIR`（assets.rs:221-236） | 模型放 `assets/chair.glb` + 写 `"path":"chair.glb"` → 加载器本可找到，`validate` 却 ERROR 拒跑（方向是"过严"，打断 agent 回路） |
| 9 | ✅ | **重复物体名静默覆盖** | `names` 用 `collect()` 进 `BTreeMap`（scene_doc.rs:1049-1053） | 两个同名物体 → 后者覆盖前者，关节引用挂到错的物体上 |
| 10 | ✅ | **地面摩擦不可配置** | 硬编码 0.85（runner.rs:376） | 想调冰面/橡胶地面只能改用静态盒子自己铺 |

> 第 2 条的修法是**近似**：凸包现在按点集 AABB 建盒体代理并给出告警（位置/尺寸正确），
> 但还没有按 `points` 生成真正的凸包网格——那需要凸包三角化，仍在 §4 P2 里。
>
> 复核方式：以上 10 条我都在源码里逐条读过对应行号（`sed -n`），不是采信转述。
> 另有 2 条"结构性"提醒：`simulate` 走的是裸循环 `build.step(steps, dt)`（runner.rs:1006），
> 没用引擎已有的 `step_with_accumulator`（world.rs:567）；`objects` 上限 20000（scene_doc.rs:41）、
> `steps` 上限 1e6（:43）、`dt` 限定 (0,0.5]（:1132）、`msaa` 只能 1/2/4/8（:1150）。

### 3.4 判据：一个真实小项目最常做的 6 件事

| 需求 | `scene.json` 能表达吗 |
| --- | --- |
| 给箱子贴个木纹贴图 | ❌（引擎能，格式不能） |
| 一块玻璃/水面 | 🟡（玻璃可用材质 alpha；水面见第 63 轮 `waters`：解析浮力 + 半透明平面，没有波浪/折射；第 64 轮 `fluids` 有 CPU PBF 粒子，但渲染要等 M4） |
| 一盏台灯（点光源） | ❌ |
| 一辆车（车身 + 四个轮子 + 悬挂） | 🟡（层级、复合形状、关节锚点/限位/马达、按端局部轴都有了；仍缺悬挂的"沿轴弹簧"（6DOF/万向节）、部件级材质与关节可视化） |
| 地上撒 500 个箱子 | 🟡（能，但 JSON 巨大） |
| 「球进洞就停」「第 2 秒推一下」 | ❌ |

**6 件事里 3 件做不了、3 件别扭/近似**（水面与玻璃已有近似表达）。所以结论是：仍不足以支撑真实小项目。

### 3.5 建议的 `scene.json` v2（按用户价值排序）

```jsonc
{
  "format": "quench3d.scene",
  "version": 2,
  "units": { "length": "m", "angle": "deg", "up_axis": "y" },   // ⑭ 自描述
  "assets": { "search_paths": ["assets", "../shared"] },         // ⑪

  "textures": { "wood": "tex/wood_diffuse.png" },                // ①
  "materials": {
    "wood": {
      "base_color": [0.8, 0.6, 0.35, 1.0],          // ② 带 alpha
      "base_color_texture": "wood",
      "normal_texture": "tex/wood_normal.png",
      "metallic_roughness_texture": "tex/wood_mr.png",
      "emissive": [0.0, 0.0, 0.0], "emissive_strength": 1.0,   // ③ 颜色+强度
      "uv_scale": 2.0, "normal_scale": 1.0,
      "alpha_cutoff": 0.5, "unlit": false, "double_sided": true
    }
  },

  "templates": {                                    // ⑧ 模板
    "crate": { "shape": { "type": "box", "size": [1,1,1] },
               "body": "dynamic", "material": "wood", "density": 500 }
  },
  "objects": [
    { "template": "crate",
      "instances": [ { "position": [0,1,0] }, { "position": [0,2,0], "rotation_deg": [0,15,0] } ] },
    { "name": "lamp", "parent": "table",             // ⑦ 层级
      "shape": { "type": "sphere", "radius": 0.15 }, "body": "static", "material": "bulb" }
  ],

  "physics": {                                       // ⑤ 补齐引擎已有参数
    "solver": { "velocity_iterations": 10, "position_iterations": 4, "parallel": true },
    "sleep": { "enabled": true, "linear_tolerance": 0.05, "time": 0.5 },
    "layers": { "player": 1, "world": 2 },           // 碰撞过滤（引擎有则透出）
    "matrix": { "player": { "world": true, "player": false } }
  },

  "actions": [                                       // ⑨ 行为（这是 agent 最需要的）
    { "at_step": 60, "object": "ball", "apply_impulse": [0, 5, 0] },
    { "at_step": 120, "object": "door", "set_velocity": [0, 0, 1] }
  ],
  "probes": [ { "object": "ball", "record": ["position","velocity"], "every": 1 } ],

  "cameras": [                                       // ⑩ 多相机 + 正交
    { "name": "main", "position": [3,3,6], "look_at": [0,1,0], "fov_deg": 45 },
    { "name": "top", "orthographic": { "height": 12.0 }, "position": [0,20,0], "look_at": [0,0,0],
      "znear": 0.1, "zfar": 100.0 }
  ],

  "lighting": {
    "sun": { "direction": [-0.5,-1,-0.35], "intensity": 2.9,
             "shadow_radius": 28.0, "shadow_map_size": 2048 },      // ⑥
    "point_lights": [ { "position": [0,2,0], "color": [1,0.9,0.7],
                        "intensity": 8, "radius": 12 } ],        // ④
    "ambient": 0.35, "exposure": 1.0,
    "sky": { "enabled": true, "horizon": [0.6,0.7,0.9] },           // ⑤
    "fog": { "enabled": true, "color": [0.7,0.75,0.8], "start": 30, "end": 200 }
  }
}
```

### 3.6 不涉及引擎改动、可以立刻做的（投入最小、收益最直接）

这几条都是**格式/CLI 层就能修**的，且属于"修完立刻不再骗人"：

1. **修掉 §3.3 的静默失效**：`scale` 要么同步到碰撞体、要么改名 `render_scale` 并在同时给 `shape` 时报错；
   凸包渲染按 `points` 生成网格；删掉死字段 `render.enabled` 或让它真的生效；
   `preset` 改成枚举并校验（`"none"` 应该真的关灯）；按实现改正 schema 里的默认值注释；
   `limits` 单位拆成 `limits_deg`（hinge）/`limits_m`（slider）或统一成度 + 明确文档。
2. **`validate` 改用 `assets::resolve_path`**，消除模型路径假阴性；重复物体名报错。
3. **材质 1:1 对齐引擎**（① ② ③）——引擎已有 15 个参数/槽位，格式只有 4 个。
4. ~~**关节锚点与局部轴**~~ ✅ 2026-10-19（第 29 / 56 轮）：`anchor_a`/`anchor_b`（局部坐标）+
   `axis_a`/`axis_b`（**按端**的局部轴，不写 = 回落 `axis`，所以老场景逐位不变）；
   `axis` 的"世界轴"说法已改成实际语义，并加了"两端朝向不同却只写 `axis`"的警告。
5. **物理参数透出**：`angular_velocity` / `linear_damping` / `angular_damping` / `gravity_scale` / `mass`
   （引擎 body.rs:66-80,416-498 全都有，纯漏接）。
6. ~~**`quench3d schema --json-schema`**~~ ✅ 2026-10-18 第 27 轮：手写 + `include_str!` 源码对账测试（不引依赖），
   让 agent 能程序化校验与补全，而不是只能事后 `validate`。

### 3.7 落地顺序建议（`scene.json` 相关）

1. **材质 1:1 对齐引擎**（① ② ③）——投入最小、用户感知最强，且不涉及引擎改动。
2. ~~**模板 + 实例**（⑧）~~ ✅ 2026-10-18 第 26 轮：`templates` + `objects[].template` + `objects[].instances[]`（每个副本一个独立刚体）；~~仍缺外部场景引用~~ ✅ 2026-10-20 第 60 轮：跨文件 `include`（见 #13）。
3. **行为 + 探针**（⑨）——让"实验"可以在一个文件里描述完，这是 agent 工作流的刚需。
4. **物理参数透出**（⑤）——把引擎已有但没暴露的参数补齐（含 solver / sleep）。
5. **机器可读 JSON Schema + 版本迁移表**（⑫）——先手写 + 加一致性测试，避免引入依赖。
6. 其余（层级、光照、多相机、资产路径、自描述约定）按需。

> 注意区分两类缺口：**格式落后于引擎**（1、2、3、6、9 的一半）是"改 schema 就能拿到"；
> **引擎本身没有**（触发器/传感器、CCD、动画）必须先补引擎。

---

## 4. 统一优先级清单（四个域合并）

分类标记：**[格式]** = 改 schema/CLI 就能拿到 · **[引擎]** = 必须先补引擎能力 · **[交付]** = 工程/合规/流程

### P0（不做就会持续骗人或挡住整类需求）

| # | 事项 | 类 | 为什么 |
| --- | --- | --- | --- |
| 1 | 修"静默失效"12 条（§3.3 十条 + 渲染两条） | 格式 | ✅ **已修**（2026-10-15，见 §1.1） |
| 2 | 项目纳入 git + 让 CI 真的跑 | 交付 | ⬜ **仍未做**：`quench3d/` 在 `infra-dev` 里未被跟踪、也没有独立 `.git`，CI 从未真正执行 |
| 3 | 补 `LICENSE-MIT` / `LICENSE-APACHE` | 交付 | ✅ **已补**（双许可全文 + README §11） |
| 4 | ~~碰撞过滤 layer/mask + 分组~~ ✅ 2026-10-16 | 引擎 | 玩法分层、单向平台、布娃娃自碰撞 |
| 5 | ~~传感器/触发器 + 碰撞事件（begin/end）~~ ✅ 2026-10-16 | 引擎 | 拾取、伤害、检查点、门与音效 |
| 6 | ~~父层级变换（`parent` + 局部变换）~~ | 引擎+格式 | ✅ **已做**（2026-10-17 第 16 轮）：格式 v2（`parent` 按名字引用）+ `Scene::{world_transform, set_parent, remove}` + 编辑器层级树/改父下拉 |
| 7 | 材质贴图槽位 + 透明 + 发光颜色 | 格式 | 引擎已有 15 个参数/槽位，格式只暴露 4 个 → 视觉上限被格式锁死 |
| 8 | ~~无头"推进 N 帧"API~~ + 渲染 golden 回归 | 引擎 | ✅ API 已补（`render_sequence`/`render_sequence_with_game`，2026-10-15）；**帧序列已经有像素摘要断言**（`movie` 的 `digest`），但单帧 `render` 仍无 golden |
| 9 | ~~修 kinematic 唤醒 bug + `wake_up_all`/公开 `sleep()`~~ ✅ 2026-10-17：`PhysicsWorld::wake_up_all() -> usize`（返回唤醒个数）、`RigidBody::sleep()` 公开（立即入睡 + 清零速度）、`allow_sleep = false` 变成硬承诺（手动 sleep 也睡不住）、`wake` 动作支持 `target: "*"`、编辑器工具栏有「唤醒全部」 | 引擎 |
| 10 | ~~关节锚点与局部轴~~ ✅ 2026-10-19（第 29 / 56 / **59** 轮）：锚点 `anchor_a`/`anchor_b`、**按端的局部轴** `axis_a`/`axis_b`（不写 = 回落 `axis` = 老行为）、固定关节的**参考姿态** `frame_a`/`frame_b`（第 59 轮：引擎按它把两端扭到参考姿态重合；不写或两端都写 0 = 锁定创建时的相对姿态）已全部接线，并有「两端朝向不同却共用一根局部轴」的 `suspicious_value` 与「参考姿态写了等于没写」的 `ineffective` 两条守门诊断。**仍缺**：关节可视化（轴/锚点/参考姿态 gizmo） | 格式 | 门框平放 + 门板转 90° 的合页以前会被求解器**扭平**（无诊断），现在能装；「把 B 扭到指定参考姿态」现在也能写出来了 |
| 11 | ~~关节补全：Ball 锥限位、位置/伺服马达、break force~~ ✅ 2026-10-18：`joints[].cone_limit`（球窝锥形限位，半角 0~180°）、`joints[].motor_target_deg / motor_stiffness / motor_damping`（铰链位置伺服；引擎 `HingeMotor::servo(..).gains(..)`）、`joints[].break_force / break_torque`（超阈值即移除，报告 `joints_broken`，`PhysicsWorld::broken_joints()`）、`joints[].collide_connected`（缺省 `false`，与 Bullet/PhysX 一致：关节两端不互相碰撞）；顺带修掉"马达驱动的刚体会睡着、停在没到位的角度"这个 bug | 引擎 |
| 12 | ~~复合形状（一刚体多 Shape）~~ ✅ 2026-10-18：格式 `objects[].shapes = [{shape, position?, rotation_deg?}]`（与 `shape` 二选一）；引擎 `BodyBuilder::part/part_at` + `RigidBody::parts()`；质量/质心/惯量按部件合成（单形状逐位不变）；碰撞/AABB/查询/角色逐部件；每对刚体最多 8 个接触点。**仍缺**：部件级材质（摩擦/恢复仍在刚体级）、部件可视化编辑面板、部件不能是三角网 | 引擎 |

### P1（决定"像不像一个能用的引擎"）

| # | 事项 | 类 |
| --- | --- | --- |
| 13 | ~~CCD / 推测接触（子弹穿墙）~~ 🟡 2026-10-16（扫掠式，非精确 TOI） | 引擎 |
| 14 | 🟡 2026-10-18：`templates`（字段集与物体相同）+ `objects[].template` + `objects[].instances[]`（每个副本一个独立刚体，名字 `本名#i`）；校验拦模板不存在/空实例/名字撞车。2026-10-20 第 60 轮：~~外部场景引用（跨文件 include）~~ ✅ 见 #13。**没做**：编辑器把 instances 存回时摊平 | 格式 |
| 15 | ✅ 2026-10-19 核对（第 29 轮）：**已实现**——`SceneDoc.actions`/`SceneDoc.probes`（`scene_doc.rs:158`/`:164`）走完"格式 + 校验 + 引擎 + CLI + 编辑器"全程：动作按 `at_step`/`at_time` 到点施力（`cli_e2e.rs:2345 scheduled_actions_reach_the_solver`），探针只读记录进出区域、不碰物理（`:2500 probes_observe_without_touching_physics`），三条渲染路径（render/movie/simulate）都能用（`:2577 actions_and_probes_work_on_all_three_paths`）；编辑器 `scene_io` 保留 `probes`、并在删除物体时同步清掉指向它的动作（`scene_io.rs:37`/`:386`）。**边界**：探针没有可视化绘制（只在报告里出现）；动作没有时间线 UI，只能手写 JSON | 格式 |
| 16 | 🟡 部分（第 31 轮核对）：**正交 / near·far / fov / 距离 / 偏航俯仰 / 自动取景都已落地**——`camera.orthographic_height` 走 `Projection::Orthographic`（`render/camera.rs:58/275/310`，`orthographic_rh`），`near`/`far` 进投影矩阵，`fov_deg`/`distance`/`yaw_deg`/`pitch_deg`/`frame_scene` 齐全；校验与 CLI 可达由 e2e `camera_clip_planes_and_orthographic_projection_are_wired`（`cli_e2e.rs:2015`，含 `near > far` 的报错路径）钉住。**没做**：**多相机 / 多输出**——格式里只有一个 `camera`，一次渲染只出一张图；真做它要动场景格式（`cameras[]` 或 `render.outputs[]` + 向后兼容单个 `camera`）、渲染循环（每台相机各渲一遍并命名输出）、CLI 报告（多份 output 字段）三处 | 格式 |
| 17 | ~~光照透出：点光源 / 天空 / 雾 / 阴影质量参数~~ ✅ 2026-10-17：点光源（本来就有）、雾（`lighting.fog`，不写 = 关闭）、阴影质量（`strength`/`radius`）、天空颜色（`sky_colors`）。**没做**：阴影贴图分辨率（渲染器构造参数）、点光/聚光阴影（#24）、线性雾的起止距离（引擎只有指数雾） | 格式 |
| 18 | ✅ 2026-10-19 核对（第 29 轮）：**已实现**——物体级 `velocity`/`angular_velocity`/`linear_damping`/`angular_damping`/`gravity_scale`/`mass`/`allow_sleep`（`scene_doc.rs:890-907`），世界级 `physics.friction_combine`/`restitution_combine`/`static_friction_speed`（`:195-205`）+ 物体级同名覆盖。**边界**：`mass` 与 `density` 同时写时密度优先（诊断里会提示）；没有 `sleep_threshold`/`sleep_time` 这类更细的睡眠参数（引擎内部常量） | 格式 |
| 19 | ✅ 2026-10-18：`quench3d schema --json-schema`（draft 2020-12，`$defs` 24 个类型 + 6 个枚举，全部 `additionalProperties: false`；`include_str!` 源码对账测试保证与结构体同步）；版本策略写进 README 与 `version` 诊断（1→2 = 改版本号，无破坏性字段变化）。**没做**：外部 `$ref` / draft-07 兼容、`--json-schema` 的 `format`/`minimum` 等语义约束（那些仍以 `validate` 为准） | 格式 |
| 20 | 🟡 部分（第 28/30 轮）：**接触特征 ID 已落地**（`ContactPoint.feature_id`：两端局部坐标按 0.5 mm 量化哈希，宽相/窄相与求解器共用同一函数；热启动缓存优先按 ID 精确匹配，2 cm 坐标容差退化为兜底）；**接触余量已落地**（`physics.speculative_margin`，缺省 0 = 历史结果逐位不变；膨胀窄相 + 负穿透 + `separation/dt` 速度目标；digest 测试证明缺省路径没漂）。2026-10-19 第 61 轮：~~余量只作用于单形状刚体~~ ✅（复合刚体逐部件膨胀）、~~余量的数值范围校验~~ ✅（负数/非有限/> 1 m → `out_of_range`，0.25~1 m → 告警）、~~专门 e2e~~ ✅（`speculative_margin_reduces_penetration_on_impact`：开余量的轨迹低点更高）；**顺带修掉一个静默 bug**——`physics.speculative_margin` 从来没被读到，而 `static_friction_speed` 会顺手把它设成同一个值（详见 CHANGELOG「修复」与 `production_readiness` §18）。**没做**：流形本身跨帧复用（跨帧只有身份与冲量） | 物理 |
| 21 | ~~查询补全 + 查询加速结构~~ ✅ 2026-10-17：`raycast_all` / 任意形状 `overlap_shape` / 单向 `QueryFilter`（mask + 传感器开关）/ `distance_to` + `QueryIndex` 快照网格（射线 3D DDA）；CLI `query`、MCP `quench3d_query`、编辑器 F7 查询探针。**没做**：`shape_cast` 仍线性、CLI 不做批量查询、索引是快照（世界一动就回退线性） | 引擎 |
| 22 | ~~物理材质与 combine mode、静/动摩擦分离~~ ✅ 2026-10-17：`CombineRule`（5 条：几何平均/平均/取小/取大/相乘，摩擦缺省几何平均、恢复缺省取大 = 老行为）、`static_friction`（缺省跟随动摩擦）、世界级 `physics.{friction_combine,restitution_combine,static_friction_speed}` + 物体级覆盖、编辑器滑块与下拉。判定用**施力前**速度（否则静摩擦永远不生效）。**没做**：材质的各向异性/滚动摩擦、`friction_combine` 在两端都显式且不同时只有一条确定性优先级（取 slot 靠前者） | 引擎 |
| 23 | 🟡 部分（第 61~62 轮，三刀）：**①节点层级/变换修好了**（`35d48a6`：按 `doc.nodes()` 走、父子累乘、法线/切线跟着转、同一网格多节点 → 多实例）；**②动画解析 + CPU 采样落地**：`LoadedModel.nodes`/`animations`、`sample_animation(name, t)`、`mesh_matrix = world(t) × bind_inverse`（绑定姿态下是单位矩阵 → 静态路径逐位不变），STEP/LINEAR（旋转 slerp）、`ModelAnimationPlayer`；**③接进场景格式 + 帧循环**：`models[].animation = { name, start?, speed?, loop? }`（不写 = 静止，向后兼容），`Scene::advance_animations(dt)` 在 `Engine::tick` / `movie` 无头循环 / 单帧 `render` 三处消费，编辑器存读保留；"动画名不存在"**不在 `validate`**（它不读资产），在 `simulate`/`render`/`movie` 报告的 `models[]`（`found`/`duration`/`tracks`/`clips`）+ 警告，退出码仍 0。**仍缺**：混合（多轨道加权）、`skins` + GPU 蒙皮（带骨骼的角色仍只能静止）、`CUBICSPLINE` 真值（现在退化线性）、morph target；模型动画是**视觉**的，`collide` 碰撞体不跟着动。顺序见 `production_readiness` §19.3 | 引擎（大） |
| 24c | ✅ 编辑器局部光编辑（第 53 轮）：点光/聚光的增删改 + 从引擎写回 + CPU 往返测试 | 视口 gizmo / 选中高亮（要靠坐标数字调） | 编辑器里能直接把灯摆明白，不用手改 JSON |
| 24d | 🟡 部分（第 54~55 轮）：**聚光灯阴影**（第 54 轮，`SpotLight.cast_shadows`，一张 2D 图）+ **点光源立方体阴影**（第 55 轮，`PointLight.cast_shadows`，`texture_depth_2d_array` 6 层）；两者的缺点都是缺省 `false`（老场景像素逐位不变），各自整帧**只支持一盏**（第一盏要求投影的），但**两套图互相独立、可以同时各有一盏**；相机/bias 都是纯函数（无显卡可跑），uniform/WGSL/校验/编辑器勾选框全链路；本机 GPU 探针（聚光 14 条 + 点光 23 条断言）与 `point_shadow_diag` 逐面对账量化 | 多盏投影（阴影图数组 / 逐灯索引）、级联聚光阴影、半透明物体阴影、可单独调的阴影图分辨率；编辑器勾选框没有人在真窗口里点过；面接缝与 bias 没有扫参 | 局部光终于会被挡住（以前光穿透一切） |
| 24b | ✅ 编辑器与 CLI 的后处理一致性（第 51–52 轮）：级联 UI + `bloom/fxaa/ssao` 载入应用与写回，共用同一份映射函数 | — | 编辑器预览与 `quench3d render` 在后处理上不再分叉 |
| 24 |（部分 ✅）第 51 轮：编辑器有了级联 UI，且载入/存盘与 CLI 共用同一份映射（预览==渲染）| 24 |（部分 ✅）第 49 轮加了**聚光灯**：`lighting.spot_lights`（引擎 `SpotLight` + 与点光源共享 8 盏名额 + WGSL 圆锥平滑过渡 + 校验 + 测试 + 本机 GPU 量化）。**仍缺**：~~点光源不投影（要立方体阴影图）~~ 第 55 轮补上了（见 24d）；聚光灯的阴影见 24d| 局部光只能照明、不能互相遮挡 | 🟡 部分（第 42~43 轮）：**CSM 已落地并已量化**——`lighting.shadow_quality.cascades`（1~4，缺省 1 = 老行为逐像素不变）+ `split_lambda`（0~1）；CPU 侧 `cascade.rs`（PSSM 切分 / 视锥角点 / 切段角点 / 每级矩阵 + 分块纹素对齐）有几何断言与"每级 bias 都等于同一纹素数"的不变式（无显卡可跑）；渲染侧画进 2×2 图集、着色器按视空间深度选级；e2e 覆盖校验（0/5/1.5 报错）；`examples/shadow_probe` 级联 sweep 给出**实测数字**（`docs/production_readiness.md` §12.2）：级联换到的是**覆盖**（60 米外投射体单级 0 个影子像素，2/3/4 级 5/3/3 个，落点误差 ≤0.6px），而"小场景 + far=500"下单级反而清晰约 33 倍（纹素足迹 0.0273 vs 0.893）。**没做**：点光源阴影（立方体阴影图）；聚光灯阴影见 24d | 渲染 |
| 25 | 后处理链骨架（HDR 中间目标 + tonemap 移出物体着色器）✅ 2026-10-16 + Bloom ✅（`lighting.bloom`）+ FXAA ✅（`lighting.fxaa`）+ SSAO ✅（`lighting.ssao`，深度-only、无去噪）+ TAA/景深 ❌ | 引擎（大） |
| 26 | IBL / 环境贴图 | 引擎 |
| 27 | 资源管线：Web 端模型加载（bytes/fetch）✅ 2026-10-15（`load_model_from_bytes` + wasm 探针验证）+ **CPU 侧模型缓存 ✅ 第 41 轮**（`assets::ModelCache`：进程内、规范化路径为 key、按 `(文件长度, mtime)` 失效、256 MiB 估算预算 + LRU 淘汰、`stats()`；`simulate` 报告的 `model_cache{...}` 是可达观测口径，e2e 钉住"同文件两条引用 = 1 次解析 + 1 次命中"、"不同文件 2 次解析"、"没模型的场景报告里不出现这个字段"）+ **显存释放仍 ❌**（GPU 缓冲还是只增不减，反复 `load_model` 必涨显存——这条本轮没动，见 readiness §11） | 引擎 |
| 28 | agent 幂等与安全：`--out` 冲突保护 ✅、`--dry-run` ✅、**API key 不再走 argv** ✅、MCP annotations/outputSchema ✅（全部 2026-10-15） | 交付 |
| 29 | 事件总线 ✅ 2026-10-18（`PhysicsWorld::events()/drain_events()/take_events()`，7 种事件：接触开始/结束、传感器进入/离开、睡眠/唤醒、关节断裂；队列有上限 4096 + `events_dropped()` 记账；`simulate` 报告里输出 `events[]`（物体名 + 步号）+ `events_dropped`；**仍缺**生命周期钩子与用户回调——没有脚本层，`on_scene_loaded`/`on_destroy` 只能由宿主轮询，编辑器只有状态栏只读显示） | 引擎 |
| 30 | ✅ 已完成（第 32/33/37 轮）：测试数/示例数每轮随门禁更新；`--locked` 在 CI 每条 cargo 命令上；**`cargo doc -D warnings`** 进门禁第七段 + CI（修掉 16 处既有 rustdoc 告警，无 allow 掩盖）；**MSRV 已由 CI 验证**——`msrv (1.90)` 腿用 1.90.0 工具链对 `quench3d-math/physics/render` + `quench3d` + `quench3d-cli` 跑 `cargo check --locked --all-targets`（第 37 轮首次跑通，全绿），`Cargo.toml` 的注释同步改成"已验证"。**边界**：编辑器不在这一腿里（它单独声明 1.95，egui 0.37 要求），所以 MSRV 的适用范围是"工作区除编辑器"；`msrv` 腿只跑 ubuntu（Windows/macOS 由 stable 腿覆盖） | 文档/CI |

### P2（长尾，按需）

- **格式**：`units`/`up_axis` 自描述、凸包按 `points` 生成真实渲染网格（现在只做 AABB 盒体近似 + 告警）、多地面、~~形状补全（Cone/圆角盒/HeightField）~~ ✅ 第 61 轮补齐。~~地面可配摩擦~~ ✅ 已做、~~`scale` 与碰撞体一致性诊断~~ ✅ 已做（告警）。
- **引擎**：透明距离排序 + OIT、贴花、粒子/ billboard、LOD + 遮挡剔除、多视口、地形/水体、快照与回放、渲染插值、时间缩放与单步调试、音频、脚本层、输入映射与手柄/触屏、多体/articulation、TGS/SIMD 求解器、精确凸包质量属性。
- **交付**：crate 元数据 + `cargo publish --dry-run`、CHANGELOG/semver 策略、cargo-deny 依赖审计、预编译发行（cargo-dist）、Web 产物 `wasm-opt`/Brotli/文件名哈希（编辑器包 11 MB，见 README §2）、移动端。
- **编辑器**：~~拖拽 gizmo~~、~~撤销/重做~~、原生文件对话框、多选/成组、~~模型导入到场景树~~、
  贴图选择、~~层级与父变换~~、场景内行为编辑 —— 详见 §2.5。

---

## 5. 建议的迭代顺序

> 原则：**先把"骗人"改成"诚实"，再把"缺的地基"补齐，最后才做"大件"**。
> 大件（动画、后处理、CSM）每一个都抵得上前面十项，但它们依赖地基（无头帧循环、资源生命周期、层级）。

- **0.2 —— 可信与诚实**：P0 #1（12 条静默失效）+ #3（LICENSE）+ #30（文档对齐）
  + #8 的一半（无头 N 帧 API + 渲染 golden）。**2026-10-15 第二轮：除"单帧 golden"外均已完成**（§1.1）；
  顺带修掉 P0 #9 的 bug 与 `paused` 的时间语义，并做了仿真视频的内置 GIF/APNG 编码器。
  #2（纳入 git）仍需人决定放哪个仓库。
- **0.3 —— 让格式配得上引擎**：P0 #7、#10、P1 #15–#19（行为/相机/光照/物理参数/JSON Schema；材质/层级/#14 实例模板 ✅；**P1 #15 关节参考姿态 ✅ 第 59 轮；#19 经复核本来就是 ✅（旧描述过时，已改正）**）
  + 仿真视频（§6）——这一轮几乎不动物理与渲染内核，全部是"接线"。
- **0.4 —— 补玩法地基**：P0 #9/#11/#12（休眠修复 ✅、关节补全 ✅、复合形状 ✅）+ #29 事件总线 ✅（无脚本钩子）
  + #21 查询补全/加速（#4 过滤、#5 传感器事件、#13 CCD 已在 2026-10-16 落地，
  #21 查询已在 2026-10-17 落地）。
- **0.5 —— 内容化**：P1 #23 动画/蒙皮、#24 阴影体系、#25 后处理、#26 IBL、#27 资源管线（含 Web 端加载）。
  到这里才谈得上"能做真实小项目"。

---

## 6. 相关文档

- **仿真视频方案**（可行性、实测速度、三条路线、行业对标、落地顺序）→ [`docs/simulation_video.md`](simulation_video.md)
- **诊断码表**（稳定契约、18 个码的含义与 agent 动作、怎么加新码）→ [`docs/error_codes.md`](error_codes.md)
