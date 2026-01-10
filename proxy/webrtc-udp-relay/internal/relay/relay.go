package relay

import (
	"fmt"
	"net"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
)

// UDPRelay is a minimal outbound UDP sender used by the WebRTC relay server.
//
// The critical security invariant is that DestinationPolicy is enforced before
// opening a UDP binding and before sending each datagram.
type UDPRelay struct {
	Policy *policy.DestinationPolicy
}

func (r *UDPRelay) Send(remote *net.UDPAddr, payload []byte) error {
	if remote == nil {
		return fmt.Errorf("udp relay: remote addr is nil")
	}
	if r.Policy == nil {
		return fmt.Errorf("udp relay: policy is nil")
	}

	// Enforce policy before opening the UDP socket/binding.
	if err := r.Policy.AllowUDP(remote.IP, uint16(remote.Port)); err != nil {
		return err
	}

	conn, err := net.DialUDP("udp", nil, remote)
	if err != nil {
		return fmt.Errorf("udp relay: dial %s: %w", remote.String(), err)
	}
	defer conn.Close()

	// Enforce policy again immediately before sending the datagram.
	if err := r.Policy.AllowUDP(remote.IP, uint16(remote.Port)); err != nil {
		return err
	}

	if _, err := conn.Write(payload); err != nil {
		return fmt.Errorf("udp relay: write %s: %w", remote.String(), err)
	}
	return nil
}

