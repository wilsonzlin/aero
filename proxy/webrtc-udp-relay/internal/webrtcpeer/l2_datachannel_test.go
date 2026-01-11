package webrtcpeer

import (
	"testing"
	"time"

	"github.com/pion/webrtc/v4"
)

func connectPeerConnections(t *testing.T, offerer, answerer *webrtc.PeerConnection) {
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
	if err := offerer.SetRemoteDescription(*answerSDP); err != nil {
		t.Fatalf("SetRemoteDescription(answer): %v", err)
	}
}

func TestL2DataChannelSemantics_OrderedReliable(t *testing.T) {
	api := webrtc.NewAPI()

	serverPC, err := api.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("NewPeerConnection(server): %v", err)
	}
	t.Cleanup(func() { _ = serverPC.Close() })

	clientPC, err := api.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("NewPeerConnection(client): %v", err)
	}
	t.Cleanup(func() { _ = clientPC.Close() })

	type result struct {
		ordered  bool
		maxRet   *uint16
		maxLife  *uint16
		validate error
	}
	gotCh := make(chan result, 1)

	serverPC.OnDataChannel(func(dc *webrtc.DataChannel) {
		if dc.Label() != DataChannelLabelL2 {
			return
		}
		gotCh <- result{
			ordered:  dc.Ordered(),
			maxRet:   dc.MaxRetransmits(),
			maxLife:  dc.MaxPacketLifeTime(),
			validate: validateL2DataChannel(dc),
		}
	})

	ordered := true
	if _, err := clientPC.CreateDataChannel(DataChannelLabelL2, &webrtc.DataChannelInit{Ordered: &ordered}); err != nil {
		t.Fatalf("CreateDataChannel(%q): %v", DataChannelLabelL2, err)
	}

	connectPeerConnections(t, clientPC, serverPC)

	select {
	case got := <-gotCh:
		if got.validate != nil {
			t.Fatalf("validateL2DataChannel: %v", got.validate)
		}
		if !got.ordered {
			t.Fatalf("l2 datachannel should be ordered")
		}
		if got.maxRet != nil {
			t.Fatalf("l2 datachannel should not set maxRetransmits")
		}
		if got.maxLife != nil {
			t.Fatalf("l2 datachannel should not set maxPacketLifeTime")
		}
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for server-side l2 datachannel")
	}
}

func TestL2DataChannelSemantics_RejectsUnordered(t *testing.T) {
	api := webrtc.NewAPI()

	serverPC, err := api.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("NewPeerConnection(server): %v", err)
	}
	t.Cleanup(func() { _ = serverPC.Close() })

	clientPC, err := api.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("NewPeerConnection(client): %v", err)
	}
	t.Cleanup(func() { _ = clientPC.Close() })

	gotCh := make(chan error, 1)
	serverPC.OnDataChannel(func(dc *webrtc.DataChannel) {
		if dc.Label() != DataChannelLabelL2 {
			return
		}
		gotCh <- validateL2DataChannel(dc)
	})

	ordered := false
	if _, err := clientPC.CreateDataChannel(DataChannelLabelL2, &webrtc.DataChannelInit{Ordered: &ordered}); err != nil {
		t.Fatalf("CreateDataChannel(%q): %v", DataChannelLabelL2, err)
	}

	connectPeerConnections(t, clientPC, serverPC)

	select {
	case err := <-gotCh:
		if err == nil {
			t.Fatalf("expected validateL2DataChannel to reject unordered l2 datachannel")
		}
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for server-side l2 datachannel")
	}
}
