#!/usr/bin/env bash
# 在**真实 WebGL2**（headless Chrome + SwiftShader）里跑一次 Web 客户端，并给出红/绿判据。
#
# 为什么需要它：`tools/gate.sh` 与本机 `cargo test` 跑的都是原生后端（Vulkan），
# GL/WebGL2 是**另一套语言后端**（naga → GLSL），有一整类错误只在它上面出现。
# 第 56 轮的线上事故就是这样：`quench3d-post` 对深度纹理用 `textureLoad`，naga 的
# GLSL 后端直接拒绝（`WGSL \`textureLoad\` from depth textures is not supported in GLSL`），
# 而 wasm 上管线校验错误是**致命**的 —— 编辑器启动即 panic，本地门禁全绿照样发得出去，
# 因为门禁里没有浏览器。本脚本把"真实 WebGL2"变成一条可复现命令。
#
# 判据（两条都要）：
#   * 绿：日志里出现 `编辑器第一帧已渲染`，且 DOM 里 `#overlay` 带 `hidden`
#         （遮罩只在第一帧真的画出来之后才收，页面不会"假装启动成功"）；
#   * 红：出现 `panicked at`（wasm 里 panic 了）。
#
# 用法：
#   tools/probe_webgl2.sh                                  # 缺省 ?backend=gl（强制 WebGL2）
#   tools/probe_webgl2.sh "index.html?backend=webgpu"       # 换后端
#   CHROME=/path/to/chrome PORT=8139 BUDGET=90000 tools/probe_webgl2.sh
#
# 依赖：python3 + google-chrome/chromium。没有 Chrome 时**跳过并返回 0**
# （与仓库里其它"要 GPU 的测试"一致：别把没有浏览器的机器弄红）。
#
# 注意：本脚本**不**取代 `tools/gate.sh`，也不取代真机浏览器验收 —— SwiftShader 是
# 软件光栅，能力集与真机 GPU（ANGLE/Intel 之类）仍有差别（例如 MSAA > 1 的行为）。
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 1

CHROME="${CHROME:-$(command -v google-chrome || command -v chromium || command -v chromium-browser || true)}"
if [[ -z "$CHROME" ]]; then
  echo "跳过：没有找到 google-chrome / chromium（这个探针要一个真实浏览器）"
  exit 0
fi

QUERY="${1:-index.html?backend=gl}"
PORT="${PORT:-8138}"
BUDGET="${BUDGET:-60000}"
LOG_DIR="${LOG_DIR:-target/tmp}"
LOG="$LOG_DIR/probe-webgl2.log"
mkdir -p "$LOG_DIR"

if [[ ! -f web/pkg/quench3d_web_bg.wasm ]]; then
  echo "跳过：web/pkg 里没有 wasm（先跑 tools/build_web.sh web_client）"
  exit 0
fi

# 静态服务 web/（Chrome 对 file:// 的 module/wasm 加载有额外限制，走 http 最省事）
( cd web && exec python3 -m http.server "$PORT" ) >/dev/null 2>&1 &
SRV=$!
trap 'kill "$SRV" 2>/dev/null' EXIT
sleep 1

"$CHROME" --headless=new --no-sandbox --disable-dev-shm-usage \
  --user-data-dir="$ROOT/target/chrome-profile" \
  --use-angle=swiftshader --enable-unsafe-swiftshader \
  --enable-logging=stderr --v=0 \
  --virtual-time-budget="$BUDGET" \
  --dump-dom "http://127.0.0.1:$PORT/$QUERY" >"$LOG" 2>&1
CHROME_EXIT=$?

overlay_hidden=0
grep -aq 'id="overlay" class="hidden"' "$LOG" && overlay_hidden=1
first_frame=0
grep -aq '编辑器第一帧已渲染' "$LOG" && first_frame=1
panicked=0
grep -aq 'panicked at' "$LOG" && panicked=1
# 第 67 轮的线上事故：alpha 管线的两个颜色目标 blend 不一致，
# WebGL2 没有 INDEPENDENT_BLEND，create_render_pipeline 直接报错。
# 这类错误有时只打日志、不一定 panic，所以单独判。
independent_blend=0
grep -aq 'INDEPENDENT_BLEND' "$LOG" && independent_blend=1
# 6 层正方形 D2 纹理被 GL 猜成 Cube、却按 D2Array 绑定；即使不致命也会污染日志。
cube_heuristic=0
grep -aq 'heuristics assumed that' "$LOG" && cube_heuristic=1

echo "== 探针：真实 WebGL2（$QUERY）=="
grep -ao '\[Quench3D\] 后端选择：[^"]*' "$LOG" | head -1
grep -ao '渲染设备就绪：[^"]*' "$LOG" | head -1
grep -ao '编辑器第一帧已渲染：[^"]*' "$LOG" | head -1
if [[ "$panicked" = 1 ]]; then
  echo "-- panic（前 3 行）--"
  grep -a -A 2 'panicked at' "$LOG" | head -6
fi

if [[ "$CHROME_EXIT" != 0 && "$first_frame" != 1 ]]; then
  echo "FAIL probe-webgl2（chrome 退出码 $CHROME_EXIT，且没有第一帧；完整日志 $LOG）" >&2
  exit 1
fi
if [[ "$panicked" = 1 ]]; then
  echo "FAIL probe-webgl2（wasm 里 panic 了；完整日志 $LOG）" >&2
  exit 1
fi
if [[ "$independent_blend" = 1 ]]; then
  echo "FAIL probe-webgl2（alpha 管线的颜色目标 blend 不一致，WebGL2 没有 INDEPENDENT_BLEND；完整日志 $LOG）" >&2
  exit 1
fi
if [[ "$cube_heuristic" = 1 ]]; then
  echo "FAIL probe-webgl2（6 层正方形纹理被 GL 猜成 Cube，却按 D2Array 绑定；完整日志 $LOG）" >&2
  exit 1
fi
if [[ "$first_frame" != 1 || "$overlay_hidden" != 1 ]]; then
  echo "FAIL probe-webgl2（没有看到第一帧 / 遮罩没收掉：first_frame=$first_frame overlay_hidden=$overlay_hidden；完整日志 $LOG）" >&2
  exit 1
fi
echo "PASS probe-webgl2（第一帧已渲染，遮罩已收；完整日志 $LOG）"
