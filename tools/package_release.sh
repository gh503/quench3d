#!/usr/bin/env bash
# 生成一份可复现的本地发行包（不发布、不联网）。
#
#   tools/package_release.sh                 # release 构建 + 打包到 dist/
#   tools/package_release.sh --skip-build    # 复用 target/release 的二进制
#   tools/package_release.sh --debug         # 调试二进制（门禁用）
#   tools/package_release.sh --dry-run       # 只打印计划，不构建、不落盘
#   tools/package_release.sh --out DIR       # 指定输出目录（必须不存在）
#   tools/package_release.sh --camera-worker-dir target/camera-worker/quench3d-camera # 附带冻结采集运行时
#
# 包内容：bin/quench3d、bin/quench3d-editor、web/pkg/*、README/CHANGELOG/LICENSE、
# tools/camera_mocap/{camera_mocap.py,requirements.txt,README.md}、
# manifest.json（版本/提交/文件大小）与 SHA256SUMS。wasm 额外生成 Brotli
# `.br` 供静态服务器协商；没有 wasm-opt，也不生成平台安装器。
set -euo pipefail
cd "$(dirname "$0")/.." || exit 1
ROOT=$(pwd)

OUT=""
PROFILE=release
SKIP_BUILD=0
DRY_RUN=0
CAMERA_WORKER_DIR=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out)
      OUT=${2:-}
      shift 2
      ;;
    --camera-worker-dir)
      CAMERA_WORKER_DIR=${2:?--camera-worker-dir 需要冻结程序目录}
      shift 2
      ;;
    --debug)
      PROFILE=debug
      shift
      ;;
    --release)
      PROFILE=release
      shift
      ;;
    --skip-build)
      SKIP_BUILD=1
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h | --help)
      sed -n '2,14p' "$0"
      exit 0
      ;;
    *)
      echo "未知参数：$1" >&2
      exit 2
      ;;
  esac
done
if [ -n "$OUT" ] && [[ "$OUT" != /* ]]; then
  OUT="$ROOT/$OUT"
fi

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
if [ -z "$version" ]; then
  echo "无法从 Cargo.toml 读出版本" >&2
  exit 1
fi
os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
dest=${OUT:-$ROOT/dist/quench3d-$version-$os-$arch}
commit=$(git rev-parse --short HEAD 2>/dev/null || true)
commit=${commit:-unknown}

printf 'quench3d 发行包计划\n'
printf '  版本：%s\n' "$version"
printf '  配置：%s\n' "$PROFILE"
printf '  平台：%s-%s\n' "$os" "$arch"
printf '  提交：%s\n' "$commit"
printf '  输出：%s\n' "$dest"
printf '  二进制：bin/quench3d、bin/quench3d-editor\n'
printf '  Web：web/pkg（wasm + JS + build_stamp + wasm.br）\n'
printf '  摄像头源码：tools/camera_mocap（采集脚本、requirements.txt、README.md）\n'
if [ -n "$CAMERA_WORKER_DIR" ]; then
  printf '  摄像头运行时：%s → bin/quench3d-camera、bin/_internal（含人体模型）\n' "$CAMERA_WORKER_DIR"
fi
printf '  文档：README.md、CHANGELOG.md、LICENSE-MIT、LICENSE-APACHE\n'
printf '  清单：manifest.json、SHA256SUMS\n'
if [ "$DRY_RUN" = "1" ]; then
  printf '（dry-run：不构建、不写文件）\n'
  exit 0
fi

if [ -e "$dest" ]; then
  echo "输出目录已存在，拒绝覆盖：$dest" >&2
  exit 1
fi
if [ ! -f web/pkg/quench3d_web_bg.wasm ] || [ ! -f web/pkg/quench3d_web.js ]; then
  echo "web/pkg 缺少 wasm/JS，先运行 tools/build_web.sh web_client" >&2
  exit 1
fi

camera_resources=(camera_mocap.py requirements.txt README.md)
for resource in "${camera_resources[@]}"; do
  if [ ! -f "$ROOT/tools/camera_mocap/$resource" ]; then
    echo "缺少摄像头发行资源：tools/camera_mocap/$resource" >&2
    exit 1
  fi
done

if [ -n "$CAMERA_WORKER_DIR" ]; then
  python3 - "$CAMERA_WORKER_DIR" <<'PY'
import json
from pathlib import Path
import sys
root = Path(sys.argv[1]).resolve()
import os
assert (root / "quench3d-camera").is_file() and os.access(root / "quench3d-camera", os.X_OK), "缺少可执行 quench3d-camera"
assert (root / "_internal").is_dir(), "缺少冻结运行时 _internal"
assert (root / "licenses/CPython-LICENSE.txt").is_file(), "缺少 Python 许可材料"
with (root / "camera-worker.json").open() as f:
    metadata = json.load(f)
assert metadata["format"] == "quench3d.camera-worker/1", metadata
assert metadata["platform"] == "linux-x86_64", metadata
assert os.uname().sysname == "Linux" and os.uname().machine == "x86_64", "摄像头运行时仅支持 Linux x86_64"
required_models = {
    "_internal/mediapipe/modules/pose_detection/pose_detection.tflite",
    "_internal/mediapipe/modules/pose_landmark/pose_landmark_full.tflite",
    "_internal/mediapipe/modules/pose_landmark/pose_landmark_cpu.binarypb",
}
assert set(metadata["models"]) == required_models, "人体模型清单不完整"
for rel, digest in metadata["models"].items():
    import hashlib
    assert hashlib.sha256((root / rel).read_bytes()).hexdigest() == digest, rel
assert metadata["models"], "缺少人体模型清单"
PY
fi

if [ "$SKIP_BUILD" = "0" ]; then
  if [ "$PROFILE" = "release" ]; then
    cargo build --locked --release -p quench3d-cli -p quench3d-editor --bins
  else
    cargo build --locked -p quench3d-cli -p quench3d-editor --bins
  fi
fi
target_dir=${CARGO_TARGET_DIR:-$ROOT/target}
case "$target_dir" in
  /*) ;;
  *) target_dir="$ROOT/$target_dir" ;;
esac
bin_dir=$target_dir/$PROFILE
for bin in quench3d quench3d-editor; do
  if [ ! -x "$bin_dir/$bin" ]; then
    echo "缺少可执行文件：$bin_dir/$bin（不要用 --skip-build）" >&2
    exit 1
  fi
done

mkdir -p "$dest/bin" "$dest/web/pkg" "$dest/tools/camera_mocap"
install -m 755 "$bin_dir/quench3d" "$dest/bin/quench3d"
install -m 755 "$bin_dir/quench3d-editor" "$dest/bin/quench3d-editor"
cp -a web/pkg/. "$dest/web/pkg/"
cp README.md CHANGELOG.md LICENSE-MIT LICENSE-APACHE "$dest/"
for resource in "${camera_resources[@]}"; do
  install -m 644 "$ROOT/tools/camera_mocap/$resource" "$dest/tools/camera_mocap/$resource"
done

if [ -n "$CAMERA_WORKER_DIR" ]; then
  # 只接收冻结程序的约定目录，避免把开发 venv 或任意顶层文件当作运行时。
  cp -a "$CAMERA_WORKER_DIR/quench3d-camera" "$dest/bin/"
  cp -a "$CAMERA_WORKER_DIR/_internal" "$dest/bin/"
  cp -a "$CAMERA_WORKER_DIR/camera-worker.json" "$dest/bin/"
  cp -a "$CAMERA_WORKER_DIR/licenses" "$dest/bin/"
fi

# 静态服务器可直接对 Accept-Encoding: br 返回同一路径的 `.br`（需配置
# `Content-Encoding: br` 与 `Content-Type: application/wasm`）。压缩在打包目录
# 内完成，不把派生文件写回 web/pkg，保证源树与门禁状态稳定。
python3 - "$dest/web/pkg/quench3d_web_bg.wasm" <<'PY'
import brotli
import os
import sys

path = sys.argv[1]
with open(path, "rb") as f:
    data = f.read()
packed = brotli.compress(data, quality=11)
if not packed or len(packed) >= len(data):
    raise SystemExit(f"Brotli 压缩无效：{len(data)} -> {len(packed)}")
with open(path + ".br", "wb") as f:
    f.write(packed)
print(f"brotli: {len(data)} -> {len(packed)} bytes ({len(packed) * 100 / len(data):.1f}%)")
PY

python3 - "$dest" "$version" "$commit" "$PROFILE" "$os" "$arch" <<'PY'
import hashlib
import json
import os
import sys

root, version, commit, profile, os_name, arch = sys.argv[1:7]
files = {}
for base, dirs, names in os.walk(root):
    dirs.sort()
    for name in sorted(names):
        path = os.path.join(base, name)
        rel = os.path.relpath(path, root).replace(os.sep, "/")
        if rel in {"manifest.json", "SHA256SUMS"}:
            continue
        with open(path, "rb") as f:
            data = f.read()
        files[rel] = {"bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()}
manifest = {
    "name": "quench3d",
    "version": version,
    "commit": commit,
    "profile": profile,
    "platform": f"{os_name}-{arch}",
    "files": files,
}
# 保留文件大小（check_package.sh 用它验清单与磁盘一致）；哈希也写进去方便审计。
sizes = {rel: info["bytes"] for rel, info in files.items()}
manifest["files"] = sizes
manifest["sha256"] = {rel: info["sha256"] for rel, info in files.items()}
with open(os.path.join(root, "manifest.json"), "w", encoding="utf-8") as f:
    json.dump(manifest, f, ensure_ascii=False, indent=2, sort_keys=True)
    f.write("\n")
print(f"manifest: {len(files)} files")
PY

(
  cd "$dest"
  find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum >SHA256SUMS
)

echo "PASS package: $dest"
