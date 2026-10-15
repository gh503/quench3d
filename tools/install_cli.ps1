# 安装 Quench3D 命令行工具（Windows / PowerShell）
#
#   powershell -ExecutionPolicy Bypass -File tools\install_cli.ps1
#   powershell -ExecutionPolicy Bypass -File tools\install_cli.ps1 -Local
#
# Linux / macOS 用 tools/install_cli.sh。
param(
    [switch]$Local
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "找不到 cargo。先装 Rust 1.82+：https://rustup.rs"
    exit 1
}
Write-Host "==> Rust: $(rustc --version)"

if ($Local) {
    Write-Host "==> 本地构建（release）"
    cargo build -p quench3d-cli --release
    $bin = Join-Path $root "target\release\quench3d.exe"
    Write-Host "==> 二进制：$bin"
    Write-Host "==> 自检："
    & $bin --version
    try { & $bin doctor } catch { }
    Write-Host ""
    Write-Host "提示：把 $root\target\release 加进 PATH 即可随处调用。"
    Write-Host "试一下："
    Write-Host "  & '$bin' example --out scene.json"
    Write-Host "  & '$bin' render scene.json --out scene.png"
    exit 0
}

Write-Host "==> 安装到 cargo bin 目录（默认 %USERPROFILE%\.cargo\bin）"
cargo install --path crates/quench3d-cli --force

Write-Host "==> 自检"
quench3d --version
try { quench3d doctor } catch { }

Write-Host ""
Write-Host "==> 装好了。三步走："
Write-Host "  quench3d schema --json                  # 让 agent 读能力清单"
Write-Host "  quench3d example --out scene.json       # 生成示例场景"
Write-Host "  quench3d render scene.json --out a.png  # 渲染出图"
Write-Host ""
Write-Host "想接大模型（本地 Ollama 为例）："
Write-Host "  ollama serve; ollama pull qwen2.5:7b"
Write-Host "  quench3d ask '斜坡上滚球' --out s.json --render s.png"
