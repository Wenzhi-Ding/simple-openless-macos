import { createHash } from "node:crypto"
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
} from "node:fs"
import { dirname, join, resolve } from "node:path"
import { fileURLToPath } from "node:url"

const appRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..")

function parseArgs(argv) {
  const options = {
    runtimeRoot: join(appRoot, "src-tauri", "resources", "qwen3-mlx", "runtime"),
    output: join(appRoot, "src-tauri", "target", "release", "openless-mlx", "mlx.metallib"),
  }
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index + 1]
    if (argv[index] === "--runtime-root" && value) {
      options.runtimeRoot = resolve(value)
      index += 1
    } else if (argv[index] === "--output" && value) {
      options.output = resolve(value)
      index += 1
    } else {
      throw new Error(`未知参数或缺少参数值：${argv[index]}`)
    }
  }
  return options
}

function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex")
}

function collectCandidates(root) {
  if (!existsSync(root)) return []
  const candidates = []
  const visit = directory => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name)
      if (entry.isDirectory()) visit(path)
      else if (
        entry.isFile()
        && entry.name === "mlx.metallib"
        && path.endsWith(join("mlx", "lib", "mlx.metallib"))
        && statSync(path).size > 0
      ) candidates.push(path)
    }
  }
  visit(root)
  return candidates.sort()
}

export function stageMetallib({ runtimeRoot, output }) {
  rmSync(output, { force: true })
  const candidates = collectCandidates(runtimeRoot)
  if (candidates.length !== 1) {
    const details = candidates.length === 0 ? "  (none)" : candidates.map(path => `  ${path}`).join("\n")
    throw new Error(`MLX runtime 必须恰好包含一个非空 mlx.metallib（找到 ${candidates.length} 个）：\n${details}`)
  }

  mkdirSync(dirname(output), { recursive: true })
  copyFileSync(candidates[0], output)
  console.log(`✓ staged MLX metallib: ${output}`)
  console.log(`  sha256=${sha256(candidates[0])} source=${candidates[0]}`)
}

try {
  stageMetallib(parseArgs(process.argv.slice(2)))
} catch (error) {
  console.error(`✗ ${error instanceof Error ? error.message : String(error)}`)
  process.exitCode = 1
}
