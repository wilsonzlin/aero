package relay

import (
	"sync"
	"time"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/ratelimit"
)

type Session struct {
	id      string
	cfg     config.Config
	metrics *metrics.Metrics
	clock   ratelimit.Clock

	limiter *ratelimit.SessionLimiter

	mu     sync.Mutex
	closed bool
	done   chan struct{}

	lastViolation time.Time
	violations    int

	onClose func()

	// onHardClose is invoked when the session is hard-closed due to repeated
	// rate/quota violations (as configured by HARD_CLOSE_AFTER_VIOLATIONS).
	onHardClose func()
}

func newSession(id string, cfg config.Config, m *metrics.Metrics, clock ratelimit.Clock, onClose func()) *Session {
	var onEvict func()
	if m != nil {
		onEvict = func() {
			m.Inc(metrics.UDPPerDestBucketEvictions)
		}
	}
	rl := ratelimit.NewSessionLimiter(clock, ratelimit.SessionConfig{
		UDPPacketsPerSecond:        cfg.MaxUDPPpsPerSession,
		UDPBytesPerSecond:          cfg.MaxUDPBpsPerSession,
		DataChannelBytesPerSecond:  cfg.MaxDataChannelBpsPerSession,
		UDPPacketsPerSecondPerDest: cfg.MaxUDPPpsPerDest,
		MaxUniqueDestinations:      cfg.MaxUniqueDestinationsPerSession,
		MaxUDPDestBuckets:          cfg.MaxUDPDestBucketsPerSession,
		OnUDPDestBucketEvicted:     onEvict,
	})

	return &Session{
		id:      id,
		cfg:     cfg,
		metrics: m,
		clock:   clock,
		limiter: rl,
		done:    make(chan struct{}),
		onClose: onClose,
	}
}

func (s *Session) ID() string { return s.id }

// Metrics returns the shared in-process metrics registry used by this session.
//
// Callers should treat the returned pointer as read-only aside from invoking
// concurrency-safe counter methods (Inc/Add/Get/Snapshot).
func (s *Session) Metrics() *metrics.Metrics { return s.metrics }

// Done is closed when the session is closed (either explicitly or due to hard
// enforcement mode).
func (s *Session) Done() <-chan struct{} {
	s.mu.Lock()
	ch := s.done
	s.mu.Unlock()
	return ch
}

func (s *Session) Closed() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.closed
}

// AddOnClose registers an additional callback to run when the session closes.
//
// It is safe to call multiple times. If the session is already closed, fn is
// invoked synchronously.
func (s *Session) AddOnClose(fn func()) {
	if fn == nil {
		return
	}

	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		fn()
		return
	}

	prev := s.onClose
	s.onClose = func() {
		if prev != nil {
			prev()
		}
		fn()
	}
	s.mu.Unlock()
}

func (s *Session) Close() {
	s.mu.Lock()
	onClose := s.closeLocked()
	s.mu.Unlock()

	if onClose != nil {
		onClose()
	}
}

// OnHardClose registers fn to run when the session is hard-closed due to
// repeated violations.
//
// It is safe to call multiple times; callbacks are chained in registration
// order.
func (s *Session) OnHardClose(fn func()) {
	if fn == nil {
		return
	}

	s.mu.Lock()
	defer s.mu.Unlock()

	if s.closed {
		return
	}

	prev := s.onHardClose
	s.onHardClose = func() {
		if prev != nil {
			prev()
		}
		fn()
	}
}

func (s *Session) closeLocked() func() {
	if s.closed {
		return nil
	}
	s.closed = true
	close(s.done)
	onClose := s.onClose
	s.onClose = nil
	return onClose
}

// AllowClientDatagramWithReason is like AllowClientDatagram but returns the
// limiter's drop reason for callers that want to surface more granular metrics.
func (s *Session) AllowClientDatagramWithReason(destKey string, payload []byte) (bool, ratelimit.DropReason) {
	if s.Closed() {
		return false, ""
	}

	allowed, reason := s.limiter.AllowUDPSend(destKey, len(payload))
	if allowed {
		return true, ""
	}

	switch reason {
	case ratelimit.DropReasonTooManyDestinations:
		s.metrics.Inc(metrics.DropReasonQuotaExceeded)
		s.metrics.Inc("too_many_destinations")
	default:
		s.metrics.Inc(metrics.DropReasonRateLimited)
	}

	s.recordViolation()
	return false, reason
}

// HandleInboundToClient enforces the DataChannel (relay -> client) bytes/sec
// limit before enqueueing the frame for send.
func (s *Session) HandleInboundToClient(payload []byte) bool {
	if s.Closed() {
		return false
	}

	allowed, _ := s.limiter.AllowDataChannelSend(len(payload))
	if allowed {
		return true
	}

	s.metrics.Inc(metrics.DropReasonRateLimited)
	s.recordViolation()
	return false
}

func (s *Session) recordViolation() {
	s.mu.Lock()
	onHardClose, onClose := s.recordViolationLocked(s.now())
	s.mu.Unlock()
	if onHardClose != nil {
		onHardClose()
	}
	if onClose != nil {
		onClose()
	}
}

func (s *Session) now() time.Time {
	if s.clock != nil {
		return s.clock.Now()
	}
	return time.Now()
}

func (s *Session) recordViolationLocked(now time.Time) (func(), func()) {
	if s.cfg.HardCloseAfterViolations <= 0 || s.closed {
		return nil, nil
	}

	if !s.lastViolation.IsZero() && s.cfg.ViolationWindow > 0 && now.Sub(s.lastViolation) > s.cfg.ViolationWindow {
		s.violations = 0
	}

	s.lastViolation = now
	s.violations++
	if s.violations >= s.cfg.HardCloseAfterViolations {
		s.metrics.Inc(metrics.SessionHardClosed)
		onHardClose := s.onHardClose
		s.onHardClose = nil
		return onHardClose, s.closeLocked()
	}

	return nil, nil
}
