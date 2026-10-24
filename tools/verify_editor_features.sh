#!/usr/bin/env bash
# 编辑器近期功能的正向验证入口。
#
# 只跑与"数据/参数/画面真的变了"直接相关的用例；全量门禁用 tools/gate.sh。
# 需要 GPU 的用例通过 QUENCH3D_GPU_TESTS=1 打开；没有可用适配器时用例会打印
# "跳过"并返回成功（与 CI 的无 GPU runner 行为一致）。
set -u
cd "$(dirname "$0")/.."

# 本机离线工具链的位置（与 tools/gate.sh 使用同一套环境）。外部已经设了就不覆盖。
DEV_ENV="${QUENCH3D_DEV_ENV:-/home/gh503/Documents/infra-dev}"
if [ -d "$DEV_ENV/.cargo-home" ] && [ -z "${CARGO_HOME:-}" ]; then
    export CARGO_HOME="$DEV_ENV/.cargo-home"
    export RUSTUP_HOME="$DEV_ENV/.rustup"
    export PATH="$CARGO_HOME/bin:$PATH"
fi

export QUENCH3D_GPU_TESTS=1
export CARGO_NET_OFFLINE=true
fail=0

run() {
    local title="$1"
    shift
    echo "== $title =="
    if "$@"; then
        echo "PASS $title"
    else
        echo "FAIL $title"
        fail=1
    fi
}

run "编辑器 UI 绘制/交互" \
    cargo test -p quench3d-editor --test ui_smoke --offline
run "贴图应用/清除/资源搜索路径" \
    cargo test -p quench3d-editor --lib texture_edit --offline
run "画中画回读/节流/统计保持" \
    cargo test -p quench3d-editor --lib preview_camera_tests --offline
run "材质 UV 变换像素级" \
    cargo test -p quench3d-render --test material_pipeline_traits uv_offset --offline
run "灯光 gizmo 线段" \
    cargo test -p quench3d --lib light_debug_lines --offline

if [ "$fail" -ne 0 ]; then
    echo "未全部通过 ❌"
    exit 1
fi
echo "全部通过 ✅"
