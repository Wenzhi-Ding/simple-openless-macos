//! 静态离线守卫:断言「纯本地」改造不被回退。
//!
//! 三层检查(全部纯静态、无网络):
//! 1. 两份 Cargo manifest 不再声明任何网络/服务端依赖;
//! 2. Rust 源码里不再出现联网客户端/监听器 API 的调用面;
//! 3. 已删除的云功能入口符号没有回流。
//!
//! 新增本地功能不需要改这里;任何重新引入联网路径的改动都必须先改本测试,
//! 让评审在 diff 里看到「为什么需要网络」。

use std::path::Path;

fn repo_root() -> &'static Path {
    // CARGO_MANIFEST_DIR = crates/openless-core
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap()
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
fn manifests_declare_no_network_dependencies() {
    for relative in ["crates/openless-core/Cargo.toml", "src-tauri/Cargo.toml"] {
        let manifest = std::fs::read_to_string(repo_root().join(relative))
            .unwrap_or_else(|error| panic!("read {relative} failed: {error}"));
        let deps = dependency_sections(&manifest);
        for token in BANNED_MANIFEST_TOKENS {
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
    assert!(!conf.contains("updater"), "tauri.conf.json 出现 updater 配置");
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
        let start = rest[..index].rfind(|c| c == '"' || c == ' ' || c == '\n').map(|i| i + 1).unwrap_or(0);
        let scheme_start = rest[start..].find("http").map(|i| start + i).unwrap_or(index);
        let tail = &rest[scheme_start..];
        let url_len = tail
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
            .unwrap_or(tail.len());
        let url = &tail[..url_len];
        // 按主机+可选路径前缀匹配:schema 文档地址带路径也放行。
        let scheme_end = url.find("://").map(|i| i + 3).unwrap_or(0);
        let host_end = url[scheme_end..].find('/').map(|i| scheme_end + i).unwrap_or(url.len());
        let host = &url[..host_end];
        let allowed = allowed_hosts.contains(&host) || allowed_hosts.iter().any(|a| url.starts_with(a));
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
                for (token, label) in BANNED_SOURCE_TOKENS {
                    assert!(
                        !source.contains(token),
                        "{} 出现了 `{token}`({label});纯本地构建禁止联网路径回流入 {}",
                        path.display(),
                        path.display()
                    );
                }
            }
        }
    }
}
