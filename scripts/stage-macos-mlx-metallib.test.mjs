import assert from "node:assert/strict"
import {
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs"
import { tmpdir } from "node:os"
import { join, resolve } from "node:path"
import { spawnSync } from "node:child_process"
import { fileURLToPath } from "node:url"

const appRoot = resolve(fileURLToPath(new URL("..", import.meta.url)))
const script = join(appRoot, "scripts", "stage-macos-mlx-metallib.mjs")

function candidate(root, version, content) {
  const path = join(root, "lib", version, "site-packages", "mlx", "lib", "mlx.metallib")
  mkdirSync(resolve(path, ".."), { recursive: true })
  writeFileSync(path, content)
  return path
}

function run(runtimeRoot, output) {
  return spawnSync(process.execPath, [script, "--runtime-root", runtimeRoot, "--output", output], {
    cwd: appRoot,
    encoding: "utf8",
  })
}

const root = mkdtempSync(join(tmpdir(), "openless-mlx-stage-test-"))
try {
  const runtimeRoot = join(root, "runtime")
  const output = join(root, "staged", "mlx.metallib")
  mkdirSync(runtimeRoot, { recursive: true })
  mkdirSync(resolve(output, ".."), { recursive: true })

  writeFileSync(output, "stale", { flag: "w" })
  const missing = run(runtimeRoot, output)
  assert.notEqual(missing.status, 0)
  assert.match(missing.stderr, /恰好包含一个非空 mlx\.metallib/)
  assert.equal(existsSync(output), false)

  candidate(runtimeRoot, "python3.12", "kernel-a")
  const single = run(runtimeRoot, output)
  assert.equal(single.status, 0, single.stderr)
  assert.equal(readFileSync(output, "utf8"), "kernel-a")

  candidate(runtimeRoot, "python3.13", "kernel-a")
  const duplicate = run(runtimeRoot, output)
  assert.notEqual(duplicate.status, 0)
  assert.match(duplicate.stderr, /找到 2 个/)
  assert.match(duplicate.stderr, /python3\.12/)
  assert.match(duplicate.stderr, /python3\.13/)
} finally {
  rmSync(root, { recursive: true, force: true })
}

console.log("stage-macos-mlx-metallib tests passed")
