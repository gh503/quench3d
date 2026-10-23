# 正向验证矩阵（可复现命令）

> 目的：把「功能存在」和「功能真的改变状态 / 画面」分开。每一项都给出**能重跑的命令**
> 与判据；没有自动化证据的项标「未覆盖」，不写成绿。
>
> 最近一次全量结果：`tools/verify_all.sh` 全绿；门禁 **788** 个测试、0 失败、3 ignored；GPU 蒙皮有调色板填充与 CPU/GPU 像素对照；
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
| PBR + 光照 + 天空 | `cargo test -p quench3d-render --test post_pipeline` | 像素与手算 ACES/sRGB 对上，误差 ≤1；三档 tonemap 与 CPU 参考误差 ≤2 且互不相同 |
| 阴影强度只暗影区 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test fog_and_shadow_quality` | 影区变暗、非影区不变 |
| 阴影级联 1–4 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test shadow_cascades` | 远处遮挡物只在有级联时出现影子 |
| 点光 / 聚光阴影 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test point_lights` | 有/无投影的像素差 |
| Bloom | `cargo test -p quench3d-render --test post_pipeline bloom` | 亮区外溢、关闭时参数无效 |
| FXAA | `cargo test -p quench3d-render --test post_pipeline fxaa` | 边缘变平滑、平面不变 |
| TAA | `cargo test -p quench3d-render --test post_pipeline taa` | 相机不动时第 1→2 帧变化（历史被消费）；feedback 0.9 与 feedback 0 的第 2 帧不同；关闭 TAA 重复 capture 逐位相同；切镜（位置 >2m/朝向 >60°）丢弃历史；GL/MSAA 跳过 |
| SSAO | `cargo test -p quench3d-render --test post_pipeline ssao` | 接触处变暗、远平面不变；可选去噪至少改 20 像素、接缝仍暗 ≥2.5/255、接触区粗糙度 5.10→2.38 |
| 水面后处理 | `cargo test -p quench3d-render --test post_pipeline water` | 开/关、波强/泡沫/折射都改变像素 |
| 解析式环境高光 | `cargo test -p quench3d-render --test post_pipeline analytic_environment` | 金属球开关像素不同 |
| 透明排序 | `cargo test -p quench3d-render --test post_pipeline transparent` | 远蓝后画、近红先画仍得到正确混合 |
| 网格深度 | `cargo test -p quench3d-render --test post_pipeline grid_writes_depth` | 地面下方线段被遮住、上方可见 |
| 骨骼蒙皮（GPU）+ 调色板 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d --test model_loading gpu_skinning` | 手搓 GLB 的 `skins/JOINTS_0/WEIGHTS_0` 被解析；纯蒙皮实例提交后 `skin_palette_len()>0`；t=0 与 t=0.25 的 64×64 渲染**像素确实变化** |
| GPU / CPU 蒙皮一致性 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d --test model_loading gpu_skinning_matches_cpu_skinning_pixels` | 同一 t=0.25 下 `gpu_skinning=true/false` 两张图 4096 像素全等（差>3 的像素 0，平均差 0.0000） |
| 材质 alpha / 双面 | `cargo test -p quench3d-render --test material_pipeline_traits` | 左右半幅像素不同 |
| 后端回退 / GL 翻译 | `cargo test -p quench3d-render --test backend_fallback`；`cargo test -p quench3d-render --lib gl_pipelines` | 候选过滤、所有 WGSL 能翻译到 GLSL |

## 2. CLI / 场景格式

| 功能 | 正向证据 | 判据 |
| --- | --- | --- |
| `validate` / 诊断码 / 退出码 | `cargo test -p quench3d-cli` | 每条诊断路径 + 退出码 0/2/3 |
| 场景 → OBJ 导出 | `cargo test -p quench3d-cli --lib obj_export`；`quench3d export target/tmp/export_scene.json target/tmp/export_demo.obj` 后 `quench3d info target/tmp/export_demo.obj` | 4 个用例：层级/材质/面数、模板实例展开、导出再导入三角形数一致、凸包进入 `skipped[]`；实际导出 2 物体/449 顶点/780 三角形，`info` 读回 2 mesh/780 三角形、包围盒与场景一致 |
| 单帧 `render` PNG | `cargo test -p quench3d-cli --test cli_e2e render_produces` | 合法 PNG、尺寸/统计 |
| 视频编码 MP4 | `cargo test -p quench3d-cli --test cli_e2e movie_encodes_mp4_when_ffmpeg_is_available` | 有 ffmpeg 时 3 帧 64×64 → 带 `ftyp` 的 MP4、`bytes>0`、trace 落在输出旁；无 ffmpeg/GPU 自动跳过 |
| 单帧黄金图 | `cargo test -p quench3d-cli --test cli_e2e golden_png_comparison` | 同场景 0 差异；换颜色 `golden_mismatch`；缺图/多输出/dry-run 的退出码 |
| 多输出 + no-clobber | `... multiple_render_outputs` | 两张路径/尺寸先解析、整体不写半套 |
| 命名相机 + `camera_name` | `... camera_flag` / `multiple_render_outputs` | 已知名/`default`/未知名；报告字段 |
| 雾 / 阴影质量 / 天空 | `... fog_and_shadow_quality` | 字段真的从 JSON 传到像素；bias 越界报错、0.75↔30 像素变化 |
| 动画 / 动作 / 探针 | `cargo test -p quench3d-cli --test cli_e2e`（animation/probe/action 用例） | 状态/探针结果随步数变化 |
| 场景 include / 模板 / 实例 | `cargo test -p quench3d-cli --test scene_fixtures` | 合并、重名、路径、往返 |
| 模型上轴 | `cargo test -p quench3d-cli --lib model_up_axis` | Z-up 局部 +Z→+Y；Y-up 不变；用户旋转在轴修正之后（`R_user * R_x(-90°)`）；schema 枚举 `y`/`z` |
| 模型动画对账 | `cargo test -p quench3d-cli --test cli_e2e model` | 动画名存在/不存在、时长/轨道 |
| 蒙皮资产对账 | `cargo test -p quench3d-cli --test cli_e2e info_reports_skins` | `info --json` 的 `skins=1` / `skinned_meshes=1`，人类可读输出有「蒙皮」行 |
| morph 位移数学 | `cargo test -p quench3d --test model_loading morph_targets_displace_vertices_by_weight` | 手搓 GLB 的 `targets[0]` 解析；权重 0 原样、1 整体 +0.5、0.5 +0.25 |
| morph 资产对账 | `cargo test -p quench3d-cli --test cli_e2e info_reports_morph_targets` | `info --json` 的 `morph_targets=1`，人类可读输出有「morph target」行 |
| 动画事件标记/队列 | `cargo test -p quench3d --lib animation_events`、`cargo test -p quench3d-cli --lib model_events`、`cargo test -p quench3d-editor --lib set_animation_events` | 半开区间不重复、循环回到开头再触发、非循环结尾补触发、clip 过滤、Scene 队列取空、无 animation/animator warning；编辑器写入/清空 `models[].events`、空数组不留空段 |
| 平面贴花 | `cargo test -p quench3d-cli --test cli_e2e decal_quad`、`cargo test -p quench3d-cli --lib decals_parse` | 无光照下红色贴花让中心 R 高 80/255、R 比 G/B 高 60/255；size/rotation/color 往返；零法线/0 尺寸/越界 alpha/缺贴图报错 |
| 粒子贴图 | `cargo test -p quench3d-cli --test cli_e2e particle_texture`、`cargo test -p quench3d-cli --lib particles_parse` | 白色实例色 + 绿色贴图：无贴图中心≈白，贴图后 G 比 R/B 高 60/255；贴图路径往返、缺文件报错 |
| 状态机输入窗口 | `cargo test -p quench3d --lib animator_event_window`、`cargo test -p quench3d-cli --lib animator_parameters` | 1 秒淡入期间入队的 `b` 0.1s 还在、0.3s（窗口 0.25s）后为 0；窗口 0 时保留；`event_window: 0.35` 往返、`11.0` 报错 |
| 动画事件负载 | `cargo test -p quench3d --lib animation_events`、`cargo test -p quench3d-cli --lib event_payload` | 带 `payload` 的标记跨过后原样带出；1025 字节/NUL 在 `models[0].events[0].payload` 报错；JSON 往返保留负载 |
| morph+skin 的 GPU 蒙皮 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d --test model_loading gpu_skinning_with_morph` | 同一 GLB（关节平移 + morph 权重同 clip）：GPU/CPU 蒙皮 0/16384 像素差、覆盖 288 像素、调色板 >0 |
| 场景 morph → CLI PNG | `cargo test -p quench3d-cli --test cli_e2e scene_morph_weights_change_cli_render` | 同一个静态模型 `morph_weights:[0]` 与 `[1]` 的 PNG 有可见像素差；无 GPU 按退出码 3 跳过 |
| morph 权重校验 | `cargo test -p quench3d-cli --test cli_e2e scene_morph_weights_are_validated` | `validate` 报 `models[0].morph_weights[0]` / `[1]` 的 `out_of_range`，退出码 2 |
| morph 权重动画 | `cargo test -p quench3d --test model_loading morph_animation_interpolates_weights` | 解析 `pulse` 的 `weights` 轨道；t=0/0.5/1 权重 0/0.5/1，喂给网格后位移 0/0.25/0.5 |
| morph 动画 → CLI PNG | `cargo test -p quench3d-cli --test cli_e2e scene_morph_animation_changes_cli_render` | `animation.start=0` 与 `0.5` 的 PNG 有可见像素差；报告 `animation.morph_tracks=1` |
| 状态机过渡 | `cargo test -p quench3d --test model_loading scene_animator_transitions_and_promotes` | 停留 0.5 秒后淡入，半程权重约 0.5；淡入结束把新 clip 提为主 clip 并重新计时 |
| 状态机报告 / 首帧 | `cargo test -p quench3d-cli --test cli_e2e scene_animator_is_reported_and_changes_cli_render` | `animator.found=true`/`states=2`/`transitions=1`；立即过渡（after=0, fade=0）的 PNG 与基线不同 |
| 状态机校验 | `cargo test -p quench3d-cli --test cli_e2e scene_animator_is_validated` | 初始状态不存在、to 不存在、after 负数、fade 负数各有诊断 |
| CUBICSPLINE 采样 | `cargo test -p quench3d --lib cubic_spline_uses_tangents` | 出切线 2/s、入切线 0 时 u=.5 为 .75（线性只会 .5）；切线缺失退回线性 |
| CUBICSPLINE 解析链路 | `cargo test -p quench3d --test model_loading cubic_spline_tangents_survive_parsing_and_sampling` | 解析后 in/out 切线保留，`sample_animation(t=.5)` 的 x=.75 |
| morph CUBICSPLINE 采样 | `cargo test -p quench3d --lib cubic_morph_track_uses_tangents_instead_of_linear` | 出切线 2/s、入切线 0 时权重 u=.5 为 .75（线性 .5）；切线缺失退化线性 |
| morph CUBICSPLINE 解析链路 | `cargo test -p quench3d --test model_loading cubic_morph_animation_uses_tangents` | 解析出 in/out 切线；`morph_weights_at(.5)`=.75，网格位移 .375 |
| 双 clip morph 混合 | `cargo test -p quench3d --test model_loading blended_morph_weights_interpolate_both_clips` | 主 clip .5、第二 clip 1、blend=.5 → 权重 .75；形变位移 .375 |
| 场景动画 → CLI PNG | `cargo test -p quench3d-cli --test cli_e2e skinned_model_animation_changes_cli_render` | `models[].animation.start=0` 与 `0.5` 两张 PNG 有可见像素差；无 GPU 按退出码 3 跳过 |
| 状态机 clip 结束触发 | `cargo test -p quench3d --test model_loading scene_animator_transitions_when_clip_finishes` | 非循环 clip 播完的**下一帧首**才切到目标状态；目标 clip 重新计时 |
| `on_finished` 省略 after | `cargo test -p quench3d-cli --test cli_e2e scene_animator_accepts_finished_trigger_without_after` | `validate` 退出 0；`simulate` 报告 animator.found/states/transitions |
| `on_finished` 首帧画面 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-cli --test cli_e2e scene_animator_finished_trigger_changes_first_frame` | 初始状态已播完（start=2）时首帧切到 after(start=.5)，与基线逐像素 0 差异；不切则停在旧 clip 末帧 |
| 动画时间检查器（CPU） | `cargo test -p quench3d --test model_loading refresh_animations` | 手动把 `player.time` 拖回 0.25，调用 `refresh_animations` 后位移精确为 10.75，重复刷新不漂移、不推进时间 |
| 动画时间检查器（GPU 蒙皮） | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d --test model_loading skinned_mesh_changes_rendered_pixels` | 用 `refresh_animations_with_renderer` 在 t=0/0.25 渲染，两张 PNG 像素有差 |
| 动画倍速/循环写回 | `cargo test -p quench3d-editor --lib set_animation_params_writes_animation_and_current_animator_state` | 普通 `animation` 与状态机当前 state 都写回底稿；未知物体/状态返回 `None` 不误写 |
| 动画 clip 选择写回 | `cargo test -p quench3d-editor --lib set_animation_clip_only_writes_plain_animation_segment` | 普通 animation 的 `name` 改为 run；状态机模型/未知物体返回 `None`，不破坏 states |
| 动画片段选择器绘制 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test ui_smoke` | 普通播放实例的「片段」选择器真的出现在绘制图元里（状态机实例不显示） |
| 状态机过渡写回 | `cargo test -p quench3d-editor --lib set_animator_transition_writes_trigger_fields` | `after/fade/on_finished` 写回底稿；越界下标/未知物体返回 `None` 不误写 |
| 状态机过渡控件绘制 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test ui_smoke` | 绘制图元里出现「待机 → 走」「播完触发」；过渡行真的排版出来 |
| morph 覆盖写回 | `cargo test -p quench3d-editor --lib set_morph_weights_writes_and_removes_override` | `Some([.25,.75])` 写入 `morph_weights`；`None` 删除覆盖；未知物体返回 `None` |
| morph 覆盖控件绘制 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test ui_smoke` | 绘制图元里出现「手动 morph 覆盖」「目标 #0」；未启用时滑杆禁用仍占位 |
| 状态 clip/起始写回 | `cargo test -p quench3d-editor --lib set_animator_state_writes_clip_and_start` | 状态 `run` + `start=.75` 写回底稿；越界下标/未知物体返回 `None` |
| 状态编辑控件绘制 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test ui_smoke` | 绘制图元里出现「#0 待机」「起始」；状态行的 clip 下拉与起始时间真的排版出来 |
| 状态/过渡增删写回 | `cargo test -p quench3d-editor --lib add_remove_animator_states_and_transitions` | 加状态/加过渡后底稿真的多一项；删除状态连带删除引用它的过渡、改写 `initial`；只剩一个状态时拒绝删除 |
| 状态/过渡按钮绘制 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test ui_smoke` | 绘制图元里出现「加状态」「加过渡」；按钮在状态机实例下真的排版出来 |
| 过渡端点写回 | `cargo test -p quench3d-editor --lib set_animator_transition_ends_rejects_self_loop` | `walk→run` 写回；自环/未知状态/越界/未知物体返回 `None` |
| 初始状态写回 | `cargo test -p quench3d-editor --lib set_animator_initial_rejects_unknown_state` | `initial` 读回 `idle`、改成 `walk` 后读回 `walk`；未知状态返回 `None` |
| 初始状态控件绘制 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test ui_smoke` | 绘制图元里出现「初始（立即切换）」；无底稿映射时回退到运行时当前状态 |
| 景深参数/校验 | `cargo test -p quench3d-cli --lib dof_defaults_and_validation` | 不写 = 关闭且不序列化；焦距≤0/光圈<0/半径为负报错；光圈 0、半径 100 警告；映射值一致 |
| 景深像素变化 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test post_pipeline depth_of_field_blurs_out_of_focus_edges` | 光圈 1 时焦外边缘至少 100 像素变化；光圈归 0 后与关闭逐像素一致 |
| 景深开关绘制 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test ui_smoke` | 绘制图元里出现「景深（DoF）」；顶栏最小高度修复后不再被换行裁掉 |
| 整场景重载释放显存 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test scene_roundtrip reloading_scene_reclaims_renderer_assets` | 连开同一份场景 4 次，网格/材质/贴图数量与场景物体数每次回到同一规模，不累加 |
| 相机裁剪面/正交往返 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test scene_roundtrip camera_clipping_and_ortho_round_trip` | 编辑器设 `near=0.2`/`far=2500`/正交高度 13.5，存盘 JSON 三个字段都在，二次载入数值一致 |
| 相机投影控件绘制 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test ui_smoke` | 绘制图元里出现「正交」；顶栏可见 |
| 自动保存读写/清理 | `cargo test -p quench3d-editor --lib autosave` | 首次写入、同内容不重写、内容变化重写、`clear` 真删；间隔未到不写 |
| Web 自动保存读/写/恢复 | `tools/probe_web_ui.sh` | 写入 localStorage → reload → `recovery=true`；F9 后 `recovery=false`、`objects` 从默认场景变成 1、截图变化；随后 localStorage 立刻写出 2987 字节的新恢复点；再刷新出现恢复点，F10 丢弃后状态与键都清 |
| 崩溃恢复提示绘制 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test ui_smoke` | `doc.recovery=Some(..)` 时绘制图元里出现「发现自动保存」与「丢弃」按钮 |
| 动画面板控件绘制 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test ui_smoke` | 绘制图元里出现「倍速」「循环」控件文本，而不只是标题 |
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
| 色调映射算子 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test post_pipeline tonemap_operator` | ACES/Reinhard/线性三档渲染像素与 CPU 公式一致、互不相同 |
| 许可审计/SBOM | `python3 tools/license_audit.py --check` | 380 个外部依赖全部有许可且允许；SBOM 与 Cargo.lock 一致；GPL-only 自测被拒绝 |
| 场景 v1 迁移 | `cargo test -p quench3d-cli --lib version_one` | v1 自动升 v2、parent 被删、无 Error；v99 仍报错 |
| 关节调试线 | `cargo test -p quench3d-physics joint_visuals` + `cargo test -p quench3d --lib joint_debug` | 世界锚点/轴、删刚体不返回悬空；调试线连线与轴端点精确；Web F11 状态翻转 |
| gizmo 轴心 | `cargo test -p quench3d-editor --lib pivot_labels` | 三档标签、默认选中中心、物体/中心/世界原点求值；UI 画出「选中中心」 |
| 撤销历史跳转 | `cargo test -p quench3d-editor --test history jump_history` | 三次编辑后跳到第 1 步、redo 到第 2 步、跳到起始；标签含操作与「起始」 |
| 桌面文件选择器 | `cargo test -p quench3d-editor --lib picker` | 目录/扩展名过滤；保存补后缀与覆盖确认；三帧窗口画出标题/文件名/文件项；Enter 确认 / Esc 取消 |
| 导出路径选择 | `picker_result_updates_path_and_request` | 选 `/tmp/c.gltf` 后 `export_path` 正确、`export_gltf` 请求置位，场景路径不被改写 |
| 单 clip 时间轴 | `cargo test -p quench3d-editor --lib timeline` + `ui_smoke` | 像素→时间夹取；UI 同帧画出「时间轴」和 `0.250s / 1.000s` 读数 |
| 桌面窗口位置 | `cargo test -p quench3d-editor --lib layout` | `layout.json` 往返保留窗口坐标；半截/离谱坐标退回默认；Wayland 不生效（协议无窗口位置） |
| 场景树拖放改父 | `cargo test -p quench3d-editor --test ui_smoke outliner_drop_request` | 静态子物体拖到父行后 `parent=Some`，且世界位置不被吸到父原点 |
| 增删 / 复制 / 重置 | `cargo test -p quench3d-editor --test history` | 撤销重做、拖拽合并、快照逐字节 |
| 层级 / 改父 / 环拒绝 | `cargo test -p quench3d-editor --test hierarchy` | 世界位姿保持、导出 parent |
| 存读无损 / 模板实例 / 水 / 流体 | `cargo test -p quench3d-editor --test scene_roundtrip` | 存→读→再存字节一致 |
| 历史 / 模型实例变换 | `cargo test -p quench3d-editor` | 模型组变换写回 |
| 摄像头协议 / 3D 骨架 | `cargo test -p quench3d-editor --lib camera` | 解析/长度/坐标/线段数量/低可见度 |
| 多输出 render | `cargo test -p quench3d-cli --test cli_e2e multiple_render_outputs`（真渲染那条需 GPU） | 路径/尺寸/no-clobber/向后兼容 + 真渲染 64×32 / 32×16 两张 PNG 尺寸对得上 |
| Web 摄像头预览 + 外部 2D/3D 姿态 | `tools/probe_web_camera.sh`（Chrome 假摄像头 + `?camera=1`，需 PIL） | 预览区域两帧运动 >500（本次 19540/116000）；2D 注入后 `camera_pose=true`、洋红像素 3942；3D 世界点注入后 `camera_world=true`、视口变化 177 像素；清空后都回 false；**内置推理不做** |
| 深度点云反投影 | `python -m unittest test_camera_mocap.PointCloudTests -v` | 中心像素落主轴、y 向上翻转、近深度 z=-0.3、步长下采样 |
| 点云读入/绘制 | `cargo test -p quench3d-editor --lib camera` | `latest.points` 长度/布局校验；每点画 3 条十字线段 |

## 4. Web / 浏览器

| 功能 | 正向证据 | 判据 |
| --- | --- | --- |
| wasm 构建 + 产物自检 | `tools/build_web.sh web_client`；`node tools/check_web.mjs` | 符号/实例化/内存都在 |
| WebGL2 启动 | `tools/probe_webgl2.sh` | 第一帧日志 + 遮罩收起 + 无 panic/`INDEPENDENT_BLEND`/Cube 启发式 |
| 线上 Pages 启动 | `tools/probe_deployed.sh [URL]` | HTTP 200 + 线上 wasm 第一帧 + 遮罩收起；网络不通跳过 |
| Web UI 按键 → 状态 → 画面 | `tools/probe_web_ui.sh` | F3/F4/R/F6/F7/T/P 前后 UI 状态与中央视口截图都必须变；F5/F8/F11/K/O 只断言状态（默认场景可能没有接触/动画/关节），Tab 断言相机模式（Orbit→Fly） |
| 模型骨骼叠加 | `cargo test -p quench3d --lib model_skeleton` | 三关节链（中间夹一个非关节节点）只产出 root→mid、mid→tip 两条边；叠加层再乘模型世界变换（+10,+2） |
| 公告板粒子 | `cargo test -p quench3d --lib particle` / `cargo test -p quench3d-cli --lib particles_parse` / `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-cli --test cli_e2e particle_billboards` / `cargo test -p quench3d-editor --test history particles_survive` | 生成 N 个隐藏代理物体、初速度/重力积分、面向相机、按寿命淡出、到寿命重生（不额外多飞一帧）、非法参数回退；CLI 同相机下中心红色分量比无粒子高 40；编辑器存盘保留 `particles` 且重新载入建出发射器 |
| 父子同选 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test history selected_parent` | 父+子同选时驱动者=父、子不进组；同层旁人仍进组；只选子时驱动者=子 |
| 编辑器导出 glTF | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test history editor_gltf_export` | 顶部「导出 glTF」→ `scene_to_gltf` 合法、节点数=物体数、三角形>0；桌面写同名 `.gltf`、Web 下载 |
| 编辑器导出 GLB | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test history editor_glb_export_has_valid_chunks` | 顶部「导出 GLB」→ `scene_to_glb` 的 magic/version/length、JSON chunk 合法且无 data URI、节点数=物体数；桌面写同名 `.glb`、Web 用 Blob 下载字节 |
| 面板布局持久化 | `cargo test -p quench3d-editor --lib layout` / `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test ui_smoke` | 「布局」菜单滑杆改左/右栏宽度；`ui_smoke` 设 300/340 后断言面板实际宽度同值；本地 JSON 原子写/坏文件回默认、Web localStorage；布局不进场景文件/历史 |
| glTF/GLB 导出 | `cargo test -p quench3d-cli --lib gltf_export` / `cargo test -p quench3d-cli --test cli_e2e export_writes_single_file_gltf` / `export_writes_glb_that_info_can_read` / `export_gltf_embeds_texture_from_scene_directory` | 合法 glTF 2.0 / GLB（JSON+BIN chunk）、内嵌 base64、父子节点树（局部 translation/rotation，孙级也行）、位置/旋转动画 channel（start/speed 烘进时间轴）；相对场景目录的 PNG/JPEG 真的进 images/textures 并挂到材质；CLI 落盘后 `info model.gltf` 三角形数/包围盒一致；凸包/缺贴图跳过有报告 |
| 复制粘贴 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test history copy_and_paste` / `tools/probe_web_ui.sh` | Ctrl+C 不改场景/不进历史；Ctrl+V +2、自动重名、根物体、刚体存在、撤销/重做；浏览器框选 7 个后 Ctrl+C/V，`objects 7 → 14` 且截图变化 |
| 框选 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test history box_select` / `tools/probe_web_ui.sh` | 只按屏幕中心命中、Shift 追加、空框清空；浏览器 Shift+拖动后 `selected` 从 1 变 7，截图出现选框/选中轮廓 |
| 多选 + 成组变换 | `cargo test -p quench3d-editor --lib multi_selection` / `--lib group_member` / `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test history` | Shift/Ctrl 加减选、主选中提级、去重；组轴心=选中中心；平移/绕中心旋转/缩放三种增量都作用到附加物体；批量删除/复制后下标与选中正确 |
| IBL/HDRI 环境光照 | `cargo test -p quench3d-render --lib environment` / `cargo test -p quench3d-cli --test cli_e2e environment_map` | Radiance 平铺/新 RLE/旧 RLE 解码；cubemap 6 面/递减 mip；CLI 有 HDR 时黑球中心亮度和 > 无图 +60（无太阳/环境光，只有 IBL）；缺文件/负强度被 `validate` 拒绝 |
| 自动 LOD（渲染器） | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test auto_lod` + `cargo test -p quench3d-render --lib simplified` | 高面数球近景 4096 三角形、远景 528；`auto_lod=false` 恢复 4096；聚类网格法线归一、包围盒不塌、低模/蒙皮返回 None |
| 手工 LOD 距离切换 | `cargo test -p quench3d --lib lods_` | 3 级 LOD：5m 细模、15m 中模、25m 粗模；19m 仍在 20m 阈值的 10% 滞回带内保持粗模，17m 回到中模；注册后只有基础层可见；空/不递增层级被拒绝 |
| 手工 LOD 场景格式 | `cargo test -p quench3d-cli --lib model_lods_` / `--test cli_e2e scene_model_lods` | `lods[].min_distance` 严格递增校验、动画模型 + LOD 被拒绝、路径存在性检查；CLI 真的装载基础 + 中模 + 粗模并成功 `simulate` |
| 状态机事件 | `cargo test -p quench3d --lib animator_event` | 事件过渡写在到期的时间过渡之后仍优先；同帧发事件→`dt=0` 切到 jump；事件只消费一次；条件不满足时不切且事件丢弃；未知事件/无监听返回 false；FIFO 8 条、每帧一条、满时丢最旧 |
| 状态机参数条件 | `cargo test -p quench3d --lib animator_` | 参数 0→1 后 `dt=0` 立即 idle→run，反向条件回 idle；`after=2` 时 1 秒不切、满 2 秒后下一帧切；未知参数/NaN 被拒绝、重置回 default |
| 参数场景格式 | `cargo test -p quench3d-cli --lib animator_parameters_and` | `parameters/conditions` 序列化往返；条件引用不存在参数、参数重名都在 `validate` 报错 |
| 两骨 IK | `cargo test -p quench3d --lib two_bone` / `ik_weight` / `set_model_ik` | 可达目标：末端误差 <1e-3、两骨长度不变、pole=+Z 时膝盖往 +Z；不可达目标夹到 2.0 不拉伸；坏链整条跳过、世界矩阵逐位不变；`set_model_ik` 后骨骼叠加层末端 = 目标；weight 0/0.5/1/2 分别保持 FK、混合、精确到点、夹到全 IK |
| IK 场景格式 | `cargo test -p quench3d-cli --lib model_ik` | `ik` 数组序列化往返保留 target/pole；拼错 `poles` 报错；没有 animation/animator 时 `validate` 在 `models[0].ik` 报错 |
| IK 编辑器写回 | `cargo test -p quench3d-editor --lib set_ik_target` | 目标 XYZ 写回 `models[i].ik[j].target`；越界链/不存在模型返回 None |
| 浏览器场景往返（纯 CPU） | `cargo test -p quench3d-editor --lib` / wasm 导出探针 | 场景 JSON 往返 |

## 5. 摄像头动捕（桌面 Linux，实验）

```bash
target/tmp/camvenv/bin/python tools/camera_mocap/camera_mocap.py --source synthetic
python -m unittest discover -s tools/camera_mocap -p 'test_*.py'
tools/probe_web_camera.sh   # Chrome 假摄像头 + ?camera=1，两帧预览必须有运动
```

| 功能 | 判据 |
| --- | --- |
| 深度归一化 | 常量深度全 0、单调、NaN/Inf 安全 |
| 轮廓提取 | 最大外轮廓、bbox、面积、空 mask |
| 骨骼连接 | 35 根骨、33 个关节、低可见度不画 |
| 协议帧 | JSON 解析、RGB 长度/尺寸上限、坐标翻转、过期帧 2 s 消失 |

## 6. 明确未覆盖（不要当成绿）

* **动画状态机**：时间/播完过渡之外，第 97 轮加了 `parameters` + `conditions`（浮点输入、6 种比较、与时间条件是“与”），第 98 轮加了 `on_event` 离散事件（优先于到期的时间过渡，事件只消费一次）；编辑器有参数滑杆和事件发送按钮；仍没有参数曲线/按时间戳匹配的输入缓冲；状态机有界 FIFO（第 137 轮）、`event_window` 过期（第 144 轮）与 **clip 动画事件标记 + 队列（第 136 轮）** 已补（`models[].events`，`Game::on_animation_event`）；第 139 轮编辑器可增删/改名/改时间/改作用 clip。
* **GPU 蒙皮 / 动画混合 / IK**：glTF `skins` + `JOINTS_0/WEIGHTS_0` 已解析，**纯蒙皮与 morph+skin** 都走顶点着色器（每顶点最多 4 关节、128 关节 uniform 调色板；`Scene::gpu_skinning=false` 可退回 CPU；morph+skin 的 morph 位移仍 CPU 叠加，关节只在 GPU 乘一次）。双 clip 混合、参数/事件驱动状态机、morph target（静态 + `weights` 动画 + 双 clip 插值）、两骨 IK（`models[].ik`）、静态模型手工 LOD 已做；**全 GPU morph、按时间戳匹配/重放的输入缓冲、遮挡剔除**仍未做（`event_window` 过期已做）；TAA、两骨 IK/FK 权重、状态机 FIFO 与 clip 动画事件队列已做。
* **IBL / HDRI**：`lighting.environment_map` 已接入（Radiance HDR / PNG / JPEG，等距柱状，64px 7 级预滤波 cubemap）；没有精确 PMREM/GGX、没有从环境图提取太阳、没有`.exr`。
* **LOD / 遮挡剔除 / 大规模合批**：逐实例视锥剔除 + 静态模型手工 LOD（`models[].lods`，距离 + 10% 滞回）+ 渲染器自动 LOD（`render.auto_lod`，顶点聚类 1/2、1/4，非蒙皮网格）；没有动画/蒙皮自动 LOD，也没有遮挡剔除。
* **编辑器多选**：Shift/Ctrl 点选 + 组变换已做；框选、层级抑制（父子同选）、成组复制后的模型引用重建仍未做。
* **音频、脚本层、贴花、地形、多视口**：没有。
* **粒子**：CPU 公告板发射器（CLI/场景 JSON）已做；没有 GPU 粒子、软粒子、粒子间排序遮挡、编辑器发射器面板。
* **Web 摄像头**：桌面文件协议 + 3D 骨架；Web 端第 116 轮补了 `getUserMedia` 预览，但**没有**骨架/轮廓/深度推理。
* **真机 GPU/浏览器矩阵**：只有 headless Chrome + SwiftShader（软件光栅）；真机 WebGPU 未验证。
* **跨机器的 golden 基线**：`--golden` 已能逐像素比对，但仓库**没有**提交一份
  "官方参考图"给所有平台；不同 GPU/驱动/抗锯齿下的基线仍要各自生成。
* **UI 每一个按钮的点击**：`ui_smoke` 只证明全面板能画，`probe_web_ui` 覆盖 F3/F4/R 三个真实按键（R 切缩放/平移手柄）；其余按钮仍靠人工/单元路径，未逐一点击。
