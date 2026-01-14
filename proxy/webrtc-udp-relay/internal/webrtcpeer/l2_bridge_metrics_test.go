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
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
)

func TestL2Bridge_OversizedMessageIncrementsMetricAndClosesBridge(t *testing.T) {
	m := metrics.New()
	sm := relay.NewSessionManager(config.Config{}, m, nil)
	sess, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}
	t.Cleanup(sess.Close)

	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)

	b := &l2Bridge{
		ctx:       ctx,
		cancel:    cancel,
		dialCfg:   l2BackendDialConfig{MaxMessageBytes: 1},
		quota:     sess,
		toBackend: make(chan []byte, 1),
	}

	b.HandleDataChannelMessage([]byte{0x01, 0x02})

	if got := m.Get(metrics.L2BridgeDroppedOversizedTotal); got != 1 {
		t.Fatalf("%s=%d, want %d", metrics.L2BridgeDroppedOversizedTotal, got, 1)
	}

	select {
	case <-ctx.Done():
	case <-time.After(2 * time.Second):
		t.Fatalf("expected bridge context to be canceled after oversized message")
	}
}

func TestL2Bridge_QuotaDropIncrementsMetric(t *testing.T) {
	m := metrics.New()
	sm := relay.NewSessionManager(config.Config{
		// Force quota.HandleInboundToClient to drop on the first frame.
		MaxDataChannelBpsPerSession: 1,
	}, m, nil)
	sess, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}
	t.Cleanup(sess.Close)

	upgrader := websocket.Upgrader{
		CheckOrigin: func(r *http.Request) bool { return true },
	}
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		defer conn.Close()

		_ = conn.SetWriteDeadline(time.Now().Add(2 * time.Second))
		_ = conn.WriteMessage(websocket.BinaryMessage, []byte{0x01, 0x02})
		_ = conn.WriteControl(websocket.CloseMessage, websocket.FormatCloseMessage(websocket.CloseNormalClosure, "bye"), time.Now().Add(2*time.Second))
	}))
	t.Cleanup(ts.Close)

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http")
	conn, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("Dial: %v", err)
	}
	t.Cleanup(func() { _ = conn.Close() })

	b := &l2Bridge{
		ctx:    context.Background(),
		cancel: func() {},
		dialCfg: l2BackendDialConfig{
			MaxMessageBytes: 0,
		},
		quota: sess,
		// dc is intentionally nil: if quota enforcement fails, wsReadLoop will
		// attempt to forward to the datachannel and panic, failing the test.
	}

	_ = b.wsReadLoop(conn)

	if got := m.Get(metrics.L2BridgeDroppedRateLimitedTotal); got != 1 {
		t.Fatalf("%s=%d, want %d", metrics.L2BridgeDroppedRateLimitedTotal, got, 1)
	}
}

func TestL2Bridge_DialCanceledDoesNotIncrementDialErrorMetric(t *testing.T) {
	m := metrics.New()
	sm := relay.NewSessionManager(config.Config{}, m, nil)
	sess, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}
	t.Cleanup(sess.Close)

	// Provide a syntactically valid backend URL. The dial should short-circuit due
	// to context cancellation, so the server should never see a request.
	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Error(w, "unexpected dial", http.StatusInternalServerError)
	}))
	t.Cleanup(ts.Close)
	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/l2"

	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	b := &l2Bridge{
		ctx:       ctx,
		cancel:    cancel,
		dialCfg:   l2BackendDialConfig{BackendWSURL: wsURL},
		quota:     sess,
		toBackend: make(chan []byte, 1),
	}

	_, _ = b.dialBackend()

	if got := m.Get(metrics.L2BridgeDialsTotal); got != 1 {
		t.Fatalf("%s=%d, want %d", metrics.L2BridgeDialsTotal, got, 1)
	}
	if got := m.Get(metrics.L2BridgeDialErrorsTotal); got != 0 {
		t.Fatalf("%s=%d, want %d (canceled dial)", metrics.L2BridgeDialErrorsTotal, got, 0)
	}
}
