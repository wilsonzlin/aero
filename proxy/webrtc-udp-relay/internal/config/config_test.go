package config

import (
	"net"
	"strings"
	"testing"
	"time"
)

func lookupMap(m map[string]string) func(string) (string, bool) {
	return func(key string) (string, bool) {
		v, ok := m[key]
		return v, ok
	}
}

func TestDefaultsDev(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey: "secret",
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
	if cfg.WebRTCSessionConnectTimeout != DefaultWebRTCSessionConnectTimeout {
		t.Fatalf("WebRTCSessionConnectTimeout=%v, want %v", cfg.WebRTCSessionConnectTimeout, DefaultWebRTCSessionConnectTimeout)
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
	wantDCMax := defaultWebRTCDataChannelMaxMessageBytes(DefaultMaxDatagramPayloadBytes, DefaultL2MaxMessageBytes)
	if cfg.WebRTCDataChannelMaxMessageBytes != wantDCMax {
		t.Fatalf("WebRTCDataChannelMaxMessageBytes=%d, want %d", cfg.WebRTCDataChannelMaxMessageBytes, wantDCMax)
	}
	wantSCTPBuf := defaultWebRTCSCTPMaxReceiveBufferBytes(wantDCMax)
	if cfg.WebRTCSCTPMaxReceiveBufferBytes != wantSCTPBuf {
		t.Fatalf("WebRTCSCTPMaxReceiveBufferBytes=%d, want %d", cfg.WebRTCSCTPMaxReceiveBufferBytes, wantSCTPBuf)
	}
	if cfg.UDPBindingIdleTimeout != DefaultUDPBindingIdleTimeout {
		t.Fatalf("UDPBindingIdleTimeout=%v, want %v", cfg.UDPBindingIdleTimeout, DefaultUDPBindingIdleTimeout)
	}
	if cfg.UDPInboundFilterMode != DefaultUDPInboundFilterMode {
		t.Fatalf("UDPInboundFilterMode=%q, want %q", cfg.UDPInboundFilterMode, DefaultUDPInboundFilterMode)
	}
	if cfg.UDPRemoteAllowlistIdleTimeout != cfg.UDPBindingIdleTimeout {
		t.Fatalf("UDPRemoteAllowlistIdleTimeout=%v, want %v", cfg.UDPRemoteAllowlistIdleTimeout, cfg.UDPBindingIdleTimeout)
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
	if cfg.MaxAllowedRemotesPerBinding != DefaultMaxAllowedRemotesPerBinding {
		t.Fatalf("MaxAllowedRemotesPerBinding=%d, want %d", cfg.MaxAllowedRemotesPerBinding, DefaultMaxAllowedRemotesPerBinding)
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
	if cfg.L2BackendForwardAeroSession {
		t.Fatalf("L2BackendForwardAeroSession=true, want false")
	}
	if cfg.L2MaxMessageBytes != DefaultL2MaxMessageBytes {
		t.Fatalf("L2MaxMessageBytes=%d, want %d", cfg.L2MaxMessageBytes, DefaultL2MaxMessageBytes)
	}
	if cfg.MaxUDPBindingsPerSession != DefaultMaxUDPBindingsPerSession {
		t.Fatalf("MaxUDPBindingsPerSession=%d, want %d", cfg.MaxUDPBindingsPerSession, DefaultMaxUDPBindingsPerSession)
	}
	if cfg.MaxUDPDestBucketsPerSession != defaultMaxUDPDestBucketsPerSession {
		t.Fatalf("MaxUDPDestBucketsPerSession=%d, want %d", cfg.MaxUDPDestBucketsPerSession, defaultMaxUDPDestBucketsPerSession)
	}
	if cfg.SessionPreallocTTL != DefaultSessionPreallocTTL {
		t.Fatalf("SessionPreallocTTL=%v, want %v", cfg.SessionPreallocTTL, DefaultSessionPreallocTTL)
	}
	if cfg.SignalingWSIdleTimeout != DefaultSignalingWSIdleTimeout {
		t.Fatalf("SignalingWSIdleTimeout=%v, want %v", cfg.SignalingWSIdleTimeout, DefaultSignalingWSIdleTimeout)
	}
	if cfg.SignalingWSPingInterval != DefaultSignalingWSPingInterval {
		t.Fatalf("SignalingWSPingInterval=%v, want %v", cfg.SignalingWSPingInterval, DefaultSignalingWSPingInterval)
	}
	if cfg.UDPWSIdleTimeout != DefaultUDPWSIdleTimeout {
		t.Fatalf("UDPWSIdleTimeout=%v, want %v", cfg.UDPWSIdleTimeout, DefaultUDPWSIdleTimeout)
	}
	if cfg.UDPWSPingInterval != DefaultUDPWSPingInterval {
		t.Fatalf("UDPWSPingInterval=%v, want %v", cfg.UDPWSPingInterval, DefaultUDPWSPingInterval)
	}
}

func TestMaxUDPDestBuckets_DefaultsToMaxUniqueDestinations(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:                          "secret",
		envVarMaxUniqueDestinationsPerSession: "123",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.MaxUDPDestBucketsPerSession != 123 {
		t.Fatalf("MaxUDPDestBucketsPerSession=%d, want %d", cfg.MaxUDPDestBucketsPerSession, 123)
	}
}

func TestMaxUDPDestBuckets_EnvOverride(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:                          "secret",
		envVarMaxUniqueDestinationsPerSession: "123",
		envVarMaxUDPDestBucketsPerSession:     "7",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.MaxUDPDestBucketsPerSession != 7 {
		t.Fatalf("MaxUDPDestBucketsPerSession=%d, want %d", cfg.MaxUDPDestBucketsPerSession, 7)
	}
}

func TestMaxUDPDestBuckets_DefaultsToMaxUniqueDestinations_Flag(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey: "secret",
	}), []string{"--max-unique-destinations-per-session", "123"})
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.MaxUDPDestBucketsPerSession != 123 {
		t.Fatalf("MaxUDPDestBucketsPerSession=%d, want %d", cfg.MaxUDPDestBucketsPerSession, 123)
	}
}

func TestSessionPreallocTTL_EnvOverride(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:             "secret",
		envVarSessionPreallocTTL: "5s",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.SessionPreallocTTL != 5*time.Second {
		t.Fatalf("SessionPreallocTTL=%v, want %v", cfg.SessionPreallocTTL, 5*time.Second)
	}
}

func TestSessionPreallocTTL_FlagOverride(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:             "secret",
		envVarSessionPreallocTTL: "5s",
	}), []string{"--session-prealloc-ttl", "10s"})
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.SessionPreallocTTL != 10*time.Second {
		t.Fatalf("SessionPreallocTTL=%v, want %v", cfg.SessionPreallocTTL, 10*time.Second)
	}
}

func TestSessionPreallocTTL_RejectsZero(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		envVarAPIKey:             "secret",
		envVarSessionPreallocTTL: "0s",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestMaxDatagramPayloadBytes_EnvOverride(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:                  "secret",
		envVarMaxDatagramPayloadBytes: "1400",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.MaxDatagramPayloadBytes != 1400 {
		t.Fatalf("MaxDatagramPayloadBytes=%d, want %d", cfg.MaxDatagramPayloadBytes, 1400)
	}
	if cfg.UDPReadBufferBytes != 1401 {
		t.Fatalf("UDPReadBufferBytes=%d, want %d (max payload + 1)", cfg.UDPReadBufferBytes, 1401)
	}
}

func TestUDPReadBufferBytes_RequiresMaxDatagramPayloadBytesPlusOne(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		envVarAPIKey:                  "secret",
		envVarUDPReadBufferBytes:      "1200",
		envVarMaxDatagramPayloadBytes: "1200",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
	if !strings.Contains(err.Error(), envVarUDPReadBufferBytes) {
		t.Fatalf("err=%v, expected mention of %s", err, envVarUDPReadBufferBytes)
	}
}

func TestWebSocketTimeouts_EnvOverride(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:                  "secret",
		envVarSignalingWSIdleTimeout:  "10s",
		envVarSignalingWSPingInterval: "3s",
		envVarUDPWSIdleTimeout:        "11s",
		envVarUDPWSPingInterval:       "4s",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.SignalingWSIdleTimeout != 10*time.Second {
		t.Fatalf("SignalingWSIdleTimeout=%v, want %v", cfg.SignalingWSIdleTimeout, 10*time.Second)
	}
	if cfg.SignalingWSPingInterval != 3*time.Second {
		t.Fatalf("SignalingWSPingInterval=%v, want %v", cfg.SignalingWSPingInterval, 3*time.Second)
	}
	if cfg.UDPWSIdleTimeout != 11*time.Second {
		t.Fatalf("UDPWSIdleTimeout=%v, want %v", cfg.UDPWSIdleTimeout, 11*time.Second)
	}
	if cfg.UDPWSPingInterval != 4*time.Second {
		t.Fatalf("UDPWSPingInterval=%v, want %v", cfg.UDPWSPingInterval, 4*time.Second)
	}
}

func TestWebSocketTimeouts_RejectsPingIntervalGTEIdleTimeout(t *testing.T) {
	t.Run("signaling", func(t *testing.T) {
		_, err := load(lookupMap(map[string]string{
			envVarAPIKey:                  "secret",
			envVarSignalingWSIdleTimeout:  "1s",
			envVarSignalingWSPingInterval: "1s",
		}), nil)
		if err == nil {
			t.Fatalf("expected error, got nil")
		}
	})

	t.Run("udp", func(t *testing.T) {
		_, err := load(lookupMap(map[string]string{
			envVarAPIKey:            "secret",
			envVarUDPWSIdleTimeout:  "1s",
			envVarUDPWSPingInterval: "1s",
		}), nil)
		if err == nil {
			t.Fatalf("expected error, got nil")
		}
	})
}

func TestWebRTCDataChannelMaxMessageBytes_AutoDerivesFromRelayLimits(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:                  "secret",
		envVarMaxDatagramPayloadBytes: "1400",
		envVarL2MaxMessageBytes:       "2048",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}

	wantMin := max(1400+webrtcDataChannelUDPFrameOverheadBytes, 2048)
	want := wantMin + DefaultWebRTCDataChannelMaxMessageOverheadBytes
	if cfg.WebRTCDataChannelMaxMessageBytes != want {
		t.Fatalf("WebRTCDataChannelMaxMessageBytes=%d, want %d", cfg.WebRTCDataChannelMaxMessageBytes, want)
	}
}

func TestWebRTCDataChannelMaxMessageBytes_EnvOverride_TooSmall(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		envVarAPIKey:                           "secret",
		envVarL2MaxMessageBytes:                "4096",
		envVarWebRTCDataChannelMaxMessageBytes: "1024",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestUDPInboundFilterMode_EnvOverride(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:               "secret",
		envVarUDPInboundFilterMode: "any",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.UDPInboundFilterMode != UDPInboundFilterModeAny {
		t.Fatalf("UDPInboundFilterMode=%q, want %q", cfg.UDPInboundFilterMode, UDPInboundFilterModeAny)
	}
}

func TestUDPInboundFilterMode_Invalid(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		envVarAPIKey:               "secret",
		envVarUDPInboundFilterMode: "nope",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestUDPInboundFilterMode_FlagOverride(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:               "secret",
		envVarUDPInboundFilterMode: "address_and_port",
	}), []string{"--udp-inbound-filter-mode", "any"})
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.UDPInboundFilterMode != UDPInboundFilterModeAny {
		t.Fatalf("UDPInboundFilterMode=%q, want %q", cfg.UDPInboundFilterMode, UDPInboundFilterModeAny)
	}
}

func TestUDPRemoteAllowlistIdleTimeout_DefaultsToBindingIdleTimeoutWhenBindingOverriddenByFlag(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey: "secret",
	}), []string{"--udp-binding-idle-timeout", "10s"})
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.UDPBindingIdleTimeout != 10*time.Second {
		t.Fatalf("UDPBindingIdleTimeout=%v, want %v", cfg.UDPBindingIdleTimeout, 10*time.Second)
	}
	if cfg.UDPRemoteAllowlistIdleTimeout != cfg.UDPBindingIdleTimeout {
		t.Fatalf("UDPRemoteAllowlistIdleTimeout=%v, want %v", cfg.UDPRemoteAllowlistIdleTimeout, cfg.UDPBindingIdleTimeout)
	}
}

func TestUDPRemoteAllowlistIdleTimeout_DefaultsToBindingIdleTimeoutWhenBindingOverriddenByEnv(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:                "secret",
		envVarUDPBindingIdleTimeout: "10s",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.UDPBindingIdleTimeout != 10*time.Second {
		t.Fatalf("UDPBindingIdleTimeout=%v, want %v", cfg.UDPBindingIdleTimeout, 10*time.Second)
	}
	if cfg.UDPRemoteAllowlistIdleTimeout != cfg.UDPBindingIdleTimeout {
		t.Fatalf("UDPRemoteAllowlistIdleTimeout=%v, want %v", cfg.UDPRemoteAllowlistIdleTimeout, cfg.UDPBindingIdleTimeout)
	}
}

func TestUDPRemoteAllowlistIdleTimeout_EnvOverride(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:                        "secret",
		envVarUDPRemoteAllowlistIdleTimeout: "30s",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.UDPRemoteAllowlistIdleTimeout != 30*time.Second {
		t.Fatalf("UDPRemoteAllowlistIdleTimeout=%v, want %v", cfg.UDPRemoteAllowlistIdleTimeout, 30*time.Second)
	}
}

func TestUDPRemoteAllowlistIdleTimeout_Invalid(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		envVarAPIKey:                        "secret",
		envVarUDPRemoteAllowlistIdleTimeout: "nope",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestUDPBindingIdleTimeout_RejectsNonPositive(t *testing.T) {
	t.Run("zero", func(t *testing.T) {
		_, err := load(lookupMap(map[string]string{
			envVarAPIKey:                "secret",
			envVarUDPBindingIdleTimeout: "0s",
		}), nil)
		if err == nil {
			t.Fatalf("expected error, got nil")
		}
	})

	t.Run("negative", func(t *testing.T) {
		_, err := load(lookupMap(map[string]string{
			envVarAPIKey:                "secret",
			envVarUDPBindingIdleTimeout: "-1s",
		}), nil)
		if err == nil {
			t.Fatalf("expected error, got nil")
		}
	})
}

func TestUDPRemoteAllowlistIdleTimeout_RejectsNonPositive(t *testing.T) {
	t.Run("zero", func(t *testing.T) {
		_, err := load(lookupMap(map[string]string{
			envVarAPIKey:                        "secret",
			envVarUDPRemoteAllowlistIdleTimeout: "0s",
		}), nil)
		if err == nil {
			t.Fatalf("expected error, got nil")
		}
	})

	t.Run("negative", func(t *testing.T) {
		_, err := load(lookupMap(map[string]string{
			envVarAPIKey:                        "secret",
			envVarUDPRemoteAllowlistIdleTimeout: "-1s",
		}), nil)
		if err == nil {
			t.Fatalf("expected error, got nil")
		}
	})
}

func TestUDPRemoteAllowlistIdleTimeout_FlagOverride(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:                        "secret",
		envVarUDPRemoteAllowlistIdleTimeout: "30s",
	}), []string{"--udp-remote-allowlist-idle-timeout", "10s"})
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.UDPRemoteAllowlistIdleTimeout != 10*time.Second {
		t.Fatalf("UDPRemoteAllowlistIdleTimeout=%v, want %v", cfg.UDPRemoteAllowlistIdleTimeout, 10*time.Second)
	}
}

func TestMaxAllowedRemotesPerBinding_EnvOverride(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:                      "secret",
		envVarMaxAllowedRemotesPerBinding: "42",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.MaxAllowedRemotesPerBinding != 42 {
		t.Fatalf("MaxAllowedRemotesPerBinding=%d, want %d", cfg.MaxAllowedRemotesPerBinding, 42)
	}
}

func TestMaxAllowedRemotesPerBinding_Invalid(t *testing.T) {
	t.Run("non_int", func(t *testing.T) {
		_, err := load(lookupMap(map[string]string{
			envVarAPIKey:                      "secret",
			envVarMaxAllowedRemotesPerBinding: "nope",
		}), nil)
		if err == nil {
			t.Fatalf("expected error, got nil")
		}
	})

	t.Run("non_positive", func(t *testing.T) {
		_, err := load(lookupMap(map[string]string{
			envVarAPIKey:                      "secret",
			envVarMaxAllowedRemotesPerBinding: "0",
		}), nil)
		if err == nil {
			t.Fatalf("expected error, got nil")
		}
	})
}

func TestMaxAllowedRemotesPerBinding_FlagOverride(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:                      "secret",
		envVarMaxAllowedRemotesPerBinding: "30",
	}), []string{"--max-allowed-remotes-per-binding", "10"})
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.MaxAllowedRemotesPerBinding != 10 {
		t.Fatalf("MaxAllowedRemotesPerBinding=%d, want %d", cfg.MaxAllowedRemotesPerBinding, 10)
	}
}

func TestWebRTCSCTPMaxReceiveBufferBytes_EnvOverride(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:                           "secret",
		envVarWebRTCDataChannelMaxMessageBytes: "4096",
		envVarWebRTCSCTPMaxReceiveBufferBytes:  "8192",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.WebRTCDataChannelMaxMessageBytes != 4096 {
		t.Fatalf("WebRTCDataChannelMaxMessageBytes=%d, want %d", cfg.WebRTCDataChannelMaxMessageBytes, 4096)
	}
	if cfg.WebRTCSCTPMaxReceiveBufferBytes != 8192 {
		t.Fatalf("WebRTCSCTPMaxReceiveBufferBytes=%d, want %d", cfg.WebRTCSCTPMaxReceiveBufferBytes, 8192)
	}
}

func TestWebRTCSCTPMaxReceiveBufferBytes_RejectsBelow1500(t *testing.T) {
	// pion/sctp rejects values below ~1500 during association setup (INIT/INIT-ACK
	// validation). Ensure config validation rejects these values early.
	_, err := load(lookupMap(map[string]string{
		envVarAPIKey:                           "secret",
		envVarMaxDatagramPayloadBytes:          "1",
		envVarL2MaxMessageBytes:                "1",
		envVarWebRTCDataChannelMaxMessageBytes: "25",   // MAX_DATAGRAM_PAYLOAD_BYTES+24
		envVarWebRTCSCTPMaxReceiveBufferBytes:  "1000", // < 1500
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
	if !strings.Contains(err.Error(), envVarWebRTCSCTPMaxReceiveBufferBytes) {
		t.Fatalf("err=%v, expected mention of %s", err, envVarWebRTCSCTPMaxReceiveBufferBytes)
	}
	if !strings.Contains(err.Error(), "1500") {
		t.Fatalf("err=%v, expected mention of minimum 1500", err)
	}
}

func TestDefaultsProdWhenModeFlagSet(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey: "secret",
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
		envVarAPIKey: "secret",
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
		envVarAPIKey:           "secret",
		envVarWebRTCUDPPortMin: "40000",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestWebRTCUDPPortRange_TooSmall(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		envVarAPIKey:           "secret",
		envVarWebRTCUDPPortMin: "40000",
		envVarWebRTCUDPPortMax: "40010",
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
		envVarAPIKey:           "secret",
		envVarWebRTCUDPPortMin: "40000",
		envVarWebRTCUDPPortMax: "40199",
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
		envVarAPIKey:                       "secret",
		envVarWebRTCNAT1To1IPs:             "203.0.113.10, 203.0.113.11",
		envVarWebRTCNAT1To1IPCandidateType: "srflx",
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
		envVarAPIKey:                       "secret",
		envVarWebRTCNAT1To1IPCandidateType: "nope",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestWebRTCNAT1To1IPs_InvalidIPs(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		envVarAPIKey:           "secret",
		envVarWebRTCNAT1To1IPs: "nope",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestWebRTCUDPListenIP(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:            "secret",
		envVarWebRTCUDPListenIP: "10.0.0.123",
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
		envVarAPIKey:            "secret",
		envVarWebRTCUDPListenIP: "bad.ip",
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
	if !strings.Contains(err.Error(), envVarAPIKey) {
		t.Fatalf("err=%v, expected mention of %s", err, envVarAPIKey)
	}
}

func TestL2BackendWSURL_ValidatesSchemeAndHost(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		envVarAPIKey:         "secret",
		envVarL2BackendWSURL: "http://example.com/l2",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}

	_, err = load(lookupMap(map[string]string{
		envVarAPIKey:         "secret",
		envVarL2BackendWSURL: "ws:///l2",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestL2BackendWSURL_AcceptsWebSocketURL(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:            "secret",
		envVarL2BackendWSURL:    "ws://127.0.0.1:8090/l2",
		envVarL2BackendWSOrigin: "HTTPS://Example.COM:443/",
		envVarL2BackendWSToken:  "test-token",
		envVarL2MaxMessageBytes: "2048",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.L2BackendWSURL != "ws://127.0.0.1:8090/l2" {
		t.Fatalf("L2BackendWSURL=%q", cfg.L2BackendWSURL)
	}
	if cfg.L2BackendWSOrigin != "https://example.com" {
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
		envVarAPIKey:           "secret",
		envVarL2BackendWSToken: "jwt_like.token-123",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.L2BackendWSToken != "jwt_like.token-123" {
		t.Fatalf("L2BackendWSToken=%q", cfg.L2BackendWSToken)
	}
}

func TestL2BackendForwardAeroSession_EnvOverride(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:                      "secret",
		envVarL2BackendForwardAeroSession: "true",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if !cfg.L2BackendForwardAeroSession {
		t.Fatalf("expected L2BackendForwardAeroSession to be true")
	}
}

func TestL2BackendWSToken_RejectsInvalidToken(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		envVarAPIKey:           "secret",
		envVarL2BackendWSToken: "not a token",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
	if !strings.Contains(err.Error(), envVarL2BackendWSToken) {
		t.Fatalf("expected error mentioning %s (err=%v)", envVarL2BackendWSToken, err)
	}
}

func TestL2BackendToken_EnvAlias_AcceptsHTTPToken(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:         "secret",
		envVarL2BackendToken: "jwt_like.token-123",
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
		envVarAPIKey:          "secret",
		envVarL2BackendWSURL:  "ws://127.0.0.1:8090/l2",
		envVarL2BackendOrigin: "HTTPS://Example.COM:443/",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.L2BackendWSOrigin != "https://example.com" {
		t.Fatalf("L2BackendWSOrigin=%q", cfg.L2BackendWSOrigin)
	}
}

func TestL2BackendOrigin_EnvAlias_RejectsPath(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		envVarAPIKey:          "secret",
		envVarL2BackendOrigin: "https://example.com/path",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestL2BackendToken_EnvAlias_RejectsComma(t *testing.T) {
	_, err := load(lookupMap(map[string]string{
		envVarAPIKey:         "secret",
		envVarL2BackendToken: "abc,def",
	}), nil)
	if err == nil {
		t.Fatalf("expected error, got nil")
	}
}

func TestL2BackendAuthForwardMode_Subprotocol(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:                   "secret",
		envVarL2BackendWSURL:           "ws://127.0.0.1:8090/l2",
		envVarL2BackendAuthForwardMode: "subprotocol",
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
		envVarAPIKey:                  "secret",
		envVarL2BackendWSURL:          "ws://127.0.0.1:8090/l2",
		envVarL2BackendOriginOverride: "HTTPS://Example.COM:443/",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.L2BackendWSOrigin != "https://example.com" {
		t.Fatalf("L2BackendWSOrigin=%q, want %q", cfg.L2BackendWSOrigin, "https://example.com")
	}
}

func TestL2BackendOriginAlias_NormalizesAndValidates(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:          "secret",
		envVarL2BackendWSURL:  "ws://127.0.0.1:8090/l2",
		envVarL2BackendOrigin: "HTTPS://Example.COM:443/",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.L2BackendWSOrigin != "https://example.com" {
		t.Fatalf("L2BackendWSOrigin=%q, want %q", cfg.L2BackendWSOrigin, "https://example.com")
	}
}

func TestL2BackendTokenAlias_AcceptsHTTPToken(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:         "secret",
		envVarL2BackendToken: "jwt_like.token-123",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.L2BackendWSToken != "jwt_like.token-123" {
		t.Fatalf("L2BackendWSToken=%q", cfg.L2BackendWSToken)
	}
}

func TestL2BackendOriginOverride_IgnoresInvalidWSOrigin(t *testing.T) {
	cfg, err := load(lookupMap(map[string]string{
		envVarAPIKey:                  "secret",
		envVarL2BackendWSURL:          "ws://127.0.0.1:8090/l2",
		envVarL2BackendWSOrigin:       "https://invalid.example.com/path",
		envVarL2BackendOriginOverride: "HTTPS://Example.COM:443/",
	}), nil)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cfg.L2BackendWSOrigin != "https://example.com" {
		t.Fatalf("L2BackendWSOrigin=%q, want %q", cfg.L2BackendWSOrigin, "https://example.com")
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
	if got[0] != "https://example.com" {
		t.Fatalf("got[0]=%q, want %q", got[0], "https://example.com")
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
