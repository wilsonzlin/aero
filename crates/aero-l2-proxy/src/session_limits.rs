use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Debug)]
pub(crate) struct SessionTunnelTracker {
    max_per_session: usize,
    active: Mutex<HashMap<String, usize>>,
}

impl SessionTunnelTracker {
    pub(crate) fn new(max_per_session: usize) -> Self {
        assert!(max_per_session > 0);
        Self {
            max_per_session,
            active: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn try_acquire(self: &Arc<Self>, session_id: &str) -> Option<SessionTunnelPermit> {
        let mut active = self.active.lock().expect("session tunnel mutex poisoned");
        let current = active.get(session_id).copied().unwrap_or(0);
        if current >= self.max_per_session {
            return None;
        }
        active.insert(session_id.to_string(), current + 1);
        Some(SessionTunnelPermit {
            tracker: Arc::clone(self),
            session_id: session_id.to_string(),
        })
    }

    fn release(&self, session_id: &str) {
        let mut active = self.active.lock().expect("session tunnel mutex poisoned");
        let current = active.get(session_id).copied().unwrap_or(0);
        let next = current.saturating_sub(1);
        if next == 0 {
            active.remove(session_id);
        } else {
            active.insert(session_id.to_string(), next);
        }
    }
}

#[derive(Debug)]
pub(crate) struct SessionTunnelPermit {
    tracker: Arc<SessionTunnelTracker>,
    session_id: String,
}

impl Drop for SessionTunnelPermit {
    fn drop(&mut self) {
        self.tracker.release(&self.session_id);
    }
}
