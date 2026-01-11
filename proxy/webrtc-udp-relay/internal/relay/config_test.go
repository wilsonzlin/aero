package relay

import (
	"testing"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
)

func TestConfigFromEnv_MaxDatagramPayloadBytes(t *testing.T) {
	t.Setenv("MAX_DATAGRAM_PAYLOAD_BYTES", "1400")
	cfg := ConfigFromEnv()
	if cfg.MaxDatagramPayloadBytes != 1400 {
		t.Fatalf("MaxDatagramPayloadBytes=%d, want %d", cfg.MaxDatagramPayloadBytes, 1400)
	}
}

func TestConfigFromEnv_L2BackendForwardOrigin_DefaultsTrueWhenL2Enabled(t *testing.T) {
	t.Setenv("L2_BACKEND_WS_URL", "ws://example.com/l2")
	cfg := ConfigFromEnv()
	if !cfg.L2BackendForwardOrigin {
		t.Fatalf("L2BackendForwardOrigin=false, want true")
	}
}

func TestConfigFromEnv_L2BackendForwardOrigin_EnvOverride(t *testing.T) {
	t.Setenv("L2_BACKEND_WS_URL", "ws://example.com/l2")
	t.Setenv("L2_BACKEND_FORWARD_ORIGIN", "false")
	cfg := ConfigFromEnv()
	if cfg.L2BackendForwardOrigin {
		t.Fatalf("L2BackendForwardOrigin=true, want false")
	}
}

func TestConfigFromEnv_L2BackendAuthForwardMode(t *testing.T) {
	t.Setenv("L2_BACKEND_AUTH_FORWARD_MODE", "subprotocol")
	cfg := ConfigFromEnv()
	if cfg.L2BackendAuthForwardMode != config.L2BackendAuthForwardModeSubprotocol {
		t.Fatalf("L2BackendAuthForwardMode=%q, want %q", cfg.L2BackendAuthForwardMode, config.L2BackendAuthForwardModeSubprotocol)
	}
}

func TestConfigFromEnv_L2BackendOriginOverrideAlias(t *testing.T) {
	t.Setenv("L2_BACKEND_ORIGIN_OVERRIDE", "https://example.com")
	cfg := ConfigFromEnv()
	if cfg.L2BackendWSOrigin != "https://example.com" {
		t.Fatalf("L2BackendWSOrigin=%q", cfg.L2BackendWSOrigin)
	}
}
