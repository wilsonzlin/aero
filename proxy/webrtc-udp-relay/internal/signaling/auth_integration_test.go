package signaling_test

import (
	"bytes"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"
	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/signaling"
)

func newTestWebRTCAPI(t *testing.T) *webrtc.API {
	t.Helper()
	me := &webrtc.MediaEngine{}
	if err := me.RegisterDefaultCodecs(); err != nil {
		t.Fatalf("register codecs: %v", err)
	}
	return webrtc.NewAPI(webrtc.WithMediaEngine(me))
}

func newOfferSDP(t *testing.T, api *webrtc.API) signaling.SDP {
	t.Helper()
	pc, err := api.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new pc: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := false
	maxRetransmits := uint16(0)
	if _, err := pc.CreateDataChannel("udp", &webrtc.DataChannelInit{Ordered: &ordered, MaxRetransmits: &maxRetransmits}); err != nil {
		t.Fatalf("create data channel: %v", err)
	}

	offer, err := pc.CreateOffer(nil)
	if err != nil {
		t.Fatalf("create offer: %v", err)
	}
	if err := pc.SetLocalDescription(offer); err != nil {
		t.Fatalf("set local offer: %v", err)
	}
	<-webrtc.GatheringCompletePromise(pc)

	local := pc.LocalDescription()
	if local == nil {
		t.Fatalf("missing local description")
	}
	return signaling.SDPFromPion(*local)
}

func startSignalingServer(t *testing.T, cfg config.Config) (*httptest.Server, *metrics.Metrics) {
	t.Helper()

	api := newTestWebRTCAPI(t)
	authz, err := signaling.NewAuthAuthorizer(cfg)
	if err != nil {
		t.Fatalf("NewAuthAuthorizer: %v", err)
	}
	m := metrics.New()
	sm := relay.NewSessionManager(cfg, m, nil)

	srv := signaling.NewServer(signaling.Config{
		Sessions:                      sm,
		WebRTC:                        api,
		ICEServers:                    nil,
		RelayConfig:                   relay.DefaultConfig(),
		Policy:                        policy.NewDevDestinationPolicy(),
		Authorizer:                    authz,
		ICEGatheringTimeout:           2 * time.Second,
		SignalingAuthTimeout:          cfg.SignalingAuthTimeout,
		MaxSignalingMessageBytes:      cfg.MaxSignalingMessageBytes,
		MaxSignalingMessagesPerSecond: cfg.MaxSignalingMessagesPerSecond,
	})

	mux := http.NewServeMux()
	srv.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	t.Cleanup(func() {
		srv.Close()
		ts.Close()
	})
	return ts, m
}

func makeJWT(secret string) string {
	header := base64.RawURLEncoding.EncodeToString([]byte(`{"alg":"HS256","typ":"JWT"}`))
	payload := base64.RawURLEncoding.EncodeToString([]byte(`{}`))
	unsigned := header + "." + payload

	mac := hmac.New(sha256.New, []byte(secret))
	_, _ = mac.Write([]byte(unsigned))
	sig := base64.RawURLEncoding.EncodeToString(mac.Sum(nil))
	return unsigned + "." + sig
}

func TestAuth_APIKey_WebRTCOffer(t *testing.T) {
	cfg := config.Config{
		AuthMode: config.AuthModeAPIKey,
		APIKey:   "secret",
	}
	ts, m := startSignalingServer(t, cfg)

	api := newTestWebRTCAPI(t)
	offerSDP := newOfferSDP(t, api)
	body, err := json.Marshal(map[string]any{"sdp": offerSDP})
	if err != nil {
		t.Fatalf("marshal offer: %v", err)
	}

	do := func(apiKey string) *http.Response {
		req, err := http.NewRequest(http.MethodPost, ts.URL+"/webrtc/offer", bytes.NewReader(body))
		if err != nil {
			t.Fatalf("NewRequest: %v", err)
		}
		req.Header.Set("Content-Type", "application/json")
		if apiKey != "" {
			req.Header.Set("X-API-Key", apiKey)
		}
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do: %v", err)
		}
		return resp
	}

	resp := do("")
	if resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("missing api key status=%d, want %d", resp.StatusCode, http.StatusUnauthorized)
	}
	_ = resp.Body.Close()

	resp = do("wrong")
	if resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("bad api key status=%d, want %d", resp.StatusCode, http.StatusUnauthorized)
	}
	_ = resp.Body.Close()

	if got := m.Get(metrics.AuthFailure); got < 2 {
		t.Fatalf("auth failure metric=%d, want >= 2", got)
	}

	resp = do("secret")
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("good api key status=%d, want %d", resp.StatusCode, http.StatusOK)
	}
	_ = resp.Body.Close()
}

func TestAuth_JWT_WebRTCOffer(t *testing.T) {
	cfg := config.Config{
		AuthMode:  config.AuthModeJWT,
		JWTSecret: "supersecret",
	}
	ts, _ := startSignalingServer(t, cfg)

	api := newTestWebRTCAPI(t)
	offerSDP := newOfferSDP(t, api)
	body, err := json.Marshal(map[string]any{"sdp": offerSDP})
	if err != nil {
		t.Fatalf("marshal offer: %v", err)
	}

	do := func(token string) *http.Response {
		req, err := http.NewRequest(http.MethodPost, ts.URL+"/webrtc/offer", bytes.NewReader(body))
		if err != nil {
			t.Fatalf("NewRequest: %v", err)
		}
		req.Header.Set("Content-Type", "application/json")
		if token != "" {
			req.Header.Set("Authorization", "Bearer "+token)
		}
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do: %v", err)
		}
		return resp
	}

	resp := do("")
	if resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("missing token status=%d, want %d", resp.StatusCode, http.StatusUnauthorized)
	}
	_ = resp.Body.Close()

	resp = do("not-a-jwt")
	if resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("bad token status=%d, want %d", resp.StatusCode, http.StatusUnauthorized)
	}
	_ = resp.Body.Close()

	resp = do(makeJWT(cfg.JWTSecret))
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("good token status=%d, want %d", resp.StatusCode, http.StatusOK)
	}
	_ = resp.Body.Close()
}

func TestAuth_APIKey_WebSocketSignal_FirstMessageAuth(t *testing.T) {
	cfg := config.Config{
		AuthMode:                      config.AuthModeAPIKey,
		APIKey:                        "secret",
		SignalingAuthTimeout:          2 * time.Second,
		MaxSignalingMessageBytes:      64 * 1024,
		MaxSignalingMessagesPerSecond: 50,
	}
	ts, _ := startSignalingServer(t, cfg)

	api := newTestWebRTCAPI(t)
	offerSDP := newOfferSDP(t, api)

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal"
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })

	if err := c.WriteJSON(signaling.SignalMessage{Type: signaling.MessageTypeAuth, APIKey: "secret"}); err != nil {
		t.Fatalf("write auth: %v", err)
	}
	if err := c.WriteJSON(signaling.SignalMessage{Type: signaling.MessageTypeOffer, SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer: %v", err)
	}

	_ = c.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	got, err := signaling.ParseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if got.Type != signaling.MessageTypeAnswer {
		t.Fatalf("unexpected message: %#v", got)
	}
}

func TestAuth_JWT_WebSocketSignal_QueryParamFallback(t *testing.T) {
	cfg := config.Config{
		AuthMode:                      config.AuthModeJWT,
		JWTSecret:                     "supersecret",
		SignalingAuthTimeout:          2 * time.Second,
		MaxSignalingMessageBytes:      64 * 1024,
		MaxSignalingMessagesPerSecond: 50,
	}
	ts, _ := startSignalingServer(t, cfg)

	api := newTestWebRTCAPI(t)
	offerSDP := newOfferSDP(t, api)

	token := makeJWT(cfg.JWTSecret)
	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal?token=" + token
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })

	if err := c.WriteJSON(signaling.SignalMessage{Type: signaling.MessageTypeOffer, SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer: %v", err)
	}

	_ = c.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	got, err := signaling.ParseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if got.Type != signaling.MessageTypeAnswer {
		t.Fatalf("unexpected message: %#v", got)
	}
}

func TestWebSocketAuthTimeout(t *testing.T) {
	cfg := config.Config{
		AuthMode:                      config.AuthModeAPIKey,
		APIKey:                        "secret",
		SignalingAuthTimeout:          50 * time.Millisecond,
		MaxSignalingMessageBytes:      64 * 1024,
		MaxSignalingMessagesPerSecond: 50,
	}
	ts, m := startSignalingServer(t, cfg)

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal"
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })

	_ = c.SetReadDeadline(time.Now().Add(2 * time.Second))
	_, _, err = c.ReadMessage()
	if err == nil {
		t.Fatalf("expected close error")
	}
	if !websocket.IsCloseError(err, websocket.ClosePolicyViolation) {
		t.Fatalf("expected policy violation close; got %v", err)
	}
	if m.Get(metrics.AuthFailure) == 0 {
		t.Fatalf("expected auth_failure metric increment")
	}
}

func TestWebSocketOversizedMessageRejected(t *testing.T) {
	cfg := config.Config{
		AuthMode:                      config.AuthModeAPIKey,
		APIKey:                        "secret",
		SignalingAuthTimeout:          2 * time.Second,
		MaxSignalingMessageBytes:      32,
		MaxSignalingMessagesPerSecond: 50,
	}
	ts, _ := startSignalingServer(t, cfg)

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal"
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })

	oversized := `{"type":"auth","apiKey":"` + strings.Repeat("a", 128) + `"}`
	if err := c.WriteMessage(websocket.TextMessage, []byte(oversized)); err != nil {
		// The server may close fast enough that the write fails, which is fine.
		return
	}

	_ = c.SetReadDeadline(time.Now().Add(2 * time.Second))
	_, _, err = c.ReadMessage()
	if err == nil {
		t.Fatalf("expected close error")
	}
	if !websocket.IsCloseError(err, websocket.CloseMessageTooBig) {
		t.Fatalf("expected message too big close; got %v", err)
	}
}

func TestWebSocketRejectsBinaryBeforeAuth(t *testing.T) {
	cfg := config.Config{
		AuthMode:                      config.AuthModeAPIKey,
		APIKey:                        "secret",
		SignalingAuthTimeout:          2 * time.Second,
		MaxSignalingMessageBytes:      64 * 1024,
		MaxSignalingMessagesPerSecond: 50,
	}
	ts, _ := startSignalingServer(t, cfg)

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal"
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })

	if err := c.WriteMessage(websocket.BinaryMessage, []byte{0x01, 0x02}); err != nil {
		t.Fatalf("write binary: %v", err)
	}

	_ = c.SetReadDeadline(time.Now().Add(2 * time.Second))
	msgType, msg, err := c.ReadMessage()
	if err != nil {
		if websocket.IsCloseError(err, websocket.CloseUnsupportedData) {
			return
		}
		t.Fatalf("read: %v", err)
	}
	if msgType != websocket.TextMessage {
		t.Fatalf("unexpected message type %d", msgType)
	}
	parsed, err := signaling.ParseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if parsed.Type != signaling.MessageTypeError || parsed.Code != "bad_message" {
		t.Fatalf("unexpected server message: %#v", parsed)
	}

	_, _, err = c.ReadMessage()
	if err == nil {
		t.Fatalf("expected close error")
	}
	if !websocket.IsCloseError(err, websocket.CloseUnsupportedData) {
		t.Fatalf("expected unsupported data close; got %v", err)
	}
}

func TestWebSocketRateLimitExceeded(t *testing.T) {
	cfg := config.Config{
		AuthMode:                      config.AuthModeNone,
		SignalingAuthTimeout:          2 * time.Second,
		MaxSignalingMessageBytes:      64 * 1024,
		MaxSignalingMessagesPerSecond: 1,
	}
	ts, m := startSignalingServer(t, cfg)

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal"
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })

	// The server tolerates auth messages even when AUTH_MODE=none.
	if err := c.WriteJSON(signaling.SignalMessage{Type: signaling.MessageTypeAuth, APIKey: "ignored"}); err != nil {
		t.Fatalf("write auth1: %v", err)
	}
	if err := c.WriteJSON(signaling.SignalMessage{Type: signaling.MessageTypeAuth, APIKey: "ignored"}); err != nil {
		t.Fatalf("write auth2: %v", err)
	}

	_ = c.SetReadDeadline(time.Now().Add(2 * time.Second))
	_, raw, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	parsed, err := signaling.ParseSignalMessage(raw)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if parsed.Type != signaling.MessageTypeError || parsed.Code != "rate_limited" {
		t.Fatalf("unexpected server message: %#v", parsed)
	}
	if m.Get(metrics.DropReasonRateLimited) == 0 {
		t.Fatalf("expected rate_limited metric increment")
	}

	_, _, err = c.ReadMessage()
	if err == nil {
		t.Fatalf("expected close error")
	}
	if !websocket.IsCloseError(err, websocket.ClosePolicyViolation) {
		t.Fatalf("expected policy violation close; got %v", err)
	}
}
