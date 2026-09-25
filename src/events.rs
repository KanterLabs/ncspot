use std::sync::{Arc, RwLock};

use crossbeam_channel::{Receiver, Sender, TryIter, unbounded};
use cursive::{CbSink, Cursive};

use crate::queue::QueueEvent;
use crate::spotify::PlayerEvent;

/// Events that can be sent to and handled by the main event loop (the one drawing the TUI).
pub enum Event {
    Player(PlayerEvent),
    Queue(QueueEvent),
    SessionDied,
    IpcInput(String),
}

/// Manager that can be used to send and receive messages across threads.
#[derive(Clone)]
pub struct EventManager {
    tx: Sender<Event>,
    rx: Receiver<Event>,
    /// The sink into the Cursive event loop, absent until the TUI has been created. The manager
    /// is handed out to background workers before that happens, so that connecting to Spotify can
    /// overlap with bringing the interface up; events sent in the meantime wait in the channel and
    /// are drained by the first pass of the event loop.
    cursive_sink: Arc<RwLock<Option<CbSink>>>,
}

impl EventManager {
    /// Create an EventManager backed by a discarded crossbeam channel, for use in tests where no
    /// events need to be processed.
    #[cfg(test)]
    pub fn new_for_test() -> Self {
        let (cb_sink, _): (CbSink, _) = crossbeam_channel::unbounded();
        let manager = Self::new();
        manager.attach_cursive(cb_sink);
        manager
    }

    pub fn new() -> Self {
        let (tx, rx) = unbounded();

        Self {
            tx,
            rx,
            cursive_sink: Arc::new(RwLock::new(None)),
        }
    }

    /// Attach the Cursive event loop, so that events start waking it up.
    pub fn attach_cursive(&self, cursive_sink: CbSink) {
        *self.cursive_sink.write().unwrap() = Some(cursive_sink);
    }

    /// Return a non-blocking iterator over the messages awaiting handling. Calling `next()` on the
    /// iterator never blocks.
    pub fn msg_iter(&self) -> TryIter<'_, Event> {
        self.rx.try_iter()
    }

    /// Send a new event to be handled.
    pub fn send(&self, event: Event) {
        self.tx.send(event).unwrap();
        self.trigger();
    }

    /// Send a no-op to the Cursive event loop to trigger immediate processing of events.
    pub fn trigger(&self) {
        if let Some(sink) = self.cursive_sink.read().unwrap().as_ref() {
            sink.send(Box::new(Cursive::noop)).unwrap();
        }
    }

    /// Like [`Self::trigger`], but report a closed event loop instead of panicking.
    /// Background animation threads use this to notice that the UI is shutting down. An event
    /// loop that hasn't been attached yet counts as live: it is still on its way up.
    pub fn try_trigger(&self) -> bool {
        match self.cursive_sink.read().unwrap().as_ref() {
            Some(sink) => sink.send(Box::new(Cursive::noop)).is_ok(),
            None => true,
        }
    }
}
