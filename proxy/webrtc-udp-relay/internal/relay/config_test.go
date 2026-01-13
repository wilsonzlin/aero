package relay

import "testing"

func TestDefaultConfig_UDPReadBufferBytes_IsMaxDatagramPayloadBytesPlusOne(t *testing.T) {
	cfg := DefaultConfig()
	if cfg.UDPReadBufferBytes != cfg.MaxDatagramPayloadBytes+1 {
		t.Fatalf("UDPReadBufferBytes=%d, want %d (max payload + 1)", cfg.UDPReadBufferBytes, cfg.MaxDatagramPayloadBytes+1)
	}

	// Worst-case per-session memory used by per-binding read buffers is:
	//   max_bindings * (IPv4 + IPv6) * UDPReadBufferBytes
	//
	// With defaults (128 bindings, 1200 max payload), this should be well under 1MiB.
	// (Previously: 128 * 2 * 65535 ~= 16MiB.)
	bufBytes := cfg.MaxUDPBindingsPerSession * 2 * cfg.UDPReadBufferBytes
	if bufBytes >= 1<<20 {
		t.Fatalf("expected per-session read buffers < 1MiB; got %d bytes", bufBytes)
	}
}

func TestConfigWithDefaults_UDPReadBufferBytes_DefaultsRelativeToMaxPayload(t *testing.T) {
	cfg := (Config{MaxDatagramPayloadBytes: 1400}).WithDefaults()
	if cfg.MaxDatagramPayloadBytes != 1400 {
		t.Fatalf("MaxDatagramPayloadBytes=%d, want %d", cfg.MaxDatagramPayloadBytes, 1400)
	}
	if cfg.UDPReadBufferBytes != 1401 {
		t.Fatalf("UDPReadBufferBytes=%d, want %d (max payload + 1)", cfg.UDPReadBufferBytes, 1401)
	}
}

func TestConfigWithDefaults_DefaultInboundFilterModeIsAddressAndPort(t *testing.T) {
	cfg := (Config{}).WithDefaults()
	if cfg.InboundFilterMode != InboundFilterAddressAndPort {
		t.Fatalf("InboundFilterMode=%v, want %v", cfg.InboundFilterMode, InboundFilterAddressAndPort)
	}
}

func TestConfigWithDefaults_ClampsInvalidInboundFilterMode(t *testing.T) {
	cfg := (Config{InboundFilterMode: InboundFilterMode(123)}).WithDefaults()
	if cfg.InboundFilterMode != InboundFilterAddressAndPort {
		t.Fatalf("InboundFilterMode=%v, want %v", cfg.InboundFilterMode, InboundFilterAddressAndPort)
	}
}
