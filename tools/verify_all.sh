#!/usr/bin/env bash
# 一条命令跑完**当前可自动化**的正向验证：
#   1. 全量门禁（fmt / clippy 原生+wasm / doc / 837 个测试 / wasm 构建 / 产物检查）
#   2. 真实 WebGL2 启动探针（headless Chrome + SwiftShader）
#   3. Web UI 按键探针（F3/F4/R：状态 + 中央视口截图都必须变）
#   4. Web 摄像头预览探针（Chrome 假摄像头，两帧必须有运动）
#   5. 摄像头采集端纯函数测试（有 venv 时用 venv）
#   6. 真正部署出去的 GitHub Pages（网络不通自动跳过）
#
# 它**不**证明：真机 GPU/浏览器矩阵、自动 LOD/遮挡剔除、音频/脚本层。
# 这些没有自动化证据，别把"verify_all 绿"当成"引擎完整"。
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 1
export CARGO_HOME="${CARGO_HOME:-$ROOT/.cargo-home}"
export RUSTUP_HOME="${RUSTUP_HOME:-$ROOT/.rustup}"
export PATH="$CARGO_HOME/bin:$PATH"
export CARGO_NET_OFFLINE="${CARGO_NET_OFFLINE:-true}"

status=0

echo "== 1/6 全量门禁 =="
tools/gate.sh || status=1

echo "== 2/6 真实 WebGL2 启动探针 =="
tools/probe_webgl2.sh || status=1

echo "== 3/6 Web UI 按键 + 截图探针 =="
tools/probe_web_ui.sh || status=1

echo "== 4/6 Web 摄像头预览探针 =="
tools/probe_web_camera.sh || status=1

echo "== 5/6 摄像头采集端纯函数测试 =="
PY=python3
if [[ -x target/tmp/camvenv/bin/python ]]; then
  PY=target/tmp/camvenv/bin/python
fi
"$PY" -m unittest discover -s tools/camera_mocap -p 'test_*.py' || status=1

echo "== 6/6 已部署 Pages 探针 =="
tools/probe_deployed.sh || status=1

if [[ "$status" -ne 0 ]]; then
  echo "正向验证未全绿 ❌"
  exit "$status"
fi
echo "正向验证全绿 ✅（边界见 docs/feature_verification.md）"
