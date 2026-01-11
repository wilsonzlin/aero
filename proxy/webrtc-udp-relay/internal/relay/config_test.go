package relay

import "testing"

func TestConfigFromEnv_MaxDatagramPayloadBytes(t *testing.T) {
	t.Setenv("MAX_DATAGRAM_PAYLOAD_BYTES", "1400")
	cfg := ConfigFromEnv()
	if cfg.MaxDatagramPayloadBytes != 1400 {
		t.Fatalf("MaxDatagramPayloadBytes=%d, want %d", cfg.MaxDatagramPayloadBytes, 1400)
	}
}
