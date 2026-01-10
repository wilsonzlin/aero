package httpserver

import (
	"strings"

	"github.com/pion/webrtc/v4"
)

func withTURNRESTCredentials(servers []webrtc.ICEServer, username, credential string) []webrtc.ICEServer {
	if len(servers) == 0 {
		return nil
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
		url := strings.ToLower(strings.TrimSpace(raw))
		if strings.HasPrefix(url, "turn:") || strings.HasPrefix(url, "turns:") {
			return true
		}
	}
	return false
}

