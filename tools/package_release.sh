#!/usr/bin/env bash
# 生成一份可复现的本地发行包（不发布、不联网）。
#
#   tools/package_release.sh                 # release 构建 + 打包到 dist/
#   tools/package_release.sh --skip-build    # 复用 target/release 的二进制
#   tools/package_release.sh --debug         # 调试二进制（门禁用）
#   tools/package_release.sh --dry-run       # 只打印计划，不构建、不落盘
#   tools/package_release.sh --out DIR       # 指定输出目录（必须不存在）
#
# 包内容：bin/quench3d、bin/quench3d-editor、web/pkg/*、README/CHANGELOG/LICENSE、
# manifest.json（版本/提交/文件大小）与 SHA256SUMS。wasm 额外生成 Brotli
# `.br` 供静态服务器协商；没有 wasm-opt，也不生成平台安装器。
set -euo pipefail
cd "$(dirname "$0")/.." || exit 1
ROOT=$(pwd)

OUT=""
PROFILE=release
SKIP_BUILD=0
DRY_RUN=0
while [ $# -gt 0 ]; do
  case "$1" in
    --out)
      OUT=${2:-}
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

if [ "$SKIP_BUILD" = "0" ]; then
  if [ "$PROFILE" = "release" ]; then
    cargo build --locked --release -p quench3d-cli -p quench3d-editor --bins
  else
    cargo build --locked -p quench3d-cli -p quench3d-editor --bins
  fi
fi
bin_dir=$ROOT/target/$PROFILE
for bin in quench3d quench3d-editor; do
  if [ ! -x "$bin_dir/$bin" ]; then
    echo "缺少可执行文件：$bin_dir/$bin（不要用 --skip-build）" >&2
    exit 1
  fi
done

mkdir -p "$dest/bin" "$dest/web/pkg"
install -m 755 "$bin_dir/quench3d" "$dest/bin/quench3d"
install -m 755 "$bin_dir/quench3d-editor" "$dest/bin/quench3d-editor"
cp -a web/pkg/. "$dest/web/pkg/"
cp README.md CHANGELOG.md LICENSE-MIT LICENSE-APACHE "$dest/"

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
