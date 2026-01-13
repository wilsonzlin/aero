package webrtcpeer

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"
	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/l2tunnel"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
)

func waitForDataChannelState(t *testing.T, dc *webrtc.DataChannel, want webrtc.DataChannelState, timeout time.Duration) {
	t.Helper()
	if dc == nil {
		t.Fatalf("nil datachannel")
	}

	deadline := time.NewTimer(timeout)
	defer deadline.Stop()

	tick := time.NewTicker(10 * time.Millisecond)
	defer tick.Stop()

	for {
		if dc.ReadyState() == want {
			return
		}
		select {
		case <-deadline.C:
			t.Fatalf("timed out waiting for datachannel %q to reach %s (state=%s)", dc.Label(), want, dc.ReadyState())
		case <-tick.C:
		}
	}
}

func newHoldingL2Backend(t *testing.T) (string, <-chan struct{}) {
	t.Helper()

	connected := make(chan struct{}, 1)

	upgrader := websocket.Upgrader{
		Subprotocols: []string{l2tunnel.Subprotocol},
		// The relay is expected to enforce Origin separately; accept all origins
		// so the test remains self-contained.
		CheckOrigin: func(r *http.Request) bool { return true },
	}

	ts := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		c, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		if got := c.Subprotocol(); got != l2tunnel.Subprotocol {
			_ = c.Close()
			return
		}
		select {
		case connected <- struct{}{}:
		default:
		}
		// Keep the connection open until the client (l2Bridge) closes it.
		for {
			if _, _, err := c.ReadMessage(); err != nil {
				_ = c.Close()
				return
			}
		}
	}))
	t.Cleanup(ts.Close)

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/l2"
	return wsURL, connected
}

func TestSession_RejectsUnknownAndDuplicateDataChannels(t *testing.T) {
	api := webrtc.NewAPI()

	wsURL, backendConnected := newHoldingL2Backend(t)

	m := metrics.New()
	sm := relay.NewSessionManager(config.Config{}, m, nil)
	quota, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}
	t.Cleanup(quota.Close)

	serverSession, err := NewSession(api, nil, relay.Config{
		L2BackendWSURL:           wsURL,
		L2BackendAuthForwardMode: config.L2BackendAuthForwardModeNone,
	}, nil, quota, "", "", nil, 0, SessionOptions{}, nil)
	if err != nil {
		t.Fatalf("NewSession: %v", err)
	}
	t.Cleanup(func() { _ = serverSession.Close() })

	clientPC, err := api.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("NewPeerConnection(client): %v", err)
	}
	t.Cleanup(func() { _ = clientPC.Close() })

	// Create one valid DataChannel of each supported label up-front to ensure the
	// SCTP association is negotiated.
	ordered := false
	maxRetransmits := uint16(0)
	udp1, err := clientPC.CreateDataChannel(DataChannelLabelUDP, &webrtc.DataChannelInit{
		Ordered:        &ordered,
		MaxRetransmits: &maxRetransmits,
	})
	if err != nil {
		t.Fatalf("CreateDataChannel(%q): %v", DataChannelLabelUDP, err)
	}

	l2Ordered := true
	l2c1, err := clientPC.CreateDataChannel(DataChannelLabelL2, &webrtc.DataChannelInit{
		Ordered: &l2Ordered,
	})
	if err != nil {
		t.Fatalf("CreateDataChannel(%q): %v", DataChannelLabelL2, err)
	}

	connectPeerConnections(t, clientPC, serverSession.PeerConnection())

	waitForDataChannelState(t, udp1, webrtc.DataChannelStateOpen, 5*time.Second)
	waitForDataChannelState(t, l2c1, webrtc.DataChannelStateOpen, 5*time.Second)

	select {
	case <-backendConnected:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for l2 backend connection")
	}

	// Duplicate "udp" must be rejected/closed.
	udp2, err := clientPC.CreateDataChannel(DataChannelLabelUDP, &webrtc.DataChannelInit{
		Ordered:        &ordered,
		MaxRetransmits: &maxRetransmits,
	})
	if err != nil {
		t.Fatalf("CreateDataChannel(%q) duplicate: %v", DataChannelLabelUDP, err)
	}
	waitForDataChannelState(t, udp2, webrtc.DataChannelStateClosed, 5*time.Second)

	// Unknown label must be rejected/closed.
	unknown, err := clientPC.CreateDataChannel("unknown-label", &webrtc.DataChannelInit{
		Ordered:        &ordered,
		MaxRetransmits: &maxRetransmits,
	})
	if err != nil {
		t.Fatalf("CreateDataChannel(unknown): %v", err)
	}
	waitForDataChannelState(t, unknown, webrtc.DataChannelStateClosed, 5*time.Second)

	// Duplicate "l2" must be rejected/closed.
	l2c2, err := clientPC.CreateDataChannel(DataChannelLabelL2, &webrtc.DataChannelInit{
		Ordered: &l2Ordered,
	})
	if err != nil {
		t.Fatalf("CreateDataChannel(%q) duplicate: %v", DataChannelLabelL2, err)
	}
	waitForDataChannelState(t, l2c2, webrtc.DataChannelStateClosed, 5*time.Second)

	if got := m.Get(metrics.WebRTCDataChannelRejectedUnknownLabel); got != 1 {
		t.Fatalf("%s=%d, want 1", metrics.WebRTCDataChannelRejectedUnknownLabel, got)
	}
	if got := m.Get(metrics.WebRTCDataChannelRejectedDuplicateUDP); got != 1 {
		t.Fatalf("%s=%d, want 1", metrics.WebRTCDataChannelRejectedDuplicateUDP, got)
	}
	if got := m.Get(metrics.WebRTCDataChannelRejectedDuplicateL2); got != 1 {
		t.Fatalf("%s=%d, want 1", metrics.WebRTCDataChannelRejectedDuplicateL2, got)
	}
}
