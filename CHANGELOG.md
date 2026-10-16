# 变更日志

本文件记录**用户看得见的变化**（新能力、行为变化、破坏性改动、性能与体积）。
格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循[语义化版本](https://semver.org/lang/zh-CN/)。

> 约定：每条都写"发生了什么 + 怎么验证"。没有验证方式的变化不要写进来。

## [Unreleased]

### 新增

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
