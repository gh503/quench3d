#!/usr/bin/env bash
# 安装 Quench3D 命令行工具（Linux / macOS）
#
#   tools/install_cli.sh            # 安装到 ~/.cargo/bin（cargo install）
#   tools/install_cli.sh --local    # 只在本仓库构建，打印二进制路径
#
# Windows 用 tools/install_cli.ps1。
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(dirname "$here")"
cd "$root"

if ! command -v cargo >/dev/null 2>&1; then
  echo "找不到 cargo。先装 Rust 1.82+：https://rustup.rs" >&2
  exit 1
fi
echo "==> Rust: $(rustc --version)"

if [ "${1:-}" = "--local" ]; then
  echo "==> 本地构建（release）"
  cargo build -p quench3d-cli --release
  bin="$root/target/release/quench3d"
  echo "==> 二进制：$bin"
  echo "==> 自检："
  "$bin" --version
  "$bin" doctor || true
  echo
  echo "提示：可以直接用 $bin，或把 $root/target/release 加进 PATH。"
  echo "试一下："
  echo "  $bin example > scene.json && $bin render scene.json --out scene.png"
  exit 0
fi

echo "==> 安装到 cargo bin 目录（默认 ~/.cargo/bin）"
cargo install --path crates/quench3d-cli --force

echo "==> 自检"
quench3d --version
quench3d doctor || true

echo
echo "==> 装好了。三步走："
echo "  quench3d schema --json                  # 让 agent 读能力清单"
echo "  quench3d example --out scene.json       # 生成示例场景"
echo "  quench3d render scene.json --out a.png  # 渲染出图"
echo
echo "想接大模型（本地 Ollama 为例）："
echo "  ollama serve && ollama pull qwen2.5:7b"
echo "  quench3d ask \"斜坡上滚球\" --out s.json --render s.png"
