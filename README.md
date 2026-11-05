# Quench3D

**用桌面编辑器、浏览器或 AI agent 搭建 3D 场景，运行物理仿真，并输出图像与视频。**

Quench3D 是 Rust 编写的开源 3D 引擎，包含自研物理引擎、PBR 渲染器、桌面／Web 编辑器，以及面向自动化的 CLI 和 MCP 服务。手工操作和编辑器 AI 助手使用同一个可见场景；命令行与外部 agent 通过场景 JSON 驱动引擎。

[下载桌面版](https://github.com/gh503/quench3d/releases/latest) · [编辑器指南](docs/editor_guide.md) · [AI 助手配置与工具](docs/ai_agent.md) · [CLI／MCP 参考](docs/cli_agent.md) · [SDK 开发指南](docs/getting_started.md)

## 可以做什么

| 你的目标 | 推荐入口 | 能得到什么 |
| --- | --- | --- |
| 查看模型、布置空间、调整外观 | 桌面／Web 编辑器 | 导入模型、图元搭建、相机视角、灯光与材质调整，保存场景或导出模型 |
| 用自然语言修改眼前的场景 | 编辑器右侧 AI 助手 | 创建和修改独立图元、设置刚体与轨道、显示名称／轨道线、切换环境、控制播放，并查看实际执行回执 |
| 演示碰撞、堆叠、关节或运动 | 编辑器、场景 JSON、SDK | 刚体仿真、暂停／单步／重置；通过 JSON 或代码使用更完整的仿真能力 |
| 做太阳系等空间运动演示 | 编辑器 AI 助手 | 独立周期的公转、月球绕移动地球、连续轨道线、中文名称标签、宇宙星空 |
| 批量验证场景、截图或录制视频 | CLI／MCP | 场景诊断、无 GPU 物理仿真、PNG、视频／动图、结构化报告 |
| 开发交互原型或小游戏 | Rust SDK | 自定义输入、相机、游戏逻辑与物理交互，复用桌面和 Web 渲染能力 |

**选择时需要知道：**太阳系公转是可保存的运动学轨道动画，尚不是万有引力／N 体模拟。编辑器 AI 工具的范围小于整个引擎，不能任意修改模型子网格、复合体或父子层级。Quench3D 适合场景创作、演示和原型验证；工程级仿真与性能要求应先针对目标场景验证。更多边界见[能力边界](docs/limitations.md)。

## 三分钟上手

### 1. 打开编辑器

从 [Releases](https://github.com/gh503/quench3d/releases/latest) 下载对应平台的归档，解压后运行 `quench3d-editor`；Windows 运行 `quench3d-editor.exe`。不需要安装 Rust。发行平台包括 Linux x86_64、Windows x86_64、macOS Apple Silicon 和 Intel。

源码用户可以运行：

```bash
cargo run --release --locked -p quench3d-editor
```

源码构建使用仓库锁定的 Rust 工具链，依赖与平台准备见[编辑器指南](docs/editor_guide.md)。

### 2. 先完成一个小场景

用“添加”创建球体或方块，调整位置与颜色，然后点击“播放”。工具栏可以暂停、单步、重置，也可以直接切换前／后／左／右／俯视／仰视、立体视角和透视／正交投影。

- 选中物体后使用“聚焦选中”找到它。
- 拖动左右侧栏靠近视口的边界调整宽度；顶部“场景栏／右侧栏”可以收起或恢复，布局会记住。
- 使用“文件 → 保存”保存场景 JSON，“打开”重新加载。打开会替换当前场景，先保存需要保留的编辑。
- 坐标轴属于本机编辑器偏好，默认隐藏；天体名称、轨道、轨道线开关和宇宙环境随场景保存。

完整鼠标、键盘和面板说明见[编辑器操作](docs/editor_guide.md)。

### 3. 让 AI 帮你搭建

在右侧“AI 助手 → 本地模型配置”中添加供应商，填写 OpenAI Chat Completions 兼容地址和可选密钥，获取并选择模型。不支持模型列表的服务也可以手动填写模型 ID。模型需要支持工具调用。

先试一个容易核对的目标：

> 创建一个半径 0.4 米的红球，放在 [0,3,0]，聚焦它，然后播放仿真。

再试空间演示：

> 创建太阳、地球和月球，地球绕太阳，月球绕移动的地球。使用宇宙星空环境，显示中文名称和连续轨道线，隐藏坐标轴与网格，然后播放。

助手会先检查场景，再调用工具；执行回执和错误可展开查看。支持连续追问、停止、新建和切换会话，历史恢复不会重放场景工具。模型配置和聊天记录保存在本机，密钥不会写进场景 JSON；本地配置并非加密存储。连接方式、工具范围与故障处理见[AI 助手说明](docs/ai_agent.md)。

### 浏览器使用

从源码仓库根目录运行（Python 3.10+，无需额外 pip 包）：

```bash
python3 tools/agent_server.py
```

打开 `http://127.0.0.1:8765`，即可使用 Web 编辑器及 AI 的同源代理，再在界面中配置模型。源码仓库包含已构建的 Web 资源；修改 Rust 后需要重新构建。

GitHub 的 Web 归档可用于静态托管，但**静态托管本身不提供 AI 请求代理**；当前 GitHub 发行归档也不保证包含上述 Python 脚本，使用 Web AI 时建议采用源码仓库。浏览器模型配置按当前来源保存，与桌面配置独立。后端选择、构建和模型文件加载见[Web 使用指南](docs/editor_guide.md#2-快速开始web-端)。

## 给 AI agent 与自动化脚本

按任务选择接口，避免混淆文件处理与实时场景操作：

| 接口 | 适用任务 | 状态与结果 |
| --- | --- | --- |
| 编辑器 AI 工具 | 修改当前打开的场景 | 使用唯一物体名和最新 revision，先暂停并检查；以成功工具回执确认操作 |
| CLI `quench3d` | 校验 JSON、物理推演、批量渲染与导出 | 读取文件，支持 `--json` 结构化报告和明确退出码；仿真无需 GPU，渲染需要 |
| MCP `quench3d mcp` | 外部 agent 调用引擎工具 | stdio 工具服务，能力由工具清单描述；不是连接已打开编辑器的远程控制接口 |
| Rust SDK | 自定义运行时逻辑与交互 | 直接集成引擎 API，详见开发指南与可运行示例 |

从下载包中的 `quench3d` 开始，或在源码中执行 `cargo install --locked --path crates/quench3d-cli`，然后运行：

```bash
quench3d schema --json
quench3d doctor --json
quench3d example --out scene.json
quench3d validate scene.json --json
quench3d simulate scene.json --steps 600 --json
quench3d render scene.json --out scene.png --width 960 --height 600
```

agent 应先读取 `schema` 确认实际支持的字段，再 `validate`，需要物理结论时运行 `simulate`，最后按需 `render`；不要把生成文件或网络响应当作执行成功。退出码、JSON 信封、完整场景格式、MCP 配置、`ask`／`chat` 以及视频导出均见 [CLI／MCP 参考](docs/cli_agent.md)和[错误码](docs/error_codes.md)。

## 文档导航

| 文档 | 内容 |
| --- | --- |
| [编辑器与 Web 指南](docs/editor_guide.md) | 安装、启动、鼠标键盘操作、导入导出、浏览器部署与源码入口 |
| [编辑器 AI 助手](docs/ai_agent.md) | 模型连接、会话保存、实时工具、revision、取消与网络行为 |
| [CLI／MCP 参考](docs/cli_agent.md) | 全部命令、场景 JSON、报告契约、外部 agent 接入 |
| [SDK 开发指南](docs/getting_started.md) | 帧循环、项目结构、自定义逻辑与开发路线 |
| [SDK 与能力参考](docs/sdk_reference.md) | 数学、场景、物理、渲染、音频、相机、示例及详细参数 |
| [仿真视频](docs/simulation_video.md) | 图像序列、视频与回放工作流 |
| [测试与验证](docs/testing.md) | 单元、集成、GPU、浏览器与打包检查 |
| [性能与资源](docs/performance.md) | 基准方法、测量环境与优化记录 |
| [能力边界](docs/limitations.md) | 物理、渲染、编辑器、Web 和格式的具体限制 |

## 开发与发布

桌面与 Web 共用编辑器代码和 WGSL，渲染基于 `wgpu`；物理库只依赖项目自己的数学库。引擎 API、目录结构与开发示例从[开发指南](docs/getting_started.md)进入，验证步骤见[测试文档](docs/testing.md)。

推送到 `main` 后，GitHub Actions 自动递增补丁版本并创建 tag；各平台及 Web 构建成功后发布 [Release](https://github.com/gh503/quench3d/releases)。失败重跑复用原 tag。构建状态见 [Actions](https://github.com/gh503/quench3d/actions)。

## 许可证

采用 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 双许可，可任选其一。
