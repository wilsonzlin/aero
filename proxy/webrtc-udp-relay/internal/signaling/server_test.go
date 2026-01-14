package signaling

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gorilla/websocket"
	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/auth"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
)

func decodeHTTPErrorResponse(t *testing.T, resp *http.Response) httpErrorResponse {
	t.Helper()

	defer resp.Body.Close()
	var out httpErrorResponse
	if err := json.NewDecoder(resp.Body).Decode(&out); err != nil {
		t.Fatalf("decode: %v", err)
	}
	return out
}

func TestServer_EnforcesMaxSessions(t *testing.T) {
	cfg := config.Config{MaxSessions: 1}
	m := metrics.New()
	sm := relay.NewSessionManager(cfg, m, nil)

	mediaEngine := &webrtc.MediaEngine{}
	if err := mediaEngine.RegisterDefaultCodecs(); err != nil {
		t.Fatalf("register codecs: %v", err)
	}
	api := webrtc.NewAPI(webrtc.WithMediaEngine(mediaEngine))

	srv := NewServer(Config{
		Sessions:            sm,
		WebRTC:              api,
		RelayConfig:         relay.DefaultConfig(),
		Policy:              policy.NewDevDestinationPolicy(),
		Authorizer:          allowAllAuthorizer{},
		ICEGatheringTimeout: 2 * time.Second,
	})

	mux := http.NewServeMux()
	srv.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal"
	ws1, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial first websocket: %v", err)
	}
	t.Cleanup(func() { _ = ws1.Close() })

	offerSDP := func() sdp {
		pc, err := api.NewPeerConnection(webrtc.Configuration{})
		if err != nil {
			t.Fatalf("new pc: %v", err)
		}
		defer pc.Close()
		if _, err := pc.CreateDataChannel("udp", nil); err != nil {
			t.Fatalf("create datachannel: %v", err)
		}
		offer, err := pc.CreateOffer(nil)
		if err != nil {
			t.Fatalf("create offer: %v", err)
		}
		if err := pc.SetLocalDescription(offer); err != nil {
			t.Fatalf("set local offer: %v", err)
		}
		local := pc.LocalDescription()
		if local == nil {
			t.Fatalf("missing local description")
		}
		return sdpFromPion(*local)
	}()

	if err := ws1.WriteJSON(signalMessage{Type: messageTypeOffer, SDP: ptr(offerSDP)}); err != nil {
		t.Fatalf("send offer: %v", err)
	}

	ws2, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial second websocket: %v", err)
	}
	t.Cleanup(func() { _ = ws2.Close() })

	if err := ws2.WriteJSON(signalMessage{Type: messageTypeOffer, SDP: ptr(offerSDP)}); err != nil {
		t.Fatalf("send offer ws2: %v", err)
	}

	_ = ws2.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, raw, err := ws2.ReadMessage()
	if err != nil {
		t.Fatalf("receive: %v", err)
	}
	msg, err := parseSignalMessage(raw)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}
	if msg.Type != messageTypeError || msg.Code != "too_many_sessions" {
		t.Fatalf("unexpected message: %#v", msg)
	}

	if m.Get(metrics.DropReasonTooManySessions) == 0 {
		t.Fatalf("expected too_many_sessions metric increment")
	}
}

func TestServer_RejectsCrossOriginHTTPRequests(t *testing.T) {
	srv := NewServer(Config{
		RelayConfig: relay.DefaultConfig(),
		Policy:      policy.NewDevDestinationPolicy(),
		Authorizer:  allowAllAuthorizer{},
	})

	mux := http.NewServeMux()
	srv.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	cases := []struct {
		method string
		path   string
	}{
		{method: http.MethodPost, path: "/offer"},
		{method: http.MethodPost, path: "/session"},
		{method: http.MethodPost, path: "/webrtc/offer"},
	}

	for _, tc := range cases {
		t.Run(tc.method+" "+tc.path, func(t *testing.T) {
			req, err := http.NewRequest(tc.method, ts.URL+tc.path, nil)
			if err != nil {
				t.Fatalf("NewRequest: %v", err)
			}
			req.Header.Set("Origin", "https://evil.example.com")

			resp, err := http.DefaultClient.Do(req)
			if err != nil {
				t.Fatalf("Do: %v", err)
			}
			resp.Body.Close()

			if resp.StatusCode != http.StatusForbidden {
				t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusForbidden)
			}
		})
	}
}

func TestServer_WebSocketUpgradeFailuresReturnJSON(t *testing.T) {
	srv := NewServer(Config{
		WebRTC:         webrtc.NewAPI(),
		RelayConfig:    relay.DefaultConfig(),
		Policy:         policy.NewDevDestinationPolicy(),
		Authorizer:     allowAllAuthorizer{},
		AllowedOrigins: []string{"https://good.example.com"},
	})

	mux := http.NewServeMux()
	srv.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	t.Run("non_websocket_request", func(t *testing.T) {
		resp, err := http.Get(ts.URL + "/webrtc/signal")
		if err != nil {
			t.Fatalf("get: %v", err)
		}
		if resp.StatusCode != http.StatusBadRequest {
			resp.Body.Close()
			t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusBadRequest)
		}
		got := decodeHTTPErrorResponse(t, resp)
		if got.Code != "bad_message" {
			t.Fatalf("code=%q, want %q", got.Code, "bad_message")
		}
	})

	t.Run("forbidden_origin", func(t *testing.T) {
		wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal"
		headers := http.Header{}
		headers.Set("Origin", "https://evil.example.com")

		_, resp, err := websocket.DefaultDialer.Dial(wsURL, headers)
		if err == nil {
			t.Fatalf("expected dial to fail")
		}
		if resp == nil {
			t.Fatalf("expected an HTTP response on handshake failure")
		}
		if resp.StatusCode != http.StatusForbidden {
			resp.Body.Close()
			t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusForbidden)
		}
		got := decodeHTTPErrorResponse(t, resp)
		if got.Code != "forbidden" {
			t.Fatalf("code=%q, want %q", got.Code, "forbidden")
		}
	})
}

type failingAuthorizer struct{}

func (failingAuthorizer) Authorize(r *http.Request, firstMsg *clientHello) (authResult, error) {
	return authResult{}, errors.New("boom")
}

func TestServer_WebSocketInternalAuthErrorCloses1011(t *testing.T) {
	cfg := config.Config{}
	m := metrics.New()
	sm := relay.NewSessionManager(cfg, m, nil)

	srv := NewServer(Config{
		Sessions:    sm,
		WebRTC:      webrtc.NewAPI(),
		RelayConfig: relay.DefaultConfig(),
		Policy:      policy.NewDevDestinationPolicy(),
		Authorizer:  failingAuthorizer{},
	})

	mux := http.NewServeMux()
	srv.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal"
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })

	deadline := time.Now().Add(2 * time.Second)
	for {
		_ = c.SetReadDeadline(deadline)
		_, msg, err := c.ReadMessage()
		if err != nil {
			if !websocket.IsCloseError(err, websocket.CloseInternalServerErr) {
				t.Fatalf("expected internal close; got %v", err)
			}
			break
		}
		parsed, parseErr := parseSignalMessage(msg)
		if parseErr == nil {
			if parsed.Type != messageTypeError || parsed.Code != "internal_error" {
				t.Fatalf("unexpected message: %#v", parsed)
			}
		}
	}

	if got := m.Get(metrics.AuthFailure); got != 0 {
		t.Fatalf("auth_failure=%d, want 0", got)
	}
}

func TestServer_HTTPInternalAuthErrorReturns500(t *testing.T) {
	cfg := config.Config{}
	m := metrics.New()
	sm := relay.NewSessionManager(cfg, m, nil)

	srv := NewServer(Config{
		Sessions:    sm,
		WebRTC:      webrtc.NewAPI(),
		RelayConfig: relay.DefaultConfig(),
		Policy:      policy.NewDevDestinationPolicy(),
		Authorizer:  failingAuthorizer{},
	})

	mux := http.NewServeMux()
	srv.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	t.Run("session", func(t *testing.T) {
		resp, err := http.Post(ts.URL+"/session", "application/json", nil)
		if err != nil {
			t.Fatalf("post: %v", err)
		}
		if resp.StatusCode != http.StatusInternalServerError {
			t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusInternalServerError)
		}
		got := decodeHTTPErrorResponse(t, resp)
		if got.Code != "internal_error" {
			t.Fatalf("code=%q, want %q", got.Code, "internal_error")
		}
	})

	t.Run("offer", func(t *testing.T) {
		body, err := json.Marshal(offerRequest{
			Version: version1,
			Offer: sessionDescription{
				Type: "offer",
				SDP:  "v=0",
			},
		})
		if err != nil {
			t.Fatalf("marshal: %v", err)
		}
		resp, err := http.Post(ts.URL+"/offer", "application/json", bytes.NewReader(body))
		if err != nil {
			t.Fatalf("post: %v", err)
		}
		if resp.StatusCode != http.StatusInternalServerError {
			t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusInternalServerError)
		}
		got := decodeHTTPErrorResponse(t, resp)
		if got.Code != "internal_error" {
			t.Fatalf("code=%q, want %q", got.Code, "internal_error")
		}
	})

	t.Run("webrtc_offer", func(t *testing.T) {
		body, err := json.Marshal(httpOfferRequest{
			SDP: sdp{Type: "offer", SDP: "v=0"},
		})
		if err != nil {
			t.Fatalf("marshal: %v", err)
		}
		resp, err := http.Post(ts.URL+"/webrtc/offer", "application/json", bytes.NewReader(body))
		if err != nil {
			t.Fatalf("post: %v", err)
		}
		if resp.StatusCode != http.StatusInternalServerError {
			t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusInternalServerError)
		}
		got := decodeHTTPErrorResponse(t, resp)
		if got.Code != "internal_error" {
			t.Fatalf("code=%q, want %q", got.Code, "internal_error")
		}
	})

	if got := m.Get(metrics.AuthFailure); got != 0 {
		t.Fatalf("auth_failure=%d, want 0", got)
	}
}

func TestServer_Offer_ICEGatheringTimeoutReturnsAnswer(t *testing.T) {
	// Force GatheringCompletePromise to never resolve so the test is deterministic.
	oldPromise := gatheringCompletePromise
	gatheringCompletePromise = func(*webrtc.PeerConnection) <-chan struct{} {
		return make(chan struct{})
	}
	t.Cleanup(func() { gatheringCompletePromise = oldPromise })

	api := webrtc.NewAPI()
	cfg := config.Config{MaxSessions: 1}
	m := metrics.New()
	sm := relay.NewSessionManager(cfg, m, nil)

	srv := NewServer(Config{
		Sessions:            sm,
		WebRTC:              api,
		RelayConfig:         relay.DefaultConfig(),
		Policy:              policy.NewDevDestinationPolicy(),
		Authorizer:          allowAllAuthorizer{},
		ICEGatheringTimeout: 1 * time.Millisecond,
	})

	mux := http.NewServeMux()
	srv.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	t.Cleanup(func() {
		srv.Close()
		ts.Close()
	})

	pc, err := api.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peerconnection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := false
	maxRetransmits := uint16(0)
	if _, err := pc.CreateDataChannel("udp", &webrtc.DataChannelInit{Ordered: &ordered, MaxRetransmits: &maxRetransmits}); err != nil {
		t.Fatalf("create datachannel: %v", err)
	}

	offer, err := pc.CreateOffer(nil)
	if err != nil {
		t.Fatalf("create offer: %v", err)
	}
	if err := pc.SetLocalDescription(offer); err != nil {
		t.Fatalf("set local description: %v", err)
	}
	local := pc.LocalDescription()
	if local == nil {
		t.Fatalf("missing local description")
	}

	body, err := json.Marshal(offerRequest{
		Version: version1,
		Offer: sessionDescription{
			Type: "offer",
			SDP:  local.SDP,
		},
	})
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}

	client := &http.Client{Timeout: 250 * time.Millisecond}
	start := time.Now()
	resp, err := client.Post(ts.URL+"/offer", "application/json", bytes.NewReader(body))
	elapsed := time.Since(start)
	if err != nil {
		t.Fatalf("post: %v (elapsed=%s)", err, elapsed)
	}
	t.Cleanup(func() { _ = resp.Body.Close() })

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusOK)
	}

	var got answerResponse
	if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if err := got.Validate(); err != nil {
		t.Fatalf("invalid answer: %v", err)
	}

	if elapsed > 250*time.Millisecond {
		t.Fatalf("request took too long: %s", elapsed)
	}

	if got := m.Get(metrics.ICEGatheringTimeout); got != 1 {
		t.Fatalf("%s=%d, want 1", metrics.ICEGatheringTimeout, got)
	}
}

func TestServer_WebRTCOffer_ICEGatheringTimeoutReturnsAnswer(t *testing.T) {
	// Force GatheringCompletePromise to never resolve so the test is deterministic.
	oldPromise := gatheringCompletePromise
	gatheringCompletePromise = func(*webrtc.PeerConnection) <-chan struct{} {
		return make(chan struct{})
	}
	t.Cleanup(func() { gatheringCompletePromise = oldPromise })

	api := webrtc.NewAPI()
	cfg := config.Config{MaxSessions: 1}
	m := metrics.New()
	sm := relay.NewSessionManager(cfg, m, nil)

	srv := NewServer(Config{
		Sessions:            sm,
		WebRTC:              api,
		RelayConfig:         relay.DefaultConfig(),
		Policy:              policy.NewDevDestinationPolicy(),
		Authorizer:          allowAllAuthorizer{},
		ICEGatheringTimeout: 1 * time.Millisecond,
	})

	mux := http.NewServeMux()
	srv.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	t.Cleanup(func() {
		srv.Close()
		ts.Close()
	})

	pc, err := api.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peerconnection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := false
	maxRetransmits := uint16(0)
	if _, err := pc.CreateDataChannel("udp", &webrtc.DataChannelInit{Ordered: &ordered, MaxRetransmits: &maxRetransmits}); err != nil {
		t.Fatalf("create datachannel: %v", err)
	}

	offer, err := pc.CreateOffer(nil)
	if err != nil {
		t.Fatalf("create offer: %v", err)
	}
	if err := pc.SetLocalDescription(offer); err != nil {
		t.Fatalf("set local description: %v", err)
	}
	local := pc.LocalDescription()
	if local == nil {
		t.Fatalf("missing local description")
	}

	body, err := json.Marshal(httpOfferRequest{SDP: sdpFromPion(*local)})
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}

	client := &http.Client{Timeout: 250 * time.Millisecond}
	start := time.Now()
	resp, err := client.Post(ts.URL+"/webrtc/offer", "application/json", bytes.NewReader(body))
	elapsed := time.Since(start)
	if err != nil {
		t.Fatalf("post: %v (elapsed=%s)", err, elapsed)
	}
	t.Cleanup(func() { _ = resp.Body.Close() })

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusOK)
	}

	var got httpOfferResponse
	if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
		t.Fatalf("decode: %v", err)
	}
	if strings.TrimSpace(got.SessionID) == "" {
		t.Fatalf("expected non-empty sessionId")
	}
	if got.SDP.Type != "answer" {
		t.Fatalf("unexpected sdp.type=%q", got.SDP.Type)
	}
	if strings.TrimSpace(got.SDP.SDP) == "" {
		t.Fatalf("expected non-empty answer sdp")
	}
	if elapsed > 250*time.Millisecond {
		t.Fatalf("request took too long: %s", elapsed)
	}

	if got := m.Get(metrics.ICEGatheringTimeout); got != 1 {
		t.Fatalf("%s=%d, want 1", metrics.ICEGatheringTimeout, got)
	}
}

func TestServer_WebRTCOffer_CanceledRequestClosesSession(t *testing.T) {
	cfg := config.Config{MaxSessions: 1}
	m := metrics.New()
	sm := relay.NewSessionManager(cfg, m, nil)

	// Force GatheringCompletePromise to never resolve so the handler blocks until
	// the request is canceled. This makes the test deterministic and ensures the
	// session is actually created.
	oldPromise := gatheringCompletePromise
	started := make(chan struct{})
	var startedOnce sync.Once
	gatheringCompletePromise = func(*webrtc.PeerConnection) <-chan struct{} {
		startedOnce.Do(func() { close(started) })
		return make(chan struct{})
	}
	t.Cleanup(func() { gatheringCompletePromise = oldPromise })

	api := webrtc.NewAPI()

	srv := NewServer(Config{
		Sessions:            sm,
		WebRTC:              api,
		RelayConfig:         relay.DefaultConfig(),
		Policy:              policy.NewDevDestinationPolicy(),
		Authorizer:          allowAllAuthorizer{},
		ICEGatheringTimeout: 30 * time.Second,
	})

	mux := http.NewServeMux()
	srv.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	t.Cleanup(func() {
		srv.Close()
		ts.Close()
	})

	// Create a valid offer SDP.
	pc, err := api.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peerconnection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := false
	maxRetransmits := uint16(0)
	if _, err := pc.CreateDataChannel("udp", &webrtc.DataChannelInit{Ordered: &ordered, MaxRetransmits: &maxRetransmits}); err != nil {
		t.Fatalf("create datachannel: %v", err)
	}
	offer, err := pc.CreateOffer(nil)
	if err != nil {
		t.Fatalf("create offer: %v", err)
	}
	if err := pc.SetLocalDescription(offer); err != nil {
		t.Fatalf("set local description: %v", err)
	}
	local := pc.LocalDescription()
	if local == nil {
		t.Fatalf("missing local description")
	}

	body, err := json.Marshal(httpOfferRequest{SDP: sdpFromPion(*local)})
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}

	reqCtx, cancel := context.WithCancel(context.Background())
	req, err := http.NewRequestWithContext(reqCtx, http.MethodPost, ts.URL+"/webrtc/offer", bytes.NewReader(body))
	if err != nil {
		t.Fatalf("NewRequest: %v", err)
	}
	req.Header.Set("Content-Type", "application/json")

	done := make(chan error, 1)
	go func() {
		resp, err := http.DefaultClient.Do(req)
		if resp != nil && resp.Body != nil {
			resp.Body.Close()
		}
		done <- err
	}()

	select {
	case <-started:
	case <-time.After(2 * time.Second):
		cancel()
		t.Fatalf("timed out waiting for handler to start ICE gathering wait")
	}

	if got := sm.ActiveSessions(); got != 1 {
		cancel()
		t.Fatalf("active sessions=%d, want 1", got)
	}

	cancel()

	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatalf("timed out waiting for client request to return after cancellation")
	}

	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if sm.ActiveSessions() == 0 {
			if got := m.Get(metrics.ICEGatheringTimeout); got != 0 {
				t.Fatalf("%s=%d, want 0", metrics.ICEGatheringTimeout, got)
			}
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("expected session to be released after cancellation; active=%d", sm.ActiveSessions())
}

func TestServer_Offer_CanceledRequestClosesSession(t *testing.T) {
	cfg := config.Config{MaxSessions: 1}
	m := metrics.New()
	sm := relay.NewSessionManager(cfg, m, nil)

	// Force GatheringCompletePromise to never resolve so the handler blocks until
	// the request is canceled.
	oldPromise := gatheringCompletePromise
	started := make(chan struct{})
	var startedOnce sync.Once
	gatheringCompletePromise = func(*webrtc.PeerConnection) <-chan struct{} {
		startedOnce.Do(func() { close(started) })
		return make(chan struct{})
	}
	t.Cleanup(func() { gatheringCompletePromise = oldPromise })

	api := webrtc.NewAPI()

	srv := NewServer(Config{
		Sessions:            sm,
		WebRTC:              api,
		RelayConfig:         relay.DefaultConfig(),
		Policy:              policy.NewDevDestinationPolicy(),
		Authorizer:          allowAllAuthorizer{},
		ICEGatheringTimeout: 30 * time.Second,
	})

	mux := http.NewServeMux()
	srv.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	t.Cleanup(func() {
		srv.Close()
		ts.Close()
	})

	// Create a valid offer SDP.
	pc, err := api.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peerconnection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := false
	maxRetransmits := uint16(0)
	if _, err := pc.CreateDataChannel("udp", &webrtc.DataChannelInit{Ordered: &ordered, MaxRetransmits: &maxRetransmits}); err != nil {
		t.Fatalf("create datachannel: %v", err)
	}
	offer, err := pc.CreateOffer(nil)
	if err != nil {
		t.Fatalf("create offer: %v", err)
	}
	if err := pc.SetLocalDescription(offer); err != nil {
		t.Fatalf("set local description: %v", err)
	}
	local := pc.LocalDescription()
	if local == nil {
		t.Fatalf("missing local description")
	}

	body, err := json.Marshal(offerRequest{
		Version: version1,
		Offer: sessionDescription{
			Type: "offer",
			SDP:  local.SDP,
		},
	})
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}

	reqCtx, cancel := context.WithCancel(context.Background())
	req, err := http.NewRequestWithContext(reqCtx, http.MethodPost, ts.URL+"/offer", bytes.NewReader(body))
	if err != nil {
		t.Fatalf("NewRequest: %v", err)
	}
	req.Header.Set("Content-Type", "application/json")

	done := make(chan error, 1)
	go func() {
		resp, err := http.DefaultClient.Do(req)
		if resp != nil && resp.Body != nil {
			resp.Body.Close()
		}
		done <- err
	}()

	select {
	case <-started:
	case <-time.After(2 * time.Second):
		cancel()
		t.Fatalf("timed out waiting for handler to start ICE gathering wait")
	}

	if got := sm.ActiveSessions(); got != 1 {
		cancel()
		t.Fatalf("active sessions=%d, want 1", got)
	}

	cancel()

	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatalf("timed out waiting for client request to return after cancellation")
	}

	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if sm.ActiveSessions() == 0 {
			if got := m.Get(metrics.ICEGatheringTimeout); got != 0 {
				t.Fatalf("%s=%d, want 0", metrics.ICEGatheringTimeout, got)
			}
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("expected session to be released after cancellation; active=%d", sm.ActiveSessions())
}

type unauthorizedAuthorizer struct{}

func (unauthorizedAuthorizer) Authorize(r *http.Request, firstMsg *clientHello) (authResult, error) {
	return authResult{}, auth.ErrMissingCredentials
}

func TestServer_SessionEndpoint_RequiresAuth(t *testing.T) {
	cfg := config.Config{}
	m := metrics.New()
	sm := relay.NewSessionManager(cfg, m, nil)

	srv := NewServer(Config{
		Sessions:            sm,
		RelayConfig:         relay.DefaultConfig(),
		Policy:              policy.NewDevDestinationPolicy(),
		Authorizer:          unauthorizedAuthorizer{},
		ICEGatheringTimeout: 2 * time.Second,
	})

	mux := http.NewServeMux()
	srv.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	t.Cleanup(func() {
		srv.Close()
		ts.Close()
	})

	resp, err := http.Post(ts.URL+"/session", "application/json", nil)
	if err != nil {
		t.Fatalf("post: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusUnauthorized {
		t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusUnauthorized)
	}
	if got := m.Get(metrics.AuthFailure); got == 0 {
		t.Fatalf("expected auth_failure metric increment")
	}
	srv.mu.Lock()
	reservationCount := len(srv.preSessions)
	srv.mu.Unlock()
	if reservationCount != 0 {
		t.Fatalf("expected 0 preSessions reservations, got %d", reservationCount)
	}
}

func TestServer_SessionEndpoint_ExpiresAndReleasesSession(t *testing.T) {
	cfg := config.Config{MaxSessions: 1}
	m := metrics.New()
	sm := relay.NewSessionManager(cfg, m, nil)

	preallocTTL := 250 * time.Millisecond
	srv := NewServer(Config{
		Sessions:           sm,
		RelayConfig:        relay.DefaultConfig(),
		Policy:             policy.NewDevDestinationPolicy(),
		Authorizer:         allowAllAuthorizer{},
		SessionPreallocTTL: preallocTTL,
	})

	mux := http.NewServeMux()
	srv.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	t.Cleanup(func() {
		srv.Close()
		ts.Close()
	})

	resp, err := http.Post(ts.URL+"/session", "application/json", nil)
	if err != nil {
		t.Fatalf("post: %v", err)
	}
	if resp.StatusCode != http.StatusCreated {
		_ = resp.Body.Close()
		t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusCreated)
	}
	body, err := io.ReadAll(resp.Body)
	_ = resp.Body.Close()
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	id := strings.TrimSpace(string(body))
	if id == "" {
		t.Fatalf("expected non-empty session id")
	}

	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		srv.mu.Lock()
		reservationCount := len(srv.preSessions)
		srv.mu.Unlock()
		if reservationCount == 0 {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}

	// Ensure the server does not retain stale reservations after expiry.
	srv.mu.Lock()
	reservationCount := len(srv.preSessions)
	srv.mu.Unlock()
	if reservationCount != 0 {
		t.Fatalf("expected 0 preSessions reservations after expiry, got %d", reservationCount)
	}

	// With max sessions set to 1, a new /session request proves the session quota
	// was released after expiry.
	resp2, err := http.Post(ts.URL+"/session", "application/json", nil)
	if err != nil {
		t.Fatalf("post second /session: %v", err)
	}
	_ = resp2.Body.Close()
	if resp2.StatusCode != http.StatusCreated {
		t.Fatalf("second /session status=%d, want %d", resp2.StatusCode, http.StatusCreated)
	}
}

func TestServer_Close_ClosesPreallocatedSessions(t *testing.T) {
	cfg := config.Config{MaxSessions: 1}
	m := metrics.New()
	sm := relay.NewSessionManager(cfg, m, nil)

	srv := NewServer(Config{
		Sessions:           sm,
		RelayConfig:        relay.DefaultConfig(),
		Policy:             policy.NewDevDestinationPolicy(),
		Authorizer:         allowAllAuthorizer{},
		SessionPreallocTTL: time.Hour,
	})

	mux := http.NewServeMux()
	srv.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	resp, err := http.Post(ts.URL+"/session", "application/json", nil)
	if err != nil {
		t.Fatalf("post: %v", err)
	}
	_ = resp.Body.Close()

	srv.Close()
	if sess, err := sm.CreateSession(); err != nil {
		t.Fatalf("CreateSession after server.Close: %v", err)
	} else {
		sess.Close()
	}
}

func TestServer_WebRTCOffer_ConnectTimeoutClosesSession(t *testing.T) {
	cfg := config.Config{MaxSessions: 1}
	m := metrics.New()
	sm := relay.NewSessionManager(cfg, m, nil)

	mediaEngine := &webrtc.MediaEngine{}
	if err := mediaEngine.RegisterDefaultCodecs(); err != nil {
		t.Fatalf("register codecs: %v", err)
	}
	api := webrtc.NewAPI(webrtc.WithMediaEngine(mediaEngine))

	srv := NewServer(Config{
		Sessions:                    sm,
		WebRTC:                      api,
		RelayConfig:                 relay.DefaultConfig(),
		Policy:                      policy.NewDevDestinationPolicy(),
		Authorizer:                  allowAllAuthorizer{},
		ICEGatheringTimeout:         10 * time.Millisecond,
		WebRTCSessionConnectTimeout: 100 * time.Millisecond,
	})
	t.Cleanup(srv.Close)

	mux := http.NewServeMux()
	srv.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	// Create a valid offer SDP that negotiates a DataChannel, but do not proceed
	// with ICE/DTLS (i.e. never apply the returned answer).
	pc, err := api.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new pc: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	if _, err := pc.CreateDataChannel("udp", nil); err != nil {
		t.Fatalf("create datachannel: %v", err)
	}
	offer, err := pc.CreateOffer(nil)
	if err != nil {
		t.Fatalf("create offer: %v", err)
	}
	if err := pc.SetLocalDescription(offer); err != nil {
		t.Fatalf("set local offer: %v", err)
	}
	local := pc.LocalDescription()
	if local == nil {
		t.Fatalf("missing local description")
	}

	body, err := json.Marshal(httpOfferRequest{
		SDP: sdpFromPion(*local),
	})
	if err != nil {
		t.Fatalf("marshal offer: %v", err)
	}

	resp, err := http.Post(ts.URL+"/webrtc/offer", "application/json", bytes.NewReader(body))
	if err != nil {
		t.Fatalf("post: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusOK)
	}
	var out httpOfferResponse
	if err := json.NewDecoder(resp.Body).Decode(&out); err != nil {
		t.Fatalf("decode response: %v", err)
	}
	if out.SessionID == "" {
		t.Fatalf("expected non-empty sessionId")
	}

	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		srv.mu.Lock()
		webrtcSessions := len(srv.webrtcSessions)
		srv.mu.Unlock()

		if webrtcSessions == 0 && m.Get(metrics.WebRTCSessionConnectTimeout) > 0 {
			sess, err := sm.CreateSession()
			if err == nil {
				sess.Close()
				break
			}
			if !errors.Is(err, relay.ErrTooManySessions) {
				t.Fatalf("CreateSession while waiting for cleanup: %v", err)
			}
		}
		time.Sleep(5 * time.Millisecond)
	}

	if sess, err := sm.CreateSession(); err != nil {
		t.Fatalf("CreateSession after connect timeout: %v", err)
	} else {
		sess.Close()
	}
	srv.mu.Lock()
	gotWebRTCSessions := len(srv.webrtcSessions)
	srv.mu.Unlock()
	if gotWebRTCSessions != 0 {
		t.Fatalf("server webrtcSessions=%d, want 0", gotWebRTCSessions)
	}
	if got := m.Get(metrics.WebRTCSessionConnectTimeout); got != 1 {
		t.Fatalf("%s=%d, want 1", metrics.WebRTCSessionConnectTimeout, got)
	}
}
