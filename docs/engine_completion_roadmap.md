# quench3d 行业级全功能：里程碑与验收口径

> 这份文件回答一个问题：**“整个引擎开发完成”到底以什么为终点。**
> 结论先写：按行业对标（Unity/Godot/Filament/PhysX 的完整层）不是一次会话、
> 也不是一个人月级的工作；当前仓库是一个成熟的自研内核，但离“全功能引擎”还差
> 若干整层。下面的里程碑每一项都必须有测试、门禁与可复现数字，不能靠文档宣称。

## 0. 当前基线（M0）

- 门禁：`CARGO_NET_OFFLINE=true bash tools/gate.sh`
- 测试：**1024 通过 / 0 失败 / 5 ignored**
- wasm：**13,879,807 字节**
- 已有：自研渲染（PBR/阴影/CSM/点光聚光阴影/后处理）、自研物理（岛并行/休眠/
  确定性/关节/复合形状/流体/水面/粒子）、CLI、MCP、桌面与 Web 编辑器、
  glTF/GLB/OBJ/STL、动画/蒙皮/morph/状态机/两骨 IK。
- 已有边界：`docs/gap_analysis.md`、`README.md` §9、`docs/robot_arm_sim.md` §13。

## 1. 完成定义

“行业级全功能”按下面六层验收；**每层全部条目有自动化证据才算该层完成**。
不以“代码写了”“文档写了”“测试绿了”替代行为证据。

### M1 资产与场景基础（3–4 人月）

- [x] 逐资源释放：网格/材质/贴图按句柄释放，句柄空间可复用；反复 load/remove 不涨。
  - 底层：`ResourceTable` 低 20 位下标 + 高 12 位代次；`remove_mesh/remove_material/remove_texture`；
    `mesh_count/material_count/texture_count` 只数活句柄；两条单测覆盖复用、旧句柄失效、重复释放、清空后不复活。
  - 场景接入：`AssetCache::release_mesh/release_material` + `Engine::remove_object`；编辑器删除物体
    在无其他引用时释放命名网格/材质。材质按 `MaterialDesc` 记录去重贴图引用，最后一个材质释放时
    才 `remove_texture`。
  - 行为证据（GPU e2e）：16 轮直接 add/remove、8 轮加物体再删、两个材质共用贴图删到最后一个，
    `resource_counts` 都回到基线，旧句柄失效。
  - 边界：模型文件级卸载/热重载没有单独入口（删光它的场景物体即可触发）；`set_material_texture`
    换贴图不更新引用表；LOD 槽位释放后留空，不复用。
- [x] 场景迁移器：旧 `version` 一键升级、备份、失败不破坏原文件。
  - `quench3d migrate <scene> [--out]`：复用 `migrate_value`；先校验，错误时一个字节都不写；
    就地升级前复制 `.bak`（已存在则 `.bak.N`）；同目录临时文件 + rename 原子替换。
  - 行为证据（CLI e2e）：v1（带 `parent`）升级到 v2，备份逐字节等于原文，升级文件 version=2 且
    `parent` 被删；坏 JSON 退出码非 0、原文件不变、不留备份；`--out` 不改源文件。
- [x] 运行时快照/恢复：刚体、关节、速度、睡眠、随机状态逐位可恢复。
  - `PhysicsWorld::snapshot/restore` 保存刚体槽位/世代、完整 `RigidBody`（位姿、线/角速度、
    睡眠/计时、伪速度、外力）、关节槽位与热启动冲量、接触热启动缓存、水域/粒子流体、
    世界累加器、`step_count`、事件队列和上一子步差分表。`Engine::snapshot/restore` 额外
    同步场景、清空引擎累加器。
  - 行为证据（物理单测）：下落 + 铰链伺服场景推进 120 步取快照，再推进 45 步后恢复，
    刚体 `RigidBody` 逐位相等；从同一快照再跑 30 步，两次结果逐位一致（证明热启动缓存
    也在快照里，不只是位姿）；同一快照可重复恢复。
  - 边界：物理没有 RNG（无随机状态可存）；恢复会丢弃查询缓存并递增 `version`，未保存
    `scratch_*`/`manifolds`（下一步重建）。
- [x] 输入抽象：键鼠/手柄/触屏/重绑定，宿主可注入。
  - `ActionBinding`（键盘/鼠标/手柄按键/手柄轴）+ `bind_action/rebind_action/clear_action`；
    `action_down/pressed/released` 统一处理键鼠与手柄边界（手柄保存上一帧状态）。
  - 触屏：4 槽手柄注入、`TouchPoint` 生命周期、`pinch_delta`/`swipe_delta`；winit `Touch`
    已接主循环。
  - 行为证据（`Input` 单测）：重绑定后旧键失效/新键生效、按住不算 pressed、手柄
    button/axis 的 pressed/released、双指捏合 5px 与平均滑动 2.5px、单指不产生捏合。
  - 边界：手柄由宿主注入（未接 gilrs/SDL 等平台后端）；移动端 UI 不在引擎层。
- [x] 统一错误码覆盖运行期路径（不只 `validate`）。
  - `ErrorInfo` 新增稳定 `code`；CLI `error.code`、MCP `structuredContent.code` 与
    `Envelope` 共用 `report::runtime_code(kind)` 映射（scene_runtime / environment_unavailable /
    output_exists / golden_mismatch / unknown_camera / io_error / network_error / invalid）。
  - 未登记类别收口到 `runtime_error`，不把内部字符串漏给 agent。
  - 行为证据：`report` 单测逐类对账 + `RunError::scene/output_exists` 序列化；CLI e2e
    缺文件得到 `invalid`、`--no-clobber` 得到退出码 2 且 `kind/code` 都是 `output_exists`；
    MCP 参数错误的结构化结果同时带 `kind=invalid` 与 `code=invalid`。

### M2 渲染架构（8–12 人月）

- [x] Render graph / Pass 扩展点，自定义后处理可插入并出图。
  - 按条目里的“Render graph / Pass 扩展点”取 **Pass 扩展点**：有序自定义后处理链，
    不是通用 DAG。
  - 已做（前两片）：`CustomPostPass` + `Renderer::set_custom_post(s)`，固定插在
    `tonemap → 自定义链 → FXAA`；宿主只给 WGSL 片段体，绑定/全屏三角形/uv 由引擎生成。
    多条 pass 按数组顺序执行，相邻 pass 在两张 HDR 目标之间 ping-pong；最后一趟在未开
    FXAA 时直接写最终目标，开了 FXAA 则写中间目标再由 FXAA 读（奇数/偶数链都覆盖）。
    每条 pass 还可读单采样场景深度（MSAA 时自动解析；不支持读写深度的后端绑 1×1 占位）。
  - 行为证据（GPU 像素 e2e）：无 FXAA 与开 FXAA 两条路径都验证“tonemap 后乘 0.5”与
    手算一致；两条 pass 的 swap→halve 与 halve→swap 像素不同且各自等于手算，删到只剩
    一条只保留它自己的效果；偶数链 + FXAA 也验证最终缓冲没接错；移除后逐位回到基线。
    深度：立方体覆盖区 `< 1` / 背景 `= 1` 被片段用来输出红/蓝，MSAA=4 也验证走的是解析后
    的单采样深度。
  - 边界：没有通用 pass 依赖图/资源别名，也没有法线/运动向量输入；自定义着色器不能
    声明额外资源。这些属于后续“完整 render graph”，不在本项的 Pass 扩展点范围内。
- [ ] GPU 剔除 + 间接绘制；数千物体 60 fps 的实测。
  - 已做（第一片）：`RendererOptions.gpu_culling`（缺省关）。compute 按批次剔除不透明
    实例、压实到第二份实例缓冲，主 pass 用 `draw_indexed_indirect`；透明与阴影仍走
    CPU 路径。设备 storage binding 上限 4，所以输出实例与间接参数共用一块缓冲。
    没有 compute 的后端（GL/WebGL2）自动退回 CPU。
  - 行为证据（GPU e2e）：同一批 4 个实例（2 个在相机背后），GPU 输入 4 个、间接参数
    `instance_count=2`；整幅图与 CPU 剔除路径逐像素一致。间接参数回读接口同时作为
    “compute 真的跑了”的证据。
  - 性能实测（本机）：2000 实例、320×240、60 帧提交后统一等 GPU，llvmpipe 软件
    适配器 **21.7–24.3 fps**（42–46 ms/帧），CPU 剔除 **23.7–23.8 fps**。本机
    `/dev/dri` 不存在，只有 llvmpipe，**无法诚实证明真实 GPU 上 60 fps**；不把软件
    光栅化的数字当行业验收。
  - 未做：真实 GPU 60 fps 实测、GPU 侧透明/阴影剔除、遮挡剔除。
- [ ] 遮挡剔除 / 自动 LOD / 多视口。
- [ ] 压缩纹理（KTX2/Basis）与浮点纹理；EXR/HDR 读写。
  - [x] KTX2 容器导入：未压缩 RGBA8 + BC1（CPU 解成 RGBA8，level 0）。
  - [ ] BasisLZ/UASTC/BC7、GPU 块压缩上传、完整 mip/face/layer。
  - 已做：`FloatImage`（线性 RGBA f32）读写 Radiance `.hdr` 与 OpenEXR
    （scanline、HALF/FLOAT/UINT、NONE/RLE 解压；写盘 HALF 无压缩）。HDR 用
    平铺 RGBE 编码；EXR 的 RLE 含预测器逆变换与奇偶字节反交错。`EnvironmentMap`
    现在也能读 `.exr` 并导出 `.hdr`/`.exr`。
  - 已做：`Rgba16Float` 浮点贴图上传（`Renderer::add_texture_float`，盒式 mip），
    GPU e2e 验证线性亮度 4.0 的贴图不会被 8 位路径截断。
  - 部分完成 KTX2（RGBA8/BC1 CPU 解码）；BasisLZ/UASTC/BC7、GPU 块压缩、
    PIZ/ZIP/tiled EXR 未做。
- [ ] FBX/USD 至少一条导入链；材质节点图或等价的自定义着色器层。
- [x] 透明排序/OIT、投影贴花、多盏投影阴影、线性雾距离。
  - 已做：线性雾 `FogParams::with_linear(start, end)` / 场景 `lighting.fog.mode = "linear"`
    + `start`/`end`。起点之前逐位无雾、终点处完全雾色，GPU 像素 e2e 验证；旧指数雾与
    “不写雾 = 关闭”语义不变。
  - 已做：盒投影贴花 `Renderer::draw_decal` + `DecalDesc`。全屏 pass 用场景深度重建
    世界坐标，投进贴花局部盒 `[-0.5, 0.5]^3`，因此贴在地面/斜坡等已有深度上，不是
    浮空四边形。GPU 像素 e2e：地面中心变红、盒外逐位不变、移除后逐位回基线；
    MSAA=4 走解析后的单采样深度。后端读不了深度时不执行。
  - 已做：多盏投影阴影。聚光灯阴影图改成 **4 层深度数组**，`plan_spot_shadows`
    按数组顺序选最多 4 盏；每层独立 pass/帧常量，shader 按 `spot_shadow_params[layer].x`
    的 packed 下标匹配。GPU e2e：两盏投影聚光灯各压暗自己下方，关掉第二盏后右半边
    像素变化，且两层深度图都真有遮挡物；越界层回读报错。
  - 已做：加权混合 OIT（`RendererOptions.oit`，缺省关闭保证旧场景逐位不变）。
    非水面半透明走 `fs_oit`：累积色 + 透射率两张单采样目标，独立混合，最后一个
    全屏 pass 合成到已解析 HDR；与绘制顺序无关。GPU e2e：同深度红/蓝半透明交换
    绘制顺序后中心像素差 ≤4，关掉 OIT 后同样交换会得到不同像素；MSAA=4 走解析后
    的单采样深度。边界：GL/WebGL2 没有独立混合 → 自动退回旧 alpha 管线；水面材质
    仍走旧管线写 mask；没有逐像素深度剥离。
  - 未做：无（本项全部完成；上述边界已写明）。

### M3 动画、音频与内容（6–9 人月）

- [ ] 骨骼动画编辑器：片段选择、速度、起始帧、事件编辑、根运动写回。
  - [x] 片段选择、倍速/循环写回、循环子区间起点（`loop_start`，编辑器
    「循环起点」）；事件/根运动仍主要靠场景文件。
- [ ] 全 GPU morph（含法线/切线）与任意权重曲线。
- [x] 音频子系统：WAV 解码、混音、3D 衰减/声像、无设备离线测试
  （`quench3d-audio`，`Engine::audio`；真实设备输出仍由宿主适配）。
- [ ] 脚本层：宿主可注册回调，场景生命周期钩子不再只能轮询。
  - [x] 场景生命周期回调 `SceneHooks`（加入/删除/清空，缺省不装零开销；
    回调只读）。不是脚本语言/热重载。回归 `scene_hooks_fire_for_add_remove_and_clear`。

### M4 物理精度与规模（8–12 人月）

- [x] Rust opt-in 全局投影法向求解：默认关闭，旧路径 1240 状态／931616648 字节兼容验证；
  4/12/32 层分别 1800 步独立 OBB 穿透与有限地板检查通过。固定验收配置为
  60 Hz、接触余量 0.005 m、20 次速度及 8 次位置迭代；不是任意场景精度保证。
  固定五轮成本比值 12 层 1.046809、32 层 0.333319，均不超过 1.15；
  持续精度、默认黄金清单及成本已接入门禁，完整门禁 1310 项通过，主分支产物验证通过。
  [源码与产物验收](experiments/global_normal_final_gate.md)。尚未接入场景 JSON／编辑器配置。
- [x] 4000 级流体性能：稠密网格邻域（与旧哈希顺序逐位一致，回归
  `fluid_grid_matches_the_hash_neighbour_order`）+ native 密度/lambda 区间并行；
  4913 颗粒子 release **18.7 → 14.4 ms/步**（显式 ignored 基准）。wasm 仍单线程。
- [x] 流体连续自由表面：Surface Nets 从 PBF 粒子提取三角形网格（`fluids[].surface_mesh`，
  分辨率 8~96），`Renderer::update_mesh_data` 支持拓扑整体替换；GPU e2e
  （`fluid_surface_replaces_the_placeholder_mesh`）与提取单测 4 条。
- [x] 流体对静态三角网碰撞：逐三角形最近点 + 扫掠薄壳检测
  （`particles_stop_on_a_static_trimesh_floor`）。
- [x] 流体对凸包/圆锥碰撞：凸包 GJK/EPA、圆锥底面圆盘 + 侧面线段投影，
  都是精确最近表面（不再按 AABB 近似）；四面体与圆锥 AABB 对照回归。
- [x] 流体表面张力/涡量约束（0~5，缺省关闭）；示例 240 步最高点 11.4 m → 1.65 m。
- [x] 解析水域水流（`waters[].flow`，XZ m/s，缺省 0）：浸没刚体被拖向流速，
  垂直速度仍走原阻尼；0 时旧路径逐位不变。回归
  `water_current_drags_a_submerged_body_along_the_flow`（静水无水平速度；1.5 m/s
  水流把球拖向 +X 且无垂直速度）与 CLI `waters_parse_validate_and_roundtrip`。
  这不是 PBF 粒子流场/波浪。编辑器「渲染 → 水域物理」逐块调 flow/水面/密度/阻尼，
  也可新增/删除水域（删除同步移除底稿；空白场景新增后存盘由 `sync_water_docs`
  从世界补全）；写回路径回归
  `water_edit_writes_flow_and_size_into_scene_doc`、
  `empty_scene_doc_gets_world_waters_on_save`，删除回归
  `remove_water_removes_by_index_and_shifts_later_entries`；NaN 水流由
  `nan_water_current_is_rejected` 与 `set_water_validates_and_replaces_by_index` 拦住。
- [x] 流体初始速度 + 开放排水面（`velocity` / `drain_y`，缺省不变）。
- [x] 流体连续发射器（`emit_*`，黄金角确定撒点、上限/排空/CLI 接线）。
- [x] 流体泡沫/飞沫（`fluids[].foam`，缺省关闭）：高速度粒子甩出被动白水，
  寿命/重力/阻尼 + 最多 `max_foam` 颗；CLI 低模球可视、`simulate` 计数。
  泡沫**与刚体/地板碰撞**（最近表面投影、法向消能 + 5% 回弹、切向 0.94 摩擦），
  但不参与密度约束、不给刚体反作用力。回归
  `foam_does_not_fall_through_the_floor`（修复前 min_y = -2.40 m）。**不是**两相流/
  气泡求解器。
- [x] 气泡/气相粒子（`fluids[].bubbles`，缺省关闭；气泡上升速度随半径
  ∝ √r，合并/膨胀后的大泡更快；可选静水压压缩、破碎与尾流）：按 `bubble_rate` 生成、
  反向重力浮升、阻尼、与刚体/地面碰撞；每步把气泡体积内的液体粒子径向推开并给
  速度（气泡受反作用）；接触气泡按体积守恒合并（位置/速度按体积加权）；可选 `bubble_expansion > 0` 让半径随上升指数膨胀（上限
  4 倍）以近似减压；浮出液面会破裂并生成一颗被动泡沫（需 `foam=true`），
  寿命结束则消失。CLI 低模球可视、`simulate` 报告气泡数。回归两条：气泡不穿地板/数量上限，气泡上浮且确实排开液体（与关闭气泡的
  同一初始状态逐粒子比较；`bubble_expansion` 让半径从 0.03 涨到 >0.036；
  两颗重叠气泡合并后气体体积变化 ≤2%；`bubble_compressibility > 0` 时按
  `r = r_surface/(1+k·depth)^(1/3)` 的等温 `pV≈const` 近似压缩深处气泡，
  `deeper_bubbles_are_compressed_by_hydrostatic_pressure` 断言深处半径更小，
  k=0 直接走旧路径。`bubble_max_radius > 0` 时超过上限的气泡按体积守恒拆成
  多颗（固定圆环排布，受 `max_bubbles` 限制）；`large_bubbles_break_up_and_conserve_gas_volume`
  断言破碎后总量变化 ≤2%；缺省 0，旧路径逐位不变。`larger_bubbles_rise_faster` 断言大泡
  在同一时间内升得更高——浮力按 r³、阻力按 r²，终端速度 ∝ √r，半径等于
  `bubble_radius` 时因子逐位为 1）。
  [x] 气泡参与 PBF 密度约束（`bubble_density_coupling`，缺省 true，可关）：
  用核支持体积上的局部气体体积分数把有效静止密度降为 `rho0 * (1 - gas)`，
  气泡周围液体膨胀；回归 `gas_density_coupling_expands_liquid_around_a_bubble`
  （同一气泡、开/关耦合对照，开启后粒子平均半径 >关闭 0.5%）。同时修正 CLI：
  `FluidDoc` 的 bubbles/bubble_rate/max_bubbles/半径/浮力/阻尼/膨胀/耦合现在
  真的接进 `FluidDesc`；回归 `scene_bubble_fields_are_actually_simulated`
  （修复前报告 bubbles 永远为 0）。
  [x] 气泡过压（`fluids[].bubble_pressure`，缺省 0）：按 `p = p0·(r_rest/r)³`
  把内压转成液体径向压力加速度 `a = p/(ρ·r)`（边缘最大、到 `reach` 衰减），
  `bubble_pressure=0` 时整段不执行、旧路径逐位不变；回归
  `bubble_pressure_pushes_liquid_outward_without_the_old_path`（开/关对照，径向
  速度总量更大、速度有限），CLI 回归 `scene_bubble_fields_are_actually_simulated`
  接受该字段并保持有限速度。这是**气泡尺度压力耦合**，不是独立气相动量方程。
  [x] 气泡动量反冲（`fluids[].bubble_momentum_coupling`，0~2，缺省 0）：
  把液体从径向排开/过压/尾流得到的总动量按 0.5·ρ·V 附加质量反推给气泡，
  回归 `bubble_momentum_recoil_reduces_injected_liquid_momentum`（总动量更接近
  守恒、液体动量不增加、速度有限）；CLI 字段已接线。0 时旧路径逐位不变。
  仍**不是**严格两相流：没有独立气相动量/相变/表面张力，`gas` 是体积分数近似；

  局部气体分数还会折算进刚体浮力（气泡占据体积不再排开液体，上限 0.9）。
  回归 `aerated_fluid_reduces_buoyancy_on_a_body`：一颗贴着表面的气泡让浮力降到
  约 0.70 倍。
- [x] 粒子间最小分离（`fluids[].particle_collision`，缺省关闭）：每步 PBF 位置
  迭代后把距离小于直径的邻居沿连线推开（各半），再投影刚体边界；0 时旧路径逐位
  不变。回归：两颗几乎重合粒子在开关下分离到直径外。
  这项勾选只表示已有逐对算子，不代表相连粒子或封闭容器同时满足直径分离。
- [ ] 全图粒子与边界耦合分离：三粒子 ABC/CBA 的真实单遍结果约 0.065 m，
  封闭容器 125 粒子 × 120 步的最小间距约 0.05428 m，均未满足固定 0.07990 m 目标。
  [链压回证据](experiments/particle_chain_recompression.md)与
  [封闭容器强目标](experiments/sealed_particle_separation.md)必须同时验收；
  [固定预算全图半空间实验](experiments/particle_global_halfspace.md)已按强目标否决：
  代数与保护控制通过、两种三粒子顺序通过，但完整封闭容器有 46/120 次未认证，
  最小间距 0.002796 m。候选已移除；未测成本，未交付全图分离求解器。
- [x] 粒子局部流场拖曳（`fluids[].flow_coupling`，1/s，缺省 0）：取刚体 AABB 内
  粒子平均速度，把浸没刚体 XZ 速度拖向它；AABB 内无粒子时跳过，0 时旧路径逐位
  不变。回归 `flowing_particles_drag_a_body_toward_the_local_flow`、
  `flow_velocity_requires_particles_inside_the_box` 与 CLI 往返/校验。
  这是局部速度耦合，不是逐点无滑移边界/两相流。
- [x] 流体-刚体切向接触摩擦（`fluids[].contact_friction`，0~1，缺省关闭）：
  库仑上限 μ·|Jn|；静置接触用重力法向支持力近似载荷，动态刚体受反作用。
  滑动水柱减速、动态刚体被拖走回归；0 时旧路径逐位不变。
- [x] 摄像头点云两种固化：体素块 + Surface Nets 平滑表面（编辑器实时跟随）。
  重建网格会按针孔内参把顶点投回 `latest.rgb`，把摄像头画面作为基础色贴图并
  无光照直出（不让场景灯光/阴影二次染色）；没内参/RGB 时退回蓝灰纯色。回归 2 条投影/UV 单测 + GPU 贴图测试
  `reconstruction_uses_camera_rgb_as_a_base_color_texture`。
- [x] 点云重建前按每轴 0.5% 分位裁剪深度飞点（<64 点不裁；>20 万点先等距抽样）。
- [x] 桌面「显示点云」按摄像头原图给深度点取色（针孔投影 → 最近纹素，
  sRGB→线性；无 RGB/内参或长度不符时退回青色十字）。回归
  `point_cloud_colors_sample_the_camera_image`；只影响显示，不改点云/重建。
- [x] 深度-only 摄像头点云：采集端 `--no-points` 时，编辑器用 JSON 内参与
  `depth.point_step/z_near/z_far/origin` 现算点云；缺内参不猜，声明了
  `latest.points` 但文件缺失时也不偷偷改用深度。3 条回归覆盖映射约定、步长与缺文件。
- [x] Web 外部深度注入：`window.quench3dCameraDepth(...)` 把 u8 相对深度按
  fx/fy/cx/cy/z_near/z_far/step/origin 反投影成引擎点云，画成青色十字；
  「从注入深度重建表面」用同一套体素/Surface Nets 生成线框。Node 探针
  `check_web.mjs` 实测 2×2 深度 → 4 点、64×64 平面 → 5180 三角形；清空后归零。
  普通 RGB 摄像头仍不产生深度，必须外部注入。
- [x] Web 深度时间累积：`window.quench3dCameraDepthAccumulation(true)` 或窗口
  「深度时间累积」用 `PointCloudAccumulator`（2 cm 体素、90 帧 TTL）合并多帧；
  Node 探针实测单帧 4096 点 → 两帧 5632 点，关闭后回到 4096。
- [x] 摄像头 ICP 实时预算与稠密网格最近邻：最近邻从每轮 27 次 `HashMap`
  查询改为构建一次的稠密网格链表（自适应格子、每轴 ≤120 格），4k 参考 +
  1.5k 当前点 release 下 **3.4 → 1.35 ms / 3 轮**（显式 ignored 基准）；
  `grid_nearest_matches_a_brute_force_search` 与暴力搜索逐点对照；
  `PointCloudRegistration.iterations` 记录收敛轮数，小运动回归断言 <24 轮。
- [x] Web 深度 ICP 位姿跟踪：`window.quench3dCameraDepthTracking(true)` 或 Web
  摄像头窗口「跟踪位姿（ICP，移动相机）」把注入的相邻深度帧用同一套
  `register_point_clouds` 搬到参考系；打开时自动开时间累积，关闭累积同时关跟踪。
  Node 探针实测三帧移动平面配准 3 帧、rms 有限，且跟踪后的点数少于不跟踪的叠加。
  仍是增量跟踪，不是全局 SLAM；普通 RGB 摄像头仍需外部注入深度。
- [x] 摄像头点云时间累积（编辑器「时间累积」开关，缺省关）：最近约 90 帧按
  1.5 cm 体素去重/平均，最多 25 万体素，过期 90 帧自动丢弃；缺省关时仍走单帧
  旧路径。回归：互补帧合并、同体素平均/排序、上限/NaN/过期、两次运行逐位一致。
- [x] 摄像头增量位姿跟踪（编辑器「跟踪位姿（ICP）」，缺省关，需同时开时间累积）：
  相邻帧点到点 ICP + 小角度线性化，空间哈希最近邻、固定采样/遍历顺序；把
  “当前相机 → 参考帧”的位姿叠到累积点云。`register_point_clouds` 回归
  `point_cloud_registration_recovers_a_known_rigid_motion`（4 mm/2° 内恢复，
  最坏点误差 <4 mm）、`..._reports_no_match_instead_of_faking_identity`（10 m
  错位返回 None）、`..._is_deterministic`；组合顺序回归
  `tracking_composes_increments_into_the_reference_frame`（两段增量叠到参考系
  误差 <6 mm）。边界：增量跟踪，不是全局 SLAM；大位移/无重叠会重置累积器；
  桌面与 Web 都可用；Web 走外部深度注入。
- [x] 精确 CCD（任意凸体 TOI），不是中心射线近似。
  - [x] **凸-凸保守推进**（`objects[].ccd_precise`，隐含 `ccd=true`）：用 GJK
    距离在时间上 1-Lipschitz 的性质，按 `Δ距离 / (|v_rel| + |ω_a|R_a + |ω_b|R_b)`
    推进到真实首次接触；不再依赖角点采样。回归
    `precise_ccd_catches_a_convex_hull_spike_between_samples`：带尖刺凸包侧向撞
    薄墙，旧中心射线路径整步穿过（y>1.0），精确路径截住（y<0.5）；
    CLI 回归 `precise_ccd_is_wired_from_the_scene_file`。半空间用支持函数沿法线
    做 1-Lipschitz 推进：回归 `precise_ccd_stops_a_rotating_hull_spike_on_a_plane`
    （300+ 点凸包的刺在采样间隙，旧路径转 2.5 rad 穿地，精确路径 1.7 rad 截住）。
    三角网用逐三角形 GJK + 同一上界推进：回归
    `precise_ccd_stops_a_rotating_hull_spike_on_a_trimesh_wall`（采样间隙长刺穿墙）。
    先做局部三角形 AABB + 静态无旋转快路径，AABB 间隙作为距离下界；再加
    局部均匀网格（三角形 AABB 中心入格、查询按最大三角形半径膨胀）、球-三角形
    解析最近点，并在精确球 TOI 已找到后跳过 9 条旧射线。20k 三角形基准
    `precise_ccd_trimesh_20k_perf` 从 35.6 ms 降到 **1.3~1.5 ms**（断言 <5 ms）；
    查询候选完整性由 `tri_grid_candidates_cover_every_overlapping_triangle` 与
    暴力扫描逐三角形对照。网格只在 ≥256 三角形时构建；仍是显式 `ccd_precise`
    高成本路径，没有跨帧缓存/BVH。
  - [x] 盒子 8 条角射线（旋转盒的角撞矮墙）；球/胶囊中心射线与分组回归。
  - [x] 盒子旋转扫掠：8 角点这一步走出的线段取最早 TOI，线/角速度同乘 t；
    0.2 rad 级旋转盒对照（无 CCD 继续转过接触角）回归。
  - [x] 胶囊端盖极点/侧壁、圆柱/圆锥 16 边环、圆角盒外角、凸包顶点（最多 256 采样）
    的旋转扫掠；半步 + 整步两段，避免"圆弧顶点在两帧之间"漏薄墙。回归：胶囊、
    圆柱、凸包三种自转体分别比无旋转扫掠更早截住。
  - [x] 球心扫掠：对盒做保守推进（球心到盒面距离 1-Lipschitz，不会越过首次接触），
    对半空间线性求交、对球解二次方程。球擦盒顶角（中心射线从上方掠过、球面却碰到角）
    不再整步穿过去；修复前无 CCD 球心嵌进盒角，修复后 gap ≥ 半径。
    回归 `ccd_catches_a_sphere_grazing_a_box_corner`。
  - [x] 球对所有形状的扫掠：胶囊/圆柱/圆锥/圆角盒/凸包/三角网用
    `distance_point_to_shape` 距离场保守推进，法线取中心差分；曲面擦碰再扫 2 次，
    切向切进曲面时直接截到接触点。回归 `ccd_catches_a_sphere_grazing_a_cylinder_rim`
    （球心航线在圆柱顶盖上方、球面擦顶盖；修复前 gap=0.05、修复后 ≥0.1）。
    球对三角网的最坏情况是逐三角形距离，开销高但只在 `ccd=true` 时走。
  - [x] 球扫掠用相对运动：目标这一步按线速度外推（静态体为零），运动学/动态
    目标穿过球心航线也能被截住。回归 `sphere_sweep_accounts_for_a_moving_target`
    （运动学盒子 +Y 4 m 穿过球 +X 航线；修复前扫掠返回 None）。
  - [x] 旋转目标（盒子/胶囊/柱/锥/圆角盒/凸包/三角网）：这一步按角点移动
    ≤ 半径/4 采样，每个时刻用旋转后的距离场判断接触；长条盒、长胶囊转 90°
    扫过球心航线都能截住。回归 `sphere_sweep_sees_a_rotating_target_corner`、
    `sphere_sweep_sees_a_rotating_capsule`（修复前均返回 None）。旋转**平面**与
    **球**仍不参与（球旋转对称；平面只按起始法线求交）。这是时间采样，不是精确
    连续 TOI。
  - 未做：默认路径（未开 `ccd_precise`）的极端点/16 边环之间仍可能漏；
    旋转平面/旋转球仍按静态姿态；三角网精确路径是逐三角形 O(T)，大网格需空间索引。
- [ ] 流形跨帧复用；三角网↔三角网、平面↔平面接触。
- [ ] 关节并行求解；多体/articulation。
  - 已做：编辑器「物理关节」面板（改伺服目标写回底稿、删除关节）；可从两个
    选中刚体建无马达铰链（锚点取世界中点，两端轴换算到各自局部空间）；回归
    `joint_target_writeback_uses_degrees_for_hinges_and_meters_for_sliders`、
    `new_hinge_anchors_meet_at_the_world_midpoint_and_share_an_axis`、
    `generated_hinge_keeps_both_anchors_together_in_the_world`。
  - 仍缺：关节/锚点/轴 gizmo 编辑、给已有无马达关节加马达、6DOF/万向节。
- [x] 滚动摩擦（`rolling_friction`，缺省关闭，球体回归测试）。
- [x] 圆形摩擦锥（`physics.friction_cone`，缺省关闭，斜向滑动单测）。
- [x] 各向异性摩擦（`friction_anisotropy` + `friction_axis`，缺省关闭；
  沿轴/横轴滑动距离回归、CLI 校验/接线）。
- [x] 0.2 mm 径向间隙（`radial_clearance`，米；fixed/ball/hinge 点约束，
  0.2 m / 0.2 mm 回归，预测式速度上限防一步穿透）。
- [x] 纯 FK 回放（`physics.mode:"fk"`）：无重力/接触，铰链子树每步直接旋到
  目标角，无 PD 收敛；回归 `kinematic_fk_rotates_the_hinge_subtree_to_the_target_immediately`、
  `kinematic_fk_moves_the_whole_descendant_subtree` 与 CLI
  `fk_mode_reaches_the_joint_target_in_one_step`。支持 hinge/slider；滑块回归
  `kinematic_fk_slides_a_slider_subtree_to_the_target`。
  - 固定附件随祖先传播、运动学底座优先定根、累计不足固定步不改位姿；分别由
    `kinematic_fk_carries_fixed_attachments`、
    `kinematic_fk_uses_kinematic_roots_independent_of_insertion_order`、
    `kinematic_fk_waits_for_an_accumulated_physics_step` 覆盖。
  - 旧端点槽位复用时跳过失效关节（`kinematic_fk_ignores_joints_with_reused_body_slots`）；
    反向铰链/滑块正负目标和限位由 `kinematic_fk_reversed_endpoints_preserve_target_sign_and_limits` 覆盖。
  - 无锥限位球窝（`cone_limit: None`）可传递祖先铰链/滑块的刚性变换，并携带后代，
    保持当前相对姿态；回归 `kinematic_fk_carries_free_ball_attachments` 检查正反端点、
    非单位相对旋转、锚点及固定后代，并分别从初始状态验证直接驱动和公开步进。
    不选择球窝自由转角、不修正初始锚点误差；有锥限位球窝同样只传递祖先变换，FK
    阶段不求解锥限位，公开步进仍由后续动力学求解器处理；多底座闭环冲突处理仍缺失，
    不代表任意机械机构完整 FK。
  - 距离关节可传递祖先刚性变换，保留当前相对姿态及两锚点距离，并携带固定后代；
    `kinematic_fk_carries_distance_attachments` 覆盖旋转/平移、正反端点和非单位姿态，
    直接调用/公开步进分别从初始状态验证。传播阶段不投影静止长度或求解弹簧刚度；
    公开步进仍运行后续动力学求解器，回归使用初始长度匹配的刚性距离关节。
  - 无位置目标的被动铰链/滑块（无马达或仅速度马达）也传递祖先变换，保留当前
    相对姿态；`kinematic_fk_carries_passive_hinge_and_slider_attachments` 覆盖两种祖先驱动、
    两类被动关节、正反端点和马达配置，直接调用/公开步进分别从初始状态验证后代。
    FK 传播阶段不执行速度马达，也不纠正被动关节自身限位或初始锚点/姿态误差；
    公开步进随后仍运行动力学求解器。公开回归使用合法姿态、无限位和零速度马达，
    非零速度与不一致限位仅在直接 FK 调用中验证，不宣称公开步进会冻结这些动力学。
- [ ] TGS/SIMD 求解器。
- [x] 深堆 2000 体实时（目标 <16 ms/步，本机量化）：本机 release 60 步
  **14.7~15.3 ms/步**；显式 `deep_stack_2000_perf` 断言 <16 ms（优化前 17.04、
  本轮开始时 18.69 ms/步都会失败）。
  - [x] ≥512 代理切 BVH 宽相：2000 体宽相 21.3 → 2.5 ms/步，整步 43.7 → 25.3 ms；
    官方 bench 同会话 34.3 → 29.6 ms/步；候选对与网格逐对相等。
  - [x] 窄相候选对并行（native，候选 ≥256；`SolverConfig::parallel_narrowphase`，
    缺省开）：每对候选独立求交，结果按原候选顺序合并，与串行**逐位相同**。
    同会话 A/B（2000 体深堆、240 预热 + 120 步、关睡眠）：29.11 → 25.11 ms/步，
    窄相 6.08 → 3.70 ms；回归 `parallel_narrowphase_is_bit_identical_to_serial`
    （断言并行真的启用 + 逐位比较全部位姿/速度/睡眠）。
  - [x] 基准防“塌堆”诊断：`bench --phases` 打印最终 `y[min,max]`/最大速度；
    `QUENCH3D_BENCH_VEL_ITER` 做迭代 A/B。2000 体深堆（release）10/8/6/5/4 次迭代：
    接触 5526/5894/6172/6286/7083，总耗时 23.1/23.0/21.7/26.2/29.6 ms；
    减少迭代因穿透/塌堆增加接触，反而更慢，缺省 10 次保留。
  - [x] 速度迭代局部化：每个接触一次 `split_at_mut` 取出两个刚体视图，
    摩擦/法向/滚动都在这两个局部引用上做；每次迭代 10 次，深堆从
    17.04 → **15.8 ms/步**（速度迭代 9.56 → 8.3 ms）。运算顺序不变，
    现有位级/黄金摘要回归全过。
  - [x] BVH 建树用 `select_nth_unstable_by` 只定位中位，不再每层整段排序：
    2000 体宽相 2.48 → 1.77 ms/步，候选对集合与旧实现相同（最终统一排序去重）。
  - [x] 大场景接触响应预乘：接触点 ≥2048 时，准备阶段把
    `inv_i_world(r × dir)` 算好，速度/松弛/位置迭代只乘标量，省掉每次的叉积和
    3x3 乘法；2000 体深堆 **18.69 → 14.7~15.3 ms/步**。小场景（<2048 接触点）
    仍走旧的逐次运算，黄金摘要回归逐位不变；大场景角速度末位可能差 1 ulp，
    因此这是按规模启用的快速路径，不是无条件替换。
  - 剩余大头仍是单岛内速度迭代（~6.9 ms）；TGS/SIMD 仍未做。

### M5 玩法与平台（4–6 人月）

- [ ] 存档/回放格式与版本化；时间缩放、单步、渲染插值。
  - [x] `Engine::time_scale`（0–4，缺省 1）：实时物理/动画/粒子/仿真时长按缩放，
    FPS 与相机手感不缩放；编辑器有拖动条与 0.25×/0.5×/1×/2× 菜单。回归
    `time_scale_scales_physics_and_engine_time_but_not_fps`。
  - [x] 刚体位姿回放格式：`ReplayClip::record/encode/decode/sample/apply`，
    二进制 magic `Q3RP` + 版本 1 + FNV-1a 校验；位置 lerp、旋转 slerp；
    截断/magic/版本/校验和/非有限值都返回 `ReplayError`。回归
    `replay_round_trips_and_samples_between_frames`、`encode_decode_preserves_frames_and_checksum`、
    `corrupt_truncated_and_future_versions_are_rejected`、`empty_clip_encodes_and_decodes`。
    边界：只录刚体位姿，不录速度/流体/关节；不是完整存档格式。CLI 已接
    `record <场景> <文件>` 与 `replay <文件>`，端到端回归
    `record_and_replay_round_trip`（含损坏文件报错）。`replay --scene --at --render`
    可把回放某一时刻写回场景并出图，像素回归
    `replay_can_render_a_frame_from_a_recorded_clip`（0s 与 0.4s 的 PNG 必须不同）。
    `movie --replay <Q3RP>` 可把整段回放渲染成视频（不推进物理，按录制步长写回
    位姿）；回归 `movie_can_play_back_a_recorded_clip` 用重力为 0 的播放场景
    证明首尾帧不同，并覆盖句柄对不上时的明确报错。
  - [x] 固定步长渲染插值：`PhysicsWorld::body_interpolated_transform` 保存上一
    子步位姿，`Engine::render_interpolation`（缺省关）按累加器 alpha 插值，
    `Scene::sync_from_physics_interpolated`；编辑器默认开。回归
    `interpolated_transform_is_between_the_last_two_fixed_steps` 与
    `scene_can_render_interpolated_body_poses`；alpha=1/缺省关仍走旧位姿路径。
    边界：只插值刚体位姿，不插值流体粒子/泡沫/气泡/动画。
  - [x] 编辑器回放时间轴：工具栏录制/播放/停止 + 拖动条；录制按物理步采刚体
    位姿，播放暂停物理、写回后立即同步场景。回归
    `replay_recording_captures_steps_and_seeks`、`replay_timeline_records_plays_and_restores_pause`。
    边界：每渲染帧最多一帧（卡顿合并步只记末帧），只录刚体位姿，不进场景文件。
- [ ] 音频/输入/脚本在桌面与 Web 两端可达；移动端触屏。
- [ ] UI/文字系统或明确稳定的宿主接口（HUD 不靠第三方示例）。

### M6 交付与发布（2–3 人月）

- [ ] 三平台 + wasm CI 每次提交真跑；真 GPU WebGPU 路径验收。
- [ ] `wasm-opt`/文件名哈希/签名；预编译发行。
  - [x] 发行包内 Brotli `.br`（quality 11；14,010,301 → 3,087,644 字节），门禁解压
    后与 wasm 逐字节比较；静态服务器仍需配置 `Content-Encoding: br`。
  - [ ] `wasm-opt`、文件名哈希、签名/安装器。
  - [x] 本地可复现发行包（`tools/package_release.sh` + 门禁里的
    `tools/check_package.sh`）：二进制、web/pkg、manifest、SHA256SUMS。
  - [ ] wasm-opt、文件名哈希、签名/安装器与真实三平台发行（Brotli `.br` 已有）。
- [ ] `cargo publish --dry-run`、SBOM、依赖漏洞扫描、semver 策略。

## 2. 执行顺序与规则

1. 按 M1 → M6 顺序，前一层没有验收证据不跳。
2. 每项先补“会红的测试”，再实现；旧场景默认路径必须逐位不变。
3. 不引入第三方物理；`quench3d-physics` 保持 `#![forbid(unsafe_code)]`。
4. 每个里程碑结束跑全门禁并更新本文件的勾选与数字。
5. 不把“行业对标里我们不做”的条目偷偷移出清单；要移出必须写明理由与代价。

## 3. 现实约束

- 上表合计约 **31–46 人月**，不含美术资产、平台适配与真实 GPU 硬件矩阵。
- 单会话/单人无法在可接受时间内真实完成；本文件是执行账本，不是完成宣告。
- 当前最先做的是 **M1 逐资源释放**：它是后续大场景、资源热重载与 Web 长会话的地基。
