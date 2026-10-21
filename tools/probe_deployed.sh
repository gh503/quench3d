#!/usr/bin/env bash
# 验证**真正部署出去的** GitHub Pages，而不是本地 `web/`：
#   * HTTP 200 且首页确实引用了 wasm；
#   * headless Chrome + SwiftShader 能启动 WebGL2、遮罩收起；
#   * 没有 panic / INDEPENDENT_BLEND / GL Cube 启发式日志。
#
# 用法：tools/probe_deployed.sh [URL]
#   URL 缺省取 https://gh503.github.io/quench3d/
# 网络不通/没有 Chrome 时**跳过并返回 0**，不把离线机器弄红。
set -uo pipefail

URL="${1:-https://gh503.github.io/quench3d/}"
CHROME="${CHROME:-$(command -v google-chrome || command -v chromium || command -v chromium-browser || true)}"
LOG_DIR="${LOG_DIR:-target/tmp}"
LOG="$LOG_DIR/probe-deployed.log"
BUDGET="${BUDGET:-30000}"
mkdir -p "$LOG_DIR"

if [[ -z "$CHROME" ]]; then
  echo "跳过：没有找到 google-chrome / chromium"
  exit 0
fi
if ! curl -fsS --max-time 20 "$URL" >"$LOG_DIR/deployed-index.html"; then
  echo "跳过：拉不到 $URL（网络或 Pages 尚未就绪）"
  exit 0
fi
if ! grep -q 'quench3d' "$LOG_DIR/deployed-index.html"; then
  echo "FAIL probe-deployed：$URL 返回的页面里没有 quench3d"
  exit 1
fi

PROFILE="$(mktemp -d "${TMPDIR:-/tmp}/quench3d-deployed-profile.XXXXXX")"
trap 'rm -rf "$PROFILE" 2>/dev/null || true' EXIT
"$CHROME" --headless=new --no-sandbox --disable-dev-shm-usage \
  --user-data-dir="$PROFILE" \
  --use-angle=swiftshader --enable-unsafe-swiftshader \
  --enable-logging=stderr --v=0 \
  --virtual-time-budget="$BUDGET" \
  --dump-dom "${URL}?backend=gl&deployed_probe=1" >"$LOG" 2>&1
CHROME_EXIT=$?

overlay_hidden=0
grep -aq 'id="overlay" class="hidden"' "$LOG" && overlay_hidden=1
first_frame=0
grep -aq '编辑器第一帧已渲染' "$LOG" && first_frame=1
panicked=0
grep -aq 'panicked at' "$LOG" && panicked=1
independent_blend=0
grep -aq 'INDEPENDENT_BLEND' "$LOG" && independent_blend=1
cube_heuristic=0
grep -aq 'heuristics assumed that' "$LOG" && cube_heuristic=1

echo "== 探针：已部署 Pages（$URL）=="
grep -ao '\[Quench3D\] 后端选择：[^"]*' "$LOG" | head -1
grep -ao '渲染设备就绪：[^"]*' "$LOG" | head -1
grep -ao '编辑器第一帧已渲染：[^"]*' "$LOG" | head -1

if [[ "$CHROME_EXIT" -ne 0 || "$overlay_hidden" -ne 1 || "$first_frame" -ne 1 || \
      "$panicked" -ne 0 || "$independent_blend" -ne 0 || "$cube_heuristic" -ne 0 ]]; then
  echo "FAIL probe-deployed（chrome=$CHROME_EXIT overlay=$overlay_hidden frame=$first_frame panic=$panicked blend=$independent_blend cube=$cube_heuristic）"
  echo "完整日志：$LOG"
  exit 1
fi
echo "PASS probe-deployed（线上第一帧已渲染，遮罩已收；完整日志 $LOG）"
