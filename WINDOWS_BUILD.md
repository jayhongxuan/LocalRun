# LocalRun Windows 安装包构建说明

> **2026-11-04 更新**：本机（Apple Silicon macOS）已用 `cargo-xwin` + 原生 `makensis` 完成真实交叉编译，
> 产物见 `installer/dist/LocalRun_0.1.0_x64-setup.exe`（NSIS 安装器）与便携版 zip。
> 完整链路、校验结果与限制见 [`WINDOWS_CROSSBUILD_REPORT.md`](WINDOWS_CROSSBUILD_REPORT.md)。
> 一键重建：`./installer/build-windows-on-macos.sh`。下文"方式 A/B"仍适用，作为 Windows 原生/CI 构建的备选路径。

## 为什么不能直接在这台 Mac 上生成（历史说明，已被上述交叉编译绕过）

Tauri 2 的 Windows NSIS/MSI 打包依赖 Windows 工具链；当前项目在 macOS 上只能生成 `.app`/`.dmg`。
从 macOS 直接交叉编译 Windows 安装包不属于 Tauri 官方支持路径，缺少 Windows SDK、WebView2 打包工具和 NSIS 工具链，不能把未验证的产物当作 Windows 安装包交付。

## 方式 A：Windows 10/11 x64 本机构建

前置：

- Windows 10/11 x64
- [Rust stable MSVC](https://rustup.rs/)
- Visual Studio Build Tools（Desktop development with C++）
- WebView2 Runtime（Windows 10/11 通常已带）

在项目根目录打开 PowerShell：

```powershell
Set-ExecutionPolicy -Scope Process Bypass
.\build-windows.ps1
```

产物通常为：

```text
src-tauri\target\release\bundle\nsis\LocalRun_0.1.0_x64-setup.exe
```

## 方式 B：GitHub Actions（无需本机 Windows）

项目已提供 `.github/workflows/windows-build.yml`。把项目放入 GitHub 仓库后：

1. 打开 Actions → `build-windows`；
2. 点击 `Run workflow`；
3. 下载 `LocalRun-Windows` artifact。

该工作流只构建 NSIS `.exe`，不签名、不公证、不发布 Release。

## 当前 Windows 构建的注意事项

- 安装包未签名，Windows SmartScreen 可能提示未知发布者。
- 应用内推理仍使用用户自行选择的本地 llama.cpp；媒体功能使用用户自行选择的 `sd-cli`。
- Windows 默认模型目录为 `%USERPROFILE%\Models`，可在设置页修改。
- Mac 上已修复的 macOS dylib 符号链接逻辑不适用于 Windows；Windows 引擎安装走 zip 路径。
- 本次没有在 Windows 实机上运行该安装包；需要 Windows 环境做启动、模型加载、命令确认、画图和视频的回归验证。
