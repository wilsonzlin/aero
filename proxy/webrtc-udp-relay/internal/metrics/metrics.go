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

func (m *Metrics) Get(name string) uint64 {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.m[name]
}
