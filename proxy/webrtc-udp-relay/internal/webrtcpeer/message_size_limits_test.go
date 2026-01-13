package webrtcpeer

import (
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
)

func TestWebRTCDataChannel_OversizeMessageClosesSession(t *testing.T) {
	// Use intentionally small limits so we can reliably trigger the error without
	// allocating large buffers in tests.
	cfg := config.Config{
		MaxDatagramPayloadBytes:          64,
		L2MaxMessageBytes:                128,
		WebRTCDataChannelMaxMessageBytes: 256,
		// Leave plenty of headroom relative to the message cap.
		WebRTCSCTPMaxReceiveBufferBytes: 1 << 20,
	}

	api, err := NewAPI(cfg)
	if err != nil {
		t.Fatalf("NewAPI: %v", err)
	}

	relayCfg := relay.DefaultConfig()
	relayCfg.MaxDatagramPayloadBytes = cfg.MaxDatagramPayloadBytes
	relayCfg.L2MaxMessageBytes = cfg.L2MaxMessageBytes

	var sessCloseOnce sync.Once
	sessClosed := make(chan struct{})
	sess, err := NewSession(
		api,
		nil,
		relayCfg,
		policy.NewDevDestinationPolicy(),
		nil,
		"example.com",
		"",
		nil,
		cfg.WebRTCDataChannelMaxMessageBytes,
		func() { sessCloseOnce.Do(func() { close(sessClosed) }) },
	)
	if err != nil {
		t.Fatalf("NewSession: %v", err)
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
	dc, err := clientPC.CreateDataChannel(DataChannelLabelUDP, &webrtc.DataChannelInit{
		Ordered:        &ordered,
		MaxRetransmits: &maxRetransmits,
	})
	if err != nil {
		t.Fatalf("CreateDataChannel(%q): %v", DataChannelLabelUDP, err)
	}

	dcOpen := make(chan struct{})
	dc.OnOpen(func() { close(dcOpen) })

	var dcCloseOnce sync.Once
	dcClosed := make(chan struct{})
	dc.OnClose(func() { dcCloseOnce.Do(func() { close(dcClosed) }) })

	// The server's SDP answer will advertise its (small) max-message-size. Pion
	// may honor this on the sending side and refuse to send oversized messages,
	// which makes it hard to exercise the receive-side guardrails.
	//
	// For test purposes, we rewrite the SDP answer seen by the client to claim a
	// very large max-message-size. The server still enforces its configured
	// SettingEngine receive cap.
	connectPeerConnectionsWithAnswerSDPTransform(t, clientPC, serverPC, func(sdp string) string {
		return forceSDPMaxMessageSize(sdp, 1<<30)
	})

	select {
	case <-dcOpen:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for datachannel open")
	}

	// Send a payload that exceeds the server-side cap by 1 byte.
	payload := make([]byte, cfg.WebRTCDataChannelMaxMessageBytes+1)
	if err := dc.Send(payload); err != nil {
		t.Fatalf("Send(oversize): %v", err)
	}

	// The relay should tear down either the DataChannel or the whole session.
	select {
	case <-dcClosed:
	case <-sessClosed:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for oversize message to close the datachannel/session")
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
