#!/usr/bin/env bash
# 冻结本地已安装的摄像头 Python 环境；不安装依赖、不下载模型、不访问摄像头。
# CAMERA_PYTHON=target/tmp/camvenv/bin/python CAMERA_FREEZER_TOOLS=target/tmp/camera-freezer-tools bash tools/build_camera_worker.sh
# Linux x86_64，需 PyInstaller 6.16.0；输出 target/camera-worker/quench3d-camera。
set -euo pipefail
cd "$(dirname "$0")/.." || exit 1
ROOT=$(pwd)
CAMERA_PYTHON=${CAMERA_PYTHON:-$ROOT/target/tmp/camvenv/bin/python}
CAMERA_FREEZER_TOOLS=${CAMERA_FREEZER_TOOLS:-$ROOT/target/tmp/camera-freezer-tools}
export PYTHONPATH="$CAMERA_FREEZER_TOOLS"
"$CAMERA_PYTHON" - <<'PY'
import importlib.metadata as md
import platform
assert platform.system() == 'Linux' and platform.machine() == 'x86_64', '仅验证 Linux x86_64'
assert md.version('pyinstaller') == '6.16.0', '需要 PyInstaller 6.16.0'
assert md.version('mediapipe') == '0.10.21', '需要已验证的 MediaPipe 0.10.21'
PY
"$CAMERA_PYTHON" -m PyInstaller --noconfirm --onedir --name quench3d-camera \
  --distpath target/camera-worker --workpath target/tmp/camera-freezer-build \
  --specpath target/tmp/camera-freezer-spec --collect-data mediapipe \
  --exclude-module jax --exclude-module jaxlib --exclude-module tensorflow \
  --exclude-module torch tools/camera_mocap/camera_mocap.py
"$CAMERA_PYTHON" - "$ROOT/target/camera-worker/quench3d-camera" <<'PY'
import hashlib
import importlib.metadata as md
import json
import platform
import shutil
import sys
from pathlib import Path
root = Path(sys.argv[1])
licenses = root / 'licenses'
licenses.mkdir(exist_ok=True)
# 保留已安装发行版的原始许可材料与版本。该 inventory 记录来源，不能代替各依赖许可审阅。
packages = {}
for dist in sorted(md.distributions(), key=lambda d: d.metadata['Name'].lower()):
    name = dist.metadata['Name']
    # 构建器本身不进入 worker；jax 等被显式排除。
    if name.lower() in {'pyinstaller', 'pyinstaller-hooks-contrib', 'altgraph', 'setuptools', 'pip', 'wheel', 'jax', 'jaxlib'}:
        continue
    copied = []
    for rel in dist.files or []:
        path = Path(str(rel))
        if not any(part.lower().startswith(('license', 'copying', 'notice')) for part in path.parts):
            continue
        source = Path(dist.locate_file(rel))
        if not source.is_file():
            continue
        # 去掉任何 parent 段，所有材料落在包内按 distribution 分组。
        safe = Path(*[p for p in path.parts if p not in {'.', '..'}])
        dest = licenses / name / safe
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, dest)
        copied.append(str(dest.relative_to(root)))
    packages[name] = {'version': dist.version, 'license_files': copied}
# Python 本体的许可文件来自构建解释器标准库。
python_license = Path(sys.base_prefix) / 'lib' / f'python{sys.version_info.major}.{sys.version_info.minor}' / 'LICENSE.txt'
if python_license.is_file():
    shutil.copyfile(python_license, licenses / 'CPython-LICENSE.txt')
else:
    raise SystemExit(f'缺少 CPython 许可材料：{python_license}')
models = {}
for rel in [
    '_internal/mediapipe/modules/pose_detection/pose_detection.tflite',
    '_internal/mediapipe/modules/pose_landmark/pose_landmark_full.tflite',
    '_internal/mediapipe/modules/pose_landmark/pose_landmark_cpu.binarypb',
]:
    models[rel] = hashlib.sha256((root / rel).read_bytes()).hexdigest()
metadata = {
    'format': 'quench3d.camera-worker/1', 'platform': 'linux-x86_64',
    'python': platform.python_version(), 'glibc_build': platform.libc_ver()[1],
    'freezer': {'pyinstaller': md.version('pyinstaller'), 'hooks': md.version('pyinstaller-hooks-contrib')},
    'models': models, 'installed_package_inventory': packages,
    'note': 'Inventory records build-environment distributions; not every listed module is included. Linux system libraries still required. MiDaS and lite/heavy pose models are not bundled.',
}
(root / 'camera-worker.json').write_text(json.dumps(metadata, indent=2, sort_keys=True) + '\n')
print(f'worker metadata: {len(models)} model/graph hashes; {len(packages)} package records')
PY
env -u PYTHONPATH -u PYTHONHOME "$ROOT/target/camera-worker/quench3d-camera/quench3d-camera" --self-test-pose
