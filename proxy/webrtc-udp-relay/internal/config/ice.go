package config

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"

	"github.com/pion/webrtc/v4"
)

const (
	envICEServersJSON = "AERO_ICE_SERVERS_JSON"

	envStunURLs       = "AERO_STUN_URLS"
	envTurnURLs       = "AERO_TURN_URLS"
	envTurnUsername   = "AERO_TURN_USERNAME"
	envTurnCredential = "AERO_TURN_CREDENTIAL"
)

func parseICEServersFromValues(iceServersJSON, stunURLs, turnURLs, turnUsername, turnCredential string) ([]webrtc.ICEServer, error) {
	if raw := strings.TrimSpace(iceServersJSON); raw != "" {
		iceServers, err := ParseICEServersJSON(raw)
		if err != nil {
			return nil, fmt.Errorf("%s: %w", envICEServersJSON, err)
		}
		return iceServers, nil
	}

	iceServers, err := ParseICEServersFromConvenienceEnv(stunURLs, turnURLs, turnUsername, turnCredential)
	if err != nil {
		return nil, err
	}
	return iceServers, nil
}

type iceServerJSON struct {
	URLs       stringOrStringSlice `json:"urls"`
	Username   string              `json:"username,omitempty"`
	Credential string              `json:"credential,omitempty"`
}

type stringOrStringSlice []string

func (s *stringOrStringSlice) UnmarshalJSON(b []byte) error {
	var single string
	if err := json.Unmarshal(b, &single); err == nil {
		*s = []string{single}
		return nil
	}
	var many []string
	if err := json.Unmarshal(b, &many); err != nil {
		return err
	}
	*s = many
	return nil
}

// ParseICEServersJSON parses and validates AERO_ICE_SERVERS_JSON.
func ParseICEServersJSON(raw string) ([]webrtc.ICEServer, error) {
	var servers []iceServerJSON
	if err := json.Unmarshal([]byte(raw), &servers); err != nil {
		return nil, err
	}

	out := make([]webrtc.ICEServer, 0, len(servers))
	for i, server := range servers {
		urls := make([]string, 0, len(server.URLs))
		for _, url := range server.URLs {
			url = strings.TrimSpace(url)
			if url == "" {
				continue
			}
			urls = append(urls, url)
		}

		pcServer := webrtc.ICEServer{
			URLs:     urls,
			Username: strings.TrimSpace(server.Username),
		}
		if strings.TrimSpace(server.Credential) != "" {
			pcServer.Credential = server.Credential
		}

		if err := validateICEServer(pcServer); err != nil {
			return nil, fmt.Errorf("iceServers[%d]: %w", i, err)
		}
		out = append(out, pcServer)
	}
	return out, nil
}

// ParseICEServersFromConvenienceEnv builds an ICE server list from the convenience env vars.
//
// The URL lists are comma-separated.
func ParseICEServersFromConvenienceEnv(stunURLs, turnURLs, turnUsername, turnCredential string) ([]webrtc.ICEServer, error) {
	stunList := splitCommaSeparated(stunURLs)
	turnList := splitCommaSeparated(turnURLs)

	var servers []webrtc.ICEServer
	if len(stunList) > 0 {
		server := webrtc.ICEServer{URLs: stunList}
		if err := validateICEServer(server); err != nil {
			return nil, fmt.Errorf("%s: %w", envStunURLs, err)
		}
		servers = append(servers, server)
	}

	if len(turnList) > 0 {
		turnUsername = strings.TrimSpace(turnUsername)
		turnCredential = strings.TrimSpace(turnCredential)
		if turnUsername == "" || turnCredential == "" {
			return nil, fmt.Errorf("%s/%s: both must be set when %s is set", envTurnUsername, envTurnCredential, envTurnURLs)
		}

		server := webrtc.ICEServer{
			URLs:     turnList,
			Username: turnUsername,
		}
		server.Credential = turnCredential
		if err := validateICEServer(server); err != nil {
			return nil, fmt.Errorf("%s: %w", envTurnURLs, err)
		}
		servers = append(servers, server)
	}

	return servers, nil
}

func splitCommaSeparated(value string) []string {
	value = strings.TrimSpace(value)
	if value == "" {
		return nil
	}
	parts := strings.Split(value, ",")
	out := make([]string, 0, len(parts))
	for _, part := range parts {
		part = strings.TrimSpace(part)
		if part == "" {
			continue
		}
		out = append(out, part)
	}
	return out
}

func validateICEServer(server webrtc.ICEServer) error {
	if len(server.URLs) == 0 {
		return errors.New("missing urls")
	}

	requiresTurnCreds := false
	for _, raw := range server.URLs {
		url := strings.TrimSpace(raw)
		if url == "" {
			return errors.New("urls must not contain empty entries")
		}
		if !isAllowedICEScheme(url) {
			return fmt.Errorf("unsupported url scheme: %q", url)
		}
		if strings.HasPrefix(url, "turn:") || strings.HasPrefix(url, "turns:") {
			requiresTurnCreds = true
		}
	}

	if requiresTurnCreds {
		if strings.TrimSpace(server.Username) == "" {
			return errors.New("turn urls require username")
		}
		cred, ok := server.Credential.(string)
		if !ok || strings.TrimSpace(cred) == "" {
			return errors.New("turn urls require credential")
		}
	}

	return nil
}

func isAllowedICEScheme(url string) bool {
	switch {
	case strings.HasPrefix(url, "stun:"),
		strings.HasPrefix(url, "stuns:"),
		strings.HasPrefix(url, "turn:"),
		strings.HasPrefix(url, "turns:"):
		return true
	default:
		return false
	}
}
