import { spawnSync } from "node:child_process"

// MLX is only compiled into Apple Silicon builds. Intel macOS uses the C/CPU
// Qwen backend and Whisper, so it must not be blocked by a Metal toolchain check.
if (process.platform !== "darwin" || process.arch !== "arm64") process.exit(0)

const result = spawnSync("xcrun", ["--find", "metal"], {
    encoding: "utf8",
})

if (result.status === 0 && result.stdout?.trim()) process.exit(0)

console.error(`
OpenLess 的 Qwen3-ASR MLX 后端需要 Apple MetalToolchain。
当前 Xcode 未找到 Metal 编译器，请先执行：

  xcodebuild -downloadComponent MetalToolchain

完成后验证：

  xcrun --find metal

然后重新运行源码构建命令（例如）：

  OPENLESS_SIGNING_IDENTITY=79372AB3796B116E7D792939EDD9B58CC677C216 bash scripts/build-mac-stable.sh
`)
process.exit(1)
