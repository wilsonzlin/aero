package main

import (
	"context"
	"log/slog"
	"sync"
	"testing"
	"time"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
)

type recordedLog struct {
	level slog.Level
	msg   string
	attrs map[string]any
}

type recordingHandler struct {
	mu      *sync.Mutex
	records *[]recordedLog
	attrs   []slog.Attr
	groups  []string
}

func newRecordingLogger() (*slog.Logger, func() []recordedLog) {
	mu := &sync.Mutex{}
	records := &[]recordedLog{}
	h := &recordingHandler{mu: mu, records: records}
	logger := slog.New(h)
	return logger, func() []recordedLog {
		mu.Lock()
		defer mu.Unlock()
		out := make([]recordedLog, len(*records))
		copy(out, *records)
		return out
	}
}

func (h *recordingHandler) Enabled(context.Context, slog.Level) bool {
	return true
}

func (h *recordingHandler) Handle(_ context.Context, r slog.Record) error {
	rec := recordedLog{
		level: r.Level,
		msg:   r.Message,
		attrs: map[string]any{},
	}
	for _, a := range h.attrs {
		rec.attrs[h.key(a.Key)] = a.Value.Any()
	}
	r.Attrs(func(a slog.Attr) bool {
		rec.attrs[h.key(a.Key)] = a.Value.Any()
		return true
	})

	h.mu.Lock()
	*h.records = append(*h.records, rec)
	h.mu.Unlock()
	return nil
}

func (h *recordingHandler) WithAttrs(attrs []slog.Attr) slog.Handler {
	nh := h.clone()
	nh.attrs = append(nh.attrs, attrs...)
	return nh
}

func (h *recordingHandler) WithGroup(name string) slog.Handler {
	nh := h.clone()
	nh.groups = append(nh.groups, name)
	return nh
}

func (h *recordingHandler) clone() *recordingHandler {
	cp := &recordingHandler{
		mu:      h.mu,
		records: h.records,
	}
	if len(h.attrs) > 0 {
		cp.attrs = append([]slog.Attr(nil), h.attrs...)
	}
	if len(h.groups) > 0 {
		cp.groups = append([]string(nil), h.groups...)
	}
	return cp
}

func (h *recordingHandler) key(k string) string {
	if len(h.groups) == 0 {
		return k
	}
	return stringsJoin(h.groups, ".") + "." + k
}

func stringsJoin(parts []string, sep string) string {
	// Small local helper to avoid pulling in strings for tests that don't need it.
	if len(parts) == 0 {
		return ""
	}
	out := parts[0]
	for _, p := range parts[1:] {
		out += sep + p
	}
	return out
}

func TestStartupSecurityWarnings_UDPInboundFilterModeAny(t *testing.T) {
	logger, records := newRecordingLogger()

	cfg := config.Config{
		Mode:                 config.ModeProd,
		AuthMode:             config.AuthModeAPIKey,
		APIKey:               "secret",
		MaxSessions:          1,
		UDPInboundFilterMode: config.UDPInboundFilterModeAny,
	}
	destPolicy := policy.NewProductionDestinationPolicy()

	logStartupSecurityWarnings(logger, cfg, destPolicy)

	var found bool
	for _, r := range records() {
		if r.level != slog.LevelWarn {
			continue
		}
		if r.attrs["warning_code"] == "udp_inbound_filter_mode_any" {
			found = true
			if r.attrs["udp_inbound_filter_mode"] != config.UDPInboundFilterModeAny {
				t.Fatalf("udp_inbound_filter_mode=%#v, want %q", r.attrs["udp_inbound_filter_mode"], config.UDPInboundFilterModeAny)
			}
			break
		}
	}
	if !found {
		t.Fatalf("expected warning_code=udp_inbound_filter_mode_any, got %#v", records())
	}
}

func TestStartupSecurityWarnings_AuthModeNone(t *testing.T) {
	logger, records := newRecordingLogger()

	cfg := config.Config{
		Mode:     config.ModeDev,
		AuthMode: config.AuthModeNone,
	}
	destPolicy := policy.NewProductionDestinationPolicy()

	logStartupSecurityWarnings(logger, cfg, destPolicy)

	var found bool
	for _, r := range records() {
		if r.level != slog.LevelWarn {
			continue
		}
		if r.attrs["warning_code"] == "auth_mode_none" {
			found = true
			if r.attrs["auth_mode"] != config.AuthModeNone {
				t.Fatalf("auth_mode attr = %#v, want %q", r.attrs["auth_mode"], config.AuthModeNone)
			}
			break
		}
	}
	if !found {
		t.Fatalf("expected warning_code=auth_mode_none, got %#v", records())
	}
}

func TestStartupSecurityWarnings_AllowedOriginsWildcard(t *testing.T) {
	logger, records := newRecordingLogger()

	cfg := config.Config{
		Mode:           config.ModeDev,
		AuthMode:       config.AuthModeAPIKey,
		AllowedOrigins: []string{"*"},
		APIKey:         "secret",
	}
	destPolicy := policy.NewProductionDestinationPolicy()

	logStartupSecurityWarnings(logger, cfg, destPolicy)

	var found bool
	for _, r := range records() {
		if r.level != slog.LevelWarn {
			continue
		}
		if r.attrs["warning_code"] == "allowed_origins_wildcard" {
			found = true
			break
		}
	}
	if !found {
		t.Fatalf("expected warning_code=allowed_origins_wildcard, got %#v", records())
	}
}

func TestStartupSecurityWarnings_DestinationPolicyPresetDev(t *testing.T) {
	logger, records := newRecordingLogger()

	cfg := config.Config{
		Mode:        config.ModeProd,
		AuthMode:    config.AuthModeAPIKey,
		APIKey:      "secret",
		MaxSessions: 1,
	}
	destPolicy := policy.NewDevDestinationPolicy()

	logStartupSecurityWarnings(logger, cfg, destPolicy)

	var found bool
	for _, r := range records() {
		if r.level != slog.LevelWarn {
			continue
		}
		if r.attrs["warning_code"] == "destination_policy_preset_dev" {
			found = true
			if r.attrs["destination_policy_preset"] != "dev" {
				t.Fatalf("destination_policy_preset=%#v, want %q", r.attrs["destination_policy_preset"], "dev")
			}
			break
		}
	}
	if !found {
		t.Fatalf("expected warning_code=destination_policy_preset_dev, got %#v", records())
	}
}

func TestStartupSecurityWarnings_AllowPrivateNetworksInProd(t *testing.T) {
	logger, records := newRecordingLogger()

	cfg := config.Config{
		Mode:        config.ModeProd,
		AuthMode:    config.AuthModeAPIKey,
		APIKey:      "secret",
		MaxSessions: 1,
	}
	destPolicy := policy.NewProductionDestinationPolicy()
	destPolicy.AllowPrivateNetworks = true

	logStartupSecurityWarnings(logger, cfg, destPolicy)

	var found bool
	for _, r := range records() {
		if r.level != slog.LevelWarn {
			continue
		}
		if r.attrs["warning_code"] == "allow_private_networks_in_prod" {
			found = true
			if r.attrs["allow_private_networks"] != true {
				t.Fatalf("allow_private_networks=%#v, want true", r.attrs["allow_private_networks"])
			}
			break
		}
	}
	if !found {
		t.Fatalf("expected warning_code=allow_private_networks_in_prod, got %#v", records())
	}
}

func TestStartupSecurityWarnings_MaxSessionsUnlimitedInProd(t *testing.T) {
	logger, records := newRecordingLogger()

	cfg := config.Config{
		Mode:        config.ModeProd,
		AuthMode:    config.AuthModeAPIKey,
		APIKey:      "secret",
		MaxSessions: 0,
	}
	destPolicy := policy.NewProductionDestinationPolicy()

	logStartupSecurityWarnings(logger, cfg, destPolicy)

	var found bool
	for _, r := range records() {
		if r.level != slog.LevelWarn {
			continue
		}
		if r.attrs["warning_code"] == "max_sessions_unlimited_in_prod" {
			found = true
			break
		}
	}
	if !found {
		t.Fatalf("expected warning_code=max_sessions_unlimited_in_prod, got %#v", records())
	}
}

func TestStartupSecurityWarnings_L2AuthForwardModeQuery(t *testing.T) {
	logger, records := newRecordingLogger()

	cfg := config.Config{
		Mode:                     config.ModeDev,
		AuthMode:                 config.AuthModeJWT,
		JWTSecret:                "secret",
		MaxSessions:              1,
		L2BackendWSURL:           "wss://example.com/l2",
		L2BackendAuthForwardMode: config.L2BackendAuthForwardModeQuery,
	}
	destPolicy := policy.NewProductionDestinationPolicy()

	logStartupSecurityWarnings(logger, cfg, destPolicy)

	var found bool
	for _, r := range records() {
		if r.level != slog.LevelWarn {
			continue
		}
		if r.attrs["warning_code"] == "l2_backend_auth_forward_mode_query" {
			found = true
			if r.attrs["l2_backend_ws_url_set"] != true {
				t.Fatalf("l2_backend_ws_url_set=%#v, want true", r.attrs["l2_backend_ws_url_set"])
			}
			if r.attrs["l2_backend_ws_host"] != "example.com" {
				t.Fatalf("l2_backend_ws_host=%#v, want %q", r.attrs["l2_backend_ws_host"], "example.com")
			}
			break
		}
	}
	if !found {
		t.Fatalf("expected warning_code=l2_backend_auth_forward_mode_query, got %#v", records())
	}
}

func TestStartupSecurityWarnings_L2AuthForwardModeQuery_WarnsWhenL2Disabled(t *testing.T) {
	logger, records := newRecordingLogger()

	cfg := config.Config{
		Mode:                     config.ModeDev,
		AuthMode:                 config.AuthModeJWT,
		JWTSecret:                "secret",
		MaxSessions:              1,
		L2BackendWSURL:           "",
		L2BackendAuthForwardMode: config.L2BackendAuthForwardModeQuery,
	}
	destPolicy := policy.NewProductionDestinationPolicy()

	logStartupSecurityWarnings(logger, cfg, destPolicy)

	var found bool
	for _, r := range records() {
		if r.level != slog.LevelWarn {
			continue
		}
		if r.attrs["warning_code"] == "l2_backend_auth_forward_mode_query" {
			found = true
			if r.attrs["l2_backend_ws_url_set"] != false {
				t.Fatalf("l2_backend_ws_url_set=%#v, want false", r.attrs["l2_backend_ws_url_set"])
			}
			if r.attrs["l2_backend_ws_host"] != "" {
				t.Fatalf("l2_backend_ws_host=%#v, want empty string", r.attrs["l2_backend_ws_host"])
			}
			break
		}
	}
	if !found {
		t.Fatalf("expected warning_code=l2_backend_auth_forward_mode_query, got %#v", records())
	}
}

func TestStartupSecurityWarnings_WebRTCDataChannelMaxMessageLarge(t *testing.T) {
	logger, records := newRecordingLogger()

	cfg := config.Config{
		Mode:                             config.ModeProd,
		AuthMode:                         config.AuthModeAPIKey,
		APIKey:                           "secret",
		MaxSessions:                      1,
		WebRTCDataChannelMaxMessageBytes: 2 * 1024 * 1024, // 2MiB
		// Avoid query-string auth forwarding mode (which would produce its own warning).
		L2BackendAuthForwardMode: config.L2BackendAuthForwardModeSubprotocol,
	}
	destPolicy := policy.NewProductionDestinationPolicy()

	logStartupSecurityWarnings(logger, cfg, destPolicy)

	var found bool
	for _, r := range records() {
		if r.level != slog.LevelWarn {
			continue
		}
		if r.attrs["warning_code"] == "webrtc_datachannel_max_message_large" {
			found = true
			got, ok := r.attrs["webrtc_datachannel_max_message_bytes"].(int64)
			if !ok {
				t.Fatalf("webrtc_datachannel_max_message_bytes=%#v, want int64", r.attrs["webrtc_datachannel_max_message_bytes"])
			}
			if got != 2*1024*1024 {
				t.Fatalf("webrtc_datachannel_max_message_bytes=%d, want %d", got, 2*1024*1024)
			}
			break
		}
	}
	if !found {
		t.Fatalf("expected warning_code=webrtc_datachannel_max_message_large, got %#v", records())
	}
}

func TestStartupSecurityWarnings_WebRTCSCTPMaxReceiveBufferLarge(t *testing.T) {
	logger, records := newRecordingLogger()

	cfg := config.Config{
		Mode:                            config.ModeProd,
		AuthMode:                        config.AuthModeAPIKey,
		APIKey:                          "secret",
		MaxSessions:                     1,
		WebRTCSCTPMaxReceiveBufferBytes: 16 * 1024 * 1024, // 16MiB
		// Avoid query-string auth forwarding mode (which would produce its own warning).
		L2BackendAuthForwardMode: config.L2BackendAuthForwardModeSubprotocol,
	}
	destPolicy := policy.NewProductionDestinationPolicy()

	logStartupSecurityWarnings(logger, cfg, destPolicy)

	var found bool
	for _, r := range records() {
		if r.level != slog.LevelWarn {
			continue
		}
		if r.attrs["warning_code"] == "webrtc_sctp_max_receive_buffer_large" {
			found = true
			got, ok := r.attrs["webrtc_sctp_max_receive_buffer_bytes"].(int64)
			if !ok {
				t.Fatalf("webrtc_sctp_max_receive_buffer_bytes=%#v, want int64", r.attrs["webrtc_sctp_max_receive_buffer_bytes"])
			}
			if got != 16*1024*1024 {
				t.Fatalf("webrtc_sctp_max_receive_buffer_bytes=%d, want %d", got, 16*1024*1024)
			}
			break
		}
	}
	if !found {
		t.Fatalf("expected warning_code=webrtc_sctp_max_receive_buffer_large, got %#v", records())
	}
}

func TestStartupSecurityWarnings_WebRTCSessionConnectTimeoutLarge(t *testing.T) {
	logger, records := newRecordingLogger()

	cfg := config.Config{
		Mode:                        config.ModeProd,
		AuthMode:                    config.AuthModeAPIKey,
		APIKey:                      "secret",
		MaxSessions:                 1,
		WebRTCSessionConnectTimeout: 10 * time.Minute,
		// Avoid query-string auth forwarding mode (which would produce its own warning).
		L2BackendAuthForwardMode: config.L2BackendAuthForwardModeSubprotocol,
	}
	destPolicy := policy.NewProductionDestinationPolicy()

	logStartupSecurityWarnings(logger, cfg, destPolicy)

	var found bool
	for _, r := range records() {
		if r.level != slog.LevelWarn {
			continue
		}
		if r.attrs["warning_code"] == "webrtc_session_connect_timeout_large" {
			found = true
			if r.attrs["webrtc_session_connect_timeout"] != 10*time.Minute {
				t.Fatalf("webrtc_session_connect_timeout=%#v, want %s", r.attrs["webrtc_session_connect_timeout"], (10 * time.Minute).String())
			}
			break
		}
	}
	if !found {
		t.Fatalf("expected warning_code=webrtc_session_connect_timeout_large, got %#v", records())
	}
}

func TestStartupSecurityWarnings_SafeConfig_NoWarnings(t *testing.T) {
	logger, records := newRecordingLogger()

	// Mirror the typical runtime defaults for the WebRTC DoS hardening caps.
	minDataChannelMax := config.DefaultL2MaxMessageBytes
	if max := config.DefaultMaxDatagramPayloadBytes + 24; max > minDataChannelMax {
		minDataChannelMax = max
	}
	webrtcDataChannelMaxMessageBytes := minDataChannelMax + config.DefaultWebRTCDataChannelMaxMessageOverheadBytes

	cfg := config.Config{
		Mode:                             config.ModeProd,
		AuthMode:                         config.AuthModeAPIKey,
		APIKey:                           "secret",
		MaxSessions:                      10,
		WebRTCSessionConnectTimeout:      config.DefaultWebRTCSessionConnectTimeout,
		WebRTCDataChannelMaxMessageBytes: webrtcDataChannelMaxMessageBytes,
		WebRTCSCTPMaxReceiveBufferBytes:  config.DefaultWebRTCSCTPMaxReceiveBufferBytes,
		// Explicitly avoid query-string auth forwarding mode (the default in config.Load)
		// so we assert a truly "safe" config produces no warnings.
		L2BackendAuthForwardMode: config.L2BackendAuthForwardModeSubprotocol,
	}
	destPolicy := policy.NewProductionDestinationPolicy()

	logStartupSecurityWarnings(logger, cfg, destPolicy)

	if got := records(); len(got) != 0 {
		t.Fatalf("expected no warnings, got %#v", got)
	}
}
