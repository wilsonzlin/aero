use std::collections::VecDeque;

// `std::time::Instant` panics at runtime on wasm32-unknown-unknown. Use `web_time::Instant` so the
// net stack can safely run in browser/Node environments.
#[cfg(not(target_arch = "wasm32"))]
use std::time::Instant;
#[cfg(target_arch = "wasm32")]
use web_time::Instant;

use aero_net_backend::NetworkBackend;

use crate::{Action, DnsResolved, Millis, NetworkStack, StackConfig, TcpProxyEvent, UdpProxyEvent};

/// Queue/memory bounds for [`NetStackBackend`].
///
/// These limits cap the amount of work buffered in the backend when the host integration is slow
/// or misbehaving (e.g. not draining actions or not polling guest RX frames).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetStackBackendLimits {
    /// Maximum number of pending guest-facing Ethernet frames queued by the stack.
    ///
    /// When exceeded, new frames are dropped (oldest frames are retained).
    pub max_pending_frames: usize,
    /// Maximum number of pending host actions queued by the stack.
    ///
    /// When exceeded, new actions are dropped (oldest actions are retained).
    pub max_pending_actions: usize,
    /// Maximum number of payload bytes buffered across pending host actions.
    ///
    /// Only payload-carrying actions contribute to this budget:
    /// - [`Action::TcpProxySend`]
    /// - [`Action::UdpProxySend`]
    ///
    /// When exceeded, new payload-carrying actions are dropped (oldest actions are retained).
    pub max_pending_action_bytes: usize,
}

impl Default for NetStackBackendLimits {
    fn default() -> Self {
        // Conservative but non-tiny defaults: they should be large enough for typical bursts while
        // still preventing unbounded growth if the host stops draining queues.
        Self {
            max_pending_frames: 4096,
            max_pending_actions: 4096,
            max_pending_action_bytes: 16 * 1024 * 1024, // 16 MiB
        }
    }
}

/// Snapshot statistics for a [`NetStackBackend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NetStackBackendStats {
    pub pending_frames: usize,
    pub pending_actions: usize,
    pub pending_action_bytes: usize,

    pub dropped_frames: u64,
    pub dropped_actions: u64,
    pub dropped_action_bytes: u64,
}

/// NIC-facing backend for [`crate::NetworkStack`].
///
/// The backend implements [`NetworkBackend`] so emulated NICs (E1000, virtio-net) can transmit
/// guest Ethernet frames into the stack. Any outbound stack actions are queued:
/// - [`Action::EmitFrame`] frames are queued for delivery back into the guest NIC (via
///   [`NetworkBackend::poll_receive`] or [`NetStackBackend::drain_frames`]).
/// - All other [`Action`] variants are queued as host actions (via
///   [`NetStackBackend::drain_actions`]).
pub struct NetStackBackend {
    stack: NetworkStack,
    start: Instant,
    pending_frames: VecDeque<Vec<u8>>,
    pending_actions: VecDeque<Action>,
    pending_action_bytes: usize,
    limits: NetStackBackendLimits,
    dropped_frames: u64,
    dropped_actions: u64,
    dropped_action_bytes: u64,
}

impl NetStackBackend {
    pub fn new(cfg: StackConfig) -> Self {
        Self::with_limits(cfg, NetStackBackendLimits::default())
    }

    pub fn with_limits(cfg: StackConfig, limits: NetStackBackendLimits) -> Self {
        Self::from_stack_with_limits(NetworkStack::new(cfg), limits)
    }

    pub fn from_stack(stack: NetworkStack) -> Self {
        Self::from_stack_with_limits(stack, NetStackBackendLimits::default())
    }

    pub fn from_stack_with_limits(stack: NetworkStack, limits: NetStackBackendLimits) -> Self {
        Self {
            stack,
            start: Instant::now(),
            pending_frames: VecDeque::new(),
            pending_actions: VecDeque::new(),
            pending_action_bytes: 0,
            limits,
            dropped_frames: 0,
            dropped_actions: 0,
            dropped_action_bytes: 0,
        }
    }

    pub fn stack(&self) -> &NetworkStack {
        &self.stack
    }

    pub fn stack_mut(&mut self) -> &mut NetworkStack {
        &mut self.stack
    }

    pub fn stats(&self) -> NetStackBackendStats {
        NetStackBackendStats {
            pending_frames: self.pending_frames.len(),
            pending_actions: self.pending_actions.len(),
            pending_action_bytes: self.pending_action_bytes,
            dropped_frames: self.dropped_frames,
            dropped_actions: self.dropped_actions,
            dropped_action_bytes: self.dropped_action_bytes,
        }
    }

    /// Monotonic timestamp in milliseconds since this backend was created.
    ///
    /// The stack uses `now_ms` for bookkeeping like DNS cache TTLs. Host glue that wants consistent
    /// timing between [`NetworkBackend::transmit`] and [`NetStackBackend::push_*`] calls can use
    /// this value as its time base.
    pub fn now_ms(&self) -> Millis {
        self.start.elapsed().as_millis().min(u64::MAX as u128) as u64
    }

    /// Process an outbound (guest → host) Ethernet frame using an explicit `now_ms`.
    ///
    /// This is primarily useful for deterministic tests and for hosts that want to drive the stack
    /// with a virtual clock.
    pub fn transmit_at(&mut self, frame: Vec<u8>, now_ms: Millis) {
        let actions = self.stack.process_outbound_ethernet(&frame, now_ms);
        self.push_actions(actions);
    }

    pub fn push_tcp_event(&mut self, event: TcpProxyEvent, now_ms: Millis) {
        let actions = self.stack.handle_tcp_proxy_event(event, now_ms);
        self.push_actions(actions);
    }

    pub fn push_udp_event(&mut self, event: UdpProxyEvent, now_ms: Millis) {
        let actions = self.stack.handle_udp_proxy_event(event, now_ms);
        self.push_actions(actions);
    }

    pub fn push_dns_resolved(&mut self, resolved: DnsResolved, now_ms: Millis) {
        let actions = self.stack.handle_dns_resolved(resolved, now_ms);
        self.push_actions(actions);
    }

    pub fn drain_actions(&mut self) -> Vec<Action> {
        let drained: Vec<Action> = self.pending_actions.drain(..).collect();
        let mut bytes = 0usize;
        for action in &drained {
            bytes = bytes.saturating_add(action_payload_len(action));
        }
        self.pending_action_bytes = self.pending_action_bytes.saturating_sub(bytes);
        drained
    }

    pub fn drain_frames(&mut self) -> Vec<Vec<u8>> {
        self.pending_frames.drain(..).collect()
    }

    fn push_actions(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::EmitFrame(frame) => self.push_frame(frame),
                other => self.push_host_action(other),
            }
        }
    }

    fn push_frame(&mut self, frame: Vec<u8>) {
        let max = self.limits.max_pending_frames;
        if max == 0 || self.pending_frames.len() >= max {
            self.dropped_frames = self.dropped_frames.saturating_add(1);
            return;
        }
        self.pending_frames.push_back(frame);
    }

    fn push_host_action(&mut self, action: Action) {
        let max = self.limits.max_pending_actions;
        let payload_len = action_payload_len(&action);

        if max == 0 || self.pending_actions.len() >= max {
            self.dropped_actions = self.dropped_actions.saturating_add(1);
            self.dropped_action_bytes =
                self.dropped_action_bytes.saturating_add(payload_len as u64);
            return;
        }

        if payload_len > 0 {
            let max_bytes = self.limits.max_pending_action_bytes;
            // Treat `max_pending_action_bytes == 0` as "no payload buffering allowed".
            let new_total = self.pending_action_bytes.saturating_add(payload_len);
            if max_bytes == 0 || new_total > max_bytes {
                self.dropped_actions = self.dropped_actions.saturating_add(1);
                self.dropped_action_bytes =
                    self.dropped_action_bytes.saturating_add(payload_len as u64);
                return;
            }
            self.pending_action_bytes = new_total;
        }

        self.pending_actions.push_back(action);
    }
}

impl NetworkBackend for NetStackBackend {
    fn transmit(&mut self, frame: Vec<u8>) {
        let now_ms = self.now_ms();
        self.transmit_at(frame, now_ms);
    }

    fn poll_receive(&mut self) -> Option<Vec<u8>> {
        self.pending_frames.pop_front()
    }
}

fn action_payload_len(action: &Action) -> usize {
    match action {
        Action::TcpProxySend { data, .. } => data.len(),
        Action::UdpProxySend { data, .. } => data.len(),
        _ => 0,
    }
}
