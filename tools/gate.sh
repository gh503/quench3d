#!/usr/bin/env bash
# quench3d 的唯一门禁入口：本地跑一次 = CI 跑的那几件事。
#
#   tools/gate.sh              # 全套（fmt + clippy×2 + 测试 + wasm + check_web）
#   tools/gate.sh --fast       # 跳过 wasm 构建与 check_web（改纯 Rust 时够用）
#   GATE_SKIP_WEB=1 tools/gate.sh
#
# 为什么要有这个脚本：门禁是**五件事**，而且顺序有讲究 ——
# `check_web.mjs` 必须最后跑（它检查的是刚构建出来的 wasm 产物）。
# 手敲五条命令容易漏、容易顺序错，漏了的那条恰恰是最容易红的那条。
#
# 需要 wasm32 目标与 clippy 组件：
#   rustup target add wasm32-unknown-unknown && rustup component add clippy rustfmt
set -uo pipefail

cd "$(dirname "$0")/.." || exit 1

FAST=0
for arg in "$@"; do
  case "$arg" in
    --fast) FAST=1 ;;
    -h | --help)
      sed -n '2,10p' "$0"
      exit 0
      ;;
    *)
      echo "未知参数：$arg（--fast / --help）" >&2
      exit 2
      ;;
  esac
done
if [ "${GATE_SKIP_WEB:-0}" = "1" ]; then FAST=1; fi

LOG_DIR="${GATE_LOG_DIR:-target/tmp}"
mkdir -p "$LOG_DIR"

fail=0
step() { printf '\n== %s ==\n' "$1"; }
ok() { printf 'PASS %s\n' "$1"; }
bad() {
  printf 'FAIL %s\n' "$1"
  fail=1
}

step "fmt（格式）"
if cargo fmt --all --check; then ok fmt; else bad fmt; fi

step "clippy（原生目标，-D warnings）"
if cargo clippy --workspace --all-targets -- -D warnings >"$LOG_DIR/gate-clippy.log" 2>&1; then
  ok clippy-native
else
  bad clippy-native
  tail -n 30 "$LOG_DIR/gate-clippy.log"
fi

step "clippy（wasm32 目标，-D warnings）"
if cargo clippy --workspace --target wasm32-unknown-unknown --all-targets -- -D warnings \
  >"$LOG_DIR/gate-clippy-wasm.log" 2>&1; then
  ok clippy-wasm
else
  bad clippy-wasm
  tail -n 30 "$LOG_DIR/gate-clippy-wasm.log"
fi

step "测试（全部 crate）"
if cargo test --workspace >"$LOG_DIR/gate-test.log" 2>&1; then
  ok tests
  grep -E '^test result:' "$LOG_DIR/gate-test.log" |
    awk '{gsub(/;/,"",$4); s+=$4} END {printf "  测试总数：%d（0 失败）\n", s}'
else
  bad tests
  grep -E '^(test result: FAILED|error)' -A 10 "$LOG_DIR/gate-test.log" | head -n 30
fi

if [ "$FAST" = "1" ]; then
  printf '\n（--fast：跳过 wasm 构建与 check_web）\n'
else
  step "wasm 构建（web_client）"
  if tools/build_web.sh web_client >"$LOG_DIR/gate-wasm.log" 2>&1; then
    ok web-build
    ls -l web/pkg/*.wasm | awk '{printf "  wasm 字节：%s\n", $5}'
  else
    bad web-build
    tail -n 20 "$LOG_DIR/gate-wasm.log"
  fi

  # 必须最后跑：它检查的就是上一步刚构建出来的产物。
  step "web 产物检查（check_web.mjs）"
  if node tools/check_web.mjs >"$LOG_DIR/gate-web.log" 2>&1; then
    ok check-web
  else
    bad check-web
    tail -n 20 "$LOG_DIR/gate-web.log"
  fi
fi

printf '\n'
if [ "$fail" = "0" ]; then
  echo "门禁全绿 ✅（日志在 $LOG_DIR/gate-*.log）"
else
  echo "门禁未过 ❌（日志在 $LOG_DIR/gate-*.log）" >&2
fi
exit "$fail"
