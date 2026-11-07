# LocalRun 0.1.0 Windows 安装包构建报告（在 macOS 上交叉编译）

构建时间：2026-11-04
产物目录：`installer/dist/`

作者 / 版权持有人：**大山**

| 文件 | 大小 | SHA-256 |
| --- | --- | --- |
| `LocalRun_0.1.0_x64-setup.exe` | 4,649,937 B (4.4 MiB) | `e329c0f569edab2f68bb8135efd0a04865602fb4d887d49be68355f6273e7174` |
| `LocalRun_0.1.0_windows-x64_portable.zip` | 3,736,018 B (3.6 MiB) | `43b26c083ec4a5fe6760d0fff250b7fcb4606dfe78b519a8a398db9de38aee29` |

> 与本仓库 `WINDOWS_BUILD.md` 原结论（"macOS 不能产出 Windows 安装包"）不同：本次在 Apple Silicon macOS 上完成了真实交叉编译，产物为正版 x86-64 PE 与 NSIS 安装器，非占位文件。但**尚未在 Windows 实机运行验证**。

## 产物静态校验结果

应用本体 `localrun.exe`：

- `PE32+ executable (GUI) x86-64`（Machine = 0x8664，Subsystem = 2 GUI），11,459,072 B
- 段表含 `.text/.rdata/.data/.pdata/.tls/.rsrc/.reloc`
- 资源：`RT_ICON × 7`、`RT_GROUP_ICON × 1`、`RT_MANIFEST × 1`、`RT_VERSION × 1`
  （版本信息 ProductName=LocalRun、ProductVersion=0.1.0、CompanyName=大山、
  LegalCopyright=`Copyright © 2026 大山 (Dashan). All rights reserved.`；
  清单由 llvm-rc 嵌入，DPI 感知/通用控件 v6 生效）
- 导入表 29 个 DLL，全部为系统组件：`kernel32/advapi32/user32/gdi32/combase/ole32/shell32/comctl32/dwmapi/imm32/ws2_32/pdh/powrprof/psapi/secur32/crypt32/shlwapi/oleaut32/ntdll/api-ms-win-crt-*` 等
  **不依赖 vcruntime140.dll 等 VC++ 可再发行组件**（UCRT 是 Windows 10+ 内置）
- 无 `WebView2Loader.dll` 外部依赖（Tauri 2 静态链接 WebView2 加载器）

安装器 `LocalRun_0.1.0_x64-setup.exe`：

- NSIS 3.12 Unicode，`lzma /SOLID` 压缩（13.6 MB 原始 → 4.4 MiB）
- 资源：`RT_DIALOG × 12`、`RT_ICON × 7`、`RT_GROUP_ICON`、`RT_MANIFEST`、`RT_VERSION 0.1.0`
- 内嵌：`LocalRun.exe`（11.4 MB）、`MicrosoftEdgeWebview2Setup.exe`（1.9 MB，Evergreen 引导器）、应用图标

## 署名与版权（作者：大山）

| 位置 | 内容 |
| --- | --- |
| 应用本体 `LocalRun.exe` 版本资源 | `CompanyName=大山`、`LegalCopyright=Copyright © 2026 大山 (Dashan). All rights reserved.` |
| 安装器 `setup.exe` 版本资源（2052/1033 双语） | `CompanyName=大山`、`Author=大山`、`LegalCopyright` 同上、`Comments=作者：大山　Copyright © 2026 …` |
| 安装向导欢迎页 | 「安装 LocalRun 0.1.0／作者：大山／版权行」 |
| 控制面板卸载项 | `Publisher=大山` |
| 应用界面左下角 | `v0.1.0 · 大山` |
| 绿色版 `使用说明.txt` | 作者与版权声明 |

来源：`src-tauri/tauri.conf.json` 的 `bundle.publisher` / `bundle.copyright`（经 `tauri-build` 写入 exe 版本资源）
与 `installer/localrun.nsi` 的 `APP_AUTHOR` / `APP_PUBLISHER` / `APP_COPYRIGHT` 宏。

## 安装器行为

- **用户级安装**：默认目录 `%LOCALAPPDATA%\LocalRun`，`RequestExecutionLevel user`，**不弹 UAC**
- 安装中先 `taskkill` 掉正在运行的实例，避免文件占用
- 检测 WebView2 运行时（HKLM 32/64 位 + HKCU 三处注册表），缺失时用内嵌引导器 `/silent /install` 联网安装；用户取消则中止
- 快捷方式：开始菜单 `LocalRun\LocalRun`、`卸载 LocalRun`；桌面快捷方式（组件页可取消勾选）
- 卸载信息写入 `HKCU\...\Uninstall\LocalRun`（含 `QuietUninstallString`），控制面板可见
- 界面语言：简体中文优先 + English（跟随系统语言）
- 卸载：结束进程、删快捷方式、删文件、清注册表

## 交叉编译链路（本次实际使用）

1. **Rust Windows 目标**：`rustup target add x86_64-pc-windows-msvc`
2. **链接器与 MSVC 运行库**：`cargo install cargo-xwin`（首次运行下载 MSVC CRT+SDK，约 2 GB 缓存于 `~/Library/Caches/cargo-xwin`）
3. **资源编译器 `llvm-rc`**：macOS 无此工具，`tauri-build → embed-resource` 会直接报 `NotAttempted("llvm-rc")`。
   取 conda-forge 的 osx-arm64 包手工解出（`llvm-tools-23` + `libllvm23` + `zstd` + `libiconv`），
   并对 `libLLVM.23.1.dylib` 用 `install_name_tool -change @rpath/libxml2.16.dylib /usr/lib/libxml2.2.dylib`
   + `codesign --force --sign -` 修掉 macOS 系统 libxml2 版本差；随后以环境变量 `RC=<path>/llvm-rc-23` 指定给 embed-resource
4. **crates.io 加速**：网络环境下载 crates 极慢，项目内新增 `src-tauri/.cargo/config.toml` 指向 rsproxy 稀疏镜像
5. **应用编译**：`cargo tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --no-bundle`
   （前端是静态 `ui/`，无 npm 步骤；`tauri.conf.json` 的 `frontendDist` 直接内嵌）
6. **NSIS 工具链**：macOS 无 makensis，用 Debian 源中的 NSIS 3.12 源码 `scons SKIPSTUBS=all SKIPPLUGINS=all SKIPUTILS=all SKIPMISC=all install-compiler`
   编译出原生 arm64 makensis，数据文件（Stubs/Include/Plugins/Contrib）取自 `nsis-common_3.12-1_all.deb`
7. **打包**：`installer/localrun.nsi` 由上述 makensis 编译产出安装器

一键重建脚本：`installer/build-windows-on-macos.sh`（需已具备上述第 1–3、6 步的工具链）。

## 已知限制

1. **未在 Windows 实机验证**：启动、WebView2 加载、模型路径（`%USERPROFILE%\Models`）、命令确认、画图/视频调用均需在 Win10/11 x64 上回归。
2. **未签名**：SmartScreen 会提示"未知发布者"，需用户点"更多信息 → 仍要运行"。若需消除，需购买代码签名证书并 `signtool` 签名（本机无法完成）。
3. 安装包**只含 x64**；32 位与 Windows ARM64 未构建。
4. 媒体功能仍依赖用户自备 `sd-cli`；Windows 走 zip 解压路径而非 macOS 的 dylib 符号链接逻辑（源码已按 `cfg(windows)` 处理，但未实机验证）。
5. `llvm-rc` 通过重定向 libxml2 依赖运行，仅用于资源编译，不参与产物运行时行为。
