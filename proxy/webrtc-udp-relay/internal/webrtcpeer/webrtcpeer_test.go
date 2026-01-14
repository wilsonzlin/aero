package webrtcpeer

import (
	"testing"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
)

func TestConfiguredSCTPMaxMessageSize_ReflectsSettingEngine(t *testing.T) {
	want := 1234
	api, err := newAPI(config.Config{
		WebRTCDataChannelMaxMessageBytes: want,
	})
	if err != nil {
		t.Fatalf("newAPI: %v", err)
	}

	if got := configuredSCTPMaxMessageSize(api); got != want {
		t.Fatalf("configuredSCTPMaxMessageSize=%d, want %d", got, want)
	}
}
