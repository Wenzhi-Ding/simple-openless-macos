//! Pure Android updater helpers (manifest URLs, version compare). Testable on all targets.

pub const MIRROR_BASE: &str = "https://fastgit.cc/https://github.com/Open-Less/openless";
pub const DIRECT_BASE: &str = "https://github.com/Open-Less/openless";

/// Must match `plugins.updater.pubkey` in `tauri.conf.json` (TAURI_SIGNING_PRIVATE_KEY pair).
pub const UPDATER_PUBKEY_B64: &str =
    "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDFERUFBODAzNTY0QzMyM0YKUldRL01reFdBNmpxSGE1K0JadlpONXNWTzhJcGZCRGxjUVdIWExNNFJpeUNsSGZwazdlQThhemkK";

/// Returned when Kotlin `installApk` cannot open the system installer (e.g. missing install permission).
pub const INSTALLER_NOT_OPENED_MSG: &str =
    "无法打开系统安装器：请先在系统设置中允许 OpenLess 安装未知应用，然后重新点击更新";

pub fn map_abi_to_arch(abi: &str) -> &'static str {
    match abi {
        "arm64-v8a" => "aarch64",
        "armeabi-v7a" => "armv7",
        "x86" => "i686",
        "x86_64" => "x86_64",
        _ => "aarch64",
    }
}

pub fn version_is_newer(remote: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u32> {
        v.split(|c| c == '.' || c == '-')
            .filter_map(|p| p.parse().ok())
            .collect()
    }
    let remote_parts = parts(remote);
    let current_parts = parts(current);
    let max = remote_parts.len().max(current_parts.len());
    for i in 0..max {
        let r = remote_parts.get(i).copied().unwrap_or(0);
        let c = current_parts.get(i).copied().unwrap_or(0);
        if r > c {
            return true;
        }
        if r < c {
            return false;
        }
    }
    false
}

pub fn stable_manifest_urls(arch: &str) -> Vec<String> {
    vec![
        format!("{MIRROR_BASE}/releases/latest/download/latest-android-{arch}-mirror.json"),
        format!("{DIRECT_BASE}/releases/latest/download/latest-android-{arch}.json"),
    ]
}

pub fn beta_manifest_urls(arch: &str, tag: &str) -> Vec<String> {
    vec![
        format!("{MIRROR_BASE}/releases/download/{tag}/latest-android-{arch}-beta-mirror.json"),
        format!("{DIRECT_BASE}/releases/download/{tag}/latest-android-{arch}-beta.json"),
    ]
}

/// Human-readable manifest fetch failure for UI tooltips.
pub fn format_manifest_error(status: u16, url: &str) -> String {
    if status == 404 {
        format!("更新清单不存在 (404): {url}")
    } else {
        format!("无法获取更新清单 (HTTP {status}): {url}")
    }
}
