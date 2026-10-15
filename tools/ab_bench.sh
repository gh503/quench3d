#!/usr/bin/env bash
# 配对 A/B 基准：同一会话内、多次运行取最小，避免机器噪声把结论带偏。
#
# 用法：
#   tools/ab_bench.sh <标签> <布局> <次数> [环境变量=值 ...]
# 例：
#   tools/ab_bench.sh 基线 stack 3
#   tools/ab_bench.sh 实验 stack 3 F3D_EXPERIMENT=1
#
# 为什么需要它：这台机器上绝对数字在会话之间会漂 ±10%，
# 「改前跑一次、改后跑一次」很容易得出相反结论（README 8.4 有这个坑的实例）。
set -euo pipefail
export CARGO_HOME="${CARGO_HOME:-$HOME/Documents/infra-dev/.cargo-home}"

label="$1"; layout="$2"; reps="$3"; shift 3
bodies="${AB_BODIES:-2000}"
steps="${AB_STEPS:-300}"
bench="cargo run --release -q -p quench3d-physics --example bench --"

best=""
for _ in $(seq 1 "$reps"); do
  ms="$(env "$@" $bench --bodies "$bodies" --steps "$steps" --layout "$layout" --no-sleep 2>/dev/null \
    | awk -v b="$bodies" '$1 == b { gsub(/ms/, "", $3); print $3 }')"
  best="$(python3 -c "print(min([x for x in ['$best','$ms'] if x]))")"
done
phases="$(env "$@" $bench --phases --bodies "$bodies" --steps 200 --layout "$layout" --no-sleep 2>/dev/null \
  | awk '/宽相|速度迭代/ { printf "%s=%s ", $1, $2 }')"
printf '%-12s %-8s 最小 %8s ms/步（%s 次取最小）  %s\n' "$label" "$layout" "$best" "$reps" "$phases"
