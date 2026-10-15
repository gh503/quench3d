#!/usr/bin/env bash
# 构建 Quench3D 的 Web 版本：编译 wasm → 运行 wasm-bindgen → 输出到 web/pkg/
#
# 依赖：
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version 0.2.129   （必须与 wasm-bindgen crate 版本一致）
#
# 本机的工具链在 infra-dev/.rustup、wasm-bindgen 在 infra-dev/.cargo-home/bin，
# 而系统默认的 ~/.rustup 里没有 wasm32 目标——脚本会自己检测并切过去，
# 也会去 $CARGO_HOME/bin 找 wasm-bindgen，所以通常直接跑就行。
#
# 用法：
#   tools/build_web.sh [example_name]      # 默认 web_client（web/index.html 对应的入口）
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$(pwd)"
REPO_ROOT="$(cd "$ROOT/.." && pwd)"
EXAMPLE="${1:-web_client}"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
WASM="$TARGET_DIR/wasm32-unknown-unknown/release/examples/${EXAMPLE}.wasm"
OUT="$ROOT/web/pkg"

# ---- 工具链环境 ----------------------------------------------------------
# 本机的 Rust 工具链（含 wasm32 目标）装在仓库内的 .rustup 里，系统默认的
# ~/.rustup 里没有 wasm32-unknown-unknown。`cargo`/`rustc` 本身是 rustup 代理，
# 决定用哪套工具链的是 RUSTUP_HOME，所以这里在「当前工具链确实缺 wasm32 目标」
# 时才切到仓库内的那套——不覆盖你自己已经配好的环境。
has_wasm_target() {
  local sysroot
  sysroot="$(rustc --print sysroot 2>/dev/null)" || return 1
  [[ -d "$sysroot/lib/rustlib/wasm32-unknown-unknown" ]]
}

if ! has_wasm_target && [[ -d "$REPO_ROOT/.rustup" && -d "$REPO_ROOT/.cargo-home" ]]; then
  echo "==> 当前工具链没有 wasm32 目标，切到仓库内的工具链：$REPO_ROOT/.rustup"
  export RUSTUP_HOME="$REPO_ROOT/.rustup"
  export CARGO_HOME="$REPO_ROOT/.cargo-home"
fi

if ! has_wasm_target; then
  cat >&2 <<'EOF'
错误：当前 Rust 工具链缺少 wasm32-unknown-unknown 目标。
      修复：rustup target add wasm32-unknown-unknown
      （若使用仓库自带工具链，请确认 <仓库的上级目录>/.rustup 存在，
        或手动 export RUSTUP_HOME=<该目录>）
EOF
  exit 1
fi

# wasm-bindgen 可能只装在工具链的 bin 目录里，没进 PATH
WASM_BINDGEN="$(command -v wasm-bindgen || true)"
if [[ -z "$WASM_BINDGEN" && -n "${CARGO_HOME:-}" && -x "$CARGO_HOME/bin/wasm-bindgen" ]]; then
  WASM_BINDGEN="$CARGO_HOME/bin/wasm-bindgen"
fi
if [[ -z "$WASM_BINDGEN" ]]; then
  cat >&2 <<'EOF'
错误：找不到 wasm-bindgen。
      安装：cargo install wasm-bindgen-cli --version 0.2.129
      （版本必须与 wasm-bindgen crate 一致，否则绑定的胶水代码会不匹配）
EOF
  exit 1
fi

echo "==> 编译 wasm (example: $EXAMPLE)"
# `--locked`：严格按 Cargo.lock 打包，避免"本地一个版本、CI 另一个版本"。
cargo build --locked --release --target wasm32-unknown-unknown --example "$EXAMPLE"

if [[ ! -f "$WASM" ]]; then
  echo "找不到产物: $WASM" >&2
  exit 1
fi

rm -rf "$OUT"
mkdir -p "$OUT"

# 构建戳：记录这个包是用哪个 example 构建的。
# tools/check_web.mjs 靠它判断「产物是否比源码旧」——
# 没有戳就只能按 mtime 粗略判断，改任何一个示例都会误报。
cat > "$OUT/build_stamp.txt" <<STAMP
example=$EXAMPLE
built_at=$(date +%s)
STAMP
echo "==> 运行 wasm-bindgen ($("$WASM_BINDGEN" --version))"
"$WASM_BINDGEN" --target web --no-typescript --out-dir "$OUT" --out-name quench3d_web "$WASM"

SIZE=$(du -h "$OUT/quench3d_web_bg.wasm" | cut -f1)
echo "==> 完成：$OUT (wasm $SIZE)"
echo
echo "本地预览（需要一个静态服务器，模块化 WASM 不能直接 file:// 打开）："
echo "    python3 -m http.server 8000 --directory \"$ROOT/web\""
echo "    然后打开 http://localhost:8000/"
