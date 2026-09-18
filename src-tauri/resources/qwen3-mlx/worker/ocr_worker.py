#!/usr/bin/env python3
"""OpenLess 屏幕 OCR(RapidOCR)离线 worker。

常驻子进程,通过 stdin/stdout 上的换行分隔 JSON 与宿主(Rust)通信。
与 qwen3_worker.py 同一套「纯本地、fail closed」自防护,不依赖宿主传参:

1. 在任何第三方 import 之前把离线环境变量钉死(HF_HUB_OFFLINE 等);
2. 禁用一切联网 socket:AF_INET/AF_INET6 的 socket 构造、create_connection、
   DNS 解析全部抛错,只放行 AF_UNIX;
3. RapidOCR 引擎在启动时即完成初始化(wheel 自带 PP-OCRv6 small ONNX
   模型,不下载任何文件),初始化失败即退出。

协议:
  worker -> host:
    {"event":"boot"}                                   # Python 与依赖 import 完成
    {"event":"ready","engine":"rapidocr <ver>"}        # 引擎初始化完成
    {"event":"failed","error":"..."}                   # 启动/初始化失败后退出
    {"id":N,"ok":true,"text":"..."}                    # ocr 成功(可能为空字符串)
    {"id":N,"ok":true,...}                             # ping / netcheck 成功
    {"id":N,"ok":false,"error":"..."}                  # 任意 op 失败
  host -> worker:
    {"id":N,"op":"ping"}
    {"id":N,"op":"netcheck"}                           # 自检:联网面必须全部被拒
    {"id":N,"op":"ocr","png_base64":"..."}             # PNG 截图(仅内存)

图片格式:png_base64 是一张 PNG 的 base64;解码只经内存,不落盘。
"""

from __future__ import annotations

import os

# ── 离线 fail-closed 第 1 层:任何第三方库 import 前钉死环境 ──
os.environ["HF_HUB_OFFLINE"] = "1"
os.environ["TRANSFORMERS_OFFLINE"] = "1"
os.environ["HF_HUB_DISABLE_TELEMETRY"] = "1"
os.environ["HF_HUB_DISABLE_IMPLICIT_TOKEN"] = "1"

import base64
import io
import json
import socket
import sys
import traceback

# ── 离线 fail-closed 第 2 层:socket 联网面整体拒绝(只放行 AF_UNIX) ──
_ALLOWED_SOCKET_FAMILIES = frozenset({socket.AF_UNIX})
_NETWORK_DISABLED_MESSAGE = "network sockets are disabled in the OpenLess OCR worker"

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
    try:
        import numpy as np  # noqa: F401
        import cv2  # noqa: F401
        from importlib.metadata import version as pkg_version
        from rapidocr import RapidOCR
    except Exception as exc:  # 环境损坏时才会走到
        emit({
            "event": "failed",
            "error": f"OCR runtime import failed: {exc}\n{traceback.format_exc()}",
        })
        return 1
    emit({"event": "boot"})

    try:
        # 默认配置 = 2026-09-18 对比实验所用的配置(wheel 自带 PP-OCRv6 small
        # det/rec/cls 模型),改动配置必须重跑对比再定。
        engine = RapidOCR()
    except Exception as exc:
        emit({
            "event": "failed",
            "error": f"init RapidOCR engine failed: {exc}\n{traceback.format_exc()}",
        })
        return 1
    emit({"event": "ready", "engine": f"rapidocr {pkg_version('rapidocr')}"})

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
            elif op == "ocr":
                png = base64.b64decode(request.get("png_base64") or "")
                if not png:
                    raise ValueError("empty png payload")
                buffer = np.frombuffer(png, dtype=np.uint8)
                image = cv2.imdecode(buffer, cv2.IMREAD_COLOR)
                if image is None:
                    raise ValueError("png decode failed")
                result = engine(image)
                lines = list(result.txts) if result is not None and result.txts else []
                emit({"id": request_id, "ok": True, "text": "\n".join(lines)})
            else:
                emit({"id": request_id, "ok": False, "error": f"unknown op: {op}"})
        except Exception as exc:
            emit({"id": request_id, "ok": False, "error": f"{exc}\n{traceback.format_exc()}"})

    return 0


if __name__ == "__main__":
    sys.exit(main())
