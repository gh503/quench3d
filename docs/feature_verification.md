# 正向验证矩阵（可复现命令）

> 目的：把「功能存在」和「功能真的改变状态 / 画面」分开。每一项都给出**能重跑的命令**
> 与判据；没有自动化证据的项标「未覆盖」，不写成绿。
>
> 最近一次全量结果：`tools/verify_all.sh` 全绿；门禁 **688** 个测试、0 失败、3 ignored；
> wasm 构建 + `check_web.mjs` + WebGL2 启动 + Web UI 按键探针 + 摄像头纯函数测试都通过。

## 0. 一条命令

```bash
tools/verify_all.sh
```

它依次跑：`tools/gate.sh` → `tools/probe_webgl2.sh` → `tools/probe_web_ui.sh` →
`python -m unittest discover -s tools/camera_mocap` → `tools/probe_deployed.sh`。没有 Chrome 的机器上前两条会
**跳过并返回 0**（与仓库其它 GPU 测试一致），不会假装绿；看输出里的「跳过」。

## 1. 引擎 / 渲染（像素证据）

| 功能 | 正向证据（命令 / 用例） | 判据 |
| --- | --- | --- |
| PBR + 光照 + 天空 | `cargo test -p quench3d-render --test post_pipeline` | 像素与手算 ACES/sRGB 对上，误差 ≤1 |
| 阴影强度只暗影区 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test fog_and_shadow_quality` | 影区变暗、非影区不变 |
| 阴影级联 1–4 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test shadow_cascades` | 远处遮挡物只在有级联时出现影子 |
| 点光 / 聚光阴影 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test point_lights` | 有/无投影的像素差 |
| Bloom | `cargo test -p quench3d-render --test post_pipeline bloom` | 亮区外溢、关闭时参数无效 |
| FXAA | `cargo test -p quench3d-render --test post_pipeline fxaa` | 边缘变平滑、平面不变 |
| SSAO | `cargo test -p quench3d-render --test post_pipeline ssao` | 接触处变暗、远平面不变 |
| 水面后处理 | `cargo test -p quench3d-render --test post_pipeline water` | 开/关、波强/泡沫/折射都改变像素 |
| 解析式环境高光 | `cargo test -p quench3d-render --test post_pipeline analytic_environment` | 金属球开关像素不同 |
| 透明排序 | `cargo test -p quench3d-render --test post_pipeline transparent` | 远蓝后画、近红先画仍得到正确混合 |
| 网格深度 | `cargo test -p quench3d-render --test post_pipeline grid_writes_depth` | 地面下方线段被遮住、上方可见 |
| 骨骼蒙皮（CPU）+ 顶点回写 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d --test model_loading skinned` | 手搓 GLB 的 `skins/JOINTS_0/WEIGHTS_0` 被解析；关节 t=1 时顶点 y+2；t=0 与 t=0.25 的 64×64 渲染**像素确实变化**（没写回 GPU 这条会红） |
| 材质 alpha / 双面 | `cargo test -p quench3d-render --test material_pipeline_traits` | 左右半幅像素不同 |
| 后端回退 / GL 翻译 | `cargo test -p quench3d-render --test backend_fallback`；`cargo test -p quench3d-render --lib gl_pipelines` | 候选过滤、所有 WGSL 能翻译到 GLSL |

## 2. CLI / 场景格式

| 功能 | 正向证据 | 判据 |
| --- | --- | --- |
| `validate` / 诊断码 / 退出码 | `cargo test -p quench3d-cli` | 每条诊断路径 + 退出码 0/2/3 |
| 单帧 `render` PNG | `cargo test -p quench3d-cli --test cli_e2e render_produces` | 合法 PNG、尺寸/统计 |
| 单帧黄金图 | `cargo test -p quench3d-cli --test cli_e2e golden_png_comparison` | 同场景 0 差异；换颜色 `golden_mismatch`；缺图/多输出/dry-run 的退出码 |
| 多输出 + no-clobber | `... multiple_render_outputs` | 两张路径/尺寸先解析、整体不写半套 |
| 命名相机 + `camera_name` | `... camera_flag` / `multiple_render_outputs` | 已知名/`default`/未知名；报告字段 |
| 雾 / 阴影质量 / 天空 | `... fog_and_shadow_quality` | 字段真的从 JSON 传到像素 |
| 动画 / 动作 / 探针 | `cargo test -p quench3d-cli --test cli_e2e`（animation/probe/action 用例） | 状态/探针结果随步数变化 |
| 场景 include / 模板 / 实例 | `cargo test -p quench3d-cli --test scene_fixtures` | 合并、重名、路径、往返 |
| 模型动画对账 | `cargo test -p quench3d-cli --test cli_e2e model` | 动画名存在/不存在、时长/轨道 |
| 蒙皮资产对账 | `cargo test -p quench3d-cli --test cli_e2e info_reports_skins` | `info --json` 的 `skins=1` / `skinned_meshes=1`，人类可读输出有「蒙皮」行 |
| morph 位移数学 | `cargo test -p quench3d --test model_loading morph_targets_displace_vertices_by_weight` | 手搓 GLB 的 `targets[0]` 解析；权重 0 原样、1 整体 +0.5、0.5 +0.25 |
| morph 资产对账 | `cargo test -p quench3d-cli --test cli_e2e info_reports_morph_targets` | `info --json` 的 `morph_targets=1`，人类可读输出有「morph target」行 |
| 场景 morph → CLI PNG | `cargo test -p quench3d-cli --test cli_e2e scene_morph_weights_change_cli_render` | 同一个静态模型 `morph_weights:[0]` 与 `[1]` 的 PNG 有可见像素差；无 GPU 按退出码 3 跳过 |
| morph 权重校验 | `cargo test -p quench3d-cli --test cli_e2e scene_morph_weights_are_validated` | `validate` 报 `models[0].morph_weights[0]` / `[1]` 的 `out_of_range`，退出码 2 |
| morph 权重动画 | `cargo test -p quench3d --test model_loading morph_animation_interpolates_weights` | 解析 `pulse` 的 `weights` 轨道；t=0/0.5/1 权重 0/0.5/1，喂给网格后位移 0/0.25/0.5 |
| morph 动画 → CLI PNG | `cargo test -p quench3d-cli --test cli_e2e scene_morph_animation_changes_cli_render` | `animation.start=0` 与 `0.5` 的 PNG 有可见像素差；报告 `animation.morph_tracks=1` |
| 状态机过渡 | `cargo test -p quench3d --test model_loading scene_animator_transitions_and_promotes` | 停留 0.5 秒后淡入，半程权重约 0.5；淡入结束把新 clip 提为主 clip 并重新计时 |
| 状态机报告 / 首帧 | `cargo test -p quench3d-cli --test cli_e2e scene_animator_is_reported_and_changes_cli_render` | `animator.found=true`/`states=2`/`transitions=1`；立即过渡（after=0, fade=0）的 PNG 与基线不同 |
| 状态机校验 | `cargo test -p quench3d-cli --test cli_e2e scene_animator_is_validated` | 初始状态不存在、to 不存在、after 负数、fade 负数各有诊断 |
| CUBICSPLINE 采样 | `cargo test -p quench3d --lib cubic_spline_uses_tangents` | 出切线 2/s、入切线 0 时 u=.5 为 .75（线性只会 .5）；切线缺失退回线性 |
| CUBICSPLINE 解析链路 | `cargo test -p quench3d --test model_loading cubic_spline_tangents_survive_parsing_and_sampling` | 解析后 in/out 切线保留，`sample_animation(t=.5)` 的 x=.75 |
| 双 clip morph 混合 | `cargo test -p quench3d --test model_loading blended_morph_weights_interpolate_both_clips` | 主 clip .5、第二 clip 1、blend=.5 → 权重 .75；形变位移 .375 |
| 场景动画 → CLI PNG | `cargo test -p quench3d-cli --test cli_e2e skinned_model_animation_changes_cli_render` | `models[].animation.start=0` 与 `0.5` 两张 PNG 有可见像素差；无 GPU 按退出码 3 跳过 |
| 动画时间检查器（CPU） | `cargo test -p quench3d --test model_loading refresh_animations` | 手动把 `player.time` 拖回 0.25，调用 `refresh_animations` 后位移精确为 10.75，重复刷新不漂移、不推进时间 |
| 动画时间检查器（GPU 蒙皮） | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d --test model_loading skinned_mesh_changes_rendered_pixels` | 用 `refresh_animations_with_renderer` 在 t=0/0.25 渲染，两张 PNG 像素有差 |
| 编辑器动画面板 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test ui_smoke` | 注入播放实例后，绘制图元里必须出现「动画（1 个实例）」「idle」「状态机：待机」；整帧不 panic |
| 双 clip 混合数学 | `cargo test -p quench3d --test model_loading blended_clips_interpolate_skinned_pose` | 局部 TRS 插值：primary y=1、secondary y=2、w=0.5 → 顶点位移 1.5；w=0/1 分别精确取两条 clip |
| 场景双时间线 | `cargo test -p quench3d --test model_loading scene_blended_animation_advances_both_clips` | `Scene::advance_animations` 同时推进两条 clip，t=.5 落在 base+2.5 |
| 一次性淡入 | `cargo test -p quench3d --test model_loading scene_blend_fades_in_over_time` | `fade=0.5`：t=.25 权重 0.5（smoothstep）位移 1.75，t=.5 权重 1 位移 3.5 |
| 场景混合 → CLI 报告/PNG | `cargo test -p quench3d-cli --test cli_e2e model_animation_blend_is_reported_and_rendered` | `models[].blend.found/weight/start` 正确；名字写错给警告；混合 PNG 与纯主 clip 有可见像素差 |
| 混合结构校验 | `cargo test -p quench3d-cli --test cli_e2e model_animation_structure_is_validated` | 空第二条名字 / 越界 weight / 负 fade / 非正 speed 落在 `models[0].animation.blend.*` |
| 物理 / 关节 / 复合形状 / CCD | `cargo test -p quench3d-physics` | 确定性、休眠、过滤、传感器、关节 |
| MCP / ask 工具 | `cargo test -p quench3d-cli --test agent_tools`、`mcp` | 工具清单、stdio 握手、模拟 LLM |
| JSON Schema | `cargo test -p quench3d-cli schema` | 文档/schema/系统提示三处对账 |

## 3. 编辑器（状态 + 存读 + UI）

| 功能 | 正向证据 | 判据 |
| --- | --- | --- |
| 全面板同帧绘制 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test ui_smoke` | 工具栏/场景树/属性/视口/帮助/摄像头窗口产出图元 |
| 增删 / 复制 / 重置 | `cargo test -p quench3d-editor --test history` | 撤销重做、拖拽合并、快照逐字节 |
| 层级 / 改父 / 环拒绝 | `cargo test -p quench3d-editor --test hierarchy` | 世界位姿保持、导出 parent |
| 存读无损 / 模板实例 / 水 / 流体 | `cargo test -p quench3d-editor --test scene_roundtrip` | 存→读→再存字节一致 |
| 历史 / 模型实例变换 | `cargo test -p quench3d-editor` | 模型组变换写回 |
| 摄像头协议 / 3D 骨架 | `cargo test -p quench3d-editor --lib camera` | 解析/长度/坐标/线段数量/低可见度 |
| 深度点云反投影 | `python -m unittest test_camera_mocap.PointCloudTests -v` | 中心像素落主轴、y 向上翻转、近深度 z=-0.3、步长下采样 |
| 点云读入/绘制 | `cargo test -p quench3d-editor --lib camera` | `latest.points` 长度/布局校验；每点画 3 条十字线段 |

## 4. Web / 浏览器

| 功能 | 正向证据 | 判据 |
| --- | --- | --- |
| wasm 构建 + 产物自检 | `tools/build_web.sh web_client`；`node tools/check_web.mjs` | 符号/实例化/内存都在 |
| WebGL2 启动 | `tools/probe_webgl2.sh` | 第一帧日志 + 遮罩收起 + 无 panic/`INDEPENDENT_BLEND`/Cube 启发式 |
| 线上 Pages 启动 | `tools/probe_deployed.sh [URL]` | HTTP 200 + 线上 wasm 第一帧 + 遮罩收起；网络不通跳过 |
| Web UI 按键 → 状态 → 画面 | `tools/probe_web_ui.sh` | F3/F4/R/F6/F7/P 前后 UI 状态与中央视口截图都必须变；F5 只断言状态（默认场景可能没有接触），Tab 断言相机模式（Orbit→Fly） |
| 浏览器场景往返（纯 CPU） | `cargo test -p quench3d-editor --lib` / wasm 导出探针 | 场景 JSON 往返 |

## 5. 摄像头动捕（桌面 Linux，实验）

```bash
target/tmp/camvenv/bin/python tools/camera_mocap/camera_mocap.py --source synthetic
python -m unittest discover -s tools/camera_mocap -p 'test_*.py'
```

| 功能 | 判据 |
| --- | --- |
| 深度归一化 | 常量深度全 0、单调、NaN/Inf 安全 |
| 轮廓提取 | 最大外轮廓、bbox、面积、空 mask |
| 骨骼连接 | 35 根骨、33 个关节、低可见度不画 |
| 协议帧 | JSON 解析、RGB 长度/尺寸上限、坐标翻转、过期帧 2 s 消失 |

## 6. 明确未覆盖（不要当成绿）

* **GPU 蒙皮 / 动画混合**：glTF `skins` + `JOINTS_0/WEIGHTS_0` 已解析，且有 **CPU 蒙皮**（每顶点最多 4 关节，逐帧变形后写回顶点缓冲）；但**顶点着色器里没有关节矩阵**，混合/状态机、IK 仍未做。**morph target** 已支持静态权重和 glTF `weights` 权重动画（先 morph 后蒙皮），双 clip 也按 `blend_weight` 插值 morph 权重；但还没有编辑器滑杆。CPU 路径在角色面数高时会把开销放在 CPU。
* **IBL / HDRI / 环境 cubemap**：只有解析式环境高光。
* **LOD / 遮挡剔除 / 大规模合批**：逐实例视锥剔除。
* **音频、脚本层、粒子、贴花、地形、多视口**：没有。
* **Web 摄像头**：只有桌面文件协议；浏览器 `getUserMedia` 未接。
* **真机 GPU/浏览器矩阵**：只有 headless Chrome + SwiftShader（软件光栅）；真机 WebGPU 未验证。
* **跨机器的 golden 基线**：`--golden` 已能逐像素比对，但仓库**没有**提交一份
  "官方参考图"给所有平台；不同 GPU/驱动/抗锯齿下的基线仍要各自生成。
* **UI 每一个按钮的点击**：`ui_smoke` 只证明全面板能画，`probe_web_ui` 覆盖 F3/F4/R 三个真实按键（R 切缩放/平移手柄）；其余按钮仍靠人工/单元路径，未逐一点击。
