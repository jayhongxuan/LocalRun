# 在 Windows 10/11 x64 上构建 LocalRun NSIS 安装包。
# 需要：Rust stable（MSVC）、Visual Studio Build Tools C++、WebView2 Runtime（Win10/11 通常自带）。
$ErrorActionPreference = "Stop"

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "未找到 cargo。请先安装 Rust stable MSVC：https://rustup.rs/"
}

rustup target add x86_64-pc-windows-msvc
cargo install tauri-cli --version "^2" --locked

Push-Location (Join-Path $PSScriptRoot "src-tauri")
try {
    cargo tauri build --bundles nsis
} finally {
    Pop-Location
}

Write-Host ""
Write-Host "安装包通常位于："
Write-Host "  src-tauri\target\release\bundle\nsis\LocalRun_0.1.0_x64-setup.exe"
Write-Host "若文件名因 Tauri 版本不同而变化，请在上述 nsis 目录查找 .exe。"
