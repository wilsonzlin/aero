package signaling_test

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/signaling"
)

func startSignalingAndUDPServer(t *testing.T, cfg config.Config) (*httptest.Server, *metrics.Metrics) {
	t.Helper()

	api := newTestWebRTCAPI(t)
	authz, err := signaling.NewAuthAuthorizer(cfg)
	if err != nil {
		t.Fatalf("NewAuthAuthorizer: %v", err)
	}
	m := metrics.New()
	sm := relay.NewSessionManager(cfg, m, nil)

	sig := signaling.NewServer(signaling.Config{
		Sessions:                      sm,
		WebRTC:                        api,
		ICEServers:                    nil,
		RelayConfig:                   relay.DefaultConfig(),
		Policy:                        policy.NewDevDestinationPolicy(),
		Authorizer:                    authz,
		ICEGatheringTimeout:           2 * time.Second,
		SessionPreallocTTL:            cfg.SessionPreallocTTL,
		SignalingAuthTimeout:          cfg.SignalingAuthTimeout,
		MaxSignalingMessageBytes:      cfg.MaxSignalingMessageBytes,
		MaxSignalingMessagesPerSecond: cfg.MaxSignalingMessagesPerSecond,
	})

	udpWS, err := relay.NewUDPWebSocketServer(cfg, sm, relay.DefaultConfig(), policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	sig.RegisterRoutes(mux)
	mux.Handle("GET /udp", udpWS)

	ts := httptest.NewServer(mux)
	t.Cleanup(func() {
		sig.Close()
		ts.Close()
	})
	return ts, m
}

type udpWSControlMessage struct {
	Type      string `json:"type"`
	SessionID string `json:"sessionId,omitempty"`
	Code      string `json:"code,omitempty"`
	Message   string `json:"message,omitempty"`
}

func TestAuth_JWT_RejectsConcurrentSessionsWithSameSID_AcrossSignalAndUDP(t *testing.T) {
	cfg := config.Config{
		AuthMode:  config.AuthModeJWT,
		JWTSecret: "supersecret",
	}
	ts, _ := startSignalingAndUDPServer(t, cfg)

	api := newTestWebRTCAPI(t)
	offerSDP := newOfferSDP(t, api)

	now := time.Now().Unix()
	tokenA := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-10)
	tokenB := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-9)

	wsSignalURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal?token=" + tokenA
	ws1, _, err := websocket.DefaultDialer.Dial(wsSignalURL, nil)
	if err != nil {
		t.Fatalf("dial ws1: %v", err)
	}
	t.Cleanup(func() { _ = ws1.Close() })

	if err := ws1.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer ws1: %v", err)
	}

	// Ensure ws1 has allocated its relay session before attempting to create a
	// /udp session with the same JWT sid.
	_ = ws1.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err := ws1.ReadMessage()
	if err != nil {
		t.Fatalf("read ws1: %v", err)
	}
	got, err := parseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse ws1: %v", err)
	}
	if got.Type != "answer" {
		t.Fatalf("unexpected ws1 message: %#v", got)
	}

	wsUDPURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/udp?apiKey=" + tokenB
	ws2, _, err := websocket.DefaultDialer.Dial(wsUDPURL, nil)
	if err != nil {
		t.Fatalf("dial ws2: %v", err)
	}
	t.Cleanup(func() { _ = ws2.Close() })

	_ = ws2.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err = ws2.ReadMessage()
	if err != nil {
		t.Fatalf("read ws2: %v", err)
	}
	var ctrl udpWSControlMessage
	if err := json.Unmarshal(msg, &ctrl); err != nil {
		t.Fatalf("unmarshal ws2 message: %v", err)
	}
	if ctrl.Type != "error" || ctrl.Code != "session_already_active" {
		t.Fatalf("unexpected ws2 message: %#v", ctrl)
	}
}

func TestAuth_JWT_RejectsConcurrentSessionsWithSameSID_AcrossSignalAndUDP_UDPHeaderAuth(t *testing.T) {
	cfg := config.Config{
		AuthMode:  config.AuthModeJWT,
		JWTSecret: "supersecret",
	}
	ts, _ := startSignalingAndUDPServer(t, cfg)

	api := newTestWebRTCAPI(t)
	offerSDP := newOfferSDP(t, api)

	now := time.Now().Unix()
	tokenA := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-10)
	tokenB := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-9)

	wsSignalURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal?token=" + tokenA
	ws1, _, err := websocket.DefaultDialer.Dial(wsSignalURL, nil)
	if err != nil {
		t.Fatalf("dial ws1: %v", err)
	}
	t.Cleanup(func() { _ = ws1.Close() })

	if err := ws1.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer ws1: %v", err)
	}

	// Ensure ws1 has allocated its relay session before attempting to create a
	// /udp session with the same JWT sid.
	_ = ws1.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err := ws1.ReadMessage()
	if err != nil {
		t.Fatalf("read ws1: %v", err)
	}
	got, err := parseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse ws1: %v", err)
	}
	if got.Type != "answer" {
		t.Fatalf("unexpected ws1 message: %#v", got)
	}

	wsUDPURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/udp"
	h := http.Header{}
	h.Set("Authorization", "Bearer "+tokenB)
	ws2, _, err := websocket.DefaultDialer.Dial(wsUDPURL, h)
	if err != nil {
		t.Fatalf("dial ws2: %v", err)
	}
	t.Cleanup(func() { _ = ws2.Close() })

	_ = ws2.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err = ws2.ReadMessage()
	if err != nil {
		t.Fatalf("read ws2: %v", err)
	}
	var ctrl udpWSControlMessage
	if err := json.Unmarshal(msg, &ctrl); err != nil {
		t.Fatalf("unmarshal ws2 message: %v", err)
	}
	if ctrl.Type != "error" || ctrl.Code != "session_already_active" {
		t.Fatalf("unexpected ws2 message: %#v", ctrl)
	}
}
