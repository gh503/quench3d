#!/usr/bin/env bash
# 校验发行打包：dry-run 不落盘、debug 包结构/校验和/二进制可执行。
#
# 用法：bash tools/check_package.sh
# 需要已构建 target/debug 的 CLI/编辑器二进制；本脚本自己构建。
set -euo pipefail
cd "$(dirname "$0")/.." || exit 1
ROOT=$(pwd)

tmp=$(mktemp -d "${TMPDIR:-/tmp}/quench3d-package.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

dry=$(bash tools/package_release.sh --dry-run --out "$tmp/dry")
grep -q 'quench3d' <<<"$dry"
grep -q 'quench3d-editor' <<<"$dry"
grep -q 'web/pkg' <<<"$dry"
grep -q 'tools/camera_mocap' <<<"$dry"
if [ -e "$tmp/dry" ]; then
  echo "FAIL dry-run 不应创建输出目录" >&2
  exit 1
fi

cargo build --locked -p quench3d-cli -p quench3d-editor --bins

camera_args=()
if [ -n "${CAMERA_WORKER_DIR:-}" ]; then
  camera_args=(--camera-worker-dir "$CAMERA_WORKER_DIR")
fi
bash tools/package_release.sh --debug --skip-build "${camera_args[@]}" --out "$tmp/pkg"

test -x "$tmp/pkg/bin/quench3d"
test -x "$tmp/pkg/bin/quench3d-editor"
test -f "$tmp/pkg/web/pkg/quench3d_web_bg.wasm"
test -f "$tmp/pkg/web/pkg/quench3d_web.js"
test -f "$tmp/pkg/web/pkg/quench3d_web_bg.wasm.br"
test -f "$tmp/pkg/manifest.json"
test -f "$tmp/pkg/SHA256SUMS"
test -f "$tmp/pkg/README.md"
test -f "$tmp/pkg/LICENSE-MIT"
test -f "$tmp/pkg/LICENSE-APACHE"
for resource in camera_mocap.py requirements.txt README.md; do
  test -f "$tmp/pkg/tools/camera_mocap/$resource"
  cmp "tools/camera_mocap/$resource" "$tmp/pkg/tools/camera_mocap/$resource"
done

if [ -n "${CAMERA_WORKER_DIR:-}" ]; then
  test -x "$tmp/pkg/bin/quench3d-camera"
  test -d "$tmp/pkg/bin/_internal"
  test -f "$tmp/pkg/bin/camera-worker.json"
  test -d "$tmp/pkg/bin/licenses"
  env -u PYTHONPATH -u PYTHONHOME "$tmp/pkg/bin/quench3d-camera" --self-test-pose
fi

# 清单里的每个文件都必须存在，版本/提交/配置必须写对。
python3 - "$tmp/pkg" <<'PY'
import hashlib
import json
import os
import sys

root = sys.argv[1]
with open(os.path.join(root, "manifest.json"), encoding="utf-8") as f:
    manifest = json.load(f)
assert manifest["name"] == "quench3d", manifest
assert manifest["profile"] == "debug", manifest
assert manifest["version"] and manifest["version"] != "unknown", manifest
assert manifest["commit"], manifest
files = manifest["files"]
assert files, "清单不能为空"
assert any(p.startswith("bin/") for p in files), files
assert any(p.startswith("web/pkg/") for p in files), files
for resource in ("camera_mocap.py", "requirements.txt", "README.md"):
    rel = f"tools/camera_mocap/{resource}"
    assert rel in files, f"摄像头资源未进清单：{rel}"
    with open(os.path.join(root, rel), "rb") as f:
        digest = hashlib.sha256(f.read()).hexdigest()
    assert manifest["sha256"][rel] == digest, f"摄像头资源哈希不一致：{rel}"
for rel in files:
    path = os.path.join(root, rel)
    assert os.path.isfile(path), f"清单列了不存在的文件：{rel}"
    assert manifest["files"][rel] == os.path.getsize(path), rel
print(f"manifest: {len(files)} files")
PY

(
  cd "$tmp/pkg"
  sha256sum -c SHA256SUMS >/dev/null
)

# Brotli 产物必须真的能还原出同一份 wasm，而不是空文件/旧缓存。
python3 - "$tmp/pkg" <<'PY'
import brotli
import os
import sys

root = sys.argv[1]
with open(os.path.join(root, "web/pkg/quench3d_web_bg.wasm"), "rb") as f:
    wasm = f.read()
with open(os.path.join(root, "web/pkg/quench3d_web_bg.wasm.br"), "rb") as f:
    packed = f.read()
assert packed, "Brotli 文件为空"
assert len(packed) < len(wasm), f"Brotli 没有变小：{len(packed)} >= {len(wasm)}"
assert brotli.decompress(packed) == wasm, "Brotli 解压后与 wasm 不一致"
print(f"brotli: {len(wasm)} -> {len(packed)} bytes")
PY

"$tmp/pkg/bin/quench3d" --help >/dev/null
"$tmp/pkg/bin/quench3d" --version | grep -q 'quench3d'

echo "PASS package ($(find "$tmp/pkg" -type f | wc -l) files)"
