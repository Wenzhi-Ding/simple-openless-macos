# 安全策略 / Security Policy

## 支持版本 / Supported versions

仅支持 `main` 分支最新代码。不维护历史版本的补丁。

## 报告漏洞 / Reporting a vulnerability

本项目为纯本地应用：唯一的联网动作是用户在引导页显式点击后的首启模型下载（源与 SHA-256 校验值钉死在代码中）；ASR worker 进程内禁用全部联网 socket，识别与润色推理不建立任何网络连接（均有回归测试）。如果发现：

- 任何数据离开本机的路径（网络、遥测、上传）；
- OCR/光标门控绕过（敏感应用、密码框、SecureInput、焦点归属）；
- 权限判断 fail-open、签名校验绕过或其他本地安全问题；

请优先使用 GitHub 的私密漏洞报告（Security → Report a vulnerability），或发邮件至 wenzhi.ding@outlook.com。不要在公开 issue 中披露细节。

## 范围外 / Out of scope

- 本机已被攻击者完全控制后的场景；
- 用户主动关闭 SIP/TCC 或重置权限导致的行为；
- 上游 Open-Less/openless 的问题（请向[上游仓库](https://github.com/Open-Less/openless)报告）。
