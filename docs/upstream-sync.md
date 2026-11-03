# 上游同步状态

记录我们对上游 `Open-Less/openless`(beta 分支)的对照结论与采纳裁定。后续再对照上游时以本文为起点,不重新盘点;新一批对照结果追加在文末。

## 对照窗口

- 分叉点:`82aa33ff`(2026-09-07,fix(windows-ime) #1021)。
- 本次对照区间:`82aa33ff..53e87830`(upstream/beta,至 2026-11-02),共 272 个提交。
- 上游动向:2.0 大版本路线——多平台(Windows/Linux egui/Android/鸿蒙/iOS)、云 ASR/LLM 渠道(Gemini Live、腾讯云、Hunyuan、OrcaRouter、ChatGPT Plan)、云同步、远程输入、引导式安装、模型设置页、Less Computer(后期已从 Linux 移除)。

## 已采纳(2026-11-02 用户裁定,共 9 项)

| # | 上游提交 | 内容 | 我方落点 | 备注 |
|---|---|---|---|---|
| 1 | `0212024a` | 事件循环线程纪律:句柄只在 event-loop 线程碰 | 胶囊高频路径借用式主线程投递;宿主级 `run_on_main_thread` 改借用;兜底卡片 show/hide/resize 三段收编;Tauri 命令入口(dismiss/report height)投递;`TauriCapsuleWindow::run_on_main_thread` 加"仅限主线程"不变量注释 | 分四轮完成;后两轮为主会话复核发现的同类残留(上轮验收扫描漏了命令线程) |
| 2 | `ed96c558`(cue 部分) | 录音提示音守卫 | `AudioCue.tsx`:订阅前取后端快照建基线(复用既有 `get_startup_snapshot`,无新增后端面)、卸载时 `stopAudioCue()`;附合同测试 | 监听器按本项目设计留在主窗,不迁 capsule 窗口(上游迁移是为配合主窗销毁方案,该项不采纳) |
| 3 | `d51006ce` | 音频流门控 | `accepting_audio` 原子门(play 成功前忽略回调);无默认设备回退第一个输入设备;全零音频独立 `NoAudio` 错误码 + 用户可读提示 | 安静但非零样本仍走既有 VAD 静音路径,不受影响 |
| 4 | `56b7f9f0` | 短输入跳润色 | 非空白字符 ≤ 4(`POLISH_SKIP_MAX_CHARS`)跳过 Gemma,走 raw 插入;诊断记 NotCalled;历史重润色不受限 | 阈值写死,不设用户配置项;词表纠正不受影响 |
| 5 | `0454dc56` | 相邻近似重复检测 | `output_cleaning.rs` 移植检测器(阈值与上游一致),润色输出清洗后检测,log::warn 只记数量 | 检测不改文本、不自动回退;生成超限错误码放宽 + Gemma 链加 repeat penalty(last_n=64, 1.1) |
| 6 | `cbde1fc2`(附带) | Qwen3-ASR 长音频 30s 分块;重转录状态防护 | Rust 侧分块(≤30s 单块路径不变);`apply_history_retranscription` 仅接受 `transcribeFailed`/`emptyTranscript` 条目,其余 InvalidState | worker 协议不变;总时长预算经第二轮修复覆盖写路径 |
| 7 | `e3f63e97`+`bd5656b5` | 构建产物新鲜度防御 | `build-mac-stable.sh`:构建前清交付目录、构建后存在性校验 | **未采用上游的 mtime 对比**——缓存二进制场景会误杀有效构建(上游 b268dcf9 后来也放弃同款检查);脚本 `set -e` + 预清空已充分 |
| 8 | `b268dcf9` | 构建加速 | `CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16` + `CARGO_PROFILE_RELEASE_STRIP=debuginfo` | 保留 opt-level 与 LTO |
| 9 | `f4ffcd42` | 推理 CPU 预算 | Gemma llama.cpp 线程数 = clamp(核 × 空闲 × 0.8, 1, 2),缺遥测按空闲 0.25;推理线程 QoS 降 UTILITY 且退出还原;prefill 线程一并限制 | MLX ASR worker **不设线程上限**:听写是延迟关键路径且 MLX 以 GPU 为主 |

**经批准的偏离**(相对项目既有约束,均已在代码注释注明 2026-11-02 批准):

- Gemma 采样链新增 repeat penalty——放宽"采样保持不变"约束,仅 Gemma 链,Qwen 旧链不动。
- 短输入跳润色属新增管线分支——放宽"`uses_llm_polisher` 恒真"的既有注释语义。

## 明确不再考虑(2026-11-02 用户裁定)

上游本区间其余更新一律不再考虑,要点归类(完整清单见 git log `82aa33ff..53e87830`):

- 云服务类:云 ASR/LLM 渠道、云同步、远程输入、官方登录渠道、更新器/CDN 清单——与纯本地边界相斥。
- 多平台类:Android/鸿蒙/iOS/Windows/Linux egui/Less Computer/TSF/UIA——本项目仅 macOS arm64。
- 功能类:风格系统/翻译/语音编辑/引导式安装/测试特性门控/自动学词(手动纠正学词管线)——本项目无对应产品面。
- 与本项目决策冲突的特定项:Fn 延迟松开判定(伤 prearm 启动延迟)、500ms 常驻预录缓冲(macOS 麦克风指示灯常亮)、关主窗销毁 WebView(架构改动,收益约 250MB)、TCC 按签名条件重置(我们构建禁止 ad-hoc,问题不存在)、可选降噪(默认关且无需求场景)、F13–F20/原生听写键(新功能入口)、Space/鼠标侧键热键(空格已覆盖,鼠标是 Windows-only)、纯文本插入开关(我们已是纯文本)、概览配置展示卡(我们的健康态+内存估算卡信息量更大)、各云提供商 LLM 参数修复(无云渠道)。

## 验证状态(2026-11-03)

- `cargo test --workspace` 退出码 0;`npm run test` 退出码 0。
- 完整签名构建通过(`scripts/build-mac-stable.sh`),产物 `src-tauri/target/release/bundle/macos/OpenLess.app`,codesign designated requirement 匹配自签身份 `79372AB3…`。
- 未覆盖:实机听写/润色/提示音/兜底卡片的真人验证(app 未安装);长音频分块的端到端效果(需真实长录音);CPU 预算的实机争用观测。
- 过程报告:`.temp/upstream-sync-2026-11/`(porting / fix / thread-cleanup / command-entry 四份)。

## 已知环境问题(本次发现,未处置,动前需授权)

1. tauri 构建把 `src-tauri/resources/qwen3-mlx/runtime/` 整棵树与 `tauri.conf.json` 列为重编译触发条件;工作区在 Dropbox 同步盘,同步触碰任一文件即触发宿主包整体重编译(2026-11-03 两次构建间无源码变化仍重编译,即此机制)。根治方向是让 Dropbox 不同步构建产物目录,属系统决策。
2. `target/` 与 `src-tauri/target/` 两棵树残留 255 个 Dropbox 冲突副本文件(可再生产物,不进 git,不影响正确性)。清理须按明确白名单执行。
3. `src-tauri/target/release/build/` 累积多个旧指纹目录,部分 output 还记录着 2026-09-18 冲突副本事故的 rerun 行(runtime 目录现已干净);与上游 bd5656b5 处理的"多份构建目录"同源,无害但属杂讯。
