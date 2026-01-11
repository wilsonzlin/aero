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

	sess, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}

	if got := sm.ActiveSessions(); got != 1 {
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
	if got := sm.ActiveSessions(); got != 0 {
		t.Fatalf("ActiveSessions=%d, want 0 after Close", got)
	}
}

func TestSessionManager_EnforcesMaxSessions(t *testing.T) {
	cfg := config.Config{MaxSessions: 1}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)

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

	if got := sm.ActiveSessions(); got != 1 {
		t.Fatalf("ActiveSessions=%d, want 1", got)
	}

	s1.Close()
	if got := sm.ActiveSessions(); got != 0 {
		t.Fatalf("ActiveSessions=%d, want 0 after Close", got)
	}

	s2, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession after Close: %v", err)
	}
	s2.Close()
}
