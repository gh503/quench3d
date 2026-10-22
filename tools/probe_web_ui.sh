#!/usr/bin/env bash
# 启动本地静态服务 + headless Chrome，再跑 probe_web_ui.mjs（CDP 按键 + 截图）。
# 没有 Chrome 时跳过并返回 0；真实 WebGL2 能力仍以 probe_webgl2.sh 为准。
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 1

CHROME="${CHROME:-$(command -v google-chrome || command -v chromium || command -v chromium-browser || true)}"
if [[ -z "$CHROME" ]]; then
  echo "跳过：没有找到 google-chrome / chromium（这个探针要一个真实浏览器）"
  exit 0
fi
if [[ ! -f web/pkg/quench3d_web_bg.wasm ]]; then
  echo "跳过：web/pkg 里没有 wasm（先跑 tools/build_web.sh web_client）"
  exit 0
fi
if ! command -v node >/dev/null 2>&1; then
  echo "跳过：没有 node（probe_web_ui.mjs 要 Node 22+ 的全局 WebSocket）"
  exit 0
fi

PORT="${PORT:-8140}"
CDP_PORT="${CDP_PORT:-9223}"
# 默认端口可能被别的会话/残留服务占着：换一个空闲端口，别让探针假红。
if ! python3 -c 'import socket,sys; s=socket.socket(); s.bind(("127.0.0.1", int(sys.argv[1])))' "$PORT" 2>/dev/null; then
  for candidate in $(seq 8141 8199); do
    if python3 -c 'import socket,sys; s=socket.socket(); s.bind(("127.0.0.1", int(sys.argv[1])))' "$candidate" 2>/dev/null; then
      PORT="$candidate"
      break
    fi
  done
fi
OUT_DIR="${OUT_DIR:-target/tmp/ui-probe}"
mkdir -p target/tmp
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"
PROFILE="$(mktemp -d /tmp/quench3d-ui-profile.XXXXXX)"
SRV_LOG="target/tmp/probe-web-ui-server.log"
CHROME_LOG="target/tmp/probe-web-ui-chrome.log"

( cd web && exec python3 -m http.server "$PORT" ) >"$SRV_LOG" 2>&1 &
SRV=$!
"$CHROME" --headless=new --no-sandbox --disable-dev-shm-usage \
  --user-data-dir="$PROFILE" \
  --use-angle=swiftshader --enable-unsafe-swiftshader \
  --remote-debugging-address=127.0.0.1 --remote-debugging-port="$CDP_PORT" \
  --window-size=1280,800 --force-device-scale-factor=1 \
  --enable-logging=stderr --v=0 \
  "http://127.0.0.1:$PORT/?backend=gl&ui_debug=1" >"$CHROME_LOG" 2>&1 &
CHROME_PID=$!
cleanup() {
  kill "$CHROME_PID" "$SRV" 2>/dev/null
  wait "$CHROME_PID" "$SRV" 2>/dev/null
  sleep 0.2
  rm -rf "$PROFILE" 2>/dev/null || true
}
trap cleanup EXIT

sleep 1
CDP_PORT="$CDP_PORT" OUT_DIR="$OUT_DIR" node tools/probe_web_ui.mjs
