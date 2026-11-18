//! 静态离线守卫:断言「纯本地」改造不被回退。
//!
//! 三层检查(全部纯静态、无网络):
//! 1. 两份 Cargo manifest 不再声明任何网络/服务端依赖(唯一豁免见下);
//! 2. Rust 源码里不再出现联网客户端/监听器 API 的调用面(唯一豁免见下);
//! 3. 已删除的云功能入口符号没有回流。
//!
//! 新增本地功能不需要改这里;任何重新引入联网路径的改动都必须先改本测试,
//! 让评审在 diff 里看到「为什么需要网络」。
//!
//! 现存豁免(2026-11-18 用户裁定):首启模型下载(`src-tauri/src/model_download.rs`)
//! 是全应用唯一网络通道——引导页显式点击后,按钉死的 revision + SHA-256 从
//! HuggingFace/镜像下载固定模型;识别/润色推理路径仍完全离线,ASR worker 进程内
//! 禁网的 fail-closed 设计不变。豁免清单与理由就登记在本文件,新增联网面必须
//! 先在此登记「为什么需要网络」,不接受未登记的例外。

use std::path::Path;

fn repo_root() -> &'static Path {
    // CARGO_MANIFEST_DIR = crates/openless-core
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
}

const BANNED_MANIFEST_TOKENS: &[&str] = &[
    "reqwest",
    "tungstenite",
    "axum",
    "hyper",
    "tower",
    "rustls",
    "rcgen",
    "local-ip-address",
    "tauri-plugin-updater",
    "tauri-plugin-shell",
    "minisign-verify",
    "foundry-local-sdk",
];

const BANNED_SOURCE_TOKENS: &[(&str, &str)] = &[
    ("reqwest::", "HTTP 客户端"),
    ("tungstenite", "WebSocket 客户端"),
    ("axum::", "HTTP 服务端"),
    ("rustls::", "TLS"),
    ("TcpListener::bind", "TCP 监听"),
    ("UdpSocket::bind", "UDP 监听"),
    ("MARKETPLACE_BASE_URL", "云市场端点"),
    ("SharedCloudTranscriptionEngine", "云端 ASR 引擎"),
    ("SharedCloudTextPolisher", "云端润色引擎"),
    ("RemoteInputService", "LAN 远程输入服务"),
    ("start_download", "模型下载"),
];

fn dependency_sections(manifest: &str) -> String {
    // 只检查 [dependencies] / [target...dependencies] 段,忽略 [patch] 等元数据。
    let mut captured = String::new();
    let mut in_deps = false;
    for line in manifest.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            in_deps = trimmed.contains("dependencies");
            continue;
        }
        if in_deps && !trimmed.starts_with('#') {
            captured.push_str(line);
            captured.push('\n');
        }
    }
    captured
}

#[test]
fn fixed_e4b_text_runtime_keeps_qwen_asr_and_no_multimodal_dependencies() {
    let host = std::fs::read_to_string(repo_root().join("src-tauri/src/local_llm.rs")).unwrap();
    let ui = std::fs::read_to_string(repo_root().join("src/lib/localModels.ts")).unwrap();
    let manifest = std::fs::read_to_string(repo_root().join("src-tauri/Cargo.toml")).unwrap();
    assert!(host.contains("pub const DEFAULT_MODEL_FILE: &str = \"gemma-4-E4B-it-Q4_0.gguf\";"));
    assert!(ui.contains("LLM_FIXED_NAME = 'Gemma 4 E4B'"));
    assert!(ui.contains("ASR_FIXED_NAME = 'Qwen3-ASR 1.7B'"));
    assert!(ui.contains("ASR_FIXED_PROVIDER = 'local-qwen3-mlx'"));
    assert!(manifest.contains("llama-cpp-2 = { version = \"0.1.156\""));
    assert!(!dependency_sections(&manifest).contains("mtmd"));
    assert!(!host.contains("mtmd::"));
}

#[test]
fn fixed_polish_ui_has_no_style_entry_points() {
    let shell =
        std::fs::read_to_string(repo_root().join("src/components/FloatingShell.tsx")).unwrap();
    let history = std::fs::read_to_string(repo_root().join("src/pages/History.tsx")).unwrap();
    // 快捷键设置节（含风格热键）已整体删除；录音快捷键唯一入口在 RecordingInputSection。
    assert!(!repo_root().join("src/pages/settings/ShortcutsSection.tsx").exists());
    let recording_input =
        std::fs::read_to_string(repo_root().join("src/pages/settings/RecordingInputSection.tsx"))
            .unwrap();
    assert!(!shell.contains("import { Style }"));
    assert!(!shell.contains("id: 'style'"));
    assert!(!shell.contains("onClick={onOpenStyle}"));
    assert!(!history.contains("history.repolish.pickStyle"));
    assert!(history.contains("repolish(session.rawTranscript)"));
    let overview = std::fs::read_to_string(repo_root().join("src/pages/Overview.tsx")).unwrap();
    for ui in [&history, &overview] {
        for obsolete in [
            "style.modes",
            "styleLabel",
            "modeLabel",
            "stylePackId",
            "listStylePacks",
            "setFilter",
        ] {
            assert!(!ui.contains(obsolete), "obsolete style UI: {obsolete}");
        }
    }
    assert!(recording_input.contains("setDictationHotkey"));
    assert!(!repo_root().join("src/pages/Style.tsx").exists());
}

/// 路径级豁免登记处。每条 = (相对路径, 允许的 token)。
/// 新增联网面必须先在这里登记理由并通过评审,否则守卫直接失败。
/// 理由(2026-11-18 用户裁定):首启模型下载是唯一网络通道,下载源钉死
/// revision + SHA-256,推理路径仍完全离线;TcpListener::bind 仅用于
/// #[cfg(test)] 的 localhost 假服务器,产品代码不监听任何端口。
const SOURCE_TOKEN_EXCEPTIONS: &[(&str, &[&str])] = &[
    ("src-tauri/src/model_download.rs", &["reqwest::", "TcpListener::bind"]),
];

#[test]
fn manifests_declare_no_network_dependencies() {
    for relative in ["crates/openless-core/Cargo.toml", "src-tauri/Cargo.toml"] {
        let manifest = std::fs::read_to_string(repo_root().join(relative))
            .unwrap_or_else(|error| panic!("read {relative} failed: {error}"));
        let deps = dependency_sections(&manifest);
        for token in BANNED_MANIFEST_TOKENS {
            // 唯一豁免:reqwest 仅允许出现在 src-tauri/Cargo.toml,供首启模型
            // 下载使用(理由同 SOURCE_TOKEN_EXCEPTIONS 登记处)。
            if *token == "reqwest" && relative == "src-tauri/Cargo.toml" {
                continue;
            }
            assert!(
                !deps.contains(token),
                "{relative} 的依赖里出现了网络库 `{token}`;纯本地构建禁止重新引入联网依赖"
            );
        }
    }
}

/// tauri.conf.json 不得再引入任何外域端点或 updater 配置。
/// 允许的白名单只有:JSON schema 文档地址、dev-only 的 localhost:1420、
/// 以及本机 IPC/资源协议(asset.localhost / ipc.localhost)。
#[test]
fn tauri_conf_has_no_external_endpoints_or_updater() {
    let conf = std::fs::read_to_string(repo_root().join("src-tauri/tauri.conf.json"))
        .expect("read tauri.conf.json failed");
    assert!(
        !conf.contains("updater"),
        "tauri.conf.json 出现 updater 配置"
    );
    // plugins 段必须为空对象(或不存在),禁止悄悄加回任何插件配置。
    if let Some(plugins_start) = conf.find("\"plugins\"") {
        let tail = conf[plugins_start..].trim_start_matches(|c: char| c != ':');
        assert!(
            tail.starts_with(": {}"),
            "tauri.conf.json 的 plugins 段必须为空对象(禁止 updater 等插件配置)"
        );
    }
    let allowed_hosts = [
        "https://schema.tauri.app",
        "http://localhost:1420",
        "http://asset.localhost",
        "http://ipc.localhost",
    ];
    let mut rest = conf.as_str();
    while let Some(index) = rest.find("://") {
        let start = rest[..index]
            .rfind(|c| c == '"' || c == ' ' || c == '\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let scheme_start = rest[start..]
            .find("http")
            .map(|i| start + i)
            .unwrap_or(index);
        let tail = &rest[scheme_start..];
        let url_len = tail
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(tail.len());
        let url = &tail[..url_len];
        // 按主机+可选路径前缀匹配:schema 文档地址带路径也放行。
        let scheme_end = url.find("://").map(|i| i + 3).unwrap_or(0);
        let host_end = url[scheme_end..]
            .find('/')
            .map(|i| scheme_end + i)
            .unwrap_or(url.len());
        let host = &url[..host_end];
        let allowed =
            allowed_hosts.contains(&host) || allowed_hosts.iter().any(|a| url.starts_with(a));
        assert!(
            allowed,
            "tauri.conf.json 出现外域端点 `{url}`;纯本地构建只允许 schema/dev/本机 IPC 白名单"
        );
        rest = &rest[scheme_start + url_len..];
    }
}

#[test]
fn sources_contain_no_network_call_surface() {
    let roots = [
        repo_root().join("crates/openless-core/src"),
        repo_root().join("src-tauri/src"),
    ];
    for root in roots {
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir)
                .unwrap_or_else(|error| panic!("read_dir {} failed: {error}", dir.display()))
                .flatten()
            {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let source = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("read {} failed: {error}", path.display()));
                // 相对仓库根的路径,用于豁免匹配。
                let relative = path
                    .strip_prefix(repo_root())
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                for (token, label) in BANNED_SOURCE_TOKENS {
                    let exempt = SOURCE_TOKEN_EXCEPTIONS.iter().any(|(file, tokens)| {
                        file == &relative && tokens.contains(token)
                    });
                    assert!(
                        exempt || !source.contains(token),
                        "{} 出现了 `{token}`({label});纯本地构建禁止联网路径回流。\
                         如确需联网,先在 offline_guard.rs 的 SOURCE_TOKEN_EXCEPTIONS \
                         登记理由再改代码",
                        path.display(),
                    );
                }
            }
        }
    }
}
