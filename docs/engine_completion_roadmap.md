# quench3d 行业级全功能：里程碑与验收口径

> 这份文件回答一个问题：**“整个引擎开发完成”到底以什么为终点。**
> 结论先写：按行业对标（Unity/Godot/Filament/PhysX 的完整层）不是一次会话、
> 也不是一个人月级的工作；当前仓库是一个成熟的自研内核，但离“全功能引擎”还差
> 若干整层。下面的里程碑每一项都必须有测试、门禁与可复现数字，不能靠文档宣称。

## 0. 当前基线（M0）

- 门禁：`CARGO_NET_OFFLINE=true bash tools/gate.sh`
- 测试：**896 通过 / 0 失败 / 3 ignored**
- wasm：**13,557,014 字节**
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

- [ ] Render graph / Pass 扩展点，自定义后处理可插入并出图。
- [ ] GPU 剔除 + 间接绘制；数千物体 60 fps 的实测。
- [ ] 遮挡剔除 / 自动 LOD / 多视口。
- [ ] 压缩纹理（KTX2/Basis）与浮点纹理；EXR/HDR 读写。
- [ ] FBX/USD 至少一条导入链；材质节点图或等价的自定义着色器层。
- [ ] 透明排序/OIT、投影贴花、多盏投影阴影、线性雾距离。

### M3 动画、音频与内容（6–9 人月）

- [ ] 骨骼动画编辑器：片段选择、速度、起始帧、事件编辑、根运动写回。
- [ ] 全 GPU morph（含法线/切线）与任意权重曲线。
- [ ] 音频子系统：解码、混音、3D 衰减/声像、无设备时的离线测试。
- [ ] 脚本层：宿主可注册回调，场景生命周期钩子不再只能轮询。

### M4 物理精度与规模（8–12 人月）

- [ ] 精确 CCD（旋转扫掠/凸体 TOI），不是中心射线近似。
- [ ] 流形跨帧复用；三角网↔三角网、平面↔平面接触。
- [ ] 关节并行求解；多体/articulation。
- [ ] 0.2 mm 径向间隙、滚动摩擦/各向异性、TGS/SIMD 求解器。
- [ ] 深堆 2000 体实时（目标 <16 ms/步，本机量化）。

### M5 玩法与平台（4–6 人月）

- [ ] 存档/回放格式与版本化；时间缩放、单步、渲染插值。
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
