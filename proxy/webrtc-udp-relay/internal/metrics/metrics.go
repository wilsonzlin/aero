package metrics

import "sync"

// Drop reasons. Names are intentionally simple; a follow-up metrics task can
// standardize and export these via Prometheus/OTel.
const (
	DropReasonRateLimited     = "rate_limited"
	DropReasonQuotaExceeded   = "quota_exceeded"
	DropReasonTooManySessions = "too_many_sessions"

	// Auth failures across HTTP and WebSocket signaling endpoints.
	AuthFailure = "auth_failure"

	// SessionHardClosed counts relay sessions that were hard-closed due to repeated
	// rate/quota violations.
	SessionHardClosed = "session_hard_closed"

	// WebRTCSessionConnectTimeout counts server-side PeerConnections that were
	// closed because they failed to reach a connected state within the configured
	// connect timeout.
	WebRTCSessionConnectTimeout = "webrtc_session_connect_timeout"

	// ICEGatheringTimeout counts responses served by non-trickle HTTP signaling
	// endpoints (/offer, /webrtc/offer) where the relay returned an SDP answer
	// before ICE gathering completed because the configured timeout was hit.
	ICEGatheringTimeout = "ice_gathering_timeout"
)

// WebSocket UDP relay (/udp) counters.
const (
	UDPWSConnections           = "udp_ws_connections"
	UDPWSDatagramsIn           = "udp_ws_datagrams_in"
	UDPWSDatagramsOut          = "udp_ws_datagrams_out"
	UDPWSDropped               = "udp_ws_dropped"
	UDPWSDroppedBackpressure   = "udp_ws_dropped_backpressure"
	UDPWSDroppedMalformed      = "udp_ws_dropped_malformed"
	UDPWSDroppedOversized      = "udp_ws_dropped_oversized"
	UDPWSDroppedRateLimited    = "udp_ws_dropped_rate_limited"
	UDPWSDroppedQuotaExceeded  = "udp_ws_dropped_quota_exceeded"
	UDPWSDroppedDeniedByPolicy = "udp_ws_dropped_denied_by_policy"
)

// WebRTC UDP relay (DataChannel label "udp") counters.
//
// These mirror the /udp WebSocket fallback counters but allow operators to
// distinguish traffic and drop reasons on the primary WebRTC transport.
const (
	WebRTCUDPDatagramsIn            = "webrtc_udp_datagrams_in"
	WebRTCUDPDatagramsOut           = "webrtc_udp_datagrams_out"
	WebRTCUDPDropped                = "webrtc_udp_dropped"
	WebRTCUDPDroppedBackpressure    = "webrtc_udp_dropped_backpressure"
	WebRTCUDPDroppedMalformed       = "webrtc_udp_dropped_malformed"
	WebRTCUDPDroppedOversized       = "webrtc_udp_dropped_oversized"
	WebRTCUDPDroppedRateLimited     = "webrtc_udp_dropped_rate_limited"
	WebRTCUDPDroppedQuotaExceeded   = "webrtc_udp_dropped_quota_exceeded"
	WebRTCUDPDroppedDeniedByPolicy  = "webrtc_udp_dropped_denied_by_policy"
	WebRTCUDPDroppedTooManyBindings = "webrtc_udp_dropped_too_many_bindings"
)

// WebRTC DataChannel rejection counters.
const (
	WebRTCDataChannelRejectedUnknownLabel = "webrtc_datachannel_rejected_unknown_label"
	WebRTCDataChannelRejectedDuplicateUDP = "webrtc_datachannel_rejected_duplicate_udp"
	WebRTCDataChannelRejectedDuplicateL2  = "webrtc_datachannel_rejected_duplicate_l2"
)

// WebRTC DataChannel oversized message counters.
//
// These count messages that exceed the negotiated max message size (SDP
// `a=max-message-size` / pion SettingEngine max message size) and cause the relay
// to close the entire session (defense in depth against malicious peers).
const (
	WebRTCDataChannelMessageTooLargeUDP = "webrtc_datachannel_udp_message_too_large"
	WebRTCDataChannelMessageTooLargeL2  = "webrtc_datachannel_l2_message_too_large"
)

// L2 tunnel bridge (WebRTC DataChannel "l2" <-> backend WS) counters.
const (
	L2BridgeDialsTotal              = "l2_bridge_dials_total"
	L2BridgeDialErrorsTotal         = "l2_bridge_dial_errors_total"
	L2BridgeMessagesFromClientTotal = "l2_bridge_messages_from_client_total"
	L2BridgeMessagesToClientTotal   = "l2_bridge_messages_to_client_total"
	L2BridgeBytesFromClientTotal    = "l2_bridge_bytes_from_client_total"
	L2BridgeBytesToClientTotal      = "l2_bridge_bytes_to_client_total"
	L2BridgeDroppedOversizedTotal   = "l2_bridge_dropped_oversized_total"
	L2BridgeDroppedRateLimitedTotal = "l2_bridge_dropped_rate_limited_total"
)

// UDP binding allowlist counters.
const (
	// UDPRemoteAllowlistEvictionsTotal counts allowlist entries evicted due to the
	// MAX_ALLOWED_REMOTES_PER_BINDING cap.
	UDPRemoteAllowlistEvictionsTotal = "udp_remote_allowlist_evictions_total"
	// UDPRemoteAllowlistOverflowDropsTotal counts inbound UDP packets dropped due
	// to inbound filtering (i.e. the sender was not currently on the allowlist,
	// typically because it was evicted or expired).
	UDPRemoteAllowlistOverflowDropsTotal = "udp_remote_allowlist_overflow_drops_total"
)

// UDP relay limiter internal counters.
const (
	UDPPerDestBucketEvictions = "udp_per_dest_bucket_evictions"
)

// Metrics is a minimal, concurrency-safe counter registry.
//
// The production relay is expected to plug into a real metrics backend; this
// type exists to keep enforcement logic testable and to provide drop counters
// as required by the task.
type Metrics struct {
	mu sync.Mutex
	m  map[string]uint64
}

func New() *Metrics {
	return &Metrics{
		m: make(map[string]uint64),
	}
}

func (m *Metrics) Inc(name string) {
	m.mu.Lock()
	m.m[name]++
	m.mu.Unlock()
}

func (m *Metrics) Add(name string, delta uint64) {
	if delta == 0 {
		return
	}
	m.mu.Lock()
	m.m[name] += delta
	m.mu.Unlock()
}

func (m *Metrics) Get(name string) uint64 {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.m[name]
}

// Snapshot returns a copy of all counters.
func (m *Metrics) Snapshot() map[string]uint64 {
	m.mu.Lock()
	defer m.mu.Unlock()
	cp := make(map[string]uint64, len(m.m))
	for k, v := range m.m {
		cp[k] = v
	}
	return cp
}
