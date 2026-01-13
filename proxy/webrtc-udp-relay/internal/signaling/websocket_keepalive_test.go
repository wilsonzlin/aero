package signaling

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"
	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
)

func TestWebSocketSignaling_IdleTimeoutClosesWithoutPong(t *testing.T) {
	srv := NewServer(Config{
		WebRTC:                  webrtc.NewAPI(),
		RelayConfig:             relay.DefaultConfig(),
		Policy:                  policy.NewDevDestinationPolicy(),
		Authorizer:              AllowAllAuthorizer{},
		SignalingWSIdleTimeout:  200 * time.Millisecond,
		SignalingWSPingInterval: 50 * time.Millisecond,
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
	defer c.Close()

	// Read, but do not reply to pings.
	c.SetPingHandler(func(string) error { return nil })

	_ = c.SetReadDeadline(time.Now().Add(2 * time.Second))
	_, _, err = c.ReadMessage()
	if err == nil {
		t.Fatalf("expected server to close the websocket")
	}
	if !websocket.IsCloseError(err, websocket.CloseNormalClosure) {
		t.Fatalf("expected close normal closure, got %v", err)
	}
}

func TestWebSocketSignaling_PongKeepsConnectionOpenBeyondIdleTimeout(t *testing.T) {
	idleTimeout := 200 * time.Millisecond

	srv := NewServer(Config{
		WebRTC:                  webrtc.NewAPI(),
		RelayConfig:             relay.DefaultConfig(),
		Policy:                  policy.NewDevDestinationPolicy(),
		Authorizer:              AllowAllAuthorizer{},
		SignalingWSIdleTimeout:  idleTimeout,
		SignalingWSPingInterval: 50 * time.Millisecond,
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

	errCh := make(chan error, 1)
	go func() {
		_, _, err := c.ReadMessage()
		errCh <- err
	}()

	// Wait longer than the idle timeout. The read goroutine will process ping
	// frames and respond with pong (gorilla's default PingHandler).
	time.Sleep(3 * idleTimeout)

	select {
	case err := <-errCh:
		t.Fatalf("unexpected close before idle timeout elapsed: %v", err)
	default:
	}

	_ = c.Close()
	select {
	case <-errCh:
	case <-time.After(2 * time.Second):
		t.Fatalf("timeout waiting for read goroutine to exit")
	}
}

