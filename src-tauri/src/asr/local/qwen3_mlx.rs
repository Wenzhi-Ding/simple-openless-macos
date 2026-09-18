//! Qwen3-ASR(MLX)本地引擎:常驻 Python 子进程 + stdio 换行分隔 JSON 协议。
//!
//! Runtime 与 provider 共享同一常驻 worker 和取消状态：
//! - `Qwen3MlxRuntime` 是 worker 进程与模型加载状态的单一持有者,
//!   不感知 Coordinator / UI / Tauri 事件;失败统一 `anyhow::Error` 上抛。
//! - `Qwen3MlxAsr` 作为 `Recorder::AudioConsumer` 缓冲 16kHz mono s16le PCM,
//!   停止录音后整段交给 worker 转写;`cancel()` 用代次隔离丢弃过期结果。
//!
//! worker 是 `resources/qwen3-mlx/worker/qwen3_worker.py`,运行在打包进 app 的
//! 独立 Python(python-build-standalone + mlx-qwen3-asr wheel)上。模型只从本地
//! 目录读取;子进程环境强制 HF_HUB_OFFLINE=1,不存在任何联网或监听路径。
//!
//! 失败语义:worker 启动/加载失败或中途退出 → runtime 记录错误并清空句柄,
//! 下一次使用会重新拉起;转写超时 → 判定 worker 卡死,杀进程并在下次重建。
//! 任何失败都会显式报错，不存在隐藏 fallback。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use parking_lot::Mutex;
use tokio::sync::oneshot;
use tokio::sync::Mutex as AsyncMutex;

use crate::asr::RawTranscript;

/// macOS MLX 引擎的 provider id(沿用上游历史 id,core 侧已有映射)。
pub const PROVIDER_ID: &str = "local-qwen3-mlx";
/// 本引擎当前唯一支持的模型(官方 Qwen/Qwen3-ASR-1.7B 权重)。
pub const SUPPORTED_MODEL_ID: &str = "qwen3-asr-1.7b";
/// worker 就绪(模型加载完成)等待上限。4.7GB 权重冷盘加载留足余量。
const WORKER_READY_TIMEOUT: Duration = Duration::from_secs(240);

#[derive(Clone, Debug, Default)]
pub struct Qwen3MlxStatus {
    pub available: bool,
    pub runtime_ready: bool,
    /// worker 正在拉起/载模型（预载或懒加载进行中）。
    pub loading: bool,
    pub active_model: String,
    pub loaded_model_id: Option<String>,
    pub error: Option<String>,
    pub last_prepare_ms: Option<u64>,
    pub last_transcribe_ms: Option<u64>,
    pub last_audio_ms: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Clone, Default)]
struct RuntimeDiagnostics {
    last_prepare_ms: Option<u64>,
    last_transcribe_ms: Option<u64>,
    last_audio_ms: Option<u64>,
    last_error: Option<String>,
}

type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>;
type StdinHandle = Arc<Mutex<ChildStdin>>;
type StderrTail = Arc<Mutex<String>>;

/// 一个正在运行的 worker 子进程及其 IO 通道。
struct WorkerProcess {
    child: Child,
    stdin: StdinHandle,
    pending: PendingMap,
    stderr_tail: StderrTail,
    model_dir: PathBuf,
    next_id: u64,
}

fn worker_is_alive(worker: &mut WorkerProcess) -> bool {
    worker
        .child
        .try_wait()
        .map(|status| status.is_none())
        .unwrap_or(false)
}

fn kill_worker(worker: &mut WorkerProcess) {
    let _ = worker.child.kill();
    let _ = worker.child.wait();
    fail_pending(&worker.pending, "worker killed");
}

fn fail_pending(pending: &PendingMap, reason: &str) {
    for (_, sender) in pending.lock().drain() {
        let _ = sender.send(serde_json::json!({
            "ok": false,
            "error": reason,
        }));
    }
}

fn worker_text(value: serde_json::Value) -> Result<String> {
    if value
        .get("ok")
        .and_then(|flag| flag.as_bool())
        .unwrap_or(false)
    {
        Ok(value
            .get("text")
            .and_then(|text| text.as_str())
            .unwrap_or_default()
            .to_string())
    } else {
        bail!(
            "worker error: {}",
            value
                .get("error")
                .and_then(|error| error.as_str())
                .unwrap_or("unknown worker error")
        )
    }
}

/// 跨会话单例。生命周期由 `AsyncMutex` 串行化,确保 ensure/release 不会并发。
pub struct Qwen3MlxRuntime {
    lifecycle: AsyncMutex<()>,
    cancel_prepare: AtomicBool,
    loading: AtomicBool,
    state: Mutex<Option<WorkerProcess>>,
    diagnostics: Mutex<RuntimeDiagnostics>,
}

/// 把 `loading` 置位并在任何退出路径（含错误/panic 展开）复位的守卫。
struct LoadingFlag<'a>(&'a AtomicBool);

impl<'a> LoadingFlag<'a> {
    fn acquire(flag: &'a AtomicBool) -> Self {
        flag.store(true, Ordering::SeqCst);
        Self(flag)
    }
}

impl Drop for LoadingFlag<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Default for Qwen3MlxRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl Qwen3MlxRuntime {
    pub fn new() -> Self {
        Self {
            lifecycle: AsyncMutex::new(()),
            cancel_prepare: AtomicBool::new(false),
            loading: AtomicBool::new(false),
            state: Mutex::new(None),
            diagnostics: Mutex::new(RuntimeDiagnostics::default()),
        }
    }

    /// 引擎只编入 macOS(MLX 依赖 Apple Silicon + Metal)。
    pub fn is_available(&self) -> bool {
        cfg!(target_os = "macos")
    }

    pub async fn status_snapshot(&self, active_model: &str) -> Qwen3MlxStatus {
        let runtime_ready = self
            .state
            .lock()
            .as_mut()
            .map(worker_is_alive)
            .unwrap_or(false);
        let diagnostics = self.diagnostics.lock().clone();
        let loading = self.loading.load(Ordering::SeqCst);
        Qwen3MlxStatus {
            available: self.is_available(),
            runtime_ready,
            loading,
            active_model: active_model.to_string(),
            loaded_model_id: runtime_ready.then(|| active_model.to_string()),
            error: diagnostics.last_error.clone(),
            last_prepare_ms: diagnostics.last_prepare_ms,
            last_transcribe_ms: diagnostics.last_transcribe_ms,
            last_audio_ms: diagnostics.last_audio_ms,
            last_error: diagnostics.last_error,
        }
    }

    pub async fn ensure_loaded(&self, model_dir: &Path) -> Result<String> {
        self.ensure_loaded_with_progress(SUPPORTED_MODEL_ID, model_dir.to_path_buf(), |_, _, _| {})
            .await
    }

    pub async fn ensure_loaded_with_progress<F>(
        &self,
        model_id: &str,
        model_dir: PathBuf,
        progress: F,
    ) -> Result<String>
    where
        F: Fn(&'static str, Option<f64>, Option<String>) + Send + Sync + 'static,
    {
        let started = Instant::now();
        let result = self
            .ensure_loaded_inner(model_id, model_dir, &progress)
            .await;
        let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        match &result {
            Ok(loaded) => {
                log::info!("[qwen3-mlx] prepare finished model={loaded} elapsed_ms={elapsed_ms}");
                self.record_prepare_result(elapsed_ms, None);
            }
            Err(error) => {
                let message = format!("{error:#}");
                log::warn!(
                    "[qwen3-mlx] prepare failed model={model_id} elapsed_ms={elapsed_ms} error={message}"
                );
                self.record_prepare_result(elapsed_ms, Some(message));
            }
        }
        result
    }

    async fn ensure_loaded_inner<F>(
        &self,
        model_id: &str,
        model_dir: PathBuf,
        progress: &F,
    ) -> Result<String>
    where
        F: Fn(&'static str, Option<f64>, Option<String>),
    {
        if model_id != SUPPORTED_MODEL_ID {
            bail!("Qwen3-ASR MLX engine supports {SUPPORTED_MODEL_ID} only (requested {model_id})");
        }
        let _lifecycle = self.lifecycle.lock().await;
        let _loading = LoadingFlag::acquire(&self.loading);
        self.cancel_prepare.store(false, Ordering::SeqCst);
        if let Some(worker) = self.state.lock().as_mut() {
            if worker_is_alive(worker) && worker.model_dir == model_dir {
                progress("finished", Some(100.0), None);
                return Ok(model_id.to_string());
            }
        }
        self.check_prepare_cancelled()?;
        if !model_dir.join("config.json").is_file() {
            bail!(
                "Qwen3-ASR model directory is missing config.json: {}",
                model_dir.display()
            );
        }
        if !model_dir
            .read_dir()
            .map(|entries| {
                entries.filter_map(|entry| entry.ok()).any(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.ends_with(".safetensors"))
                })
            })
            .unwrap_or(false)
        {
            bail!(
                "Qwen3-ASR model directory has no safetensors weights: {}",
                model_dir.display()
            );
        }
        self.check_prepare_cancelled()?;
        progress("runtime", Some(100.0), None);
        progress("load", Some(0.0), None);
        let (mut worker, ready_rx) = spawn_worker(&model_dir)?;
        self.check_prepare_cancelled()?;
        let wait = tokio::time::timeout(WORKER_READY_TIMEOUT, ready_rx).await;
        match wait {
            Ok(Ok(ready)) => match ready {
                Ok(_model) => {
                    *self.state.lock() = Some(worker);
                    progress("load", Some(100.0), None);
                    progress("finished", Some(100.0), None);
                    Ok(model_id.to_string())
                }
                Err(error) => {
                    let stderr = worker.stderr_tail.lock().clone();
                    kill_worker(&mut worker);
                    bail!("Qwen3-ASR worker failed to load model: {error}; stderr tail: {stderr}");
                }
            },
            Ok(Err(_channel_closed)) => {
                let stderr = worker.stderr_tail.lock().clone();
                kill_worker(&mut worker);
                bail!("Qwen3-ASR worker closed before ready; stderr tail: {stderr}");
            }
            Err(_timeout) => {
                let stderr = worker.stderr_tail.lock().clone();
                kill_worker(&mut worker);
                bail!(
                    "Qwen3-ASR worker did not become ready within {}s; stderr tail: {stderr}",
                    WORKER_READY_TIMEOUT.as_secs()
                );
            }
        }
    }

    /// 整段 PCM batch 转写。未加载时先拉起 worker(懒加载;首次听写需要等待
    /// 模型加载,与 worker 的懒加载行为一致)。
    pub async fn transcribe_pcm(
        &self,
        model_dir: &Path,
        pcm: &[u8],
        language_hint: Option<&str>,
        audio_timeout: Duration,
    ) -> Result<String> {
        if pcm.is_empty() {
            return Ok(String::new());
        }
        let audio_ms = pcm_duration_ms(pcm);
        self.ensure_loaded(model_dir).await?;
        let mut payload = serde_json::json!({
            "op": "transcribe",
            "pcm_base64": base64::engine::general_purpose::STANDARD.encode(pcm),
        });
        if let Some(language) = language_hint.filter(|value| !value.trim().is_empty()) {
            payload["language"] = serde_json::json!(language);
        }
        let started = Instant::now();
        let result = self.worker_roundtrip(payload, audio_timeout).await;
        let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        let result = result.and_then(worker_text);
        match &result {
            Ok(text) => {
                log::info!(
                    "[qwen3-mlx] transcribe finished audio_ms={audio_ms} elapsed_ms={elapsed_ms} text_chars={}",
                    text.chars().count()
                );
                self.record_transcribe_result(audio_ms, elapsed_ms, None);
            }
            Err(error) => {
                let message = format!("{error:#}");
                log::warn!(
                    "[qwen3-mlx] transcribe failed audio_ms={audio_ms} elapsed_ms={elapsed_ms} error={message}"
                );
                self.record_transcribe_result(audio_ms, elapsed_ms, Some(message));
            }
        }
        result
    }

    /// 发送请求并等待响应;超时或通道断开视为 worker 异常,杀掉进程以便下次重建。
    async fn worker_roundtrip(
        &self,
        payload: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value> {
        // 短持锁:分配请求 id、登记 oneshot、编码行,不在锁内 await。
        let (request_id, receiver, stdin, stderr_tail, line) = {
            let mut state = self.state.lock();
            let worker = state.as_mut().context("Qwen3-ASR worker is not running")?;
            let request_id = worker.next_id;
            worker.next_id += 1;
            let mut payload = payload;
            if let Some(object) = payload.as_object_mut() {
                object.insert("id".into(), serde_json::json!(request_id));
            }
            let line = serde_json::to_string(&payload)
                .map_err(|error| anyhow::anyhow!("serialize worker request failed: {error}"))?;
            let (tx, rx) = oneshot::channel();
            worker.pending.lock().insert(request_id, tx);
            (
                request_id,
                rx,
                Arc::clone(&worker.stdin),
                Arc::clone(&worker.stderr_tail),
                line,
            )
        };
        // 写 stdin 可能因管道背压阻塞(base64 音频可达数 MB),放到阻塞线程。
        let write_result = tokio::task::spawn_blocking(move || {
            let mut guard = stdin.lock();
            guard
                .write_all(line.as_bytes())
                .and_then(|_| guard.write_all(b"\n"))
                .and_then(|_| guard.flush())
        })
        .await
        .map_err(|error| anyhow::anyhow!("worker stdin write join failed: {error:#}"))?;
        if let Err(error) = write_result {
            let _ = self.state.lock().as_mut().map(|worker| {
                worker.pending.lock().remove(&request_id);
            });
            bail!("worker stdin write failed: {error}");
        }
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_cancelled)) => {
                let stderr = stderr_tail.lock().clone();
                self.kill_state_worker();
                bail!("Qwen3-ASR worker dropped the request; stderr tail: {stderr}");
            }
            Err(_) => {
                let stderr = stderr_tail.lock().clone();
                self.kill_state_worker();
                bail!(
                    "Qwen3-ASR transcription timed out after {}s; worker was restarted for the next use; stderr tail: {stderr}",
                    timeout.as_secs()
                );
            }
        }
    }

    fn kill_state_worker(&self) {
        let mut state = self.state.lock();
        if let Some(mut worker) = state.take() {
            kill_worker(&mut worker);
        }
    }

    pub fn request_cancel_prepare(&self) {
        self.cancel_prepare.store(true, Ordering::SeqCst);
    }

    pub async fn release_now(&self) -> Result<()> {
        let _lifecycle = self.lifecycle.lock().await;
        self.kill_state_worker();
        Ok(())
    }

    fn check_prepare_cancelled(&self) -> Result<()> {
        if self.cancel_prepare.load(Ordering::SeqCst) {
            bail!("Qwen3-ASR prepare cancelled");
        }
        Ok(())
    }

    fn record_prepare_result(&self, elapsed_ms: u64, error: Option<String>) {
        let mut diagnostics = self.diagnostics.lock();
        diagnostics.last_prepare_ms = Some(elapsed_ms);
        diagnostics.last_error = error;
    }

    fn record_transcribe_result(&self, audio_ms: u64, elapsed_ms: u64, error: Option<String>) {
        let mut diagnostics = self.diagnostics.lock();
        diagnostics.last_audio_ms = Some(audio_ms);
        diagnostics.last_transcribe_ms = Some(elapsed_ms);
        diagnostics.last_error = error;
    }
}

/// 解析 worker 可执行文件与脚本位置。
/// 顺序:环境变量覆盖(开发)→ exe 祖先目录中的 `resources/qwen3-mlx`
/// (打包后为 Contents/Resources;qwen3-mlx 由 tauri resources 映射进去)。
pub fn resolve_worker_locations() -> Result<(PathBuf, PathBuf)> {
    resolve_runtime_worker("OPENLESS_QWEN3_MLX_WORKER", "qwen3_worker.py")
}

/// 同一个打包运行时里的 OCR worker(ocr_worker.py)解析入口。
pub fn resolve_ocr_worker_locations() -> Result<(PathBuf, PathBuf)> {
    resolve_runtime_worker("OPENLESS_OCR_WORKER", "ocr_worker.py")
}

fn resolve_runtime_worker(worker_env: &str, worker_name: &str) -> Result<(PathBuf, PathBuf)> {
    if let Ok(runtime_dir) = std::env::var("OPENLESS_QWEN3_MLX_RUNTIME") {
        let runtime_dir = PathBuf::from(runtime_dir.trim());
        let python = runtime_dir.join("bin").join("python3");
        if python.is_file() {
            let worker = std::env::var(worker_env)
                .ok()
                .map(|value| PathBuf::from(value.trim()))
                .filter(|path| path.is_file())
                .unwrap_or_else(|| default_worker_script_path(&runtime_dir, worker_name));
            if worker.is_file() {
                return Ok((python, worker));
            }
        }
    }
    let exe =
        std::env::current_exe().context("cannot locate current executable for the bundled runtime")?;
    for ancestor in exe.ancestors().skip(1) {
        // 打包后:Resources/resources/qwen3-mlx(数组形式 resources 保留相对路径);
        // 开发:src-tauri/resources/qwen3-mlx(exe 在 target/{debug,release} 下)。
        for candidate in [
            ancestor.join("resources").join("qwen3-mlx"),
            ancestor.join("Resources").join("qwen3-mlx"),
            ancestor
                .join("Resources")
                .join("resources")
                .join("qwen3-mlx"),
        ] {
            let python = candidate.join("runtime").join("bin").join("python3");
            let worker = candidate.join("worker").join(worker_name);
            if python.is_file() && worker.is_file() {
                return Ok((python, worker));
            }
        }
    }
    bail!(
        "bundled Python runtime not found next to {} (expected resources/qwen3-mlx/runtime)",
        exe.display()
    )
}

fn default_worker_script_path(runtime_dir: &Path, worker_name: &str) -> PathBuf {
    runtime_dir
        .parent()
        .unwrap_or(runtime_dir)
        .join("worker")
        .join(worker_name)
}

fn spawn_worker(
    model_dir: &Path,
) -> Result<(WorkerProcess, oneshot::Receiver<Result<String, String>>)> {
    let (python, worker_script) = resolve_worker_locations()
        .with_context(|| "the bundled Qwen3-ASR MLX runtime is missing; reinstall the app")?;
    let mut command = Command::new(&python);
    command
        .arg("-I")
        .arg("-B")
        .arg(&worker_script)
        .arg(model_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HF_HUB_OFFLINE", "1")
        .env("TRANSFORMERS_OFFLINE", "1")
        .env(
            "HF_HOME",
            std::env::temp_dir().join("openless-qwen3-mlx-hf-home"),
        )
        .env("PYTHONUNBUFFERED", "1");
    // 隔离宿主可能注入的 Python 配置,保证行为可复现。
    for key in [
        "PYTHONPATH",
        "PYTHONHOME",
        "PYTHONSTARTUP",
        "PYTHONUSERBASE",
    ] {
        command.env_remove(key);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("spawn Qwen3-ASR worker failed: {}", python.display()))?;
    let stdin = child.stdin.take().context("worker stdin was not piped")?;
    let stdout = child.stdout.take().context("worker stdout was not piped")?;
    let stderr = child.stderr.take().context("worker stderr was not piped")?;

    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let stderr_tail: StderrTail = Arc::new(Mutex::new(String::new()));
    let (ready_tx, ready_rx) = oneshot::channel::<Result<String, String>>();

    let stdout_pending = Arc::clone(&pending);
    let ready_slot = Mutex::new(Some(ready_tx));
    std::thread::Builder::new()
        .name("openless-qwen3-mlx-stdout".into())
        .spawn(move || {
            let reader = BufReader::new(stdout);
            let ready_slot = ready_slot;
            for line in reader.split(b'\n') {
                let Ok(line) = line else { break };
                let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line) else {
                    continue;
                };
                match value.get("event").and_then(|event| event.as_str()) {
                    Some("ready") => {
                        if let Some(sender) = ready_slot.lock().take() {
                            let _ = sender.send(Ok(value
                                .get("model")
                                .and_then(|model| model.as_str())
                                .unwrap_or_default()
                                .to_string()));
                        }
                    }
                    Some("failed") => {
                        let error = value
                            .get("error")
                            .and_then(|error| error.as_str())
                            .unwrap_or("unknown startup failure")
                            .to_string();
                        if let Some(sender) = ready_slot.lock().take() {
                            let _ = sender.send(Err(error.clone()));
                        }
                        log::error!("[qwen3-mlx] worker startup failed: {error}");
                    }
                    _ => {
                        if let Some(id) = value.get("id").and_then(|id| id.as_u64()) {
                            if let Some(sender) = stdout_pending.lock().remove(&id) {
                                let _ = sender.send(value);
                            }
                        }
                    }
                }
            }
            // EOF:worker 退出。唤醒等待者并让在途请求立刻失败。
            if let Some(sender) = ready_slot.lock().take() {
                let _ = sender.send(Err("worker exited before signaling ready".into()));
            }
            fail_pending(&stdout_pending, "worker exited unexpectedly");
        })
        .expect("spawn qwen3 mlx stdout reader");

    let stderr_tail_clone = Arc::clone(&stderr_tail);
    std::thread::Builder::new()
        .name("openless-qwen3-mlx-stderr".into())
        .spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.split(b'\n') {
                let Ok(line) = line else { break };
                let mut tail = stderr_tail_clone.lock();
                tail.push_str(&String::from_utf8_lossy(&line));
                tail.push('\n');
                // 只保留末尾 ~16KB,足够定位报错又不吃内存。
                if tail.len() > 16 * 1024 {
                    let drain = tail.len() - 16 * 1024;
                    tail.replace_range(..drain, "…");
                }
            }
        })
        .expect("spawn qwen3 mlx stderr reader");

    Ok((
        WorkerProcess {
            child,
            stdin: Arc::new(Mutex::new(stdin)),
            pending,
            stderr_tail,
            model_dir: model_dir.to_path_buf(),
            next_id: 1,
        },
        ready_rx,
    ))
}

// ─────────────────────────── provider(Recorder 消费端) ───────────────────────────

/// 本地批式转写 provider：缓冲 PCM，转写时整段交给 runtime；
/// `cancel()` 递增代次使过期结果被丢弃。
pub struct Qwen3MlxAsr {
    runtime: Arc<Qwen3MlxRuntime>,
    model_id: String,
    model_dir: PathBuf,
    language_hint: Option<String>,
    buffer: Mutex<Vec<u8>>,
    cancel_generation: AtomicU64,
}

impl Qwen3MlxAsr {
    pub fn new(
        runtime: Arc<Qwen3MlxRuntime>,
        model_id: String,
        model_dir: PathBuf,
        language_hint: Option<String>,
    ) -> Self {
        Self {
            runtime,
            model_id,
            model_dir,
            language_hint: normalize_language_hint(language_hint),
            buffer: Mutex::new(Vec::new()),
            cancel_generation: AtomicU64::new(0),
        }
    }

    #[allow(dead_code)]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    pub fn buffer_duration_ms(&self) -> u64 {
        pcm_duration_ms(&self.buffer.lock())
    }

    pub async fn transcribe(&self, audio_timeout: Duration) -> Result<RawTranscript> {
        let cancel_generation = self.cancel_generation.load(Ordering::SeqCst);
        let pcm = self.buffer.lock().clone();
        if pcm.is_empty() {
            return Ok(RawTranscript {
                text: String::new(),
                duration_ms: 0,
            });
        }
        let duration_ms = pcm_duration_ms(&pcm);
        // 送识别前先整流:整段低于语音门限时直接按空转写返回,不拉起 worker
        // (静音送进模型只会得到幻觉词或空串);有语音则裁掉首尾静音再转写。
        let conditioned = openless_core::audio::condition_dictation_pcm(&pcm);
        if !conditioned.has_speech {
            self.buffer.lock().clear();
            return Ok(RawTranscript {
                text: String::new(),
                duration_ms,
            });
        }
        let result = self
            .runtime
            .transcribe_pcm(
                &self.model_dir,
                &conditioned.pcm,
                self.language_hint.as_deref(),
                audio_timeout,
            )
            .await;
        if self.cancel_generation.load(Ordering::SeqCst) != cancel_generation {
            bail!("Qwen3-ASR transcription cancelled");
        }
        // 与 worker 对齐:进入推理后清空缓冲,避免重复消费。
        self.buffer.lock().clear();
        let text = result?;
        Ok(RawTranscript {
            text: text.trim().to_string(),
            duration_ms,
        })
    }

    pub fn cancel(&self) {
        self.cancel_generation.fetch_add(1, Ordering::SeqCst);
        self.runtime.request_cancel_prepare();
        self.buffer.lock().clear();
    }
}

impl crate::recorder::AudioConsumer for Qwen3MlxAsr {
    fn consume_pcm_chunk(&self, pcm: &[u8]) {
        self.buffer.lock().extend_from_slice(pcm);
    }
}

fn normalize_language_hint(raw: Option<String>) -> Option<String> {
    raw.map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
}

fn pcm_duration_ms(pcm: &[u8]) -> u64 {
    (pcm.len() as u64 / 2).saturating_mul(1_000) / 16_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::AudioConsumer;

    #[test]
    fn pcm_duration_matches_16k_mono_s16le() {
        assert_eq!(pcm_duration_ms(&[0u8; 32_000]), 1000);
        assert_eq!(pcm_duration_ms(&[]), 0);
    }

    #[test]
    fn language_hint_normalizes() {
        assert_eq!(
            normalize_language_hint(Some("  ZH ".into())),
            Some("zh".into())
        );
        assert_eq!(normalize_language_hint(Some("   ".into())), None);
        assert_eq!(normalize_language_hint(None), None);
    }

    #[test]
    fn worker_text_parses_success_and_failure() {
        let ok = serde_json::json!({"id": 1, "ok": true, "text": " 你好 "});
        assert_eq!(worker_text(ok).unwrap(), " 你好 ");
        let err = serde_json::json!({"id": 2, "ok": false, "error": "boom"});
        assert!(worker_text(err).unwrap_err().to_string().contains("boom"));
    }

    #[tokio::test]
    async fn unsupported_model_id_is_rejected_before_spawn() {
        let runtime = Qwen3MlxRuntime::new();
        let result = runtime
            .ensure_loaded_with_progress("qwen3-asr-0.6b", PathBuf::from("unused"), |_, _, _| {})
            .await;
        let error = result.unwrap_err().to_string();
        assert!(error.contains("qwen3-asr-1.7b only"), "{error}");
    }

    #[tokio::test]
    async fn missing_model_files_fail_with_clear_error() {
        let temp = std::env::temp_dir().join(format!(
            "openless-qwen3-empty-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        let runtime = Qwen3MlxRuntime::new();
        let result = runtime
            .ensure_loaded_with_progress(SUPPORTED_MODEL_ID, temp.clone(), |_, _, _| {})
            .await;
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains("config.json"), "{error}");
        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn cancel_bumps_generation_and_clears_buffer() {
        let provider = Qwen3MlxAsr::new(
            Arc::new(Qwen3MlxRuntime::new()),
            SUPPORTED_MODEL_ID.into(),
            PathBuf::from("unused"),
            None,
        );
        provider.consume_pcm_chunk(&[1, 2, 3, 4]);
        let before = provider.cancel_generation.load(Ordering::SeqCst);
        provider.cancel();
        assert!(provider.cancel_generation.load(Ordering::SeqCst) > before);
        assert!(provider.buffer.lock().is_empty());
        assert_eq!(provider.buffer_duration_ms(), 0);
    }

    #[tokio::test]
    async fn empty_buffer_returns_empty_transcript() {
        let provider = Qwen3MlxAsr::new(
            Arc::new(Qwen3MlxRuntime::new()),
            SUPPORTED_MODEL_ID.into(),
            PathBuf::from("unused"),
            None,
        );
        let result = provider.transcribe(Duration::from_secs(5)).await.unwrap();
        assert!(result.text.is_empty());
        assert_eq!(result.duration_ms, 0);
    }

    /// 全静音录音不拉起 worker,直接按空转写返回并清空缓冲;
    /// 测试环境没有 worker,若误入转写路径必然失败。
    #[tokio::test]
    async fn all_silent_buffer_skips_worker_and_returns_empty_transcript() {
        let provider = Qwen3MlxAsr::new(
            Arc::new(Qwen3MlxRuntime::new()),
            SUPPORTED_MODEL_ID.into(),
            PathBuf::from("unused"),
            None,
        );
        provider.consume_pcm_chunk(&vec![0u8; 32_000]);
        let result = provider.transcribe(Duration::from_secs(5)).await.unwrap();
        assert!(result.text.is_empty());
        assert_eq!(result.duration_ms, 1000);
        assert!(provider.buffer.lock().is_empty());
    }

    #[tokio::test]
    async fn release_now_without_worker_is_noop() {
        let runtime = Qwen3MlxRuntime::new();
        runtime.release_now().await.unwrap();
        let status = runtime.status_snapshot(SUPPORTED_MODEL_ID).await;
        assert!(!status.runtime_ready);
        assert_eq!(status.active_model, SUPPORTED_MODEL_ID);
    }

    /// 启动预载真实加载冒烟(默认 ignore;OPENLESS_QWEN3_REAL_MODEL=<模型目录> 后
    /// 运行):ensure_loaded(启动预载同一路径)把固定模型拉起为常驻 worker,
    /// 二次调用幂等不重建;不包含任何转写质量评测。
    #[tokio::test]
    #[ignore = "requires real model and runtime; set OPENLESS_QWEN3_REAL_MODEL to run"]
    async fn real_worker_preload_is_idempotent_without_transcription() {
        let model_dir = PathBuf::from(std::env::var("OPENLESS_QWEN3_REAL_MODEL").unwrap());
        assert!(
            model_dir.join("config.json").is_file(),
            "missing real model dir: {}",
            model_dir.display()
        );
        let runtime = Qwen3MlxRuntime::new();
        assert!(
            !runtime
                .status_snapshot(SUPPORTED_MODEL_ID)
                .await
                .runtime_ready,
            "fresh runtime must not report a ready worker"
        );
        let started = Instant::now();
        let loaded = runtime
            .ensure_loaded(&model_dir)
            .await
            .expect("worker must become ready");
        assert_eq!(loaded, SUPPORTED_MODEL_ID);
        println!("qwen3_preload elapsed_ms={}", started.elapsed().as_millis());
        let status = runtime.status_snapshot(SUPPORTED_MODEL_ID).await;
        assert!(status.runtime_ready);
        assert_eq!(status.loaded_model_id.as_deref(), Some(SUPPORTED_MODEL_ID));
        assert!(!status.loading, "loading flag must clear after ready");
        // 幂等:再次 ensure 直接复用已就绪 worker。
        let again = runtime
            .ensure_loaded(&model_dir)
            .await
            .expect("second ensure must reuse the ready worker");
        assert_eq!(again, SUPPORTED_MODEL_ID);
        runtime.release_now().await.expect("release worker");
        assert!(
            !runtime
                .status_snapshot(SUPPORTED_MODEL_ID)
                .await
                .runtime_ready,
            "released runtime must not report a ready worker"
        );
    }

    /// 真实链路检查(默认 ignore;设置 OPENLESS_QWEN3_REAL_MODEL=<模型目录> 与
    /// OPENLESS_QWEN3_TEST_PCM=<16k mono s16le PCM 文件> 后运行):
    /// 拉起打包运行时里的 worker → 加载 4.7GB 模型 → 转写一段真实语音 →
    /// 再次转写验证 worker 常驻复用 → 释放。只验证链路可用,不做效果评估。
    #[tokio::test]
    #[ignore = "requires real model and runtime; set OPENLESS_QWEN3_REAL_MODEL and OPENLESS_QWEN3_TEST_PCM"]
    async fn real_worker_loads_model_and_transcribes_speech() {
        let model_dir = PathBuf::from(std::env::var("OPENLESS_QWEN3_REAL_MODEL").unwrap());
        let pcm_path = PathBuf::from(std::env::var("OPENLESS_QWEN3_TEST_PCM").unwrap());
        let pcm = std::fs::read(&pcm_path).unwrap();
        assert!(pcm.len() > 32_000, "test pcm too short");

        let runtime = Qwen3MlxRuntime::new();
        let loaded = runtime.ensure_loaded(&model_dir).await.unwrap();
        assert_eq!(loaded, SUPPORTED_MODEL_ID);

        let started = Instant::now();
        let first = runtime
            .transcribe_pcm(&model_dir, &pcm, None, Duration::from_secs(120))
            .await
            .unwrap();
        println!(
            "[real-chain] first transcribe: elapsed={:?}ms text={:?}",
            started.elapsed().as_millis(),
            first
        );
        assert!(
            !first.trim().is_empty(),
            "model returned no text for real speech"
        );

        // 第二次转写应复用常驻 worker(不再有加载耗时)。
        let second = runtime
            .transcribe_pcm(&model_dir, &pcm, None, Duration::from_secs(120))
            .await
            .unwrap();
        assert_eq!(
            first, second,
            "resident worker must be deterministic on the same audio"
        );

        runtime.release_now().await.unwrap();
        let status = runtime.status_snapshot(SUPPORTED_MODEL_ID).await;
        assert!(!status.runtime_ready, "worker must be gone after release");
    }
}
