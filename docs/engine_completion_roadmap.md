# quench3d 行业级全功能：里程碑与验收口径

> 这份文件回答一个问题：**“整个引擎开发完成”到底以什么为终点。**
> 结论先写：按行业对标（Unity/Godot/Filament/PhysX 的完整层）不是一次会话、
> 也不是一个人月级的工作；当前仓库是一个成熟的自研内核，但离“全功能引擎”还差
> 若干整层。下面的里程碑每一项都必须有测试、门禁与可复现数字，不能靠文档宣称。

## 0. 当前基线（M0）

- 门禁：`CARGO_NET_OFFLINE=true bash tools/gate.sh`
- 测试：**999 通过 / 0 失败 / 5 ignored**
- wasm：**13,809,044 字节**
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
  - 已做：`FloatImage`（线性 RGBA f32）读写 Radiance `.hdr` 与 OpenEXR
    （scanline、HALF/FLOAT/UINT、NONE/RLE 解压；写盘 HALF 无压缩）。HDR 用
    平铺 RGBE 编码；EXR 的 RLE 含预测器逆变换与奇偶字节反交错。`EnvironmentMap`
    现在也能读 `.exr` 并导出 `.hdr`/`.exr`。
  - 已做：`Rgba16Float` 浮点贴图上传（`Renderer::add_texture_float`，盒式 mip），
    GPU e2e 验证线性亮度 4.0 的贴图不会被 8 位路径截断。
  - 未做：KTX2/Basis 压缩纹理、块压缩格式、PIZ/ZIP/tiled EXR。
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
- [ ] 全 GPU morph（含法线/切线）与任意权重曲线。
- [x] 音频子系统：WAV 解码、混音、3D 衰减/声像、无设备离线测试
  （`quench3d-audio`，`Engine::audio`；真实设备输出仍由宿主适配）。
- [ ] 脚本层：宿主可注册回调，场景生命周期钩子不再只能轮询。
  - [x] 场景生命周期回调 `SceneHooks`（加入/删除/清空，缺省不装零开销；
    回调只读）。不是脚本语言/热重载。回归 `scene_hooks_fire_for_add_remove_and_clear`。

### M4 物理精度与规模（8–12 人月）

- [x] 流体连续自由表面：Surface Nets 从 PBF 粒子提取三角形网格（`fluids[].surface_mesh`，
  分辨率 8~96），`Renderer::update_mesh_data` 支持拓扑整体替换；GPU e2e
  （`fluid_surface_replaces_the_placeholder_mesh`）与提取单测 4 条。
- [x] 流体对静态三角网碰撞：逐三角形最近点 + 扫掠薄壳检测
  （`particles_stop_on_a_static_trimesh_floor`）。
- [x] 流体对凸包/圆锥碰撞：凸包 GJK/EPA、圆锥底面圆盘 + 侧面线段投影，
  都是精确最近表面（不再按 AABB 近似）；四面体与圆锥 AABB 对照回归。
- [x] 流体表面张力/涡量约束（0~5，缺省关闭）；示例 240 步最高点 11.4 m → 1.65 m。
- [x] 流体初始速度 + 开放排水面（`velocity` / `drain_y`，缺省不变）。
- [x] 流体连续发射器（`emit_*`，黄金角确定撒点、上限/排空/CLI 接线）。
- [x] 流体泡沫/飞沫（`fluids[].foam`，缺省关闭）：高速度粒子甩出被动白水，
  寿命/重力/阻尼 + 最多 `max_foam` 颗；CLI 低模球可视、`simulate` 计数。
  泡沫**与刚体/地板碰撞**（最近表面投影、法向消能 + 5% 回弹、切向 0.94 摩擦），
  但不参与密度约束、不给刚体反作用力。回归
  `foam_does_not_fall_through_the_floor`（修复前 min_y = -2.40 m）。**不是**两相流/
  气泡求解器。
- [x] 粒子间最小分离（`fluids[].particle_collision`，缺省关闭）：每步 PBF 位置
  迭代后把距离小于直径的邻居沿连线推开（各半），再投影刚体边界；0 时旧路径逐位
  不变。回归：两颗几乎重合粒子在开关下分离到直径外。
- [x] 流体-刚体切向接触摩擦（`fluids[].contact_friction`，0~1，缺省关闭）：
  库仑上限 μ·|Jn|；静置接触用重力法向支持力近似载荷，动态刚体受反作用。
  滑动水柱减速、动态刚体被拖走回归；0 时旧路径逐位不变。
- [x] 摄像头点云两种固化：体素块 + Surface Nets 平滑表面（编辑器实时跟随）。
- [x] 点云重建前按每轴 0.5% 分位裁剪深度飞点（<64 点不裁；>20 万点先等距抽样）。
- [x] 深度-only 摄像头点云：采集端 `--no-points` 时，编辑器用 JSON 内参与
  `depth.point_step/z_near/z_far/origin` 现算点云；缺内参不猜，声明了
  `latest.points` 但文件缺失时也不偷偷改用深度。3 条回归覆盖映射约定、步长与缺文件。
- [x] 摄像头点云时间累积（编辑器「时间累积」开关，缺省关）：最近约 90 帧按
  1.5 cm 体素去重/平均，最多 25 万体素，过期 90 帧自动丢弃；缺省关时仍走单帧
  旧路径。回归：互补帧合并、同体素平均/排序、上限/NaN/过期、两次运行逐位一致。
- [ ] 精确 CCD（任意凸体 TOI），不是中心射线近似。
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
  - 未做：极端点/16 边环之间仍可能漏；目标自转未参与扫掠；精确连续 TOI。
- [ ] 流形跨帧复用；三角网↔三角网、平面↔平面接触。
- [ ] 关节并行求解；多体/articulation。
- [x] 滚动摩擦（`rolling_friction`，缺省关闭，球体回归测试）。
- [x] 圆形摩擦锥（`physics.friction_cone`，缺省关闭，斜向滑动单测）。
- [x] 各向异性摩擦（`friction_anisotropy` + `friction_axis`，缺省关闭；
  沿轴/横轴滑动距离回归、CLI 校验/接线）。
- [x] 0.2 mm 径向间隙（`radial_clearance`，米；fixed/ball/hinge 点约束，
  0.2 m / 0.2 mm 回归，预测式速度上限防一步穿透）。
- [ ] TGS/SIMD 求解器。
- [ ] 深堆 2000 体实时（目标 <16 ms/步，本机量化）。
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
  - 剩余瓶颈：整堆仍多岛并行，但速度迭代 12–13 ms 是单岛内串行；目标 <16 ms 未达。

### M5 玩法与平台（4–6 人月）

- [ ] 存档/回放格式与版本化；时间缩放、单步、渲染插值。
  - [x] `Engine::time_scale`（0–4，缺省 1）：实时物理/动画/粒子/仿真时长按缩放，
    FPS 与相机手感不缩放；编辑器有拖动条与 0.25×/0.5×/1×/2× 菜单。回归
    `time_scale_scales_physics_and_engine_time_but_not_fps`。
- [ ] 音频/输入/脚本在桌面与 Web 两端可达；移动端触屏。
- [ ] UI/文字系统或明确稳定的宿主接口（HUD 不靠第三方示例）。

### M6 交付与发布（2–3 人月）

- [ ] 三平台 + wasm CI 每次提交真跑；真 GPU WebGPU 路径验收。
- [ ] `wasm-opt`/Brotli/文件名哈希；预编译发行。
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
