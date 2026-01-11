package relay

import (
	"testing"
	"time"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
)

func TestSession_SoftRateLimitDropsButKeepsSession(t *testing.T) {
	clk := &ratelimitTestClock{now: time.Unix(0, 0)}
	cfg := config.Config{
		MaxUDPPpsPerSession:             2,
		MaxUniqueDestinationsPerSession: 10,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, clk)
	s, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}

	var forwarded int
	for i := 0; i < 5; i++ {
		if s.HandleClientDatagram(1234, "1.1.1.1:53", []byte("hi")) {
			forwarded++
		}
	}
	if forwarded != 2 {
		t.Fatalf("expected 2 forwarded packets, got %d", forwarded)
	}
	if m.Get(metrics.DropReasonRateLimited) == 0 {
		t.Fatalf("expected rate_limited drops to be recorded")
	}
	if s.Closed() {
		t.Fatalf("expected session to remain open in soft mode")
	}
}

func TestSession_HardModeClosesAfterViolations(t *testing.T) {
	clk := &ratelimitTestClock{now: time.Unix(0, 0)}
	cfg := config.Config{
		MaxUDPPpsPerSession:             1,
		HardCloseAfterViolations:        2,
		ViolationWindow:                 10 * time.Second,
		MaxUniqueDestinationsPerSession: 10,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, clk)
	s, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}

	// First packet allowed.
	if !s.HandleClientDatagram(1234, "1.1.1.1:53", []byte("hi")) {
		t.Fatalf("expected first packet allowed")
	}

	// Next packets violate rate limit; after 2 violations, session closes.
	_ = s.HandleClientDatagram(1234, "1.1.1.1:53", []byte("hi"))
	_ = s.HandleClientDatagram(1234, "1.1.1.1:53", []byte("hi"))

	if !s.Closed() {
		t.Fatalf("expected session to close in hard mode")
	}
	if m.Get(metrics.SessionHardClosed) == 0 {
		t.Fatalf("expected session_hard_closed metric increment")
	}
}

func TestSession_EnforcesUniqueDestinationQuota(t *testing.T) {
	clk := &ratelimitTestClock{now: time.Unix(0, 0)}
	cfg := config.Config{
		MaxUniqueDestinationsPerSession: 1,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, clk)
	s, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}

	if !s.HandleClientDatagram(1234, "1.1.1.1:53", []byte("hi")) {
		t.Fatalf("expected first destination to be allowed")
	}
	if s.HandleClientDatagram(1234, "8.8.8.8:53", []byte("hi")) {
		t.Fatalf("expected second unique destination to be rejected")
	}
	if m.Get(metrics.DropReasonQuotaExceeded) == 0 || m.Get("too_many_destinations") == 0 {
		t.Fatalf("expected destination quota metrics to be incremented")
	}
}

func TestSession_EnforcesDataChannelBps(t *testing.T) {
	clk := &ratelimitTestClock{now: time.Unix(0, 0)}
	cfg := config.Config{
		MaxDataChannelBpsPerSession: 4,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, clk)
	s, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}

	if !s.HandleInboundToClient([]byte("1234")) {
		t.Fatalf("expected first frame to be accepted")
	}
	if s.HandleInboundToClient([]byte("x")) {
		t.Fatalf("expected second frame to be dropped due to rate limit")
	}
	if m.Get(metrics.DropReasonRateLimited) == 0 {
		t.Fatalf("expected rate_limited metric increment")
	}
}

func TestSession_EnforcesUDPBpsPerSession(t *testing.T) {
	clk := &ratelimitTestClock{now: time.Unix(0, 0)}
	cfg := config.Config{
		MaxUDPBpsPerSession: 4,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, clk)
	s, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}

	if !s.HandleClientDatagram(1234, "1.1.1.1:53", []byte("1234")) {
		t.Fatalf("expected first packet to be accepted")
	}
	if s.HandleClientDatagram(1234, "1.1.1.1:53", []byte("x")) {
		t.Fatalf("expected second packet to be dropped due to byte limit")
	}
	if m.Get(metrics.DropReasonRateLimited) == 0 {
		t.Fatalf("expected rate_limited metric increment")
	}
}

func TestSession_EnforcesUDPPpsPerDest(t *testing.T) {
	clk := &ratelimitTestClock{now: time.Unix(0, 0)}
	cfg := config.Config{
		MaxUDPPpsPerDest:                1,
		MaxUniqueDestinationsPerSession: 10,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, clk)
	s, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}

	if !s.HandleClientDatagram(1234, "1.1.1.1:53", []byte("hi")) {
		t.Fatalf("expected first packet to be accepted")
	}
	if s.HandleClientDatagram(1234, "1.1.1.1:53", []byte("hi")) {
		t.Fatalf("expected second packet to be dropped due to per-dest rate limit")
	}
	if m.Get(metrics.DropReasonRateLimited) == 0 {
		t.Fatalf("expected rate_limited metric increment")
	}
}

type ratelimitTestClock struct {
	now time.Time
}

func (c *ratelimitTestClock) Now() time.Time { return c.now }
