package webrtcpeer

import (
	"testing"
	"time"

	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
)

func TestSession_RejectsOversizedUDPDataChannelMessage_Metrics(t *testing.T) {
	api, err := NewAPI(config.Config{
		// Ensure pion advertises a large max message size so the (pion-based) client
		// will actually send our oversized message.
		WebRTCDataChannelMaxMessageBytes: 1 << 30,
		WebRTCSCTPMaxReceiveBufferBytes:  1 << 20,
	})
	if err != nil {
		t.Fatalf("NewAPI: %v", err)
	}

	m := metrics.New()
	sm := relay.NewSessionManager(config.Config{}, m, nil)
	quota, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}
	t.Cleanup(quota.Close)

	relayCfg := relay.DefaultConfig()
	relayCfg.MaxDatagramPayloadBytes = 8 // => max UDP DC msg bytes = 8 + webrtcDataChannelUDPFrameOverheadBytes

	serverSession, err := NewSession(api, nil, relayCfg, policy.NewDevDestinationPolicy(), quota, "", "", nil, 0, SessionOptions{}, nil)
	if err != nil {
		t.Fatalf("NewSession(server): %v", err)
	}
	t.Cleanup(func() { _ = serverSession.Close() })

	clientPC, err := api.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("NewPeerConnection(client): %v", err)
	}
	t.Cleanup(func() { _ = clientPC.Close() })

	ordered := false
	maxRetransmits := uint16(0)
	clientDC, err := clientPC.CreateDataChannel(dataChannelLabelUDP, &webrtc.DataChannelInit{
		Ordered:        &ordered,
		MaxRetransmits: &maxRetransmits,
	})
	if err != nil {
		t.Fatalf("CreateDataChannel(%q): %v", dataChannelLabelUDP, err)
	}

	connectPeerConnections(t, clientPC, serverSession.PeerConnection())
	waitForDataChannelState(t, clientDC, webrtc.DataChannelStateOpen, 5*time.Second)

	// Send a payload that exceeds max payload + framing overhead by 1 byte.
	oversize := make([]byte, relayCfg.MaxDatagramPayloadBytes+webrtcDataChannelUDPFrameOverheadBytes+1)
	if err := clientDC.Send(oversize); err != nil {
		t.Fatalf("Send(oversize): %v", err)
	}

	waitForDataChannelState(t, clientDC, webrtc.DataChannelStateClosed, 5*time.Second)

	if got := m.Get(metrics.WebRTCUDPDroppedOversized); got == 0 {
		t.Fatalf("expected %s metric increment", metrics.WebRTCUDPDroppedOversized)
	}
}
