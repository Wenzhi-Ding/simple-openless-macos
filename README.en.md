# OpenLess — Fully Local Voice Input for macOS

[简体中文](README.md) | English

OpenLess is a fully local voice input tool for macOS (Apple Silicon): press a hotkey, speak, and on release your speech is transcribed, lightly polished, and typed at the current cursor position. Speech recognition and text polishing run entirely on-device — no cloud services, and no audio or text ever leaves the machine.

This project is a trimmed-down fork of [Open-Less/openless](https://github.com/Open-Less/openless) (AGPL-3.0) that keeps only the macOS arm64 desktop dictation product line; mobile and cloud-related code has been removed. Model weights are not distributed with the repository: download them through the in-app first-run guide, or place them manually — see [Getting the models](#getting-the-models).

## Features

- **Speech recognition**: Qwen3-ASR 1.7B (MLX format), preloaded at startup; supports Chinese, English, and many other languages (full list in the model's `config.json` under `support_languages`).
- **Text polishing**: Gemma 4 E4B (llama.cpp, Q4_0). Runs a single fixed light-polish prompt baked into the code (`crates/openless-core/src/prompts.rs`): preserves meaning, tone, language, and number formatting; only removes meaningless filler/repeats, adds punctuation, and fixes obvious typos. It never translates, expands, answers, or executes the text. No style, mode, or model selection.
- **Manual dictionary**: user-added custom terms, used for homophone correction after recognition and as polishing context; the app never learns new words from history or screen content.
- **Cursor & screen context**: during dictation, the local cursor position and on-screen OCR text (RapidOCR, on-device) help correct proper nouns. OCR refuses password fields, SecureInput, and sensitive apps; screenshots are processed in memory only and never saved. Context is always enabled but gated by permissions and sensitive-content checks.
- **History & replay**: dictation history with audio replay, stored locally; entries can be re-polished.
- **Local-only boundary**: no cloud ASR/LLM, telemetry, or update services. The only network action is the in-app first-run model download (sources and SHA-256 checks are pinned in code); recognition and polishing never open a network connection. The ASR worker process disables all network sockets outright (fail closed), with regression tests guarding that boundary.

### Known limitations

- The fixed polish prompt is not a model-fidelity guarantee: in real E4B tests, Arabic numerals, English, and mixed-language samples pass, but「明天十点」still becomes「明天10点」(number-format drift).
- macOS arm64 (Apple Silicon) only; no Intel Mac, Windows, Linux, Android, or iOS support.
- No prebuilt releases — build from source.

## Requirements

- Apple Silicon Mac, macOS 14+
- Xcode with the Metal toolchain (`xcodebuild -downloadComponent MetalToolchain`)
- Rust (rustup), Node.js/npm (Homebrew builds), CMake

## Getting the models

The app loads two fixed models from the data directory `~/Library/Application Support/OpenLess/`. There is no model selection or fallback UI; recognition and polishing run fully offline.

### Option 1: in-app guided download (recommended)

On first use, the overview cards show the models as missing — click "Get models" to download inside the app:

- **Speech recognition Qwen3-ASR 1.7B (MLX)**: choose full-precision bf16 (~4.1 GB, the author-verified baseline) or 4-bit quantized (~1.6 GB, faster download);
- **Text polishing Gemma 4 E4B (GGUF)**: ~4.6 GB;
- Sources are pinned to fixed revisions with per-file SHA-256 verification; resumable and cancellable; users in mainland China can switch the download source to the hf-mirror.com community mirror;
- The app reloads the engines automatically when the download finishes.

### Option 2: manual placement

If you prefer fetching the weights yourself, place them under the data directory and restart the app (same public sources as option 1).

**Qwen3-ASR 1.7B (MLX format)** → `~/Library/Application Support/OpenLess/models/qwen3-asr-1.7b/`, containing `config.json`, `*.safetensors` weights, and tokenizer vocab files. Sources (any of):

- [`mlx-community/Qwen3-ASR-1.7B-bf16`](https://huggingface.co/mlx-community/Qwen3-ASR-1.7B-bf16) (~4.1 GB, the variant the in-app download uses) or [`...-4bit`](https://huggingface.co/mlx-community/Qwen3-ASR-1.7B-4bit) (~1.6 GB);
- [`moona3k/mlx-qwen3-asr-1.7b-4bit`](https://huggingface.co/moona3k/mlx-qwen3-asr-1.7b-4bit) / `...-8bit` (pre-converted weights published by the loader's author);
- Or convert the official [`Qwen/Qwen3-ASR-1.7B`](https://huggingface.co/Qwen/Qwen3-ASR-1.7B) weights yourself with [mlx-qwen3-asr](https://github.com/moona3k/mlx-qwen3-asr) (Apache-2.0; pinned at 0.4.0 in the bundled runtime) via its `scripts/convert.py`.

**Gemma 4 E4B (GGUF)** → `~/Library/Application Support/OpenLess/models/llm/gemma-4-E4B-it-Q4_0.gguf`.

Source: [ggml-org/gemma-4-E4B-it-GGUF](https://huggingface.co/ggml-org/gemma-4-E4B-it-GGUF/tree/b8093469224f83f5c38f691eb906c380e9e63114), pinned revision `b8093469224f83f5c38f691eb906c380e9e63114`, file `gemma-4-E4B-it-Q4_0.gguf`:

- Size: 4,590,807,392 bytes
- SHA-256: `a555b900214b477d8880e7832e0b8925e139b0159640036b09fe472b6f2097f2`

Only the main GGUF's text inference is used; no mmproj or audio/vision encoders are loaded.

## Building from source

```bash
# 1. Install frontend dependencies
npm install

# 2. Build the standalone Qwen3-ASR Python runtime (downloads
#    python-build-standalone and PyPI wheels; the result is fully
#    offline-capable — only needed once)
bash scripts/build-qwen3-mlx-runtime.sh

# 3. Pick a valid codesigning identity (40-character SHA-1)
security find-identity -v -p codesigning

# 4. Signed build
OPENLESS_SIGNING_IDENTITY=<your-identity-sha1> bash scripts/build-mac-stable.sh
```

The output is `src-tauri/target/release/bundle/macos/OpenLess.app`; it is not installed automatically — drag it into `/Applications`. The build script rejects ad-hoc signing (`codesign -s -`) and requires a valid codesigning identity (a self-signed development certificate in Keychain Access is enough; no paid Apple Developer account needed).

## Install

After building from source, drag `OpenLess.app` into `/Applications`; you can also grab a prebuilt app from [Releases](https://github.com/Wenzhi-Ding/simple-openless-macos/releases) (~271MB: it bundles the full inference runtime — Python/MLX/OpenCV/ONNX Runtime — with the **screen-OCR models baked in**, so nothing else is needed for OCR; self-signed certificate — right-click → "Open" the first time to pass Gatekeeper). The two large model weights (~8.7GB) are not included; download them via the in-app guide on first launch.

## Usage

- On first launch the models are missing and the overview cards show them as such; click "Get models" and follow the guide (see above).
- Default hotkey: press **right Command** once to start recording, press again to stop and insert the text; press again to cancel.
- On first launch, grant Microphone, Accessibility, and Screen Recording permissions from the in-app settings page. A failed permission check is not the same as "not granted"; return to the app to refresh status after changing permissions.
- Overview cards show the real load state of both models (missing / loading / loaded / failed).

## Privacy

- The microphone is used for recording; Accessibility for text insertion and cursor/focus reading; Screen Recording for local OCR. All inference runs on-device.
- The app's only network action is the first-run model download (static files hosted on HuggingFace and its mirror, pinned revisions + SHA-256 checks); beyond that it opens no network connections, and your audio and text are never uploaded.
- OCR and cursor context are only captured when dictation triggers them; sensitive apps, password fields, SecureInput, and focus/window mismatches are refused. Screenshots are processed in memory only and never saved.
- The optional "polish diagnostics" in history store the actual final prompt (which may include cursor, OCR, and previous-turn text) locally in `history.json` only — never uploaded, never written to ordinary logs; debug logs are redacted.
- The app never resets system permissions or touches user data, certificates, or the keychain on its own.

## Repository layout

| Path | Purpose |
|---|---|
| `src/` | React UI |
| `crates/openless-core/` | Dictation, context, history, diagnostics, regression tests |
| `src-tauri/src/` | macOS host, ASR, LLM, OCR, permissions |
| `src-tauri/resources/qwen3-mlx/` | Bundled standalone Python runtime & workers (`runtime/` is built by script, not checked in) |
| `src-tauri/vendor/qwen-asr` | Upstream Qwen3-ASR C implementation submodule ([Open-Less/qwen-asr](https://github.com/Open-Less/qwen-asr), MIT) |
| `scripts/` | Build and test entry points |
| `docs/upstream-sync.md` | Log of upstream sync and trimming decisions |

## Relationship to upstream

This repository is a trimmed fork of [Open-Less/openless](https://github.com/Open-Less/openless) as of fork point `82aa33ff` (2026-09-07): mobile platforms (Android/iOS/Harmony), cloud ASR/LLM channels, the style system, translation, Marketplace, and similar features were removed, pinning the product to local-only macOS arm64 dictation. Adoption and trimming decisions are documented per-item in [docs/upstream-sync.md](docs/upstream-sync.md).

## Contributing

Issues and PRs are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md). Security disclosures: [SECURITY.md](SECURITY.md).

## License & acknowledgements

Released under the [AGPL-3.0](LICENSE). Based on [Open-Less/openless](https://github.com/Open-Less/openless) (original project and its contributors retain their copyrights).

Thanks to: [Tauri](https://tauri.app/), [llama.cpp](https://github.com/ggml-org/llama.cpp), [MLX](https://github.com/ml-explore/mlx), [mlx-qwen3-asr](https://github.com/moona3k/mlx-qwen3-asr), [RapidOCR](https://github.com/RapidAI/RapidOCR), [Qwen3-ASR](https://github.com/QwenLM/Qwen3-ASR), [Gemma](https://ai.google.dev/gemma/docs).
