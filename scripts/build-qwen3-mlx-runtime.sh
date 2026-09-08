#!/bin/bash
# 重建 Qwen3-ASR(MLX)打包运行时:python-build-standalone(PBS)+ mlx-qwen3-asr wheel。
# 产物落 src-tauri/resources/qwen3-mlx/runtime/,由 tauri.conf.json 的 bundle
# resources 映射进 .app(Contents/Resources/qwen3-mlx)。该目录不入版本库。
#
# 本脚本只在开发机上联网下载 PBS 归档与 PyPI wheel;产物本身完全离线可用,
# worker 以 -I 启动且 HF_HUB_OFFLINE=1,不访问网络。
set -euo pipefail

APP_DIR="$(cd "$(dirname "$0")/.." && pwd)"
RUNTIME_DIR="$APP_DIR/src-tauri/resources/qwen3-mlx/runtime"
STAGE="$(mktemp -d "${TMPDIR:-/tmp}/openless-qwen3-mlx-build.XXXXXX")"
PBS_TAG="20260901"
PBS_ASSET="cpython-3.12.14+20260901-aarch64-apple-darwin-install_only.tar.gz"
PBS_URL="https://github.com/astral-sh/python-build-standalone/releases/download/$PBS_TAG/$PBS_ASSET"
PKG="mlx-qwen3-asr==0.4.0"   # Apache-2.0, https://github.com/moona3k/mlx-qwen3-asr

echo "=== download python-build-standalone ($PBS_TAG) ==="
curl -L --fail -C - -o "$STAGE/$PBS_ASSET" "$PBS_URL"

echo "=== extract into $RUNTIME_DIR ==="
rm -rf "$RUNTIME_DIR"
mkdir -p "$RUNTIME_DIR"
tar -xzf "$STAGE/$PBS_ASSET" -C "$RUNTIME_DIR" --strip-components 1
"$RUNTIME_DIR/bin/python3" --version

echo "=== pip install $PKG (PyPI wheels) ==="
"$RUNTIME_DIR/bin/python3" -m pip install --no-cache-dir --no-input "$PKG"

echo "=== verify imports (offline env) ==="
HF_HUB_OFFLINE=1 "$RUNTIME_DIR/bin/python3" -c "import mlx_qwen3_asr; print('mlx-qwen3-asr', mlx_qwen3_asr.__version__)"

echo "=== prune (headers/tests/idle + hub/httpx 网络栈整体移除) ==="
# 网络面清理:worker 只从本地目录加载模型,huggingface-hub/httpx 及其专属传递
# 依赖全部移除(mlx_qwen3_asr 对 hub 只有 _resolve_path 内的惰性 import,本地
# 目录分支不可达;worker 另有 socket 拒绝守卫兜底)。
LIB="$RUNTIME_DIR/lib/python3.12"
SP="$LIB/site-packages"
rm -rf "$LIB/test" "$LIB/idlelib" "$LIB/lib2to3" "$LIB/tkinter" "$LIB/turtledemo" \
       "$RUNTIME_DIR/bin/pip3" "$RUNTIME_DIR/bin/pip3.12" \
       "$SP/setuptools" "$SP/wheel" 2>/dev/null || true
for pkg in huggingface_hub hf_xet httpx httpcore h11 anyio sniffio \
           certifi idna filelock fsspec pip; do
    rm -rf "$SP/$pkg"
    find "$SP" -maxdepth 1 -name "${pkg}-*.dist-info" -exec rm -rf {} + 2>/dev/null || true
done
find "$RUNTIME_DIR" -type d -name "__pycache__" -prune -exec rm -rf {} + 2>/dev/null || true
find "$RUNTIME_DIR" -type d -name "tests" -path "*site-packages*" -prune -exec rm -rf {} + 2>/dev/null || true
find "$RUNTIME_DIR" -name "*.a" -delete 2>/dev/null || true
find "$RUNTIME_DIR" -name "include" -maxdepth 4 -type d -prune -exec rm -rf {} + 2>/dev/null || true

echo "=== verify imports after prune ==="
HF_HUB_OFFLINE=1 "$RUNTIME_DIR/bin/python3" -c "import mlx_qwen3_asr; print('post-prune ok')"

rm -rf "$STAGE"
du -sh "$RUNTIME_DIR"
echo "RUNTIME_DONE"
