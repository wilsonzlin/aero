package webrtcpeer

import (
	"testing"
	"time"

	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
)

func TestWebRTCDataChannel_OversizeMessageNotDelivered(t *testing.T) {
	// The primary DoS risk is that extremely large user messages could be fully
	// reassembled/buffered by pion/SCTP before DataChannel.OnMessage is invoked.
	//
	// Pion's SCTP max message size setting only influences SDP negotiation (send
	// limits for compliant peers). The receive-side hard cap is enforced via the
	// SCTP receive buffer size.
	cfg := config.Config{
		// Advertise a large max message size so the (pion-based) client will send
		// the oversized message. The server-side receive buffer should prevent the
		// oversized message from being delivered to OnMessage.
		WebRTCDataChannelMaxMessageBytes: 1 << 30,
		// Keep the receive buffer small enough that a large message cannot be
		// fully reassembled.
		WebRTCSCTPMaxReceiveBufferBytes: 16 * 1024,
	}

	api, err := NewAPI(cfg)
	if err != nil {
		t.Fatalf("NewAPI: %v", err)
	}

	serverPC, err := api.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("NewPeerConnection(server): %v", err)
	}
	t.Cleanup(func() { _ = serverPC.Close() })

	clientPC, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("NewPeerConnection(client): %v", err)
	}
	t.Cleanup(func() { _ = clientPC.Close() })

	received := make(chan int, 8)
	serverOpen := make(chan struct{})

	serverPC.OnDataChannel(func(dc *webrtc.DataChannel) {
		if dc.Label() != DataChannelLabelUDP {
			return
		}
		dc.OnOpen(func() {
			select {
			case <-serverOpen:
			default:
				close(serverOpen)
			}
		})
		dc.OnMessage(func(msg webrtc.DataChannelMessage) {
			if msg.IsString {
				return
			}
			select {
			case received <- len(msg.Data):
			default:
			}
		})
	})

	ordered := false
	maxRetransmits := uint16(0)
	clientDC, err := clientPC.CreateDataChannel(DataChannelLabelUDP, &webrtc.DataChannelInit{
		Ordered:        &ordered,
		MaxRetransmits: &maxRetransmits,
	})
	if err != nil {
		t.Fatalf("CreateDataChannel(%q): %v", DataChannelLabelUDP, err)
	}
	clientOpen := make(chan struct{})
	clientDC.OnOpen(func() { close(clientOpen) })

	connectPeerConnections(t, clientPC, serverPC)

	select {
	case <-clientOpen:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for client datachannel open")
	}
	select {
	case <-serverOpen:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for server datachannel open")
	}

	oversize := make([]byte, cfg.WebRTCSCTPMaxReceiveBufferBytes*4) // 64KiB
	if err := clientDC.Send(oversize); err != nil {
		t.Fatalf("Send(oversize): %v", err)
	}

	// The oversized message should not be delivered to the receiver's OnMessage
	// because the SCTP receive buffer cap prevents full reassembly.
	select {
	case n := <-received:
		t.Fatalf("unexpected server-side delivery of oversized message: %d bytes", n)
	case <-time.After(750 * time.Millisecond):
		// ok
	}
}

