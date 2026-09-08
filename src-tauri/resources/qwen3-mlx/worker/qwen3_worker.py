#!/usr/bin/env python3
"""OpenLess Qwen3-ASR (MLX) offline worker.

常驻子进程,通过 stdin/stdout 上的换行分隔 JSON 与宿主(Rust)通信。
本进程按「纯本地、fail closed」原则自防护,不依赖宿主传参:

1. 在任何第三方 import 之前把离线环境变量钉死(HF_HUB_OFFLINE 等);
2. 禁用一切联网 socket:AF_INET/AF_INET6 的 socket 构造、create_connection、
   DNS 解析全部抛错,只放行 AF_UNIX —— 即使 site-packages 里存在
   httpx/huggingface-hub,也无法建立任何网络连接;
3. 模型目录必须先通过完整性校验(is_dir + config.json + *.safetensors +
   tokenizer 词表)才允许 import 模型栈并创建 Session,校验失败即退出。

协议:
  worker -> host:
    {"event":"boot"}                                   # Python 与依赖 import 完成
    {"event":"ready","model":"<dir>"}                  # 模型加载完成
    {"event":"failed","error":"..."}                   # 启动/加载失败后退出
    {"id":N,"ok":true,"text":"...","audio_ms":123}     # transcribe 成功
    {"id":N,"ok":true,...}                              # ping / netcheck 成功
    {"id":N,"ok":false,"error":"..."}                  # 任意 op 失败
  host -> worker:
    {"id":N,"op":"ping"}
    {"id":N,"op":"netcheck"}                           # 自检:联网面必须全部被拒
    {"id":N,"op":"transcribe","pcm_base64":"...","language":null|"Chinese"|"zh"}

音频格式:pcm_base64 是 16 kHz 单声道 little-endian int16 PCM 的 base64。
language 接受 ISO/BCP 码(zh/en/...)或规范语言名(Chinese/English/...),
未识别的值按自动语言处理。
"""

from __future__ import annotations

import os

# ── 离线 fail-closed 第 1 层:任何第三方库 import 前钉死环境 ──
os.environ["HF_HUB_OFFLINE"] = "1"
os.environ["TRANSFORMERS_OFFLINE"] = "1"
os.environ["HF_HUB_DISABLE_TELEMETRY"] = "1"
os.environ["HF_HUB_DISABLE_IMPLICIT_TOKEN"] = "1"

import base64
import json
import socket
import sys
import traceback
from pathlib import Path

# ── 离线 fail-closed 第 2 层:socket 联网面整体拒绝(只放行 AF_UNIX) ──
_ALLOWED_SOCKET_FAMILIES = frozenset({socket.AF_UNIX})
_NETWORK_DISABLED_MESSAGE = "network sockets are disabled in the OpenLess Qwen3-ASR worker"

_ORIGINAL_SOCKET_INIT = socket.socket.__init__


def _offline_socket_init(self, family=socket.AF_INET, type=socket.SOCK_STREAM, proto=0, fileno=None):
    effective_family = family if family not in (-1, None) else socket.AF_INET
    if effective_family not in _ALLOWED_SOCKET_FAMILIES:
        raise OSError(f"{_NETWORK_DISABLED_MESSAGE} (family={family!r})")
    _ORIGINAL_SOCKET_INIT(self, family, type, proto, fileno)


def _refused(*_args, **_kwargs):
    raise OSError(_NETWORK_DISABLED_MESSAGE)


socket.socket.__init__ = _offline_socket_init
socket.create_connection = _refused
socket.getaddrinfo = _refused
socket.gethostbyname = _refused
socket.gethostbyname_ex = _refused

# ── 离线 fail-closed 第 3 层:模型目录完整性校验(import 模型栈之前) ──


def validate_model_dir(model_dir: Path) -> str:
    """返回空字符串表示通过;否则返回面向宿主的错误说明。"""
    try:
        if not model_dir.is_dir():
            return f"model directory does not exist: {model_dir}"
        if not (model_dir / "config.json").is_file():
            return f"config.json not found in {model_dir}"
        has_weights = any(
            entry.is_file() and entry.suffix == ".safetensors"
            for entry in model_dir.iterdir()
        )
        if not has_weights:
            return f"no *.safetensors weights found in {model_dir}"
        if not (
            (model_dir / "vocab.json").is_file()
            or (model_dir / "tokenizer.json").is_file()
        ):
            return f"tokenizer files (vocab.json / tokenizer.json) not found in {model_dir}"
        return ""
    except OSError as exc:
        return f"cannot inspect model directory {model_dir}: {exc}"


# ISO/BCP 码 + 规范语言名(小写)→ 上游库接受的规范语言名。
LANGUAGE_ALIASES = {
    # ISO / BCP-47
    "zh": "Chinese", "zh-cn": "Chinese", "zh-tw": "Chinese", "zh-hk": "Chinese",
    "yue": "Chinese",
    "en": "English", "ja": "Japanese", "ko": "Korean", "de": "German",
    "fr": "French", "es": "Spanish", "ru": "Russian", "pt": "Portuguese",
    "it": "Italian", "vi": "Vietnamese", "th": "Thai", "ar": "Arabic",
    "id": "Indonesian", "ms": "Malay", "hi": "Hindi",
    # 规范语言名(不区分大小写)
    "chinese": "Chinese", "english": "English", "japanese": "Japanese",
    "korean": "Korean", "german": "German", "french": "French",
    "spanish": "Spanish", "russian": "Russian", "portuguese": "Portuguese",
    "italian": "Italian", "vietnamese": "Vietnamese", "thai": "Thai",
    "arabic": "Arabic", "indonesian": "Indonesian", "malay": "Malay",
    "hindi": "Hindi",
}
KNOWN_LANGUAGE_NAMES = frozenset(LANGUAGE_ALIASES.values())


def normalize_language(raw) -> str | None:
    """ISO 码或规范语言名 → 上游规范名;未识别的值按自动语言(None)。"""
    if not raw:
        return None
    text = str(raw).strip()
    if not text:
        return None
    mapped = LANGUAGE_ALIASES.get(text.lower())
    if mapped:
        return mapped
    if text in KNOWN_LANGUAGE_NAMES:
        return text
    return None


def emit(payload: dict) -> None:
    sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def op_netcheck() -> dict:
    """自检联网面:INET socket 构造与 DNS 必须被拒,AF_UNIX 必须可用。"""
    inet_blocked = False
    try:
        socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    except OSError:
        inet_blocked = True
    inet6_blocked = False
    try:
        socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
    except OSError:
        inet6_blocked = True
    dns_blocked = False
    try:
        socket.getaddrinfo("localhost", 80)
    except OSError:
        dns_blocked = True
    unix_allowed = False
    try:
        probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        probe.close()
        unix_allowed = True
    except OSError:
        unix_allowed = False
    ok = inet_blocked and inet6_blocked and dns_blocked and unix_allowed
    return {
        "ok": ok,
        "inetBlocked": inet_blocked,
        "inet6Blocked": inet6_blocked,
        "dnsBlocked": dns_blocked,
        "unixAllowed": unix_allowed,
    }


def main() -> int:
    model_dir_raw = sys.argv[1] if len(sys.argv) > 1 else ""
    if not model_dir_raw:
        emit({"event": "failed", "error": "model directory argument is missing"})
        return 1
    model_dir = Path(model_dir_raw)
    validation_error = validate_model_dir(model_dir)
    if validation_error:
        emit({"event": "failed", "error": validation_error})
        return 1

    try:
        import numpy as np  # noqa: F401
        from mlx_qwen3_asr import Session
    except Exception as exc:  # pragma: no cover - 环境损坏时才会走到
        emit({
            "event": "failed",
            "error": f"MLX runtime import failed: {exc}\n{traceback.format_exc()}",
        })
        return 1
    emit({"event": "boot"})

    try:
        session = Session(model=str(model_dir))
    except Exception as exc:
        emit({
            "event": "failed",
            "error": f"load model {model_dir} failed: {exc}\n{traceback.format_exc()}",
        })
        return 1
    emit({"event": "ready", "model": str(model_dir)})

    for line in sys.stdin.buffer:
        line = line.strip()
        if not line:
            continue
        try:
            request = json.loads(line)
            request_id = request.get("id")
        except Exception as exc:
            emit({"id": None, "ok": False, "error": f"bad request line: {exc}"})
            continue

        op = request.get("op")
        try:
            if op == "ping":
                emit({"id": request_id, "ok": True})
            elif op == "netcheck":
                result = op_netcheck()
                if not result.pop("ok"):
                    raise OSError("offline socket guard self-check failed")
                emit({"id": request_id, "ok": True, **result})
            elif op == "transcribe":
                pcm = base64.b64decode(request.get("pcm_base64") or "")
                if len(pcm) % 2 != 0:
                    raise ValueError("pcm length is not aligned to int16 samples")
                audio_ms = len(pcm) * 1000 // 32000
                import numpy as np

                samples = np.frombuffer(pcm, dtype="<i2").astype(np.float32) / 32768.0
                language = normalize_language(request.get("language"))
                result = session.transcribe((samples, 16000), language=language)
                emit({"id": request_id, "ok": True, "text": result.text, "audio_ms": audio_ms})
            else:
                emit({"id": request_id, "ok": False, "error": f"unknown op: {op}"})
        except Exception as exc:
            emit({"id": request_id, "ok": False, "error": f"{exc}\n{traceback.format_exc()}"})

    return 0


if __name__ == "__main__":
    sys.exit(main())
