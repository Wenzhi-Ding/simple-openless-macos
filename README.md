# OpenLess — macOS 纯本地语音输入

简体中文 | [English](README.en.md)

OpenLess 是一款 macOS（Apple Silicon）纯本地语音输入工具：按下热键说话，松开后语音被识别、轻度润色，并自动输入到当前光标位置。语音识别与文本润色全部在本机完成，不依赖任何云服务，也不上传音频或文本。

本项目基于 [Open-Less/openless](https://github.com/Open-Less/openless)（AGPL-3.0）精简而来，只保留 macOS arm64 桌面听写这一条产品线；移动端与云服务相关代码已删除。模型权重不随仓库分发：首次使用时在应用内引导下载，或按[获取模型](#获取模型)手动放置。

## 功能特性

- **语音识别**：Qwen3-ASR 1.7B（MLX 格式），启动时预加载，支持中文、英文等多语言识别（完整语言列表见模型 `config.json` 的 `support_languages`）。
- **文本润色**：Gemma 4 E4B（llama.cpp，Q4_0 量化）。只执行代码内置的一套固定轻度润色模板（`crates/openless-core/src/prompts.rs`）：保留原意、语气、语言和数字写法，仅去除无意义口癖重复、补标点、纠正明显错字；不翻译、不扩写、不回答或执行正文。没有风格、模式或模型选择入口。
- **手动词表**：用户手动添加专名词条，同时用于识别后的同音纠正和润色参考；应用不会从历史或屏幕内容自动发现新词。
- **光标与屏幕上下文**：听写时利用本地光标位置与屏幕 OCR 文字（RapidOCR，本机推理）纠正专名。OCR 拒绝密码框、SecureInput 与敏感应用；截图仅在内存中处理，不保存。上下文固定启用，但受权限与敏感内容门控保护。
- **历史与回放**：听写历史与录音回放保存在本机，可对历史条目重新润色。
- **纯本地边界**：无云识别、云润色、遥测或更新服务；唯一的联网动作是首次使用时应用内引导下载模型（下载源与 SHA-256 校验值钉死在代码里），识别/润色推理不建立任何网络连接；ASR worker 进程内直接禁用全部联网 socket（fail closed），有测试保证该边界不被回归。

### 已知限制

- 固定润色模板不等于模型保真保证：真实 E4B 测试中，阿拉伯数字、英文、中英混合等样例通过，但「明天十点」仍会被写成「明天10点」（数字写法漂移）。
- 仅支持 macOS arm64（Apple Silicon）；不支持 Intel Mac、Windows、Linux、Android 或 iOS。
- 不提供预编译发行包，需从源码构建。

## 系统要求

- Apple Silicon Mac，macOS 14 及以上
- Xcode 与 Metal 工具链（`xcodebuild -downloadComponent MetalToolchain`）
- Rust（rustup）、Node.js/npm（Homebrew 版本）、CMake

## 获取模型

应用从数据目录 `~/Library/Application Support/OpenLess/` 加载两个固定模型，无模型选择或回退入口；识别与润色完全离线。

### 方式一：应用内引导下载（推荐）

首次使用时，概览页的模型状态卡显示「未找到」，点击「获取模型」即可在应用内下载：

- **语音识别 Qwen3-ASR 1.7B（MLX）**：可选全精度 bf16（约 4.1 GB，作者验证基准）或 4bit 量化（约 1.6 GB，下载更快）；
- **文本润色 Gemma 4 E4B（GGUF）**：约 4.6 GB；
- 下载源钉死固定 revision 并逐文件 SHA-256 校验，支持断点续传与取消；国内网络可在「下载源」切换社区镜像 hf-mirror.com；
- 下载完成后应用自动重预载引擎，无需重启。

### 方式二：手动放置

偏好自行下载时，将权重放到数据目录对应位置后重启应用（与方式一相同的公开来源）。

**Qwen3-ASR 1.7B（MLX 格式）** → `~/Library/Application Support/OpenLess/models/qwen3-asr-1.7b/`，目录内需包含 `config.json`、`*.safetensors` 权重与 tokenizer 词表文件。来源（任选其一）：

- [`mlx-community/Qwen3-ASR-1.7B-bf16`](https://huggingface.co/mlx-community/Qwen3-ASR-1.7B-bf16)（约 4.1 GB，应用内下载所用版本）或 [`...-4bit`](https://huggingface.co/mlx-community/Qwen3-ASR-1.7B-4bit)（约 1.6 GB）；
- [`moona3k/mlx-qwen3-asr-1.7b-4bit`](https://huggingface.co/moona3k/mlx-qwen3-asr-1.7b-4bit) / `...-8bit`（加载器作者发布的预转换版）；
- 从官方权重 [`Qwen/Qwen3-ASR-1.7B`](https://huggingface.co/Qwen/Qwen3-ASR-1.7B) 用 [mlx-qwen3-asr](https://github.com/moona3k/mlx-qwen3-asr)（Apache-2.0，运行时锁定 0.4.0 版）的 `scripts/convert.py` 自行转换。

**Gemma 4 E4B（GGUF）** → `~/Library/Application Support/OpenLess/models/llm/gemma-4-E4B-it-Q4_0.gguf`。

来源：[ggml-org/gemma-4-E4B-it-GGUF](https://huggingface.co/ggml-org/gemma-4-E4B-it-GGUF/tree/b8093469224f83f5c38f691eb906c380e9e63114)，固定 revision `b8093469224f83f5c38f691eb906c380e9e63114`，文件 `gemma-4-E4B-it-Q4_0.gguf`：

- 大小：4,590,807,392 bytes
- SHA-256：`a555b900214b477d8880e7832e0b8925e139b0159640036b09fe472b6f2097f2`

仅使用该主 GGUF 的文本推理，不加载 mmproj 或音频/视觉编码器。

## 从源码构建

```bash
# 1. 安装前端依赖
npm install

# 2. 构建 Qwen3-ASR 独立 Python 运行时（联网下载 python-build-standalone
#    与 PyPI wheels；构建后的产物完全离线可用，只需执行一次）
bash scripts/build-qwen3-mlx-runtime.sh

# 3. 选择一个有效的 codesigning 身份（40 位 SHA-1）
security find-identity -v -p codesigning

# 4. 签名构建
OPENLESS_SIGNING_IDENTITY=<你的身份SHA-1> bash scripts/build-mac-stable.sh
```

产物为 `src-tauri/target/release/bundle/macos/OpenLess.app`，不会自动安装；手动拖入 `/Applications` 即可。构建脚本拒绝 ad-hoc 签名（`codesign -s -`），需要一个有效的 codesigning 身份（Keychain Access 里自签的开发证书即可，无需付费开发者账号）。

## 安装

从源码构建后把 `OpenLess.app` 拖入 `/Applications`；也可以从 [Releases](https://github.com/Wenzhi-Ding/simple-openless-macos/releases) 下载已构建的 app（约 271MB：内置完整推理运行时 Python/MLX/OpenCV/onnxruntime，**屏幕 OCR 模型已内置**，无需下载；自签证书签名，首次打开需右键 →「打开」绕过 Gatekeeper 提示）。应用不含两个大模型权重（约 8.7GB），首次启动后按引导下载。

## 使用

- 首次启动时模型缺失，概览卡显示「未找到」；点击「获取模型」按引导下载（见上节）。
- 默认热键：按一下**右 Command** 开始录音，再按一下结束并上屏；再按取消。
- 首次使用在应用内设置页授予麦克风、辅助功能、屏幕录制权限。权限检查失败不等于未授权；权限变更后回到应用刷新状态。
- 概览卡实时显示两个模型的真实加载状态（未找到/加载中/已加载/失败）。

## 隐私

- 麦克风用于录音；辅助功能用于上屏和光标/焦点读取；屏幕录制用于本地 OCR。全部推理在本机完成。
- 应用唯一的联网动作是首次下载模型（静态文件托管在 HuggingFace 及其镜像，钉死 revision + SHA-256 校验）；下载之外不建立任何网络连接，识别与润色的音频、文本永不上传。
- OCR 与光标上下文仅在听写触发时采集；敏感应用、密码框、SecureInput 以及焦点/窗口归属变化时拒绝采集。截图仅在内存中处理，不保存。
- 历史中的「润色诊断」（可选）会保存实际完整 prompt（可能包含光标、OCR 与前轮文本），仅存本机 `history.json`，不上传、不写普通日志；Debug 日志脱敏。
- 应用不自动重置系统权限，不改动用户数据、证书或钥匙串。

## 目录结构

| 路径 | 用途 |
|---|---|
| `src/` | React 界面 |
| `crates/openless-core/` | 听写、上下文、历史、诊断与回归测试 |
| `src-tauri/src/` | macOS 宿主、ASR、LLM、OCR 与权限 |
| `src-tauri/resources/qwen3-mlx/` | 打包用独立 Python runtime 与 worker（`runtime/` 由脚本构建，不入库） |
| `src-tauri/vendor/qwen-asr` | 上游 Qwen3-ASR C 实现子模块（[Open-Less/qwen-asr](https://github.com/Open-Less/qwen-asr)，MIT） |
| `scripts/` | 构建及测试入口 |
| `docs/upstream-sync.md` | 与上游的同步/裁剪决策记录 |

## 与上游的关系

本仓库是 [Open-Less/openless](https://github.com/Open-Less/openless) 在分叉点 `82aa33ff`（2026-09-07）之后的精简分支：删除移动端（Android/iOS/Harmony）、云 ASR/LLM 渠道、风格系统、翻译、Marketplace 等功能，固定为 macOS arm64 纯本地听写。具体的采纳与裁剪决策逐项记录在 [docs/upstream-sync.md](docs/upstream-sync.md)。

## 贡献

欢迎 issue 与 PR，约定见 [CONTRIBUTING.md](CONTRIBUTING.md)。安全漏洞报告见 [SECURITY.md](SECURITY.md)。

## 许可证与致谢

本项目基于 [AGPL-3.0](LICENSE) 发布。基于 [Open-Less/openless](https://github.com/Open-Less/openless) 修改（原项目及其贡献者版权归各自作者所有）。

依赖与模型致谢：[Tauri](https://tauri.app/)、[llama.cpp](https://github.com/ggml-org/llama.cpp)、[MLX](https://github.com/ml-explore/mlx)、[mlx-qwen3-asr](https://github.com/moona3k/mlx-qwen3-asr)、[RapidOCR](https://github.com/RapidAI/RapidOCR)、[Qwen3-ASR](https://github.com/QwenLM/Qwen3-ASR)、[Gemma](https://ai.google.dev/gemma/docs)。
