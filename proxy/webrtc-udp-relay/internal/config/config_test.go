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
	cfg, err := load(func(string) (string, bool) { return "", false }, nil)
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
	if cfg.PreferV2 {
		t.Fatalf("PreferV2=true, want false")
	}
	if cfg.MaxUDPBindingsPerSession != DefaultMaxUDPBindingsPerSession {
		t.Fatalf("MaxUDPBindingsPerSession=%d, want %d", cfg.MaxUDPBindingsPerSession, DefaultMaxUDPBindingsPerSession)
	}
}

func TestDefaultsProdWhenModeFlagSet(t *testing.T) {
	cfg, err := load(func(string) (string, bool) { return "", false }, []string{"--mode", "prod"})
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
	cfg, err := load(func(string) (string, bool) { return "", false }, []string{"--mode", "prod", "--log-format", "text"})
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.LogFormat != LogFormatText {
		t.Fatalf("logFormat=%q, want %q", cfg.LogFormat, LogFormatText)
	}
}

func TestWebRTCUDPPortRange_RequiresBoth(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		EnvWebRTCUDPPortMin: "40000",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestWebRTCUDPPortRange_TooSmall(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
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
		EnvWebRTCNAT1To1IPCandidateType: "nope",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestWebRTCNAT1To1IPs_InvalidIPs(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		EnvWebRTCNAT1To1IPs: "nope",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestWebRTCUDPListenIP(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
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
		EnvWebRTCUDPListenIP: "bad.ip",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}
