# 贡献指南 / Contributing

欢迎 issue 与 PR。本项目是个人精简分支，维护节奏以「保持纯本地边界、小步采纳」为准。

## 报告问题 / Reporting issues

提交 issue 时请附：

- macOS 版本与机型（Apple Silicon 型号）；
- 应用日志 `~/Library/Logs/OpenLess/openless.log`（只含状态与字数，可直接附）；
- 如涉及润色质量问题：历史页该条目的「润色诊断」。注意诊断包含实际完整 prompt（可能含屏幕 OCR、光标附近文本与前轮内容），**附上前请自行脱敏**，或只描述现象。

## 提交代码 / Submitting PRs

- 许可：贡献按 AGPL-3.0 授权，无需另签协议。
- 目标平台：仅 macOS arm64；不接受引入移动端或云服务（云 ASR/LLM、更新、遥测等）的改动——这是产品的硬边界。
- 提交前请跑通：
  - `npm test`（含前端构建与合同测试）
  - `cargo test`（在 `src-tauri/` 下）
- 修改构建或打包行为时，请用 `scripts/build-mac-stable.sh` 实际构建一次并在 PR 说明中附结果。
- 与上游 `Open-Less/openless` 的同步/裁剪决策记录在 `docs/upstream-sync.md`；涉及上游行为的 PR 请在该文档追记。

## 开发环境 / Development setup

按 README「从源码构建」一节搭建（依赖、模型放置、runtime 构建）。日常开发可用 `npm run dev` 与 `npx tauri dev`。
