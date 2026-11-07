#!/usr/bin/env bash
# 在 macOS（Apple Silicon 已验证）上交叉编译 LocalRun 的 Windows x64 安装包。
# 前置（只需一次，详见 WINDOWS_CROSSBUILD_REPORT.md）：
#   1) rustup target add x86_64-pc-windows-msvc
#   2) cargo install cargo-xwin
#   3) 可用的 llvm-rc（macOS 无自带）：conda-forge llvm-tools-23 解包，或自建，挂到 PATH
#   4) 原生 makensis：由 NSIS 3.12 源码 POSIX 构建（SKIPSTUBS/SKIPPLUGINS/SKIPUTILS/SKIPMISC）
# 用法：
#   LLVM_RC=/path/to/llvm-rc MAKENSIS=/path/to/makensis ./installer/build-windows-on-macos.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="x86_64-pc-windows-msvc"
LLVM_RC="${LLVM_RC:-llvm-rc}"
MAKENSIS="${MAKENSIS:-makensis}"

command -v cargo-xwin >/dev/null || { echo "缺少 cargo-xwin：cargo install cargo-xwin"; exit 1; }
command -v "$LLVM_RC" >/dev/null || { echo "缺少 llvm-rc：设置 LLVM_RC=<路径>"; exit 1; }
command -v "$MAKENSIS" >/dev/null || { echo "缺少 makensis：设置 MAKENSIS=<路径>"; exit 1; }

echo "==> 1/3 交叉编译应用本体"
cd "$ROOT/src-tauri"
RC="$LLVM_RC" cargo tauri build --runner cargo-xwin --target "$TARGET" --no-bundle

EXE="$ROOT/src-tauri/target/$TARGET/release/localrun.exe"
[ -f "$EXE" ] || { echo "未找到 $EXE"; exit 1; }

echo "==> 2/3 准备打包载荷"
cd "$ROOT/installer"
mkdir -p payload dist
cp "$EXE" payload/LocalRun.exe
cp "$ROOT/src-tauri/icons/icon.ico" payload/icon.ico
if [ ! -f payload/MicrosoftEdgeWebview2Setup.exe ]; then
  echo "    下载 WebView2 Evergreen 引导器…"
  curl -sSL -o payload/MicrosoftEdgeWebview2Setup.exe "https://go.microsoft.com/fwlink/p/?LinkId=2124703"
fi

echo "==> 3/3 生成 NSIS 安装包"
"$MAKENSIS" localrun.nsi

echo
echo "完成："
ls -la dist/
shasum -a 256 dist/*.exe
