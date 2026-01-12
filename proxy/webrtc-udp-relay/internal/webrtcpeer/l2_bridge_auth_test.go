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
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/l2tunnel"
)

func newL2BackendWSServer(t *testing.T, expectedOrigin string, requiredToken string) string {
	t.Helper()

	upgrader := websocket.Upgrader{
		Subprotocols: []string{l2tunnel.Subprotocol},
		// The default origin check only accepts same-origin requests, which isn't
		// what we want in these tests.
		CheckOrigin: func(r *http.Request) bool { return true },
	}

	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if got := r.Header.Get("Origin"); got != expectedOrigin {
			http.Error(w, "origin rejected", http.StatusForbidden)
			return
		}

		offered := websocket.Subprotocols(r)
		foundTunnel := false
		for _, p := range offered {
			if p == l2tunnel.Subprotocol {
				foundTunnel = true
				break
			}
		}
		if !foundTunnel {
			http.Error(w, "missing required subprotocol", http.StatusBadRequest)
			return
		}

		if requiredToken != "" {
			want := l2tunnel.TokenSubprotocolPrefix + requiredToken
			foundToken := false
			for _, p := range offered {
				if p == want {
					foundToken = true
					break
				}
			}
			if !foundToken {
				http.Error(w, "missing token subprotocol", http.StatusUnauthorized)
				return
			}
		}

		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			// The client side sees the error; the test doesn't need extra noise.
			return
		}
		defer conn.Close()
		// Ensure the server selects the required tunnel subprotocol even when
		// additional subprotocols (e.g. auth) are offered.
		if got := conn.Subprotocol(); got != l2tunnel.Subprotocol {
			return
		}
	}))
	t.Cleanup(ts.Close)

	return "ws" + strings.TrimPrefix(ts.URL, "http") + "/l2"
}

func TestL2BridgeDialBackend_SucceedsWithOriginAndToken(t *testing.T) {
	const (
		origin = "https://aero.example.com"
		token  = "secret-token"
	)

	wsURL := newL2BackendWSServer(t, origin, token)

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	conn, err := dialL2Backend(ctx, l2BackendDialConfig{
		BackendWSURL:          wsURL,
		BackendOriginOverride: origin,
		BackendToken:          token,
		AuthForwardMode:       config.L2BackendAuthForwardModeNone,
	})
	if err != nil {
		t.Fatalf("dialL2Backend: %v", err)
	}
	_ = conn.Close()
}

func TestL2BridgeDialBackend_FailsWithoutOrigin(t *testing.T) {
	const origin = "https://aero.example.com"

	wsURL := newL2BackendWSServer(t, origin, "")

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	if _, err := dialL2Backend(ctx, l2BackendDialConfig{
		BackendWSURL:    wsURL,
		AuthForwardMode: config.L2BackendAuthForwardModeNone,
	}); err == nil {
		t.Fatalf("expected dialL2Backend to fail, got nil error")
	}
}

func TestL2BridgeDialBackend_FailsWithMismatchedOrigin(t *testing.T) {
	const origin = "https://aero.example.com"

	wsURL := newL2BackendWSServer(t, origin, "")

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	if _, err := dialL2Backend(ctx, l2BackendDialConfig{
		BackendWSURL:          wsURL,
		BackendOriginOverride: "https://wrong.example.com",
		AuthForwardMode:       config.L2BackendAuthForwardModeNone,
	}); err == nil {
		t.Fatalf("expected dialL2Backend to fail, got nil error")
	}
}

func TestL2BridgeDialBackend_FailsWithoutTokenWhenRequired(t *testing.T) {
	const (
		origin = "https://aero.example.com"
		token  = "required-token"
	)

	wsURL := newL2BackendWSServer(t, origin, token)

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	if _, err := dialL2Backend(ctx, l2BackendDialConfig{
		BackendWSURL:          wsURL,
		BackendOriginOverride: origin,
		AuthForwardMode:       config.L2BackendAuthForwardModeNone,
	}); err == nil {
		t.Fatalf("expected dialL2Backend to fail, got nil error")
	}
}
