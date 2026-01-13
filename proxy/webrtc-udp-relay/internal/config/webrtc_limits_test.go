package config

import "testing"

func TestDefaultWebRTCDataChannelMaxMessageBytes_DerivedFromProtocolLimits(t *testing.T) {
	// Case 1: L2 dominates (default-like).
	{
		maxDatagramPayloadBytes := 1200
		l2MaxMessageBytes := 4096
		min := minWebRTCDataChannelMaxMessageBytes(maxDatagramPayloadBytes, l2MaxMessageBytes)
		if min != 4096 {
			t.Fatalf("min=%d, want 4096", min)
		}
		def := defaultWebRTCDataChannelMaxMessageBytes(maxDatagramPayloadBytes, l2MaxMessageBytes)
		if def != 4096+DefaultWebRTCDataChannelMaxMessageOverheadBytes {
			t.Fatalf("default=%d, want %d", def, 4096+DefaultWebRTCDataChannelMaxMessageOverheadBytes)
		}
	}

	// Case 2: UDP dominates.
	{
		maxDatagramPayloadBytes := 1500
		l2MaxMessageBytes := 1024
		min := minWebRTCDataChannelMaxMessageBytes(maxDatagramPayloadBytes, l2MaxMessageBytes)
		if min != 1500+webrtcDataChannelUDPFrameOverheadBytes {
			t.Fatalf("min=%d, want %d", min, 1500+webrtcDataChannelUDPFrameOverheadBytes)
		}
		def := defaultWebRTCDataChannelMaxMessageBytes(maxDatagramPayloadBytes, l2MaxMessageBytes)
		want := min + DefaultWebRTCDataChannelMaxMessageOverheadBytes
		if def != want {
			t.Fatalf("default=%d, want %d", def, want)
		}
	}
}

func TestDefaultWebRTCSCTPMaxReceiveBufferBytes_AtLeastMessageSize(t *testing.T) {
	// Small message size: expect minimum default (1MiB).
	if got := defaultWebRTCSCTPMaxReceiveBufferBytes(4096); got != DefaultWebRTCSCTPMaxReceiveBufferBytes {
		t.Fatalf("got=%d, want %d", got, DefaultWebRTCSCTPMaxReceiveBufferBytes)
	}

	// Large message size: expect >= 2x.
	if got := defaultWebRTCSCTPMaxReceiveBufferBytes(2 * 1024 * 1024); got < 4*1024*1024 {
		t.Fatalf("got=%d, want >= %d", got, 4*1024*1024)
	}
}
