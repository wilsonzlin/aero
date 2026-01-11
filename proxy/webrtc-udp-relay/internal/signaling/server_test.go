package signaling

import (
	"bytes"
	"encoding/json"
	"errors"
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
		Authorizer:          AllowAllAuthorizer{},
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

	offerSDP := func() SDP {
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
		return SDPFromPion(*local)
	}()

	if err := ws1.WriteJSON(SignalMessage{Type: MessageTypeOffer, SDP: ptr(offerSDP)}); err != nil {
		t.Fatalf("send offer: %v", err)
	}

	ws2, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial second websocket: %v", err)
	}
	t.Cleanup(func() { _ = ws2.Close() })

	if err := ws2.WriteJSON(SignalMessage{Type: MessageTypeOffer, SDP: ptr(offerSDP)}); err != nil {
		t.Fatalf("send offer ws2: %v", err)
	}

	_ = ws2.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, raw, err := ws2.ReadMessage()
	if err != nil {
		t.Fatalf("receive: %v", err)
	}
	msg, err := ParseSignalMessage(raw)
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

func TestServer_RejectsCrossOriginHTTPRequests(t *testing.T) {
	srv := NewServer(Config{
		RelayConfig: relay.DefaultConfig(),
		Policy:      policy.NewDevDestinationPolicy(),
		Authorizer:  AllowAllAuthorizer{},
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
		Authorizer:     AllowAllAuthorizer{},
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

func (failingAuthorizer) Authorize(r *http.Request, firstMsg *ClientHello) (AuthResult, error) {
	return AuthResult{}, errors.New("boom")
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
		parsed, parseErr := ParseSignalMessage(msg)
		if parseErr == nil {
			if parsed.Type != MessageTypeError || parsed.Code != "internal_error" {
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
		body, err := json.Marshal(OfferRequest{
			Version: Version1,
			Offer: SessionDescription{
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
			SDP: SDP{Type: "offer", SDP: "v=0"},
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
