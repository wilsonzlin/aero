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
	// The current stack (`crates/aero-net-stack`) intentionally does not implement full
	// TCP reassembly, so we require ordered delivery at the tunnel layer.
	if !dc.Ordered() {
		return fmt.Errorf("l2 datachannel must be ordered (ordered=false)")
	}
	if dc.MaxPacketLifeTime() != nil {
		return fmt.Errorf("l2 datachannel must be fully reliable (maxPacketLifeTime must be unset)")
	}
	if dc.MaxRetransmits() != nil {
		return fmt.Errorf("l2 datachannel must be fully reliable (maxRetransmits must be unset)")
	}
	return nil
}
