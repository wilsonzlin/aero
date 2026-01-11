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
	if clock == nil {
		clock = ratelimit.RealClock{}
	}
	rl := ratelimit.NewSessionLimiter(clock, ratelimit.SessionConfig{
		UDPPacketsPerSecond:        cfg.MaxUDPPpsPerSession,
		UDPBytesPerSecond:          cfg.MaxUDPBpsPerSession,
		DataChannelBytesPerSecond:  cfg.MaxDataChannelBpsPerSession,
		UDPPacketsPerSecondPerDest: cfg.MaxUDPPpsPerDest,
		MaxUniqueDestinations:      cfg.MaxUniqueDestinationsPerSession,
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

// AllowClientDatagram applies rate limiting and destination quota enforcement
// to a client request to send UDP to destKey.
//
// Guest-port binding limits are enforced by the relay engine (SessionRelay).
func (s *Session) AllowClientDatagram(destKey string, payload []byte) bool {
	if s.Closed() {
		return false
	}

	allowed, reason := s.limiter.AllowUDPSend(destKey, len(payload))
	if allowed {
		return true
	}

	switch reason {
	case ratelimit.DropReasonTooManyDestinations:
		s.metrics.Inc(metrics.DropReasonQuotaExceeded)
		s.metrics.Inc("too_many_destinations")
	default:
		s.metrics.Inc(metrics.DropReasonRateLimited)
	}

	s.recordViolation()
	return false
}

// HandleClientDatagram applies rate limiting and quota enforcement to a client
// request to send UDP to destKey.
//
// On soft failures the datagram is dropped and false is returned. In hard mode
// the session may also be closed after repeated violations.
func (s *Session) HandleClientDatagram(_ uint16, destKey string, payload []byte) bool {
	return s.AllowClientDatagram(destKey, payload)
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
	onHardClose, onClose := s.recordViolationLocked(s.clock.Now())
	s.mu.Unlock()
	if onHardClose != nil {
		onHardClose()
	}
	if onClose != nil {
		onClose()
	}
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
