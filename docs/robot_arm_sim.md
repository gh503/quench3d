# robot-modkit M1 机械臂仿真（4 自由度 / 3 铰链 + 夹爪）

本文记录 quench3d 对 `robot-modkit M1`（≈360 g、上臂 120 mm、小臂 110 mm、3× 30 kg·cm 总线舵机、关节径向间隙 0.2 mm、40 mm 夹爪）的支持范围、推荐参数、验收命令与**已知缺口**。

> 结论先写：**P0（串链伺服、`joints[]` 遥测、`joint_target` 运行时控制）已实现并通过自动化测试；P0-3 的夹持用了“理想刚性抓取”而不是摩擦夹持；P1/P2 没有完成。** 不要把本页当作生产完成证明。

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

`joints[].angle_deg`、`target_deg`、`error_deg` 是度；`torque_nm` 是 N·m。滑块关节的轨迹用米（`unit: "m"`），铰链用度（`unit: "deg"`）。

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
* `target_deg` 必填，单位度。
* `speed_deg_s` 可选：按这个最大角速度逐步逼近；不写 = 立即设到目标。
* 动作在 `at_time`（按 `round(t/dt)` 折算）或 `at_step` 指定的外层步开始前触发。
* 只支持铰链伺服（有 `motor_target_deg`/`motor_kp` 的 hinge）；滑块/固定关节会报校验错误。
* 改目标只改 `target_angle`，不改增益/限幅；两端刚体自动唤醒。

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
| P0-3 **真实摩擦夹持** | **FAIL / 未实现** | fixture 中方块从 t=0 就与夹爪用**场景固定关节**连接，是理想真空/刚性抓取；手指闭合是视觉动作，不靠摩擦。动态手指 + 力闭合在接触求解下会弹出方块，未解决 |
| P1-1 `objects[].visual` | **FAIL / 未实现** | 场景没有 `visual` 字段；渲染器也不支持 STL。M1 整机 STL 渲染未做 |
| P1-2 运动学 drive | **FAIL / 未实现** | 有 `body:"kinematic"`，但没有“只播关节轨迹、不解重力/接触”的 drive 模式；逐帧位姿一致性未验证 |
| P1-3 wasm `setJointTarget/...` | **FAIL / 未实现** | wasm 侧没有新增这四个导出，示例页/尺寸/60 fps 文档未写 |
| P2 MCP 新工具 / 接触力查询 / 1000 步 <2 s | **FAIL / 未实现** | 未做；`run_simulate` 是 CPU 路径，未做 1000 步性能门 |

## 8. 门禁结果

`CARGO_NET_OFFLINE=true bash tools/gate.sh`：**全绿**，871 个测试 0 失败，wasm 13,471,362 字节（基线 13,449,106，+22 KB），fmt/clippy（原生 + wasm32）/doc/license/check_web 全部 PASS。

## 9. 确定性

* 所有新增字段都是可选的；不写 `motor_kp` 时仍走老的 `motor_stiffness/damping` 伺服路径，老场景结果不变。
* 关节名不写时报告用 `joint[i]`；写 `name` 只影响报告/动作引用，不参与求解。
* `--trace` 是只读采样，不写回世界。
* 抓取 fixture 含固定关节和 `force` 动作，**不是**老场景，不能拿来证明老场景逐位不变。

## 10. 还没做

* 真实摩擦/接触夹持（P0-3 的理想化是当前最大缺口）。
* `visual` 字段 + STL/GLB/OBJ 绑定刚体姿态。
* 运动学 drive / 逐帧确定性截图。
* wasm 关节 API、示例页、60 fps 规模数据。
* MCP `joint_target/simulate_steps/query_contact`、接触力查询、批量性能门。
* 关节径向间隙 0.2 mm 的建模（当前铰链是理想约束，没有间隙）。
