# LocalRun

作者 / 维护者：**Jayhongxuan (大山)**

LocalRun 是基于 Tauri 2、Rust 和原生 HTML/CSS/JavaScript 的本地模型管理器。此源码分支补入 Agent、图片生成和视频生成入口，并修复若干 macOS 路径与硬件识别问题。

## 新增功能

- **Agent**：使用当前本地 llama.cpp 模型进行多轮工具调用。读文件可离线使用；联网搜索/读网页只有在用户主动勾选后才访问网络；shell 默认关闭，启用后每次命令执行前要求确认；勾选「桌面图片」后可在对话中列出并显示桌面截图（本地模型无视觉能力，仅展示给用户）。
- **画图**：调用用户在界面中选择的本地 stable-diffusion.cpp `sd-cli` 与本地扩散模型，生成 PNG。
- **视频**：调用本地 `sd-cli` 与 Wan 扩散模型、VAE、T5XXL 权重生成 WebM；可选首帧图。任务支持取消、查看日志和定位输出文件。

推理不调用云端生成 API。首次准备引擎/模型仍需用户自行下载对应的公开程序和权重；当前页面负责本地路径配置与生成，不内置媒体引擎或模型下载器。权重很大，兼容性、显存/统一内存需求及速度取决于具体模型和 Mac 机型。官方资料与参数说明见 [IMPLEMENTATION_NOTES.md](IMPLEMENTATION_NOTES.md)。

## 在 Mac 上构建

建议在目标 Mac 上构建，以匹配其 Apple Silicon/Intel 架构和系统签名环境。

```bash
# 安装 Rust stable 与 Xcode Command Line Tools
xcode-select --install

# 在项目根目录
cd src-tauri
cargo install tauri-cli --version '^2' --locked
cargo tauri dev

# 正式打包 .app 与 .dmg
cargo tauri build --bundles dmg
```

仓库中的 `.github/workflows/macos-build.yml` 也可在 GitHub Actions 的 macOS runner 上构建 `.app` 与 `.dmg`。本项目不使用 npm 前端构建步骤。

## 首次使用媒体功能

1. 从 stable-diffusion.cpp 官方 Releases 下载与本机 Mac 架构匹配的 `sd-cli`，在“画图”或“视频”页选择它。
2. 选择图片模型：
   - 单文件 SD/SDXL checkpoint：只选“图片模型”；
   - Z-Image 等分离式模型：同时选择 diffusion 模型、VAE（`ae.safetensors`）和 Qwen3-4B LLM 文本编码器。
3. 视频页还需选择兼容的 Wan 扩散模型、VAE、T5XXL/UMT5，并按模式选择额外文件：
   - Wan2.1 I2V：首帧图片 + `CLIP Vision`（内部传 `--clip_vision`）；
   - Wan2.2 A14B：LowNoise 扩散模型 + HighNoise 扩散模型（内部传 `--high-noise-diffusion-model`）；
   - Wan2.2 TI2V 5B：使用 wan2.2 VAE，可选首帧，不需要 CLIP Vision。
4. 输入提示词并生成。文件默认保存在 LocalRun 应用数据目录的 `media/output` 下；视频要求 `sd-cli` 能输出 WebM，失败时任务卡片会显示日志和错误。完成后可在任务卡片中定位输出。

本地推理不会上传提示词或权重。首次下载引擎/模型本身需要网络；设置后可以离线运行。

## 验证说明

当前源码已在 macOS 26.6.1 / Apple M1 Max 实机通过 `node --check`、`cargo check`、13 项 Rust 单元测试、启动与页面导航，并生成 `.app` 与 `.dmg`。画图已用本机 Z-Image 权重生成 PNG；视频已通过 GGUF Q8 T5 + CPU VAE 生成 WebM，但 4 steps/256×256/33 帧单次约 87 分钟，速度很慢。DMG 未签名、未公证。详细证据、路径和剩余限制见 [MAC_BUILD_REPORT.md](MAC_BUILD_REPORT.md) 与 [FUNCTIONAL_TEST_REPORT.md](FUNCTIONAL_TEST_REPORT.md)。

## Windows 安装包

项目已提供 Windows x64 的 NSIS 安装器和便携版：

- `installer/dist/LocalRun_0.1.0_x64-setup.exe`
- `installer/dist/LocalRun_0.1.0_windows-x64_portable.zip`

该安装器由 macOS 交叉编译链路生成，**尚未在 Windows 实机运行验证，也未签名**；Windows SmartScreen 可能提示未知发布者。构建和校验细节见 [WINDOWS_CROSSBUILD_REPORT.md](WINDOWS_CROSSBUILD_REPORT.md)，Windows 本机/CI 构建方式见 [WINDOWS_BUILD.md](WINDOWS_BUILD.md)。

## 许可证

本项目使用 [MIT License](LICENSE)，Copyright (c) 2026 Jayhongxuan (大山)。

欢迎通过 Issue / Pull Request 提交优化。Windows 安装器目前是未签名、未在 Windows 实机验证的构建产物。
