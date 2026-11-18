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

use anyhow::{anyhow, bail, Context, Result};
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
/// Qwen3-ASR 整段转写长音频会退化(重复、漏句),超过该时长就按块转写再拼接。
/// 块边界按采样对齐，短于等于该时长的音频仍然单次转写。
const QWEN3_ASR_CHUNK_LIMIT_MS: u64 = 30_000;

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
    ///
    /// 超过 30 秒的音频按 30 秒块顺序送给同一个常驻 worker，逐块转写后拼接；
    /// 不超过 30 秒时只有一块，行为与原来一致。worker 协议不变，仍是一个
    /// 请求一段 PCM、返回一段文本。
    ///
    /// `audio_timeout` 是整段音频共用的总预算：分块循环只用一个绝对 deadline，
    /// 每块用这个 deadline 等待，不会让等待上限随块数放大。`is_cancelled` 在
    /// 每块发送前与每次响应后检查；取消命中后不再发送后续块。取消发生时如果
    /// 某块正在等响应，该块会先返回（worker 协议没有中断在途推理的通道），
    /// 这里不杀 worker，避免为了取消丢掉已加载的模型。
    pub async fn transcribe_pcm(
        &self,
        model_dir: &Path,
        pcm: &[u8],
        language_hint: Option<&str>,
        audio_timeout: Duration,
        is_cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<String> {
        if pcm.is_empty() {
            return Ok(String::new());
        }
        let audio_ms = pcm_duration_ms(pcm);
        self.ensure_loaded(model_dir).await?;
        let chunks = openless_core::asr::pcm::split_pcm_by_duration(pcm, QWEN3_ASR_CHUNK_LIMIT_MS);
        let chunk_count = chunks.len();
        let started = Instant::now();
        let deadline = Instant::now() + audio_timeout;
        let mut timed_out = false;
        let result = match run_chunked_transcription(
            &chunks,
            language_hint,
            deadline,
            &Instant::now,
            is_cancelled,
            |payload, chunk_deadline| self.worker_roundtrip(payload, chunk_deadline, audio_timeout),
        )
        .await
        {
            Ok(texts) => Ok(openless_core::asr::transcript::join_transcript_chunks(
                &texts,
            )),
            Err(ChunkRunError::Cancelled) => {
                let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                log::info!(
                    "[qwen3-mlx] transcribe cancelled audio_ms={audio_ms} chunks={chunk_count} elapsed_ms={elapsed_ms}"
                );
                bail!("Qwen3-ASR transcription cancelled");
            }
            Err(ChunkRunError::TimedOut) => {
                timed_out = true;
                Err(anyhow!(
                    "Qwen3-ASR transcription timed out after {}s; worker was restarted for the next use",
                    audio_timeout.as_secs()
                ))
            }
            Err(ChunkRunError::Send(error)) => Err(error),
        };
        if timed_out {
            // 总预算耗尽与单块超时同样按 worker 卡死处理：杀进程，下次重建。
            self.kill_state_worker();
        }
        let elapsed_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        match &result {
            Ok(text) => {
                log::info!(
                    "[qwen3-mlx] transcribe finished audio_ms={audio_ms} chunks={chunk_count} elapsed_ms={elapsed_ms} text_chars={}",
                    text.chars().count()
                );
                self.record_transcribe_result(audio_ms, elapsed_ms, None);
            }
            Err(error) => {
                let message = format!("{error:#}");
                log::warn!(
                    "[qwen3-mlx] transcribe failed audio_ms={audio_ms} chunks={chunk_count} elapsed_ms={elapsed_ms} error={message}"
                );
                self.record_transcribe_result(audio_ms, elapsed_ms, Some(message));
            }
        }
        result
    }

    /// 发送请求并等待响应;写 stdin 与等响应共用整段共享 `deadline`,
    /// 任一阶段超时或通道断开都视为 worker 异常,杀掉进程以便下次重建。
    /// `budget` 是整段音频的总预算，只用于错误信息。
    async fn worker_roundtrip(
        &self,
        payload: serde_json::Value,
        deadline: Instant,
        budget: Duration,
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
        // 写 stdin 可能因管道背压阻塞(base64 音频可达数 MB),仍放到阻塞线程;
        // 完成结果经通道回报,让下面的 `timeout_at` 同时覆盖「写完成」与
        // 「响应」,而不是先无上限地等写完再开始计响应超时。worker 卡住不读
        // 管道时,写线程会一直阻塞在 write_all 上,只有把写完成也纳入同一个
        // deadline,整段音频的总时长上限才成立。
        let (write_tx, write_rx) = oneshot::channel::<std::io::Result<()>>();
        tokio::task::spawn_blocking(move || {
            let mut guard = stdin.lock();
            let result = guard
                .write_all(line.as_bytes())
                .and_then(|_| guard.write_all(b"\n"))
                .and_then(|_| guard.flush());
            let _ = write_tx.send(result);
        });
        let wait = tokio::time::timeout_at(
            deadline.into(),
            async {
                match write_rx.await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => return Err(RoundtripFailure::Write(error)),
                    // 写线程在回报前消失(panic),与旧的 join 失败同一族。
                    Err(_) => return Err(RoundtripFailure::WriteJoin),
                }
                receiver.await.map_err(|_| RoundtripFailure::Dropped)
            },
        )
        .await;
        match wait {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(RoundtripFailure::Dropped)) => {
                let stderr = stderr_tail.lock().clone();
                self.kill_state_worker();
                bail!("Qwen3-ASR worker dropped the request; stderr tail: {stderr}");
            }
            Ok(Err(RoundtripFailure::WriteJoin)) => {
                self.remove_pending(request_id);
                bail!("worker stdin write join failed: write thread ended without reporting");
            }
            Ok(Err(RoundtripFailure::Write(error))) => {
                self.remove_pending(request_id);
                bail!("worker stdin write failed: {error}");
            }
            Err(_) => {
                let stderr = stderr_tail.lock().clone();
                self.kill_state_worker();
                // 写入阶段超时时,阻塞写线程仍卡在 write_all 上。这个泄漏可以
                // 接受:上面已杀掉 worker,管道读端关闭后该线程会带着 EPIPE
                // 自行结束;若进程先退出,线程也随进程一起回收,不会留下僵尸
                // 或占用后续请求(下次转写重建的是新的 stdin 句柄)。
                bail!(
                    "Qwen3-ASR transcription timed out after {}s; worker was restarted for the next use; stderr tail: {stderr}",
                    budget.as_secs()
                );
            }
        }
    }

    /// 丢弃一个已登记但不会再有响应的请求;worker 不在时无事可做。
    fn remove_pending(&self, request_id: u64) {
        if let Some(worker) = self.state.lock().as_mut() {
            worker.pending.lock().remove(&request_id);
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
    model_dir: PathBuf,
    language_hint: Option<String>,
    buffer: Mutex<Vec<u8>>,
    cancel_generation: AtomicU64,
}

impl Qwen3MlxAsr {
    pub fn new(
        runtime: Arc<Qwen3MlxRuntime>,
        model_dir: PathBuf,
        language_hint: Option<String>,
    ) -> Self {
        Self {
            runtime,
            model_dir,
            language_hint: normalize_language_hint(language_hint),
            buffer: Mutex::new(Vec::new()),
            cancel_generation: AtomicU64::new(0),
        }
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
                &|| self.cancel_generation.load(Ordering::SeqCst) != cancel_generation,
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

/// 组装一次转写请求。worker 只接受 pcm_base64 与 language，没有长度参数。
fn transcribe_request(pcm: &[u8], language_hint: Option<&str>) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "op": "transcribe",
        "pcm_base64": base64::engine::general_purpose::STANDARD.encode(pcm),
    });
    if let Some(language) = language_hint.filter(|value| !value.trim().is_empty()) {
        payload["language"] = serde_json::json!(language);
    }
    payload
}

/// `worker_roundtrip` 内部等待阶段的失败原因：写线程消失、写失败、响应通道断开。
/// 三者都不算「超时」；超时由外层 `timeout_at` 单独判定，以便沿用工卡死重建。
#[derive(Debug)]
enum RoundtripFailure {
    WriteJoin,
    Write(std::io::Error),
    Dropped,
}

/// 分块循环的退出原因。取消与超时分开，调用方据此决定是否杀 worker、是否记故障。
#[derive(Debug)]
enum ChunkRunError {
    Cancelled,
    TimedOut,
    Send(anyhow::Error),
}

/// 逐块转写循环：整段只用一个绝对 `deadline`，每块都拿这个 deadline，
/// 不会每块重新获得一份完整超时预算；`is_cancelled` 在每块发送前与每次
/// 响应后检查，命中后立即停止后续块。`now` 只为测试注入时钟，生产传
/// `Instant::now`。
async fn run_chunked_transcription<'a, S, Fut>(
    chunks: &[&'a [u8]],
    language_hint: Option<&str>,
    deadline: Instant,
    now: &(dyn Fn() -> Instant + Sync),
    is_cancelled: &(dyn Fn() -> bool + Sync),
    mut send: S,
) -> std::result::Result<Vec<String>, ChunkRunError>
where
    S: FnMut(serde_json::Value, Instant) -> Fut + Send,
    Fut: std::future::Future<Output = Result<serde_json::Value>> + Send,
{
    let mut texts = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        if is_cancelled() {
            return Err(ChunkRunError::Cancelled);
        }
        if now() >= deadline {
            return Err(ChunkRunError::TimedOut);
        }
        let payload = transcribe_request(chunk, language_hint);
        let value = send(payload, deadline).await.map_err(ChunkRunError::Send)?;
        if is_cancelled() {
            return Err(ChunkRunError::Cancelled);
        }
        texts.push(worker_text(value).map_err(ChunkRunError::Send)?);
    }
    Ok(texts)
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
    fn qwen3_chunking_uses_thirty_second_boundaries() {
        // 2 分钟 = 4 个完整 30 秒块；30 秒以内保持单块。
        let two_minutes = vec![0u8; 32_000 * 120];
        let chunks =
            openless_core::asr::pcm::split_pcm_by_duration(&two_minutes, QWEN3_ASR_CHUNK_LIMIT_MS);
        assert_eq!(chunks.len(), 4);
        assert!(chunks.iter().all(|chunk| chunk.len() == 32_000 * 30));

        let short = vec![0u8; 32_000 * 12];
        assert_eq!(
            openless_core::asr::pcm::split_pcm_by_duration(&short, QWEN3_ASR_CHUNK_LIMIT_MS),
            vec![short.as_slice()]
        );
    }

    /// 多块转写共用同一个绝对 deadline：旧实现每块各拿一份完整
    /// `audio_timeout`，块数越多最长等待越长；这里断言每块拿到同一个 deadline。
    #[tokio::test]
    async fn multi_chunk_transcription_shares_one_deadline() {
        let audio = vec![0u8; 32_000 * 90];
        let chunks =
            openless_core::asr::pcm::split_pcm_by_duration(&audio, QWEN3_ASR_CHUNK_LIMIT_MS);
        assert_eq!(chunks.len(), 3);
        let deadline = Instant::now() + Duration::from_secs(82);
        let mut sent_deadlines = Vec::new();
        let texts = run_chunked_transcription(
            &chunks,
            None,
            deadline,
            &Instant::now,
            &|| false,
            |_payload, chunk_deadline| {
                sent_deadlines.push(chunk_deadline);
                async { Ok(serde_json::json!({"ok": true, "text": "块"})) }
            },
        )
        .await
        .expect("all chunks must succeed");
        assert_eq!(texts.len(), 3);
        assert_eq!(sent_deadlines, vec![deadline; 3]);
    }

    /// 总预算耗尽后不再发送后续块：120 秒音频 4 块、总预算 82 秒；
    /// 模拟每块耗时 40 秒，第 4 块必须被拒绝（旧实现会继续送）。
    #[tokio::test]
    async fn multi_chunk_transcription_stops_when_total_budget_is_spent() {
        let audio = vec![0u8; 32_000 * 120];
        let chunks =
            openless_core::asr::pcm::split_pcm_by_duration(&audio, QWEN3_ASR_CHUNK_LIMIT_MS);
        assert_eq!(chunks.len(), 4);
        let started = Instant::now();
        let deadline = started + Duration::from_secs(82);
        let now = Mutex::new(started);
        let mut sends = 0;
        let result = run_chunked_transcription(
            &chunks,
            None,
            deadline,
            &|| *now.lock(),
            &|| false,
            |_payload, _chunk_deadline| {
                sends += 1;
                *now.lock() += Duration::from_secs(40);
                async { Ok(serde_json::json!({"ok": true, "text": "块"})) }
            },
        )
        .await;
        assert!(
            matches!(result, Err(ChunkRunError::TimedOut)),
            "expected timeout, got {result:?}"
        );
        assert_eq!(sends, 3);
    }

    /// worker 卡死不读 stdin 时,`write_all` 会一直阻塞在管道背压上。这条用例
    /// 走真实 `worker_roundtrip`:接一个真实子进程(`sleep`)当 worker,它的
    /// stdin 是管道却从不读取,payload 远大于管道缓冲区,写入必然阻塞。断言
    /// 总时长被共享 deadline 封住(旧实现会无限期卡在写线程上),且超时后
    /// worker 句柄被清空,下一次转写走既有重建路径。
    #[tokio::test]
    async fn blocked_stdin_write_is_bounded_by_deadline() {
        let mut child = Command::new("/bin/sleep")
            .arg("300")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn stand-in worker");
        let stdin = child.stdin.take().expect("piped stdin");
        let runtime = Qwen3MlxRuntime::new();
        *runtime.state.lock() = Some(WorkerProcess {
            child,
            stdin: Arc::new(Mutex::new(stdin)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            stderr_tail: Arc::new(Mutex::new(String::new())),
            model_dir: PathBuf::from("unused"),
            next_id: 0,
        });
        // macOS 管道缓冲区只有几十 KB,4MB 的 payload 必然写不进去。
        let payload = serde_json::json!({ "pcm": "A".repeat(4 * 1024 * 1024) });
        let started = Instant::now();
        let result = runtime
            .worker_roundtrip(
                payload,
                Instant::now() + Duration::from_millis(200),
                Duration::from_millis(200),
            )
            .await;
        let elapsed = started.elapsed();
        assert!(result.is_err(), "blocked write must fail, got {result:?}");
        assert!(
            elapsed < Duration::from_secs(5),
            "blocked stdin write must be bounded by the shared deadline, elapsed={elapsed:?}"
        );
        assert!(
            runtime.state.lock().is_none(),
            "write timeout must drop the worker so the next transcription rebuilds it"
        );
        let _ = runtime.release_now().await;
    }

    /// 第一块响应后取消：不得再发送剩余块。
    #[tokio::test]
    async fn cancellation_between_chunks_stops_remaining_chunks() {
        let audio = vec![0u8; 32_000 * 120];
        let chunks =
            openless_core::asr::pcm::split_pcm_by_duration(&audio, QWEN3_ASR_CHUNK_LIMIT_MS);
        assert_eq!(chunks.len(), 4);
        let cancelled = AtomicBool::new(false);
        let mut sends = 0;
        let result = run_chunked_transcription(
            &chunks,
            None,
            Instant::now() + Duration::from_secs(300),
            &Instant::now,
            &|| cancelled.load(Ordering::SeqCst),
            |_payload, _chunk_deadline| {
                sends += 1;
                cancelled.store(true, Ordering::SeqCst);
                async { Ok(serde_json::json!({"ok": true, "text": "块"})) }
            },
        )
        .await;
        assert!(
            matches!(result, Err(ChunkRunError::Cancelled)),
            "expected cancellation, got {result:?}"
        );
        assert_eq!(sends, 1);
    }

    /// deadline 已过时一块都不发送，直接按超时返回。
    #[tokio::test]
    async fn expired_deadline_sends_no_chunks() {
        let audio = vec![0u8; 32_000 * 60];
        let chunks =
            openless_core::asr::pcm::split_pcm_by_duration(&audio, QWEN3_ASR_CHUNK_LIMIT_MS);
        assert_eq!(chunks.len(), 2);
        let mut sends = 0;
        let result = run_chunked_transcription(
            &chunks,
            None,
            Instant::now(),
            &Instant::now,
            &|| false,
            |_payload, _chunk_deadline| {
                sends += 1;
                async { Ok(serde_json::json!({"ok": true, "text": "块"})) }
            },
        )
        .await;
        assert!(
            matches!(result, Err(ChunkRunError::TimedOut)),
            "expected timeout, got {result:?}"
        );
        assert_eq!(sends, 0);
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
            .transcribe_pcm(&model_dir, &pcm, None, Duration::from_secs(120), &|| false)
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
            .transcribe_pcm(&model_dir, &pcm, None, Duration::from_secs(120), &|| false)
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
