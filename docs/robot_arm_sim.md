# robot-modkit M1 机械臂仿真（4 自由度 / 3 铰链 + 夹爪）

本文记录 quench3d 对 `robot-modkit M1`（≈360 g、上臂 120 mm、小臂 110 mm、3× 30 kg·cm 总线舵机、关节径向间隙 0.2 mm、40 mm 夹爪）的支持范围、推荐参数、验收命令与**已知缺口**。

> 结论先写：**P0（串链伺服、`joints[]` 遥测、`joint_target` 运行时控制、真实摩擦夹持）与 P1-1/P1-2/P1-3 的主体已实现并有自动化证据；P1-1 的 STL 局部对位是从官方尺寸推导并对位，但上游没有官方装配矩阵；P2 完成了 MCP `joint_target`、`query_contact` 与 1000 步性能门。** 不要把本页当作生产完成证明。

## 1. 单位

| 量 | 场景字段 | 单位 |
|---|---|---|
| 长度 | `position` / `size` / `anchor_*` / `rest_length` | 米（m） |
| 质量 | `mass` | 千克（kg） |
| 力 / 力矩 | `force` / `motor_max_torque` / `break_torque` | N / N·m |
| 角度（人读字段） | 所有 `*_deg` | 度（°） |
| 角度（求解器 / 限位） | `limits`（铰链） | 弧度（rad） |
| 角度（运行时 API） | `set_joint_target` | 弧度（rad） |
| 角速度 | `motor_velocity` | rad/s |
| 时间 | `simulate.dt` / `at_time` | 秒（s） |

`joints[].angle_deg`、`target_deg`、`error_deg` 是度；`torque_nm` 是 N·m。滑块关节的位移/目标/误差分别是 `displacement_m`、`target_m`、`error_m`（米），`angle_deg` 为 0；`--trace` 里滑块 `unit: "m"`、`value` 是米，目标/误差用 `target_m`/`error_m`。滑块 `torque_nm` 实际是最大推力方向的力（N，字段名沿用铰链）。

## 2. 推荐参数（M1）

```json
{
  "simulate": { "steps": 2500, "dt": 0.002 },
  "physics": { "substeps": 8, "solver_iterations": 10, "position_iterations": 4 }
}
```

| 关节 | 惯量（约） | `motor_kp` (N·m/rad) | `motor_kd` (N·m·s/rad) | `motor_max_torque` |
|---|---|---|---|---|
| yaw | 1.7e-5 kg·m² | 12 | 0.08 | 2.5 N·m |
| shoulder | 2.5e-3 kg·m² | 12 | 0.40 | 2.5 N·m |
| elbow | 2.8e-4 kg·m² | 12 | 0.15 | 2.5 N·m |

规则：`kd ≈ 2√(kp·I)` 附近取值；30 kg·cm 舵机额定 2.94 N·m，`motor_max_torque` 要略低于额定值（推荐 2.5），否则断裂阈值/限幅前会出现尖峰。

**三档增益都通过**（测试里实际跑的组合）：

| 档 | `kp` | `kd` 缩放 |
|---|---|---|
| 低 | 10 | 0.85 |
| 中 | 12 | 1.0 |
| 高 | 16 | 1.3 |

## 3. 子步与 dt

`physics.substeps` 只把**每个外层步**再等分，不改变 `simulate.steps * simulate.dt` 的总时长。推荐：

| 外层 dt | `steps`（5 s） | `substeps` | 等效子步 |
|---|---|---|---|
| 1/500 | 2500 | 8 | 4000 Hz |
| 1/120 | 600 | 8 | 960 Hz |
| 1/60 | 300 | 32 | 1920 Hz |

1/60 且 `substeps ≤ 8`（480 Hz）时，肘关节在 2.5 N·m 限幅下会持续振荡，**不要**用这个组合做 5 s 收敛验收。

PD 力矩每个子步只施加一次（不是每个速度迭代一次）。`torque_nm` 用子步长 `dt / substeps` 换算。

## 4. 运行时改目标：`joint_target` action

```json
{
  "type": "joint_target",
  "joint": "shoulder",
  "target_deg": 0,
  "at_time": 1.0,
  "speed_deg_s": 90
}
```

* `joint` 是 `joints[].name`；不写名字的关节是 `joint[i]`。
* 铰链用 `target_deg`（度）；滑块/夹爪用 `target_m`（米），二选一。
* `speed_deg_s` / `speed_m_s` 可选：按这个最大速度逐步逼近；不写 = 立即设到目标。
* 动作在 `at_time`（按 `round(t/dt)` 折算）或 `at_step` 指定的外层步开始前触发。
* 铰链伺服：`motor_target_deg` + `motor_kp`（N·m/rad）/`motor_kd`（N·m·s/rad）/`motor_max_torque`（N·m）。
* 滑块伺服（夹爪开合）：`motor_target_m`（米）+ `motor_kp`（N/m）/`motor_kd`（N·s/m）/`motor_max_torque`（N，最大推力）。只写目标不写 `motor_kp` 会报错，不会静默不动。
* 改目标只改 `target_angle`，不改增益/限幅；两端刚体自动唤醒。

滑块伺服示例（夹爪）：

```json
{
  "type": "slider", "name": "finger_l_slide",
  "a": "gripper", "b": "finger_l", "axis": [0, 0, 1],
  "limits": [0.0255, 0.04],
  "motor_target_m": 0.04, "motor_kp": 1000, "motor_kd": 20,
  "motor_max_torque": 5
}
```

```json
{"type": "joint_target", "joint": "finger_l_slide",
 "target_m": 0.0255, "speed_m_s": 0.005, "at_time": 1.2}
```

单位是**米**，不是毫米；`motor_max_torque` 名字沿用铰链字段，但在滑块上是最大推力 N。

`weld` / `release` 是实验性的运行时固定约束（`target` + `other`），当前**不建议**用于产品场景：在关节链 + 接触同时存在时求解器可能产生很大冲量，M1 抓取 fixture 改用场景内固定关节。

## 5. 报告字段

`quench3d simulate scene.json --json` 的 `data`：

```json
{
  "joint_count": 3,
  "joints": [
    {
      "name": "shoulder",
      "kind": "hinge",
      "angle_deg": 34.30,
      "target_deg": 35.0,
      "error_deg": 0.70,
      "torque_nm": 0.147,
      "limit_state": "ok",
      "broken": false
    }
  ],
  "joint_traces": [
    {
      "name": "shoulder",
      "kind": "hinge",
      "samples": [
        { "step": 0, "time": 0.0, "value": 0.0, "unit": "deg",
          "target_deg": 35.0, "error_deg": 35.0, "torque_nm": 0.0 }
      ]
    }
  ]
}
```

* `joint_count` 是**当前仍存在**的关节数（断开的会移除）。
* `joints[]` 按场景顺序，**断开的关节仍保留一条**，`broken: true`，`angle_deg`/`torque_nm` 为 0，`target_deg` 取场景里配置的值。
* `limit_state`：`ok` / `low` / `high`（铰链/滑块限位）。
* `--trace shoulder` 输出 `joint_traces`；`--trace-all` 会同时跟踪刚体和关节。轨迹的 `step/time` 是外层步，不是子步。

## 6. fixture 与几何修正（重要）

`tests/fixtures/modkit_arm.upstream.scene.json` 是上游生成器
`robot-modkit/sim/quench_scene.py` 的原样输出；它有三处**刚体中心与关节锚点不重合**：

| 关节 | 上游锚点间距 | 原因 |
|---|---|---|
| yaw | 10.0 mm | `column` 的 `anchor_a.y = 0.045`，但 yaw 块中心在 y=0.105 |
| shoulder | 29.2 mm | link1 中心 `[0.085, 0.12]`，锚点 `[-0.06,0]` 推出 shoulder 在 x=0.025,y=0.12 |
| elbow | 25.0 mm | link2 中心 `[0.225, 0.12]`，锚点 `[-0.055,0]` 推出 elbow 在 x=0.17,y=0.12 |

点约束会在第一步把两端硬拉到一起（相当于瞬移 1–3 cm），伺服残差会被这个瞬移污染到 2–8°，且随 dt 变化。quench3d 的校验器现在会对 >1 mm 的锚点错位报 `suspicious_value` warning。

`tests/fixtures/modkit_arm.scene.json` 用最小修正：

* `column.anchor_a.y = 0.055`
* `link1.position = [0.110, 0.105, 0]`
* `link2.position = [0.225, 0.105, 0]`

这些修正**应该回上游生成器**；在修好之前，直接用上游场景做 ±1° 验收会失败，不是增益能救的。

## 7. 验收命令与结果

```bash
# P0-1：5 s 收敛（三档增益 × 三种 dt，4 个测试）
cargo test -p quench3d-cli --test robot_arm serial_chain_servo

# P0-2：肩静力矩 + 关节轨迹 + 锚点诊断
cargo test -p quench3d-cli --test robot_arm shoulder_holding_torque
cargo test -p quench3d-cli --test cli_e2e robot_arm_joint_trace_and_anchor_diagnostic

# P0-3：joint_target + 抓取
cargo test -p quench3d-cli --test robot_arm joint_target_action
cargo test -p quench3d-cli --test robot_arm grasp_script
cargo test -p quench3d-cli --test robot_arm friction_gripper

# P1-1：visual 只影响渲染；整机 STL 渲染
cargo test -p quench3d-cli --test cli_e2e visual_mesh
cargo test -p quench3d-cli --test cli_e2e modkit_arm_visual
cargo run -p quench3d-cli -- render \
  crates/quench3d-cli/tests/fixtures/modkit_arm_visual.scene.json \
  --out target/tmp/m1_visual.png --width 640 --height 480

# P1-2：drive 回放逐位确定性
cargo test -p quench3d-cli --test cli_e2e drive_mode
cargo test -p quench3d-physics --lib collisions_can_be_disabled

# P1-3：wasm API（构建后由 check_web 在 Node 里真调用）
bash tools/build_web.sh web_client
node tools/check_web.mjs
```

| 项 | 结果 | 数值 / 说明 |
|---|---|---|
| P0-1 肩 35°±1° | **PASS** | 1/500、1/120、1/60(32 子步) × 三档增益；最差 34.155°（误差 0.845°） |
| P0-1 肘 -55°±1° | **PASS** | 最差 -54.150°（误差 0.850°）；1/60+8 子步会振荡，已排除 |
| P0-1 偏航倾角 ≤0.5° | **PASS** | 实测 0.0°（yaw 关节角 0.0°） |
| P0-1 无持续振荡 / 不越限 | **PASS** | 5 s 末误差稳定；`limit_state=ok`，`broken=false` |
| P0-2 `joints[]` 字段 | **PASS** | `name/angle_deg/target_deg/error_deg/torque_nm/limit_state/broken` |
| P0-2 肩静力矩 15% | **PASS（对解析值）** | 仿真 0.147 N·m；按 fixture 质量/几何解析 0.152 N·m（3.4%）。**不是**对用户粗估 0.19 N·m 的 15%（差 22.6%）；0.19 需要更大质量或更长力臂 |
| P0-2 `--trace` 关节序列 | **PASS** | 2501 个采样（初始 + 2500 步），末点 34.30°/35° |
| P0-2 断开显式标记 | **PASS** | `joint_count=0`，`joints[0].broken=true`（e2e 已断言） |
| P0-3 `joint_target` 运行时改目标 | **PASS** | 1 s 时把肩 35°→0°，2 s 后 0.7° |
| P0-3 抓取场景/脚本 | **PASS（带理想化）** | 方块从 y=0.061 抬到 0.207（+0.146 m），相对夹爪滑移 0；手指从 ±0.04 合到 ±0.028 |
| P0-3 滑块位置伺服 | **PASS（单元 + CLI）** | 单元：目标 0.5 m，600 步后误差 <1 cm；CLI：`target_m=0.2` 到 0.19999996 m，报告只有 `target_m/error_m`，没有把米转成度 |
| P0-3 **真实摩擦夹持** | **PASS** | `modkit_arm_friction_grasp.scene.json`：40 mm 方块与任何刚体间**没有场景关节**，只靠两个滑块手指的接触摩擦；0 s 先抬到肩 −10°/肘 10°，0.8 s 下压回 0°/0°（above→descend），1.5 s 滑块手指闭合到 ±0.0255 m，4 s 起以 5°/s 抬到肩 64°/肘 −64°，18 s 末仍夹住；方块从 y=0.100 升到 y=0.203（**+0.103 m**），相对夹爪滑移 **<1 mm**，手指不撞限位、关节不断裂。测试断言 scene joints 中没有 `cube` |
| P0-3 100 g 加严 | **PASS** | `modkit_arm_friction_grasp_100g.scene.json`：预紧 ±0.0248 m、手指 kp 8000 / 限力 12 N，抬升 **0.101 m**、滑移 **<0.3 mm**，无 cube 关节；200 g 同参数只到 0.098 m，未声称 |
| P0-3 旧“理想抓取” fixture | PASS（保留） | 方块从 t=0 用固定关节挂住，只用于验证脚本时序；真实抓取以上面这一行为准 |
| P1-1 `objects[].visual` | **PASS（功能与整机渲染）** | `visual` 支持 STL/GLB/GLTF/OBJ，挂成主物体子节点；加/不加 visual 的模拟 `states` 逐位相同；整机 STL 渲染 33,318 三角形 / 11 object；立柱已修正为 90 mm 竖直。上游没有官方装配矩阵，对位按官方尺寸（base 90 / l1 120 / l2 110 / link_bar 100）推导，见 §8 |
| P1-2 运动学 drive | **PASS** | `physics.mode:"drive"`：重力 0、接触 0，只解关节；`--trace` 新增逐帧四元数；两次运行 301 个采样逐位相同，见 §9 |
| P0-3 真实抓取根因 | 已修 | 旧 `modkit_arm_grasp` 沿用**未修正的 link 锚点/中心**，在负载下肘关节会自己塌；把手指/方块放到按 `modkit_arm` 修正几何算出的腕点并 +20 mm 避开 `link2` 后，同一套 2.5 N·m 限幅的 PD 就能稳定夹持。这不是给抓取“加力”，而是修场景几何 |

| P1-3 wasm `setJointTarget/...` | **PASS（API/示例）** | 导出 `loadScene/setJointTarget/step/getJointStates/getBodyTransform`；Node 里真实装载 modkit 场景，2 s 后 shoulder=20.00°，矩阵 16 个有限数；`web/robot.html` 滑杆预览；wasm/Node 1000 步 1.106 s；headless Chrome 1000 步 0.81–1.07 s（0.81–1.07 ms/步）；Canvas 预览 0.09 ms/帧（CPU 绘制，非 wgpu）；桌面 1000 刚体 0.153 ms/步，见 §10 |
| P2 MCP `joint_target` | **PASS** | `quench3d_joint_target`：铰链 `target_deg` / 滑块 `target_m`，移除同关节的脚本动作后步进，返回 `joints[]`；单测用 modkit drive 场景到 20°±2° |
| P2 1000 步 <2 s | **PASS** | wasm/Node 1000 步 **1.106 s**；headless Chrome **0.81–1.07 s**；桌面 bench 1000 刚体 0.153 ms/步 |
| P2 接触力查询 | **PASS** | `simulate --json` 新增 `contact_forces[]`（名字、法线、位置、冲量、`force_n=impulse/fixed_dt`、sensor）；MCP `quench3d_query_contact` 可按 `min_force_n` 过滤 |

## 8. P1-1：`objects[].visual`

```json
{
  "name": "link1",
  "body": "dynamic",
  "shape": { "type": "box", "size": [0.12, 0.025, 0.025] },
  "visual": {
    "path": "meshes/link1.stl",
    "position": [0, 0, 0],
    "rotation_deg": [0, 0, 0],
    "scale": 1.0
  }
}
```

* 支持 `.stl` / `.glb` / `.gltf` / `.obj`；路径相对**场景文件**解析。
* 网格挂成主物体的**子节点**：主物体由刚体驱动，子节点只保留 `visual.position/rotation_deg/scale` 的局部变换。子节点不写回物理，`shape`/`shapes` 仍是唯一碰撞体。
* 复用了现有 `load_model`/`attach` 管线；多网格模型的内部层级保持原样，只有根节点挂到主物体。
* 编辑器存盘按同名物体继承 `visual`，不会“打开带外观的场景、随手存一次、外观丢了”。
* 校验阶段拒绝不支持的扩展名、非有限位姿、非正缩放，都有 JSON 路径。

**整机证据**：`tests/fixtures/modkit_arm_visual.scene.json` 用 robot-modkit 的真实 STL
（`chassis_plate` / `chassis_joiner` / `joint_base` / `joint_lid` / `output_disc` /
`link_bar` / `jaw_block`，MIT）合并成 5 个外观网格，挂到 5 个刚体上。渲染报告
**33,318 个三角形、11 个 object**；`modkit_arm_visual_meshes_attach_and_render` 断言
>30,000 三角形且 ≥10 个 object。每个网格是刚体的子节点，位置/朝向由父链精确继承，
不存在“每帧重新算一次导致漂移”的路径。

**已知近似**：5 个外观网格的局部变换按 M1 官方尺寸（base 90 / l1 120 / l2 110 /
link_bar 100 mm）推导并与关节轴对位；立柱本轮已从横躺修正为 90 mm 竖直。上游仓库
没有提交 `cad/out/`，也没有官方装配矩阵，所以这不是“抄官方装配文件”；夹爪块仍是
腕部占位。要做逐孔/逐螺柱的精确对位，需要上游先导出已装配 STEP/STL。见
`tests/fixtures/meshes/README.md`。

**对照**：`visual_mesh.scene.json` 的球体从 y=0.4 落到 y=-3.95；加/不加 visual 的
`states` 逐位相同，证明 `visual` 不进物理。

## 9. P1-2：`physics.mode:"drive"`

```json
{
  "physics": { "mode": "drive", "substeps": 32 },
  "simulate": { "steps": 300, "dt": 0.016666667 },
  "actions": [
    { "type": "joint_target", "joint": "shoulder", "target_deg": 35, "at_time": 0, "speed_deg_s": 90 },
    { "type": "joint_target", "joint": "elbow", "target_deg": -55, "at_time": 0.5, "speed_deg_s": 90 }
  ]
}
```

* `drive` 把重力设为 0、关闭宽相/窄相/接触求解；关节 PD、限位、积分器照常。
* 报告 `drive: true`、`gravity: [0,0,0]`、`contacts: 0`。
* `--trace 名字` 的每个采样新增 `rotation: [x,y,z,w]`，用于逐帧截图/视频；老字段不变。
* 测试 `drive_mode_replays_joint_targets_without_contacts_deterministically` 跑两遍，`states` 和 301 个采样（含四元数）**逐位相同**。
* 这不是“纯运动学 FK”：关节仍由 PD 求解，所以有收敛过程；但它不跑重力/接触，满足“回放不受地面/夹爪接触影响”。`body:"kinematic"` 仍只作用于单个刚体，没有单独的“整链 kinematic”模式。

## 10. P1-3：wasm 关节 API

```js
import init, {
  loadScene, setJointTarget, step, getJointStates, getBodyTransform,
} from "./pkg/quench3d_web.js";

await init();
loadScene(sceneText);              // quench3d.scene，纯 CPU
setJointTarget("shoulder", 35);    // 度；非铰链/未知名字返回 false
step(1 / 60);                      // 一个外层步，内部按 physics.substeps 切
JSON.parse(getJointStates());      // [{name, kind, angle_deg, target_deg, error_deg, torque_nm, broken}]
const m = getBodyTransform("link2"); // 列主序 16 个 f32，可直接喂 WebGL
```

* 不启动 winit/GPU；页面自己画模型。Web Serial 仍完全由页面 JS 管，引擎不碰串口。
* `web/robot.html` 是滑杆示例：改 yaw/shoulder/elbow，`setJointTarget` + 60 次 `step(1/60)`，用 `getBodyTransform` 画关节骨架；状态栏打印 `getJointStates`。
* `tools/check_web.mjs` 在 Node 里真的实例化 wasm、装载 `modkit_arm_drive` 场景、跑 120 步，断言 shoulder=20.00°、`link2` 矩阵 16 个有限数、未知刚体返回空数组。
* wasm 体积：**13,540,657 字节**（约 13.2 MiB；P0 基线 13,449,106，+91.6 KB）。这是把完整编辑器 + 物理 + 关节 API 打进同一个 example 的代价；只做物理的页面可以用 `wasm-opt`/裁剪入口再降。
* **桌面物理规模**：`quench3d bench` 在本机 1000 刚体 **0.153 ms/步**（睡眠开启），4 刚体轨迹 3.1–3.6 ms/2404 采样；4 自由度手臂 + 手指只有约 10 个刚体，物理远低于 1 ms/步，60 fps 的 16.7 ms 预算主要留给 JS/wgpu 渲染。
* **wasm/Node 实测**（`node tools/check_web.mjs`，每次门禁都跑）：1000 步 **1106 ms（1.106 ms/步，纯物理等效 904 fps）**，低于 P2 的 1000 步 <2 s。
* **真实浏览器实测**（`node tools/check_browser.mjs`，headless Chrome，装了 Chrome 的门禁会跑）：1000 步本机多轮 **809–1069 ms（0.81–1.07 ms/步，纯物理等效约 935–1237 fps）**；同一页的 Canvas 2D 预览 **0.09–0.10 ms/帧**。这两个都是 CPU 数字，**不是 wgpu 全渲染帧率**；页面 60 fps 仍需带 GPU 的浏览器实测。

## 11. 门禁结果

`CARGO_NET_OFFLINE=true bash tools/gate.sh`：**全绿**，883 个测试 0 失败，wasm 13,540,657 字节（P0 基线 13,449,106，+91.6 KB），fmt/clippy（原生 + wasm32）/doc/license/check_web/browser-bench 全部 PASS。

## 12. 确定性

* 所有新增字段都是可选的；不写 `motor_kp` 时仍走老的 `motor_stiffness/damping` 伺服路径，老场景结果不变。
* 关节名不写时报告用 `joint[i]`；写 `name` 只影响报告/动作引用，不参与求解。
* `--trace` 是只读采样，不写回世界。
* 抓取 fixture 含固定关节和 `force` 动作，**不是**老场景，不能拿来证明老场景逐位不变。

## 13. 还没做

* robot-modkit 上游没有提交 `cad/out/` 或官方装配矩阵；当前 STL 对位按官方尺寸推导，逐孔/逐螺柱的 CAD 装配仍需上游导出已装配资产后才能替换。
* 浏览器 wgpu 渲染下的 60 fps 实测（wasm/Node 的 1000 步 1.106 s 已测；1000 刚体的 wasm 规模没测）。
* 真实夹持已验证 20 g 与 100 g、40 mm 方块、friction 2.0；200 g 同套参数只抬起 0.098 m，更重物体仍需重新标定预紧/增益。
* 纯 FK/kinematic 整链（当前 drive 仍走 PD；`body:"kinematic"` 只作用于单刚体）。
* MCP `simulate_steps` 已由 `quench3d_simulate.steps` 覆盖；`joint_target` 与 `query_contact` 已加。
* 关节径向间隙 0.2 mm 未建模：当前铰链是理想点约束，没有径向自由间隙。按 120 mm 连杆估算，0.2 mm 间隙对应末端约 `atan(0.2/120)=0.095°` 的角度不确定度，小于 ±1° 验收；但它会带来真实的回差/抖动，本页不把“理想铰链”说成“已含间隙”。
