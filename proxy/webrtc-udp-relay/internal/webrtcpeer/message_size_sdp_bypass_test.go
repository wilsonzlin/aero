package webrtcpeer

import (
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
)

func TestWebRTCDataChannel_OversizeMessage_IgnoresSDP_ClosesSession(t *testing.T) {
	// Pion's SCTP max message size is signaled via SDP `a=max-message-size`, and
	// pion-based clients will usually refuse to send larger messages.
	//
	// However, malicious peers can ignore SDP negotiation and still transmit
	// oversized user messages. The relay should tear down the session when it
	// observes an oversized inbound message (defense in depth; receive-side memory
	// allocation is still bounded by the SCTP receive buffer cap).
	cfg := config.Config{
		WebRTCDataChannelMaxMessageBytes: 256,
		WebRTCSCTPMaxReceiveBufferBytes:  1 << 20,
	}

	api, err := NewAPI(cfg)
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

	var closeOnce sync.Once
	sessClosed := make(chan struct{})
	sess, err := NewSession(
		api,
		nil,
		relay.DefaultConfig(),
		nil,
		quota,
		"",
		"",
		nil,
		cfg.WebRTCDataChannelMaxMessageBytes,
		SessionOptions{},
		func() { closeOnce.Do(func() { close(sessClosed) }) },
	)
	if err != nil {
		t.Fatalf("NewSession(server): %v", err)
	}
	t.Cleanup(func() { _ = sess.Close() })

	serverPC := sess.PeerConnection()

	clientPC, err := webrtc.NewPeerConnection(webrtc.Configuration{})
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

	connectPeerConnectionsWithAnswerSDPTransform(t, clientPC, serverPC, func(sdp string) string {
		want := "a=max-message-size:" + strconv.Itoa(cfg.WebRTCDataChannelMaxMessageBytes)
		if !strings.Contains(sdp, want) {
			t.Fatalf("expected answer SDP to contain %q; got:\n%s", want, sdp)
		}
		return forceSDPMaxMessageSize(sdp, 1<<30)
	})

	waitForDataChannelState(t, clientDC, webrtc.DataChannelStateOpen, 5*time.Second)

	// The payload is intentionally:
	// - larger than cfg.WebRTCDataChannelMaxMessageBytes (should trigger session close)
	// - smaller than the UDP relay framing max (default MAX_DATAGRAM_PAYLOAD_BYTES+webrtcDataChannelUDPFrameOverheadBytes)
	// so we exercise the session-level cap specifically.
	payload := make([]byte, cfg.WebRTCDataChannelMaxMessageBytes*2)
	if err := clientDC.Send(payload); err != nil {
		t.Fatalf("Send(oversize): %v", err)
	}

	select {
	case <-sessClosed:
		// ok
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for session close after oversized message")
	}

	if got := m.Get(metrics.WebRTCDataChannelMessageTooLargeUDP); got != 1 {
		t.Fatalf("%s=%d, want 1", metrics.WebRTCDataChannelMessageTooLargeUDP, got)
	}
}

func TestWebRTCDataChannel_OversizeL2Message_IgnoresSDP_ClosesSession(t *testing.T) {
	cfg := config.Config{
		WebRTCDataChannelMaxMessageBytes: 256,
		WebRTCSCTPMaxReceiveBufferBytes:  1 << 20,
	}

	api, err := NewAPI(cfg)
	if err != nil {
		t.Fatalf("NewAPI: %v", err)
	}

	wsURL, backendConnected := newHoldingL2Backend(t)

	m := metrics.New()
	sm := relay.NewSessionManager(config.Config{}, m, nil)
	quota, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}
	t.Cleanup(quota.Close)

	var closeOnce sync.Once
	sessClosed := make(chan struct{})
	sess, err := NewSession(
		api,
		nil,
		relay.Config{
			L2BackendWSURL:           wsURL,
			L2BackendAuthForwardMode: config.L2BackendAuthForwardModeNone,
		},
		nil,
		quota,
		"",
		"",
		nil,
		cfg.WebRTCDataChannelMaxMessageBytes,
		SessionOptions{},
		func() { closeOnce.Do(func() { close(sessClosed) }) },
	)
	if err != nil {
		t.Fatalf("NewSession(server): %v", err)
	}
	t.Cleanup(func() { _ = sess.Close() })

	serverPC := sess.PeerConnection()

	clientPC, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("NewPeerConnection(client): %v", err)
	}
	t.Cleanup(func() { _ = clientPC.Close() })

	l2Ordered := true
	clientDC, err := clientPC.CreateDataChannel(dataChannelLabelL2, &webrtc.DataChannelInit{
		Ordered: &l2Ordered,
	})
	if err != nil {
		t.Fatalf("CreateDataChannel(%q): %v", dataChannelLabelL2, err)
	}

	connectPeerConnectionsWithAnswerSDPTransform(t, clientPC, serverPC, func(sdp string) string {
		want := "a=max-message-size:" + strconv.Itoa(cfg.WebRTCDataChannelMaxMessageBytes)
		if !strings.Contains(sdp, want) {
			t.Fatalf("expected answer SDP to contain %q; got:\n%s", want, sdp)
		}
		return forceSDPMaxMessageSize(sdp, 1<<30)
	})

	waitForDataChannelState(t, clientDC, webrtc.DataChannelStateOpen, 5*time.Second)

	select {
	case <-backendConnected:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for l2 backend connection")
	}

	payload := make([]byte, cfg.WebRTCDataChannelMaxMessageBytes*2)
	if err := clientDC.Send(payload); err != nil {
		t.Fatalf("Send(oversize): %v", err)
	}

	select {
	case <-sessClosed:
		// ok
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for session close after oversized message")
	}

	if got := m.Get(metrics.WebRTCDataChannelMessageTooLargeL2); got != 1 {
		t.Fatalf("%s=%d, want 1", metrics.WebRTCDataChannelMessageTooLargeL2, got)
	}
}

func connectPeerConnectionsWithAnswerSDPTransform(t *testing.T, offerer, answerer *webrtc.PeerConnection, transform func(string) string) {
	t.Helper()

	offer, err := offerer.CreateOffer(nil)
	if err != nil {
		t.Fatalf("CreateOffer: %v", err)
	}
	offerGatherComplete := webrtc.GatheringCompletePromise(offerer)
	if err := offerer.SetLocalDescription(offer); err != nil {
		t.Fatalf("SetLocalDescription(offer): %v", err)
	}
	<-offerGatherComplete

	offerSDP := offerer.LocalDescription()
	if offerSDP == nil {
		t.Fatalf("missing local offer")
	}
	if err := answerer.SetRemoteDescription(*offerSDP); err != nil {
		t.Fatalf("SetRemoteDescription(offer): %v", err)
	}

	answer, err := answerer.CreateAnswer(nil)
	if err != nil {
		t.Fatalf("CreateAnswer: %v", err)
	}
	answerGatherComplete := webrtc.GatheringCompletePromise(answerer)
	if err := answerer.SetLocalDescription(answer); err != nil {
		t.Fatalf("SetLocalDescription(answer): %v", err)
	}
	<-answerGatherComplete

	answerSDP := answerer.LocalDescription()
	if answerSDP == nil {
		t.Fatalf("missing local answer")
	}
	mod := *answerSDP
	if transform != nil {
		mod.SDP = transform(mod.SDP)
	}
	if err := offerer.SetRemoteDescription(mod); err != nil {
		t.Fatalf("SetRemoteDescription(answer): %v", err)
	}
}

func forceSDPMaxMessageSize(sdp string, max uint64) string {
	want := "a=max-message-size:" + strconv.FormatUint(max, 10)
	lines := strings.Split(sdp, "\n")
	replaced := false
	for i, line := range lines {
		raw := strings.TrimSuffix(line, "\r")
		if strings.HasPrefix(raw, "a=max-message-size:") {
			suffix := ""
			if strings.HasSuffix(line, "\r") {
				suffix = "\r"
			}
			lines[i] = want + suffix
			replaced = true
		}
	}
	if !replaced {
		// Some stacks omit this attribute. In that case, keep the SDP unchanged.
		return sdp
	}
	return strings.Join(lines, "\n")
}
