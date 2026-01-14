package relay

import (
	"encoding/hex"
	"testing"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
)

func TestSessionManager_AssignsHexSessionIDsAndRemovesOnClose(t *testing.T) {
	m := metrics.New()
	sm := NewSessionManager(config.Config{}, m, nil)
	activeSessions := func() int {
		sm.mu.Lock()
		defer sm.mu.Unlock()
		return len(sm.sessions)
	}

	sess, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}

	if got := activeSessions(); got != 1 {
		t.Fatalf("ActiveSessions=%d, want 1", got)
	}

	id := sess.ID()
	if len(id) != 32 {
		t.Fatalf("session id length=%d, want 32", len(id))
	}
	raw, err := hex.DecodeString(id)
	if err != nil {
		t.Fatalf("DecodeString(%q): %v", id, err)
	}
	if len(raw) != 16 {
		t.Fatalf("decoded id length=%d, want 16", len(raw))
	}

	sess.Close()
	if got := activeSessions(); got != 0 {
		t.Fatalf("ActiveSessions=%d, want 0 after Close", got)
	}
}

func TestSessionManager_EnforcesMaxSessions(t *testing.T) {
	cfg := config.Config{MaxSessions: 1}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	activeSessions := func() int {
		sm.mu.Lock()
		defer sm.mu.Unlock()
		return len(sm.sessions)
	}

	s1, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}
	t.Cleanup(s1.Close)

	_, err = sm.CreateSession()
	if err != ErrTooManySessions {
		t.Fatalf("CreateSession err=%v, want %v", err, ErrTooManySessions)
	}
	if got := m.Get(metrics.DropReasonTooManySessions); got == 0 {
		t.Fatalf("expected %s metric increment", metrics.DropReasonTooManySessions)
	}

	if got := activeSessions(); got != 1 {
		t.Fatalf("ActiveSessions=%d, want 1", got)
	}

	s1.Close()
	if got := activeSessions(); got != 0 {
		t.Fatalf("ActiveSessions=%d, want 0 after Close", got)
	}

	s2, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession after Close: %v", err)
	}
	s2.Close()
}

func TestSessionManager_CreateSessionWithKey_RejectsWhenKeyAlreadyActive(t *testing.T) {
	m := metrics.New()
	sm := NewSessionManager(config.Config{}, m, nil)

	s1, err := sm.CreateSessionWithKey("sid_test")
	if err != nil {
		t.Fatalf("CreateSessionWithKey: %v", err)
	}
	t.Cleanup(s1.Close)

	_, err = sm.CreateSessionWithKey("sid_test")
	if err != ErrSessionAlreadyActive {
		t.Fatalf("CreateSessionWithKey err=%v, want %v", err, ErrSessionAlreadyActive)
	}

	// Closing the session should free the stable key for reuse.
	s1.Close()
	s2, err := sm.CreateSessionWithKey("sid_test")
	if err != nil {
		t.Fatalf("CreateSessionWithKey after Close: %v", err)
	}
	s2.Close()
}

func TestSessionManager_CreateSessionWithKey_DoesNotTrimKey(t *testing.T) {
	m := metrics.New()
	sm := NewSessionManager(config.Config{}, m, nil)

	// Whitespace keys should still be treated as stable keys. This protects
	// against bypasses if an upstream auth layer accepts a whitespace-only JWT
	// `sid` value.
	s1, err := sm.CreateSessionWithKey(" ")
	if err != nil {
		t.Fatalf("CreateSessionWithKey: %v", err)
	}
	t.Cleanup(s1.Close)

	_, err = sm.CreateSessionWithKey(" ")
	if err != ErrSessionAlreadyActive {
		t.Fatalf("CreateSessionWithKey err=%v, want %v", err, ErrSessionAlreadyActive)
	}
}

func TestSessionManager_CreateSessionWithKey_AllowsDifferentKeys(t *testing.T) {
	m := metrics.New()
	sm := NewSessionManager(config.Config{}, m, nil)

	s1, err := sm.CreateSessionWithKey("sid_a")
	if err != nil {
		t.Fatalf("CreateSessionWithKey(sid_a): %v", err)
	}
	t.Cleanup(s1.Close)

	s2, err := sm.CreateSessionWithKey("sid_b")
	if err != nil {
		t.Fatalf("CreateSessionWithKey(sid_b): %v", err)
	}
	t.Cleanup(s2.Close)

	if s1.ID() == s2.ID() {
		t.Fatalf("expected distinct public session IDs, got %q", s1.ID())
	}
	if got := sm.ActiveSessions(); got != 2 {
		t.Fatalf("ActiveSessions=%d, want 2", got)
	}
}
