use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, TryIter, bounded, unbounded};
use cursive::{CbSink, Cursive};

use crate::queue::QueueEvent;
use crate::spotify::PlayerEvent;

/// Events handled by either the terminal application or the headless engine.
pub enum Event {
    Player(PlayerEvent),
    Queue(QueueEvent),
    SessionDied,
    IpcInput(String),
    OpenPrototype,
    Shutdown,
}

/// A runtime notice retained long enough for a frontend to observe it.
#[derive(Clone, Debug, Serialize)]
pub struct Notification {
    pub id: u64,
    pub message: String,
    pub created_at_ms: u64,
}

#[derive(Clone)]
pub struct EventManager {
    tx: Sender<Event>,
    rx: Receiver<Event>,
    cursive_sink: Arc<RwLock<Option<CbSink>>>,
    wake_tx: Sender<()>,
    wake_rx: Receiver<()>,
    closed: Arc<AtomicBool>,
    notices: Arc<RwLock<VecDeque<Notification>>>,
    notice_sequence: Arc<AtomicU64>,
}

impl EventManager {
    #[cfg(test)]
    pub fn new_for_test() -> Self {
        Self::new()
    }

    pub fn new() -> Self {
        let (tx, rx) = unbounded();
        // Wakeups coalesce: one pending wake is enough to drain the event queue.
        let (wake_tx, wake_rx) = bounded(1);
        Self {
            tx,
            rx,
            cursive_sink: Arc::new(RwLock::new(None)),
            wake_tx,
            wake_rx,
            closed: Arc::new(AtomicBool::new(false)),
            notices: Arc::new(RwLock::new(VecDeque::new())),
            notice_sequence: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn attach_cursive(&self, cursive_sink: CbSink) {
        *self.cursive_sink.write().unwrap() = Some(cursive_sink);
    }

    pub fn has_cursive(&self) -> bool {
        self.cursive_sink.read().unwrap().is_some()
    }

    pub fn msg_iter(&self) -> TryIter<'_, Event> {
        self.rx.try_iter()
    }

    pub fn send(&self, event: Event) {
        if !self.closed.load(Ordering::Acquire) && self.tx.send(event).is_ok() {
            self.trigger();
        }
    }

    /// Wait independently of Cursive, with a timeout for periodic playback maintenance.
    pub fn wait_timeout(&self, timeout: Duration) {
        let _ = self.wake_rx.recv_timeout(timeout);
    }

    pub fn trigger(&self) {
        self.try_trigger();
    }

    pub fn try_trigger(&self) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        let _ = self.wake_tx.try_send(());
        match self.cursive_sink.read().unwrap().as_ref() {
            Some(sink) => sink.send(Box::new(Cursive::noop)).is_ok(),
            None => true,
        }
    }

    pub fn notify(&self, message: impl Into<String>) {
        let notification = Notification {
            id: self.notice_sequence.fetch_add(1, Ordering::Relaxed) + 1,
            message: message.into(),
            created_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
        };
        log::info!("notice: {}", notification.message);
        let mut notices = self.notices.write().unwrap();
        notices.push_back(notification);
        while notices.len() > 64 {
            notices.pop_front();
        }
        drop(notices);
        self.trigger();
    }

    pub fn notifications(&self) -> Vec<Notification> {
        self.notices.read().unwrap().iter().cloned().collect()
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let _ = self.wake_tx.try_send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_events_wake_and_preserve_messages_without_a_terminal() {
        let events = EventManager::new();
        events.send(Event::IpcInput("next".into()));
        events.wait_timeout(Duration::from_millis(1));
        assert!(
            matches!(events.msg_iter().next(), Some(Event::IpcInput(input)) if input == "next")
        );
        assert!(events.try_trigger());
        events.close();
        assert!(!events.try_trigger());
    }

    #[test]
    fn headless_notices_are_ordered_and_bounded() {
        let events = EventManager::new();
        for index in 0..70 {
            events.notify(format!("notice {index}"));
        }
        let notices = events.notifications();
        assert_eq!(notices.len(), 64);
        assert_eq!(notices[0].id, 7);
        assert_eq!(notices[63].message, "notice 69");
    }
}
