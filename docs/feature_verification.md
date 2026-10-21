# 正向验证矩阵（可复现命令）

> 目的：把「功能存在」和「功能真的改变状态 / 画面」分开。每一项都给出**能重跑的命令**
> 与判据；没有自动化证据的项标「未覆盖」，不写成绿。
>
> 最近一次全量结果：`tools/verify_all.sh` 全绿；门禁 **663** 个测试、0 失败、3 ignored；
> wasm 构建 + `check_web.mjs` + WebGL2 启动 + Web UI 按键探针 + 摄像头纯函数测试都通过。

## 0. 一条命令

```bash
tools/verify_all.sh
```

它依次跑：`tools/gate.sh` → `tools/probe_webgl2.sh` → `tools/probe_web_ui.sh` →
`python -m unittest discover -s tools/camera_mocap`。没有 Chrome 的机器上前两条会
**跳过并返回 0**（与仓库其它 GPU 测试一致），不会假装绿；看输出里的「跳过」。

## 1. 引擎 / 渲染（像素证据）

| 功能 | 正向证据（命令 / 用例） | 判据 |
| --- | --- | --- |
| PBR + 光照 + 天空 | `cargo test -p quench3d-render --test post_pipeline` | 像素与手算 ACES/sRGB 对上，误差 ≤1 |
| 阴影强度只暗影区 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test fog_and_shadow_quality` | 影区变暗、非影区不变 |
| 阴影级联 1–4 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test shadow_cascades` | 远处遮挡物只在有级联时出现影子 |
| 点光 / 聚光阴影 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-render --test point_lights` | 有/无投影的像素差 |
| Bloom | `cargo test -p quench3d-render --test post_pipeline bloom` | 亮区外溢、关闭时参数无效 |
| FXAA | `cargo test -p quench3d-render --test post_pipeline fxaa` | 边缘变平滑、平面不变 |
| SSAO | `cargo test -p quench3d-render --test post_pipeline ssao` | 接触处变暗、远平面不变 |
| 水面后处理 | `cargo test -p quench3d-render --test post_pipeline water` | 开/关、波强/泡沫/折射都改变像素 |
| 解析式环境高光 | `cargo test -p quench3d-render --test post_pipeline analytic_environment` | 金属球开关像素不同 |
| 透明排序 | `cargo test -p quench3d-render --test post_pipeline transparent` | 远蓝后画、近红先画仍得到正确混合 |
| 网格深度 | `cargo test -p quench3d-render --test post_pipeline grid_writes_depth` | 地面下方线段被遮住、上方可见 |
| 材质 alpha / 双面 | `cargo test -p quench3d-render --test material_pipeline_traits` | 左右半幅像素不同 |
| 后端回退 / GL 翻译 | `cargo test -p quench3d-render --test backend_fallback`；`cargo test -p quench3d-render --lib gl_pipelines` | 候选过滤、所有 WGSL 能翻译到 GLSL |

## 2. CLI / 场景格式

| 功能 | 正向证据 | 判据 |
| --- | --- | --- |
| `validate` / 诊断码 / 退出码 | `cargo test -p quench3d-cli` | 每条诊断路径 + 退出码 0/2/3 |
| 单帧 `render` PNG | `cargo test -p quench3d-cli --test cli_e2e render_produces` | 合法 PNG、尺寸/统计 |
| 多输出 + no-clobber | `... multiple_render_outputs` | 两张路径/尺寸先解析、整体不写半套 |
| 命名相机 + `camera_name` | `... camera_flag` / `multiple_render_outputs` | 已知名/`default`/未知名；报告字段 |
| 雾 / 阴影质量 / 天空 | `... fog_and_shadow_quality` | 字段真的从 JSON 传到像素 |
| 动画 / 动作 / 探针 | `cargo test -p quench3d-cli --test cli_e2e`（animation/probe/action 用例） | 状态/探针结果随步数变化 |
| 场景 include / 模板 / 实例 | `cargo test -p quench3d-cli --test scene_fixtures` | 合并、重名、路径、往返 |
| 模型动画对账 | `cargo test -p quench3d-cli --test cli_e2e model` | 动画名存在/不存在、时长/轨道 |
| 物理 / 关节 / 复合形状 / CCD | `cargo test -p quench3d-physics` | 确定性、休眠、过滤、传感器、关节 |
| MCP / ask 工具 | `cargo test -p quench3d-cli --test agent_tools`、`mcp` | 工具清单、stdio 握手、模拟 LLM |
| JSON Schema | `cargo test -p quench3d-cli schema` | 文档/schema/系统提示三处对账 |

## 3. 编辑器（状态 + 存读 + UI）

| 功能 | 正向证据 | 判据 |
| --- | --- | --- |
| 全面板同帧绘制 | `QUENCH3D_GPU_TESTS=1 cargo test -p quench3d-editor --test ui_smoke` | 工具栏/场景树/属性/视口/帮助/摄像头窗口产出图元 |
| 增删 / 复制 / 重置 | `cargo test -p quench3d-editor --test history` | 撤销重做、拖拽合并、快照逐字节 |
| 层级 / 改父 / 环拒绝 | `cargo test -p quench3d-editor --test hierarchy` | 世界位姿保持、导出 parent |
| 存读无损 / 模板实例 / 水 / 流体 | `cargo test -p quench3d-editor --test scene_roundtrip` | 存→读→再存字节一致 |
| 历史 / 模型实例变换 | `cargo test -p quench3d-editor` | 模型组变换写回 |
| 摄像头协议 / 3D 骨架 | `cargo test -p quench3d-editor --lib camera` | 解析/长度/坐标/线段数量/低可见度 |

## 4. Web / 浏览器

| 功能 | 正向证据 | 判据 |
| --- | --- | --- |
| wasm 构建 + 产物自检 | `tools/build_web.sh web_client`；`node tools/check_web.mjs` | 符号/实例化/内存都在 |
| WebGL2 启动 | `tools/probe_webgl2.sh` | 第一帧日志 + 遮罩收起 + 无 panic/`INDEPENDENT_BLEND`/Cube 启发式 |
| Web UI 按键 → 状态 → 画面 | `tools/probe_web_ui.sh` | F3/F4/R 前后 UI 状态与中央视口截图都必须变（暂停态，排除物理运动） |
| 浏览器场景往返（纯 CPU） | `cargo test -p quench3d-editor --lib` / wasm 导出探针 | 场景 JSON 往返 |

## 5. 摄像头动捕（桌面 Linux，实验）

```bash
target/tmp/camvenv/bin/python tools/camera_mocap/camera_mocap.py --source synthetic
python -m unittest discover -s tools/camera_mocap -p 'test_*.py'
```

| 功能 | 判据 |
| --- | --- |
| 深度归一化 | 常量深度全 0、单调、NaN/Inf 安全 |
| 轮廓提取 | 最大外轮廓、bbox、面积、空 mask |
| 骨骼连接 | 35 根骨、33 个关节、低可见度不画 |
| 协议帧 | JSON 解析、RGB 长度/尺寸上限、坐标翻转、过期帧 2 s 消失 |

## 6. 明确未覆盖（不要当成绿）

* **GPU 蒙皮 / 骨骼动画**：glTF `skins` + `JOINTS_0/WEIGHTS_0` 仍未实现；带骨骼角色只能静止。
* **IBL / HDRI / 环境 cubemap**：只有解析式环境高光。
* **LOD / 遮挡剔除 / 大规模合批**：逐实例视锥剔除。
* **音频、脚本层、粒子、贴花、地形、多视口**：没有。
* **Web 摄像头**：只有桌面文件协议；浏览器 `getUserMedia` 未接。
* **真机 GPU/浏览器矩阵**：只有 headless Chrome + SwiftShader（软件光栅）；真机 WebGPU 未验证。
* **单帧 `render` 的 golden 像素基线**：仍只断言 PNG 合法与场景内像素差，没有跨运行黄金图。
* **UI 每一个按钮的点击**：`ui_smoke` 只证明全面板能画，`probe_web_ui` 覆盖 F3/F4/R 三个真实按键（R 切缩放/平移手柄）；其余按钮仍靠人工/单元路径，未逐一点击。
