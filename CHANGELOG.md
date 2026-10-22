# 变更日志

本文件记录**用户看得见的变化**（新能力、行为变化、破坏性改动、性能与体积）。
格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循[语义化版本](https://semver.org/lang/zh-CN/)。

> 约定：每条都写"发生了什么 + 怎么验证"。没有验证方式的变化不要写进来。

## [Unreleased]

### 修复

- **`Quat::from_rotation_arc` 在非 90° 夹角时转过头**。旧实现把四元数实部写成
  `sqrt(1 + dot)`，只有 90°（dot=0）碰巧正确；任意夹角旋转角会偏大，两骨 IK 因此
  永远对不准目标。改成标准最短弧 `normalize([cross, 1 + dot])`，并加一条
  `Y → (0, 0.6, 0.8)` 的任意夹角用例。这是库函数修正，现有调用方（IK、旋转朝向）
  都受益。
- **Web UI 探针的默认端口被占用时假红**。`tools/probe_web_ui.sh` 固定用 8140，别的会话/
  残留服务占着时本地静态服务起不来，浏览器加载不到页面，探针就报"30 秒内遮罩没有收起"，
  看起来像渲染回归。现在启动前先探测端口，占用则自动换 8141~8199；本次实测在 8140
  被占的情况下 6 项按键全部通过。
- **CI 的 ubuntu runner 没有 GPU 适配器时，蒙皮像素回归直接 panic**。新加的
  `skinned_mesh_changes_rendered_pixels` 在 `QUENCH3D_GPU_TESTS=1` 下创建上下文失败
  就 `.expect("t=0 渲染")`，把"找不到 GPU"报成像素回归失败。现在只有适配器缺失
  才打印可读跳过；着色器/渲染错误仍然 panic，不掩盖真故障。
- **网格不写深度，调试线段会从地面下穿出来**。`grid.wgsl` 写了 `frag_depth`，但
  `quench3d-grid` 管线是 `depth_write_enabled: false`——网格只按深度测试、不写回，
  地面下方的坐标轴/包围盒/骨架线全部可见。现在网格真正写深度；地面**下方**的线段
  被遮住、上方仍可见。回归：`grid_writes_depth_and_hides_lines_below_the_floor`
  （把 `Some(true)` 改回 `false` 这条会红，已实测）。
- **F3/F4 快捷键时灵时不灵（开→关，看起来没反应）**。原来事件回调和
  `Game::update` 各调一次 `handle_engine_keys`，而 `tick` 开头才清瞬时按键；同一次
  "按下 + 抬起"落在同一帧时 keydown/keyup 各处理一次，正好抵消。现在只在**按下
  事件**里处理一次（忽略 auto-repeat），`Game::update` 不再重复处理；`tick` 开头的
  `begin_frame` 顺序保持不变。回归：`press_and_release_lifecycle`（基础语义）+
  浏览器探针 `tools/probe_web_ui.sh`。
- **单级阴影的纹素吸附用了太阳自己的贴图尺寸，而不是实际建出来的那张图**。
  `DirectionalLight.shadow_map_size` 与 `RendererOptions.shadow_map_size` 可以不同，
  两者不一致时阴影会随相机轻微抖动、PCF 偏移也算错。单级路径改用
  `shadow_matrix_with_map_size(..., options.shadow_map_size)`。

### 新增

- **桌面内置文件选择器（第 121 轮）**：`picker.rs` 纯 egui 目录浏览 + 文件名输入；
  「打开」弹选择器，「保存」右键「另存为…」；扩展名过滤、缺后缀自动补、覆盖二次确认。
  Web 仍用浏览器文件输入。新增 5 条测试（总测试 774 → 779）。

- **Web 摄像头外部 2D 骨架注入（第 120 轮）**：页面暴露 `window.quench3dCameraPose`
  / `...Clear`；`set_web_pose` 解析 33 点归一化坐标，预览图上画 35 根骨 + 关节。
  `data-quench3d-ui` 加 `camera_pose`；摄像头探针注入姿态后断言状态与洋红像素
  （3934），再清空回 false。新增 1 个坐标映射单测（总测试 773 → 774）。浏览器
  仍不内置推理。

- **单 clip 可视化时间轴（第 119 轮）**：动画面板增加刻度/进度/播放头/读数，
  点击拖动定位、双击回 0；仍不是多轨混合器。新增像素→时间映射测试，并在 UI
  冒烟里断言时间轴读数与假实例的 `player.time=0.25` 同步（总测试 772 → 773）。

- **桌面窗口位置持久化（第 118 轮）**：`layout.json` 增加 `window_x/window_y`，
  启动 `with_position`、`Moved` 记录、最多 1 秒节流写盘、关闭时补写；半截/离谱坐标
  静默回默认。新增 2 个测试（布局读写 + 窗口属性；总测试 770 → 772）。无显示器环境不自动验证真实拖动。

- **场景树拖放改父子（第 117 轮）**：拖动静态物体到另一行松开 → `Doc::set_parent`
  保持世界位姿并写历史；拖到空白区解除父子。刚体/模型分组成员不参与；成环由
  `set_parent` 拒绝。新增 1 个 UI 测试（`outliner_drop_request...`，总测试
  769 → 770），验证请求真的被面板执行且世界位置不变。手势坐标未做自动化点击。

- **Web 摄像头预览（第 116 轮）**：Web 顶部「摄像头」→ `getUserMedia` →
  `<video>` → 2D canvas → egui 纹理，约 30 fps；权限拒绝/非 https/无设备/
  `video.play` 失败都显示具体错误。桌面骨架管线不变。新增 7 个 web-sys feature
  与 wasm 约 +32 KB；只有预览，无骨架/轮廓/深度推理。验证：wasm check/clippy、`build_web.sh`、`probe_web_ui.sh`、新增
  `probe_web_camera.sh`（Chrome 假摄像头两帧运动 2234–15181/116000 像素）。

- **`movie --encoder mp4`（第 115 轮）**：渲染帧通过 stdin rawvideo 流式喂给外部
  `ffmpeg`（libx264 / crf 18 / yuv420p / faststart），直接产出 MP4；不把
  ffmpeg/H.264 链进二进制。`--out` 指定文件，缺省 `movie.mp4`；奇宽高在启动
  ffmpeg 前报错；ffmpeg 缺失/失败给出可读错误。PNG/GIF/APNG 行为不变。
  回归：1 个 CLI e2e（有 ffmpeg 才跑）断言 `ftyp`、`bytes>0`、trace 位置；
  总测试 768 → 769。

- **glTF 位置/旋转动画导出（第 114 轮）**：`animations[]` 转成 glTF clip；
  `position` → VEC3 translation channel，`rotation`（欧拉角）→ VEC4 四元数 rotation
  channel，输入时间按 `start + t/speed` 烘进 accessor，`LINEAR` 采样。目标按物体名
  找节点；非法轨道/找不到目标只进 `skipped`。CLI/编辑器共用；编辑器状态栏显示动画
  条数。回归：1 个 lib 单测（2 channel + 时间轴换算 + VEC4，总测试 767 → 768）。

- **面板布局持久化（第 113 轮）**：顶部「布局」菜单可调左/右栏宽度（左 140–340、
  右 180–420），面板用 `min_size/max_size` 强制生效；桌面写
  `$XDG_STATE_HOME/quench3d/layout.json`（原子写），Web 写
  `localStorage["quench3d.editor.layout.v1"]`，坏文件/NaN/越界静默回默认。布局不进
  场景文件、不进撤销历史；启动时载入。
  回归：2 个布局单测（JSON/夹紧/文件往返）+ `ui_smoke` 断言 300/340 真的落到面板宽度
  （总测试 765 → 767）。

- **编辑器导出 GLB（第 112 轮）**：顶部工具栏加「导出 GLB」，复用
  `scene_to_gltf` 的场景语义后再转 BIN chunk（`glb_from_json`）；桌面写同名 `.glb`，
  Web 新增 `download_bytes` 用 Blob 下载二进制。状态栏报告物体/三角形/字节数。
  回归：编辑器 history 单测（chunk/无 data URI/节点数）+ UI 冒烟断言两个导出按钮
  （总测试 764 → 765）。

- **GLB 二进制导出（第 111 轮）**：`quench3d export scene.json model.glb` 复用 glTF 场景/
  贴图/节点树导出器，把 base64 buffer 解进 GLB 的 BIN chunk，JSON chunk 去掉
  `buffers[0].uri`，两个 chunk 4 字节对齐，文件头按 GLB 2.0 写；`.gltf` 路径不变。
  回归：1 个 lib 单测 + 1 个 CLI e2e（落盘后用 `info model.glb` 读回三角形数/mesh 数，
  总测试 762 → 764）。

- **glTF 节点树导出（第 110 轮）**：不再把父位移烘进子顶点。每个物体写一个节点，
  节点带局部 `translation`/`rotation`，`children` 按 `parent` 串联，场景只列根节点；
  物体缩放与部件变换仍烘进顶点，所以世界位置不变。父链成环会断开并进 `skipped`；
  空/跳过物体保留空节点，孙物体不会失去父链。
  回归：2 个 lib 单测（父子/三级孙）更新与新增（总测试 761 → 762）。

- **glTF 贴图内嵌（第 109 轮）**：`quench3d export scene.json model.gltf` 现在按场景目录
  解析 `base_color_texture` / `normal_texture` / `metallic_roughness_texture` /
  `emissive_texture`，把 PNG/JPEG 原字节内嵌进 `.gltf` 的同一个 base64 buffer，并写
  `images` / `textures` / 采样器与材质的贴图槽；`alpha_cutoff`→`MASK`、
  `double_sided`/`normal_scale`/`unlit` 也一并映射。缺文件或非 PNG/JPEG 只进 `skipped`，
  几何照常导出。编辑器导出用当前场景目录；Web 无文件系统，贴图报告未嵌入。
  回归：2 个 lib 单测 + 1 个 CLI e2e（总测试 758 → 761）。

- **编辑器导出 glTF（第 108 轮）**：顶部「导出 glTF」按钮；`scene_to_gltf` 复用
  `scene_to_json` 再交给 CLI 导出器，桌面写同名 `.gltf`、Web 下载，状态栏报物体/三角形/
  跳过项。回归：原生编辑器导出单测（总测试 757 → 758）。
- **单文件 glTF 2.0 导出（第 107 轮）**：`quench3d export scene.json model.gltf` 按扩展名
  写 glTF 2.0（几何 base64 内嵌、材质标量、alpha/自发光），几何烘进世界空间；复用 OBJ
  导出的形状/父级逻辑。贴图、`models[]`、凸包跳过并报告。回归：2 个 lib 单测 + 1 个
  CLI e2e（落盘后用 `info model.gltf` 读回三角形数/包围盒，总测试 754 → 757）。
- **父子同选的层级抑制（第 106 轮）**：父+子同时选中时，`drag_driver` 找最上层被选中的
  祖先，组里只留选中集合的根，子不再单独写一次变换（修掉"一秒走两倍"）。gizmo 的投影、
  开始拖拽、Esc 还原、物理冻结全部用驱动者。回归：原生父/子/旁人组测试（总测试
  753 → 754）。边界：不做选择去重；旋转/缩放绕父自身原点。
- **编辑器内部剪贴板（第 105 轮）**：`Ctrl+C` 存当前选择（世界变换 + 刚体形状/静态动态），
  `Ctrl+V` 偏移粘贴、自动重名、重建刚体、粘贴体成为根物体并选中；粘贴可撤销。打开/恢复
  场景清空剪贴板。回归：原生复制/粘贴/撤销/重做/空选择单测；浏览器框选 7 个后
  Ctrl+C/V，`objects 7 → 14` + 截图变化（总测试 752 → 753）。边界：不复制父子关系与
  刚体密度/摩擦等细节，不碰系统剪贴板。
- **编辑器框选（第 104 轮）**：`Shift`/`Ctrl` + 左键拖动画选框，松开后按物体**屏幕
  中心**命中并追加到现有选择；空框（非追加）清空。app 层在有修饰键时不再把左键拖动
  当转视角，框选不会带着相机转。回归：原生双物体框选单测；浏览器 CDP 探针
  `selected 1 → 7` + 截图变化（总测试 751 → 753）。边界：按中心、不做遮挡判断。
- **Web 自动保存 + 恢复快捷键（第 103 轮）**：`AutoSave` 两端可用；桌面仍写原子
  文件，Web 写 `localStorage["quench3d.autosave.scene.v1"]`，刷新/崩溃后启动挂出
  「发现自动保存 / 恢复 / 丢弃」，`F9`/`F10` 可直接恢复/丢弃。Web 的计时改用
  `performance.now()`（`std::time::Instant` 在 wasm 上会 panic），配额满只延后重试。
  浏览器探针新增「未编辑不写→reload→读→F9 恢复→再写出→reload→F10 丢弃」全链路
  （`PASS 自动保存写入`）。
  边界：Web 没有退出钩子，手动下载/丢弃才清恢复点；布局持久化仍无。
- **CPU 公告板粒子（第 102 轮）**：顶层 `particles[]`（`count`/`position`/`radius`/
  `velocity`/`spread`/`lifetime`/`size`/`gravity`/`color`）→ N 个面向相机的四边形代理；
  `Scene::update_particles` 做老化、重力、速度积分、按寿命重生与二次 alpha 淡出，
  暂停时只转向不推进；场景树/拾取/导出跳过代理物体，编辑器保存时原样保留 `particles`。
  回归：4 条场景单测、1 条场景格式单测、CLI 像素 e2e（中心红色 +40）、编辑器存盘/重载
  e2e（总测试 744 → 752）。边界：无贴图/软粒子/粒子间深度排序/GPU 粒子；编辑器无发射器面板。
- **编辑器多选 + 成组变换（第 101 轮）**：场景树/视口 `Shift`/`Ctrl` 点选加减选，
  `Doc.extra_selected` 保存附加选中；多选 gizmo 的轴心取所有选中物体世界位置的平均值，
  平移/旋转/缩放把同一增量套到整组（绕中心旋转/缩放，主物体也绕心），一次拖拽仍只记
  一步历史；「复制」「删除」按整组执行。`Doc::selection` / `is_selected` /
  `select_only` / `toggle_selected` / `selection_center` / `group_selection` 提供统一语义；
  同一模型展开的其它网格不重复套用（主物体的模型组已经带走）。场景树显示已选数量。
  回归：`multi_selection_toggle_keeps_a_primary_and_dedups`、
  `group_member_follows_translate_rotate_and_scale_around_pivot`、
  `delete_selected_removes_every_selected_object`、`duplicate_selected_copies_every_member`
  （总测试 740 → 744）。边界：框选、父子同选的层级抑制仍未做。
- **IBL / HDRI 环境光照（第 100 轮）**：`lighting.environment_map` 支持等距柱状
  `.hdr`（Radiance RGBE，平铺/新 RLE/旧 RLE）/`.png`/`.jpg`，装载时解码成线性 RGB，
  CPU 生成 64×64×6、7 级 mip 的预滤波 cubemap；`pbr.wgsl` 按粗糙度取 mip，金属/光滑
  表面有环境反射，漫反射也吃环境平均照度。`environment_intensity` / `_rotation_deg`
  可调；不写 = 解析式天空，**旧场景像素不变**。环境贴图挂在 group 3（WebGL2 的 4 个
  bind group 上限），固定纹理换图只覆写内容。`validate` 检查文件存在、强度非负、旋转
  有限；schema 同步。回归：6 条 `environment` 单测（含两种 RLE）、
  `environment_map_is_validated_and_loadable`、`environment_map_changes_pixels`（无太阳/
  环境光时 HDR 照亮黑球，总亮度差 >60；总测试 732 → 740）。
- **静态模型手工 LOD（第 99 轮）**：`models[].lods` 给静态模型挂粗模替换层
  `[{ "path":"tree_low.glb", "min_distance":25 }]`。基础模型显示到第一个距离，之后每层
  显示到下一层距离、最后一层到无穷远；所有层共用基础模型的位姿/缩放，碰撞仍用基础模型。
  `Scene::add_model_lod` 校验层级非空/距离递增，`Scene::update_lods` 每帧按相机距离切
  `visible`，阈值旁有 **10% 滞回**；`Engine::tick` 在提交前调用。编辑器「模型」面板显示
  `LOD N 级（... 起切换）`。`validate` 检查路径存在、距离 >0 且严格递增、动画/IK 模型不能
  同时用 LOD；schema 增加 `modelLod`。回归：`lods_switch_visibility_by_camera_distance_with_hysteresis`、
  `lods_reject_invalid_levels_and_noop_without_groups`、`model_lods_validate_distances_and_static_only`、
  `scene_model_lods_load_replacements_without_breaking_simulate`（总测试 728 → 732）。
- **动画状态机离散事件（第 98 轮）**：`transitions[].on_event` 声明事件名，宿主用
  `Scene::fire_animator_event(index,name)` 发事件，下一帧帧首**立即**过渡（忽略
  `after`/`on_finished`，但 `conditions` 仍要满足）。事件过渡**优先于**同状态到期的时间
  过渡；事件只保留一个、消费一次，条件不满足或没有监听时丢弃，`fire_animator_event`
  返回 `false`。编辑器为每个事件画「发送 xxx」按钮，点击后以 `dt=0` 当场判定；过渡行
  显示事件名。`validate` 检查空事件名，schema 增加 `on_event`。回归：
  `animator_event_transitions_win_over_due_time_transitions`、
  `animator_event_conditions_are_checked_before_switching`（总测试 726 → 728）。
- **参数驱动的动画状态机（第 97 轮）**：`models[].animator` 新增 `parameters`
  （`[{ "name":"speed", "default":0.0 }]`）和过渡的 `conditions`
  （`[{ "parameter":"speed", "op":"greater", "value":0.1 }]`，6 种比较）。条件与
  `after`/`on_finished` 是**与**关系；条件缺失/参数非有限一律不切。宿主用
  `Scene::set_animator_parameter(index,name,value)` 写输入，`reset_animator_parameters`
  回默认值。编辑器「动画」面板列出参数滑杆，拖动后以 `dt=0` 立即判一次过渡（暂停也生效），
  只改当前会话、不写场景 `default`。`validate` 检查参数重名/空名/非有限默认值/条件引用
  不存在的参数。回归：`animator_conditions_switch_state_and_respect_the_delay`、
  `animator_conditions_wait_for_the_transition_delay`、
  `animator_parameter_setter_rejects_unknown_and_nonfinite`、
  `animator_parameters_and_conditions_roundtrip_and_validate`（总测试 722 → 726）。
- **两骨 IK（第 96 轮）**：场景格式新增 `models[].ik`：
  `{ "root": "LeftUpLeg", "mid": "LeftLeg", "tip": "LeftFoot", "target": [x,y,z],
  "pole": [0,0,1] }`。按 glTF 节点名引用，目标/极向量在模型局部空间；每帧采样后解
  两骨三角形，只改 root/mid 朝向，骨骼长度不变；不可达目标夹到最长/最短、不拉伸。
  求解在蒙皮之前，所以 CPU 蒙皮、GPU 关节调色板、刚性片段和 `F8` 骨骼叠加层看到
  同一份关节矩阵。编辑器「动画」面板可直接改每条链的 target XYZ 并写回场景；没有
  `animation`/`animator` 时 `validate` 报错，坏链/缺失节点整条跳过。回归：
  `two_bone_ik_reaches_target_and_keeps_bone_lengths`、
  `two_bone_ik_clamps_unreachable_target_without_stretching`、
  `two_bone_ik_skips_missing_or_broken_chains`、`set_model_ik_drives_the_skeleton_lines`、
  `model_ik_chains_roundtrip_through_the_scene_format`、`set_ik_target_writes_the_model_chain`
  （总测试 716 → 722）。
- **模型骨骼叠加（第 95 轮）**：`F8` 开关在调试层画出所有播放中模型的关节线段（父关节 → 最近关节祖先，
  中间的非关节变换节点自动跳过）与关节点；线段乘模型当前世界变换，所以移动/旋转模型后骨架跟着走。
  纯函数 `skeleton_edges` 有两组离线用例（三关节链 2 条边、空 clip + 世界平移 (10,2,0)），
  Web 探针在 `ui_debug=1` 下断言 `bones` false→true。回归：
  `skeleton_edges_connect_joints_and_skip_non_joint_nodes` /
  `model_skeleton_lines_follow_the_model_world_transform`（总测试 714 → 716）。
- **GPU 蒙皮（第 94 轮）**：顶点格式加入 `JOINTS_0/WEIGHTS_0`（`Vertex.joints/weights`），
  PBR 与阴影着色器都按关节矩阵变形。场景每帧把所有**纯蒙皮**实例的关节矩阵拼进
  128 关节的 uniform 调色板（8 KB；只满足 WebGL2 最低 4 KB uniform 的设备可退回 CPU），实例只带
  `(base, count)`；不再每帧重算并回传整张顶点缓冲。带 morph 的蒙皮仍走 CPU（先 morph
  再蒙皮），调色板放不下的模型也自动退回 CPU；`Scene::gpu_skinning = false` 可整体回退。
  回归：`gpu_skinning_fills_the_joint_palette`（调色板真的被填）、
  `gpu_skinning_matches_cpu_skinning_pixels`（同一 t 下 GPU/CPU 4096 像素全等，差>3 为 0）
  + 原有 `skinned_mesh_changes_rendered_pixels`（总测试 712 → 714）。
- **场景导出 OBJ（第 93 轮）**：新增 `quench3d export scene.json model.obj`，把 `objects`
  （含模板/实例展开、父级位置与朝向、复合形状部件、材质标量色）写成 OBJ + 同名 MTL。
  本仓库的 OBJ 导入器能把导出结果读回来，因此测试是真正的"导出 → 再导入"往返：
  物体数、三角形数对得上。边界：凸包没有三角网格、`models[]` 外部模型不导出，二者都进
  `skipped[]` 明说；贴图不嵌入 MTL；导出的是文档初始位姿，不是物理步进后的快照。
  回归：`obj_export::tests::exports_vertices_faces_and_hierarchy` /
  `export_round_trips_through_obj_parser` / `exports_template_instances_as_independent_objects` /
  `convex_is_reported_not_silently_dropped`，另有 CLI e2e
  `export_writes_obj_and_mtl_that_info_can_read`（总测试 707 → 712）。
- **崩溃恢复自动保存（第 92 轮，桌面端）**：每 30 秒把可无损表达的场景写到
  `$XDG_STATE_HOME/quench3d/autosave.scene.json`（临时文件 + rename；场景里有导入模型等
  不可表达物体时跳过，避免恢复一份悄悄丢模型的场景）。正常退出删掉恢复文件，只有崩溃/
  强杀才留下；下次启动工具栏显示「发现自动保存 / 恢复 / 丢弃」，恢复只装进引擎、不覆盖
  磁盘文件。Web 端没有本地文件系统，仍不支持；布局持久化也未做。回归：
  `autosave_writes_only_on_change_and_clears`、`interval_gates_writes` +
  `ui_smoke` 的「发现自动保存 / 丢弃」绘制断言。
- **相机投影控件与裁剪面存盘（第 91 轮）**：编辑器顶栏新增「正交」开关、视场角、近/远
  裁剪面与正交高度；`scene_to_json` 现在写回 `camera.near/far/orthographic_height`
  （以前被 `..Default::default()` 丢掉，调过裁剪面存一次就还原）。回归：
  `camera_clipping_and_ortho_round_trip`（设 0.2 / 2500 / 13.5，存读一致）+
  `ui_smoke` 的「正交」绘制断言。
- **整场景重载释放显存（第 90 轮）**：`Renderer::clear_scene_assets()` 丢弃旧网格/材质/贴图
  并重置句柄空间，编辑器 `install_doc` 在清 `Scene` 后调用它、重建 `AssetCache`；反复
  "打开"同一份场景不再累加显存，旧批次也不会指向同号新资源。仍**没有**按句柄的
  `remove_*`/引用计数，单场景内反复 `load_model` 依旧只增不减。顺带给渲染器加了
  `mesh_count/material_count/texture_count` 诊断计数。回归：
  `reloading_scene_reclaims_renderer_assets`（连开 4 次，资源表与物体数不漂移）。
- **景深（DoF，第 89 轮）**：后处理新增深度景深，`post.wgsl` 用单采样深度算
  CoC，在 tonemap 前做 16 tap 黄金角圆盘模糊；天空不参与。`RendererOptions.dof`
  + 场景 `lighting.dof { enabled, focus, aperture, max_blur }`，CLI 与编辑器共用
  `runner::dof_options`，编辑器存盘会写回。UI 顶栏新增「景深（DoF）」开关与
  对焦/光圈/模糊半径。缺省关闭，关闭时像素不变；GL/WebGL2 读不了深度，整段被
  `@gl-depth` 剥离。顺带修复顶栏 `horizontal_wrapped` 换行后控件被裁掉的问题
  （顶栏现在有 240px 最小高度，SSAO/FXAA/DoF/水等控件重新可见）。回归：
  `dof_defaults_and_validation` + `depth_of_field_blurs_out_of_focus_edges` +
  `ui_smoke` 的「景深（DoF）」文本断言。
- **过渡端点与初始状态编辑（第 88 轮）**：每条过渡新增「端点」行，两个下拉框可改
  `from/to`（自环被拒绝）；状态机新增「初始（立即切换）」下拉，改初始状态会立即
  切到该状态并重建播放器，同时写回 `animator.initial`。过渡端点变化会取消正在进行
  的待进入混合，避免旧目标继续生效。回归：
  `set_animator_transition_ends_rejects_self_loop`、
  `set_animator_initial_rejects_unknown_state` + `ui_smoke` 的初始状态控件文本断言。
- **状态/过渡增删（第 87 轮）**：状态机面板新增「加状态」「加过渡」按钮和每条
  状态/过渡右侧的「删」。新状态自动命名为 `状态N` 并选模型第一个 clip；新过渡
  从当前状态指向下一个状态（0.5s 等待、无淡入）。删除状态会删除引用它的过渡、
  调整当前/待进入下标，如果删的是当前状态就重建播放器；只剩一个状态时拒绝删除。
  通过 `Doc::add_animator_state` / `remove_animator_state` /
  `add_animator_transition` / `remove_animator_transition` 写回底稿（可撤销、
  存盘生效）。回归：`add_remove_animator_states_and_transitions` + `ui_smoke` 的
  「加状态」「加过渡」按钮文本断言。
- **状态 clip/起始编辑（第 86 轮）**：状态机实例的「状态」列表现在逐行显示
  `#i 状态名`、clip 下拉和「起始」时间；改当前状态会重建播放器（保留 speed/loop/
  start，重置 finished）并刷新蒙皮/morph，改非当前状态只影响之后进入该状态时。
  通过 `Doc::set_animator_state` 写回 `models[i].animator.states[i]`（可撤销、
  存盘生效）。回归：`set_animator_state_writes_clip_and_start` + `ui_smoke` 的
  「#0 待机」「起始」控件文本断言。
- **编辑器 morph 覆盖（第 85 轮）**：带 morph target 的动画实例新增「手动 morph
  覆盖」开关与按 target 的 `目标 #N` 滑杆；勾选后把覆盖权重写进运行时
  `ModelAnimationInstance.morph_weights`（立即重传网格）和底稿
  `models[i].morph_weights`（可撤销、存盘生效），取消勾选则删除覆盖。未启用时
  滑杆以禁用状态占位，明确告诉用户模型有 morph target。回归：
  `set_morph_weights_writes_and_removes_override` + `ui_smoke` 的开关/目标滑杆文本断言。
- **状态机过渡编辑（第 84 轮）**：状态机实例的「动画」面板现在列出每条
  `from → to` 过渡，可直接改等待秒数、淡入秒数和「播完触发」勾选；改动同步运行时
  `animator.transitions[i]` 并通过 `Doc::set_animator_transition` 写回
  `models[i].animator.transitions[i]`（可撤销、存盘生效）。`on_finished` 打开时等待
  时间控件禁用，避免误以为两个触发条件会叠加。回归：
  `set_animator_transition_writes_trigger_fields` + `ui_smoke` 的「待机 → 走」「播完触发」
  控件文本断言。
- **动画片段选择（第 83 轮）**：普通模型动画（非状态机）的「动画」面板新增
  「片段」下拉框，列出资产里的 clip；切换时重建 `ModelAnimationPlayer`（保留倍速/
  循环，时间归零），刷新蒙皮/morph，并通过 `Doc::set_animation_clip` 写回
  `models[i].animation.name`（可撤销、存盘生效）。状态机的 clip 仍由 `states` 管，
  不显示该下拉框，避免把状态机结构写坏。回归：
  `set_animation_clip_only_writes_plain_animation_segment` + `ui_smoke` 的「片段」控件断言。
- **动画面板倍速/循环编辑（第 82 轮）**：右侧「动画」面板的倍速 `DragValue` 与
  循环勾选框不再只读。改运行时 `player.speed/looping` 的同时同步状态机当前 state，
  并通过 `Doc::set_animation_params` 写回 `models[i].animation`（或
  `models[i].animator.states[current]`），可撤销、存盘生效；时间滑杆仍只改当前会话。
  回归：`set_animation_params_writes_animation_and_current_animator_state`（普通片段 +
  状态机当前 state + 未知物体/状态不误写）+ `ui_smoke` 断言「倍速」「循环」控件文本
  真的出现在绘制图元里。
- **morph CUBICSPLINE 切线（第 81 轮）**：`MorphTrack` 现在同时保留
  `[入切线, 值, 出切线]` 三块，`sample_morph_track` 用与 TRS 相同的 Hermite 基函数
  求值；切线数组缺失/不齐时显式退化线性。回归：
  `cubic_morph_track_uses_tangents_instead_of_linear`（采样 .75 vs .5）+
  `cubic_morph_animation_uses_tangents`（手搓 CUBICSPLINE GLB 解析 → 权重 .75 →
  顶点位移 .375）。边界：仍没有任意权重曲线、morph 事件、编辑器 morph 滑杆。
- **状态机 clip 结束触发（第 80 轮）**：`models[].animator.transitions[]` 新增
  `on_finished: true`，当前状态的非循环 clip 播完后的**下一帧首**切到目标状态，
  不再需要猜 `after` 秒数；此模式可省略 `after`（缺省 0），`fade` 语义不变。
  回归：`scene_animator_transitions_when_clip_finishes`（帧首语义 + 目标重新计时）+
  CLI `scene_animator_accepts_finished_trigger_without_after`（校验通过、报告不变）。
  边界：仍没有输入参数/事件条件；当前状态若循环则永远不会 finished。
- **动画时间检查器（第 79 轮）**：编辑器右侧新增「动画」面板，列出每个播放实例的
  clip 名、时长、倍速、循环/播完状态与状态机当前状态；拖动 `Slider` 改
  `player.time` 后调用新 API `Scene::refresh_animations_with_renderer`，**暂停时也**
  立刻重算刚体位姿和蒙皮/morph 顶点，且不推进播放器、不写回场景文件。新增纯 CPU
  `Scene::refresh_animations`；GPU 蒙皮像素测试改用刷新入口（t=0 vs 0.25 仍有差）。
  边界：不是完整时间轴编辑器，不能选片段/改速度/加事件；状态机仍是时间驱动。
- **摄像头深度点云（第 78 轮）**：采集端把 MiDaS 相对逆深度按相机内参反投影成
  `latest.points`（`N*3` little-endian f32，缺省每 6 像素一点，约 8.5k 点），
  z 映射到 0.3~3.0 的任意尺度并在 `latest.json` 里写 `points.{count,layout,space,step,bounds}`；
  新增 `--no-points` / `--point-step`。编辑器读端校验长度/布局/坐标系，勾选
  「显示点云」后把每点画成 3 条青色小十字。回归：Python `PointCloudTests`（中心主轴、
  y 向上翻转、近深度 z=-0.3、步长下采样）+ Rust `read_frame_accepts_point_cloud` /
  `point_cloud_draws_three_cross_segments_per_point`。边界：不是米制深度，没有 TSDF；
  Web 仍只有桌面文件协议。
- **双 clip morph 权重混合（第 77 轮）**：之前双 clip 只取主 clip 的 morph 轨道；现在
  `LoadedModel::blended_morph_weights_at` 按 `blend_weight` 逐节点、逐 target 插值两条
  clip 的权重（只有主/第二 clip 有轨道的节点按各自规则处理，`blend=0` 不会凭空加上
  第二 clip 的形变），`Scene::advance_animations_with_renderer` 在淡入期间即按权重
  变形。回归：`blended_morph_weights_interpolate_both_clips`（.5 与 1 混合 → .75，
  位移 .375）。边界：morph 权重的 CUBICSPLINE 切线仍按线性取关键帧值。
- **CUBICSPLINE 真值（第 76 轮）**：glTF TRS 动画不再只取中间关键帧值。解析器保留每个
  关键帧的 `[入切线, 值, 出切线]`，`AnimationTrack` 新增 `in_tangents` / `out_tangents`；
  `Interpolation::CubicSpline` 用 Hermite 基函数按区间时长缩放切线，旋转分量求值后
  归一化。切线缺失/长度不齐时退回线性。morph 权重轨道的 CUBICSPLINE 仍只取值（线性）。
  回归：`cubic_spline_uses_tangents`（u=.5 的 .75 vs 线性 .5）+
  `cubic_spline_tangents_survive_parsing_and_sampling`（手搓 GLB → 采样 x=.75）。
- **时间驱动动画状态机（第 75 轮）**：场景格式新增 `models[].animator`
  （`initial` / `states` / `transitions`），与 `models[].animation` 互斥。当前状态
  停留 `after` 秒后开始过渡，`fade` 秒内从旧 clip smoothstep 淡入新 clip；淡入完成后
  新 clip 提为主 clip 并重新计时。引擎新增 `Scene::play_model_animator`，报告新增
  `models[].animator.{initial,found,states,transitions,missing}`。`validate` 检查
  空状态、重复状态名、未知初始状态、过渡 from/to 不存在、after/fade 越界，以及
  `animator` 与 `animation` 同时写。回归：`scene_animator_transitions_and_promotes`
  （0.5 秒后过渡、半程权重、淡入结束提为主 clip）+
  `scene_animator_is_reported_and_changes_cli_render`（立即过渡改变首帧 PNG）+
  `scene_animator_is_validated`。边界：只按时间过渡，没有输入参数/事件条件；
  没有编辑器状态机面板。
- **morph 权重动画（第 74 轮）**：解析 glTF 动画的 `weights` 输出为 `MorphTrack`
  （按节点的 target 数切帧；CUBICSPLINE 只取值块），新增
  `LoadedModel::morph_weights_at`；场景的 `models[].animation` 每帧按节点采样，
  与静态 morph 共用 `deformed_mesh`（先 morph 后蒙皮）。报告新增
  `models[].animation.morph_tracks` / `blend.morph_tracks`。手工
  `models[].morph_weights` 优先于动画轨道；双 clip 混合只取主 clip 的 morph 轨道。
  回归：`morph_animation_interpolates_weights`（t=0/0.5/1 权重与顶点位移对账）+
  `scene_morph_animation_changes_cli_render`（start=0 与 0.5 的 PNG 必须不同，
  报告 `morph_tracks=1`）。边界：不做双 clip morph 权重插值，没有编辑器滑杆。
- **静态 morph target（第 73 轮）**：解析 glTF primitive 的 `targets`
  （POSITION/NORMAL/TANGENT 位移，按节点世界方向变换），新增
  `LoadedModel::morphed_mesh` 与 `deformed_mesh`；场景格式新增
  `models[].morph_weights`（0~1，按 target 顺序，缺项 0），静态模型装载后立即写回
  GPU 顶点缓冲，蒙皮模型按"先 morph、再乘关节矩阵"变形。`info` 报 `morph_targets`。
  回归：`morph_targets_displace_vertices_by_weight`（权重 0/0.5/1 的顶点对账）、
  `info_reports_morph_targets`、`scene_morph_weights_change_cli_render`（0 与 1 的 PNG
  必须有可见差异）、`scene_morph_weights_are_validated`。边界：**静态权重**，没有
  morph 动画轨道/关键帧/编辑器滑杆；碰撞体仍是绑定姿态。
- **双 clip 一次性淡入（第 72 轮）**：`models[].animation.blend.fade` 给出淡入秒数；
  第二条 clip 的权重从第一帧的 0 按 smoothstep 升到 `weight`，`fade=0` 或省略
  保持上一轮的立即生效行为。场景层新增
  `Scene::play_model_animation_blend_with_fade`；报告 `models[].blend.fade`
  原样回显。边界：只做**一次性**淡入，没有循环淡入/任意权重曲线/状态机。
  回归：`scene_blend_fades_in_over_time`（t=.25 权重 0.5 位移 1.75、t=.5 权重 1
  位移 3.5）+ `model_animation_blend_is_reported_and_rendered` 的 fade 回显 +
  结构校验拒绝负 `fade`。
- **双 clip 固定权重动画混合（第 71 轮）**：`models[].animation.blend =
  { name, start?, speed?, loop?, weight? }` 给第二条 glTF clip；两条各自推进自己的
  时间，先对**局部** TRS 插值（平移/缩放 lerp、旋转 slerp），再统一累乘父子链。
  `weight` 缺省 0.5，0 = 只用第一条，1 = 只用第二条；报告新增
  `models[].blend`（`found` / `weight` / `duration` / `tracks`），第二条名字写错时
  两条都不登记并给警告（几何保持绑定姿态，不会偷偷只播第一条）。场景层新增
  `Scene::play_model_animation_blend`，蒙皮与刚性片段共用同一份混合世界矩阵。
  边界：**固定权重**，没有时变权重曲线、>2 条 clip、状态机、根运动/事件。
  回归：`blended_clips_interpolate_skinned_pose`（局部 TRS 数学：w=0.5 位移 1.5，
  w=0/1 精确取两条）+ `model_animation_blend_is_reported_and_rendered`（报告字段、
  名字错误警告、混合 PNG 与纯主 clip 有可见像素差）+ 结构校验用例。
- **glTF 骨骼蒙皮（CPU 路径，第 70 轮）**：装载 `skins`、`inverseBindMatrices`、
  `JOINTS_0`、`WEIGHTS_0`；每个蒙皮片段的顶点按
  `Σ w·world(joint)·inverseBind·bindInverse` 逐帧变形，再通过新的
  `Renderer::update_mesh_vertices` 原地写回同一个 `MeshId`（不换句柄、不动索引，
  选择/材质/阴影都跟着变）。蒙皮片段不再乘节点动画矩阵，避免"顶点蒙皮 + 物体动画"
  动两遍；刚性片段路径逐位不变。`info` 报告新增 `skins` / `skinned_meshes`，
  人类可读输出多一行"蒙皮"。
  边界（诚实）：**这是 CPU 蒙皮**，每顶点最多 4 关节；没有 GPU 关节矩阵、动画混合/
  状态机、IK、morph target；`models[].collide` 的静态碰撞体仍是绑定姿态。
  回归：`skinned_glb_deforms_vertices_from_joint_animation`（纯 CPU 顶点对账）+
  `skinned_mesh_changes_rendered_pixels`（GPU 上 t=0/t=0.25 两张图必须有像素差）+
  `info_reports_skins_and_skinned_meshes` + `skinned_model_animation_changes_cli_render`
  （场景 `models[].animation` 经 CLI 出图，start=0/0.5 的 PNG 必须不同）。
- **单帧黄金图回归（`render --golden`）**：渲染后把 PNG 与参考图逐像素比对，
  报告 `golden: { matched, changed_pixels, total_pixels, max_channel_diff, tolerance }`；
  超出 `--golden-tolerance`（0..255，缺省 0）退出码 1，并提示"确认是有意变化后更新
  参考图 / 用容差放宽"。仍会先写出本次 PNG，方便直接看差异；`--dry-run` 与多输出
  会在碰 GPU 之前拒绝（多张各比一张参考图没有意义）。回归：
  `golden_png_comparison_is_wired_and_fails_loudly`（同场景 0 差异、换颜色报
  `golden_mismatch`、缺文件/缺参考图参数/多输出各自的退出码）。
- **`render` / `render.outputs[]` 报告 `camera_name`**：`--camera`、`default` 别名或
  每张输出自己的 `camera` 都会如实写进报告；不传旗标的老场景不多出字段。
  回归：`camera_flag_reports_unknown_names_without_touching_the_gpu` 与
  `multiple_render_outputs_resolve_before_writing_and_honour_no_clobber`。
- **编辑器 UI 正向冒烟 + Web 按键探针**：`ui_smoke` 在无窗口引擎上把整块
  `ui::panels` 真跑一帧（工具栏/场景树/属性/视口/帮助/摄像头窗口）；新增
  `tools/probe_web_ui.sh` 用 CDP 在暂停态依次按 F3/F4/R/F6/F7/P，要求
  `<body data-quench3d-ui>` 状态与中央视口截图**同时**变化；F5 只断言状态（默认场景
  可能没有接触），Tab 断言 Orbit→Fly。`?ui_debug=1` 才暴露这个只读状态属性
  （默认不污染页面），避免"截图自己动了"的假绿。
- **`tools/verify_all.sh` + [`docs/feature_verification.md`](docs/feature_verification.md)**：
  一条命令串起门禁、WebGL2 启动、Web UI 按键、摄像头纯函数测试，并给出每类功能的
  可复现命令与"未覆盖"清单。
- **WebGL2 启动即 panic：alpha 管线的两个颜色目标 blend 不一致**。`quench3d-alpha`
  的颜色 0 用 `ALPHA_BLENDING`、水面 mask 用 `blend: None`，wgpu 因此要求
  `INDEPENDENT_BLEND`；WebGL2 downlevel 没有这个能力，`create_render_pipeline`
  直接失败。现在两个目标共用同一个 blend，mask 的 alpha 写成覆盖（非水面 0、
  水面 1），语义不变。新增 `main_pass_targets_share_blend_for_webgl2` 守卫。
- **点光源阴影纹理被 GL 猜成 Cube**。6 层正方形 D2 纹理触发 wgpu-hal 的
  `is_cube_compatible()`，与 `D2Array` 视图冲突并刷错误日志。分配改为 7 层
  （6 面 + 1 填充），数组视图明确只取 6 层；新增
  `point_shadow_texture_avoids_the_gl_cube_heuristic` 守卫。`tools/probe_webgl2.sh`
  现在把 `INDEPENDENT_BLEND` 与 `heuristics assumed` 都判为 FAIL。

### 新增

- **摄像头动捕（实验，桌面 Linux）**：新增 `tools/camera_mocap/` 采集端——普通 RGB
  摄像头 + MediaPipe Pose 33 点（2D + 3D 世界点）+ 人体分割轮廓 + 可选 MiDaS 单目
  **相对**深度。采集端只写 `latest.json` / `latest.rgb` / `latest.depth`，编辑器
  （`quench3d-editor/src/camera.rs`）轮询后显示画面/轮廓/骨架，并把 3D 世界点作为
  调试线段叠进视口。单帧、单人、无 SMPL 体型拟合、无米制深度、无点云/TSDF；
  所有推理本地运行。回归：编辑器协议/坐标/线段数量测试 + Python 纯函数测试。
- **解析式环境高光（无 HDRI）**：金属/光滑材质现在按反射方向取程序化天空（天顶/地平线/地面
  + 太阳光晕），并用 Karis split-sum 近似算环境 BRDF；`RendererOptions.environment`
  缺省开，关掉恢复旧的半球环境光路径。对金属是主要能量来源，对电介质只补一点反射。
  **不是 IBL**：没有 cubemap、没有按粗糙度预滤波、没有屏幕空间反射；反射只包含天空/地面。
  验证：`QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test post_pipeline
  analytic_environment`——同一金属球只切开关，像素必须变化。
- **透明物体全局由远到近排序**：半透明实例不再按 `(材质, 网格)` 批次顺序画，而是按
  相机距离（实例 AABB 中心）**全局**排序后逐实例绘制。此前两个不同材质/网格的透明物体
  谁先画取决于材质 id，玻璃叠玻璃会因为提交顺序不同而闪错。代价是透明物体失去合批
  （不透明路径不变）；粒子球是不透明的，所以 M4 的 4000 颗球不受影响。
  验证：`QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test post_pipeline
  transparent_`——故意先提交近红后提交远蓝，期望颜色由远到近混合；把排序去掉这条测试
  会失败（已实测）。
- **编辑器屏幕空间水面板**：左侧渲染面板新增「屏幕空间水」折叠区，可直接调颜色、清澈度、
  波浪、泡沫、折射；改的是 `engine.renderer.options.water`，下一帧生效，存盘时
  `scene_io::scene_to_json` 写回顶层 `water` 段。没有 `waters`/`fluids` 时参数仍可保存，
  但后处理保持关闭（不会凭空出现水）。
  没有水、也没改过风格时**不写** `water` 段，避免"打开白模存盘就多出五项默认值"。
  验证：`cargo test -p quench3d-editor --test scene_roundtrip water`（编辑后写回、白模不繁殖）。
- **屏幕空间水面（M4.1/M4.2）**：主 pass 增加第二颜色附件（`WATER_MASK_FORMAT`，
  `Rgba16Float`：r = 覆盖，g = 水面世界高度），水面材质（`MaterialDesc.water_mask`）
  写自身高度、其余材质写 0；后处理 `post.wgsl` 用屏幕射线与 mask.g 求交重建水面世界
  坐标，生成程序化波面法线，再做折射、吸收、菲涅耳、太阳高光与波峰/边缘泡沫。
  mask 已经过主 pass 深度测试，所以不需要读深度纹理，GL/WebGL2 也能用；
  `waters` 与 `fluids` 两条路径共用。
  顶层新增根独有的 `water` 风格：`{ color, clarity, wave_strength, foam, refraction }`
  （缺省 `[0.06,0.28,0.48]` / 1 / 1 / 1 / 1），映射到 `RendererOptions.water`；
  只影响画面，不改变浮力/粒子结果，`validate` 挡越界值，`include` 片段里写会报错。
  `render` 报告新增 `water_post`（有水域时为 true），`RendererOptions.water` 默认关闭，
  所以 API 用户不启用时像素与以前一致。`examples/fluid_pool.json` 960×600：仍为
  4909 实例、47.2 万三角形、约 10 s，相对无流体基线 7.4% 像素变化（平均绝对差 4.06；
  相对 M4.1 的 2.55% 像素、平均 0.59 变化）；把风格参数归零另有 1.75% 像素、平均 0.25
  变化；640×420 48 帧 GIF 帧间平均 8.1%。
  验证：`cargo test -p quench3d-cli --lib water_style`、
  `cargo test -p quench3d-render --lib water_mask`、
  `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test post_pipeline water_post`
  （mask + 后处理改变像素；调大风格后中心像素继续变化）。
  边界：吸收厚度固定、折射是屏幕空间偏移（不是光线追踪）、水面几何仍是平面、
  风格是全局一份；相机在水下时射线-平面重建不成立；没有焦散/水下体积光/真实波浪；
  编辑器面板只调全局风格、不能逐水域上色；mask 附件始终分配
  （约 8 字节/像素 + MSAA）。
- **粒子/水面渲染（M4）**：`SceneObject` 新增 `fluid_particle` / `fluid_surface`，
  `sync_from_physics` 每帧从 `world.fluids()` 读粒子位置与速度：每颗粒子画成 8×6 低模球
  （可视半径 `radius * 1.4`，速度越快越偏青白），水面是一张跟着最高粒子走的 alpha 平面。
  `render`/`movie`/编辑器共用同一条同步路径。M4 先落地的是 alpha 平面；屏幕空间折射/
  吸收/泡沫与波面法线由 M4.1/M4.2 补上（见上一条）。`examples/fluid_pool.json` 960×600：4909 实例、47.2 万三角形、
  约 10 s，相对无流体基线 7.4% 像素变化；640×420 48 帧 GIF 帧间平均 7.6% 变化。
  验证：`cargo test -p quench3d --lib fluid_visuals_follow`（粒子/水面同步，不需要 GPU）。
- **流体 ↔ 刚体双向耦合（M3）**：粒子撞到动态刚体时按动量守恒反推法向冲量（只反推 20%，
  避免成千上万颗粒子把刚体当鼓面敲），并把睡眠体叫醒；反过来，粒子的**近似自由表面**
  （刚体 XZ 附近粒子的最高 y）与 `water::submerged_in` 给刚体**浮力 + 浸没阻尼**
  （同一套采样，不重复实现体积/形心）。`fluids[].drag` 新增（缺省 6，0 = 无阻尼），
  `simulate` 的 `fluids[].contacts` 报告本步粒子-刚体接触数（0 时不出现）。
  `examples/fluid_pool.json` 里 300 kg/m³ 的浮球 480 步后停在水面下 26%（密度比 0.3，
  符合阿基米德），流体最高速度 0.38 m/s。**不写 `fluids` = 行为与以前逐位相同**。
  边界：浮力用瞬时最高粒子当水面（飞溅会抬高读数），多流体只取最大浸没占比、不叠加浮力；
  还没有表面张力/涡量/粒子间碰撞。验证：`cargo test -p quench3d-physics fluid`
  （重物被压下去、轻物浮住、睡眠体被叫醒、确定性）、`cargo test -p quench3d-cli --test
  cli_e2e fluid`（报告 `contacts` 与浮球位置）。
- **`waters`：解析水域 + 浮力（M1，不是流体求解器）**：场景新增 `waters: [{ center, size,
  level, density, drag }]`，动态刚体按**排开体积**受浮力（作用在排开体积形心上，因此有扶正
  力矩）、按浸没比例受线性阻尼；`simulate` 报告新增 `waters` 计数与 `states[].submerged`
  （0~1，人读输出显示"浸没 %"）。渲染把水域画成半透明水面（alpha 混合批次，排在 opaque 之后）。
  **不写 = 没有水，物理结果与以前逐位相同**；浮力是环境力，**不会把睡眠体叫醒**（睡眠体连采样都跳过，`submerged` 停在最后醒着的值），
  水面变化后要显式唤醒。边界：没有粒子/流动/波浪/折射，没有池底（半空间）；这是 M1，PBF 粒子流体是
  后续批次。验证：`cargo test -p quench3d-physics water`（球浮到水面、重物沉底、确定性）、
  `cargo test -p quench3d-cli --test cli_e2e water`（报告字段与校验路径）、
  `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test scene_roundtrip waters`
  （编辑器存读保留 + 水面物体存在）；`render` 有无水域对比：25.9% 像素变化，差异区域在画面
  下半部（水面），不是全屏噪声。

- **`fluids`：CPU PBF 粒子流体（M2，单向耦合）**：场景新增 `fluids: [{ center, size, spacing,
  radius, rest_density, viscosity, iterations, max_particles }]`，从长方体按固定晶格发射粒子，
  用 **Position Based Fluids**（密度约束 + 迭代位置修正 + XSPH 粘性）求解；粒子被静态/动态
  刚体推开（**单向**：刚体 → 流体，还不会反推刚体）。粒子质量按晶格静止密度校准（静止时
  密度比≈1），边界投影放进迭代内部（先陷进地面再一次性弹出来会炸）。`simulate` 报告新增
  `fluids[]`（`particles` / `avg_density_ratio` / `max_speed` / `min_y` / `max_y`），人读输出
  有每个流体一行。**不写 = 没有粒子，物理结果与以前逐位相同**。边界：没有表面张力/涡量/
  粒子间碰撞，敞开的平地上会摊成薄层（`examples/fluid_pool.json` 用四堵墙围成池子）；
  `TriMesh` 碰撞体不挡粒子，凸包/圆锥用 AABB 近似；粒子**渲染**在 M4（现在 `render` 还看不到
  粒子）。验证：`cargo test -p quench3d-physics fluid`（晶格确定性、落地不穿地、围池能堆成
  三维水堆）、`cargo test -p quench3d-cli --test cli_e2e fluid`（报告字段与 `fluids[0].spacing`
  校验路径）、`cargo test -p quench3d-editor --test scene_roundtrip fluids`（存读保留）；
  `examples/fluid_pool.json` 240 步：4000 粒子、密度比 1.01、最高速度 0.33 m/s、y 0.05..1.25。

### 工程卫生

- **CI 不再出现无法诊断的 Windows 假红**：在**没有 GPU 的 Windows runner** 上创建真实 wgpu
  上下文会**原生崩溃**（`STATUS_ACCESS_VIOLATION` / `0xc0000005`），进程直接死、Rust 侧
  拿不到 `Err` —— "没 GPU 就返回 `Err`"这种兜底**接不住原生访问违例**，CI 于是只报
  `process didn't exit successfully`。两次实测：run 38935053883（`model_instances`）与
  run 38931539230（`backend_fallback`），同 sha 重跑即绿。
  现在这类测试改成**显式 opt-in**：设 `QUENCH3D_GPU_TESTS=1` 才执行，否则**整条测试体不执行**
  并打印可读的跳过原因（不是删断言、也不是体内 `if false`）；CI 在 ubuntu / macOS 腿上打开它，
  windows 腿给 `"0"`。**实测只有 macOS 腿真建得出上下文**（Metal，0.22s / 0.17s）；
  ubuntu-latest 没有适配器（0.00s，走内层"没有可用的 GPU"分支），所以那两条断言在 ubuntu 上
  仍是空的 —— 这与改动前一致（"覆盖本来就是 0"，不是"这次丢了覆盖"）。
  本地 `tools/gate.sh` 默认打开（本机有显卡），没有显卡的机器用
  `QUENCH3D_GPU_TESTS=0 tools/gate.sh` 关掉。
  验证：`cargo test -p quench3d-render --test backend_fallback` 与
  `cargo test -p quench3d-editor --test model_instances` **各跑两态**（不设变量 → 0.00s、
  可见跳过原因；设了 → 0.14s / 0.96s、真的在真上下文上过）；不需要显卡的守卫测试
  `gpu_gate_builds_nothing_unless_explicitly_enabled` + **三条变异红-绿**
  （开关恒 `true` → 红；判定内核去掉拦截 → 红；打开开关时改坏断言 → 红，改回 → 绿）。
  **上线实测**：CI run `38955064316` @ `2a0f0db`，`attempt = 1`，五腿全绿 ——
  `test (windows-latest)` **首跑绿**（同一 target 的可执行文件名
  `model_instances-8259af2638c9b300.exe` / `backend_fallback-8cfe002d8085f3e7.exe` 上一轮报
  `0xc0000005`，这一轮 `test result: ok. 5 passed` / `ok. 7 passed`，全日志
  `STATUS_ACCESS_VIOLATION` 出现 0 次）；ubuntu 的两个 target `0.00s`（该 runner 真的没有适配器，
  走的是内层"没有可用的 GPU"分支），macOS `0.22s` / `0.17s`（真的建出了 Metal 上下文）。
  判据、为什么不选 `cfg!(windows)` / known-flake、以及边界（其余"真建上下文"的测试二进制
  仍是欠账）见 `docs/overnight_report.md` §52 与 `docs/production_readiness.md` §11。
- **另外 8 个"真建 wgpu 上下文"的测试也改成了显式 opt-in，并加了一条会自己发现新裸奔者的守卫**
  （第 58 轮下半场）：`post_pipeline` / `shadow_cascades` / `material_pipeline_traits` /
  `fog_and_shadow_quality` / `point_lights`（render）与 `hierarchy` / `history` /
  `scene_roundtrip`（editor）—— **断言一条没删、没放宽**，只是把建上下文那一步放进
  `with_gpu_tests` 的闭包里（没设 `QUENCH3D_GPU_TESTS=1` 时闭包一次都不执行并打印原因）。
  新增守卫 `every_real_gpu_context_creation_in_tests_goes_through_the_gate`（不需要显卡，
  每条 CI 腿都跑）：扫描 `crates/*/tests/*.rs`，剥掉注释与字符串后断言"凡建 wgpu 上下文
  必过 `with_gpu_tests`"，**自动覆盖将来新增的测试文件**。
  验证：三条变异红-绿（拆掉 `point_lights.rs` 的包裹 → 红并点名 `point_lights.rs:28`；
  新建一个裸奔测试文件 → 红并点名它；把 `code_only` 退化成不剥注释 → 红，误报点正是**别人**
  文档注释里的 `GpuContext::headless`）；本机两态实测（关：10 个二进制全 `0.00s` + 逐条原因；
  开：全真跑且绿，`post_pipeline` 4.09s / `shadow_cascades` 2.34s / `scene_roundtrip` 2.29s …）。
  CI 另加一段"只跑这 10 个二进制并加 `-- --nocapture`"的步骤 —— 整树测试会把通过测试的输出
  吞掉，"跳过"和"没跑"在日志里分不出来，那一段专门用来把跳过原因留在日志里。
  判据、守卫的边界（词法）、以及"门禁是整树的、同一棵树只允许一个写者"这条教训见
  `docs/overnight_report.md` §54 与 `docs/production_readiness.md` §13。

### 新增

- **模型动画接进场景格式与帧循环**（gap #23 的第三刀：`models[].animation`）
  - **格式**：`models[i].animation = { "name": "走路", "start"?: 0, "speed"?: 1, "loop"?: true }`。
    不写 = 静止，老场景逐字节不变；`loop` 缺省 **true**（与 `animations[].loop` 缺省 false 相反，
    模型片段通常是循环的），写 `false` 播完停在最后一帧。
  - **消费点同批落地**（不留"能解析、能校验、没人消费"的字段）：`Scene::advance_animations(dt)`
    每帧推进所有登记过的模型动画，窗口循环（`Engine::tick`）与无头逐帧循环
    （`render_sequence_with_game` / `movie`）都在 `sync_from_physics` 之后调一次、暂停时不推进；
    单帧 `render` 用 `dt = 0` 摆一次起始姿态。`LoadedModel::spawn` 改返回
    `SpawnedModel { objects, base }`，让"片段 → 物体下标 / 片段 → 出生变换"两份映射同源，
    `base × 动画矩阵` 每帧重算（不是在上一帧结果上累乘，所以不会漂）。
  - **名字不存在只能装载后判定**：`validate` 不读资产，只查结构（名字非空 / `speed > 0` /
    `start` 有限且 ≥ 0，诊断路径逐项可对）；真正的对账在 `simulate` / `render` / `movie` 报告的
    `models[]`：`found`、`duration`、`tracks`、资产里全部 `clips`，并在 `warnings` / `notes` 里
    说明"不会播、模型保持绑定姿态"。写错名字**不是**校验错误（退出码 0）。
    `quench3d info 模型.glb` 新增 `animations`（agent 发现可用名字的入口）。
  - **边界（诚实）**：这是**视觉**动画 —— 只改模型展开物体的变换，不动物理；
    `models[].collide` 的静态三角网碰撞体仍是绑定姿态，不会跟着动。`skins` + GPU 蒙皮、
    混合（多轨道权重）、`CUBICSPLINE` 真值、morph target 仍未做。像素级"真的动了"留给有 GPU
    的腿（本机 + macOS CI），本轮自动验证止于 CPU 与报告。
  - 验证：`cargo test -p quench3d --lib -- anim_tests`、
    `cargo test -p quench3d --test model_loading`（真 GLB 字节 → `Scene::play_model_animation`
    → 物体真的移动/循环绕回）、`cargo test -p quench3d-cli --test cli_e2e -- model_animation`
    （名字对/错/不写三态 + 结构校验 + `info` 列动画）、
    `cargo test -p quench3d-editor --test scene_roundtrip -- model_transform_edits_are_exported`
    （编辑器存一次再读一次不丢 `animation` 字段）。

- **glTF 动画解析 + CPU 侧采样**（gap #23 的第一刀）
  - `LoadedModel` 现在带**节点表**（`nodes: Vec<LoadedNode>`：名字、父节点、局部 TRS）和
    **动画**（`animations: Vec<LoadedAnimation>`）；轨道覆盖平移/旋转/缩放，插值支持
    `STEP` 与 `LINEAR`（旋转用球面插值）。
  - `sample_animation(name, t) -> Option<Vec<Mat4>>`：给出某条动画在 `t` 秒的**节点世界矩阵**；
    没被轨道覆盖的节点保持绑定姿态（"只动一部分关节"的资产需要这个）；名字不存在返回 `None`
    （不是 panic、也不是静默给绑定姿态）。
  - `mesh_matrix(mesh_index, worlds) = world(t) × bind_inverse`：几何是按绑定姿态烘过的，
    所以画动画只需要这一个矩阵；**绑定姿态下它恰好是单位矩阵**（静态路径逐位不变，
    测试盯着这条不变量）。
  - `Scene::apply_animation(&model, objects, base, &player)`：把播放器当前帧应用到场景物体上
    （`base × 额外矩阵`，**不是**在上一帧结果上累乘，所以不会漂）；只动被轨道覆盖的网格片段，
    越界下标跳过。这一步纯 CPU（`MeshId` 只是句柄），所以"动画到底动了什么"在没有显卡的
    测试里就能断言：t=0 不动、t=0.5 滑轮 +1 而静止物体一分不动、t=1（关掉循环）+2 而不是 +3。
    变异 2 组全红（组合时丢基础变换 / 在上一帧结果上累乘）。
  - 另有一条**无操作不变量**：模型没有动画、或动画名对不上（资产改名/旧场景）时，
    `apply_animation` 必须严格不动几何 —— 否则表现成"模型突然飞到原点"而没有任何诊断。
    `base` 比 `objects` 短时用物体当前变换当基准，不 panic、不出 NaN。
  - `ModelAnimationPlayer`（`model.player(name)`）：纯 CPU 的**播放状态** —— `advance(dt)` 推时间
    （循环用 `rem_euclid`、不循环夹到时长并置 `finished`、`speed` 倍速、`duration <= 0` 不除零、
    `dt` 非法不产生 NaN），`mesh_matrices(&model)` 直接给出"每个网格片段该用的矩阵"。
    渲染侧只要把它乘到片段的基础变换上，播放语义（循环/夹取/倍速）不必在渲染循环里重写一遍。
  - 测试（手搓 4 节点 + 2 轨道 GLB）：node 表/父子关系、时长、t=0/0.5/1 的平移采样、
    90° 旋转、子节点跟随父节点、"烘过的几何 + 动画矩阵 = 动画后位置"、未知名字 → None、
    绑定姿态单位矩阵；播放器的时间推进/倍速/循环/播完/非法输入。变异 5 组全红
    （忽略时间 / 忘掉 bind_inverse / 丢掉动画 / 不循环 / 忽略倍速）。
  - **边界（诚实）**：这一轮只到**引擎 API 层**。还没做：场景格式里的动画字段（`objects[].model.animation`）、
    渲染循环按时间推进、混合（多轨道加权）、`skins` 与 GPU 蒙皮；`CUBICSPLINE` 退化到线性
    （只取关键帧的"值"，丢切线）；形变动画（morph target）跳过不猜。顺序见
    `docs/production_readiness.md` §19.2/§19.3。

- **接触余量对复合刚体也生效 + 数值范围校验 + 专门 e2e**（gap #20 的另外两条欠账）
  - 以前「余量只作用于单形状刚体」：复合刚体贴上墙之前没有早接触，表现和没开余量一样。
    现在逐部件膨胀（每个部件都是凸体，膨胀它就是它自己的 Minkowski 和 —— 不需要重算
    刚体的局部质心或聚合 AABB，那些只跟**装配**有关、跟窄相用哪个几何无关）。
    测试 `speculative_margin_applies_to_compound_bodies`：2 cm 的空气 + 5 cm 余量
    必须产生接触对，而且**不能**把没碰上的物体推开。
  - `physics.speculative_margin` 现在有校验：负数/非有限 → `out_of_range`；
    > 1 米 → `out_of_range`（那不是"提前一点点算接触"，是写错了）；
    0.25~1 米 → `suspicious_value` 告警（物体之间会"还没碰到就被推开"）。
  - 专门 e2e `speculative_margin_reduces_penetration_on_impact`：同一个球掉到地上，
    开 6 cm 余量的轨迹**低点更高**（提前接触、吃得浅），并且余量不改变静止状态。
    这条是"余量真的生效"的唯一观测口径（单测只能证明配置读到了）。

- **圆角盒（`shape.type = "rounded_box"`）** —— gap #18「形状集合固定 6 种」的第三件
  （6 种 → 9 种，本条补齐 #18）。
  - **格式**：`size`（外接**边长**，和 `box` 同一套）+ `radius`（圆角半径，**必填**，
    ≤ 最短边长的一半；超过会被校验拦下并提示"再大就滚成胶囊/球了"）。
  - **几何**：盒子 ⊕ 半径 `radius` 的球。支持函数精确（`support_K(d) + r·d̂`），
    AABB 就是外接盒，`inflated(margin)` 也是**精确**的 Minkowski 膨胀（外沿与半径同时长胖），
    这在接触余量（`speculative_margin`）下比圆柱/圆锥的近似膨胀更干净。
  - **体积**用 Steiner 闭式（8ixiyiz + 8(...)r + 2π(...)r² + 4πr³/3）；
    **惯量**用确定性网格采样（32³ 个格心、`mass / 命中数` 归一），误差 < 1%：
    闭式解要按"6 面片 + 12 段 1/4 圆柱 + 8 个 1/8 球"逐块套平行轴定理，写错一位只表现为
    "滚起来有点怪"，没人看得出来。用球/盒两个已知解对账（见测试）。
  - **射线**用**解析 SDF** 球体步进（不是复用的 GJK 版本）：GJK 的"点-形状最近点"在
    点位于形状内部时**不返回 0**（返回单纯形上某个顶点的距离，实测能到 0.57），
    而球体步进一旦踩进内部就会跳过头 —— 这条是这轮踩到的真坑。
  - **网格**按尺寸生成（`MeshData::rounded_box`：从球的**方向网格径向投影**到圆角盒表面，
    二分求根 + 解析法线），按（边长 + 半径）的内容哈希缓存，`render_scale` 恒为 1。
    代价是疏密不均（面片中心三角形大、圆角处密），换来的是 20 行而不是 200 行。
  - **`--overlap rounded_box:sx,sy,sz,r[@x,y,z]`** 也能用（有解析的点在内判据，和高度场不同）。
  - **编辑器**：反推是精确的（`size = 2·half_extents`、`radius` 原样），所以往返测试覆盖到了。
  - **验证（都不需要显卡）**：形状支持函数/AABB/体积/膨胀、惯量对球与盒两个闭式解、
    平放与斜落两种静置、五组形状对的窄相与射线、网格（顶点在表面、法线 == SDF 梯度、
    AABB == 外接盒、退化输入不 panic）、格式校验 5 种坏输入、CLI 映射表 + e2e
    （与同尺寸箱子并排落地都停在半高、轨迹形状名、重叠查询、半径超限的人话错误）。
  - **边界**：惯量是采样值（误差 < 1%，标量各轴独立采样）；网格疏密不均；
    不提供"添加圆角盒"的编辑器工具栏按钮（半径逐物体不同，得写 JSON 或用底稿恢复）。
    见 `docs/production_readiness.md` §17。

- **地形高度场（`shape.type = "heightfield"`）** —— gap #18「形状集合固定 6 种」的第二件
  （6 种 → 8 种）。以前只有 `models[].collide` 能间接用上三角网，地形得靠外部模型文件；
  现在可以**直接写在场景里**。
  - **格式**：`heights` 是**行主序**的二维数组（`heights[行][列]`，行沿 Z、列沿 X），
    `cell_size` 是格子边长（米，缺省 1）。网格在 XZ 平面**以物体原点为中心**，
    高度是**相对原点**的，所以 `position` 能整体抬起地形。
  - **物理**：展开成**静态三角网**（`Shape::TriMesh`，每格两个三角形、绕序朝 +Y），
    复用已有的"三角网 vs 任意形状"窄相；半空间接触、射线、复合形状与 `models[].collide`
    走的是同一条路径。**只能 `body: "static"`**：写 `dynamic`/`kinematic` 会被校验拦下
    （三角网没有质量属性，引擎本来会把这类刚体静默降级成静态 —— 那不是"能用"，是陷阱）。
  - **一份几何、两处使用**：渲染网格与物理三角网都来自 `MeshData::heightfield`
    （新增的生成函数），顶点/索引**逐位相同**，所以"看到的"就是"踩到的"；
    网格按**内容哈希**缓存，同一份地形只上传一次显存。
    UV 覆盖整块地形 `[0,1]²`，于是材质的 `uv_scale` 是"整块地形上的平铺次数"。
  - **校验**：至少 2×2、每行列数相同、高度必须有限、`cell_size` 为正有限数、
    格点数（行×列）≤ 65536（≈13 万三角形），越界给 `out_of_range`/`invalid_value`/
    `limit_exceeded` 并指到精确 JSON 路径；`--overlap heightfield:...` 会明确告诉你
    "静态三角网没有解析的点在内判据，请用 raycast" 而不是猜。
  - **编辑器**：地形在编辑器里能装载、能摆位姿，存盘时按名字从**底稿**恢复形状
    （编辑器没有形状编辑面板，所以"形状没被改过"成立）—— 没有这条兜底，
    「载入带地形的场景 → 随手存一次」会把整块地形从文件里删掉（静默丢数据）。
    工具栏**不**提供"添加高度场"按钮：它是写 JSON 的地形，不是摆件图元。
  - **验证（都不需要显卡）**：`MeshData::heightfield` 的单元测试（格点数/三角形数/居中性/
    绕序朝上/UV 范围/退化输入不 panic）、CLI 单测两条（物理三角网与渲染网格逐顶点对账 +
    内容哈希缓存名）、校验单测一条（8 种坏输入各自的码与路径）、e2e 一条
    （两个球停在地形上、`lowest_y` 证明 `position` 抬起了地形、射线距离 3.5 与法线朝上、
    轨迹报告里形状名 `heightfield`、`--overlap` 的人话错误、动态刚体被拦）、
    编辑器两条（底稿兜底能恢复 `heights`/`cell_size`，且"有刚体"与"没刚体"两条路径都接了）。
  - **边界**：窄相是**逐三角形**的（有逐三角形 AABB 早退，但没有 BVH/网格层次结构），
    65536 格点 ≈ 13 万三角形已经是"能跑但吃力"的量级 —— 大地形请切块（多个物体各自一块），
    或者等专用加速结构；高度场不能当 `--overlap` 查询形状；高度场的**复合形状部件**存盘时
    仍会报 skipped（按名字整体套用会串形状）。见 `docs/production_readiness.md` §16。

- **圆锥（`shape.type = "cone"`）** —— gap #18「形状集合固定 6 种」的第一件（6 种 → 7 种）。
  底面半径 `radius`（缺省 0.5）、半高 `half_height`（缺省 0.5）；底面在 `-half_height`、
  顶点在 `+half_height`（与圆柱/胶囊同族的本地朝向：轴沿 +Y）。
  - **引擎**（`crates/quench3d-physics`）：`Shape::Cone` 的**支持函数**是闭式的
    （顶点与底面圆周二选一），所以 GJK/EPA 直接就能算「锥 vs 任何凸体」，不需要为每一对写专用路径；
    质量属性是实心圆锥的闭式解（体积 `πr²H/3`、绕轴 `3mr²/10`、绕质心横轴
    `m(3r²/20 + 3H²/80)`），而且如实报出**质心偏移** `(0, -half_height/2, 0)` ——
    实心圆锥的重心不在几何中心（引擎本来就用 `MassProperties::center` 支持这件事，凸包也是这么报的），
    所以重力作用点、静置高度、转动惯量都对。AABB / 包围球 / `inflated` 同步补齐。
  - **半空间接触**：顶点 + 底面圆周 8 个点（与圆柱同一套做法），立着放时靠底圈稳住；
    **射线查询**：侧面当二次曲面解（`x²+z² = k²(y-h)²`）、底面当圆盘，取近的那个。
  - **渲染**：`MeshData::cone` 本来就有，这次补的是**中心在原点**的单位网格
    （`assets.unit_cone`：底面 y=-0.5、顶点 y=+0.5），于是 `parts[].rotation` 绕的是几何中心；
    物理形状与网格的尺寸由 `crates/quench3d/tests/unit_primitives.rs` 逐轴对账
    （把球/盒/圆柱/胶囊也一起对了一遍 —— 以前没有测试盯着"网格 == 形状"）。
  - **可达**：`validate` / `simulate` / `render` / `movie` 都认；`--overlap cone:0.5,0.5`
    也能当查询形状；编辑器的"添加"工具栏多了**圆锥**按钮（单位尺寸与其他图元一致）。
  - **验证（都不需要显卡）**：物理 4 条（质量/惯量/质心闭式解、支持函数与包围盒、底面朝下静置
    在 `y=half_height`、顶点朝下会翻倒且不穿地）+ 射线 1 条（侧面/底面/顶点三段手算距离）+
    CLI e2e 1 条（静置/翻倒/静态锥/轨迹里形状名 `cone`/坏参数给出 `out_of_range` 与精确路径）+
    映射表 1 条（每种 `ShapeKind` 的物理形状与渲染缩放一致，含 `unit_cone` 与物理 AABB 对账）。
  - **边界**：锥-锥 / 锥-盒走 GJK+EPA 的**单点**流形（静置稳定性弱于盒-盒，与圆柱、凸包同一档，
    堆叠圆锥会互相滑）；没有专门的"锥台"（截头圆锥）与"倒角"参数；圆角盒、高度场仍未做
    （gap #18 剩下的两件）。见 `docs/production_readiness.md` §15。

- **场景可以拆成多个文件了（`include`）** —— gap #13「多文件组合 / 跨文件 prefab」，
  这是 gap 分析里最后一件"格式层大件"。
  以前一份场景只能是一个文件：材质、模板、关节组想被多个场景复用，只能复制粘贴 JSON；
  agent 生成大场景时同一段描述要反复吐 token，改一处要改多处。
  - **格式**（`crates/quench3d-cli/src/scene_doc.rs`）：顶层 `include: ["parts/wall.json", ...]`
    （可选）。相对路径以**写这一行的那个文件**所在目录为基准，候选位置与贴图/模型同一套
    （文件目录 → `assets.search_paths` → `--asset-path` → 工作目录 → 引擎搜索路径）；
    片段可以再 `include`（最多 16 层）；**同一个文件在一次解析里只并入一次**（菱形包含会得到
    一条 `ineffective` 警告，不是静默跳过）；成环 → 新码 `include_cycle`（消息里给完整链条）。
  - **合并语义**：片段只能贡献**可合并的段** —— `materials`/`templates`/`cameras`
    （具名表，**重名 = `duplicate_name` 错误**，消息里点名两份文件）与
    `objects`/`models`/`joints`/`actions`/`probes`/`animations`（数组，按声明顺序追加：
    **本文件自己的内容在前**，被包含的在后）。根独有的段（`ground`/`camera`/`lighting`/
    `simulate`/`render`/`gravity`/`physics`/`name`/`assets`）写在片段里是 `invalid_value`
    错误 —— 这些段只有一个实例，"谁赢"没有答案，宁可报错也不含糊合并。
  - **片段里的资源相对路径按片段自己的目录重定基一次**（4 个贴图槽、`models[].path`），
    所以 `parts/wall.json` 里的 `tex/brick.png` 指的是 `parts/tex/brick.png`，
    片段搬到哪都跟着自己走；`include` 自己写的路径**不**重定基。
  - **不写 `include` = 老行为逐字节不变**（`skip_serializing_if` + `#[serde(default)]`，
    不读任何额外文件、不重定基任何路径；有测试钉住序列化字符串相同）。
  - **可达性**：`validate` / `simulate` / `render` / `movie` / `ask` / MCP 全部自动生效
    （CLI 在 `load_scene` 里合并一次，world 装配、探针、报告看到的都是同一份合并文档）；
    编辑器能装载，存盘时把片段**内联**进根文件并清掉 `include`（存盘报告里说明内联了哪些文件 ——
    编辑器编辑的是合并后的世界，保留 `include` 会重复并入，只清不合并会丢掉片段里的
    `models`/`joints`/材质，两条都是静默错法）；JSON Schema、`scene_grammar()`、系统提示词三处同步。
  - **验证**（都不需要显卡）：`cli_e2e.rs` 6 条 —— 其中 **多文件版与手写单文件版的 `states`
    逐物体逐坐标位相同**（`position`/`rotation` 直接比 f32）；片段贴图**只**放在片段目录
    （按根目录解析会找不到）；缺失/成环/重名/根独有段/片段内容错误各自的码与路径；
    菱形包含只并入一次且给警告；跨文件 `templates` + `instances` 摆出 3 个刚体；
    不写 `include` 的场景报告与退出码全同。`scene_doc.rs` 6 条单元测试（合并顺序、嵌套相对路径、
    重定基后的**路径字符串**、幂等、五类失败、字节不变）；`quench3d-editor/tests/include_export.rs`
    4 条（内联 + 清 `include` + 坏片段拒绝存盘 + 源码结构守卫，因为编辑器往返测试要真 GPU）。
    **变异验证**：重定基改空操作 → 单元 + e2e 各红一条；重名静默覆盖 → 红；让合并搬运 `include`
    → 3 条红；编辑器绕过合并 / 材质继承查未合并底稿 → 各红一条。
  - **没做**（边界）：`variant`（按场景覆盖片段字段）、条件包含、资源去重报告、
    编辑器的"另存为片段"；浏览器纯内存入口 `from_json` 没有场景文件目录（那里的 `include`
    相对路径按工作目录解析，没在真浏览器里验证）。详见 `docs/production_readiness.md` §14。

- **固定关节的参考姿态真正生效了（`joints[].frame_a` / `frame_b`）** —— gap #15 的第三段，
  也是最后一段接线。
  以前引擎的 `Joint::Fixed` 是**速度级**角约束：它锁的是"创建那一刻"的相对姿态，
  `local_frame_a/b` 只是被保存下来、求解器根本不看。于是"把这扇门扭到与门框成 90°"
  只能靠手算世界朝向、把物体初始 `rotation_deg` 摆好 —— 一旦两端各自还有朝向（比如 B 的
  "正面"是局部 +X、A 的是局部 +Z），就没人能表达"让这两个特征对齐"。
  - **引擎**（`crates/quench3d-physics/src/joints.rs`）：`Joint::Fixed` 的 3 自由度角约束
    多了一个**参考姿态偏置** —— 每子步算世界系误差
    `e = (q_b⊗frame_b)⊗(q_a⊗frame_a)⁻¹`（`fixed_frame_error`，取**最短**路径），
    目标相对角速度 = `-0.2/dt · e`，按 `clamp_length` 夹在 20 rad/s 以内（与
    `angular_rhs` 的轴对齐偏置同源）。**两端参考姿态都是单位四元数时走原来那条表达式**
    （`-wrel`），老场景逐位不变 —— 由 `digest_tests` 与下面那条"写全 0 == 不写、逐位相同"一起钉住。
  - **睡眠**：参考姿态是**每步都在修**的约束，而睡眠体在求解器里是完全冻结的
    （`BodyView::from_body` 把 `inv_m` / `inv_I` 清零），睡着就会把修正停在半路。所以世界改用
    `Joint::drives_bodies()`（马达 **或** 参考姿态）当"这一步不许睡"的判据；缺省固定关节
    不在其列，老场景的睡眠行为不变。
  - **格式**（`scene_doc.rs`）：`JointDoc.frame_a` / `frame_b`（`Option<[f32;3]>`，欧拉角 XYZ、
    **度**，与 `rotation_deg` 同单位）。不写 = 老行为；只要有一端不是 0 就切到"扭到参考姿态"。
  - 校验（**全部复用已有稳定码**，不新增码表条目）：非有限 → `out_of_range`；写在非 `fixed`
    关节上 → `ineffective`；**两端都写 0**（与不写逐位等价）→ `ineffective`，消息点明"等于没写"。
  - 共用同一份换算：`runner::joint_frame_quat`（`pub` 纯函数，度 → 四元数）由 CLI 装配、
    格式校验、编辑器守卫三处共用；编辑器预览本来就调 `runner::build_world`，所以预览与
    `quench3d simulate` 不会再分叉。
  - agent 自描述三处同步：`scene_grammar()["joint"]`、`$defs.joint.properties`、系统提示词。
    本轮顺手补上了 `joint` 人读字段表里**一直缺的** `motor_max_impulse`，并加了守卫
    `joint_field_table_covers_every_joint_field`（人读表 ↔ `JointDoc` 字段集**逐项**对账）。
  - 测试（**全部无显卡**）：
    * 引擎 `crates/quench3d-physics/tests/fixed_reference_frame.rs`（4 个用例）：
      `frame_a = 90°` 的箱体 1 秒内误差 < 5°、3 秒 < 2°，且**不过冲**、锚点位移 < 0.05；
      两端**都是动态体**时也收敛（冲量两侧都作用）；被 3 rad/s 推歪后自己扭回来；
      缺省参考姿态时**保持**创建时的 90°（差 < 2°）。
    * CLI `crates/quench3d-cli/tests/joint_reference_frames.rs`：从**场景 JSON** 出发
      （`SceneDoc` → `runner::build_world` → 引擎）验证 `frame_a=[0,90,0]` 让门板停在绕 Y 90°
      （差 < 2°）、只写 `frame_b` 得到 -90°、缺省时保持创建姿态、**写全 0 与不写 120 步后
      位置/朝向逐位相同**、驱动中的两端跑 10 秒也不睡。
    * 编辑器 `crates/quench3d-editor/tests/joint_frames.rs`（3 条，**纯 CPU**，无显卡也真跑）：
      预览走 `runner::build_world`；编辑器源码里不许出现 `Joint::Fixed` / `local_frame_*` /
      `PhysicsWorld::new`（"两边各写一套"是静默分叉的起点）；共享换算的单位契约。
    * `scene_doc::tests::joint_reference_frames_parse_round_trip_and_diagnose`：解析 / 往返 /
      缺省不落盘 / 非有限 `out_of_range` / 错关节类型与"全 0"→ `ineffective` / 老场景不被吵醒。
  - **变异验证**（改坏 → 红 → 改回 → 绿，四条独立）：
    ① `frame_target_velocity` 去掉负号 → 3 个引擎用例红（"1 秒后误差 164.6°"）；
    ② runner 里把 `frame_a`/`frame_b` 对调 → CLI 3 条红（门板差 179.96°）；
    ③ `scene_grammar()["joint"]` 删掉 `frame_b` → `joint_field_table_covers_every_joint_field` 红；
    ④ 引擎里摘掉"两端都是单位四元数"的判断（永远按参考姿态算）→ 引擎"保持创建姿态"红
    （差 89.95°）与 CLI"缺省保持创建姿态"红。**注意**：④ 之下"写全 0 == 不写、逐位相同"
    **仍然绿** —— 它量的是两条写法彼此一致，不是老行为的数值。两条测试分工不同，别拿错尺子。
  - **没做**：关节的**可视化**（轴 / 锚点 / 参考姿态的 gizmo）仍然没有 —— 那要动渲染侧，
    本轮围栏外；编辑器没有关节编辑面板（所以本轮只加了"不许分叉"的结构守卫）；
    参考姿态只对 `fixed` 生效（`ball` 的 `cone_limit` 早就在求解器里生效，不是这一轮的事）。

- **关节的局部轴可以按端指定（`joints[].axis_a` / `axis_b`）** —— gap #15 的第二段。
  以前格式里只有一根 `axis`，装配时被**同时**喂给 `local_axis_a` 与 `local_axis_b`。这只在
  "两端刚体初始朝向相同"时才等于"同一个世界轴"：门框平放、门板转了 90° 时，世界 Y 在门框
  局部是 Y、在门板局部却是 Z。两端共用一根局部轴，求解器的角度约束会先把门板**扭平**
  再当铰链用（现象是"门装上去自己弹回原位"），而且没有任何诊断。
  - 格式：`JointDoc.axis_a` / `axis_b`（`Option<[f32;3]>`，**不写 = 用 `axis`**，所以老场景
    逐位不变）。只写一端也合法：另一端回落到 `axis`。
  - 校验（都用**已有**的稳定码，不新增码表条目）：`axis_a`/`axis_b` 非有限或零向量 →
    `out_of_range`（报在自己的 `path` 上）；写在 `fixed`/`ball`/`distance` 上（这三种没有轴）
    → `ineffective` 提醒；**两端初始朝向相差 >10° 却只写 `axis`** → `suspicious_value`
    警告并指路 `axis_a`/`axis_b`。最后一条是这一轮的守门人：它把"静默装错"变成一条可 grep 的诊断。
  - CLI / 编辑器**共用同一份映射**：`runner::build_world` 里按端取轴（`axis_a.unwrap_or(axis)`），
    编辑器预览走的就是这个函数，不会与 `quench3d render` 分叉；编辑器存盘不许丢这两个字段。
  - agent 自描述三处同步：`scene_grammar()["joint"]` 的字段说明、JSON Schema 的
    `$defs.joint.properties`、系统提示词里的关节行（漏一处 `json_schema_matches_the_source_structs` 就红）。
  - 测试（全部无显卡，真跑二进制）：
    * `scene_doc::tests::joint_local_axes_are_per_side_and_diagnosed`：解析 / 往返 /
      **缺省不落盘** / 零向量按端报错 / 给错关节类型提醒 / 朝向不同提醒 / 朝向相同**不**提醒。
    * e2e `hinge_local_axes_can_differ_per_side`：门板（`rotation_deg:[90,0,0]`、铰链边在原点、
      初始状态正好在约束流形上 = 绕世界 Y 90°/s 的圆周运动）。按端给
      `axis_a=[0,1,0]` / `axis_b=[0,0,1]` → 1 秒后转过 **80.4°**、门板薄轴与世界 Y 的夹角
      `|n·Y| = 0.0002`（门还是立着的门）；两端共用一根局部轴 → `|n·Y| = 1.0000`（门板被扭平），
      两种写法相差**三个数量级**。同一条用例还断言**只写 `axis`** 与"把 `axis` 显式抄到
      `axis_a`/`axis_b`"的 `states` **逐字节相同** —— "不写 = 老行为"的直接证据。
    * 编辑器 `scene_roundtrip::export_preserves_sections_the_editor_does_not_model`：
      `axis_a`/`axis_b` 经"载入 → 存盘 → 读回"仍在（编辑器没有关节面板，但更不许丢字段）。
  - **变异验证**（改坏 → 红 → 改回 → 绿，三条独立）：
    ① 铰链 B 端改回用 A 端的轴 → e2e 红（门板薄轴断言失败）；
    ② `$defs.joint` 删掉 `axis_a` → `json_schema_matches_the_source_structs` 红
    （"JSON Schema 的 joint 少了字段 `axis_a`"）；
    ③ 把朝向差阈值抬到 100000°（等于摘掉这条诊断）→ 单元测试红（"朝向不同必须提醒"）。
  - **没做**：`local_frame_a/b`（固定关节的"参考姿态"）引擎仍只保存不生效 —— 那要动
    `crates/quench3d-physics` 的角约束，不在本轮的接线范围；关节可视化（轴/锚点 gizmo）也没有。

- **校验诊断的稳定机器可读错误码（`diagnostics[].code`）**。以前
  `quench3d validate` 只给 `severity` + `path` + 中文 `message`，agent 想区分
  「模型路径不存在」和「字段非法」只能正则匹配中文文案 —— 文案一改，分支就烂。
  现在每条校验诊断都带一个**稳定**的 `code`（当时 17 个 —— **第 60 轮起 18 个**（新增 `include_cycle`）：`unknown_field`、`missing_field`、
  `invalid_type`、`invalid_value`、`out_of_range`、`unknown_reference`、`duplicate_name`、
  `reserved_name`、`cycle`、`asset_not_found`、`io_error`、`limit_exceeded`、
  `mutually_exclusive`、`ineffective`、`suspicious_value`、`unit_ambiguity`、`invalid`）。
  - **`code` 说性质、`path` 说位置、`message` 只给人读**：码表故意做得窄（一条诊断一个码
    既不稳定也没法穷举）。码是契约：**发布后不改名、不改含义，只新增**（新增算契约变更）。
  - 覆盖：`scene_doc.rs` 的 **232 个**（第 60 轮起 **252 个**）校验出错点全部显式给码；字段级问题（未知字段/类型不对/
    枚举拼错/缺字段）在**唯一出口** `check_at` 里由 `classify_serde_code` 分类，结构检查不可能漏码。
    两参数的老构造器保留，它给兜底码 `invalid`（兼容围栏外的调用点与老 JSON）。
  - **向后兼容**：老 JSON 里没有 `code` 的诊断仍能反序列化（`#[serde(default)]`，落到 `invalid`）；
    序列化时 `code` 一定在。
  - 人读输出同步：`validate` 的文本输出与错误信封都打成 `[错误][invalid_value] path: message`，
    `grep` 一行就能按码分流，不必先解析 JSON。
  - agent 自描述：`quench3d schema --json` 新增顶层 `error_codes` 段（由 `ErrorCode::TABLE`
    生成，不手写第二份），系统提示也写明"按 code 分支、别按文案"。
  - 文档：新增 [docs/error_codes.md](docs/error_codes.md)（码表 + 契约 + 怎么加新码 + 一条
    重算整表的命令）；README §6.4 补了分支表。
  - 测试（全部无显卡）：`error_code_table_is_self_consistent`、
    `error_codes_doc_matches_the_table`（与文档逐格对账）、
    `diagnostic_codes_are_stable_and_specific`（码表里 16 个非兜底码 —— 第 60 轮起 17 个 —— **各有真实触发场景**，
    钉死 `code` + `path`，并在末尾反向断言"覆盖集 == 码表 − `invalid`"）、
    `every_code_in_the_table_is_emitted_by_the_source`（码表里每个码在源码主体里至少有一个
    发出点，专门挡"加了码但没接线的死码"）、`serde_errors_classify_into_stable_codes`、
    `diagnostics_without_a_code_deserialize_to_invalid`、`schema_error_codes_match_the_table`、
    `system_prompt_mentions_diagnostic_codes`，以及 e2e
    `diagnostics_carry_stable_machine_readable_codes`（真跑二进制，含"每条诊断都有码"
    与"文本输出也带码"）。代码 ↔ `docs/error_codes.md` 的一一对应有两条可复核判据
    （`schema --json` 的码集 vs 文档表第一列，见 `docs/error_codes.md`）。
  - **变异验证**（故意改坏 → 测试必须红 → 改回来 → 绿）：改名 `asset_not_found` → 3 条独立
    守卫变红；误改 `models[0].path` 的码 → 稳定性测试变红（文档对账正确地保持绿）；
    手改文档一格 → 对账测试变红；停掉 unknown-field 分类分支 → 分类测试变红；
    摘掉 `ReservedName` 的唯一发出点 → 死码守卫变红；删掉 `suspicious_value` 的触发场景 →
    稳定性测试变红并报"缺场景的码 = [suspicious_value]"。
  - **没做**：顶层 `error.kind`（`scene`/`invalid`/…）与运行期 `RunError` 仍是粗粒度 ——
    `ErrorInfo` 定义在 `crates/quench3d-cli/src/report.rs`，不在该轮允许改动的文件围栏内。
- **点光源阴影（point shadow）：一盏点光源可以投影（立方体阴影）**。第 54 轮把聚光灯的
  阴影做完了，点光源仍然是"光穿透一切"—— 聚光灯解决"手电筒/舞台灯"，点光源解决"台灯/灯泡"：
  桌腿的影子该往四面八方铺开，而不是只在一个锥里。这一轮补上它。
  - 引擎：`PointLight` 新增 `cast_shadows: bool`（**缺省 `false`**：老场景打开后像素逐位不变）、
    链式构造器 `with_cast_shadows`。
  - **六个面存成 `texture_depth_2d_array` 的 6 层**（`+X, -X, +Y, -Y, +Z, -Z` = 层 0..5），
    每层一台 90° 透视相机。**不用硬件的 `texture_depth_cube`**：面序/u/v 镜像/±Y 朝向由图形 API
    规定，而本仓库的 CI **没有显卡**（naga 只查语法，写错符号只表现为"某个面里的影子镜像了"）。
    改用"把约定变成等式"：面选择 + 面内 uv 是纯函数 `point_shadow::cube_face_uv`，
    WGSL 里的 `cube_face_uv` **逐字镜像**它，然后有两类 CPU 测试钉住这条等式 ——
    (a) 2500+ 个方向 × 6 个面：公式给的 `(面, uv)` 必须等于面矩阵投出来的 `(面, uv)`；
    (b) 从 WGSL 源里抠出那 6 个分支的表达式逐个比对（含"不许写死 1.01 余量"）。
  - 面余量 `POINT_SHADOW_FACE_MARGIN = 1.01`（在 `tan` 空间上乘）：相邻面在边界处轻微重叠，
    3×3 PCF 的采样点因此仍落在本面图内；边界方向落在 `1/1.01`，有单元测试钉住。
  - **整帧只支持一盏投影点光源**（第一盏 `cast_shadows == true` 的点光源）；点光源与聚光灯是
    **两套独立资源**，所以"一盏投影点光源 + 一盏投影聚光灯"可以同时存在（各有各的警告）。
  - 深度用**主分量**（`d = max(|dx|,|dy|,|dz|)`）而不是欧氏距离：`z_ndc = far/(far-near) × (1 - near/d)`，
    六面共用一组 `near`/`far`/偏移常量（有测试断言同样垂直距离在三个面上给出同一条曲线）。
    `near = radius × 1%` 夹到 `[0.05, 0.5]`（与本模块矩阵对账的测试：`near = 0` 会让所有深度塌成 1）。
  - uniform：`FrameUniforms` 新增 `point_shadow_params`（`x` = 灯在 `point_lights` 里的**下标**，
    `x < 0` = 这一帧没有投影点光源）与 `point_shadow_texel`（`y` 是面余量的倒数），
    总大小 1312 → **1344**（单测钉死 `point_shadow_params` @1312、`point_shadow_texel` @1328、
    总大小 1344 —— WGSL 那边偏移错一位只是"影子偏一点"，很难查）；另有单测断言
    **一盏投影点光源 + 一盏投影聚光灯可以同时成立**（点光的槽位 0、聚光的打包下标 1，
    两套参数在结构体里各占各的 vec4）。
  - WGSL：`texture_depth_2d_array` + 数组重载 `textureSampleCompareLevel(tex, samp, uv, layer, depth)`
    —— 深度数组没有 mip，LOD 0 与隐式 LOD 等价；同时避开第 54 轮踩过的坑（非一致控制流里的
    隐式梯度采样在 D3D/FXC 上是硬报错）。
  - 渲染：阴影管线复用聚光灯那条（同一个 `Depth32Float`、同一条只写深度的管线、同一份
    "世界空间固定纹素数 × `dz/dd`"偏移），单面边长取聚光那张的**一半**（`shadow_map_size = 2048`
    → 每面 512），6 面合计 1.5 × 1024²；投射体用"球与包围盒相交"筛（点光源照亮的是一个球，
    没有单一视锥）。
  - 场景格式：`PointLightDoc.cast_shadows`（`Option<bool>`，不写 = `false`，老场景逐字节不变）；
    校验：>1 盏要求投影时警告并写明**实际用的是哪一盏**（`lighting.point_lights[i].cast_shadows`），
    排在 8 盏名额之外还要求投影时单独警告（那盏灯既不照明也不投影）。
  - 编辑器：每盏点光源一个"投影"勾选框（勾上一盏会自动取消其它点光源的勾选；
    与聚光灯互不影响），存盘只在 `true` 时写这个字段。
  - 文档：`schema.rs` 的 JSON Schema / 人读表 / agent 提示词三处同步（有测试断言 schema 与
    结构体字段一一对应）。
- 测试（CPU，不需要显卡）：`point_shadow.rs` 10 条（六面矩阵与 uv 公式的等式、45°/47° 边界、
  深度只与主分量有关、`near = 0` 塌成 1、近平面夹取、偏移是世界空间常数、挑灯规则与
  `pack_local_lights` 槽位一致、投射体球测）；渲染器 3 条（uniform 布局 / 负下标哨兵 /
  WGSL 漂移探测）；CLI 校验 1 条；编辑器往返 1 条。
- 本机 GPU 探针 `tools/probe_point_shadow.py`（20 张图 / 23 条判据，CI 没有显卡所以不进 CI）：
  关键结论 —— 无遮挡物时开投影**逐像素相同**（0/102400 个像素变，这条同时证明六面的
  near/far 与偏移是对的）；±X 两面镜像（偏移 +103.5 / −104.5 px，和 = −1.00 px，面积 7047 = 7047）；
  ±Z 两面把暗区换到画面中心的另一侧（y 260.3 vs 108.3）；前面多一盏不投影的点光源时
  暗区重心逐位相同（下标没错位）；点光 + 聚光同时投影时两张暗区各自都在。
- **逐面对账**（`examples/point_shadow_diag`，覆盖到探针照不到的 `+Y` 面）：
  不投影连渲两次逐位相同（基线 0）；打开投影后 26915/102400（26.28%）像素变化、12 次点光阴影绘制调用；
  五面最小深度 ≈ `normalized_depth(1.5) = 0.808081`，`±X/±Z` 四面统计量**逐位相同**
  （32847 个纹素、重心 (0.499, 0.802)），`+Y` 面**必须为空**（灯上方没东西）；
  `+X` 面里盒子背面 36 个点（深度跨 0.0631）与 CPU 对账：平均差 0.004596、极差 0.002206，
  残余就是阴影管线那条 `slope_scale = 2.0` 的多边形偏移。

- **聚光灯阴影（spot shadow）：一盏聚光灯可以投影**。以前只有太阳（平行光）会投影，
  聚光灯打出来的光是"穿透一切"的 —— 聚光灯是场景里最像"手电筒 / 舞台灯"的东西，
  没有影子时箱子底下、墙根都是亮的，一眼假。
  - 引擎：`SpotLight` 新增 `cast_shadows: bool`（**缺省 `false`**：老场景打开后像素逐位不变），
    链式构造器 `with_cast_shadows`。
  - **整帧只支持一盏投影聚光灯**（只有一张 2D 阴影图）。选灯规则：**第一盏
    `cast_shadows == true` 的聚光灯**；后面即使还有勾上的也不投影（校验会警告并写明用的是哪一盏）。
    点光源仍然不投影（那需要立方体阴影图，这一轮没做）。
  - 阴影相机是**纯函数** `quench3d_render::spot_shadow::spot_shadow_camera`：
    透视投影，`fov_y = 2 × 外锥角`（再乘 1.01 的余量，在 tan 空间做）、
    近平面 = 半径 × 1% 并夹到 `[0.05, 0.5]`、远平面 = `radius`。
    近平面**不能是 0**：`perspective_rh` 会退化（矩阵不可逆），而且 `clip.z` 与 `clip.w`
    都等于视空间深度，`z_ndc ≡ 1`，深度图里一个比特的信息都没有 —— 这条有单元测试
    （`near_zero_destroys_every_depth`）钉住。
  - uniform：`FrameUniforms` 新增 `spot_shadow_matrix` / `spot_shadow_params` / `spot_shadow_texel`
    （`x < 0` = 这一帧没有投影聚光灯），`common.wgsl` 的 `struct Frame` 同步；
    布局测试断言了三个字段的字节偏移与总大小（1312）。
  - WGSL：`pbr.wgsl` 的聚光分支里，只有"这一盏就是投影灯"才采样；
    深度偏移 / 法线偏移全部由 uniform 传进来（不在着色器里写死数字），
    3x3 PCF 与太阳**共用同一份** `pcf_3x3`（贴图作为参数传入），采样用
    `textureSampleCompareLevel`：D3D/FXC 不允许非一致控制流里出现带隐式梯度的采样
    （深度图只有 1 级 mip，LOD 0 与隐式 LOD 结果完全相同）。
  - 场景格式：`SpotLightDoc.cast_shadows`（`Option<bool>`，不写 = `false`）；
    校验：>1 盏要求投影时警告并写明**实际用的是哪一盏**（`lighting.spot_lights[i].cast_shadows`），
    被 8 盏局部光名额挤掉时单独警告（那盏灯既不照明也不投影）。
  - 编辑器：每盏聚光灯一个"投影"勾选框（勾上一盏会**自动取消**其它盏的勾选，
    hover 说明只支持一盏），存盘只在 `true` 时写这个字段（不写 = 老行为，文件保持干净）。
- 测试（CPU，不需要显卡）：阴影相机的不变量（轴心点落 NDC 中心、锥边界落 `1/1.01`、
  超出 `radius` 落远平面外、`near = 0` 时所有深度塌成 1、偏移折算到世界空间后与距离无关）；
  挑灯规则与 `pack_local_lights` 的槽位顺序**逐位对齐**（不一致 = 影子长在别的灯下面）；
  文档 → 引擎的逐字段映射；多盏投影 / 名额不足的校验诊断；编辑器存盘的往返。
- 本机 GPU 探针 `tools/probe_spot_shadow.py`（10 张图 / 14 条断言，CI 没有显卡所以不进 CI）：
  关键结论 —— 无遮挡物时开投影**逐像素相同**（0/102400 个像素变，这条同时证明阴影相机与偏移是对的，
  偏移不够时地面会投出满屏自阴影）；有遮挡物时 3479 个像素变暗（3.40%，最大变暗 65 灰阶，
  且**没有**任何像素变亮）；暗区重心在遮挡物**朝光的一侧** 35.7 px、镜像光源后严格变成 -35.7 px；
  在聚光灯前面插一盏点光源（占掉第 0 个槽位）后暗区重心与面积不变（195.2 / 3470 px → 195.2 / 3469 px），
  证明"点光源优先占槽位"这条下标规则在 pack → uniform → 着色器整条链路上是对的。
- **明确没做**：点光源投影（要立方体阴影图）、多盏投影聚光灯（要阴影图数组）、
  级联 / 多层聚光阴影、半透明物体的阴影。编辑器那套勾选框是按**代码与 CPU 测试**验收的，
  这一轮没有人在真实窗口里点过它。

- **编辑器：局部光（点光源 / 聚光灯）的增删改**。以前点光与聚光在编辑器里只能靠手改 JSON
  （存盘时"沿用底稿"）。现在"光照"里有 `局部光 N / 8 盏` 的列表：位置 / 颜色 / 强度 / 半径，
  聚光灯多一组 方向 + 内外锥角；每盏有"删"，底部有"加点光 / 加聚光"。
  - 锥角控件把内锥夹到不大于外锥，与 CLI 校验同一条规则（UI 不该造出引擎不认的锥）。
  - 存盘从**引擎**写回（`point_light_docs` / `spot_light_docs` 两个纯函数）：
    删光所有灯写"不写这一节"而不是 `[]` —— 对点光源来说"没写"（保留预设补光）与
    "写了 `[]`"（显式清空）是两种意思，删几盏 UI 里的灯不该顺手清掉预设补光。
  - 角度是**从余弦反解**的（引擎只存余弦），所以存回文件的角度可能与作者原始小数值差 1e-5，
    语义完全一样；这条写进了函数注释。
- 测试（CPU，不需要显卡）：`local_lights_round_trip_through_the_scene_doc`
  （空 = 不写这一节；位置/颜色/强度/半径一个不丢；角度→余弦→角度回到原值；
  方向 (0,-2,0) 这种会被归一化）。

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

### 修复

- **glTF 的节点变换被整段忽略：多节点模型的所有部件堆在原点**（第 61 轮发现）。
  加载器原来直接遍历 `doc.meshes()`，**完全不看 `doc.nodes()`** —— 节点上的
  平移/旋转/缩放全被丢掉，同一个网格被多个节点引用时也只画一次。
  单网格资产（DamagedHelmet 之类）完全看不出来，两个轮子的车、一栋由多块拼起来的房子
  会立刻变形。
  - 现在按节点走：从 `scenes` 的根往下累乘世界变换（glTF 没提供 `parent()`，只能自己建
    父子链），把位置按点、法线/切线按方向变换后烘进顶点；每份实例的名字带上节点名
    （`右/轮`、`左/轮`）。
  - 兜底：没被任何节点引用的网格仍按单位变换输出（坏文件不该"以前能看见、现在消失"）；
    节点递归有深度上限 64（防御坏文件里的环 —— 递归爆栈不可恢复）。
  - 测试 `gltf_node_transforms_are_baked_into_geometry` 手搓 4 个节点的 GLB（平移、绕 X
    转 90°、父子累乘、同一网格 4 份实例），断言顶点与法线都跟着走。变异三组全红
    （不烘 / 父子不累乘 / 位置不做平移）。
  - **边界**：非均匀缩放下法线用的是"方向变换 + 归一化"而不是逆转置矩阵，高光角度会略偏
    （换来的是一条不求逆、不会因奇异矩阵出 NaN 的路径）。
  - 这也是 gap #23（动画/蒙皮）的**前置**：动画就是对同一批节点改变换；设计草图见
    `docs/production_readiness.md` §19。

- **`physics.speculative_margin` 从来没被读到，而 `static_friction_speed` 会顺手把它设上**
  （第 61 轮发现，闭合 gap #20 的两条欠账）。`build_world` 里原本是：
  ```
  if let Some(v) = doc.physics.static_friction_speed {
      world.config.static_friction_speed = v;
      world.config.speculative_margin = v;   // ← 这两行黏在一起
  }
  ```
  后果有两个，都**静默**：只调了摩擦阈值的场景被悄悄加了 1:1 的接触余量（物理结果变了、
  没有任何诊断），而真正写 `physics.speculative_margin` 的场景**毫无效果**。
  - 现在两个字段各归各的；并且新增 `world_config_wiring_is_field_for_field`：把
    `friction_combine` / `restitution_combine` / `static_friction_speed` / `speculative_margin`
    四个字段逐一对到 `SolverConfig` 的同名字段上，外加"只写 A 时 B 必须保持默认"的三条。
    变异（把旧代码原样放回 / 或删掉余量的接线）都会红。
  - 为什么以前没抓到：单测只验"配置读到了"、没有一条 e2e 观察**物理结果**，而
    `speculative_margin` 缺省是 0（默认路径的 digest 测试当然是绿的）。见
    `docs/overnight_report.md` §59。

- **Wayland 上关闭编辑器窗口必崩（SIGSEGV / 退出码 139 / core dumped）**（用户实测，本机真显卡 + Wayland）。
  进程在正常跑、第一帧也正常，**用户点窗口关闭按钮的那一刻**收到 `SIGSEGV`；回溯固定落在
  `smithay-clipboard` 的 worker 线程上（`#9 smithay_clipboard::worker::worker_impl
  src/worker.rs:101` → `#5 ZwpPrimarySelectionDeviceManagerV1::destroy` →
  `#1 wl_proxy_destroy` → `libwayland-client`）。与 GPU、场景内容无关（软件 Vulkan 下同样复现）。
  - 根因是**析构顺序**，不是剪贴板本身：`winit::EventLoop::run_app(self, app)` 是**按值吃掉**
    EventLoop 的，于是 winit 自己的 Wayland 连接在 `run_app` **内部**就析构了；而调用点上
    那个 `EditorApp` 是**临时值**，要到整条语句结束才析构 —— **晚于**连接。`egui-winit` 只在
    Wayland 下才建 `smithay-clipboard`，它包的是 winit 的裸 `wl_display*`（不持有所有权），
    worker 线程析构主选区管理器时那个 `wl_display` 已经没了 → 释放后使用。
  - 修法：在 `ApplicationHandler::exiting()` 里把 `Inner`（窗口 / GPU / 剪贴板）按序释放 ——
    这是 winit 唯一保证"事件循环即将退出、但连接仍然有效"的钩子。**只在 Wayland 触发**
    （判据 = `RawDisplayHandle::Wayland`，正是 `egui-winit` 造那个对象的同一判据）。
  - **没有**用"捕获 / 忽略 SIGSEGV"这类掩盖手段，也没有跳过析构。
  - 新增**环境变量门控、默认关闭**的测试钩子 `QUENCH3D_EDITOR_EXIT_AFTER_MS=<毫秒>`：
    窗口起来后再等 N 毫秒就投一个**真的 `WindowEvent::CloseRequested`**（= 用户点关闭按钮
    的那条 match 分支），让这条"只有真跑 GUI 才会经过"的路径能被无人值守复现。
    不设 / `=0` / 非法值一律**不自动退出**。
  - 验证：同一配方，修复前 `EXIT_CODE=139` + gdb `Thread 20 "smithay-clipboa" received signal
    SIGSEGV`；修复后 `EXIT_CODE=0` + gdb `exited normally`、`SIGSEGV` 出现 0 次（Wayland 复跑 4/4 全 0）。
    解析逻辑有 6 条**不需要显卡**的守卫测试，两条变异（`0` 不再等于关闭 / 默认不再关闭）实测**红**，
    还原后**绿**。配方、回溯、取舍与边界见
    [docs/production_readiness.md](docs/production_readiness.md) §15 与
    [docs/overnight_report.md](docs/overnight_report.md) §56。
  - **顺带核实**：X11/XWayland 下关窗口**本来就**会 panic（`winit-0.30.13` X11 的
    `BadWindow`，`event_processor.rs:1324`），基线 7 次跑出 101×6 / 134×1 —— 那是**另一个**
    上游缺陷，本次**没有**修，也特意不让本次改动去扰动它。

- **GL/WebGL2 上"建不出来的管线"：编辑器启动即 panic**（用户实测）。浏览器控制台原文：
  `Shader translation error for stage ShaderStages(FRAGMENT): WGSL \`textureLoad\` from depth textures is not supported in GLSL`
  → `panicked at ... In Device::create_render_pipeline, label = 'quench3d-post'`。
  后端是 `gl`（`navigator.gpu` 存在但没有适配器时的 WebGL2 回退，限制档位 `downlevel_webgl2`）。
  wasm 上 wgpu 把管线校验错误当**致命**，所以编辑器还没画第一帧就死了。
  - 根因：`post.wgsl` 的 SSAO 用 `textureLoad(depth_texture, coord, 0)` 按像素读深度。
    naga 的 **GLSL 后端**把 `texture_depth_2d` 一律映射成 `sampler2DShadow`：`textureLoad`
    直接报错；非比较采样连 naga 的 WGSL 校验都过不去（`InvalidSampleLevelExactType`）；
    比较采样（`textureSampleCompareLevel`）拿不到深度值 ——
    **在 GL 上"把深度当纹理读"这条路是断的**，不是能力位。
  - 修法（能力判定 + 着色器变体）：新增 `context::supports_depth_texture_read(backend)`
    （只有 `Gl` 为 `false`）。GL 上：① `post.wgsl` 里 `//@gl-depth-begin` /
    `//@gl-depth-end` 圈出的**三段**（深度绑定、`ssao_factor` 一族函数、`fs_main` 的调用点）
    在**交给驱动之前**被整段删掉，tonemap / bloom / FXAA 一行不动；② `quench3d-depth-resolve`
    （读多采样深度）这条管线根本不建；③ 新增 `Renderer::supports_ssao()` 报 `false`，
    `options.ssao.enabled` 按能力忽略（打一条警告），`ensure_targets` 因此也不建解析用的深度目标。
    三者必须同时成立：否则要么管线建不出来、要么着色器读了没绑的资源。
    **"运行时把开关置 0"救不了场** —— 翻译发生在建管线时，只要 WGSL 里还留着那段代码，
    管线就建不出来，所以必须"删源码"。
  - 本地判据（本机默认跑 Vulkan，抓不到这个 bug）：`WGPU_BACKEND=gl`（Mesa llvmpipe）能在原生 GL
    上复现**同一条消息、同一个 label**：
    `WGPU_BACKEND=gl cargo run -p quench3d-render --example offline_render`
    —— 修前 `panicked at ... label = 'quench3d-post'`（exit 101），修后同一命令 exit 0 并出图。
  - 无显卡守卫（**只在有 GL 后端的平台上**编：windows / linux / android / freebsd / emscripten
    —— `naga/glsl-out` 跟着 wgpu 的 gles 后端走，macOS 上 wgpu 走 Metal、连 GL 后端都不编，
    那里靠不依赖 naga 的文本级守卫 `post_gl_variant_drops_every_depth_texture` 兜底）：
    `renderer::tests::gl_pipelines_all_translate_through_the_glsl_backend` 用
    **naga 的 GLSL 后端**把"GL 上会建的每一条管线"的**每个入口点**真翻一遍（WebGL2 GLSL ES 3.00
    与桌面 3.30 两档）；另有两条反向守卫 `full_post_wgsl_is_rejected_by_the_glsl_backend` /
    `depth_resolve_wgsl_is_rejected_by_the_glsl_backend` 钉死"原文必然被拒"（哪天 naga 支持了，
    它们会红，那时可以删掉整个变体机制）。**变异验证**：把 `post_wgsl_for` 的 GL 分支改成
    "返回原文" → 守卫变红并打印 naga 原文；改回 → 绿。
  - `crates/quench3d-render` 全部测试在 `WGPU_BACKEND=gl` 下 **82 passed / 0 failed**
    （修前：任何建 `Renderer` 的测试都在 `quench3d-post` 上 panic）。
  - **真机验证（真实 WebGL2）**：新增 `tools/probe_webgl2.sh`（headless Chrome + SwiftShader，
    `?backend=gl`），判据是日志里出现「编辑器第一帧已渲染」且页面遮罩被收掉。
    修前：真实 WebGL2 里**复现出用户那条 panic**（遮罩停在「正在创建渲染面…」）；
    修后：`编辑器第一帧已渲染：1280x800`、遮罩收起、0 次 panic，探针 PASS。
  - **顺带修掉第二条"GL 上必死"的路（同一根因家族：GL 后端不支持的能力）**：MSAA。
    上面的 panic 修掉之后，同一个真实 WebGL2 环境里紧接着死在
    `glow-0.17/src/web_sys.rs: panic!("Tex storage 2D multisample is not supported")`
    —— wgpu-hal 的 gles 后端对 `sample_count > 1` 的纹理就调这个函数，
    而它在 WebGL2 那条路里是 `panic!`（与浏览器/显卡无关）；原生 GL 上不 panic，
    但解析出来**全黑**。编辑器缺省 `render.msaa = 4`，所以它**在任何浏览器上都会踩到**。
    现在 `context::effective_msaa_samples(backend, requested)` 在 GL 上一律给 1
    （唯一调用点是 `GpuContext::build`，并打一条 `log::warn!`：用户能知道为什么 MSAA 没生效），
    于是多采样纹理根本不建。判据：GL 上 `F3D_MSAA=4` 与 `F3D_MSAA=1` 的输出**逐字节相同**、
    非黑像素 100%、整幅均值 `[156.41,165.44,179.80]` 对 Vulkan 的 `[156.48,165.53,179.84]`
    （差 < 0.1/255）；真实 WebGL2 里第一帧正常画出来。
  - **没做 / 边界**：GL 上 SSAO 与 MSAA 都不是"修好了"，是**按能力降级**
    （SSAO 整段不参与；MSAA 夹到 1）。真实 WebGL2 那一层跑在 **SwiftShader（软件光栅）**上，
    不是用户的 ANGLE/Intel 真机 —— 能确定的是"管线建得出来、第一帧画得出来"，
    真机上的最终确认只能由用户刷新页面给出。
    另一处**没查**的：原生 GL 上 MSAA 解析出黑帧的根因（在 wgpu-hal 里，不在围栏内；
    现在这条路径在项目里再也走不到）。详见
    [docs/production_readiness.md](docs/production_readiness.md) §12.7。
- **默认场景与载入路径的光照不对称**：编辑器默认场景以前吃 `Lighting::default()`，而
  载入/撤销路径走的是 CLI 的 `apply_lighting`（没写 `preset` = `Lighting::studio()`，带两盏补光）。
  两条路径的默认值不同 —— 存盘后重新载入（或撤销一次）会**凭空多出那两盏点光源**
  （`[-4,5,4]` 暖色 12/18 与 `[5,4,-3]` 冷色 9/20）。
  现在 `Doc::build_default_scene` 用一个空文档调**同一个** `apply_lighting`
  （该函数改成 `pub`，注释写明"编辑器共用，避免预览/载入分叉"），两条路径落在同一套默认值上。
  - 以前看不见的原因：`scene_to_json` 对局部光"沿用底稿"，把这个不对称藏住了；
    第 53 轮改成"从引擎写回"（UI 改的灯必须能存盘）之后才暴露。
  - 回归测试：`default_scene_survives_save_and_reload_byte_for_byte` —— 默认场景
    存盘 → 载入 → 再存盘，两次 JSON **逐字节相同**，并转两圈确认不是"第一次凑巧"。
  - 验证：`cargo test -p quench3d-editor --test history`（7 passed，含此前失败的
    `undo_restores_the_exact_serialized_scene`）。
- **顺带修掉两处不健全的断言**（`scene_roundtrip::model_transform_edits_are_exported`）：
  它用"整篇 JSON 的子串匹配"来判断模型变换有没有被写出去，而预设补光的位置里就有 `-3.0`，
  `contains("3.0")` 于是**误报**（正向检查更糟：模型位置真丢了它也会通过）。改成解析成
  场景类型后对字段断言 —— 更严也更准。

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
