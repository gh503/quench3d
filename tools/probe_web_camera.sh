#!/usr/bin/env bash
# Web 摄像头预览的端到端探针：Chrome 假摄像头 + `?camera=1` 自动开窗，
# 两张截图在预览区域必须有运动；没有 Chrome/Node/PIL 时跳过并返回 0。
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT" || exit 1

CHROME="${CHROME:-$(command -v google-chrome || command -v chromium || command -v chromium-browser || true)}"
[[ -z "$CHROME" ]] && { echo "跳过：没有 Chrome/Chromium"; exit 0; }
command -v node >/dev/null 2>&1 || { echo "跳过：没有 node"; exit 0; }
python3 -c 'import PIL' 2>/dev/null || { echo "跳过：没有 Python PIL"; exit 0; }
[[ -f web/pkg/quench3d_web_bg.wasm ]] || { echo "跳过：没有 web/pkg"; exit 0; }

PORT="${PORT:-8152}"
CDP_PORT="${CDP_PORT:-9232}"
OUT_DIR="${OUT_DIR:-target/tmp/web-camera-probe}"
PROFILE="$(mktemp -d /tmp/quench3d-cam-profile.XXXXXX)"
mkdir -p "$OUT_DIR"
rm -rf "$OUT_DIR"/* 2>/dev/null || true

(cd web && exec python3 -m http.server "$PORT") >target/tmp/probe-web-camera-server.log 2>&1 &
SRV=$!
"$CHROME" --headless=new --no-sandbox --disable-dev-shm-usage \
  --user-data-dir="$PROFILE" \
  --use-angle=swiftshader --enable-unsafe-swiftshader \
  --use-fake-device-for-media-stream --use-fake-ui-for-media-stream \
  --autoplay-policy=no-user-gesture-required \
  --remote-debugging-address=127.0.0.1 --remote-debugging-port="$CDP_PORT" \
  --window-size=1280,800 \
  --enable-logging=stderr --v=0 \
  about:blank >target/tmp/probe-web-camera-chrome.log 2>&1 &
CHROME_PID=$!
cleanup() {
  kill "$CHROME_PID" "$SRV" 2>/dev/null
  wait "$CHROME_PID" "$SRV" 2>/dev/null
  rm -rf "$PROFILE" 2>/dev/null || true
}
trap cleanup EXIT
sleep 2

CAMERA_URL="http://127.0.0.1:${PORT}/?backend=gl&ui_debug=1&camera=1" \
CDP_PORT="$CDP_PORT" OUT_DIR="$OUT_DIR" node tools/probe_web_camera.mjs || exit 1

python3 - "$OUT_DIR" <<'PY' || exit 1
import sys
from pathlib import Path
from PIL import Image, ImageChops
import numpy as np

out = Path(sys.argv[1])
a = Image.open(out / "camera-a.png").convert("RGB")
b = Image.open(out / "camera-b.png").convert("RGB")
diff = np.asarray(ImageChops.difference(a, b)).max(axis=2)
# 摄像头窗口默认在左上；取视频区域（避开窗口标题/按钮）比较两帧运动。
crop = diff[70:360, 30:430]
changed = int((crop > 24).sum())
print(f"摄像头预览区域运动像素：{changed} / {crop.size}")
if changed < 500:
    raise SystemExit("FAIL probe-web-camera：预览区域两帧没有足够运动（可能没拿到摄像头流）")
# 骨架用洋红画；假摄像头是绿色测试图，洋红只可能来自注入的骨架。
rgb = np.asarray(b).astype(np.int16)
magenta = (rgb[:, :, 0] > 180) & (rgb[:, :, 2] > 180) & (rgb[:, :, 1] < 130)
pose_pixels = int(magenta.sum())
print(f"外部姿态洋红像素：{pose_pixels}")
if pose_pixels < 200:
    raise SystemExit("FAIL probe-web-camera：camera_pose=true 但画面里找不到骨架颜色")
print("PASS probe-web-camera：摄像头预览两帧有运动 + 外部姿态真的画出来了")
PY
