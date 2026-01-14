package signaling_test

import (
	"bytes"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
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

type sdp struct {
	Type string `json:"type"`
	SDP  string `json:"sdp"`
}

type candidate struct {
	Candidate        string  `json:"candidate"`
	SDPMid           *string `json:"sdpMid,omitempty"`
	SDPMLineIndex    *uint16 `json:"sdpMLineIndex,omitempty"`
	UsernameFragment *string `json:"usernameFragment,omitempty"`
}

type signalMessage struct {
	Type      string     `json:"type"`
	SDP       *sdp       `json:"sdp,omitempty"`
	Candidate *candidate `json:"candidate,omitempty"`

	APIKey string `json:"apiKey,omitempty"`
	Token  string `json:"token,omitempty"`

	Code    string `json:"code,omitempty"`
	Message string `json:"message,omitempty"`
}

func (m signalMessage) validate() error {
	switch m.Type {
	case "auth":
		if m.APIKey == "" && m.Token == "" {
			return errors.New("auth message missing apiKey/token")
		}
		if m.APIKey != "" && m.Token != "" && m.APIKey != m.Token {
			return errors.New("auth message must not include both apiKey and token unless they match")
		}
		if m.SDP != nil || m.Candidate != nil || m.Code != "" || m.Message != "" {
			return errors.New("auth message has unexpected fields")
		}
	case "offer":
		if m.SDP == nil {
			return errors.New("offer message missing sdp")
		}
		if m.SDP.Type != "offer" {
			return fmt.Errorf("offer message has sdp.type=%q", m.SDP.Type)
		}
		if m.Candidate != nil || m.APIKey != "" || m.Token != "" || m.Code != "" || m.Message != "" {
			return errors.New("offer message has unexpected fields")
		}
	case "answer":
		if m.SDP == nil {
			return errors.New("answer message missing sdp")
		}
		if m.SDP.Type != "answer" {
			return fmt.Errorf("answer message has sdp.type=%q", m.SDP.Type)
		}
		if m.Candidate != nil || m.APIKey != "" || m.Token != "" || m.Code != "" || m.Message != "" {
			return errors.New("answer message has unexpected fields")
		}
	case "candidate":
		if m.Candidate == nil {
			return errors.New("candidate message missing candidate")
		}
		if m.SDP != nil || m.APIKey != "" || m.Token != "" || m.Code != "" || m.Message != "" {
			return errors.New("candidate message has unexpected fields")
		}
	case "close":
		if m.SDP != nil || m.Candidate != nil || m.APIKey != "" || m.Token != "" || m.Code != "" || m.Message != "" {
			return errors.New("close message has unexpected fields")
		}
	case "error":
		if m.Code == "" || m.Message == "" {
			return errors.New("error message missing code/message")
		}
		if m.SDP != nil || m.Candidate != nil || m.APIKey != "" || m.Token != "" {
			return errors.New("error message has unexpected fields")
		}
	default:
		return fmt.Errorf("unsupported message type %q", m.Type)
	}
	return nil
}

func newOfferSDP(t *testing.T, api *webrtc.API) sdp {
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
	return sdp{Type: local.Type.String(), SDP: local.SDP}
}

func parseSignalMessage(data []byte) (signalMessage, error) {
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.DisallowUnknownFields()

	var msg signalMessage
	if err := dec.Decode(&msg); err != nil {
		return signalMessage{}, err
	}
	if err := msg.validate(); err != nil {
		return signalMessage{}, err
	}
	if err := dec.Decode(&struct{}{}); err != io.EOF {
		return signalMessage{}, errors.New("unexpected trailing data")
	}
	return msg, nil
}

type legacySessionDescription struct {
	Type string `json:"type"`
	SDP  string `json:"sdp"`
}

type legacyOfferRequest struct {
	Version int                      `json:"version"`
	Offer   legacySessionDescription `json:"offer"`
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
		SessionPreallocTTL:            cfg.SessionPreallocTTL,
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

func makeJWT(secret, sid string) string {
	return makeJWTWithIat(secret, sid, time.Now().Unix())
}

func makeJWTWithIat(secret, sid string, iat int64) string {
	header := base64.RawURLEncoding.EncodeToString([]byte(`{"alg":"HS256","typ":"JWT"}`))
	payloadJSON, _ := json.Marshal(struct {
		Iat int64  `json:"iat"`
		Exp int64  `json:"exp"`
		SID string `json:"sid"`
	}{
		Iat: iat,
		Exp: iat + 60,
		SID: sid,
	})
	payload := base64.RawURLEncoding.EncodeToString(payloadJSON)
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

	t.Run("valid query token alias", func(t *testing.T) {
		req, err := http.NewRequest(http.MethodPost, ts.URL+"/webrtc/offer?token=secret", bytes.NewReader(body))
		if err != nil {
			t.Fatalf("NewRequest: %v", err)
		}
		req.Header.Set("Content-Type", "application/json")

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do: %v", err)
		}
		defer resp.Body.Close()

		if resp.StatusCode != http.StatusOK {
			t.Fatalf("query token alias status=%d, want %d", resp.StatusCode, http.StatusOK)
		}
	})
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

	token := makeJWT(cfg.JWTSecret, "sess_test")
	resp = do(token)
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("good token status=%d, want %d", resp.StatusCode, http.StatusOK)
	}
	_ = resp.Body.Close()

	t.Run("valid query apiKey alias", func(t *testing.T) {
		token := makeJWT(cfg.JWTSecret, "sess_test_2")
		req, err := http.NewRequest(http.MethodPost, ts.URL+"/webrtc/offer?apiKey="+token, bytes.NewReader(body))
		if err != nil {
			t.Fatalf("NewRequest: %v", err)
		}
		req.Header.Set("Content-Type", "application/json")

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do: %v", err)
		}
		defer resp.Body.Close()

		if resp.StatusCode != http.StatusOK {
			t.Fatalf("query apiKey alias status=%d, want %d", resp.StatusCode, http.StatusOK)
		}
	})
}

func TestAuth_APIKey_WebSocketSignal_AuthTimeoutClosesUnauthenticatedConnection(t *testing.T) {
	cfg := config.Config{
		AuthMode:                      config.AuthModeAPIKey,
		APIKey:                        "secret",
		SignalingAuthTimeout:          200 * time.Millisecond,
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

	// Send no auth message and verify the server closes the connection once
	// SignalingAuthTimeout elapses.
	_ = c.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, _, err = c.ReadMessage()
	if err == nil {
		t.Fatalf("expected auth timeout close error, got nil")
	}
	if !websocket.IsCloseError(err, websocket.ClosePolicyViolation) {
		t.Fatalf("expected close policy violation, got %v", err)
	}

	if got := m.Get(metrics.AuthFailure); got == 0 {
		t.Fatalf("expected auth failure metric increment")
	}
}

func TestAuth_JWT_RejectsConcurrentSessionsWithSameSID_WebSocketSignal(t *testing.T) {
	cfg := config.Config{
		AuthMode:  config.AuthModeJWT,
		JWTSecret: "supersecret",
	}
	ts, _ := startSignalingServer(t, cfg)

	api := newTestWebRTCAPI(t)
	offerSDP := newOfferSDP(t, api)

	now := time.Now().Unix()
	tokenA := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-10)
	tokenB := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-9)
	wsURLA := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal?token=" + tokenA
	wsURLB := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal?token=" + tokenB

	ws1, _, err := websocket.DefaultDialer.Dial(wsURLA, nil)
	if err != nil {
		t.Fatalf("dial ws1: %v", err)
	}
	t.Cleanup(func() { _ = ws1.Close() })

	if err := ws1.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer ws1: %v", err)
	}

	// Ensure the first offer was processed (and the relay session allocated)
	// before attempting to create another session with the same JWT sid.
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

	ws2, _, err := websocket.DefaultDialer.Dial(wsURLB, nil)
	if err != nil {
		t.Fatalf("dial ws2: %v", err)
	}
	t.Cleanup(func() { _ = ws2.Close() })

	if err := ws2.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer ws2: %v", err)
	}

	_ = ws2.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err = ws2.ReadMessage()
	if err != nil {
		t.Fatalf("read ws2: %v", err)
	}
	got, err = parseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse ws2: %v", err)
	}
	if got.Type != "error" || got.Code != "session_already_active" {
		t.Fatalf("unexpected ws2 message: %#v", got)
	}
}

func TestAuth_JWT_RejectsConcurrentSessionsWithSameSID_WebSocketSignal_QueryAPIKeyAlias(t *testing.T) {
	cfg := config.Config{
		AuthMode:  config.AuthModeJWT,
		JWTSecret: "supersecret",
	}
	ts, _ := startSignalingServer(t, cfg)

	api := newTestWebRTCAPI(t)
	offerSDP := newOfferSDP(t, api)

	now := time.Now().Unix()
	tokenA := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-10)
	tokenB := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-9)
	wsURLA := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal?token=" + tokenA
	wsURLB := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal?apiKey=" + tokenB

	ws1, _, err := websocket.DefaultDialer.Dial(wsURLA, nil)
	if err != nil {
		t.Fatalf("dial ws1: %v", err)
	}
	t.Cleanup(func() { _ = ws1.Close() })

	if err := ws1.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer ws1: %v", err)
	}
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

	ws2, _, err := websocket.DefaultDialer.Dial(wsURLB, nil)
	if err != nil {
		t.Fatalf("dial ws2: %v", err)
	}
	t.Cleanup(func() { _ = ws2.Close() })

	if err := ws2.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer ws2: %v", err)
	}
	_ = ws2.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err = ws2.ReadMessage()
	if err != nil {
		t.Fatalf("read ws2: %v", err)
	}
	got, err = parseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse ws2: %v", err)
	}
	if got.Type != "error" || got.Code != "session_already_active" {
		t.Fatalf("unexpected ws2 message: %#v", got)
	}
}

func TestAuth_JWT_RejectsConcurrentSessionsWithSameSID_WebSocketSignal_FirstMessageAuth_AliasField(t *testing.T) {
	cfg := config.Config{
		AuthMode:  config.AuthModeJWT,
		JWTSecret: "supersecret",
	}
	ts, _ := startSignalingServer(t, cfg)

	api := newTestWebRTCAPI(t)
	offerSDP := newOfferSDP(t, api)

	now := time.Now().Unix()
	tokenA := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-10)
	tokenB := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-9)
	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal"

	ws1, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial ws1: %v", err)
	}
	t.Cleanup(func() { _ = ws1.Close() })
	if err := ws1.WriteJSON(signalMessage{Type: "auth", Token: tokenA}); err != nil {
		t.Fatalf("write auth ws1: %v", err)
	}
	if err := ws1.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer ws1: %v", err)
	}
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

	ws2, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial ws2: %v", err)
	}
	t.Cleanup(func() { _ = ws2.Close() })
	if err := ws2.WriteJSON(signalMessage{Type: "auth", APIKey: tokenB}); err != nil {
		t.Fatalf("write auth ws2: %v", err)
	}
	if err := ws2.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer ws2: %v", err)
	}
	_ = ws2.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err = ws2.ReadMessage()
	if err != nil {
		t.Fatalf("read ws2: %v", err)
	}
	got, err = parseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse ws2: %v", err)
	}
	if got.Type != "error" || got.Code != "session_already_active" {
		t.Fatalf("unexpected ws2 message: %#v", got)
	}
}

func TestAuth_JWT_AllowsConcurrentSessionsWithDifferentSID_WebSocketSignal(t *testing.T) {
	cfg := config.Config{
		AuthMode:    config.AuthModeJWT,
		JWTSecret:   "supersecret",
		MaxSessions: 2,
	}
	ts, _ := startSignalingServer(t, cfg)

	api := newTestWebRTCAPI(t)
	offerSDP := newOfferSDP(t, api)

	now := time.Now().Unix()
	tokenA := makeJWTWithIat(cfg.JWTSecret, "sess_test_a", now-10)
	tokenB := makeJWTWithIat(cfg.JWTSecret, "sess_test_b", now-9)
	wsURLA := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal?token=" + tokenA
	wsURLB := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal?token=" + tokenB

	ws1, _, err := websocket.DefaultDialer.Dial(wsURLA, nil)
	if err != nil {
		t.Fatalf("dial ws1: %v", err)
	}
	t.Cleanup(func() { _ = ws1.Close() })
	if err := ws1.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer ws1: %v", err)
	}
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

	ws2, _, err := websocket.DefaultDialer.Dial(wsURLB, nil)
	if err != nil {
		t.Fatalf("dial ws2: %v", err)
	}
	t.Cleanup(func() { _ = ws2.Close() })
	if err := ws2.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer ws2: %v", err)
	}
	_ = ws2.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err = ws2.ReadMessage()
	if err != nil {
		t.Fatalf("read ws2: %v", err)
	}
	got, err = parseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse ws2: %v", err)
	}
	if got.Type != "answer" {
		t.Fatalf("unexpected ws2 message: %#v", got)
	}
}

func TestAuth_JWT_AllowsConcurrentSessionsWithDifferentSID_WebSocketSignal_FirstMessageAuth(t *testing.T) {
	cfg := config.Config{
		AuthMode:    config.AuthModeJWT,
		JWTSecret:   "supersecret",
		MaxSessions: 2,
	}
	ts, _ := startSignalingServer(t, cfg)

	api := newTestWebRTCAPI(t)
	offerSDP := newOfferSDP(t, api)

	now := time.Now().Unix()
	tokenA := makeJWTWithIat(cfg.JWTSecret, "sess_test_a", now-10)
	tokenB := makeJWTWithIat(cfg.JWTSecret, "sess_test_b", now-9)
	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal"

	ws1, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial ws1: %v", err)
	}
	t.Cleanup(func() { _ = ws1.Close() })
	if err := ws1.WriteJSON(signalMessage{Type: "auth", Token: tokenA}); err != nil {
		t.Fatalf("write auth ws1: %v", err)
	}
	if err := ws1.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer ws1: %v", err)
	}
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

	ws2, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial ws2: %v", err)
	}
	t.Cleanup(func() { _ = ws2.Close() })
	if err := ws2.WriteJSON(signalMessage{Type: "auth", Token: tokenB}); err != nil {
		t.Fatalf("write auth ws2: %v", err)
	}
	if err := ws2.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer ws2: %v", err)
	}
	_ = ws2.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err = ws2.ReadMessage()
	if err != nil {
		t.Fatalf("read ws2: %v", err)
	}
	got, err = parseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse ws2: %v", err)
	}
	if got.Type != "answer" {
		t.Fatalf("unexpected ws2 message: %#v", got)
	}
}

func TestAuth_JWT_RejectsConcurrentSessionsWithSameSID_SessionEndpoint(t *testing.T) {
	cfg := config.Config{
		AuthMode:  config.AuthModeJWT,
		JWTSecret: "supersecret",
	}
	ts, _ := startSignalingServer(t, cfg)

	now := time.Now().Unix()
	tokenA := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-10)
	tokenB := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-9)

	do := func(token string) *http.Response {
		req, err := http.NewRequest(http.MethodPost, ts.URL+"/session", nil)
		if err != nil {
			t.Fatalf("NewRequest: %v", err)
		}
		req.Header.Set("Authorization", "Bearer "+token)
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do: %v", err)
		}
		return resp
	}

	resp := do(tokenA)
	if resp.StatusCode != http.StatusCreated {
		defer resp.Body.Close()
		t.Fatalf("first /session status=%d, want %d", resp.StatusCode, http.StatusCreated)
	}
	body, _ := io.ReadAll(resp.Body)
	_ = resp.Body.Close()
	if strings.TrimSpace(string(body)) == "" {
		t.Fatalf("expected non-empty session id, got %q", string(body))
	}

	resp = do(tokenB)
	if resp.StatusCode != http.StatusConflict {
		defer resp.Body.Close()
		t.Fatalf("second /session status=%d, want %d", resp.StatusCode, http.StatusConflict)
	}
	var errResp struct {
		Code string `json:"code"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&errResp); err != nil {
		resp.Body.Close()
		t.Fatalf("decode error response: %v", err)
	}
	resp.Body.Close()
	if errResp.Code != "session_already_active" {
		t.Fatalf("error code=%q, want %q", errResp.Code, "session_already_active")
	}
}

func TestAuth_JWT_RejectsConcurrentSessionsWithSameSID_SessionEndpoint_HeaderAlias(t *testing.T) {
	cfg := config.Config{
		AuthMode:  config.AuthModeJWT,
		JWTSecret: "supersecret",
	}
	ts, _ := startSignalingServer(t, cfg)

	now := time.Now().Unix()
	tokenA := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-10)
	tokenB := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-9)

	// First session via the canonical Authorization header.
	{
		req, err := http.NewRequest(http.MethodPost, ts.URL+"/session", nil)
		if err != nil {
			t.Fatalf("NewRequest: %v", err)
		}
		req.Header.Set("Authorization", "Bearer "+tokenA)
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do: %v", err)
		}
		resp.Body.Close()
		if resp.StatusCode != http.StatusCreated {
			t.Fatalf("first /session status=%d, want %d", resp.StatusCode, http.StatusCreated)
		}
	}

	// Second session attempt with the api key header alias (forward/compat for
	// mode-agnostic clients). This must still be rejected based on sid.
	{
		req, err := http.NewRequest(http.MethodPost, ts.URL+"/session", nil)
		if err != nil {
			t.Fatalf("NewRequest: %v", err)
		}
		req.Header.Set("X-API-Key", tokenB)
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do: %v", err)
		}
		defer resp.Body.Close()

		if resp.StatusCode != http.StatusConflict {
			t.Fatalf("second /session status=%d, want %d", resp.StatusCode, http.StatusConflict)
		}
		var errResp struct {
			Code string `json:"code"`
		}
		if err := json.NewDecoder(resp.Body).Decode(&errResp); err != nil {
			t.Fatalf("decode error response: %v", err)
		}
		if errResp.Code != "session_already_active" {
			t.Fatalf("error code=%q, want %q", errResp.Code, "session_already_active")
		}
	}
}

func TestAuth_JWT_RejectsConcurrentSessionsWithSameSID_SessionEndpoint_QueryAlias(t *testing.T) {
	cfg := config.Config{
		AuthMode:  config.AuthModeJWT,
		JWTSecret: "supersecret",
	}
	ts, _ := startSignalingServer(t, cfg)

	now := time.Now().Unix()
	tokenA := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-10)
	tokenB := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-9)

	// First session via the canonical query parameter.
	{
		resp, err := http.Post(ts.URL+"/session?token="+tokenA, "application/json", nil)
		if err != nil {
			t.Fatalf("post: %v", err)
		}
		resp.Body.Close()
		if resp.StatusCode != http.StatusCreated {
			t.Fatalf("first /session status=%d, want %d", resp.StatusCode, http.StatusCreated)
		}
	}

	// Second session attempt with the apiKey query alias. This must still be
	// rejected based on sid.
	{
		resp, err := http.Post(ts.URL+"/session?apiKey="+tokenB, "application/json", nil)
		if err != nil {
			t.Fatalf("post: %v", err)
		}
		defer resp.Body.Close()

		if resp.StatusCode != http.StatusConflict {
			t.Fatalf("second /session status=%d, want %d", resp.StatusCode, http.StatusConflict)
		}
		var errResp struct {
			Code string `json:"code"`
		}
		if err := json.NewDecoder(resp.Body).Decode(&errResp); err != nil {
			t.Fatalf("decode error response: %v", err)
		}
		if errResp.Code != "session_already_active" {
			t.Fatalf("error code=%q, want %q", errResp.Code, "session_already_active")
		}
	}
}

func TestAuth_JWT_AllowsConcurrentSessionsWithDifferentSID_SessionEndpoint(t *testing.T) {
	cfg := config.Config{
		AuthMode:    config.AuthModeJWT,
		JWTSecret:   "supersecret",
		MaxSessions: 2,
	}
	ts, _ := startSignalingServer(t, cfg)

	now := time.Now().Unix()
	tokenA := makeJWTWithIat(cfg.JWTSecret, "sess_test_a", now-10)
	tokenB := makeJWTWithIat(cfg.JWTSecret, "sess_test_b", now-9)

	do := func(token string) (int, string) {
		req, err := http.NewRequest(http.MethodPost, ts.URL+"/session", nil)
		if err != nil {
			t.Fatalf("NewRequest: %v", err)
		}
		req.Header.Set("Authorization", "Bearer "+token)
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do: %v", err)
		}
		defer resp.Body.Close()
		body, _ := io.ReadAll(resp.Body)
		return resp.StatusCode, strings.TrimSpace(string(body))
	}

	statusA, idA := do(tokenA)
	if statusA != http.StatusCreated {
		t.Fatalf("first /session status=%d, want %d", statusA, http.StatusCreated)
	}
	if idA == "" {
		t.Fatalf("expected non-empty session id for sid A")
	}

	statusB, idB := do(tokenB)
	if statusB != http.StatusCreated {
		t.Fatalf("second /session status=%d, want %d", statusB, http.StatusCreated)
	}
	if idB == "" {
		t.Fatalf("expected non-empty session id for sid B")
	}
	if idA == idB {
		t.Fatalf("expected distinct session IDs, got %q", idA)
	}
}

func TestAuth_JWT_ReleasesSIDAfterSessionPreallocTTLExpires(t *testing.T) {
	cfg := config.Config{
		AuthMode:           config.AuthModeJWT,
		JWTSecret:          "supersecret",
		SessionPreallocTTL: 50 * time.Millisecond,
	}
	ts, _ := startSignalingServer(t, cfg)

	now := time.Now().Unix()
	tokenA := makeJWTWithIat(cfg.JWTSecret, "sess_test_prealloc_ttl", now-10)
	tokenB := makeJWTWithIat(cfg.JWTSecret, "sess_test_prealloc_ttl", now-9)

	do := func(token string) (status int, text string, errCode string) {
		req, err := http.NewRequest(http.MethodPost, ts.URL+"/session", nil)
		if err != nil {
			t.Fatalf("NewRequest: %v", err)
		}
		req.Header.Set("Authorization", "Bearer "+token)
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do: %v", err)
		}
		defer resp.Body.Close()
		body, _ := io.ReadAll(resp.Body)
		text = strings.TrimSpace(string(body))

		if resp.StatusCode != http.StatusCreated {
			var errResp struct {
				Code string `json:"code"`
			}
			_ = json.Unmarshal(body, &errResp)
			errCode = errResp.Code
		}

		return resp.StatusCode, text, errCode
	}

	firstStatus, firstID, _ := do(tokenA)
	if firstStatus != http.StatusCreated {
		t.Fatalf("first /session status=%d, want %d", firstStatus, http.StatusCreated)
	}
	if firstID == "" {
		t.Fatalf("expected non-empty session id")
	}

	secondStatus, _, secondCode := do(tokenB)
	if secondStatus != http.StatusConflict {
		t.Fatalf("second /session status=%d, want %d", secondStatus, http.StatusConflict)
	}
	if secondCode != "session_already_active" {
		t.Fatalf("second /session code=%q, want %q", secondCode, "session_already_active")
	}

	var thirdStatus int
	var thirdID string
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		thirdStatus, thirdID, _ = do(tokenB)
		if thirdStatus == http.StatusCreated {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}

	if thirdStatus != http.StatusCreated {
		t.Fatalf("third /session status=%d, want %d (after TTL expiry)", thirdStatus, http.StatusCreated)
	}
	if thirdID == "" {
		t.Fatalf("expected non-empty session id after TTL expiry")
	}
}

func TestAuth_JWT_AllowsConcurrentSessionsWithDifferentSID_HTTPOfferEndpoints(t *testing.T) {
	cfg := config.Config{
		AuthMode:    config.AuthModeJWT,
		JWTSecret:   "supersecret",
		MaxSessions: 2,
	}

	api := newTestWebRTCAPI(t)
	offerSDP := newOfferSDP(t, api)
	now := time.Now().Unix()
	tokenA := makeJWTWithIat(cfg.JWTSecret, "sess_test_a", now-10)
	tokenB := makeJWTWithIat(cfg.JWTSecret, "sess_test_b", now-9)

	prealloc := func(t *testing.T, ts *httptest.Server, token string) {
		t.Helper()

		req, err := http.NewRequest(http.MethodPost, ts.URL+"/session", nil)
		if err != nil {
			t.Fatalf("NewRequest(prealloc): %v", err)
		}
		req.Header.Set("Authorization", "Bearer "+token)
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do(prealloc): %v", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusCreated {
			t.Fatalf("prealloc /session status=%d, want %d", resp.StatusCode, http.StatusCreated)
		}
	}

	t.Run("POST /webrtc/offer", func(t *testing.T) {
		ts, _ := startSignalingServer(t, cfg)
		prealloc(t, ts, tokenA)

		body, err := json.Marshal(map[string]any{"sdp": offerSDP})
		if err != nil {
			t.Fatalf("marshal offer: %v", err)
		}
		req, err := http.NewRequest(http.MethodPost, ts.URL+"/webrtc/offer", bytes.NewReader(body))
		if err != nil {
			t.Fatalf("NewRequest: %v", err)
		}
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("Authorization", "Bearer "+tokenB)

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do: %v", err)
		}
		defer resp.Body.Close()

		if resp.StatusCode != http.StatusOK {
			t.Fatalf("/webrtc/offer status=%d, want %d", resp.StatusCode, http.StatusOK)
		}
	})

	t.Run("POST /offer", func(t *testing.T) {
		ts, _ := startSignalingServer(t, cfg)
		prealloc(t, ts, tokenA)

		body, err := json.Marshal(legacyOfferRequest{Version: 1, Offer: legacySessionDescription(offerSDP)})
		if err != nil {
			t.Fatalf("marshal offer: %v", err)
		}
		req, err := http.NewRequest(http.MethodPost, ts.URL+"/offer", bytes.NewReader(body))
		if err != nil {
			t.Fatalf("NewRequest: %v", err)
		}
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("Authorization", "Bearer "+tokenB)

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do: %v", err)
		}
		defer resp.Body.Close()

		if resp.StatusCode != http.StatusOK {
			t.Fatalf("/offer status=%d, want %d", resp.StatusCode, http.StatusOK)
		}
	})
}

func TestAuth_JWT_RejectsConcurrentSessionsWithSameSID_HTTPOfferEndpoints(t *testing.T) {
	cfg := config.Config{
		AuthMode:  config.AuthModeJWT,
		JWTSecret: "supersecret",
	}
	ts, _ := startSignalingServer(t, cfg)

	now := time.Now().Unix()
	tokenA := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-10)
	tokenB := makeJWTWithIat(cfg.JWTSecret, "sess_test", now-9)

	// Create an active session (via preallocation) so offer endpoints must reject
	// concurrent session creation with the same JWT sid.
	{
		req, err := http.NewRequest(http.MethodPost, ts.URL+"/session", nil)
		if err != nil {
			t.Fatalf("NewRequest: %v", err)
		}
		req.Header.Set("Authorization", "Bearer "+tokenA)
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do: %v", err)
		}
		if resp.StatusCode != http.StatusCreated {
			defer resp.Body.Close()
			t.Fatalf("prealloc /session status=%d, want %d", resp.StatusCode, http.StatusCreated)
		}
		resp.Body.Close()
	}

	type errResp struct {
		Code string `json:"code"`
	}

	t.Run("POST /webrtc/offer", func(t *testing.T) {
		body, err := json.Marshal(map[string]any{
			"sdp": sdp{Type: "offer", SDP: "v=0"},
		})
		if err != nil {
			t.Fatalf("marshal offer: %v", err)
		}

		req, err := http.NewRequest(http.MethodPost, ts.URL+"/webrtc/offer", bytes.NewReader(body))
		if err != nil {
			t.Fatalf("NewRequest: %v", err)
		}
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("Authorization", "Bearer "+tokenB)

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do: %v", err)
		}
		defer resp.Body.Close()

		if resp.StatusCode != http.StatusConflict {
			t.Fatalf("/webrtc/offer status=%d, want %d", resp.StatusCode, http.StatusConflict)
		}
		var got errResp
		if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
			t.Fatalf("decode error response: %v", err)
		}
		if got.Code != "session_already_active" {
			t.Fatalf("error code=%q, want %q", got.Code, "session_already_active")
		}
	})

	t.Run("POST /webrtc/offer query apiKey alias", func(t *testing.T) {
		body, err := json.Marshal(map[string]any{
			"sdp": sdp{Type: "offer", SDP: "v=0"},
		})
		if err != nil {
			t.Fatalf("marshal offer: %v", err)
		}

		req, err := http.NewRequest(http.MethodPost, ts.URL+"/webrtc/offer?apiKey="+tokenB, bytes.NewReader(body))
		if err != nil {
			t.Fatalf("NewRequest: %v", err)
		}
		req.Header.Set("Content-Type", "application/json")

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do: %v", err)
		}
		defer resp.Body.Close()

		if resp.StatusCode != http.StatusConflict {
			t.Fatalf("/webrtc/offer status=%d, want %d", resp.StatusCode, http.StatusConflict)
		}
		var got errResp
		if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
			t.Fatalf("decode error response: %v", err)
		}
		if got.Code != "session_already_active" {
			t.Fatalf("error code=%q, want %q", got.Code, "session_already_active")
		}
	})

	t.Run("POST /offer", func(t *testing.T) {
		body, err := json.Marshal(legacyOfferRequest{Version: 1, Offer: legacySessionDescription{Type: "offer", SDP: "v=0"}})
		if err != nil {
			t.Fatalf("marshal offer: %v", err)
		}

		req, err := http.NewRequest(http.MethodPost, ts.URL+"/offer", bytes.NewReader(body))
		if err != nil {
			t.Fatalf("NewRequest: %v", err)
		}
		req.Header.Set("Content-Type", "application/json")
		req.Header.Set("Authorization", "Bearer "+tokenB)

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do: %v", err)
		}
		defer resp.Body.Close()

		if resp.StatusCode != http.StatusConflict {
			t.Fatalf("/offer status=%d, want %d", resp.StatusCode, http.StatusConflict)
		}
		var got errResp
		if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
			t.Fatalf("decode error response: %v", err)
		}
		if got.Code != "session_already_active" {
			t.Fatalf("error code=%q, want %q", got.Code, "session_already_active")
		}
	})

	t.Run("POST /offer query apiKey alias", func(t *testing.T) {
		body, err := json.Marshal(legacyOfferRequest{Version: 1, Offer: legacySessionDescription{Type: "offer", SDP: "v=0"}})
		if err != nil {
			t.Fatalf("marshal offer: %v", err)
		}

		req, err := http.NewRequest(http.MethodPost, ts.URL+"/offer?apiKey="+tokenB, bytes.NewReader(body))
		if err != nil {
			t.Fatalf("NewRequest: %v", err)
		}
		req.Header.Set("Content-Type", "application/json")

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("Do: %v", err)
		}
		defer resp.Body.Close()

		if resp.StatusCode != http.StatusConflict {
			t.Fatalf("/offer status=%d, want %d", resp.StatusCode, http.StatusConflict)
		}
		var got errResp
		if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
			t.Fatalf("decode error response: %v", err)
		}
		if got.Code != "session_already_active" {
			t.Fatalf("error code=%q, want %q", got.Code, "session_already_active")
		}
	})
}

func TestAuth_SessionEndpoint_AllowsMultipleSessionsForNoneAndAPIKey(t *testing.T) {
	t.Run("none", func(t *testing.T) {
		ts, _ := startSignalingServer(t, config.Config{AuthMode: config.AuthModeNone})

		resp, err := http.Post(ts.URL+"/session", "application/json", nil)
		if err != nil {
			t.Fatalf("post: %v", err)
		}
		if resp.StatusCode != http.StatusCreated {
			resp.Body.Close()
			t.Fatalf("first /session status=%d, want %d", resp.StatusCode, http.StatusCreated)
		}
		resp.Body.Close()

		resp, err = http.Post(ts.URL+"/session", "application/json", nil)
		if err != nil {
			t.Fatalf("post: %v", err)
		}
		if resp.StatusCode != http.StatusCreated {
			resp.Body.Close()
			t.Fatalf("second /session status=%d, want %d", resp.StatusCode, http.StatusCreated)
		}
		resp.Body.Close()
	})

	t.Run("api_key", func(t *testing.T) {
		cfg := config.Config{
			AuthMode: config.AuthModeAPIKey,
			APIKey:   "secret",
		}
		ts, _ := startSignalingServer(t, cfg)

		do := func() *http.Response {
			req, err := http.NewRequest(http.MethodPost, ts.URL+"/session", nil)
			if err != nil {
				t.Fatalf("NewRequest: %v", err)
			}
			req.Header.Set("X-API-Key", cfg.APIKey)
			resp, err := http.DefaultClient.Do(req)
			if err != nil {
				t.Fatalf("Do: %v", err)
			}
			return resp
		}

		resp := do()
		if resp.StatusCode != http.StatusCreated {
			resp.Body.Close()
			t.Fatalf("first /session status=%d, want %d", resp.StatusCode, http.StatusCreated)
		}
		resp.Body.Close()

		resp = do()
		if resp.StatusCode != http.StatusCreated {
			resp.Body.Close()
			t.Fatalf("second /session status=%d, want %d", resp.StatusCode, http.StatusCreated)
		}
		resp.Body.Close()
	})
}

func TestAuth_APIKey_Offer(t *testing.T) {
	cfg := config.Config{
		AuthMode: config.AuthModeAPIKey,
		APIKey:   "secret",
	}
	ts, m := startSignalingServer(t, cfg)

	body, err := json.Marshal(legacyOfferRequest{Version: 1, Offer: legacySessionDescription{Type: "offer", SDP: "v=0"}})
	if err != nil {
		t.Fatalf("marshal offer: %v", err)
	}

	do := func(apiKey string) *http.Response {
		req, err := http.NewRequest(http.MethodPost, ts.URL+"/offer", bytes.NewReader(body))
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
	// We used a dummy SDP, so we expect the relay to reject it with 400. This
	// confirms auth was accepted and we reached SDP parsing.
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("good api key status=%d, want %d", resp.StatusCode, http.StatusBadRequest)
	}
	_ = resp.Body.Close()
}

func TestAuth_JWT_Offer(t *testing.T) {
	cfg := config.Config{
		AuthMode:  config.AuthModeJWT,
		JWTSecret: "supersecret",
	}
	ts, m := startSignalingServer(t, cfg)

	body, err := json.Marshal(legacyOfferRequest{Version: 1, Offer: legacySessionDescription{Type: "offer", SDP: "v=0"}})
	if err != nil {
		t.Fatalf("marshal offer: %v", err)
	}

	do := func(token string) *http.Response {
		req, err := http.NewRequest(http.MethodPost, ts.URL+"/offer", bytes.NewReader(body))
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

	if got := m.Get(metrics.AuthFailure); got < 2 {
		t.Fatalf("auth failure metric=%d, want >= 2", got)
	}

	resp = do(makeJWT(cfg.JWTSecret, "sess_test"))
	// We used a dummy SDP, so we expect the relay to reject it with 400. This
	// confirms auth was accepted and we reached SDP parsing.
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("good token status=%d, want %d", resp.StatusCode, http.StatusBadRequest)
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

	if err := c.WriteJSON(signalMessage{Type: "auth", APIKey: "secret"}); err != nil {
		t.Fatalf("write auth: %v", err)
	}
	if err := c.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer: %v", err)
	}

	_ = c.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	got, err := parseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if got.Type != "answer" {
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

	token := makeJWT(cfg.JWTSecret, "sess_test")
	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal?token=" + token
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })

	if err := c.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer: %v", err)
	}

	_ = c.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	got, err := parseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if got.Type != "answer" {
		t.Fatalf("unexpected message: %#v", got)
	}
}

func TestAuth_JWT_WebSocketSignal_FirstMessageAuth_RejectsConcurrentSessions(t *testing.T) {
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

	token := makeJWT(cfg.JWTSecret, "sess_test")
	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal"

	ws1, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial ws1: %v", err)
	}
	t.Cleanup(func() { _ = ws1.Close() })

	if err := ws1.WriteJSON(signalMessage{Type: "auth", Token: token}); err != nil {
		t.Fatalf("write auth ws1: %v", err)
	}
	if err := ws1.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer ws1: %v", err)
	}

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

	ws2, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial ws2: %v", err)
	}
	t.Cleanup(func() { _ = ws2.Close() })

	if err := ws2.WriteJSON(signalMessage{Type: "auth", Token: token}); err != nil {
		t.Fatalf("write auth ws2: %v", err)
	}
	if err := ws2.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer ws2: %v", err)
	}

	_ = ws2.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err = ws2.ReadMessage()
	if err != nil {
		t.Fatalf("read ws2: %v", err)
	}
	got, err = parseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse ws2: %v", err)
	}
	if got.Type != "error" || got.Code != "session_already_active" {
		t.Fatalf("unexpected ws2 message: %#v", got)
	}
}

func TestAuth_APIKey_WebSocketSignal_QueryTokenAlias(t *testing.T) {
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

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal?token=secret"
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })

	if err := c.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer: %v", err)
	}

	_ = c.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	got, err := parseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if got.Type != "answer" {
		t.Fatalf("unexpected message: %#v", got)
	}
}

func TestAuth_JWT_WebSocketSignal_QueryAPIKeyAlias(t *testing.T) {
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

	token := makeJWT(cfg.JWTSecret, "sess_test")
	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal?apiKey=" + token
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })

	if err := c.WriteJSON(signalMessage{Type: "offer", SDP: &offerSDP}); err != nil {
		t.Fatalf("write offer: %v", err)
	}

	_ = c.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	got, err := parseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if got.Type != "answer" {
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
	parsed, err := parseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if parsed.Type != "error" || parsed.Code != "bad_message" {
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

func TestWebSocketRejectsOfferBeforeAuth(t *testing.T) {
	cfg := config.Config{
		AuthMode:                      config.AuthModeAPIKey,
		APIKey:                        "secret",
		SignalingAuthTimeout:          2 * time.Second,
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

	offer := sdp{Type: "offer", SDP: "v=0"}
	if err := c.WriteJSON(signalMessage{Type: "offer", SDP: &offer}); err != nil {
		t.Fatalf("write offer: %v", err)
	}

	_ = c.SetReadDeadline(time.Now().Add(2 * time.Second))
	_, msg, err := c.ReadMessage()
	if err != nil {
		if websocket.IsCloseError(err, websocket.ClosePolicyViolation) {
			if m.Get(metrics.AuthFailure) == 0 {
				t.Fatalf("expected auth_failure metric increment")
			}
			return
		}
		t.Fatalf("read: %v", err)
	}
	parsed, err := parseSignalMessage(msg)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if parsed.Type != "error" || parsed.Code != "unauthorized" {
		t.Fatalf("unexpected server message: %#v", parsed)
	}
	if m.Get(metrics.AuthFailure) == 0 {
		t.Fatalf("expected auth_failure metric increment")
	}

	_, _, err = c.ReadMessage()
	if err == nil {
		t.Fatalf("expected close error")
	}
	if !websocket.IsCloseError(err, websocket.ClosePolicyViolation) {
		t.Fatalf("expected policy violation close; got %v", err)
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
	if err := c.WriteJSON(signalMessage{Type: "auth", APIKey: "ignored"}); err != nil {
		t.Fatalf("write auth1: %v", err)
	}
	if err := c.WriteJSON(signalMessage{Type: "auth", APIKey: "ignored"}); err != nil {
		t.Fatalf("write auth2: %v", err)
	}

	_ = c.SetReadDeadline(time.Now().Add(2 * time.Second))
	_, raw, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	parsed, err := parseSignalMessage(raw)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if parsed.Type != "error" || parsed.Code != "rate_limited" {
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

func TestWebSocketSignal_RejectsCrossOrigin(t *testing.T) {
	cfg := config.Config{
		AuthMode: config.AuthModeNone,
	}
	ts, _ := startSignalingServer(t, cfg)

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal"
	headers := http.Header{}
	headers.Set("Origin", "https://evil.example.com")

	_, resp, err := websocket.DefaultDialer.Dial(wsURL, headers)
	if resp != nil && resp.Body != nil {
		resp.Body.Close()
	}
	if err == nil {
		t.Fatalf("expected dial error")
	}
	if resp == nil || resp.StatusCode != http.StatusForbidden {
		status := 0
		if resp != nil {
			status = resp.StatusCode
		}
		t.Fatalf("status=%d, want %d (err=%v)", status, http.StatusForbidden, err)
	}
}
