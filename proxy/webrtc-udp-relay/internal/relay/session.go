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

	mu       sync.Mutex
	closed   bool
	bindings map[uint16]struct{}

	lastViolation time.Time
	violations    int

	onClose func()
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
		id:       id,
		cfg:      cfg,
		metrics:  m,
		clock:    clock,
		limiter:  rl,
		bindings: make(map[uint16]struct{}),
		onClose:  onClose,
	}
}

func (s *Session) ID() string { return s.id }

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

func (s *Session) closeLocked() func() {
	if s.closed {
		return nil
	}
	s.closed = true
	onClose := s.onClose
	s.onClose = nil
	return onClose
}

// EnsureBinding enforces MAX_UDP_BINDINGS_PER_SESSION.
func (s *Session) EnsureBinding(srcPort uint16) error {
	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return ErrSessionClosed
	}

	if _, ok := s.bindings[srcPort]; ok {
		s.mu.Unlock()
		return nil
	}

	if s.cfg.MaxUDPBindingsPerSession > 0 && len(s.bindings) >= s.cfg.MaxUDPBindingsPerSession {
		s.metrics.Inc(metrics.DropReasonQuotaExceeded)
		s.metrics.Inc("too_many_bindings")
		onClose := s.recordViolationLocked(s.clock.Now())
		s.mu.Unlock()
		if onClose != nil {
			onClose()
		}
		return ErrTooManyBindings
	}

	s.bindings[srcPort] = struct{}{}
	s.mu.Unlock()
	return nil
}

// HandleClientDatagram applies rate limiting and quota enforcement to a client
// request to send UDP to destKey.
//
// On soft failures the datagram is dropped and false is returned. In hard mode
// the session may also be closed after repeated violations.
func (s *Session) HandleClientDatagram(srcPort uint16, destKey string, payload []byte) bool {
	if err := s.EnsureBinding(srcPort); err != nil {
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
	onClose := s.recordViolationLocked(s.clock.Now())
	s.mu.Unlock()
	if onClose != nil {
		onClose()
	}
}

func (s *Session) recordViolationLocked(now time.Time) func() {
	if s.cfg.HardCloseAfterViolations <= 0 || s.closed {
		return nil
	}

	if !s.lastViolation.IsZero() && s.cfg.ViolationWindow > 0 && now.Sub(s.lastViolation) > s.cfg.ViolationWindow {
		s.violations = 0
	}

	s.lastViolation = now
	s.violations++
	if s.violations >= s.cfg.HardCloseAfterViolations {
		return s.closeLocked()
	}

	return nil
}
