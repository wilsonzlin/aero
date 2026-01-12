package httpserver

import (
	"strings"

	"github.com/pion/webrtc/v4"
)

func withTURNRESTCredentials(servers []webrtc.ICEServer, username, credential string) []webrtc.ICEServer {
	if len(servers) == 0 {
		// Preserve empty (non-nil) slices so JSON responses consistently encode as
		// `[]` rather than `null`.
		return servers
	}
	out := make([]webrtc.ICEServer, len(servers))
	for i, server := range servers {
		out[i] = server
		if iceServerHasTURNURL(server) {
			out[i].Username = username
			out[i].Credential = credential
		}
	}
	return out
}

func iceServerHasTURNURL(server webrtc.ICEServer) bool {
	for _, raw := range server.URLs {
		url := strings.TrimSpace(raw)
		if asciiHasPrefixFold(url, "turn:") || asciiHasPrefixFold(url, "turns:") {
			return true
		}
	}
	return false
}

func asciiHasPrefixFold(s, prefix string) bool {
	if len(s) < len(prefix) {
		return false
	}
	for i := 0; i < len(prefix); i++ {
		c := s[i]
		if c >= 'A' && c <= 'Z' {
			c = c + ('a' - 'A')
		}
		if c != prefix[i] {
			return false
		}
	}
	return true
}
