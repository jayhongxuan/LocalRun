# LocalRun 本地 Agent 与媒体模块说明

## 实现边界

LocalRun 的 Agent 使用当前本地 llama.cpp 模型。绘图与视频通过用户选择的本机 `sd-cli` 执行，不调用云端生成服务。模型权重由用户自行选择并存放在本机；当前页面不会自动下载或安装引擎/权重。`run_command` 工具默认关闭，启用后每次执行都要求确认。

Agent 还提供可选的 `list_desktop_images` 工具：用户勾选「桌面图片」后，模型可以列出 `~/Desktop` 下的 png/jpg/jpeg/webp/gif/bmp，并将缩略图显示在聊天中。图片只用于本地 UI 展示，不会以 base64 回填给模型；当前本地模型没有视觉能力，工具结果明确提示不要猜测图片内容。

本实现依据公开的 stable-diffusion.cpp CLI 与模型文档，不使用 LocModel 私有服务接口。LocModel 二进制中观察到的会员、下单和激活功能不在本次范围内。

## 本地模型准备

图片页面支持单文件 GGUF、SafeTensors 或 CKPT checkpoint；也支持 Z-Image/Flux 这类分离式组合：diffusion 模型 + `--vae` + `--llm` 文本编码器（Z-Image 使用 Qwen3-4B）。视频页面使用 Wan 视频扩散模型、对应 VAE 和 T5XXL/UMT5 编码器，并提供四种明确的模型模式：

- `wan21_t2v`：Wan2.1 T2V，不使用首帧；
- `wan21_i2v`：Wan2.1 I2V，必须同时选择首帧和 `CLIP Vision`，分别传 `-i` 与 `--clip_vision`；
- `wan22_ti2v`：Wan2.2 TI2V 5B，使用 wan2.2 VAE，可选首帧，不需要 CLIP Vision；
- `wan22_a14b`：Wan2.2 A14B，必须选择 LowNoise 与 HighNoise 两个扩散模型，传 `--diffusion-model` 与 `--high-noise-diffusion-model`。

视频模式固定传 `-M vid_gen`。输出扩展名为 `.webm`；如果所选 `sd-cli` 不支持 WebM 封装，任务会失败并保留引擎日志。模型权重可能很大，实际可用性和生成速度取决于 Mac 内存、芯片、模型量化和扩散后端。

Mac 用户需选择与本机架构匹配的 `sd-cli` 可执行文件。首次获取引擎和模型仍需联网下载；下载完成后推理过程在本机执行。

## 官方资料

- stable-diffusion.cpp 项目主页：<https://github.com/leejet/stable-diffusion.cpp>。项目 README 列出 macOS 与 Metal 后端，也列出图像、Wan 与其他视频模型支持；README 同时提示项目 API/CLI 仍在活跃变化。
- macOS/Metal 构建说明：<https://github.com/leejet/stable-diffusion.cpp/blob/master/docs/build.md>。文档提供 `-DSD_METAL=ON` 的构建方法，并注明 Metal 在大矩阵计算上的性能仍有局限。
- Wan 视频模型说明：<https://github.com/leejet/stable-diffusion.cpp/blob/master/docs/wan.md>。包含模型/VAE/T5XXL 文件要求、`vid_gen` 模式及图生视频示例。
- CLI 参数说明：<https://github.com/leejet/stable-diffusion.cpp/blob/master/examples/cli/README.md> 与 <https://raw.githubusercontent.com/leejet/stable-diffusion.cpp/master/examples/cli/main.cpp>。
- 官方 Releases：<https://github.com/leejet/stable-diffusion.cpp/releases>。下载时应按 Mac 芯片架构选择对应构建，并遵循上游许可证和模型各自许可。

## 验证限制

当前代码已在 macOS 26.6.1 / Apple M1 Max 实机验证启动、Agent 工具循环、命令确认、桌面截图和画图（Z-Image）。视频已通过 GGUF Q8 T5 + CPU VAE 生成 WebM，但 4 steps/256×256/33 帧在本次 App 任务中约 87 分钟，速度是当前主要限制；详见 [FUNCTIONAL_TEST_REPORT.md](FUNCTIONAL_TEST_REPORT.md)。构建产物未签名/未公证。
