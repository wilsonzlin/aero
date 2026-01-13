package main

import (
	"context"
	"log/slog"
	"sync"
	"testing"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
)

type recordedLog struct {
	level slog.Level
	msg   string
	attrs map[string]any
}

type recordingHandler struct {
	mu     *sync.Mutex
	records *[]recordedLog
	attrs  []slog.Attr
	groups []string
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

