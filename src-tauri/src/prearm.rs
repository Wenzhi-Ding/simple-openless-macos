//! 热键按下瞬间的预置录音（pre-arm）。
//!
//! 背景：一次听写从按键到麦克风真正上线，改造前要走完「修饰键 150ms 宽限 →
//! 光标/屏幕上下文捕获 → ASR 会话创建 → cpal 建流 + play」，典型 250~600ms，
//! 用户按下即说的第一个字常被吞掉。
//!
//! 方案：热键 Pressed 边沿一到达，宿主立刻在后台线程把 cpal 输入流拉起来，
//! PCM 先攒进 [`crate::recorder::PrearmTap`] 的内存缓冲；core 会话链走到
//! `recorder.start` 时调用 [`PrearmRegistry::adopt`] 接管这条已在运行的流，
//! 缓冲音频回放给 ASR 后切实时直通。从流上线那一刻起的声音全部保留。
//!
//! 安全边界：
//! - 组合键（修饰键+普通键）的 Combined 边沿到达 → [`PrearmRegistry::cancel`]
//!   立刻停流；未被接管超过 TTL 也自动停。误预置的音频只在内存缓冲里，
//!   不喂 ASR、不落盘（归档器在接管时才创建）。
//! - 预置期间麦克风指示灯会真实点亮（系统行为），组合键场景通常只亮零点几秒。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Condvar, Mutex as StdMutex};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tauri::Manager;

use crate::core_adapters::AppHandleSlot;
use crate::recorder::{PrearmTap, Recorder, RecorderError};

/// 预置流等待被接管的上限。覆盖最坏链路：150ms 修饰键宽限 + 最坏 ~1.3s 的
/// 光标 AX 探测 + 引擎启动。到期未接管说明这次按键不是听写（组合键、防抖
/// 拦截、core 链失败），必须收掉麦克风。
const PREARM_TTL: Duration = Duration::from_secs(4);

/// adopt 遇到「流还在建」时等建流完成的超时。cpal 建流正常 50~300ms；
/// 超时后放弃预置走新建（与没有预置时的行为一致），不无限阻塞会话启动。
const ADOPT_BUILD_TIMEOUT: Duration = Duration::from_secs(2);

/// 一条已建好的预置流。`stop` 是消费式停止（生产实现 = `Recorder::stop`）。
pub(crate) struct PrearmedRecorder {
    pub tap: Arc<PrearmTap>,
    pub stop: Box<dyn FnOnce() + Send>,
    pub error_rx: Receiver<RecorderError>,
    pub mic_name: Option<String>,
    pub created_at: Instant,
}

/// 建流工厂：测试注入假实现，生产实现调用 `Recorder::start`。
type StreamFactory = Arc<
    dyn Fn(Option<String>, Arc<PrearmTap>) -> Result<PrearmedRecorder, RecorderError> + Send + Sync,
>;

/// 建流结果的交接盒。所有权规则：
/// - 结果只能被取走一次（`result` 是 Option）。
/// - adopt 先认领（`claimed = true`）再等结果；认领后由 adopt 负责收尾
///   （包括等不到时把晚到的结果停掉）。
/// - 没人认领（cancel 清了 slot）时，build 线程在 `finish` 后负责停掉。
#[derive(Default)]
struct PrearmBuildInner {
    result: Option<Result<PrearmedRecorder, RecorderError>>,
    claimed: bool,
}

struct PrearmBuild {
    inner: StdMutex<PrearmBuildInner>,
    ready: Condvar,
}

impl PrearmBuild {
    fn new() -> Self {
        Self {
            inner: StdMutex::new(PrearmBuildInner::default()),
            ready: Condvar::new(),
        }
    }

    fn finish(&self, result: Result<PrearmedRecorder, RecorderError>) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .result = Some(result);
        self.ready.notify_all();
    }

    fn claim(&self) {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .claimed = true;
    }

    /// 认领方等待结果。超时即放弃认领；若结果恰好同时到达，取走交给调用方处置。
    fn take_claimed(&self, timeout: Duration) -> ClaimedOutcome {
        let deadline = Instant::now() + timeout;
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            if let Some(result) = inner.result.take() {
                return ClaimedOutcome::Finished(result);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                // 放弃认领。build 线程随后的 finish 会看到未认领并自行停流；
                // 若结果已经躺在里面，由我们顺手处置，免得双头管理。
                inner.claimed = false;
                return match inner.result.take() {
                    Some(result) => ClaimedOutcome::Finished(result),
                    None => ClaimedOutcome::TimedOut,
                };
            }
            let (guard, _) = self
                .ready
                .wait_timeout(inner, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner = guard;
        }
    }
}

enum ClaimedOutcome {
    Finished(Result<PrearmedRecorder, RecorderError>),
    TimedOut,
}

enum PrearmState {
    /// 后台线程正在建流；adopt 可以认领并等它完成。
    Building(Arc<PrearmBuild>),
    Ready(PrearmedRecorder),
}

struct PrearmEntry {
    id: u64,
    state: PrearmState,
}

/// 预置流单例注册表。同一时刻最多一条预置流；热键 bridge（trigger/cancel）
/// 与 `TauriAudioRecorder::start`（adopt）从两侧访问。
pub(crate) struct PrearmRegistry {
    app: AppHandleSlot,
    ttl: Duration,
    factory: StreamFactory,
    slot: Mutex<Option<PrearmEntry>>,
    generation: AtomicU64,
}

impl PrearmRegistry {
    pub(crate) fn new(app: AppHandleSlot) -> Self {
        Self::with_factory(app, PREARM_TTL, Self::real_factory())
    }

    fn with_factory(app: AppHandleSlot, ttl: Duration, factory: StreamFactory) -> Self {
        Self {
            app,
            ttl,
            factory,
            slot: Mutex::new(None),
            generation: AtomicU64::new(0),
        }
    }

    fn real_factory() -> StreamFactory {
        Arc::new(|mic_name, tap| {
            let consumer: Arc<dyn crate::recorder::AudioConsumer> = tap.clone();
            let level_tap = Arc::clone(&tap);
            let level_handler: Arc<dyn Fn(f32) + Send + Sync> =
                Arc::new(move |level| level_tap.forward_level(level));
            let created_at = Instant::now();
            let (recorder, error_rx, _archive_active) =
                Recorder::start(mic_name.clone(), consumer, level_handler, None)?;
            Ok(PrearmedRecorder {
                tap,
                stop: Box::new(move || recorder.stop()),
                error_rx,
                mic_name,
                created_at,
            })
        })
    }

    /// 热键 Pressed 边沿调用。立即返回；建流（停预览 + cpal build/play，
    /// 几十到几百毫秒）在后台线程完成。已有预置（在建或就绪）时复用，不重复建。
    pub(crate) fn trigger(self: &Arc<Self>, mic_name: Option<String>) {
        enum Action {
            /// 建流中的预置来自同一颗按键的同一组偏好，直接复用。
            KeepBuilding,
            /// 还活着的流直接复用（换代 + 新 TTL）。
            Reuse(u64),
            /// 建一条新流（旧 Ready 实例停流后重建）。
            Build(u64, Arc<PrearmBuild>),
        }
        let action = {
            let mut slot = self.slot.lock();
            match slot.as_ref().map(|entry| &entry.state) {
                Some(PrearmState::Building(_)) => Action::KeepBuilding,
                Some(PrearmState::Ready(prearmed))
                    if prearmed.mic_name == mic_name
                        && prearmed.created_at.elapsed() < self.ttl =>
                {
                    // 复用还活着的流：缓冲里可能有上一次按键窗口攒下的旧音频，
                    // 必须清空，从本次按键重新累积；计时起点也刷到本次按键。
                    prearmed.tap.clear();
                    let id = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
                    let entry = slot.take().expect("entry was just observed");
                    let mut prearmed = match entry.state {
                        PrearmState::Ready(prearmed) => prearmed,
                        PrearmState::Building(_) => unreachable!("matched Ready above"),
                    };
                    prearmed.created_at = Instant::now();
                    *slot = Some(PrearmEntry {
                        id,
                        state: PrearmState::Ready(prearmed),
                    });
                    Action::Reuse(id)
                }
                _ => {
                    // 过期/不匹配的占位清掉。停旧流可能等线程 join 占几十毫秒
                    // ——热键 bridge 上的一次性收尾，比泄漏一条流划算。
                    if let Some(old) = slot.take() {
                        if let PrearmState::Ready(prearmed) = old.state {
                            (prearmed.stop)();
                        }
                    }
                    let id = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
                    let build = Arc::new(PrearmBuild::new());
                    *slot = Some(PrearmEntry {
                        id,
                        state: PrearmState::Building(Arc::clone(&build)),
                    });
                    Action::Build(id, build)
                }
            }
        };
        match action {
            Action::KeepBuilding => {}
            Action::Reuse(id) => self.spawn_ttl_watch(id),
            Action::Build(id, build) => {
                let registry = Arc::clone(self);
                if std::thread::Builder::new()
                    .name("openless-recorder-prearm".into())
                    .spawn(move || registry.build_on_thread(id, build, mic_name))
                    .is_err()
                {
                    // 线程都没起得来：清掉占位，让下次按键重试。
                    let mut slot = self.slot.lock();
                    if matches!(slot.as_ref(), Some(entry) if entry.id == id) {
                        *slot = None;
                    }
                }
            }
        }
    }

    fn build_on_thread(self: Arc<Self>, id: u64, build: Arc<PrearmBuild>, mic_name: Option<String>) {
        // 交接盒随线程走：即使 slot 已被 adopt 取走（对方已认领交接盒并在等
        // 结果），建流也必须完成并投递，否则认领方会干等到超时。

        // 设置页的麦克风预览与录音共用输入设备；正式 recorder.start 会先停预览，
        // 预置遵循同一约定（预览只在设置页打开时存在）。
        if let Some(app) = self.app.lock().clone() {
            let state = app.state::<crate::commands::MicrophoneMonitorState>();
            let preview = state.lock().take();
            if let Some(preview) = preview {
                preview.stop();
            }
        }

        let result = (self.factory)(mic_name, PrearmTap::new());
        build.finish(result);

        let mut slot = self.slot.lock();
        match slot.as_mut() {
            Some(entry) if entry.id == id => {
                // adopt 没来过（它要先从 slot 取走 entry），结果必定还躺着。
                let result = build
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .result
                    .take()
                    .expect("build result is present unless adopt claimed it");
                match result {
                    Ok(prearmed) => {
                        entry.state = PrearmState::Ready(prearmed);
                        drop(slot);
                        self.spawn_ttl_watch(id);
                    }
                    Err(error) => {
                        log::info!("[prearm] stream build failed: {error}");
                        *slot = None;
                    }
                }
            }
            _ => {
                // slot 里已经不是我们：adopt 认领过就归 adopt 管；否则（cancel/
                // 换代）没人会接管，把建好的流停掉。
                let mut inner = build
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !inner.claimed {
                    if let Some(Ok(prearmed)) = inner.result.take() {
                        (prearmed.stop)();
                    }
                }
            }
        }
    }

    fn spawn_ttl_watch(self: &Arc<Self>, id: u64) {
        let registry = Arc::clone(self);
        let ttl = self.ttl;
        let _ = std::thread::Builder::new()
            .name("openless-recorder-prearm-ttl".into())
            .spawn(move || {
                std::thread::sleep(ttl);
                let expired = {
                    let mut slot = registry.slot.lock();
                    match slot.as_ref() {
                        Some(entry) if entry.id == id => slot.take(),
                        _ => None,
                    }
                };
                if let Some(entry) = expired {
                    if let PrearmState::Ready(prearmed) = entry.state {
                        log::info!("[prearm] stream expired without adoption; stopping");
                        (prearmed.stop)();
                    }
                }
            });
    }

    /// 正式会话的 `recorder.start` 调用点：mic 匹配、流还活着、未过期 → 接管。
    /// 流还在建时认领并等它建好（上限 [`ADOPT_BUILD_TIMEOUT`]）。返回 None 时
    /// 调用方走新建路径，行为与没有预置完全一致。
    ///
    /// 会阻塞调用线程（condvar 等待），必须在 `spawn_blocking` 里调。
    pub(crate) fn adopt(&self, mic_name: Option<String>) -> Option<PrearmedRecorder> {
        let entry = {
            let mut slot = self.slot.lock();
            // 认领必须在同一把 slot 锁里完成：build 线程对「无人认领」的判断也要
            // 先拿 slot 锁，这样它永远看不到「entry 被取走但还没认领」的中间态。
            if let Some(PrearmEntry {
                state: PrearmState::Building(build),
                ..
            }) = slot.as_ref()
            {
                build.claim();
            }
            slot.take()?
        };
        let prearmed = match entry.state {
            PrearmState::Ready(prearmed) => Some(prearmed),
            PrearmState::Building(build) => match build.take_claimed(ADOPT_BUILD_TIMEOUT) {
                ClaimedOutcome::Finished(Ok(prearmed)) => Some(prearmed),
                ClaimedOutcome::Finished(Err(error)) => {
                    log::info!("[prearm] adoption skipped; build failed: {error}");
                    None
                }
                ClaimedOutcome::TimedOut => {
                    log::warn!("[prearm] adoption skipped; build did not finish in time");
                    None
                }
            },
        };
        let prearmed = prearmed?;
        if prearmed.mic_name != mic_name {
            log::info!("[prearm] microphone changed since key press; discarding pre-armed stream");
            (prearmed.stop)();
            return None;
        }
        if prearmed.created_at.elapsed() > self.ttl {
            (prearmed.stop)();
            return None;
        }
        // 预置期间流可能已死（设备掉线等）：错误通道有货即不可接管。
        match prearmed.error_rx.try_recv() {
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Ok(error) => {
                log::info!("[prearm] stream died before adoption: {error}");
                (prearmed.stop)();
                return None;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                (prearmed.stop)();
                return None;
            }
        }
        Some(prearmed)
    }

    /// 组合键边沿到达 / 关机：立刻停掉未接管的预置流。已被接管的（slot 空）
    /// 不受影响——会话自己的 cancel/stop 负责。
    pub(crate) fn cancel(&self) {
        let entry = self.slot.lock().take();
        if let Some(entry) = entry {
            if let PrearmState::Ready(prearmed) = entry.state {
                (prearmed.stop)();
            }
            // Building 状态：build 线程 finish 后发现 slot 已空且无人认领，
            // 会自行停掉建好的流。
        }
    }
}

/// 热键 Pressed 时是否值得预置：只有「这次按下会启动新会话」才预置。
/// 正在录音/转写/收尾时按下是停止，不预置。
pub(crate) fn should_prearm(snapshot: &openless_core::BackendSnapshot) -> bool {
    snapshot.dictation.session_id.is_none()
        && matches!(snapshot.dictation.phase, openless_core::DictationPhase::Idle)
}

/// 归一化麦克风偏好：空串/全空白 = 系统默认设备（与 Recorder 的语义一致）。
pub(crate) fn normalize_mic_name(name: &str) -> Option<String> {
    let trimmed = name.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::AudioConsumer as _;
    use std::sync::mpsc::channel;
    use std::sync::Mutex as StdMutex;

    #[derive(Default)]
    struct Fixture {
        stops: Arc<AtomicU64>,
        builds: Arc<AtomicU64>,
        build_delay: Duration,
        fail: bool,
        poisoned: Arc<StdMutex<Vec<RecorderError>>>,
        /// 生产里错误通道的发送端由音频线程持有，流的整个存活期都不掉线；
        /// 测试里也必须压着，否则 adopt 的 Disconnected 检查会误判流已死。
        error_senders: Arc<StdMutex<Vec<std::sync::mpsc::Sender<RecorderError>>>>,
    }

    fn fixture_registry(fixture: &Fixture) -> Arc<PrearmRegistry> {
        let stops = Arc::clone(&fixture.stops);
        let builds = Arc::clone(&fixture.builds);
        let build_delay = fixture.build_delay;
        let fail = fixture.fail;
        let poisoned = Arc::clone(&fixture.poisoned);
        let error_senders = Arc::clone(&fixture.error_senders);
        let factory: StreamFactory = Arc::new(move |mic_name, tap| {
            builds.fetch_add(1, Ordering::SeqCst);
            if build_delay > Duration::ZERO {
                std::thread::sleep(build_delay);
            }
            if fail {
                return Err(RecorderError::EngineFailed("fixture build failed".into()));
            }
            let (error_tx, error_rx) = channel::<RecorderError>();
            // 把预先安排好的错误塞进通道：adopt 的活性检查必须看到。
            if let Some(error) = poisoned.lock().unwrap().pop() {
                let _ = error_tx.send(error);
            }
            error_senders.lock().unwrap().push(error_tx);
            let stops = Arc::clone(&stops);
            Ok(PrearmedRecorder {
                tap,
                stop: Box::new(move || {
                    stops.fetch_add(1, Ordering::SeqCst);
                }),
                error_rx,
                mic_name,
                created_at: Instant::now(),
            })
        });
        Arc::new(PrearmRegistry::with_factory(
            crate::core_adapters::app_handle_slot(),
            Duration::from_millis(150),
            factory,
        ))
    }

    fn wait_for(predicate: impl Fn() -> bool, what: &str) {
        for _ in 0..200 {
            if predicate() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for {what}");
    }

    /// 等 build 线程把 Ready 落进 slot。
    fn wait_until_ready(registry: &PrearmRegistry) {
        wait_for(
            || {
                matches!(
                    registry.slot.lock().as_ref(),
                    Some(entry) if matches!(entry.state, PrearmState::Ready(_))
                )
            },
            "prearm stream to become ready",
        );
    }

    #[test]
    fn trigger_builds_once_and_adopt_hands_over_the_stream() {
        let fixture = Fixture::default();
        let registry = fixture_registry(&fixture);
        registry.trigger(Some("mic-a".into()));
        registry.trigger(Some("mic-a".into())); // 重复触发不得重复建流
        wait_until_ready(&registry);
        assert_eq!(fixture.builds.load(Ordering::SeqCst), 1);

        let adopted = registry.adopt(Some("mic-a".into())).expect("must adopt");
        assert_eq!(adopted.mic_name.as_deref(), Some("mic-a"));
        drop(adopted); // 接管后 registry 不再管停止

        assert!(registry.slot.lock().is_none());
    }

    #[test]
    fn adopt_rejects_a_different_microphone_and_stops_the_stream() {
        let fixture = Fixture::default();
        let registry = fixture_registry(&fixture);
        registry.trigger(Some("mic-a".into()));
        wait_until_ready(&registry);

        assert!(registry.adopt(Some("mic-b".into())).is_none());
        assert_eq!(fixture.stops.load(Ordering::SeqCst), 1);
        assert!(registry.slot.lock().is_none());
    }

    #[test]
    fn adopt_rejects_a_stream_that_died_during_prearm() {
        let fixture = Fixture::default();
        fixture
            .poisoned
            .lock()
            .unwrap()
            .push(RecorderError::EngineFailed("device unplugged".into()));
        let registry = fixture_registry(&fixture);
        registry.trigger(None);
        wait_until_ready(&registry);

        assert!(registry.adopt(None).is_none());
        assert_eq!(fixture.stops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn adopt_waits_for_an_in_flight_build_to_finish() {
        let fixture = Fixture {
            build_delay: Duration::from_millis(50),
            ..Fixture::default()
        };
        let registry = fixture_registry(&fixture);
        registry.trigger(None);
        // 不 wait_until_ready：直接 adopt，必须能等到建流完成。
        let adopted = registry.adopt(None).expect("adopt must wait for build");
        assert!(adopted.mic_name.is_none());
    }

    #[test]
    fn cancel_stops_a_ready_stream() {
        let fixture = Fixture::default();
        let registry = fixture_registry(&fixture);
        registry.trigger(None);
        wait_until_ready(&registry);
        registry.cancel();
        assert_eq!(fixture.stops.load(Ordering::SeqCst), 1);
        assert!(registry.slot.lock().is_none());
    }

    #[test]
    fn build_in_flight_stops_itself_when_cancelled_midway() {
        let fixture = Fixture {
            build_delay: Duration::from_millis(50),
            ..Fixture::default()
        };
        let registry = fixture_registry(&fixture);
        registry.trigger(None);
        // 先等建流真正开始（fixture 进工厂即计数），再撤销——否则建流线程
        // 起手就发现 slot 已空直接返回，没有东西可停。
        wait_for(
            || fixture.builds.load(Ordering::SeqCst) == 1,
            "build to enter the factory",
        );
        registry.cancel();
        wait_for(
            || fixture.stops.load(Ordering::SeqCst) == 1,
            "in-flight build to stop itself after cancel",
        );
    }

    #[test]
    fn ready_stream_expires_without_adoption() {
        let fixture = Fixture::default();
        let registry = fixture_registry(&fixture);
        registry.trigger(None);
        wait_until_ready(&registry);
        wait_for(
            || fixture.stops.load(Ordering::SeqCst) == 1,
            "TTL to stop an unadopted stream",
        );
        assert!(registry.slot.lock().is_none());
    }

    #[test]
    fn failed_build_clears_the_slot_and_adopt_falls_back() {
        let fixture = Fixture {
            fail: true,
            ..Fixture::default()
        };
        let registry = fixture_registry(&fixture);
        registry.trigger(None);
        wait_for(|| registry.slot.lock().is_none(), "slot to be cleared");
        assert!(registry.adopt(None).is_none());
        assert_eq!(fixture.stops.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn retrigger_reuses_a_ready_stream_but_clears_its_stale_buffer() {
        let fixture = Fixture::default();
        let registry = fixture_registry(&fixture);
        registry.trigger(None);
        wait_until_ready(&registry);

        // 第一次按键窗口攒下的旧音频。
        let adopted_tap = {
            let slot = registry.slot.lock();
            match &slot.as_ref().expect("ready entry").state {
                PrearmState::Ready(prearmed) => Arc::clone(&prearmed.tap),
                _ => panic!("must be ready"),
            }
        };
        adopted_tap.consume_pcm_chunk(&[1, 0]);

        // 同设备再次按键：复用同一条流（不重复建），但缓冲必须清零。
        registry.trigger(None);
        wait_until_ready(&registry);
        assert_eq!(fixture.builds.load(Ordering::SeqCst), 1, "reuse must not rebuild");
        assert_eq!(adopted_tap.buffered_bytes(), 0, "stale buffer must be cleared");

        adopted_tap.consume_pcm_chunk(&[2, 0]);
        let adopted = registry.adopt(None).expect("must adopt the reused stream");
        let consumer = Arc::new(BufferingConsumer::default());
        let replayed = adopted
            .tap
            .attach(consumer.clone(), None, Arc::new(|_| {}));
        assert_eq!(replayed, 2);
        assert_eq!(consumer.chunks.lock().unwrap().as_slice(), &[vec![2, 0]]);
    }

    #[test]
    fn retrigger_with_a_different_microphone_rebuilds() {
        let fixture = Fixture::default();
        let registry = fixture_registry(&fixture);
        registry.trigger(Some("mic-a".into()));
        wait_until_ready(&registry);
        registry.trigger(Some("mic-b".into()));
        wait_until_ready(&registry);
        assert_eq!(fixture.builds.load(Ordering::SeqCst), 2, "mic change must rebuild");
        assert_eq!(fixture.stops.load(Ordering::SeqCst), 1, "old stream must stop");

        let adopted = registry.adopt(Some("mic-b".into())).expect("must adopt");
        assert_eq!(adopted.mic_name.as_deref(), Some("mic-b"));
    }

    #[derive(Default)]
    struct BufferingConsumer {
        chunks: StdMutex<Vec<Vec<u8>>>,
    }

    impl crate::recorder::AudioConsumer for BufferingConsumer {
        fn consume_pcm_chunk(&self, pcm: &[u8]) {
            self.chunks.lock().unwrap().push(pcm.to_vec());
        }
    }

    #[test]
    fn should_prearm_only_when_the_next_press_would_start_a_session() {        let mut snapshot = openless_core::BackendSnapshot::default();
        assert!(should_prearm(&snapshot));
        snapshot.dictation.phase = openless_core::DictationPhase::Recording;
        assert!(!should_prearm(&snapshot));
    }

    #[test]
    fn normalize_mic_name_treats_blank_as_default_device() {
        assert_eq!(normalize_mic_name(""), None);
        assert_eq!(normalize_mic_name("   "), None);
        assert_eq!(
            normalize_mic_name(" USB Mic "),
            Some("USB Mic".to_string())
        );
    }
}
