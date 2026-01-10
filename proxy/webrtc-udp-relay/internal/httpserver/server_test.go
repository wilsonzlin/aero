package httpserver

import (
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net"
	"net/http"
	"testing"
	"time"

	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
)

func startTestServer(t *testing.T, cfg config.Config) (baseURL string) {
	t.Helper()

	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	build := BuildInfo{Commit: "abc", BuildTime: "time"}
	srv := New(cfg, log, build)

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}

	errCh := make(chan error, 1)
	go func() {
		errCh <- srv.Serve(ln)
	}()

	t.Cleanup(func() {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		_ = srv.Shutdown(ctx)
		<-errCh
	})

	return "http://" + ln.Addr().String()
}

func TestHealthzReadyzVersion(t *testing.T) {
	cfg := config.Config{
		ListenAddr:      "127.0.0.1:0",
		LogFormat:       config.LogFormatText,
		LogLevel:        slog.LevelInfo,
		ShutdownTimeout: 2 * time.Second,
		Mode:            config.ModeDev,
	}

	baseURL := startTestServer(t, cfg)

	t.Run("healthz", func(t *testing.T) {
		resp, err := http.Get(baseURL + "/healthz")
		if err != nil {
			t.Fatalf("get: %v", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusOK {
			t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusOK)
		}
		var body map[string]any
		if err := json.NewDecoder(resp.Body).Decode(&body); err != nil {
			t.Fatalf("decode: %v", err)
		}
		if body["ok"] != true {
			t.Fatalf("body=%v, want ok=true", body)
		}
	})

	t.Run("readyz", func(t *testing.T) {
		resp, err := http.Get(baseURL + "/readyz")
		if err != nil {
			t.Fatalf("get: %v", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusOK {
			t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusOK)
		}
	})

	t.Run("version", func(t *testing.T) {
		resp, err := http.Get(baseURL + "/version")
		if err != nil {
			t.Fatalf("get: %v", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusOK {
			t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusOK)
		}
		var got BuildInfo
		if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
			t.Fatalf("decode: %v", err)
		}
		want := BuildInfo{Commit: "abc", BuildTime: "time"}
		if got != want {
			t.Fatalf("got=%+v, want=%+v", got, want)
		}
	})
}

func TestICEEndpointSchema(t *testing.T) {
	cfg := config.Config{
		ListenAddr:      "127.0.0.1:0",
		LogFormat:       config.LogFormatText,
		LogLevel:        slog.LevelInfo,
		ShutdownTimeout: 2 * time.Second,
		Mode:            config.ModeDev,
		ICEServers: []webrtc.ICEServer{
			{URLs: []string{"stun:stun.example.com:3478"}},
			{URLs: []string{"turn:turn.example.com:3478?transport=udp"}, Username: "user", Credential: "pass"},
		},
	}

	baseURL := startTestServer(t, cfg)

	resp, err := http.Get(baseURL + "/webrtc/ice")
	if err != nil {
		t.Fatalf("request failed: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("expected 200, got %d", resp.StatusCode)
	}

	var payload struct {
		ICEServers []map[string]any `json:"iceServers"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&payload); err != nil {
		t.Fatalf("decode failed: %v", err)
	}
	if len(payload.ICEServers) != 2 {
		t.Fatalf("expected 2 iceServers, got %d", len(payload.ICEServers))
	}
	if _, ok := payload.ICEServers[0]["urls"]; !ok {
		t.Fatalf("expected urls field on first server: %#v", payload.ICEServers[0])
	}
}

func TestICEEndpoint_RejectsCrossOrigin(t *testing.T) {
	cfg := config.Config{
		ListenAddr:      "127.0.0.1:0",
		LogFormat:       config.LogFormatText,
		LogLevel:        slog.LevelInfo,
		ShutdownTimeout: 2 * time.Second,
		Mode:            config.ModeDev,
		ICEServers:      []webrtc.ICEServer{{URLs: []string{"stun:stun.example.com:3478"}}},
	}

	baseURL := startTestServer(t, cfg)

	req, err := http.NewRequest(http.MethodGet, baseURL+"/webrtc/ice", nil)
	if err != nil {
		t.Fatalf("new request: %v", err)
	}
	req.Header.Set("Origin", "https://evil.example.com")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("request failed: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusForbidden {
		t.Fatalf("expected 403, got %d", resp.StatusCode)
	}
}

func TestReadyzFailsOnInvalidICEConfig(t *testing.T) {
	t.Setenv("AERO_ICE_SERVERS_JSON", "[")

	cfg, err := config.Load([]string{"--listen-addr", "127.0.0.1:0"})
	if err != nil {
		t.Fatalf("config.Load returned fatal error: %v", err)
	}
	if cfg.ICEConfigError() == nil {
		t.Fatalf("expected ICE config error to be captured for readiness")
	}

	baseURL := startTestServer(t, cfg)

	resp, err := http.Get(baseURL + "/readyz")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusServiceUnavailable {
		t.Fatalf("expected 503, got %d", resp.StatusCode)
	}
}
