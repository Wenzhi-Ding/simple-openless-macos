# OpenLess — macOS 纯本地语音输入

基于 [Open-Less/openless](https://github.com/Open-Less/openless) 精简的个人本地实现，使用 Tauri 2、Rust、React。唯一产品源码直接位于本仓库根目录；上游参考资料保留在 `upstream/`。

## 使用与功能

启动 `/Applications/OpenLess.app`，默认按一下右 Command 开始录音，再按一下结束并上屏。

- 识别固定为 **Qwen3-ASR 1.7B（MLX）**，润色固定为 **Gemma 4 E2B（llama.cpp，Q4_0）**；无模型选择、自动回退或下载入口。
- 启动时独立预载两个模型；概览卡显示后端真实加载状态，预载与实际听写/润色共享实例。
- 保留热词、历史与录音回放、上屏、选区润色，以及本地光标和屏幕 OCR 上下文。
- 产品运行不使用云识别、云润色、更新服务或远程输入，不恢复已删除的联网功能。
- 已知限制：Gemma 对纯英文输入可能产生中文输出；模型质量不能仅凭“已加载”状态判断。

## 权限与隐私

macOS 权限面板统一展示麦克风、辅助功能、屏幕录制的实际状态，并提供授权或重试操作。检查失败不等于未授权；权限变更后回到应用刷新状态。授权针对实际运行的 OpenLess，不是开发终端。

- 麦克风用于录音；辅助功能用于上屏和光标/焦点读取；屏幕录制用于本地 Vision OCR。
- 光标与 OCR 上下文固定启用，但仍受权限和敏感内容门控保护。启动不截屏，只有听写触发时才尝试采集；缺少权限或结果未及时就绪时忽略上下文，不阻塞录音。
- OCR 拒绝敏感应用、密码框、SecureInput，以及窗口/前台/焦点归属变化；截图仅在内存中处理，不保存。
- 经用户授权，历史中的“润色诊断”会保存实际完整 prompt（可能包含光标、OCR 和前轮文本）、结构化消息及调用结果，仅在本机 `history.json` 和历史页显示，不上传、不写普通日志。删除一条记录不会删除其他记录中已有的文本副本；清空全部历史才会清除全部历史记录中的副本。
- 不自动重置系统权限，不擅改用户数据、证书、钥匙串或已安装应用。

## 目录与模型

以下路径相对于仓库根目录：

| 路径 | 用途 |
|---|---|
| `src/` | React 界面 |
| `crates/openless-core/` | 听写、上下文、历史、诊断与回归测试 |
| `src-tauri/src/` | macOS 宿主、ASR、LLM、OCR 与权限 |
| `models/qwen3-asr-1.7b/` | 当前识别模型的工作区来源 |
| `models/llm/gemma-4-E2B-it-Q4_0.gguf` | 当前润色模型的工作区来源及测试资源 |
| `src-tauri/resources/qwen3-mlx/` | 打包用独立 Python runtime 与 worker |
| `scripts/` | 构建及测试入口 |

运行时模型、热词与历史位于 `~/Library/Application Support/OpenLess/`；工作区模型不等于用户数据目录，Tauri 当前仅将 Qwen3 runtime/worker 作为资源打包。当前两模型及内嵌 runtime 已随源码提升至根目录；不要另行移动、转换或删除。当前实现不依赖根目录 Python 虚拟环境。

## 构建

默认环境为本机 macOS arm64，使用 Rust、Node/npm、CMake 与 macOS 编译工具链。保留 lockfile、`node_modules/`、`vendor/`、内嵌 runtime 和必要测试；删除编译缓存后首次构建会重新编译。

在工作区根目录执行：

```bash
OPENLESS_SIGNING_IDENTITY=79372AB3796B116E7D792939EDD9B58CC677C216 bash scripts/build-mac-stable.sh
```

当前本机自签身份为 `79372AB3796B116E7D792939EDD9B58CC677C216`。稳定脚本检查身份、执行前端与 Tauri 构建并验证产物签名；输出为 `src-tauri/target/release/bundle/macos/OpenLess.app`，不会自动安装。脚本覆盖上游 Metal 工具链前置检查，不意味着可删除实际编译依赖。

**不要使用 `codesign -s -` 或其他 ad-hoc 回退。** 安装/替换 `/Applications/OpenLess.app` 需明确授权，安装后验证签名及 designated requirement 的连续性；稳定身份不保证所有 TCC 授权永久保留。不要为构建自动创建证书、更改信任或重置权限。

`build-qwen3-mlx-runtime.sh` 是独立 runtime 的重建入口，会下载 Python 与 wheels；日常构建使用现有离线资源，不需重跑。更多实施约束见根 `AGENTS.md`。
