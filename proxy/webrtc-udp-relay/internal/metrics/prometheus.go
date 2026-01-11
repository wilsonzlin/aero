package metrics

import (
	"fmt"
	"net/http"
	"sort"
	"strings"
)

// PrometheusHandler exposes Metrics in Prometheus' text exposition format.
//
// It intentionally exposes all internal counters as a single metric with an
// `event` label. This keeps the in-process metrics registry simple while still
// allowing scraping by Prometheus.
func PrometheusHandler(m *Metrics) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if m == nil {
			http.Error(w, "metrics not configured", http.StatusInternalServerError)
			return
		}

		snap := m.Snapshot()
		keys := make([]string, 0, len(snap))
		for k := range snap {
			keys = append(keys, k)
		}
		sort.Strings(keys)

		w.Header().Set("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
		_, _ = fmt.Fprintln(w, "# HELP aero_webrtc_udp_relay_events_total Internal event counters.")
		_, _ = fmt.Fprintln(w, "# TYPE aero_webrtc_udp_relay_events_total counter")
		for _, k := range keys {
			escaped := strings.NewReplacer("\\", "\\\\", "\"", "\\\"").Replace(k)
			_, _ = fmt.Fprintf(w, "aero_webrtc_udp_relay_events_total{event=\"%s\"} %d\n", escaped, snap[k])
		}
	})
}
