# M1 外观网格（测试夹具）

这些 STL 从 [robot-modkit](https://github.com/gh503/robot-modkit) 的 `cad/out/*.stl`
（MIT）合并/变换而来，只用于 quench3d 的**外观**回归，不是碰撞体，也不是装配图。

| 文件 | 来源 | 变换 |
|---|---|---|
| `m1_chassis.stl` | `chassis_plate.stl` | mm→m，X 轴 -90°（Z-up → Y-up） |
| `m1_column.stl` | `chassis_joiner.stl` | mm→m ×0.45，X 轴 +90°，y -45 mm |
| `m1_yaw.stl` | `joint_base + joint_lid + output_disc` | mm→m，X 轴 -90°，y -18.5 mm |
| `m1_link1.stl` | `link_bar.stl` | mm→m ×1.2，X 轴 -90°，x -25 mm |
| `m1_link2.stl` | `link_bar.stl + jaw_block.stl` | mm→m ×1.1，X 轴 -90°；夹爪块平移到腕部 |

`modkit_arm_visual.scene.json` 把这 5 个网格分别挂到 `chassis` / `column` / `yaw` /
`link1` / `link2` 刚体上，碰撞仍用原来的 box。整机三角形约 **33,318**。

**已知近似**：这不是 robot-modkit 的官方装配变换；各零件按 M1 的关节轴手工对位，
用于验证“STL 能挂到刚体、随关节动、渲染不崩”。要做逐零件精确对位，应以
robot-modkit 的 CAD 装配为准重新导出。
