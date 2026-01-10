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

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
)

func TestHealthzReadyzVersion(t *testing.T) {
	cfg := config.Config{
		ListenAddr:      "127.0.0.1:0",
		LogFormat:       config.LogFormatText,
		LogLevel:        slog.LevelInfo,
		ShutdownTimeout: 2 * time.Second,
		Mode:            config.ModeDev,
	}

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

	baseURL := "http://" + ln.Addr().String()

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
		if got.Commit != build.Commit || got.BuildTime != build.BuildTime {
			t.Fatalf("got=%+v, want=%+v", got, build)
		}
	})
}
