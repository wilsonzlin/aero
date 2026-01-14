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
	idleTimeout := 500 * time.Millisecond
	pingInterval := 50 * time.Millisecond

	srv := NewServer(Config{
		WebRTC:                  webrtc.NewAPI(),
		RelayConfig:             relay.DefaultConfig(),
		Policy:                  policy.NewDevDestinationPolicy(),
		Authorizer:              allowAllAuthorizer{},
		SignalingWSIdleTimeout:  idleTimeout,
		SignalingWSPingInterval: pingInterval,
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

	pingSeen := make(chan struct{}, 1)
	c.SetPingHandler(func(string) error {
		select {
		case pingSeen <- struct{}{}:
		default:
		}
		// Intentionally do not respond with pong.
		return nil
	})

	errCh := make(chan error, 1)
	go func() {
		_, _, err := c.ReadMessage()
		errCh <- err
	}()

	select {
	case <-pingSeen:
	case err := <-errCh:
		t.Fatalf("connection closed before receiving ping: %v", err)
	case <-time.After(2 * time.Second):
		t.Fatalf("timeout waiting for server ping")
	}

	select {
	case err := <-errCh:
		if err == nil {
			t.Fatalf("expected server to close the websocket")
		}
		if !websocket.IsCloseError(err, websocket.CloseNormalClosure) {
			t.Fatalf("expected close normal closure, got %v", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatalf("timeout waiting for server to close idle websocket")
	}
}

func TestWebSocketSignaling_PongKeepsConnectionOpenBeyondIdleTimeout(t *testing.T) {
	idleTimeout := 500 * time.Millisecond
	pingInterval := 50 * time.Millisecond

	srv := NewServer(Config{
		WebRTC:                  webrtc.NewAPI(),
		RelayConfig:             relay.DefaultConfig(),
		Policy:                  policy.NewDevDestinationPolicy(),
		Authorizer:              allowAllAuthorizer{},
		SignalingWSIdleTimeout:  idleTimeout,
		SignalingWSPingInterval: pingInterval,
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

	pingSeen := make(chan struct{}, 1)
	c.SetPingHandler(func(appData string) error {
		select {
		case pingSeen <- struct{}{}:
		default:
		}
		// Respond with pong so the server extends the read deadline.
		return c.WriteControl(websocket.PongMessage, []byte(appData), time.Now().Add(1*time.Second))
	})

	errCh := make(chan error, 1)
	go func() {
		_, _, err := c.ReadMessage()
		errCh <- err
	}()

	select {
	case <-pingSeen:
	case err := <-errCh:
		t.Fatalf("connection closed before receiving ping: %v", err)
	case <-time.After(2 * time.Second):
		t.Fatalf("timeout waiting for server ping")
	}

	// Wait longer than the idle timeout. The read goroutine will process ping
	// frames and respond with pong.
	time.Sleep(idleTimeout + 2*pingInterval)

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
