package webrtcpeer

import (
	"fmt"

	"github.com/pion/webrtc/v4"
)

const (
	// DataChannelLabelUDP is the DataChannel label used for the UDP relay framing
	// protocol described in `proxy/webrtc-udp-relay/PROTOCOL.md`.
	DataChannelLabelUDP = "udp"

	// DataChannelLabelL2 is the DataChannel label used for the Option C L2 tunnel
	// (raw Ethernet frames). See `docs/l2-tunnel-protocol.md`.
	DataChannelLabelL2 = "l2"
)

func validateUDPDataChannel(dc *webrtc.DataChannel) error {
	if dc.Label() != DataChannelLabelUDP {
		return fmt.Errorf("expected label=%q (got %q)", DataChannelLabelUDP, dc.Label())
	}
	// UDP relay aims to emulate UDP: unordered + unreliable.
	if dc.Ordered() {
		return fmt.Errorf("udp datachannel must be unordered (ordered=true)")
	}
	if dc.MaxPacketLifeTime() != nil {
		return fmt.Errorf("udp datachannel must not set maxPacketLifeTime (use maxRetransmits=0)")
	}
	maxRetransmits := dc.MaxRetransmits()
	if maxRetransmits == nil || *maxRetransmits != 0 {
		return fmt.Errorf("udp datachannel must set maxRetransmits=0")
	}
	return nil
}

func validateL2DataChannel(dc *webrtc.DataChannel) error {
	if dc.Label() != DataChannelLabelL2 {
		return fmt.Errorf("expected label=%q (got %q)", DataChannelLabelL2, dc.Label())
	}
	// L2 is a raw Ethernet tunnel that carries TCP segments to a user-space stack.
	// The tunnel MUST be reliable (no partial reliability); ordering is optional.
	if dc.MaxPacketLifeTime() != nil {
		return fmt.Errorf("l2 datachannel must be fully reliable (maxPacketLifeTime must be unset)")
	}
	if dc.MaxRetransmits() != nil {
		return fmt.Errorf("l2 datachannel must be fully reliable (maxRetransmits must be unset)")
	}
	return nil
}

// NewL2DataChannelInit returns the recommended DataChannelInit for the L2
// tunnel.
//
// The L2 tunnel MUST be reliable. Do not set MaxRetransmits/MaxPacketLifeTime.
// Using ordered=false is recommended to reduce head-of-line blocking.
func NewL2DataChannelInit() *webrtc.DataChannelInit {
	ordered := false
	return &webrtc.DataChannelInit{
		Ordered: &ordered,
	}
}

// CreateL2DataChannel creates a DataChannel labeled "l2" with reliable delivery
// semantics.
func CreateL2DataChannel(pc *webrtc.PeerConnection) (*webrtc.DataChannel, error) {
	return pc.CreateDataChannel(DataChannelLabelL2, NewL2DataChannelInit())
}
