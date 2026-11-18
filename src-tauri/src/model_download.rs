//! 首启模型下载——全应用唯一的网络通道。
//!
//! 2026-11-18 用户裁定：产品新增「引导页显式点击下载固定模型」能力（此前边界
//! 为无任何下载入口）。边界约束没有放松，只是收窄地开了一个口：
//!
//! - 这是唯一的联网模块；识别/润色推理路径仍完全离线，ASR worker 进程内
//!   禁网（socket 全拒）的 fail-closed 设计不变。
//! - 下载源钉死（pinned）：仓库、revision、文件清单、字节数与 SHA-256 全部
//!   写死在本文件，不调用任何动态目录/搜索/版本接口；改动 manifest 必须同步
//!   更新 README 的模型获取指引。
//! - 下载器不删除任何已存在的文件：目标目录里出现清单之外的文件、或同名文件
//!   尺寸不符时直接失败并列出冲突，由用户自行清理（fail closed）。
//! - 完整性：每个文件流式 SHA-256 对账，通过后原子 rename 到最终路径；
//!   失败不落半份模型（`.partial` 保留供续传，取消/失败可重试）。
//! - 下载只在用户点击「获取模型」后开始，与权限请求的「用户主动触发」一致。
//!
//! `crates/openless-core/tests/offline_guard.rs` 对本模块做了路径级豁免，
//! 豁免理由与评审提示记录在该测试文件。

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ── 钉死的下载清单（pinned manifest）────────────────────────────────────
// 校验值取自 HuggingFace API 与 pinned revision 实件（2026-11-18 核对）。
// GGUF 的 SHA-256 与 README 记录一致。改这里必须同步改 README 的模型获取指引。

/// ASR 固定模型目录名（数据目录 models/ 之下），与 qwen3_mlx 加载路径一致。
pub const ASR_MODEL_DIR_NAME: &str = "qwen3-asr-1.7b";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelKey {
    /// Qwen3-ASR 1.7B 全精度（bf16，约 4.1 GB）
    AsrBf16,
    /// Qwen3-ASR 1.7B 4bit 量化（约 1.6 GB）
    Asr4Bit,
    /// Gemma 4 E4B Q4_0 GGUF（约 4.6 GB）
    LlmGguf,
}

impl ModelKey {
    pub fn as_str(self) -> &'static str {
        match self {
            ModelKey::AsrBf16 => "asr-bf16",
            ModelKey::Asr4Bit => "asr-4bit",
            ModelKey::LlmGguf => "llm-gguf",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "asr-bf16" => Some(ModelKey::AsrBf16),
            "asr-4bit" => Some(ModelKey::Asr4Bit),
            "llm-gguf" => Some(ModelKey::LlmGguf),
            _ => None,
        }
    }

    fn is_asr(self) -> bool {
        matches!(self, ModelKey::AsrBf16 | ModelKey::Asr4Bit)
    }
}

struct PinnedFile {
    path: &'static str,
    size: u64,
    sha256: &'static str,
}

struct PinnedModel {
    repo: &'static str,
    revision: &'static str,
    files: &'static [PinnedFile],
}

/// bf16 与 4bit 的仓库/revision/清单来自 mlx-community 的 pinned 提交；
/// GGUF 来自 ggml-org 官方仓库的 pinned revision（与既有 SHA-256 记录一致）。
static ASR_BF16: PinnedModel = PinnedModel {
    repo: "mlx-community/Qwen3-ASR-1.7B-bf16",
    revision: "e1f6c266914abc5a46e8756e02580f834a6cf8a7",
    files: &[
        PinnedFile { path: "chat_template.json", size: 1_161, sha256: "75a8cfca24f00de72d796fbfed6858fc9614ef3dabd8696684cc3bc03a9c58ff" },
        PinnedFile { path: "config.json", size: 6_983, sha256: "62fe9a2ba2de536820229e126fdfb35f9910b9a337dc46d3737c3b1ac0a9caa9" },
        PinnedFile { path: "generation_config.json", size: 142, sha256: "1da527824d81e07118facff437e03f2e24a23311e3bdeb2368973fe77e5f275c" },
        PinnedFile { path: "merges.txt", size: 1_671_853, sha256: "8831e4f1a044471340f7c0a83d7bd71306a5b867e95fd870f74d0c5308a904d5" },
        PinnedFile { path: "model.safetensors", size: 4_076_186_653, sha256: "2f080a3b769ae469aeaaa2dcb9e13a94141e54c9e6d5a7aa63392e0dc5a51789" },
        PinnedFile { path: "model.safetensors.index.json", size: 51_384, sha256: "a3ee6037ec82e95dc8577216ac84cf0b3c92cfefc099cf20b2296571f2fb3fc0" },
        PinnedFile { path: "preprocessor_config.json", size: 330, sha256: "45e120a4eda2c20c5d7f2ea9354e63536bf35e27aa573fb7cdf78017b378770d" },
        PinnedFile { path: "tokenizer_config.json", size: 12_487, sha256: "4942d005604266809309cabc9f4e9cb89ce855d59b14681fdc0e1cc62ea26c4c" },
        PinnedFile { path: "vocab.json", size: 2_776_833, sha256: "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910" },
    ],
};

static ASR_4BIT: PinnedModel = PinnedModel {
    repo: "mlx-community/Qwen3-ASR-1.7B-4bit",
    revision: "78a389c776a5483b2d0d4ea5494e11012e0d6159",
    files: &[
        PinnedFile { path: "chat_template.json", size: 1_161, sha256: "75a8cfca24f00de72d796fbfed6858fc9614ef3dabd8696684cc3bc03a9c58ff" },
        PinnedFile { path: "config.json", size: 7_188, sha256: "539c6c9d482349066e2e740241e39896f3bf4de0866c245f6539c85dbb19d93c" },
        PinnedFile { path: "generation_config.json", size: 142, sha256: "1da527824d81e07118facff437e03f2e24a23311e3bdeb2368973fe77e5f275c" },
        PinnedFile { path: "merges.txt", size: 1_671_853, sha256: "8831e4f1a044471340f7c0a83d7bd71306a5b867e95fd870f74d0c5308a904d5" },
        PinnedFile { path: "model.safetensors", size: 1_603_081_617, sha256: "9848eaf7a5c1589c671b35035ac27b72e248dd0c604eacae547e7e403d29db45" },
        PinnedFile { path: "model.safetensors.index.json", size: 78_968, sha256: "2612ab715223843b3dec0742737bc85f50914ca1484ade45955f99b4e012f2bb" },
        PinnedFile { path: "preprocessor_config.json", size: 330, sha256: "45e120a4eda2c20c5d7f2ea9354e63536bf35e27aa573fb7cdf78017b378770d" },
        PinnedFile { path: "tokenizer_config.json", size: 12_487, sha256: "4942d005604266809309cabc9f4e9cb89ce855d59b14681fdc0e1cc62ea26c4c" },
        PinnedFile { path: "vocab.json", size: 2_776_833, sha256: "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910" },
    ],
};

static LLM_GGUF: PinnedModel = PinnedModel {
    repo: "ggml-org/gemma-4-E4B-it-GGUF",
    revision: "b8093469224f83f5c38f691eb906c380e9e63114",
    files: &[PinnedFile {
        path: "gemma-4-E4B-it-Q4_0.gguf",
        size: 4_590_807_392,
        sha256: "a555b900214b477d8880e7832e0b8925e139b0159640036b09fe472b6f2097f2",
    }],
};

static PINNED: [(ModelKey, &PinnedModel); 3] = [
    (ModelKey::AsrBf16, &ASR_BF16),
    (ModelKey::Asr4Bit, &ASR_4BIT),
    (ModelKey::LlmGguf, &LLM_GGUF),
];

fn pinned(key: ModelKey) -> &'static PinnedModel {
    PINNED
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, m)| *m)
        .expect("manifest covers every ModelKey")
}

fn pinned_total_bytes(key: ModelKey) -> u64 {
    pinned(key).files.iter().map(|f| f.size).sum()
}

/// 下载目标目录：ASR → models/qwen3-asr-1.7b/，LLM → models/llm/。
pub fn dest_dir(models_root: &Path, key: ModelKey) -> PathBuf {
    if key.is_asr() {
        models_root.join(ASR_MODEL_DIR_NAME)
    } else {
        models_root.join("llm")
    }
}

/// 下载源镜像。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mirror {
    /// 官方源 `huggingface.co`
    Huggingface,
    /// 国内社区镜像 `hf-mirror.com`（非官方但长期稳定）
    HfMirror,
}

impl Mirror {
    pub fn base_url(self) -> &'static str {
        match self {
            Mirror::Huggingface => "https://huggingface.co",
            Mirror::HfMirror => "https://hf-mirror.com",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "hf-mirror" => Mirror::HfMirror,
            _ => Mirror::Huggingface,
        }
    }
}

fn file_url(mirror: Mirror, key: ModelKey, path: &str) -> String {
    let model = pinned(key);
    format!(
        "{}/{}/resolve/{}/{}",
        mirror.base_url(),
        model.repo,
        model.revision,
        path
    )
}

fn partial_path(dest: &Path) -> PathBuf {
    dest.with_extension(format!(
        "{}.partial",
        dest.extension().and_then(|e| e.to_str()).unwrap_or("bin")
    ))
}

/// 目标卷剩余空间（字节）。仅 macOS（statfs），与产品目标平台一致。
fn available_bytes(path: &Path) -> io::Result<u64> {
    use std::ffi::CString;
    let c = CString::new(path.to_string_lossy().as_bytes())?;
    let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: fs 指向已分配的 statfs，path 是合法 C 字符串。
    let rc = unsafe { libc::statfs(c.as_ptr(), &mut fs) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fs.f_bavail as u64 * fs.f_bsize as u64)
}

/// 目标目录里与清单冲突的既有文件：清单之外的多余文件，或同名但尺寸不符的文件。
/// 下载器不删除任何既有文件——冲突一律报错交用户处理（fail closed）。
fn conflicts_in_dest(dir: &Path, key: ModelKey) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let known: std::collections::HashSet<&str> = pinned(key)
        .files
        .iter()
        .map(|f| f.path)
        .chain([".DS_Store"])
        .collect();
    let mut conflicts = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if name.ends_with(".partial") || known.contains(name) {
            continue;
        }
        conflicts.push(name.to_string());
    }
    for file in pinned(key).files {
        let path = dir.join(file.path);
        if let Ok(meta) = std::fs::metadata(&path) {
            // 同名同尺寸视为既有着落（完整校验在下载器内做）；尺寸不同则视为冲突，
            // 避免覆盖用户手放的权重。
            if meta.is_file() && meta.len() != file.size {
                conflicts.push(format!(
                    "{}（尺寸 {} ≠ 清单 {}）",
                    file.path,
                    meta.len(),
                    file.size
                ));
            }
        }
    }
    conflicts.sort();
    conflicts.dedup();
    conflicts
}

/// 已安装（清单内每个文件都存在且尺寸吻合）。完整性由下载期的 SHA-256 对账
/// 保证；这里不做全量重哈希——4GB 级重读不能挂在状态查询路径上。
fn is_installed(dir: &Path, key: ModelKey) -> bool {
    pinned(key).files.iter().all(|f| {
        std::fs::metadata(dir.join(f.path))
            .map(|m| m.is_file() && m.len() == f.size)
            .unwrap_or(false)
    })
}

// ── 进度事件 ───────────────────────────────────────────────────────────

pub const PROGRESS_EVENT: &str = "model-download:progress";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub key: String,
    pub file: String,
    pub file_index: usize,
    pub file_count: usize,
    pub bytes_downloaded: u64,
    pub bytes_total: u64,
    pub phase: DownloadPhase,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DownloadPhase {
    Started,
    Progress,
    Finished,
    Cancelled,
    Failed,
}

type EmitFn<'a> = dyn Fn(&DownloadProgress) + Send + Sync + 'a;

#[allow(clippy::too_many_arguments)]
fn emit_to(
    emit: &EmitFn<'_>,
    key: ModelKey,
    file: &str,
    file_index: usize,
    file_count: usize,
    bytes_done: u64,
    bytes_total: u64,
    phase: DownloadPhase,
    error: Option<String>,
) {
    emit(&DownloadProgress {
        key: key.as_str().into(),
        file: file.into(),
        file_index,
        file_count,
        bytes_downloaded: bytes_done,
        bytes_total,
        phase,
        error,
    });
}

// ── 下载器核心 ─────────────────────────────────────────────────────────

const CHUNK: u64 = 32 * 1024 * 1024;
const MAX_CHUNK_RETRIES: u32 = 4;

fn new_http_client() -> Result<reqwest::Client> {
    // native-tls（macOS = SecureTransport）：HF LFS CDN 常不发 TLS close_notify
    // 就关 TCP，rustls 0.22+ 视为致命 unexpected_eof，SecureTransport 容错
    // （上游实践）。必须带 User-Agent——HF 对无 UA 流量限速；连接池超时调短，
    // 避免复用已被 CDN 关闭的连接。
    Ok(reqwest::Client::builder()
        .use_native_tls()
        .user_agent(concat!(
            "openless/",
            env!("CARGO_PKG_VERSION"),
            "; model-download"
        ))
        .connect_timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(15))
        .build()
        .context("build download client failed")?)
}

/// 单文件下载：`.partial` 续传 + Range 分块重试 + SHA-256 流式校验 + 原子 rename。
/// `url` 由调用方按 manifest 拼（便于测试注入本地服务器）；
/// `bytes_before` 是同一清单里排在它之前的文件累计字节数（进度合计用）。
#[allow(clippy::too_many_arguments)]
async fn download_one(
    client: &reqwest::Client,
    url: &str,
    file: &PinnedFile,
    dest: &Path,
    bytes_before: u64,
    bytes_total: u64,
    key: ModelKey,
    cancel: &AtomicBool,
    emit: &EmitFn<'_>,
    file_index: usize,
    file_count: usize,
) -> Result<()> {
    let partial = partial_path(dest);
    let progress = |have: u64| {
        emit_to(
            emit,
            key,
            file.path,
            file_index,
            file_count,
            bytes_before + have,
            bytes_total,
            DownloadPhase::Progress,
            None,
        );
    };

    // 续传基线：`.partial` 已有前缀先计入哈希，续传只补尾部。
    let mut hasher = Sha256::new();
    let mut have = 0u64;
    if partial.exists() {
        let mut reader = tokio::io::BufReader::new(tokio::fs::File::open(&partial).await?);
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            have += n as u64;
        }
    }
    if have > file.size {
        // 残留的 .partial 比目标还长：内容不可信，弃用续传。
        tokio::fs::remove_file(&partial).await?;
        hasher = Sha256::new();
        have = 0;
    }

    let mut chunk_retries: u32 = 0;
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(anyhow!("cancelled"));
        }
        let range = format!("bytes={have}-{}", file.size.saturating_sub(1));
        let response = match client.get(url).header("Range", &range).send().await {
            Ok(resp) => resp,
            Err(error) => return Err(anyhow!("GET {url} failed: {error}")),
        };
        let status = response.status();

        // 防御服务端忽略 Range 返回 200 + 全文件：续传场景会污染 append，
        // 截断 `.partial` 从头再来。start=0 时 200 是合法路径。
        if status == reqwest::StatusCode::OK && have > 0 {
            tokio::fs::remove_file(&partial).await?;
            hasher = Sha256::new();
            have = 0;
            continue;
        }
        if status != reqwest::StatusCode::OK && status != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(anyhow!("GET {url} returned unexpected HTTP {status}"));
        }

        let mut stream = response.bytes_stream();
        let mut out = tokio::fs::OpenOptions::new()
            .create(true)
            .append(have > 0)
            .write(true)
            .truncate(have == 0)
            .open(&partial)
            .await
            .with_context(|| format!("open .partial failed: {}", partial.display()))?;
        let mut received_this_report = 0u64;
        let mut io_error: Option<io::Error> = None;
        while let Some(chunk) = stream.next().await {
            if cancel.load(Ordering::SeqCst) {
                return Err(anyhow!("cancelled"));
            }
            match chunk {
                Ok(bytes) => {
                    if let Err(error) = out.write_all(&bytes).await {
                        io_error = Some(error);
                        break;
                    }
                    hasher.update(&bytes);
                    have += bytes.len() as u64;
                    received_this_report += bytes.len() as u64;
                    if received_this_report >= CHUNK {
                        received_this_report = 0;
                        progress(have);
                    }
                }
                Err(error) => {
                    io_error = Some(io::Error::other(error.to_string()));
                    break;
                }
            }
        }
        let flush_result = out.flush().await;
        drop(out);
        if io_error.is_none() {
            io_error = flush_result.err();
        }

        if io_error.is_none() && have == file.size {
            break;
        }
        if io_error.is_none() {
            // 流正常结束但字节数不足：服务端提前断流，按失败重试。
            io_error = Some(io::Error::other(format!(
                "stream ended early: have {have} of {}",
                file.size
            )));
        }
        let error = io_error.unwrap_or_else(|| io::Error::other("download interrupted"));
        if cancel.load(Ordering::SeqCst) {
            return Err(anyhow!("cancelled"));
        }
        chunk_retries += 1;
        if chunk_retries > MAX_CHUNK_RETRIES {
            return Err(anyhow!(
                "download failed after {MAX_CHUNK_RETRIES} retries at {have}/{} bytes: {error}",
                file.size
            ));
        }
        log::warn!(
            "[model-download] {}:{} interrupted at {have}/{} (retry {chunk_retries}): {error}",
            key.as_str(),
            file.path,
            file.size
        );
        tokio::time::sleep(Duration::from_millis(500u64.saturating_mul(1 << chunk_retries.min(4)))).await;
        progress(have);
    }

    // 完整性对账：哈希不符不落盘。
    let digest = hex(&hasher.finalize());
    if digest != file.sha256 {
        tokio::fs::remove_file(&partial).await?;
        return Err(anyhow!(
            "SHA-256 mismatch for {}: got {digest}, expected {}",
            file.path,
            file.sha256
        ));
    }
    tokio::fs::rename(&partial, dest)
        .await
        .with_context(|| format!("finalize {} failed", file.path))?;
    progress(file.size);
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 一次完整清单下载。`emit` 由调用方注入（命令层接 Tauri 事件，测试接断言）。
pub async fn run_download(
    models_root: &Path,
    key: ModelKey,
    mirror: Mirror,
    cancel: &AtomicBool,
    emit: &EmitFn<'_>,
) -> Result<()> {
    let dir = dest_dir(models_root, key);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("create model dir failed: {}", dir.display()))?;

    let conflicts = conflicts_in_dest(&dir, key);
    if !conflicts.is_empty() {
        return Err(anyhow!(
            "目标目录已有无法识别的文件，为避免覆盖用户内容已停止下载，请手动清理后再试：{}",
            conflicts.join("、")
        ));
    }

    if is_installed(&dir, key) {
        let total = pinned_total_bytes(key);
        emit_to(
            emit,
            key,
            "",
            0,
            pinned(key).files.len(),
            total,
            total,
            DownloadPhase::Finished,
            None,
        );
        return Ok(());
    }

    let total = pinned_total_bytes(key);
    let free = available_bytes(&dir).unwrap_or(u64::MAX);
    if free < total {
        return Err(anyhow!(
            "磁盘空间不足：需要约 {:.1} GB，当前剩余 {:.1} GB",
            total as f64 / 1024.0 / 1024.0 / 1024.0,
            free as f64 / 1024.0 / 1024.0 / 1024.0
        ));
    }

    emit_to(
        emit,
        key,
        "",
        0,
        pinned(key).files.len(),
        0,
        total,
        DownloadPhase::Started,
        None,
    );

    let client = new_http_client()?;
    let mut bytes_before = 0u64;
    for (index, file) in pinned(key).files.iter().enumerate() {
        if cancel.load(Ordering::SeqCst) {
            emit_to(
                emit,
                key,
                file.path,
                index,
                pinned(key).files.len(),
                bytes_before,
                total,
                DownloadPhase::Cancelled,
                None,
            );
            return Ok(());
        }
        let dest = dir.join(file.path);
        let installed = std::fs::metadata(&dest)
            .map(|m| m.is_file() && m.len() == file.size)
            .unwrap_or(false);
        if installed {
            bytes_before += file.size;
            continue;
        }
        let url = file_url(mirror, key, file.path);
        download_one(
            &client,
            &url,
            file,
            &dest,
            bytes_before,
            total,
            key,
            cancel,
            emit,
            index,
            pinned(key).files.len(),
        )
        .await?;
        bytes_before += file.size;
    }
    log::info!("[model-download] {} finished ({} bytes)", key.as_str(), total);
    emit_to(
        emit,
        key,
        "",
        pinned(key).files.len(),
        pinned(key).files.len(),
        total,
        total,
        DownloadPhase::Finished,
        None,
    );
    Ok(())
}

// ── Tauri 管理器 ───────────────────────────────────────────────────────

/// 每个模型键一个取消标志；同键并发请求只接受第一个。
#[derive(Default)]
pub struct DownloadManager {
    cancel_flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl DownloadManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// 启动下载；立即返回，进度经 `model-download:progress` 事件上报。
    /// 完成后触发引擎重预载（与启动预载同一入口，共享实例不会载入第二份）。
    pub fn start(&self, app: AppHandle, key: ModelKey, mirror: Mirror) -> Result<(), String> {
        let mut flags = self.cancel_flags.lock();
        if flags.contains_key(key.as_str()) {
            return Err("download already in progress".into());
        }
        let flag = Arc::new(AtomicBool::new(false));
        flags.insert(key.as_str().into(), Arc::clone(&flag));
        drop(flags);

        // tauri::async_runtime 而非 tokio::spawn：同步 command 上下文没有
        // tokio reactor，直接 spawn 会 panic。
        tauri::async_runtime::spawn(async move {
            let result = match crate::persistence::models_root() {
                Ok(root) => {
                    let emit = |progress: &DownloadProgress| {
                        let _ = app.emit(PROGRESS_EVENT, progress);
                    };
                    run_download(&root, key, mirror, &flag, &emit).await
                }
                Err(error) => Err(anyhow!("resolve models root failed: {error}")),
            };
            app.state::<DownloadManager>()
                .cancel_flags
                .lock()
                .remove(key.as_str());
            match result {
                Ok(()) => reload_engines(app),
                Err(error) if error.to_string() == "cancelled" => {
                    log::info!("[model-download] {} cancelled", key.as_str());
                    let _ = app.emit(
                        PROGRESS_EVENT,
                        &DownloadProgress {
                            key: key.as_str().into(),
                            file: String::new(),
                            file_index: 0,
                            file_count: 0,
                            bytes_downloaded: 0,
                            bytes_total: 0,
                            phase: DownloadPhase::Cancelled,
                            error: None,
                        },
                    );
                }
                Err(error) => {
                    log::error!("[model-download] {} failed: {error:#}", key.as_str());
                    let _ = app.emit(
                        PROGRESS_EVENT,
                        &DownloadProgress {
                            key: key.as_str().into(),
                            file: String::new(),
                            file_index: 0,
                            file_count: 0,
                            bytes_downloaded: 0,
                            bytes_total: 0,
                            phase: DownloadPhase::Failed,
                            error: Some(format!("{error:#}")),
                        },
                    );
                }
            }
        });
        Ok(())
    }

    pub fn cancel(&self, key: ModelKey) {
        if let Some(flag) = self.cancel_flags.lock().get(key.as_str()) {
            flag.store(true, Ordering::SeqCst);
        }
    }

    pub fn is_active(&self, key: ModelKey) -> bool {
        self.cancel_flags.lock().contains_key(key.as_str())
    }
}

/// 下载完成后重预载引擎。与启动预载同一个 `preload_local_models`：ASR 共享
/// 同一个 Qwen3MlxRuntime，LLM 共享同一个 LocalLlamaPolisher slot；缺模型的
/// 一侧 prepare 失败只记日志，不阻塞另一侧（与启动语义一致）。
fn reload_engines(app: AppHandle) {
    let coordinator = app.state::<Arc<crate::coordinator::Coordinator>>();
    coordinator.preload_local_models();
}

// ── 测试 ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn manifest_pins_expected_sources_and_sizes() {
        // 清单即合同：仓库/revision/字节数被外部文档（README）引用，这里锁死防漂移。
        assert_eq!(ASR_BF16.repo, "mlx-community/Qwen3-ASR-1.7B-bf16");
        assert_eq!(ASR_BF16.revision, "e1f6c266914abc5a46e8756e02580f834a6cf8a7");
        assert_eq!(ASR_4BIT.repo, "mlx-community/Qwen3-ASR-1.7B-4bit");
        assert_eq!(ASR_4BIT.revision, "78a389c776a5483b2d0d4ea5494e11012e0d6159");
        assert_eq!(LLM_GGUF.repo, "ggml-org/gemma-4-E4B-it-GGUF");
        assert_eq!(LLM_GGUF.revision, "b8093469224f83f5c38f691eb906c380e9e63114");

        let gguf = &LLM_GGUF.files[0];
        assert_eq!(gguf.size, 4_590_807_392);
        assert_eq!(
            gguf.sha256,
            "a555b900214b477d8880e7832e0b8925e139b0159640036b09fe472b6f2097f2"
        );
        assert_eq!(pinned_total_bytes(ModelKey::AsrBf16), 4_080_707_826);
        assert_eq!(pinned_total_bytes(ModelKey::Asr4Bit), 1_607_630_579);

        for (_, model) in PINNED {
            assert!(model
                .files
                .iter()
                .any(|f| f.path.ends_with(".safetensors") || f.path.ends_with(".gguf")));
            for f in model.files {
                assert_eq!(f.sha256.len(), 64, "{} hash shape", f.path);
                assert!(f.sha256.chars().all(|c| c.is_ascii_hexdigit()));
                assert!(f.size > 0);
            }
        }
    }

    #[test]
    fn dest_dirs_match_loader_layout() {
        let root = Path::new("/tmp/models-root");
        assert_eq!(
            dest_dir(root, ModelKey::AsrBf16),
            root.join("qwen3-asr-1.7b")
        );
        assert_eq!(dest_dir(root, ModelKey::LlmGguf), root.join("llm"));
    }

    #[test]
    fn urls_are_pinned_to_revision() {
        let url = file_url(
            Mirror::Huggingface,
            ModelKey::LlmGguf,
            "gemma-4-E4B-it-Q4_0.gguf",
        );
        assert!(url.contains("/resolve/b8093469224f83f5c38f691eb906c380e9e63114/"));
        let mirrored = file_url(Mirror::HfMirror, ModelKey::Asr4Bit, "config.json");
        assert!(mirrored.starts_with("https://hf-mirror.com/mlx-community/"));
    }

    #[test]
    fn foreign_files_in_dest_are_conflicts() {
        let dir = std::env::temp_dir().join(format!("openless-dl-conflict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 清单之外的文件（例如作者本地的双分片布局）
        std::fs::write(dir.join("model-00001-of-00002.safetensors"), b"x").unwrap();
        // 清单之外的普通文件
        std::fs::write(dir.join("config.json"), vec![0u8; 100]).unwrap();
        // 清单内同名但尺寸不符（走尺寸冲突分支，避免覆盖用户手放权重）
        std::fs::write(dir.join("gemma-4-E4B-it-Q4_0.gguf"), vec![0u8; 100]).unwrap();
        // 残留 .partial 不算冲突
        std::fs::write(dir.join("vocab.json.partial"), b"p").unwrap();
        let conflicts = conflicts_in_dest(&dir, ModelKey::LlmGguf);
        assert!(conflicts.iter().any(|c| c.contains("model-00001-of-00002")));
        assert!(conflicts.iter().any(|c| c == "config.json"));
        assert!(conflicts
            .iter()
            .any(|c| c.starts_with("gemma-4-E4B-it-Q4_0.gguf（尺寸")));
        assert!(!conflicts.iter().any(|c| c.contains("partial")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 极简本地 HTTP 服务器，供真实 download_one 走完整下载路径。
    /// mode：
    /// - "ok"：按 Range 回 206 剩余部分（start=0 时回 200 全量）；
    /// - "drop-first"：第一次请求只回 4KB 就断连，之后按 "ok" 行为（模拟 CDN 断流）；
    /// - "full200"：无视 Range 恒回 200 全量（模拟不支持 Range 的源）；
    /// - "corrupt"：按 Range 回 206，但内容与预期不符（模拟哈希对账失败）。
    struct TestServer {
        addr: std::net::SocketAddr,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl TestServer {
        fn spawn(mode: &'static str, content: Vec<u8>) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let handle = std::thread::spawn(move || {
                let served = std::sync::atomic::AtomicUsize::new(0);
                for stream in listener.incoming() {
                    let mut stream = stream.unwrap();
                    let mut buf = [0u8; 4096];
                    let _ = stream.read(&mut buf);
                    let request = String::from_utf8_lossy(&buf);
                    let start: u64 = request
                        .lines()
                        .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                        .and_then(|l| l.split('=').nth(1))
                        .and_then(|r| r.split('-').next())
                        .and_then(|s| s.trim().parse().ok())
                        .unwrap_or(0);
                    let total = content.len();
                    let first = served.fetch_add(1, Ordering::SeqCst) == 0;
                    let corrupt: Vec<u8> = vec![b'x'; total];
                    let body: &[u8] = match mode {
                        "full200" => &content,
                        "corrupt" => &corrupt[start as usize..],
                        _ => &content[start as usize..],
                    };
                    let drop_partial = mode == "drop-first" && first;
                    let status_line = if start == 0 || mode == "full200" {
                        "HTTP/1.1 200 OK"
                    } else {
                        "HTTP/1.1 206 Partial Content"
                    };
                    let mut header = format!("{status_line}\r\n");
                    if start > 0 && mode != "full200" {
                        header.push_str(&format!("Content-Range: bytes {start}-{}/{total}\r\n", total - 1));
                    }
                    let cut = if drop_partial { 4096.min(body.len()) } else { body.len() };
                    header.push_str(&format!("Content-Length: {}\r\n", body.len()));
                    header.push_str("\r\n");
                    let _ = stream.write_all(header.as_bytes());
                    let _ = stream.write_all(&body[..cut]);
                    // drop_partial：不带 shutdown 直接丢弃 socket，模拟半途断流。
                }
            });
            Self {
                addr,
                handle: Some(handle),
            }
        }
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            // 不 join：服务器线程阻塞在 accept 上不会自行退出，join 会死锁。
            // 测试进程存活期内多留一个监听中的 localhost 线程无害（每个服务器
            // 绑定独立端口），进程退出时统一回收。
            drop(self.handle.take());
        }
    }

    const TEST_SIZE: u64 = 16 * 1024;
    const TEST_SHA256: &str = "2e600e0c6431f914136c6a1885dde510028766429f6f201cb068de58b0a50894";
    fn test_pattern() -> Vec<u8> {
        (0..TEST_SIZE).map(|i| 65 + (i % 26) as u8).collect()
    }

    fn test_file() -> PinnedFile {
        PinnedFile {
            path: "weights.bin",
            size: TEST_SIZE,
            sha256: TEST_SHA256,
        }
    }

    fn record_phases() -> Arc<Mutex<Vec<DownloadPhase>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    async fn download_from(
        url: &str,
        base: &Path,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let file = test_file();
        let client = new_http_client()?;
        let phases: Arc<Mutex<Vec<DownloadPhase>>> = record_phases();
        let emit = |p: &DownloadProgress| phases.lock().push(p.phase);
        download_one(
            &client,
            url,
            &file,
            &base.join("weights.bin"),
            0,
            TEST_SIZE,
            ModelKey::AsrBf16,
            cancel,
            &emit,
            0,
            1,
        )
        .await
    }

    #[tokio::test]
    async fn resumes_after_connection_drop() {
        let (server, base) = {
            let dir = std::env::temp_dir().join(format!("openless-dl-drop-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            (TestServer::spawn("drop-first", test_pattern()), dir)
        };
        let cancel = AtomicBool::new(false);
        let url = format!("http://{}/weights.bin", server.addr);
        let result = download_from(&url, &base, &cancel).await;
        assert!(result.is_ok(), "resume after drop should succeed: {result:?}");
        let final_bytes = std::fs::read(base.join("weights.bin")).unwrap();
        assert_eq!(final_bytes, test_pattern());
        assert!(!partial_path(&base.join("weights.bin")).exists());
    }

    #[tokio::test]
    async fn full_200_response_restarts_from_zero() {
        let (server, base) = {
            let dir = std::env::temp_dir().join(format!("openless-dl-200-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            (TestServer::spawn("full200", test_pattern()), dir)
        };
        // 预置半份 .partial 模拟续传场景；服务器无视 Range 回全量 200。
        std::fs::write(partial_path(&base.join("weights.bin")), b"A").unwrap();
        let cancel = AtomicBool::new(false);
        let url = format!("http://{}/weights.bin", server.addr);
        let result = download_from(&url, &base, &cancel).await;
        assert!(result.is_ok(), "200-defence path should succeed: {result:?}");
        let final_bytes = std::fs::read(base.join("weights.bin")).unwrap();
        assert_eq!(final_bytes, test_pattern());
        assert!(!partial_path(&base.join("weights.bin")).exists());
    }

    #[tokio::test]
    async fn sha_mismatch_fails_without_install() {
        let (server, base) = {
            let dir = std::env::temp_dir().join(format!("openless-dl-bad-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            (TestServer::spawn("corrupt", test_pattern()), dir)
        };
        let cancel = AtomicBool::new(false);
        let url = format!("http://{}/weights.bin", server.addr);
        let result = download_from(&url, &base, &cancel).await;
        assert!(result.is_err(), "corrupt content must fail the hash check");
        assert!(!base.join("weights.bin").exists(), "mismatch must not install");
        assert!(!partial_path(&base.join("weights.bin")).exists(), "partial removed on mismatch");
    }

    #[test]
    fn available_bytes_reports_free_space() {
        let free = available_bytes(&std::env::temp_dir()).unwrap();
        assert!(free > 0);
    }
}
