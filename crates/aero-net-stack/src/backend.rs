use std::collections::VecDeque;
use std::time::Instant;

use aero_net_backend::NetworkBackend;

use crate::{Action, DnsResolved, Millis, NetworkStack, StackConfig, TcpProxyEvent, UdpProxyEvent};

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
}

impl NetStackBackend {
    pub fn new(cfg: StackConfig) -> Self {
        Self::from_stack(NetworkStack::new(cfg))
    }

    pub fn from_stack(stack: NetworkStack) -> Self {
        Self {
            stack,
            start: Instant::now(),
            pending_frames: VecDeque::new(),
            pending_actions: VecDeque::new(),
        }
    }

    pub fn stack(&self) -> &NetworkStack {
        &self.stack
    }

    pub fn stack_mut(&mut self) -> &mut NetworkStack {
        &mut self.stack
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
        self.pending_actions.drain(..).collect()
    }

    pub fn drain_frames(&mut self) -> Vec<Vec<u8>> {
        self.pending_frames.drain(..).collect()
    }

    fn push_actions(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::EmitFrame(frame) => self.pending_frames.push_back(frame),
                other => self.pending_actions.push_back(other),
            }
        }
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
