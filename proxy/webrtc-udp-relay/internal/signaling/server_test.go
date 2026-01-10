package signaling

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/pion/webrtc/v4"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
	"golang.org/x/net/websocket"
)

func TestServer_EnforcesMaxSessions(t *testing.T) {
	cfg := config.Config{MaxSessions: 1}
	m := metrics.New()
	sm := relay.NewSessionManager(cfg, m, nil)

	srv := NewServer(Config{
		Sessions:            sm,
		WebRTC:              webrtc.NewAPI(),
		RelayConfig:         relay.DefaultConfig(),
		Policy:              policy.NewDevDestinationPolicy(),
		Authorizer:          AllowAllAuthorizer{},
		ICEGatheringTimeout: 2 * time.Second,
	})

	mux := http.NewServeMux()
	srv.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal"
	ws1, err := websocket.Dial(wsURL, "", ts.URL)
	if err != nil {
		t.Fatalf("dial first websocket: %v", err)
	}
	defer ws1.Close()

	ws2, err := websocket.Dial(wsURL, "", ts.URL)
	if err != nil {
		t.Fatalf("dial second websocket: %v", err)
	}
	defer ws2.Close()

	_ = ws2.SetDeadline(time.Now().Add(2 * time.Second))
	var raw string
	if err := websocket.Message.Receive(ws2, &raw); err != nil {
		t.Fatalf("receive: %v", err)
	}
	msg, err := ParseSignalMessage([]byte(raw))
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if msg.Type != MessageTypeError || msg.Code != "too_many_sessions" {
		t.Fatalf("unexpected message: %#v", msg)
	}

	if m.Get(metrics.DropReasonTooManySessions) == 0 {
		t.Fatalf("expected too_many_sessions metric increment")
	}
}
