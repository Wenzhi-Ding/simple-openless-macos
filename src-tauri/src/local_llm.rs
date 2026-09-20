#![allow(clippy::cast_possible_truncation)]

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use encoding_rs::UTF_8;
use futures_util::future::BoxFuture;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;
use openless_core::polish_diagnostics::{
    context_diagnostics, PolishDiagnosticError, PolishDiagnosticMessage, PolishDiagnostics,
    PolishDiagnosticsOutcome, PolishDiagnosticsStage, PolishTemplateKind, ADD_BOS_NOTE,
};
use openless_core::{
    BackendError, BackendErrorCode, DictationContext, PolishAttempt, PolishOutput, SessionId,
    TextPolisher, TextStreamSink,
};
use serde::Serialize;

pub const LOCAL_LLM_PROVIDER_ID: &str = "local-llama";
const CONTEXT_TOKENS: u32 = 16_384;
const MIN_GENERATION_TOKENS: usize = 1024;

/// 固定润色模型：Gemma 4 E4B it Q4_0（llama.cpp 官方 ggml-org 转换，Apache-2.0，
/// QAT 量化）。思考关闭由 Gemma 4 canonical chat template 的 enable_thinking=false
/// 分支保证，见 [`render_gemma4_prompt`]。
/// 模型由构建固定：没有导入/选择/回退路径（旧版会回退 Qwen2.5 或扫描目录里其它
/// GGUF，已按要求移除）；磁盘上其它 GGUF 保留但不会被自动选用。
pub const DEFAULT_MODEL_FILE: &str = "gemma-4-E4B-it-Q4_0.gguf";
#[cfg(test)]
const LEGACY_SWAP_MODEL_FILE: &str = "qwen2.5-7b-instruct-q4_k_m.gguf";

pub fn models_dir() -> Result<PathBuf, String> {
    crate::persistence::models_root()
        .map(|root| root.join("llm"))
        .map_err(|error| error.to_string())
}

/// 固定模型的路径：目录 + DEFAULT_MODEL_FILE，不读标记、不回退、不扫描目录。
pub fn resolve_model_in_dir(dir: &Path) -> PathBuf {
    dir.join(DEFAULT_MODEL_FILE)
}

/// 固定模型的完整路径（AppSupport 数据目录下的 llm/）。
pub fn resolve_active_model_path() -> PathBuf {
    match models_dir() {
        Ok(dir) => resolve_model_in_dir(&dir),
        Err(_) => PathBuf::from("models").join("llm").join(DEFAULT_MODEL_FILE),
    }
}

/// 引擎状态（概览卡与事件共用；`Loaded` 以共享 slot 里的真实模型为准，见
/// [`LocalLlamaPolisher::status`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum LoadState {
    NotLoaded,
    Loading,
    Loaded,
    Failed(String),
}

/// 共享实例的对外状态快照。`state` 是前端直接消费的枚举字符串：
/// `notFound | notLoaded | loading | loaded | failed`。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalLlmEngineStatus {
    pub state: String,
    pub error: Option<String>,
}

pub struct LocalLlamaPolisher {
    /// `None` = 使用固定模型（`resolve_active_model_path()`，AppSupport 模型目录，
    /// 当前即 Gemma 4 E4B）；`Some(path)` = 固定模型路径（测试与显式指定）。
    fixed_model_path: Option<PathBuf>,
    // 字段声明顺序即 drop 顺序：loaded 里的模型必须先于 backend 消失。
    loaded: Arc<Mutex<Option<Arc<LoadedModel>>>>,
    /// llama backend 全进程只能 init 一次，换载模型时复用同一个实例。
    backend: Arc<Mutex<Option<Arc<LlamaBackend>>>>,
    /// llama.cpp's backend/model access is process-global enough that local requests must not
    /// overlap, even though every request receives its own decode context.
    inference: Arc<Mutex<()>>,
    cancelled: Arc<Mutex<HashMap<SessionId, Arc<AtomicBool>>>>,
    max_generation_tokens: usize,
    /// 加载状态机：preload 写入，status 读取；与 `loaded` slot 的真实状态合并后
    /// 对外报告（slot 里有模型 = loaded，即使状态机还没来得及翻）。
    state: Arc<Mutex<LoadState>>,
    /// 状态变化回调（Coordinator 注册，用来广播 `local-llm:engine-changed`）。
    watcher: Arc<Mutex<Option<Box<dyn Fn(&LocalLlmEngineStatus) + Send + Sync>>>>,
}

struct LoadedModel {
    model: LlamaModel,
    /// 载入该模型用的文件路径；与本次请求解析出的路径不一致时触发换载。
    path: PathBuf,
}

impl LocalLlamaPolisher {
    /// 应用运行时用：每次请求解析激活模型，导入切换后即时生效。
    pub fn new() -> Self {
        Self::build(None)
    }

    #[cfg(test)]
    fn with_fixed_model(path: PathBuf) -> Self {
        Self::build(Some(path))
    }

    fn build(fixed_model_path: Option<PathBuf>) -> Self {
        Self {
            fixed_model_path,
            loaded: Arc::new(Mutex::new(None)),
            backend: Arc::new(Mutex::new(None)),
            inference: Arc::new(Mutex::new(())),
            cancelled: Arc::new(Mutex::new(HashMap::new())),
            max_generation_tokens: usize::MAX,
            state: Arc::new(Mutex::new(LoadState::NotLoaded)),
            watcher: Arc::new(Mutex::new(None)),
        }
    }

    /// 当前应加载的模型路径。
    fn active_model_path(&self) -> PathBuf {
        match &self.fixed_model_path {
            Some(fixed) => fixed.clone(),
            None => resolve_active_model_path(),
        }
    }

    /// 状态快照：以共享 `loaded` slot 的真实内容为准（润色自己触发的载入也会被
    /// 如实报告为 loaded），状态机只补充 notFound/loading/failed 的过程信息。
    pub fn status(&self) -> LocalLlmEngineStatus {
        let state = self.state.lock().unwrap().clone();
        self.status_from_state(&state)
    }

    fn status_from_state(&self, state: &LoadState) -> LocalLlmEngineStatus {
        let path = self.active_model_path();
        let exists = path.is_file();
        let resident = self
            .loaded
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|loaded| loaded.path == path);
        let (state, error) = if resident {
            ("loaded", None)
        } else {
            match state {
                LoadState::NotLoaded => (if exists { "notLoaded" } else { "notFound" }, None),
                LoadState::Loading => ("loading", None),
                // 载入后文件被移走：内存里的模型仍可用，保持 loaded。
                LoadState::Loaded => ("loaded", None),
                LoadState::Failed(message) => ("failed", Some(message.clone())),
            }
        };
        LocalLlmEngineStatus {
            state: state.to_string(),
            error,
        }
    }

    fn set_state(&self, next: LoadState) {
        let status = {
            let mut guard = self.state.lock().unwrap();
            if *guard == next {
                return;
            }
            *guard = next;
            self.status_from_state(&guard)
        };
        if let Some(watcher) = self.watcher.lock().unwrap().as_ref() {
            watcher(&status);
        }
    }

    /// 注册状态变化回调（Coordinator 用来向前端广播引擎状态事件）。
    pub fn set_status_watcher(&self, watcher: Box<dyn Fn(&LocalLlmEngineStatus) + Send + Sync>) {
        *self.watcher.lock().unwrap() = Some(watcher);
    }

    /// 幂等预载：把固定模型载入**本实例**共享的 slot（与润色同 `inference` 锁、
    /// 同一个全进程 backend 单例），供启动后台常驻调用。
    /// - 已加载（slot 命中）或已有一次预载在跑：直接返回，不重复载模型；
    /// - 预载进行中触发的听写润色会在同一把锁上排队，等预载完成后复用模型；
    /// - 失败把状态机翻到 Failed（可再次调用重试），错误原样返回。
    pub async fn preload(&self) -> Result<(), BackendError> {
        if matches!(*self.state.lock().unwrap(), LoadState::Loading) {
            return Ok(());
        }
        let path = self.active_model_path();
        if !path.is_file() {
            let error = BackendError::new(
                BackendErrorCode::Unsupported,
                format!("local LLM model missing: {}", path.display()),
            );
            self.set_state(LoadState::Failed(error.message.clone()));
            return Err(error);
        }
        self.set_state(LoadState::Loading);
        let loaded = Arc::clone(&self.loaded);
        let backend = Arc::clone(&self.backend);
        let inference = Arc::clone(&self.inference);
        let worker = tauri::async_runtime::spawn_blocking(move || {
            ensure_model_loaded(&path, loaded, backend, inference)
        })
        .await
        .map_err(|error| {
            BackendError::new(
                BackendErrorCode::Internal,
                format!("local LLM preload worker failed: {error}"),
            )
        });
        match worker.and_then(|loaded| loaded) {
            Ok(_model) => {
                self.set_state(LoadState::Loaded);
                Ok(())
            }
            Err(error) => {
                self.set_state(LoadState::Failed(error.message.clone()));
                Err(error)
            }
        }
    }

    /// 退出前同步归还 llama.cpp 资源（幂等）。
    ///
    /// llama.cpp 的 Metal 设备由 C++ 静态析构在进程收尾阶段释放，如果那时本
    /// 进程还留着未释放的模型缓冲，`ggml_metal_rsets_free` 的
    /// `GGML_ASSERT([rsets->data count] == 0)` 就会直接 abort。Tauri 在
    /// `RunEvent::Exit` 回调返回后立刻退出进程，异步清理来不及跑，所以这里
    /// 必须同步完成：先取 `inference` 锁确认没有在途请求还持有模型，再按
    /// “模型先于 backend”的顺序清空两个 slot（与字段声明顺序一致），把
    /// Metal 缓冲还给驱动。
    ///
    /// 等锁有上限：真在跑的润色会一直握着 `inference`，退出不该为它无限期
    /// 挂住。拿不到锁就放弃本次清理（保留旧的收尾行为），不让退出变成卡死。
    ///
    /// 清理路径不因锁中毒再次 panic：退出阶段任何一次 panic 都会让整个进程
    /// 以 abort 结束，正是要避免的结果。
    pub fn unload_blocking(&self) {
        const UNLOAD_LOCK_TIMEOUT: Duration = Duration::from_secs(2);
        let deadline = Instant::now() + UNLOAD_LOCK_TIMEOUT;
        let _inference_guard = loop {
            match self.inference.try_lock() {
                Ok(guard) => break guard,
                Err(TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        log::warn!(
                            "[local-llm] 退出时润色仍在进行，跳过模型卸载（保持既有退出路径）"
                        );
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        };
        *lock_ignoring_poison(&self.loaded) = None;
        *lock_ignoring_poison(&self.backend) = None;
        *lock_ignoring_poison(&self.state) = LoadState::NotLoaded;
    }
}

/// 退出清理专用：忽略 poisoning 取锁，避免收尾阶段二次 panic 变成 abort。
fn lock_ignoring_poison<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl TextPolisher for LocalLlamaPolisher {
    fn polish(
        &self,
        session_id: SessionId,
        context: Arc<DictationContext>,
        raw_text: String,
        _partials: Arc<dyn TextStreamSink>,
    ) -> BoxFuture<'static, PolishAttempt> {
        let path = self.active_model_path();
        let loaded = Arc::clone(&self.loaded);
        let backend = Arc::clone(&self.backend);
        let inference = Arc::clone(&self.inference);
        let cancelled = Arc::clone(&self.cancelled);
        let max_generation_tokens = self.max_generation_tokens;
        let flag = Arc::new(AtomicBool::new(false));
        cancelled
            .lock()
            .unwrap()
            .insert(session_id, Arc::clone(&flag));
        Box::pin(async move {
            let worker = tauri::async_runtime::spawn_blocking(move || {
                generate(
                    path,
                    loaded,
                    backend,
                    inference,
                    flag,
                    &context,
                    &raw_text,
                    max_generation_tokens,
                )
            })
            .await;
            // Always remove the request, including a JoinError caused by a worker panic/cancel.
            cancelled.lock().unwrap().remove(&session_id);
            match worker {
                Ok(attempt) => attempt,
                Err(error) => PolishAttempt::plain(Err(BackendError::new(
                    BackendErrorCode::Internal,
                    format!("local LLM worker failed: {error}"),
                ))),
            }
        })
    }

    fn cancel(&self, session_id: SessionId) -> BoxFuture<'static, Result<(), BackendError>> {
        let flag = self.cancelled.lock().unwrap().get(&session_id).cloned();
        Box::pin(async move {
            if let Some(flag) = flag {
                flag.store(true, Ordering::Release);
            }
            Ok(())
        })
    }
}

/// 载入/换载模型到共享 slot。调用方必须已持有 `inference` 互斥锁（generate 持有
/// 整个请求；预载经 [`ensure_model_loaded`] 取锁后调用）。
fn load_model_into_slot(
    path: &Path,
    slot: &Arc<Mutex<Option<Arc<LoadedModel>>>>,
    backend_slot: &Arc<Mutex<Option<Arc<LlamaBackend>>>>,
) -> Result<Arc<LoadedModel>, BackendError> {
    let mut guard = slot.lock().unwrap();
    let reload_needed = match guard.as_ref() {
        Some(loaded) => loaded.path != path,
        None => true,
    };
    if reload_needed {
        *guard = None;
        let backend = {
            let mut backend_guard = backend_slot.lock().unwrap();
            if backend_guard.is_none() {
                *backend_guard = Some(Arc::new(LlamaBackend::init().map_err(local_error)?));
            }
            Arc::clone(
                backend_guard
                    .as_ref()
                    .expect("backend was just initialized"),
            )
        };
        let params = LlamaModelParams::default().with_n_gpu_layers(1000);
        let model = LlamaModel::load_from_file(&backend, path, &params).map_err(local_error)?;
        *guard = Some(Arc::new(LoadedModel {
            model,
            path: path.to_path_buf(),
        }));
    }
    Ok(guard
        .as_ref()
        .expect("loaded model was just initialized")
        .clone())
}

/// 取 inference 锁后执行 [`load_model_into_slot`]（幂等：slot 已命中同一文件时
/// 直接复用，不重载）——预载与直接测试共用。
fn ensure_model_loaded(
    path: &Path,
    slot: Arc<Mutex<Option<Arc<LoadedModel>>>>,
    backend_slot: Arc<Mutex<Option<Arc<LlamaBackend>>>>,
    inference: Arc<Mutex<()>>,
) -> Result<Arc<LoadedModel>, BackendError> {
    let _guard = inference.lock().unwrap();
    load_model_into_slot(path, &slot, &backend_slot)
}

/// 诊断快照构建器：generate 内部在**真正渲染 prompt 之后**、用喂给 tokenizer 与
/// prefill 的同一份 messages/prompt 值填充。历史层拿到什么就是什么，禁止重拼。
struct DiagnosticsCapture {
    diagnostics: PolishDiagnostics,
}

impl DiagnosticsCapture {
    fn new(path: &Path, context: &DictationContext) -> Self {
        Self {
            diagnostics: PolishDiagnostics {
                stage: PolishDiagnosticsStage::NotCalled,
                outcome: PolishDiagnosticsOutcome::Failed,
                provider: Some(LOCAL_LLM_PROVIDER_ID.into()),
                model: Some(
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                ),
                template: None,
                add_bos: None,
                add_bos_note: None,
                prompt: None,
                messages: None,
                prompt_tokens: None,
                context_tokens: Some(CONTEXT_TOKENS),
                generation_budget: None,
                error: None,
                not_called_reason: None,
                // 渲染成功前一律按「未采用」记录（有正文时）；Included 在渲染点覆盖。
                cursor: Some(context_diagnostics(
                    &context.polish.cursor_context_trace,
                    context.polish.cursor_context.as_deref(),
                    false,
                )),
                screen_ocr: Some(context_diagnostics(
                    &context.polish.screen_ocr_trace,
                    context.polish.screen_ocr.as_deref(),
                    false,
                )),
            },
        }
    }

    fn mark_not_called(&mut self, reason: &str) {
        self.diagnostics.stage = PolishDiagnosticsStage::NotCalled;
        self.diagnostics.outcome = PolishDiagnosticsOutcome::NotCalled;
        self.diagnostics.not_called_reason = Some(reason.to_string());
    }

    /// prompt 已产出：记入完整最终 prompt、结构化消息（含真实 prior turns）、
    /// 渲染路径与 BOS 说明，并把带正文的上下文升级为 Included。
    fn rendered(
        &mut self,
        prompt: &str,
        messages: &[ChatTurn],
        template: PolishTemplateKind,
        context: &DictationContext,
    ) {
        self.diagnostics.stage = PolishDiagnosticsStage::Rendered;
        self.diagnostics.template = Some(template);
        self.diagnostics.add_bos = Some(true);
        self.diagnostics.add_bos_note = Some(ADD_BOS_NOTE.to_string());
        self.diagnostics.prompt = Some(prompt.to_string());
        self.diagnostics.messages = Some(
            messages
                .iter()
                .map(|turn| PolishDiagnosticMessage {
                    role: turn.role.clone(),
                    content: turn.content.clone(),
                })
                .collect(),
        );
        self.diagnostics.cursor = Some(context_diagnostics(
            &context.polish.cursor_context_trace,
            context.polish.cursor_context.as_deref(),
            true,
        ));
        self.diagnostics.screen_ocr = Some(context_diagnostics(
            &context.polish.screen_ocr_trace,
            context.polish.screen_ocr.as_deref(),
            true,
        ));
    }

    /// 失败收尾：错误摘要进快照（code + message，均不含 prompt 正文；message
    /// 截断兜底，防止任何底层错误意外带出正文）。渲染前失败保持 NotAdopted。
    fn failed(&mut self, error: &BackendError) {
        if self.diagnostics.stage == PolishDiagnosticsStage::NotCalled {
            self.diagnostics.outcome = PolishDiagnosticsOutcome::NotCalled;
            self.diagnostics.not_called_reason.get_or_insert_with(|| {
                "the polish call ended before the local model was invoked".to_string()
            });
        } else {
            self.diagnostics.outcome = PolishDiagnosticsOutcome::Failed;
        }
        self.diagnostics.error = Some(PolishDiagnosticError {
            code: format!("{:?}", error.code),
            // 只留计数/状态词；硬截断 400 字符兜底（本引擎错误信息本就不含正文）。
            message: error.message.chars().take(400).collect(),
        });
    }
}

fn generate(
    path: PathBuf,
    slot: Arc<Mutex<Option<Arc<LoadedModel>>>>,
    backend_slot: Arc<Mutex<Option<Arc<LlamaBackend>>>>,
    inference: Arc<Mutex<()>>,
    cancelled: Arc<AtomicBool>,
    context: &DictationContext,
    raw: &str,
    max_generation_tokens: usize,
) -> PolishAttempt {
    let mut capture = DiagnosticsCapture::new(&path, context);
    let result = generate_inner(
        &path,
        &slot,
        &backend_slot,
        inference,
        cancelled,
        context,
        raw,
        max_generation_tokens,
        &mut capture,
    );
    match result {
        Ok(output) => {
            capture.diagnostics.stage = PolishDiagnosticsStage::Succeeded;
            capture.diagnostics.outcome = PolishDiagnosticsOutcome::Succeeded;
            PolishAttempt {
                result: Ok(output),
                diagnostics: Some(capture.diagnostics),
            }
        }
        Err(error) => {
            if matches!(error.code, BackendErrorCode::Cancelled)
                && capture.diagnostics.stage == PolishDiagnosticsStage::NotCalled
            {
                capture.mark_not_called("cancelled before the local model was invoked");
            } else {
                capture.failed(&error);
            }
            PolishAttempt {
                result: Err(error),
                diagnostics: Some(capture.diagnostics),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn generate_inner(
    path: &Path,
    slot: &Arc<Mutex<Option<Arc<LoadedModel>>>>,
    backend_slot: &Arc<Mutex<Option<Arc<LlamaBackend>>>>,
    inference: Arc<Mutex<()>>,
    cancelled: Arc<AtomicBool>,
    context: &DictationContext,
    raw: &str,
    max_generation_tokens: usize,
    capture: &mut DiagnosticsCapture,
) -> Result<PolishOutput, BackendError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(cancelled_error());
    }
    if !path.is_file() {
        capture.diagnostics.stage = PolishDiagnosticsStage::ModelMissing;
        return Err(BackendError::new(
            BackendErrorCode::Unsupported,
            format!("local LLM model missing: {}", path.display()),
        ));
    }

    // The guard intentionally covers loading and the whole request. It serializes Metal/backend
    // access without asserting Send/Sync for any wrapper type ourselves.
    let _inference_guard = inference.lock().unwrap();
    if cancelled.load(Ordering::Acquire) {
        return Err(cancelled_error());
    }
    // 载入/换载判断与预载共用同一 slot 逻辑（见 load_model_into_slot）：本请求要
    // 用的文件与缓存里已载入的不同，就先丢弃旧模型（连同其 Metal 缓冲）再载新
    // 模型。backend 全进程只 init 一次并被复用。inference 互斥锁覆盖整个请求，
    // 换载不会与其他请求/预载并发。
    let loaded = match load_model_into_slot(path, slot, backend_slot) {
        Ok(loaded) => loaded,
        Err(error) => {
            capture.diagnostics.stage = PolishDiagnosticsStage::ModelLoadFailed;
            return Err(error);
        }
    };
    let backend = {
        let backend_guard = backend_slot.lock().unwrap();
        Arc::clone(
            backend_guard
                .as_ref()
                .expect("backend initialized during model load"),
        )
    };

    let template = match loaded.model.chat_template(None).map_err(local_error) {
        Ok(template) => template,
        Err(error) => {
            capture.diagnostics.stage = PolishDiagnosticsStage::RenderFailed;
            return Err(error);
        }
    };
    let template_text = match template.to_str().map_err(local_error) {
        Ok(text) => text.to_owned(),
        Err(error) => {
            capture.diagnostics.stage = PolishDiagnosticsStage::RenderFailed;
            return Err(error);
        }
    };
    // Gemma 4 canonical 模板以 <|turn> 标记轮次。llama.cpp 内建的
    // llama_chat_apply_template 只有预置模板表（没有 gemma4），会回退 chatml
    // 产出错误 prompt，因此 gemma4 在 Rust 侧按 canonical 模板自渲染。
    let is_gemma4 = template_text.contains("<|turn>");
    let messages = prompt_messages(context, raw, is_gemma4);
    let prompt = if is_gemma4 {
        let rendered = render_gemma4_prompt(&messages);
        capture.rendered(
            &rendered,
            &messages,
            PolishTemplateKind::Gemma4Canonical,
            context,
        );
        rendered
    } else {
        let chat: Result<Vec<LlamaChatMessage>, BackendError> = messages
            .iter()
            .map(|turn| chat_message(&turn.role, turn.content.clone()))
            .collect();
        match loaded
            .model
            .apply_chat_template(&template, &chat.map_err(local_error)?, true)
            .map_err(local_error)
        {
            Ok(rendered) => {
                capture.rendered(
                    &rendered,
                    &messages,
                    PolishTemplateKind::GgufChatTemplate,
                    context,
                );
                rendered
            }
            Err(error) => {
                capture.diagnostics.stage = PolishDiagnosticsStage::RenderFailed;
                return Err(error);
            }
        }
    };
    // 保险：GGUF 元数据若未把 <turn|> 标为 EOG，也按该 token 停止生成。
    let gemma4_eot = if is_gemma4 {
        loaded
            .model
            .str_to_token("<turn|>", AddBos::Never)
            .ok()
            .and_then(|tokens| tokens.last().copied())
    } else {
        None
    };
    let tokens = match loaded
        .model
        .str_to_token(&prompt, AddBos::Always)
        .map_err(local_error)
    {
        Ok(tokens) => tokens,
        Err(error) => {
            capture.diagnostics.stage = PolishDiagnosticsStage::RenderFailed;
            return Err(error);
        }
    };
    let n_ctx = NonZeroU32::new(CONTEXT_TOKENS).expect("context is non-zero");
    capture.diagnostics.stage = PolishDiagnosticsStage::Tokenized;
    capture.diagnostics.prompt_tokens = Some(tokens.len());
    if tokens.len() >= n_ctx.get() as usize {
        return Err(BackendError::new(
            BackendErrorCode::InvalidArgument,
            format!(
                "text requires {} prompt tokens, exceeding the local LLM {}-token context",
                tokens.len(),
                n_ctx.get(),
            ),
        ));
    }
    if tokens.is_empty() {
        return Err(BackendError::new(
            BackendErrorCode::InvalidArgument,
            "local LLM prompt has no tokens",
        ));
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(cancelled_error());
    }

    let adaptive_generation_tokens = tokens
        .len()
        .saturating_mul(3)
        .saturating_div(2)
        .saturating_add(256)
        .max(MIN_GENERATION_TOKENS);
    let generation_budget = adaptive_generation_tokens
        .min(n_ctx.get() as usize - tokens.len())
        .min(max_generation_tokens);
    capture.diagnostics.generation_budget = Some(generation_budget);
    if generation_budget == 0 {
        return Err(BackendError::new(
            BackendErrorCode::InvalidArgument,
            "text leaves no room for local LLM output",
        ));
    }
    let prompt_token_count = tokens.len();
    let n_batch = u32::try_from(prompt_token_count).map_err(|_| {
        BackendError::new(
            BackendErrorCode::InvalidArgument,
            "local LLM prompt is too long",
        )
    })?;
    let params = LlamaContextParams::default()
        .with_n_ctx(Some(n_ctx))
        .with_n_batch(n_batch);
    let mut llama_context = match loaded
        .model
        .new_context(&backend, params)
        .map_err(local_error)
    {
        Ok(context) => context,
        Err(error) => {
            capture.diagnostics.stage = PolishDiagnosticsStage::Tokenized;
            return Err(error);
        }
    };
    let mut batch = LlamaBatch::new(tokens.len(), 1);
    let last = tokens.len() - 1;
    for (i, token) in tokens.into_iter().enumerate() {
        batch
            .add(token, i as i32, &[0], i == last)
            .map_err(local_error)?;
    }
    // Cancellation is checked immediately before prefill rather than only in generation.
    if cancelled.load(Ordering::Acquire) {
        return Err(cancelled_error());
    }
    llama_context.decode(&mut batch).map_err(local_error)?;
    capture.diagnostics.stage = PolishDiagnosticsStage::Prefilled;

    // Gemma 4 it 系模型需要接近官方推荐区间的采样；实测（2026-09，gemma-4-E2B-it-Q4_0）
    // temp 0.2/0.5/0.6/1.0 都会把英文输入漂移成中文输出，0.7 + top_k 64 + top_p 0.95
    // 是矩阵实验里唯一稳定保持输出语言的组合。Qwen 等既有模型维持原采样不变。
    let mut sampler = if is_gemma4 {
        LlamaSampler::chain_simple([
            LlamaSampler::temp(0.7),
            LlamaSampler::top_k(64),
            LlamaSampler::top_p(0.95, 1),
            LlamaSampler::dist(42),
        ])
    } else {
        LlamaSampler::chain_simple([LlamaSampler::temp(0.2), LlamaSampler::dist(42)])
    };
    let mut decoder = UTF_8.new_decoder();
    let mut output = String::new();
    let mut position = batch.n_tokens();
    for generated in 0..generation_budget {
        if generated == 0 {
            capture.diagnostics.stage = PolishDiagnosticsStage::Generating;
        }
        if cancelled.load(Ordering::Acquire) {
            return Err(cancelled_error());
        }
        let token = sampler.sample(&llama_context, batch.n_tokens() - 1);
        sampler.accept(token);
        if loaded.model.is_eog_token(token) || gemma4_eot == Some(token) {
            let text = output.trim().to_string();
            if text.is_empty() {
                return Err(BackendError::new(
                    BackendErrorCode::Internal,
                    "local LLM returned empty text",
                ));
            }
            return Ok(PolishOutput {
                text,
                source_text: Some(raw.to_string()),
                llm_call_label: Some(openless_core::polish::LlmCallLabel {
                    provider: LOCAL_LLM_PROVIDER_ID.into(),
                    model: path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                }),
            });
        }
        output.push_str(
            &loaded
                .model
                .token_to_piece(token, &mut decoder, true, None)
                .map_err(local_error)?,
        );
        batch.clear();
        batch
            .add(token, position, &[0], true)
            .map_err(local_error)?;
        position += 1;
        llama_context.decode(&mut batch).map_err(local_error)?;
    }
    // 生成耗尽：错误只保留计数（prompt/输出正文一律不进错误信息）。
    Err(BackendError::new(
        BackendErrorCode::InvalidArgument,
        format!(
            "local LLM output exceeded its {generation_budget}-token limit after a {tokens_len}-token prompt; generated {generated_chars} characters",
            generated_chars = output.chars().count(),
            tokens_len = prompt_token_count,
        ),
    ))
}

/// 消息先以本类型流转：Gemma 4 需要在 Rust 侧自渲染模板（见 [`render_gemma4_prompt`]），
/// 其余模型仍走 llama-cpp-2 的 `apply_chat_template`，两种路径共用同一份消息。
#[derive(Debug, Clone)]
struct ChatTurn {
    role: String,
    content: String,
}

fn prompt_messages(context: &DictationContext, raw: &str, _gemma4: bool) -> Vec<ChatTurn> {
    let (system_prompt, user_prompt) = context.effective_polish_prompts(raw);
    // One code-owned contract for every model call; no legacy style or language suffix.
    let mut messages = Vec::with_capacity(context.polish.prior_turns.len() * 2 + 2);
    messages.push(ChatTurn {
        role: "system".into(),
        content: system_prompt,
    });
    for turn in context.polish.prior_turns.iter().rev() {
        messages.push(ChatTurn {
            role: "user".into(),
            content: openless_core::prompts::user_prompt(&turn.raw_text),
        });
        messages.push(ChatTurn {
            role: "assistant".into(),
            content: turn.polished_text.clone(),
        });
    }
    messages.push(ChatTurn {
        role: "user".into(),
        content: user_prompt,
    });
    messages
}

/// 按 Google Gemma 4 canonical chat template（2026-07-09 版）渲染 prompt，
/// 固定 `enable_thinking=false`、无 tools、`add_generation_prompt=true`。
/// 思考关闭是模板级语义，不依赖提示词技巧：不注入 `<|think|>`，生成提示
/// 直接以 `<|turn>model\n` 开头（canonical 模板 enable_thinking=false 分支），
/// 不会打开 `<|channel>thought` 通道。system/user 内容按模板 trim；model
/// 内容经模板 strip_thinking 的尾部 trim，等效 trim。`assistant` 角色名映射为
/// `model`。BOS 由 tokenizer 的 AddBos::Always 注入，正文不再包含 <bos> 文本。
fn render_gemma4_prompt(messages: &[ChatTurn]) -> String {
    let mut prompt = String::new();
    for message in messages {
        let role = match message.role.as_str() {
            "assistant" => "model",
            role => role,
        };
        let content = message.content.trim();
        prompt.push_str("<|turn>");
        prompt.push_str(role);
        prompt.push('\n');
        prompt.push_str(content);
        prompt.push_str("<turn|>\n");
    }
    prompt.push_str("<|turn>model\n");
    prompt
}

fn chat_message(role: &str, content: String) -> Result<LlamaChatMessage, BackendError> {
    LlamaChatMessage::new(role.to_string(), content).map_err(local_error)
}

fn cancelled_error() -> BackendError {
    BackendError::new(BackendErrorCode::Cancelled, "local LLM cancelled")
}

fn local_error(error: impl std::fmt::Display) -> BackendError {
    BackendError::new(BackendErrorCode::Internal, format!("local LLM: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_messages_use_the_captured_style_and_context_not_debug_mode() {
        let mut context = DictationContext::default();
        context.polish.working_languages = vec!["English".into()];
        context.polish.front_app = Some("Mail".into());
        context
            .polish
            .prior_turns
            .push(openless_core::PolishHistoryTurn {
                raw_text: "old raw".into(),
                polished_text: "old polished".into(),
            });
        let messages = prompt_messages(&context, "new raw", false);
        assert_eq!(messages.len(), 4);
        let rendered = format!("{messages:?}");
        assert!(!rendered.contains("STYLE-CONTRACT"));
        assert!(!rendered.contains("Mail"));
        assert!(messages[0]
            .content
            .contains(openless_core::prompts::FIXED_POLISH_PROMPT));
        assert!(rendered.contains("old polished"));
        assert!(!rendered.contains("模式："));
    }

    #[test]
    fn diagnostics_capture_marks_contexts_not_adopted_before_rendering() {
        let mut context = DictationContext::default();
        context.polish.cursor_context = Some("doc before cursor".into());
        context.polish.cursor_context_trace =
            openless_core::polish_diagnostics::PolishContextTrace::included();
        context.polish.screen_ocr_trace =
            openless_core::polish_diagnostics::PolishContextTrace::new(
                openless_core::polish_diagnostics::PolishContextStatus::NotReady,
                None,
            );
        let path = PathBuf::from("/tmp/models/llm/gemma-4-E4B-it-Q4_0.gguf");
        let capture = DiagnosticsCapture::new(&path, &context);
        // 渲染前：有正文也只算「捕获未采用」，绝不误标 Included。
        assert_eq!(
            capture.diagnostics.cursor.as_ref().unwrap().status,
            openless_core::polish_diagnostics::PolishContextStatus::NotAdopted
        );
        assert_eq!(
            capture.diagnostics.cursor.as_ref().unwrap().text.as_deref(),
            Some("doc before cursor")
        );
        assert_eq!(
            capture.diagnostics.screen_ocr.as_ref().unwrap().status,
            openless_core::polish_diagnostics::PolishContextStatus::NotReady
        );
        assert_eq!(
            capture.diagnostics.model.as_deref(),
            Some("gemma-4-E4B-it-Q4_0.gguf")
        );
        assert_eq!(
            capture.diagnostics.provider.as_deref(),
            Some(LOCAL_LLM_PROVIDER_ID)
        );
    }

    #[test]
    fn diagnostics_capture_rendered_stores_the_exact_prompt_and_messages() {
        let mut context = DictationContext::default();
        context.polish.cursor_context = Some("doc".into());
        context.polish.cursor_context_trace =
            openless_core::polish_diagnostics::PolishContextTrace::included();
        let path = PathBuf::from("/tmp/models/llm/gemma-4-E4B-it-Q4_0.gguf");
        let mut capture = DiagnosticsCapture::new(&path, &context);
        let messages = prompt_messages(&context, "你好世界", true);
        let prompt = render_gemma4_prompt(&messages);
        capture.rendered(
            &prompt,
            &messages,
            PolishTemplateKind::Gemma4Canonical,
            &context,
        );
        // 同一份值：诊断里的 prompt/messages 就是喂给 tokenizer 的那份（含真实
        // prior turns 的结构化消息）。
        assert_eq!(capture.diagnostics.prompt.as_deref(), Some(prompt.as_str()));
        assert_eq!(
            capture.diagnostics.messages.as_ref().map(|m| m.len()),
            Some(messages.len())
        );
        assert_eq!(
            capture.diagnostics.messages.as_ref().unwrap()[0].role,
            "system"
        );
        assert_eq!(
            capture
                .diagnostics
                .messages
                .as_ref()
                .unwrap()
                .last()
                .unwrap()
                .role,
            "user"
        );
        assert_eq!(
            capture.diagnostics.template,
            Some(PolishTemplateKind::Gemma4Canonical)
        );
        assert_eq!(capture.diagnostics.add_bos, Some(true));
        assert_eq!(
            capture.diagnostics.add_bos_note.as_deref(),
            Some(ADD_BOS_NOTE)
        );
        // 渲染成功后上下文升级为 Included，正文为处理后文本。
        assert_eq!(
            capture.diagnostics.cursor.as_ref().unwrap().status,
            openless_core::polish_diagnostics::PolishContextStatus::Included
        );
    }

    #[test]
    fn diagnostics_capture_failure_keeps_a_sanitized_error_summary() {
        let context = DictationContext::default();
        let path = PathBuf::from("/tmp/models/llm/gemma-4-E4B-it-Q4_0.gguf");
        let mut capture = DiagnosticsCapture::new(&path, &context);
        capture.diagnostics.stage = PolishDiagnosticsStage::Tokenized;
        capture.failed(&BackendError::new(
            BackendErrorCode::InvalidArgument,
            "local LLM output exceeded its 1-token limit after a 9-token prompt; generated 12 characters",
        ));
        assert_eq!(
            capture.diagnostics.outcome,
            PolishDiagnosticsOutcome::Failed
        );
        let error = capture.diagnostics.error.as_ref().unwrap();
        assert_eq!(error.code, "InvalidArgument");
        assert!(error.message.contains("1-token limit"));
        // 超长错误信息截断到 400 字符，防意外泄漏。
        capture.failed(&BackendError::new(
            BackendErrorCode::Internal,
            std::iter::repeat('x').take(1_000).collect::<String>(),
        ));
        assert!(
            capture
                .diagnostics
                .error
                .as_ref()
                .unwrap()
                .message
                .chars()
                .count()
                <= 400
        );
    }

    #[test]
    fn output_limit_error_is_invalid_argument() {
        let error = BackendError::new(
            BackendErrorCode::InvalidArgument,
            "local LLM output exceeded its token limit",
        );
        assert_eq!(error.code, BackendErrorCode::InvalidArgument);
        assert!(error.message.contains("output exceeded"));
    }

    #[test]
    fn gemma4_prompts_carry_the_english_language_constraint() {
        let mut context = DictationContext::default();
        context.polish.working_languages = vec!["English".into()];
        let legacy = prompt_messages(&context, "hello", false);
        let gemma = prompt_messages(&context, "hello", true);
        assert_eq!(legacy.len(), gemma.len());
        assert_eq!(legacy[0].content, gemma[0].content);
        assert!(gemma[0].content.contains("英文不翻译"));
        assert!(gemma[0].content.contains("阿拉伯数字不改成汉字"));
        assert!(!gemma[0].content.contains("Language constraint"));
        assert!(!gemma[0].content.contains("本地模型语言约束"));
        // 两种家族的 user 信封一致，差异只在约束块。
        assert_eq!(
            legacy.last().unwrap().content,
            gemma.last().unwrap().content
        );
    }
    fn temp_resolve_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("openless-llm-resolve-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp resolve dir");
        dir
    }

    #[test]
    fn model_resolution_is_fixed_to_gemma_regardless_of_directory_contents() {
        let dir = temp_resolve_dir("fixed");
        // 空目录、残留标记、目录里其它 GGUF（含旧回退 Qwen）都不影响解析结果：
        // 永远指向固定 Gemma 文件,不做任何回退/扫描。
        assert_eq!(DEFAULT_MODEL_FILE, "gemma-4-E4B-it-Q4_0.gguf");
        assert_eq!(resolve_model_in_dir(&dir), dir.join(DEFAULT_MODEL_FILE));
        // 升级后旧 E2B 即使仍在盘上也不得成为缺失 E4B 的隐式回退。
        std::fs::write(dir.join("gemma-4-E2B-it-Q4_0.gguf"), "x").unwrap();
        std::fs::write(dir.join(LEGACY_SWAP_MODEL_FILE), "x").unwrap();
        std::fs::write(dir.join(".active-model"), LEGACY_SWAP_MODEL_FILE).unwrap();
        std::fs::write(dir.join("aa-random.gguf"), "x").unwrap();
        assert_eq!(resolve_model_in_dir(&dir), dir.join(DEFAULT_MODEL_FILE));
        assert!(!resolve_model_in_dir(&dir).ends_with(LEGACY_SWAP_MODEL_FILE));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gemma4_prompt_rendering_disables_thinking_at_template_level() {
        let messages = [
            ChatTurn {
                role: "system".into(),
                content: "  SYS  ".into(),
            },
            ChatTurn {
                role: "user".into(),
                content: " hi ".into(),
            },
            ChatTurn {
                role: "assistant".into(),
                content: " old ".into(),
            },
            ChatTurn {
                role: "user".into(),
                content: " new ".into(),
            },
        ];
        let prompt = render_gemma4_prompt(&messages);
        assert_eq!(
            prompt,
            "<|turn>system\nSYS<turn|>\n\
             <|turn>user\nhi<turn|>\n\
             <|turn>model\nold<turn|>\n\
             <|turn>user\nnew<turn|>\n\
             <|turn>model\n"
        );
        // 关闭思考 = 不出现 <|think|> 注入与 <|channel>thought 通道。
        assert!(!prompt.contains("<|think|>"));
        assert!(!prompt.contains("<|channel>"));
        // BOS 由 tokenizer 注入（AddBos::Always），正文不得再包含 <bos> 文本。
        assert!(!prompt.contains("<bos>"));
    }

    struct NoopTextStream;

    impl TextStreamSink for NoopTextStream {
        fn publish(&self, _chunk: openless_core::TextStreamChunk) -> Result<(), BackendError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn missing_model_reports_unsupported() {
        let polisher = LocalLlamaPolisher::with_fixed_model(PathBuf::from(
            "/definitely/missing/openless-model.gguf",
        ));
        let attempt = polisher
            .polish(
                SessionId::new(),
                Arc::new(DictationContext::default()),
                "hello".into(),
                Arc::new(NoopTextStream),
            )
            .await;
        assert_eq!(
            attempt.result.unwrap_err().code,
            BackendErrorCode::Unsupported
        );
        // 模型缺失也必须携带诊断快照：阶段 ModelMissing、无 prompt、结局 Failed。
        let diagnostics = attempt
            .diagnostics
            .expect("missing model carries diagnostics");
        assert_eq!(
            diagnostics.stage,
            openless_core::polish_diagnostics::PolishDiagnosticsStage::ModelMissing
        );
        assert_eq!(
            diagnostics.outcome,
            openless_core::polish_diagnostics::PolishDiagnosticsOutcome::Failed
        );
        assert!(diagnostics.prompt.is_none());
        assert!(diagnostics.messages.is_none());
        // Debug 输出不得泄漏任何正文（此场景本无正文，守恒断言结构安全）。
        let rendered = format!("{diagnostics:?}");
        assert!(rendered.contains("ModelMissing"));
        assert!(rendered.contains("model: Some"));
    }

    #[test]
    fn fixed_rendered_prompt_ignores_legacy_style_and_language_preferences() {
        let context = DictationContext::default();
        let prompt = render_gemma4_prompt(&prompt_messages(
            &context,
            "Release v2.3 costs $12.50",
            true,
        ));
        assert!(prompt.contains("英文不翻译"));
        assert!(prompt.contains("阿拉伯数字不改成汉字"));
        assert!(!prompt.contains("{{"));
        assert!(!prompt.contains("旧风格"));
        assert!(!prompt.contains("翻译成中文"));
        assert!(!prompt.contains("Language constraint"));
    }

    #[tokio::test]
    #[ignore = "explicit real E4B text evaluation"]
    async fn fixed_template_e4b_text_fidelity() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("models/llm")
            .join(DEFAULT_MODEL_FILE);
        assert!(path.is_file(), "missing {}", path.display());
        let polisher = Arc::new(LocalLlamaPolisher::with_fixed_model(path));
        let cases: [(&str, &str, &[&str]); 5] = [
            (
                "digits",
                "嗯我们有 3 个项目和 12,500 条请求，完成率 98.5%。",
                &["3", "12,500", "98.5%"],
            ),
            (
                "english",
                "uh the release v2.3.1 costs $12.50 and is due on 2026-09-08",
                &["v2.3.1", "$12.50", "2026-09-08", "release"],
            ),
            (
                "mixed",
                "这个 API update 用 Rust 1.89.0，ETA is Friday，预算 USD 1,250.00。",
                &["API update", "Rust 1.89.0", "ETA is Friday", "USD 1,250.00"],
            ),
            (
                "amount",
                "金额是 2,500 元，折扣 12.5%，版本 v4.0.2，容量 512 GB。",
                &["2,500", "12.5%", "v4.0.2", "512 GB"],
            ),
            (
                "han_digits",
                "我们有三个项目，明天十点开会。",
                &["三个", "十点"],
            ),
        ];
        let mut failures = Vec::new();
        for (name, input, expected) in cases {
            let context = DictationContext::default();
            let expected_prompt = render_gemma4_prompt(&prompt_messages(&context, input, true));
            let attempt = polisher
                .polish(
                    SessionId::new(),
                    Arc::new(context),
                    input.into(),
                    Arc::new(NoopTextStream),
                )
                .await;
            let diagnostics = attempt.diagnostics.expect("actual diagnostics");
            assert_eq!(
                diagnostics.prompt.as_deref(),
                Some(expected_prompt.as_str())
            );
            match attempt.result {
                Ok(output) => {
                    let mut missing: Vec<_> = expected
                        .iter()
                        .filter(|value| !output.text.contains(**value))
                        .copied()
                        .collect();
                    if name == "english"
                        && output
                            .text
                            .chars()
                            .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
                    {
                        missing.push("English-only output");
                    }
                    println!(
                        "FIXED_E4B case={name} input={input:?} output={:?} missing={missing:?}",
                        output.text
                    );
                    if !missing.is_empty() {
                        failures.push(name);
                    }
                }
                Err(error) => {
                    println!("FIXED_E4B case={name} error={error}");
                    failures.push(name);
                }
            }
        }
        assert!(failures.is_empty(), "fidelity failures: {failures:?}");
    }

    /// Exercises the installed GGUF only when explicitly requested because model loading is large
    /// and Metal inference is intentionally not part of the regular test suite.
    #[tokio::test]
    #[ignore = "set OPENLESS_LOCAL_LLM_SMOKE=1 to run the installed-GGUF smoke test"]
    async fn installed_gguf_smoke_chinese_english_mixed_long_and_cancel() {
        assert_eq!(
            std::env::var("OPENLESS_LOCAL_LLM_SMOKE").as_deref(),
            Ok("1")
        );
        let model_path = std::env::var_os("OPENLESS_LOCAL_LLM_MODEL_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                resolve_model_in_dir(
                    &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .parent()
                        .expect("src-tauri has an app parent")
                        .join("models/llm"),
                )
            });
        assert!(
            model_path.is_file(),
            "missing smoke model: {}",
            model_path.display()
        );
        let polisher = Arc::new(LocalLlamaPolisher::with_fixed_model(model_path.clone()));
        let mut english_context = DictationContext::default();
        english_context.polish.working_languages = vec!["English".into()];
        let mut mixed_context = DictationContext::default();
        mixed_context.polish.working_languages = vec!["简体中文".into(), "English".into()];
        let cases = [
            ("zh", DictationContext::default(), "帮我把这句话整理一下，明天上午十点和产品团队开会。"),
            (
                "en",
                english_context,
                "please make this concise: the deployment is delayed because CI is red.",
            ),
            (
                "mixed",
                mixed_context,
                "请把这段 Slack update polish 一下：API rate limit 已修复，ETA is Friday，涉及 3 个客户和 12,500 条请求。",
            ),
        ];
        for (name, context, input) in cases {
            let started = std::time::Instant::now();
            let attempt = polisher
                .polish(
                    SessionId::new(),
                    Arc::new(context),
                    input.into(),
                    Arc::new(NoopTextStream),
                )
                .await;
            let output = attempt
                .result
                .unwrap_or_else(|error| panic!("{name} failed: {error}"));
            // 真实 prompt 相等断言：诊断里的最终 prompt 必须就是本次喂给模型的
            // 渲染结果（Gemma canonical 路径），token 数已记录。
            let diagnostics = attempt.diagnostics.expect("smoke case carries diagnostics");
            assert_eq!(diagnostics.stage, PolishDiagnosticsStage::Succeeded);
            let prompt = diagnostics.prompt.as_deref().expect("prompt captured");
            assert!(prompt.starts_with("<|turn>system\n"));
            assert!(prompt.ends_with("<|turn>model\n"));
            assert!(diagnostics.prompt_tokens.is_some_and(|tokens| tokens > 0));
            assert!(diagnostics.generation_budget.is_some());
            println!(
                "local_llm_smoke case={name} elapsed_ms={} output={:?}",
                started.elapsed().as_millis(),
                output.text
            );
            assert!(
                !output.text.trim().is_empty(),
                "{name} returned empty output"
            );
            if name == "en" {
                assert!(
                    output.text.is_ascii(),
                    "English context changed language: {:?}",
                    output.text
                );
                assert!(output.text.contains("CI"));
            }
            if name == "mixed" {
                assert!(output.text.contains("API"));
                assert!(output.text.contains("12,500"));
                assert!(output.text.contains('3'));
            }
        }
        // Reuse the already-loaded backend/model. llama.cpp permits backend initialization once
        // per process, so a second LocalLlamaPolisher cannot provide a valid test configuration.
        let output_limit_attempt = generate(
            model_path.clone(),
            Arc::clone(&polisher.loaded),
            Arc::clone(&polisher.backend),
            Arc::clone(&polisher.inference),
            Arc::new(AtomicBool::new(false)),
            &DictationContext::default(),
            "请完整润色这句话，不要省略任何信息：版本 v9.8.7 于周五发布。",
            1,
        );
        let output_limit = output_limit_attempt
            .result
            .expect_err("one-token generation cap must be reported as an error");
        println!("local_llm_smoke case=output_limit result={output_limit:?}");
        assert_eq!(output_limit.code, BackendErrorCode::InvalidArgument);
        assert!(
            output_limit.message.contains("1-token limit")
                || output_limit.message.contains("output exceeded")
        );
        // 生成耗尽的诊断快照：阶段 Generating、prompt 与消息在场、错误信息不含正文。
        let output_limit_diag = output_limit_attempt
            .diagnostics
            .expect("budget exhaustion carries diagnostics");
        assert_eq!(
            output_limit_diag.stage,
            openless_core::polish_diagnostics::PolishDiagnosticsStage::Generating
        );
        assert!(output_limit_diag
            .prompt
            .as_deref()
            .unwrap()
            .contains("<|turn>"));
        assert!(!output_limit.message.contains("版本"));
        assert!(!output_limit.message.contains("prefix:"));

        let long_input = (1..=16)
            .map(|day| format!(
                "第{day}项：负责人团队{day}完成模块{day}的验收；版本 v{day}.2.{day} 在 2026-09-{day:02} 发布，处理 {} 条请求，预算 {} 元，风险为供应商接口延迟，下一步由项目经理在周五复核。",
                1_000 + day * 37,
                8_000 + day * 113,
            ))
            .collect::<Vec<_>>()
            .join("
");
        // Use the stock Light style, not a test-authored summarization instruction: this is the
        // application's standard non-structuring polish contract for a long status document.
        let long_context = DictationContext::default();
        let started = std::time::Instant::now();
        let attempt = polisher
            .polish(
                SessionId::new(),
                Arc::new(long_context),
                long_input,
                Arc::new(NoopTextStream),
            )
            .await;
        let output = attempt
            .result
            .expect("long input over 512 tokens should prefill successfully");
        println!(
            "local_llm_smoke case=long elapsed_ms={} output={:?}",
            started.elapsed().as_millis(),
            output.text
        );
        assert!(!output.text.trim().is_empty());
        for key in [
            "模块1",
            "v1.2.1",
            "2026-09-01",
            "模块16",
            "v16.2.16",
            "2026-09-16",
        ] {
            assert!(
                output.text.contains(key),
                "long output lost key {key}: {:?}",
                output.text
            );
        }

        let session_id = SessionId::new();
        let cancelled_polisher = Arc::clone(&polisher);
        let task = tokio::spawn(async move {
            cancelled_polisher
                .polish(
                    session_id,
                    Arc::new(DictationContext::default()),
                    std::iter::repeat_n("cancel me ", 100).collect(),
                    Arc::new(NoopTextStream),
                )
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        polisher.cancel(session_id).await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(120), task)
            .await
            .expect("cancelled local inference did not finish")
            .expect("cancelled local inference task panicked");
        println!("local_llm_smoke case=cancel result={result:?}");
        assert_eq!(result.result.unwrap_err().code, BackendErrorCode::Cancelled);
    }

    /// 预载缺失模型：状态机翻到 failed、错误为 Unsupported，不 panic。
    #[tokio::test]
    async fn preload_missing_model_reports_failed_state() {
        let polisher = LocalLlamaPolisher::with_fixed_model(PathBuf::from(
            "/definitely/missing/openless-model.gguf",
        ));
        // 状态回调在失败转移时触发（Coordinator 依赖它广播事件）。重复的同一失败
        // 不重复通知：状态未变化就不打扰订阅方。
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let sink = Arc::clone(&seen);
        polisher.set_status_watcher(Box::new(move |status| {
            sink.lock().unwrap().push(status.state.clone());
        }));
        let result = polisher.preload().await;
        assert_eq!(result.unwrap_err().code, BackendErrorCode::Unsupported);
        let status = polisher.status();
        assert_eq!(status.state, "failed");
        assert!(status.error.is_some_and(|error| error.contains("missing")));
        assert_eq!(seen.lock().unwrap().as_slice(), ["failed"]);
        polisher.preload().await.unwrap_err();
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["failed"],
            "unchanged state must not re-notify"
        );
    }

    /// 预载与润色共享同一 slot：预载后 slot 已是 Gemma，generate 直接复用（不二次
    /// 载入）。需要真实 GGUF（AppSupport 固定路径），与既有冒烟测试同款开关。
    #[tokio::test]
    #[ignore = "set OPENLESS_LOCAL_LLM_SMOKE=1 to run the preload shared-slot test"]
    async fn preload_populates_the_shared_dictation_slot() {
        assert_eq!(
            std::env::var("OPENLESS_LOCAL_LLM_SMOKE").as_deref(),
            Ok("1")
        );
        let polisher = Arc::new(LocalLlamaPolisher::new());
        let model_path = resolve_active_model_path();
        assert!(
            model_path.is_file(),
            "missing fixed model: {}",
            model_path.display()
        );
        let started = std::time::Instant::now();
        polisher
            .preload()
            .await
            .expect("preload must load the fixed model");
        println!(
            "preload_shared_slot case=preload elapsed_ms={} file={}",
            started.elapsed().as_millis(),
            model_path.display()
        );
        assert_eq!(polisher.status().state, "loaded");
        assert_eq!(
            polisher
                .loaded
                .lock()
                .unwrap()
                .as_ref()
                .map(|m| m.path.clone()),
            Some(model_path.clone()),
            "preload must populate the same slot the polish path uses"
        );

        let context = DictationContext::default();
        let attempt = polisher
            .polish(
                SessionId::new(),
                Arc::new(context),
                "把这句话整理一下：今天下午三点开会。".into(),
                Arc::new(NoopTextStream),
            )
            .await;
        let output = attempt.result.expect("polish after preload");
        assert!(!output.text.trim().is_empty());
        // 润色没有触发换载：slot 里的路径仍是预载写入的那份。
        assert_eq!(
            polisher
                .loaded
                .lock()
                .unwrap()
                .as_ref()
                .map(|m| m.path.clone()),
            Some(model_path),
            "polish must reuse the preloaded slot instead of reloading"
        );
        assert_eq!(polisher.status().state, "loaded");
    }

    /// 导入/切换模型后不需要重启：同一个 polisher 的 slot 换到新文件时先丢弃旧模型再
    /// 复用全进程唯一的 backend 载入新模型。用工作区两份真实 GGUF 验证换载路径。
    #[tokio::test]
    #[ignore = "set OPENLESS_LOCAL_LLM_SMOKE=1 to run the model hot-swap smoke test"]
    async fn model_hot_swap_reloads_without_restart() {
        assert_eq!(
            std::env::var("OPENLESS_LOCAL_LLM_SMOKE").as_deref(),
            Ok("1")
        );
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri has an app parent")
            .join("models/llm");
        let first = dir.join(DEFAULT_MODEL_FILE);
        let second = dir.join(LEGACY_SWAP_MODEL_FILE);
        assert!(first.is_file(), "missing model: {}", first.display());
        assert!(second.is_file(), "missing swap model: {}", second.display());

        let slot: Arc<Mutex<Option<Arc<LoadedModel>>>> = Arc::new(Mutex::new(None));
        let backend_slot: Arc<Mutex<Option<Arc<LlamaBackend>>>> = Arc::new(Mutex::new(None));
        let inference = Arc::new(Mutex::new(()));
        let context = DictationContext::default();
        for (name, path) in [("first", first), ("second", second)] {
            let started = std::time::Instant::now();
            let attempt = generate(
                path.clone(),
                Arc::clone(&slot),
                Arc::clone(&backend_slot),
                Arc::clone(&inference),
                Arc::new(AtomicBool::new(false)),
                &context,
                "把这句话整理一下：今天下午三点开会。",
                usize::MAX,
            );
            let output = attempt
                .result
                .unwrap_or_else(|error| panic!("{name} failed: {error}"));
            let label_model = output
                .llm_call_label
                .unwrap_or_else(|| panic!("{name} produced no call label"))
                .model;
            println!(
                "hot_swap case={name} file={label_model} elapsed_ms={} output={:?}",
                started.elapsed().as_millis(),
                output.text
            );
            assert_eq!(
                label_model,
                path.file_name().unwrap().to_string_lossy().as_ref()
            );
        }
        // backend 只初始化一次且被两次换载复用。
        assert!(backend_slot.lock().unwrap().is_some());
    }

    /// 未见过的英文/中英混合短句评测（否定词、金额、日期、命名实体），全程用生产参数，
    /// 不为单题调参。gemma 的语言保持失败会在这里如实暴露。
    #[tokio::test]
    #[ignore = "set OPENLESS_LOCAL_LLM_LANG_EVAL=1 to run the unseen-language eval"]
    async fn gemma_unseen_language_eval() {
        assert_eq!(
            std::env::var("OPENLESS_LOCAL_LLM_LANG_EVAL").as_deref(),
            Ok("1")
        );
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("src-tauri has an app parent")
            .join("models/llm");
        let model_path = resolve_model_in_dir(&dir);
        assert!(
            model_path.is_file(),
            "missing model: {}",
            model_path.display()
        );
        let polisher = Arc::new(LocalLlamaPolisher::with_fixed_model(model_path));

        let has_cjk = |text: &str| text.chars().any(|c| ('\u{4E00}'..='\u{9FFF}').contains(&c));
        let has_negation_en =
            |text: &str| text.to_lowercase().contains("not") || text.to_lowercase().contains("n't");

        let mut en = DictationContext::default();
        en.polish.working_languages = vec!["English".into()];
        let mut mixed = DictationContext::default();
        mixed.polish.working_languages = vec!["简体中文".into(), "English".into()];

        // (name, context, input, 断言集)：keep_en = 输出必须纯 ASCII；混合样本要求
        // CJK 与关键拉丁词/数字并存；否定与数字/实体逐项保留。
        let cases: Vec<(&str, DictationContext, &str, Vec<&str>)> = vec![];
        let _ = cases; // 结构化断言在下方逐 case 写死，避免闭包借用混淆。

        let en_inputs: Vec<(&str, &str, Vec<&str>)> = vec![
            (
                "en_negation_pr",
                "don't merge the two PRs yet, we are not ready to release",
                vec![],
            ),
            (
                "en_invoice_amount_date",
                "please confirm the invoice for $12,450.90 was paid on March 3, 2026",
                vec!["12,450.90", "2026"],
            ),
            (
                "en_entity_oncall",
                "restart postgres-primary and page Priya once the failover completes",
                vec!["Priya"],
            ),
            (
                "en_negation_webhook",
                "the Stripe webhook never fires in production, only staging",
                vec!["Stripe"],
            ),
            (
                "en_meeting_move",
                "I can't make standup on Tuesday, move it to Thursday at 9:30 AM",
                vec!["Thursday", "9:30"],
            ),
        ];
        let mut failures: Vec<String> = Vec::new();
        for (name, input, tokens) in en_inputs {
            let started = std::time::Instant::now();
            let attempt = polisher
                .polish(
                    SessionId::new(),
                    Arc::new(en.clone()),
                    input.into(),
                    Arc::new(NoopTextStream),
                )
                .await;
            let output = attempt
                .result
                .unwrap_or_else(|error| panic!("{name} failed: {error}"));
            println!(
                "lang_eval case={name} elapsed_ms={} output={:?}",
                started.elapsed().as_millis(),
                output.text
            );
            if output.text.trim().is_empty() {
                failures.push(format!("{name}: empty output"));
            }
            if !output.text.is_ascii() {
                failures.push(format!("{name}: lost English -> {:?}", output.text));
            }
            if has_negation_en(input) && !has_negation_en(&output.text) {
                failures.push(format!("{name}: lost negation -> {:?}", output.text));
            }
            for token in tokens {
                if !output.text.contains(token) {
                    failures.push(format!("{name}: lost token {token} -> {:?}", output.text));
                }
            }
        }

        let mixed_inputs: Vec<(&str, &str, Vec<&str>)> = vec![
            (
                "mixed_budget",
                "把 Q3 的预算报一下：marketing 花了 ¥128,000，比上季度多 15%",
                vec!["marketing", "128,000", "15%"],
            ),
            (
                "mixed_deadline",
                "提醒老板 Figma 的 deadline 是 2026-10-15，不能延期",
                vec!["Figma", "2026-10-15"],
            ),
            (
                "mixed_aws_bill",
                "理顺一下：AWS 上个月的 bill 是 $3,847.20，S3 占大头",
                vec!["AWS", "3,847.20", "S3"],
            ),
            (
                "mixed_rate_limit",
                "这个 API 的 rate limit 从 1000 提到 5000 了，QA team 不用再 mock 数据了",
                vec!["QA", "5000"],
            ),
        ];
        for (name, input, tokens) in mixed_inputs {
            let started = std::time::Instant::now();
            let attempt = polisher
                .polish(
                    SessionId::new(),
                    Arc::new(mixed.clone()),
                    input.into(),
                    Arc::new(NoopTextStream),
                )
                .await;
            let output = attempt
                .result
                .unwrap_or_else(|error| panic!("{name} failed: {error}"));
            println!(
                "lang_eval case={name} elapsed_ms={} output={:?}",
                started.elapsed().as_millis(),
                output.text
            );
            if output.text.trim().is_empty() {
                failures.push(format!("{name}: empty output"));
            }
            if !has_cjk(&output.text) {
                failures.push(format!("{name}: lost Chinese -> {:?}", output.text));
            }
            for token in tokens {
                if !output.text.contains(token) {
                    failures.push(format!("{name}: lost token {token} -> {:?}", output.text));
                }
            }
        }
        // 断言不因聚合而削弱：任一 case 的任一检查不过本测试仍失败；先打印全部真实
        // 输出再统一报告，保留完整失败画像。
        assert!(
            failures.is_empty(),
            "unseen-language eval failures ({}):\n  {}",
            failures.len(),
            failures.join("\n  ")
        );
    }

    #[test]
    fn unload_blocking_clears_both_slots_in_model_before_backend_order() {
        let polisher = LocalLlamaPolisher::new();
        let backend = Arc::new(LlamaBackend::init().expect("llama backend init"));
        // 不加载真实模型：这里只验证退出清理会清空两个 slot，以及顺序（模型先于
        // backend 释放）。Metal 缓冲的归还在真机退出验证里覆盖。
        *polisher.backend.lock().unwrap() = Some(Arc::clone(&backend));
        *polisher.state.lock().unwrap() = LoadState::Loaded;

        polisher.unload_blocking();

        assert!(polisher.loaded.lock().unwrap().is_none());
        assert!(polisher.backend.lock().unwrap().is_none());
        assert!(matches!(*polisher.state.lock().unwrap(), LoadState::NotLoaded));
    }

    #[test]
    fn unload_blocking_gives_up_instead_of_hanging_when_a_request_holds_inference() {
        let polisher = LocalLlamaPolisher::new();
        // 模拟在途润色：持有 inference 锁。
        let held = Arc::clone(&polisher.inference);
        let guard = held.lock().unwrap();

        let started = Instant::now();
        polisher.unload_blocking();
        let elapsed = started.elapsed();

        // 必须放弃清理并返回，不能无限等锁。
        assert!(
            elapsed < Duration::from_secs(5),
            "unload_blocking waited too long: {elapsed:?}"
        );
        drop(guard);
    }

    #[test]
    fn unload_blocking_is_idempotent_and_survives_a_poisoned_lock() {
        let polisher = LocalLlamaPolisher::new();
        polisher.unload_blocking();
        polisher.unload_blocking();

        // 让 loaded 锁中毒，模拟在途请求 panic 后的退出场景：清理必须照常返回，
        // 不能因为二次 panic 把退出变成 abort。
        let loaded = Arc::clone(&polisher.loaded);
        let _ = std::panic::catch_unwind(move || {
            let _guard = loaded.lock().unwrap();
            panic!("simulated in-flight worker panic");
        });
        polisher.unload_blocking();
        assert!(lock_ignoring_poison(&polisher.loaded).is_none());
    }
}
