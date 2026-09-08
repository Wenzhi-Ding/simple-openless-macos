use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use crate::domains::{LocalAsrRuntimeStatus, SelectionSnapshot};
use crate::shared_types::{CredentialsStatus, HotkeyStatus, PendingCorrection};
use crate::types::{
    DictationResult, DictationStateSnapshot, HistoryChange, InsertFallbackPayload,
    NotificationPayload, PermissionSnapshot, PolishDelta, PreferencesChange, SessionId,
    StylePackChange, TranscriptDelta, VocabularyChange,
};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendEvent {
    pub sequence: u64,
    pub session_id: Option<SessionId>,
    pub kind: BackendEventKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalAsrRuntimeKind {
    Foundry,
    SherpaOnnx,
    Generic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalAsrPreparePhase {
    Runtime,
    Model,
    Load,
    Finished,
    Failed,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAsrPrepareProgress {
    pub runtime: LocalAsrRuntimeKind,
    pub phase: LocalAsrPreparePhase,
    pub model_alias: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingControlAction {
    Stop,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingControlRequest {
    pub session_id: SessionId,
    pub action: RecordingControlAction,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum BackendEventKind {
    BackendStarted,
    BackendStopping,
    DictationStateChanged(DictationStateSnapshot),
    TranscriptDelta(TranscriptDelta),
    PolishDelta(PolishDelta),
    DictationCompleted(DictationResult),
    RecordingControlRequested(RecordingControlRequest),
    SelectionStateChanged(SelectionSnapshot),
    InsertFallback(InsertFallbackPayload),
    PreferencesChanged(PreferencesChange),
    CredentialsChanged(CredentialsStatus),
    HistoryChanged(HistoryChange),
    VocabularyChanged(VocabularyChange),
    StylePacksChanged(StylePackChange),
    PermissionChanged(PermissionSnapshot),
    HotkeyStatusChanged(HotkeyStatus),
    Notification(NotificationPayload),
    LocalAsrPrepareProgress(LocalAsrPrepareProgress),
    LocalAsrEngineChanged(LocalAsrRuntimeStatus),
    MicrophoneDevicesChanged,
    VocabularySuggestionsChanged(Vec<PendingCorrection>),
}

/// Bounded, instance-local replay result used when a host mounts after events
/// were already published or needs to recover after subscription lag.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventReplay {
    pub events: Vec<BackendEvent>,
    pub oldest_sequence: Option<u64>,
    pub latest_sequence: u64,
    pub truncated: bool,
}

const EVENT_REPLAY_CAPACITY: usize = 2048;

#[derive(Debug)]
pub struct EventBus {
    sequence: AtomicU64,
    sender: broadcast::Sender<BackendEvent>,
    backlog: Mutex<VecDeque<BackendEvent>>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self {
            sequence: AtomicU64::new(0),
            sender,
            backlog: Mutex::new(VecDeque::with_capacity(EVENT_REPLAY_CAPACITY)),
        }
    }

    pub fn publish(&self, session_id: Option<SessionId>, mut kind: BackendEventKind) {
        // One existing lock linearizes sequence allocation, projection, replay
        // and live delivery. Native audio and Agent threads publish concurrently:
        // assigning a number before locking (or sending after unlocking) lets a
        // higher sequence overtake an earlier event, which UI watermarks discard.
        // No asynchronous work or Host callback belongs in this critical section.
        let mut backlog = self.backlog.lock().expect("event backlog lock poisoned");
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let event = BackendEvent {
            sequence,
            session_id,
            kind,
        };
        backlog.push_back(event.clone());
        while backlog.len() > EVENT_REPLAY_CAPACITY {
            backlog.pop_front();
        }
        let _ = self.sender.send(event);
    }

    pub fn subscribe(&self) -> EventSubscription {
        EventSubscription {
            receiver: self.sender.subscribe(),
        }
    }

    pub fn replay_after(&self, sequence: u64) -> EventReplay {
        let backlog = self.backlog.lock().expect("event backlog lock poisoned");
        let oldest_sequence = backlog.front().map(|event| event.sequence);
        // Publish holds this same lock through send, so this watermark never
        // advances past an event which has not yet entered the replay buffer.
        let latest_sequence = self.sequence.load(Ordering::Acquire);
        let truncated = oldest_sequence.is_some_and(|oldest| sequence.saturating_add(1) < oldest);
        let events = backlog
            .iter()
            .filter(|event| event.sequence > sequence)
            .cloned()
            .collect();
        EventReplay {
            events,
            oldest_sequence,
            latest_sequence,
            truncated,
        }
    }

}

/// Cloneable typed event sink for platform and transport Adapters.
///
/// Adapters publish semantic core events through this Interface instead of
/// creating a host-only event stream. The publisher shares the backend's
/// sequence counter and subscriptions, so lag detection and snapshot resync
/// work identically for core- and Adapter-originated events.
#[derive(Clone)]
pub struct BackendEventPublisher {
    bus: Arc<EventBus>,
}

impl BackendEventPublisher {
    pub(crate) fn new(bus: Arc<EventBus>) -> Self {
        Self { bus }
    }

    pub fn publish(&self, session_id: Option<SessionId>, kind: BackendEventKind) {
        self.bus.publish(session_id, kind);
    }

    pub fn replay_after(&self, sequence: u64) -> EventReplay {
        self.bus.replay_after(sequence)
    }

}

pub struct EventSubscription {
    receiver: broadcast::Receiver<BackendEvent>,
}

impl EventSubscription {
    pub async fn recv(&mut self) -> Result<BackendEvent, EventRecvError> {
        self.receiver.recv().await.map_err(EventRecvError::from)
    }

    /// Drain one event without ever waiting on the UI thread.
    ///
    /// A frame should call this repeatedly until [`EventRecvError::Empty`],
    /// then request a repaint when at least one event was received.  A lagged
    /// receiver is deliberately surfaced so the caller can resynchronise from
    /// [`OpenLessBackend::snapshot`](crate::OpenLessBackend::snapshot).
    pub fn try_recv(&mut self) -> Result<BackendEvent, EventRecvError> {
        self.receiver.try_recv().map_err(EventRecvError::from)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EventRecvError {
    #[error("event subscription has no pending event")]
    Empty,
    #[error("event subscription lagged by {0} event(s)")]
    Lagged(u64),
    #[error("event bus closed")]
    Closed,
}

impl From<broadcast::error::RecvError> for EventRecvError {
    fn from(error: broadcast::error::RecvError) -> Self {
        match error {
            broadcast::error::RecvError::Lagged(count) => Self::Lagged(count),
            broadcast::error::RecvError::Closed => Self::Closed,
        }
    }
}

impl From<broadcast::error::TryRecvError> for EventRecvError {
    fn from(error: broadcast::error::TryRecvError) -> Self {
        match error {
            broadcast::error::TryRecvError::Empty => Self::Empty,
            broadcast::error::TryRecvError::Lagged(count) => Self::Lagged(count),
            broadcast::error::TryRecvError::Closed => Self::Closed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::DictationPhase;

    #[tokio::test]
    async fn sequence_is_monotonic_and_lag_is_explicit() {
        let bus = EventBus::new(1);
        let mut subscription = bus.subscribe();
        bus.publish(None, BackendEventKind::BackendStarted);
        bus.publish(
            None,
            BackendEventKind::DictationStateChanged(DictationStateSnapshot {
                phase: DictationPhase::Idle,
                ..DictationStateSnapshot::default()
            }),
        );

        assert_eq!(subscription.recv().await, Err(EventRecvError::Lagged(1)));
        let event = subscription.recv().await;
        assert_eq!(event.unwrap().sequence, 2);
    }

    #[test]
    fn native_publishers_keep_live_and_replay_sequences_in_the_same_order() {
        const THREADS: usize = 8;
        const PER_THREAD: usize = 1000;
        let bus = EventBus::new(THREADS * PER_THREAD);
        let mut subscription = bus.subscribe();
        let barrier = std::sync::Barrier::new(THREADS);
        // Native audio, Agent output and Core transitions are independent OS
        // threads. Capacity covers every message, so a failure here is order
        // corruption, not the documented bounded-subscription lag behavior.
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    barrier.wait();
                    for _ in 0..PER_THREAD {
                        bus.publish(None, BackendEventKind::BackendStarted);
                    }
                });
            }
        });
        for expected in 1..=(THREADS * PER_THREAD) as u64 {
            assert_eq!(subscription.try_recv().unwrap().sequence, expected);
        }
        assert_eq!(subscription.try_recv(), Err(EventRecvError::Empty));
        let replay = bus.replay_after(0);
        assert!(replay.truncated);
        assert_eq!(replay.events.len(), EVENT_REPLAY_CAPACITY);
        assert_eq!(replay.latest_sequence, (THREADS * PER_THREAD) as u64);
        let first = replay.latest_sequence - EVENT_REPLAY_CAPACITY as u64 + 1;
        for (expected, event) in (first..=replay.latest_sequence).zip(replay.events) {
            assert_eq!(event.sequence, expected);
        }
    }

    #[test]
    fn try_recv_is_non_blocking_and_reports_empty() {
        let bus = EventBus::new(2);
        let mut subscription = bus.subscribe();
        assert_eq!(subscription.try_recv(), Err(EventRecvError::Empty));
        bus.publish(None, BackendEventKind::BackendStarted);
        assert_eq!(subscription.try_recv().unwrap().sequence, 1);
        assert_eq!(subscription.try_recv(), Err(EventRecvError::Empty));
    }

    #[test]
    fn event_serialization_is_tagged_and_does_not_add_secret_fields() {
        let event = BackendEvent {
            sequence: 7,
            session_id: None,
            kind: BackendEventKind::CredentialsChanged(CredentialsStatus {
                active_asr_provider: "fixture-asr".to_string(),
                active_llm_provider: "fixture-llm".to_string(),
                asr_configured: true,
                llm_configured: true,
                ..CredentialsStatus::default()
            }),
        };
        let json = serde_json::to_string(&event).expect("event should serialize");
        assert!(json.contains("credentials_changed"));
        assert!(json.contains("activeAsrProvider"));
        assert!(!json.contains("token"));
        assert!(!json.contains("authorization"));
    }

    #[test]
    fn adapter_publisher_shares_sequence_and_subscription_with_backend_events() {
        let bus = Arc::new(EventBus::new(4));
        let publisher = BackendEventPublisher::new(Arc::clone(&bus));
        let mut subscription = bus.subscribe();

        bus.publish(None, BackendEventKind::BackendStarted);
        publisher.publish(
            None,
            BackendEventKind::Notification(NotificationPayload {
                level: crate::types::NotificationLevel::Info,
                message: "adapter-ready".to_string(),
            }),
        );

        assert_eq!(subscription.try_recv().unwrap().sequence, 1);
        let adapter_event = subscription.try_recv().unwrap();
        assert_eq!(adapter_event.sequence, 2);
        assert!(matches!(
            adapter_event.kind,
            BackendEventKind::Notification(NotificationPayload { ref message, .. })
                if message == "adapter-ready"
        ));
    }

    #[test]
    fn replay_is_instance_local_bounded_and_reports_truncation() {
        let bus = EventBus::new(2);
        for _ in 0..(EVENT_REPLAY_CAPACITY + 2) {
            bus.publish(None, BackendEventKind::BackendStarted);
        }

        let replay = bus.replay_after(0);
        assert_eq!(replay.events.len(), EVENT_REPLAY_CAPACITY);
        assert_eq!(replay.oldest_sequence, Some(3));
        assert_eq!(replay.latest_sequence, (EVENT_REPLAY_CAPACITY + 2) as u64);
        assert!(replay.truncated);

        let other = EventBus::new(2);
        assert!(other.replay_after(0).events.is_empty());
    }



}