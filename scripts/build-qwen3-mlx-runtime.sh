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
# RapidOCR(Apache-2.0):屏幕 OCR 引擎(2026-09-18 由 Apple Vision 切换,
# 对比实验见 .temp/ocr-bench-2026-09-18/)。wheel 自带 PP-OCRv6 small ONNX
# 模型(det/rec/cls),运行期完全离线;onnxruntime 是它的推理后端(其
# 声明为可选依赖,必须显式装)。版本锁定与实验环境一致。
OCR_PKGS=("rapidocr==3.9.2" "onnxruntime==1.30.0")

echo "=== download python-build-standalone ($PBS_TAG) ==="
curl -L --fail -C - -o "$STAGE/$PBS_ASSET" "$PBS_URL"

echo "=== extract into $RUNTIME_DIR ==="
rm -rf "$RUNTIME_DIR"
mkdir -p "$RUNTIME_DIR"
tar -xzf "$STAGE/$PBS_ASSET" -C "$RUNTIME_DIR" --strip-components 1
"$RUNTIME_DIR/bin/python3" --version

echo "=== pip install $PKG + ${OCR_PKGS[*]} (PyPI wheels) ==="
"$RUNTIME_DIR/bin/python3" -m pip install --no-cache-dir --no-input "$PKG" "${OCR_PKGS[@]}"

echo "=== verify imports (offline env) ==="
HF_HUB_OFFLINE=1 "$RUNTIME_DIR/bin/python3" -c "import mlx_qwen3_asr; print('mlx-qwen3-asr', mlx_qwen3_asr.__version__)"
HF_HUB_OFFLINE=1 "$RUNTIME_DIR/bin/python3" -c "from rapidocr import RapidOCR; RapidOCR(); print('rapidocr engine ok')"

echo "=== prune (headers/tests/idle + hub/httpx 网络栈整体移除) ==="
# 网络面清理:worker 只从本地目录加载模型,huggingface-hub/httpx 及其专属传递
# 依赖全部移除(mlx_qwen3_asr 对 hub 只有 _resolve_path 内的惰性 import,本地
# 目录分支不可达;worker 另有 socket 拒绝守卫兜底)。
# 注意:certifi/idna 必须保留——rapidocr 经 download_file.py 硬性 import
# requests,而 requests 启动时就要 import 这两个包。requests 本体留在运行时
# 里无害:真正的离线边界是 worker 的 socket 拒绝守卫(netcheck 自检)。
# PyYAML 的 C 加速扩展(_yaml)是 wheel 自带的 ad-hoc 签名二进制,macOS 按未
# 公证代码拦截它(首次加载弹"Apple 无法验证"对话框;2026-09-18 实测于非
# /Applications 路径)。OCR 配置解析量极小,PyYAML 自动回退纯 Python,剔除。
LIB="$RUNTIME_DIR/lib/python3.12"
SP="$LIB/site-packages"
rm -f "$SP/yaml/"_yaml*.so
rm -rf "$SP/_yaml"
rm -rf "$LIB/test" "$LIB/idlelib" "$LIB/lib2to3" "$LIB/tkinter" "$LIB/turtledemo" \
       "$RUNTIME_DIR/bin/pip3" "$RUNTIME_DIR/bin/pip3.12" \
       "$SP/setuptools" "$SP/wheel" 2>/dev/null || true
for pkg in huggingface_hub hf_xet httpx httpcore h11 anyio sniffio \
           filelock fsspec pip; do
    rm -rf "$SP/$pkg"
    find "$SP" -maxdepth 1 -name "${pkg}-*.dist-info" -exec rm -rf {} + 2>/dev/null || true
done
find "$RUNTIME_DIR" -type d -name "__pycache__" -prune -exec rm -rf {} + 2>/dev/null || true
find "$RUNTIME_DIR" -type d -name "tests" -path "*site-packages*" -prune -exec rm -rf {} + 2>/dev/null || true
find "$RUNTIME_DIR" -name "*.a" -delete 2>/dev/null || true
find "$RUNTIME_DIR" -name "include" -maxdepth 4 -type d -prune -exec rm -rf {} + 2>/dev/null || true

echo "=== verify imports after prune ==="
HF_HUB_OFFLINE=1 "$RUNTIME_DIR/bin/python3" -c "import mlx_qwen3_asr; print('post-prune ok')"
HF_HUB_OFFLINE=1 "$RUNTIME_DIR/bin/python3" -c "from rapidocr import RapidOCR; RapidOCR(); print('post-prune rapidocr ok')"

rm -rf "$STAGE"
du -sh "$RUNTIME_DIR"
echo "RUNTIME_DONE"
