# M1 外观网格（测试夹具）

这些 STL 从 [robot-modkit](https://github.com/gh503/robot-modkit) 的 `cad/out/*.stl`
（MIT）合并/变换而来，只用于 quench3d 的**外观**回归，不是碰撞体，也不是装配图。

| 文件 | 来源 | 变换 |
|---|---|---|
| `m1_chassis.stl` | `chassis_plate.stl` | mm→m，X 轴 -90°（Z-up → Y-up） |
| `m1_column.stl` | `chassis_joiner.stl` | mm→m ×0.45，X 轴 −90°，再平移 (0, +50, −44) mm；包围盒 y 0.005–0.095 m，与 90 mm 立柱对齐 |
| `m1_yaw.stl` | `joint_base + joint_lid + output_disc` | mm→m，X 轴 -90°，y -18.5 mm |
| `m1_link1.stl` | `link_bar.stl` | mm→m ×1.2，X 轴 -90°，x -25 mm |
| `m1_link2.stl` | `link_bar.stl + jaw_block.stl` | mm→m ×1.1，X 轴 -90°；夹爪块平移到腕部 |

`modkit_arm_visual.scene.json` 把这 5 个网格分别挂到 `chassis` / `column` / `yaw` /
`link1` / `link2` 刚体上，碰撞仍用原来的 box。整机三角形约 **33,318**。

**与上游的关系**：robot-modkit 的 `cad/build.py` 只导出**零件局部坐标**的 STL，
仓库里没有官方装配矩阵，也没有提交 `cad/out/`。因此这里的变换是从官方尺寸
（`base_h=90`、`l1=120`、`l2=110`、`link_bar` 名义长度 100 mm、底盘 240×200×5 mm）
推导并按关节轴对位，不是抄自一份上游装配文件。`link_bar` 的 ×1.2 / ×1.1 就是
120/100 与 110/100 的比例；立柱本轮已从“横躺”修正为 90 mm 竖直。

**已知近似**：连杆的宽度/厚度仍是按 STL 实测对位，未做逐孔/逐螺柱的 CAD 装配；
要更强的一致性，应由上游导出**已装配**的 STEP/STL 或装配矩阵后再替换这里。
