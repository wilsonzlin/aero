package main

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
)

func TestPrometheusHandler_ExposesSnapshot(t *testing.T) {
	m := &metrics.Metrics{}
	m.Inc("foo")
	m.Add("bar", 2)
	m.Inc(`quote"back\slash`)

	req := httptest.NewRequest(http.MethodGet, "/metrics", nil)
	rr := httptest.NewRecorder()

	prometheusHandler(m).ServeHTTP(rr, req)

	if rr.Code != http.StatusOK {
		t.Fatalf("status=%d, want %d", rr.Code, http.StatusOK)
	}

	body := rr.Body.String()
	if !strings.Contains(body, "# TYPE aero_webrtc_udp_relay_events_total counter") {
		t.Fatalf("missing TYPE header: %s", body)
	}
	if !strings.Contains(body, `aero_webrtc_udp_relay_events_total{event="bar"} 2`) {
		t.Fatalf("missing bar counter: %s", body)
	}
	if !strings.Contains(body, `aero_webrtc_udp_relay_events_total{event="foo"} 1`) {
		t.Fatalf("missing foo counter: %s", body)
	}
	// Ensure label escaping matches Prometheus text format rules.
	if !strings.Contains(body, `aero_webrtc_udp_relay_events_total{event="quote\"back\\slash"} 1`) {
		t.Fatalf("missing escaped counter: %s", body)
	}
}
