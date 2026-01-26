use std::sync::Arc;
use std::sync::Mutex;

use tokio::sync::mpsc::UnboundedSender;

use crate::app_event::AppEvent;
use crate::session_log;
use codex_protocol::ThreadId;

#[derive(Clone, Debug)]
pub(crate) struct AppEventSender {
    pub app_event_tx: UnboundedSender<AppEvent>,
    thread_id: Arc<Mutex<Option<ThreadId>>>,
}

impl AppEventSender {
    pub(crate) fn new(app_event_tx: UnboundedSender<AppEvent>) -> Self {
        Self {
            app_event_tx,
            thread_id: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn scoped(&self) -> Self {
        Self {
            app_event_tx: self.app_event_tx.clone(),
            thread_id: Arc::new(Mutex::new(None)),
        }
    }

    pub(crate) fn set_thread_id(&self, thread_id: ThreadId) {
        let mut guard = match self.thread_id.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard = Some(thread_id);
    }

    /// Send an event to the app event channel. If it fails, we swallow the
    /// error and log it.
    pub(crate) fn send(&self, event: AppEvent) {
        let event = match event {
            AppEvent::InsertHistoryCell(cell) => AppEvent::InsertHistoryCellForThread {
                thread_id: self.thread_id(),
                cell,
            },
            other => other,
        };
        // Record inbound events for high-fidelity session replay.
        // Avoid double-logging Ops; those are logged at the point of submission.
        if !matches!(event, AppEvent::CodexOp(_)) {
            session_log::log_inbound_app_event(&event);
        }
        if let Err(e) = self.app_event_tx.send(event) {
            tracing::error!("failed to send event: {e}");
        }
    }

    fn thread_id(&self) -> Option<ThreadId> {
        let guard = match self.thread_id.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        *guard
    }
}
