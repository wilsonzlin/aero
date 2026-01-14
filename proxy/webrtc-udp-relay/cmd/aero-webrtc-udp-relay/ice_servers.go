package main

import (
	"strings"

	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
)

// peerConnectionICEServers returns the ICE server list to use when constructing
// server-side PeerConnections.
//
// When TURN REST is enabled, the client-facing ICE list may include TURN URLs
// without credentials (because credentials are injected per /webrtc/ice request).
// Pion requires TURN credentials for server-side usage, so we filter out TURN
// servers that don't have complete credentials.
func peerConnectionICEServers(cfg config.Config) []webrtc.ICEServer {
	if !cfg.TURNREST.Enabled() {
		return cfg.ICEServers
	}

	out := make([]webrtc.ICEServer, 0, len(cfg.ICEServers))
	for _, server := range cfg.ICEServers {
		if !iceServerHasTURNURL(server) {
			out = append(out, server)
			continue
		}
		if strings.TrimSpace(server.Username) == "" {
			continue
		}
		cred, ok := server.Credential.(string)
		if !ok || strings.TrimSpace(cred) == "" {
			continue
		}
		out = append(out, server)
	}
	return out
}

func iceServerHasTURNURL(server webrtc.ICEServer) bool {
	for _, raw := range server.URLs {
		url := strings.ToLower(strings.TrimSpace(raw))
		if strings.HasPrefix(url, "turn:") || strings.HasPrefix(url, "turns:") {
			return true
		}
	}
	return false
}
