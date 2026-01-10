package webrtcpeer

import (
	"github.com/pion/webrtc/v4"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
)

// NewPeerConnection constructs the server-side PeerConnection.
//
// The relay typically works with only host/public candidates, but STUN/TURN can help
// in NAT'd environments.
func NewPeerConnection(cfg config.Config) (*webrtc.PeerConnection, error) {
	return webrtc.NewPeerConnection(webrtc.Configuration{
		ICEServers: cfg.ICEServers,
	})
}
