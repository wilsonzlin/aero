package webrtcpeer

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
)

type l2HandshakeObserved struct {
	Origin       string
	Token        string
	APIKey       string
	Subprotocols []string
}

func newTestL2Backend(t *testing.T, expectedOrigin string, expectedCredential string, mode config.L2BackendAuthForwardMode) (string, <-chan l2HandshakeObserved) {
	t.Helper()

	obsCh := make(chan l2HandshakeObserved, 1)

	upgrader := websocket.Upgrader{
		Subprotocols: []string{l2TunnelSubprotocol},
		// The test itself verifies Origin; accept all origins at the upgrader
		// level so CheckOrigin does not interfere.
		CheckOrigin: func(r *http.Request) bool { return true },
	}

	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		obs := l2HandshakeObserved{
			Origin:       r.Header.Get("Origin"),
			Token:        r.URL.Query().Get("token"),
			APIKey:       r.URL.Query().Get("apiKey"),
			Subprotocols: websocket.Subprotocols(r),
		}
		select {
		case obsCh <- obs:
		default:
		}

		if obs.Origin != expectedOrigin {
			http.Error(w, "bad origin", http.StatusForbidden)
			return
		}

		switch mode {
		case config.L2BackendAuthForwardModeQuery:
			if obs.Token != expectedCredential || obs.APIKey != expectedCredential {
				http.Error(w, "missing credential in query", http.StatusUnauthorized)
				return
			}
		case config.L2BackendAuthForwardModeSubprotocol:
			want := l2TokenSubprotocolPrefix + expectedCredential
			found := false
			for _, proto := range obs.Subprotocols {
				if proto == want {
					found = true
					break
				}
			}
			if !found {
				http.Error(w, "missing credential subprotocol", http.StatusUnauthorized)
				return
			}
		default:
			http.Error(w, "unsupported test auth mode", http.StatusInternalServerError)
			return
		}

		c, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		// Ensure the server selects the required tunnel subprotocol even when
		// additional subprotocols (e.g. auth) are offered.
		if got := c.Subprotocol(); got != l2TunnelSubprotocol {
			_ = c.Close()
			return
		}
		_ = c.Close()
	}))
	t.Cleanup(ts.Close)

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/l2"
	return wsURL, obsCh
}

func TestDialL2Backend_AuthQueryAndOrigin(t *testing.T) {
	const origin = "https://example.com"
	const credential = "secret"

	wsURL, obsCh := newTestL2Backend(t, origin, credential, config.L2BackendAuthForwardModeQuery)

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	conn, err := dialL2Backend(ctx, l2BackendDialConfig{
		BackendWSURL:    wsURL,
		ClientOrigin:    origin,
		Credential:      credential,
		ForwardOrigin:   true,
		AuthForwardMode: config.L2BackendAuthForwardModeQuery,
	})
	if err != nil {
		t.Fatalf("dialL2Backend: %v", err)
	}
	t.Cleanup(func() { _ = conn.Close() })

	select {
	case obs := <-obsCh:
		if obs.Origin != origin {
			t.Fatalf("Origin=%q, want %q", obs.Origin, origin)
		}
		if obs.Token != credential {
			t.Fatalf("token=%q, want %q", obs.Token, credential)
		}
		if obs.APIKey != credential {
			t.Fatalf("apiKey=%q, want %q", obs.APIKey, credential)
		}
	case <-ctx.Done():
		t.Fatalf("timed out waiting for backend handshake: %v", ctx.Err())
	}
}

func TestDialL2Backend_AuthSubprotocolAndOriginOverride(t *testing.T) {
	const (
		clientOrigin   = "https://client.example.com"
		overrideOrigin = "https://override.example.com"
		credential     = "sekrit"
	)

	wsURL, obsCh := newTestL2Backend(t, overrideOrigin, credential, config.L2BackendAuthForwardModeSubprotocol)

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	conn, err := dialL2Backend(ctx, l2BackendDialConfig{
		BackendWSURL:          wsURL,
		ClientOrigin:          clientOrigin,
		Credential:            credential,
		ForwardOrigin:         true,
		BackendOriginOverride: overrideOrigin,
		AuthForwardMode:       config.L2BackendAuthForwardModeSubprotocol,
	})
	if err != nil {
		t.Fatalf("dialL2Backend: %v", err)
	}
	t.Cleanup(func() { _ = conn.Close() })

	select {
	case obs := <-obsCh:
		if obs.Origin != overrideOrigin {
			t.Fatalf("Origin=%q, want %q", obs.Origin, overrideOrigin)
		}
		wantTokenProto := l2TokenSubprotocolPrefix + credential
		foundTokenProto := false
		foundTunnelProto := false
		for _, proto := range obs.Subprotocols {
			if proto == l2TunnelSubprotocol {
				foundTunnelProto = true
			}
			if proto == wantTokenProto {
				foundTokenProto = true
			}
		}
		if !foundTunnelProto {
			t.Fatalf("expected offered subprotocol %q in %v", l2TunnelSubprotocol, obs.Subprotocols)
		}
		if !foundTokenProto {
			t.Fatalf("expected offered subprotocol %q in %v", wantTokenProto, obs.Subprotocols)
		}
	case <-ctx.Done():
		t.Fatalf("timed out waiting for backend handshake: %v", ctx.Err())
	}
}

func TestDialL2Backend_StrictSubprotocolNegotiation(t *testing.T) {
	// Ensure we reject backends that don't negotiate the required
	// aero-l2-tunnel-v1 subprotocol.
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		upgrader := websocket.Upgrader{
			Subprotocols: []string{"something-else"},
			CheckOrigin:  func(r *http.Request) bool { return true },
		}
		c, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		_ = c.Close()
	}))
	t.Cleanup(ts.Close)

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/l2"

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	_, err := dialL2Backend(ctx, l2BackendDialConfig{
		BackendWSURL:    wsURL,
		ClientOrigin:    "https://example.com",
		ForwardOrigin:   true,
		AuthForwardMode: config.L2BackendAuthForwardModeNone,
	})
	if err == nil {
		t.Fatalf("expected error")
	}
	if !strings.Contains(err.Error(), "did not negotiate required subprotocol") {
		t.Fatalf("err=%v, want subprotocol negotiation error", err)
	}
}
