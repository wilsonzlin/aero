package webrtcpeer

import (
	"bytes"
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
		Subprotocols: []string{l2tunnel.Subprotocol},
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

		foundTunnelProto := false
		for _, proto := range obs.Subprotocols {
			if proto == l2tunnel.Subprotocol {
				foundTunnelProto = true
				break
			}
		}
		if !foundTunnelProto {
			http.Error(w, "missing required subprotocol", http.StatusBadRequest)
			return
		}

		if obs.Origin != expectedOrigin {
			http.Error(w, "bad origin", http.StatusForbidden)
			return
		}

		switch mode {
		case config.L2BackendAuthForwardModeQuery:
			if obs.Token != "" && obs.APIKey != "" && obs.Token != obs.APIKey {
				http.Error(w, "conflicting query credentials", http.StatusBadRequest)
				return
			}
			if obs.Token != expectedCredential || obs.APIKey != expectedCredential {
				http.Error(w, "missing credential in query", http.StatusUnauthorized)
				return
			}
		case config.L2BackendAuthForwardModeSubprotocol:
			want := l2tunnel.TokenSubprotocolPrefix + expectedCredential
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
		if got := c.Subprotocol(); got != l2tunnel.Subprotocol {
			_ = c.Close()
			return
		}

		_ = c.SetReadDeadline(time.Now().Add(5 * time.Second))
		msgType, payload, err := c.ReadMessage()
		if err != nil {
			_ = c.Close()
			return
		}
		if msgType == websocket.BinaryMessage {
			_ = c.WriteMessage(websocket.BinaryMessage, payload)
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

	_ = conn.SetReadDeadline(time.Now().Add(2 * time.Second))
	_ = conn.SetWriteDeadline(time.Now().Add(2 * time.Second))
	want := []byte("hello")
	if err := conn.WriteMessage(websocket.BinaryMessage, want); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}
	msgType, got, err := conn.ReadMessage()
	if err != nil {
		t.Fatalf("ReadMessage: %v", err)
	}
	if msgType != websocket.BinaryMessage || !bytes.Equal(got, want) {
		t.Fatalf("unexpected echo: type=%d payload=%q", msgType, got)
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
		wantTokenProto := l2tunnel.TokenSubprotocolPrefix + credential
		foundTokenProto := false
		foundTunnelProto := false
		for _, proto := range obs.Subprotocols {
			if proto == l2tunnel.Subprotocol {
				foundTunnelProto = true
			}
			if proto == wantTokenProto {
				foundTokenProto = true
			}
		}
		if !foundTunnelProto {
			t.Fatalf("expected offered subprotocol %q in %v", l2tunnel.Subprotocol, obs.Subprotocols)
		}
		if !foundTokenProto {
			t.Fatalf("expected offered subprotocol %q in %v", wantTokenProto, obs.Subprotocols)
		}
	case <-ctx.Done():
		t.Fatalf("timed out waiting for backend handshake: %v", ctx.Err())
	}

	_ = conn.SetReadDeadline(time.Now().Add(2 * time.Second))
	_ = conn.SetWriteDeadline(time.Now().Add(2 * time.Second))
	want := []byte("hello")
	if err := conn.WriteMessage(websocket.BinaryMessage, want); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}
	msgType, got, err := conn.ReadMessage()
	if err != nil {
		t.Fatalf("ReadMessage: %v", err)
	}
	if msgType != websocket.BinaryMessage || !bytes.Equal(got, want) {
		t.Fatalf("unexpected echo: type=%d payload=%q", msgType, got)
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

func TestDialL2Backend_SubprotocolAuthRejectsInvalidCredential(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	_, err := dialL2Backend(ctx, l2BackendDialConfig{
		BackendWSURL:    "ws://127.0.0.1:1/l2",
		ClientOrigin:    "https://example.com",
		Credential:      "not a token",
		ForwardOrigin:   true,
		AuthForwardMode: config.L2BackendAuthForwardModeSubprotocol,
	})
	if err == nil {
		t.Fatalf("expected error")
	}
	if !strings.Contains(err.Error(), "Sec-WebSocket-Protocol") {
		t.Fatalf("err=%v, want token validity error", err)
	}
}

func TestDialL2Backend_FailsOnMissingOrigin(t *testing.T) {
	const (
		origin     = "https://example.com"
		credential = "secret"
	)

	wsURL, _ := newTestL2Backend(t, origin, credential, config.L2BackendAuthForwardModeQuery)

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	_, err := dialL2Backend(ctx, l2BackendDialConfig{
		BackendWSURL:    wsURL,
		ClientOrigin:    "",
		Credential:      credential,
		ForwardOrigin:   true,
		AuthForwardMode: config.L2BackendAuthForwardModeQuery,
	})
	if err == nil {
		t.Fatalf("expected error")
	}
}

func TestDialL2Backend_FailsOnWrongOrigin(t *testing.T) {
	const (
		origin     = "https://example.com"
		credential = "secret"
	)

	wsURL, _ := newTestL2Backend(t, origin, credential, config.L2BackendAuthForwardModeQuery)

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	_, err := dialL2Backend(ctx, l2BackendDialConfig{
		BackendWSURL:    wsURL,
		ClientOrigin:    "https://wrong.example.com",
		Credential:      credential,
		ForwardOrigin:   true,
		AuthForwardMode: config.L2BackendAuthForwardModeQuery,
	})
	if err == nil {
		t.Fatalf("expected error")
	}
}

func TestDialL2Backend_FailsOnMissingCredentialQuery(t *testing.T) {
	const origin = "https://example.com"

	wsURL, _ := newTestL2Backend(t, origin, "secret", config.L2BackendAuthForwardModeQuery)

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	_, err := dialL2Backend(ctx, l2BackendDialConfig{
		BackendWSURL:    wsURL,
		ClientOrigin:    origin,
		ForwardOrigin:   true,
		AuthForwardMode: config.L2BackendAuthForwardModeQuery,
	})
	if err == nil {
		t.Fatalf("expected error")
	}
}

func TestDialL2Backend_FailsOnMissingCredentialSubprotocol(t *testing.T) {
	const origin = "https://example.com"

	wsURL, _ := newTestL2Backend(t, origin, "secret", config.L2BackendAuthForwardModeSubprotocol)

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()

	_, err := dialL2Backend(ctx, l2BackendDialConfig{
		BackendWSURL:    wsURL,
		ClientOrigin:    origin,
		ForwardOrigin:   true,
		AuthForwardMode: config.L2BackendAuthForwardModeSubprotocol,
	})
	if err == nil {
		t.Fatalf("expected error")
	}
}
