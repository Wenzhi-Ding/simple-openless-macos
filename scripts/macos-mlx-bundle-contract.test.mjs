import assert from "node:assert/strict"
import { existsSync, readFileSync } from "node:fs"
import { resolve } from "node:path"
import { fileURLToPath } from "node:url"

const appRoot = resolve(fileURLToPath(new URL("..", import.meta.url)))
const stable = resolve(appRoot, "scripts/build-mac-stable.sh")
const metallibStage = resolve(appRoot, "scripts/stage-macos-mlx-metallib.mjs")
const macosConfig = readFileSync(resolve(appRoot, "src-tauri/tauri.macos-mlx.conf.json"), "utf8")
const buildScript = readFileSync(stable, "utf8")

assert.ok(existsSync(stable), "the sole supported macOS build entrypoint must exist")
assert.ok(existsSync(metallibStage), "the MLX metallib staging entrypoint must exist")
assert.match(macosConfig, /stage-macos-mlx-metallib\.mjs/)
assert.match(buildScript, /OpenLess\.app/)
assert.doesNotMatch(buildScript, /hdiutil|\.dmg|updater|stapler|notar/i)
assert.doesNotMatch(buildScript, /codesign\s+-s\s+-\s*['\"][ -]?['\"]/i)
assert.doesNotMatch(buildScript, /\/Applications\/OpenLess\.app|tccutil/)
assert.ok(!existsSync(resolve(appRoot, "scripts/build-mac.sh")))

console.log("macOS MLX bundle contract tests passed")
