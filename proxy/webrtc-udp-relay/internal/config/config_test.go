package config

import (
	"net"
	"strings"
	"testing"
)

func lookupMap(m map[string]string) func(string) (string, bool) {
	return func(key string) (string, bool) {
		v, ok := m[key]
		return v, ok
	}
}

func TestDefaultsDev(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		EnvAPIKey: "secret",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.Mode != ModeDev {
		t.Fatalf("mode=%q, want %q", cfg.Mode, ModeDev)
	}
	if cfg.LogFormat != LogFormatText {
		t.Fatalf("logFormat=%q, want %q", cfg.LogFormat, LogFormatText)
	}
	if cfg.WebRTCUDPPortRange != nil {
		t.Fatalf("expected WebRTCUDPPortRange unset, got %+v", *cfg.WebRTCUDPPortRange)
	}
	if !cfg.WebRTCUDPListenIP.Equal(net.IPv4zero) {
		t.Fatalf("WebRTCUDPListenIP=%v, want 0.0.0.0", cfg.WebRTCUDPListenIP)
	}
	if cfg.WebRTCNAT1To1IPCandidateType != NAT1To1CandidateTypeHost {
		t.Fatalf("WebRTCNAT1To1IPCandidateType=%q, want %q", cfg.WebRTCNAT1To1IPCandidateType, NAT1To1CandidateTypeHost)
	}
	if len(cfg.WebRTCNAT1To1IPs) != 0 {
		t.Fatalf("expected WebRTCNAT1To1IPs empty, got %v", cfg.WebRTCNAT1To1IPs)
	}
	if cfg.UDPBindingIdleTimeout != DefaultUDPBindingIdleTimeout {
		t.Fatalf("UDPBindingIdleTimeout=%v, want %v", cfg.UDPBindingIdleTimeout, DefaultUDPBindingIdleTimeout)
	}
	if cfg.UDPReadBufferBytes != DefaultUDPReadBufferBytes {
		t.Fatalf("UDPReadBufferBytes=%d, want %d", cfg.UDPReadBufferBytes, DefaultUDPReadBufferBytes)
	}
	if cfg.DataChannelSendQueueBytes != DefaultDataChannelSendQueueBytes {
		t.Fatalf("DataChannelSendQueueBytes=%d, want %d", cfg.DataChannelSendQueueBytes, DefaultDataChannelSendQueueBytes)
	}
	if cfg.MaxDatagramPayloadBytes != DefaultMaxDatagramPayloadBytes {
		t.Fatalf("MaxDatagramPayloadBytes=%d, want %d", cfg.MaxDatagramPayloadBytes, DefaultMaxDatagramPayloadBytes)
	}
	if cfg.PreferV2 {
		t.Fatalf("PreferV2=true, want false")
	}
	if cfg.L2BackendWSURL != "" {
		t.Fatalf("L2BackendWSURL=%q, want empty", cfg.L2BackendWSURL)
	}
	if cfg.L2BackendWSOrigin != "" {
		t.Fatalf("L2BackendWSOrigin=%q, want empty", cfg.L2BackendWSOrigin)
	}
	if cfg.L2BackendWSToken != "" {
		t.Fatalf("L2BackendWSToken=%q, want empty", cfg.L2BackendWSToken)
	}
	if cfg.L2BackendForwardOrigin {
		t.Fatalf("L2BackendForwardOrigin=true, want false")
	}
	if cfg.L2BackendAuthForwardMode != L2BackendAuthForwardModeQuery {
		t.Fatalf("L2BackendAuthForwardMode=%q, want %q", cfg.L2BackendAuthForwardMode, L2BackendAuthForwardModeQuery)
	}
	if cfg.L2MaxMessageBytes != DefaultL2MaxMessageBytes {
		t.Fatalf("L2MaxMessageBytes=%d, want %d", cfg.L2MaxMessageBytes, DefaultL2MaxMessageBytes)
	}
	if cfg.MaxUDPBindingsPerSession != DefaultMaxUDPBindingsPerSession {
		t.Fatalf("MaxUDPBindingsPerSession=%d, want %d", cfg.MaxUDPBindingsPerSession, DefaultMaxUDPBindingsPerSession)
	}
}

func TestMaxDatagramPayloadBytes_EnvOverride(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		EnvAPIKey:                  "secret",
		EnvMaxDatagramPayloadBytes: "1400",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.MaxDatagramPayloadBytes != 1400 {
		t.Fatalf("MaxDatagramPayloadBytes=%d, want %d", cfg.MaxDatagramPayloadBytes, 1400)
	}
}

func TestDefaultsProdWhenModeFlagSet(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		EnvAPIKey: "secret",
	}), []string{"--mode", "prod"})
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.Mode != ModeProd {
		t.Fatalf("mode=%q, want %q", cfg.Mode, ModeProd)
	}
	if cfg.LogFormat != LogFormatJSON {
		t.Fatalf("logFormat=%q, want %q", cfg.LogFormat, LogFormatJSON)
	}
}

func TestLogFormatExplicitOverride(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		EnvAPIKey: "secret",
	}), []string{"--mode", "prod", "--log-format", "text"})
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.LogFormat != LogFormatText {
		t.Fatalf("logFormat=%q, want %q", cfg.LogFormat, LogFormatText)
	}
}

func TestWebRTCUDPPortRange_RequiresBoth(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		EnvAPIKey:           "secret",
		EnvWebRTCUDPPortMin: "40000",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestWebRTCUDPPortRange_TooSmall(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		EnvAPIKey:           "secret",
		EnvWebRTCUDPPortMin: "40000",
		EnvWebRTCUDPPortMax: "40010",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
	if !strings.Contains(err.Error(), "too small") {
		t.Fatalf("err=%v, expected mention of too small range", err)
	}
}

func TestWebRTCUDPPortRange_OK(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		EnvAPIKey:           "secret",
		EnvWebRTCUDPPortMin: "40000",
		EnvWebRTCUDPPortMax: "40199",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.WebRTCUDPPortRange == nil {
		t.Fatalf("expected WebRTCUDPPortRange set")
	}
	if cfg.WebRTCUDPPortRange.Min != 40000 || cfg.WebRTCUDPPortRange.Max != 40199 {
		t.Fatalf("WebRTCUDPPortRange=%+v", *cfg.WebRTCUDPPortRange)
	}
}

func TestWebRTCNAT1To1IPsAndCandidateType(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		EnvAPIKey:                       "secret",
		EnvWebRTCNAT1To1IPs:             "203.0.113.10, 203.0.113.11",
		EnvWebRTCNAT1To1IPCandidateType: "srflx",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if got, want := len(cfg.WebRTCNAT1To1IPs), 2; got != want {
		t.Fatalf("len(WebRTCNAT1To1IPs)=%d, want %d", got, want)
	}
	if cfg.WebRTCNAT1To1IPCandidateType != NAT1To1CandidateTypeSrflx {
		t.Fatalf("WebRTCNAT1To1IPCandidateType=%q, want %q", cfg.WebRTCNAT1To1IPCandidateType, NAT1To1CandidateTypeSrflx)
	}
}

func TestWebRTCNAT1To1IPs_InvalidCandidateType(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		EnvAPIKey:                       "secret",
		EnvWebRTCNAT1To1IPCandidateType: "nope",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestWebRTCNAT1To1IPs_InvalidIPs(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		EnvAPIKey:           "secret",
		EnvWebRTCNAT1To1IPs: "nope",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestWebRTCUDPListenIP(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		EnvAPIKey:            "secret",
		EnvWebRTCUDPListenIP: "10.0.0.123",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if !cfg.WebRTCUDPListenIP.Equal(net.ParseIP("10.0.0.123")) {
		t.Fatalf("WebRTCUDPListenIP=%v", cfg.WebRTCUDPListenIP)
	}
}

func TestWebRTCUDPListenIP_Invalid(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		EnvAPIKey:            "secret",
		EnvWebRTCUDPListenIP: "bad.ip",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestAuthModeAPIKey_RequiresAPIKey(t *testing.T) {
	_, err := load(func(string) (string, bool) { return "", false }, nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
	if !strings.Contains(err.Error(), EnvAPIKey) {
		t.Fatalf("err=%v, expected mention of %s", err, EnvAPIKey)
	}
}

func TestL2BackendWSURL_ValidatesSchemeAndHost(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		EnvAPIKey:         "secret",
		EnvL2BackendWSURL: "http://example.com/l2",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}

	_, err = load(lookupMap(map[string]string{
		EnvAPIKey:         "secret",
		EnvL2BackendWSURL: "ws:///l2",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestL2BackendWSURL_AcceptsWebSocketURL(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		EnvAPIKey:            "secret",
		EnvL2BackendWSURL:    "ws://127.0.0.1:8090/l2",
		EnvL2BackendWSOrigin: "HTTPS://Example.COM:443/",
		EnvL2BackendWSToken:  "test-token",
		EnvL2MaxMessageBytes: "2048",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.L2BackendWSURL != "ws://127.0.0.1:8090/l2" {
		t.Fatalf("L2BackendWSURL=%q", cfg.L2BackendWSURL)
	}
	if cfg.L2BackendWSOrigin != "https://example.com:443" {
		t.Fatalf("L2BackendWSOrigin=%q", cfg.L2BackendWSOrigin)
	}
	if cfg.L2BackendWSToken != "test-token" {
		t.Fatalf("L2BackendWSToken=%q", cfg.L2BackendWSToken)
	}
	if !cfg.L2BackendForwardOrigin {
		t.Fatalf("expected L2BackendForwardOrigin default true when L2 is enabled")
	}
	if cfg.L2BackendAuthForwardMode != L2BackendAuthForwardModeQuery {
		t.Fatalf("L2BackendAuthForwardMode=%q, want %q", cfg.L2BackendAuthForwardMode, L2BackendAuthForwardModeQuery)
	}
	if cfg.L2MaxMessageBytes != 2048 {
		t.Fatalf("L2MaxMessageBytes=%d, want 2048", cfg.L2MaxMessageBytes)
	}
}

func TestL2BackendWSToken_AcceptsHTTPToken(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		EnvAPIKey:           "secret",
		EnvL2BackendWSToken: "jwt_like.token-123",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.L2BackendWSToken != "jwt_like.token-123" {
		t.Fatalf("L2BackendWSToken=%q", cfg.L2BackendWSToken)
	}
}

func TestL2BackendWSToken_RejectsInvalidToken(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		EnvAPIKey:           "secret",
		EnvL2BackendWSToken: "not a token",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
	if !strings.Contains(err.Error(), EnvL2BackendWSToken) {
		t.Fatalf("expected error mentioning %s (err=%v)", EnvL2BackendWSToken, err)
	}
}

func TestL2BackendToken_EnvAlias_AcceptsHTTPToken(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		EnvAPIKey:        "secret",
		EnvL2BackendToken: "jwt_like.token-123",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.L2BackendWSToken != "jwt_like.token-123" {
		t.Fatalf("L2BackendWSToken=%q", cfg.L2BackendWSToken)
	}
}

func TestL2BackendOrigin_EnvAlias_NormalizesAndValidates(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		EnvAPIKey:         "secret",
		EnvL2BackendWSURL: "ws://127.0.0.1:8090/l2",
		EnvL2BackendOrigin: "HTTPS://Example.COM:443/",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.L2BackendWSOrigin != "https://example.com:443" {
		t.Fatalf("L2BackendWSOrigin=%q", cfg.L2BackendWSOrigin)
	}
}

func TestL2BackendOrigin_EnvAlias_RejectsPath(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		EnvAPIKey:         "secret",
		EnvL2BackendOrigin: "https://example.com/path",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestL2BackendToken_EnvAlias_RejectsComma(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		EnvAPIKey:        "secret",
		EnvL2BackendToken: "abc,def",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestL2BackendAuthForwardMode_Subprotocol(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		EnvAPIKey:                   "secret",
		EnvL2BackendWSURL:           "ws://127.0.0.1:8090/l2",
		EnvL2BackendAuthForwardMode: "subprotocol",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.L2BackendAuthForwardMode != L2BackendAuthForwardModeSubprotocol {
		t.Fatalf("L2BackendAuthForwardMode=%q, want %q", cfg.L2BackendAuthForwardMode, L2BackendAuthForwardModeSubprotocol)
	}
}

func TestL2BackendOriginOverride_NormalizesAndValidates(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		EnvAPIKey:                  "secret",
		EnvL2BackendWSURL:          "ws://127.0.0.1:8090/l2",
		EnvL2BackendOriginOverride: "HTTPS://Example.COM:443/",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.L2BackendWSOrigin != "https://example.com:443" {
		t.Fatalf("L2BackendWSOrigin=%q, want %q", cfg.L2BackendWSOrigin, "https://example.com:443")
	}
}

func TestParseAllowedOrigins_NormalizesAndValidates(t *testing.T) {
	got, err := parseAllowedOrigins("HTTPS://Example.COM:443, http://localhost:5173/")
	if err != nil {
		t.Fatalf("parseAllowedOrigins: %v", err)
	}
	if len(got) != 2 {
		t.Fatalf("len=%d, want 2 (%v)", len(got), got)
	}
	if got[0] != "https://example.com:443" {
		t.Fatalf("got[0]=%q, want %q", got[0], "https://example.com:443")
	}
	if got[1] != "http://localhost:5173" {
		t.Fatalf("got[1]=%q, want %q", got[1], "http://localhost:5173")
	}
}

func TestParseAllowedOrigins_AllowsStarAndNull(t *testing.T) {
	got, err := parseAllowedOrigins("*,null")
	if err != nil {
		t.Fatalf("parseAllowedOrigins: %v", err)
	}
	if len(got) != 2 || got[0] != "*" || got[1] != "null" {
		t.Fatalf("got=%v, want [* null]", got)
	}
}

func TestParseAllowedOrigins_RejectsPathQueryAndCredentials(t *testing.T) {
	cases := []string{
		"ftp://example.com",
		"https://example.com/path",
		"https://example.com/?q=1",
		"https://user@example.com",
		"https://example.com/#frag",
	}
	for _, raw := range cases {
		if _, err := parseAllowedOrigins(raw); err == nil {
			t.Fatalf("expected error for %q, got nil", raw)
		}
	}
}
