package httpserver

import (
	"context"
	"crypto/hmac"
	"crypto/sha1"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"io"
	"log/slog"
	"net"
	"net/http"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
)

func startTestServer(t *testing.T, cfg config.Config, register func(*Server)) string {
	t.Helper()

	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	build := BuildInfo{Commit: "abc", BuildTime: "time"}
	srv := New(cfg, log, build)
	if register != nil {
		register(srv)
	}

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

func makeJWT(secret string) string {
	header := base64.RawURLEncoding.EncodeToString([]byte(`{"alg":"HS256","typ":"JWT"}`))
	payload := base64.RawURLEncoding.EncodeToString([]byte(`{}`))
	unsigned := header + "." + payload

	mac := hmac.New(sha256.New, []byte(secret))
	_, _ = mac.Write([]byte(unsigned))
	sig := base64.RawURLEncoding.EncodeToString(mac.Sum(nil))
	return unsigned + "." + sig
}

func TestHealthzReadyzVersion(t *testing.T) {
	cfg := config.Config{
		ListenAddr:      "127.0.0.1:0",
		LogFormat:       config.LogFormatText,
		LogLevel:        slog.LevelInfo,
		ShutdownTimeout: 2 * time.Second,
		Mode:            config.ModeDev,
		AuthMode:        config.AuthModeNone,
	}

	baseURL := startTestServer(t, cfg, nil)

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
		AuthMode:        config.AuthModeNone,
		ICEServers: []webrtc.ICEServer{
			{URLs: []string{"stun:stun.example.com:3478"}},
			{URLs: []string{"turn:turn.example.com:3478?transport=udp"}, Username: "user", Credential: "pass"},
		},
	}

	baseURL := startTestServer(t, cfg, nil)

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

func TestICEEndpoint_AuthAPIKey(t *testing.T) {
	cfg := config.Config{
		ListenAddr:      "127.0.0.1:0",
		LogFormat:       config.LogFormatText,
		LogLevel:        slog.LevelInfo,
		ShutdownTimeout: 2 * time.Second,
		Mode:            config.ModeDev,
		AuthMode:        config.AuthModeAPIKey,
		APIKey:          "secret",
		ICEServers:      []webrtc.ICEServer{{URLs: []string{"stun:stun.example.com:3478"}}},
	}
	baseURL := startTestServer(t, cfg, nil)

	t.Run("missing", func(t *testing.T) {
		resp, err := http.Get(baseURL + "/webrtc/ice")
		if err != nil {
			t.Fatalf("get: %v", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusUnauthorized {
			t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusUnauthorized)
		}
	})

	t.Run("valid header", func(t *testing.T) {
		req, err := http.NewRequest(http.MethodGet, baseURL+"/webrtc/ice", nil)
		if err != nil {
			t.Fatalf("new request: %v", err)
		}
		req.Header.Set("X-API-Key", "secret")
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("do: %v", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusOK {
			t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusOK)
		}
	})

	t.Run("valid query", func(t *testing.T) {
		resp, err := http.Get(baseURL + "/webrtc/ice?apiKey=secret")
		if err != nil {
			t.Fatalf("get: %v", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusOK {
			t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusOK)
		}
	})
}

func TestICEEndpoint_AuthJWT(t *testing.T) {
	cfg := config.Config{
		ListenAddr:      "127.0.0.1:0",
		LogFormat:       config.LogFormatText,
		LogLevel:        slog.LevelInfo,
		ShutdownTimeout: 2 * time.Second,
		Mode:            config.ModeDev,
		AuthMode:        config.AuthModeJWT,
		JWTSecret:       "secret",
		ICEServers:      []webrtc.ICEServer{{URLs: []string{"stun:stun.example.com:3478"}}},
	}
	baseURL := startTestServer(t, cfg, nil)

	t.Run("missing", func(t *testing.T) {
		resp, err := http.Get(baseURL + "/webrtc/ice")
		if err != nil {
			t.Fatalf("get: %v", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusUnauthorized {
			t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusUnauthorized)
		}
	})

	token := makeJWT("secret")

	t.Run("valid bearer header", func(t *testing.T) {
		req, err := http.NewRequest(http.MethodGet, baseURL+"/webrtc/ice", nil)
		if err != nil {
			t.Fatalf("new request: %v", err)
		}
		req.Header.Set("Authorization", "Bearer "+token)
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("do: %v", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusOK {
			t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusOK)
		}
	})

	t.Run("valid query", func(t *testing.T) {
		resp, err := http.Get(baseURL + "/webrtc/ice?token=" + token)
		if err != nil {
			t.Fatalf("get: %v", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusOK {
			t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusOK)
		}
	})
}

func TestICEEndpoint_TURNRESTInjectsCredentials(t *testing.T) {
	t.Setenv("AUTH_MODE", "api_key")
	t.Setenv("API_KEY", "secret")
	t.Setenv("TURN_REST_SHARED_SECRET", "shared-secret")
	t.Setenv("TURN_REST_TTL_SECONDS", "10")
	t.Setenv("TURN_REST_USERNAME_PREFIX", "aero")
	t.Setenv("AERO_ICE_SERVERS_JSON", `[
	  {"urls":["turn:turn.example.com:3478?transport=udp"]}
	]`)

	cfg, err := config.Load([]string{"--listen-addr", "127.0.0.1:0"})
	if err != nil {
		t.Fatalf("config.Load: %v", err)
	}

	baseURL := startTestServer(t, cfg, nil)

	startUnix := time.Now().Unix()
	req, err := http.NewRequest(http.MethodGet, baseURL+"/webrtc/ice", nil)
	if err != nil {
		t.Fatalf("new request: %v", err)
	}
	req.Header.Set("Origin", baseURL)
	req.Header.Set("X-API-Key", "secret")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("request failed: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("expected 200, got %d", resp.StatusCode)
	}

	var payload struct {
		ICEServers []webrtc.ICEServer `json:"iceServers"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&payload); err != nil {
		t.Fatalf("decode failed: %v", err)
	}
	if len(payload.ICEServers) != 1 {
		t.Fatalf("expected 1 iceServer, got %d", len(payload.ICEServers))
	}

	server := payload.ICEServers[0]
	if len(server.URLs) != 1 || server.URLs[0] != "turn:turn.example.com:3478?transport=udp" {
		t.Fatalf("unexpected urls: %#v", server.URLs)
	}

	if server.Username == "" {
		t.Fatalf("expected username to be set")
	}
	cred, ok := server.Credential.(string)
	if !ok || cred == "" {
		t.Fatalf("expected credential string to be set, got %#v", server.Credential)
	}

	parts := strings.Split(server.Username, ":")
	if len(parts) != 3 {
		t.Fatalf("username=%q, want 3 colon-separated parts", server.Username)
	}
	if parts[1] != "aero" {
		t.Fatalf("username prefix=%q, want %q", parts[1], "aero")
	}

	expiry, err := strconv.ParseInt(parts[0], 10, 64)
	if err != nil {
		t.Fatalf("invalid expiry timestamp %q: %v", parts[0], err)
	}
	if expiry < startUnix+9 || expiry > startUnix+11 {
		t.Fatalf("expiry=%d, expected approx %d (+/-1s)", expiry, startUnix+10)
	}

	raw, err := base64.StdEncoding.DecodeString(cred)
	if err != nil {
		t.Fatalf("credential not base64: %v", err)
	}
	if len(raw) != sha1.Size {
		t.Fatalf("decoded credential length=%d, want %d", len(raw), sha1.Size)
	}

	mac := hmac.New(sha1.New, []byte("shared-secret"))
	_, _ = mac.Write([]byte(server.Username))
	want := base64.StdEncoding.EncodeToString(mac.Sum(nil))
	if cred != want {
		t.Fatalf("credential mismatch: got %q, want %q", cred, want)
	}
}

func TestICEEndpoint_RejectsCrossOrigin(t *testing.T) {
	cfg := config.Config{
		ListenAddr:      "127.0.0.1:0",
		LogFormat:       config.LogFormatText,
		LogLevel:        slog.LevelInfo,
		ShutdownTimeout: 2 * time.Second,
		Mode:            config.ModeDev,
		AuthMode:        config.AuthModeNone,
		ICEServers:      []webrtc.ICEServer{{URLs: []string{"stun:stun.example.com:3478"}}},
	}

	baseURL := startTestServer(t, cfg, nil)

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

func TestOriginMiddleware_RejectsInvalidOrigin(t *testing.T) {
	cfg := config.Config{
		ListenAddr:      "127.0.0.1:0",
		LogFormat:       config.LogFormatText,
		LogLevel:        slog.LevelInfo,
		ShutdownTimeout: 2 * time.Second,
		Mode:            config.ModeDev,
		AuthMode:        config.AuthModeNone,
		ICEServers:      []webrtc.ICEServer{{URLs: []string{"stun:stun.example.com:3478"}}},
	}

	baseURL := startTestServer(t, cfg, nil)

	req, err := http.NewRequest(http.MethodGet, baseURL+"/webrtc/ice", nil)
	if err != nil {
		t.Fatalf("new request: %v", err)
	}
	req.Header.Set("Origin", "https://evil.example.com/path")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("request failed: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusForbidden {
		t.Fatalf("expected 403, got %d", resp.StatusCode)
	}
}

func TestOriginMiddleware_RejectsNonHTTPOrigin(t *testing.T) {
	cfg := config.Config{
		ListenAddr:      "127.0.0.1:0",
		LogFormat:       config.LogFormatText,
		LogLevel:        slog.LevelInfo,
		ShutdownTimeout: 2 * time.Second,
		Mode:            config.ModeDev,
		AuthMode:        config.AuthModeNone,
		ICEServers:      []webrtc.ICEServer{{URLs: []string{"stun:stun.example.com:3478"}}},
	}

	baseURL := startTestServer(t, cfg, nil)

	req, err := http.NewRequest(http.MethodGet, baseURL+"/webrtc/ice", nil)
	if err != nil {
		t.Fatalf("new request: %v", err)
	}
	req.Header.Set("Origin", "ftp://evil.example.com")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("request failed: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusForbidden {
		t.Fatalf("expected 403, got %d", resp.StatusCode)
	}
}

func TestICEEndpoint_AllowsConfiguredOrigin(t *testing.T) {
	cfg := config.Config{
		ListenAddr:      "127.0.0.1:0",
		LogFormat:       config.LogFormatText,
		LogLevel:        slog.LevelInfo,
		ShutdownTimeout: 2 * time.Second,
		Mode:            config.ModeDev,
		AllowedOrigins:  []string{"https://app.example.com"},
		AuthMode:        config.AuthModeNone,
		ICEServers:      []webrtc.ICEServer{{URLs: []string{"stun:stun.example.com:3478"}}},
	}

	baseURL := startTestServer(t, cfg, nil)

	req, err := http.NewRequest(http.MethodGet, baseURL+"/webrtc/ice", nil)
	if err != nil {
		t.Fatalf("new request: %v", err)
	}
	req.Header.Set("Origin", "https://app.example.com")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("request failed: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("expected 200, got %d", resp.StatusCode)
	}

	if got := resp.Header.Get("Access-Control-Allow-Origin"); got != "https://app.example.com" {
		t.Fatalf("Access-Control-Allow-Origin=%q, want %q", got, "https://app.example.com")
	}

	if got := resp.Header.Get("Access-Control-Allow-Credentials"); got != "true" {
		t.Fatalf("Access-Control-Allow-Credentials=%q, want %q", got, "true")
	}
	if got := resp.Header.Get("Access-Control-Expose-Headers"); !strings.Contains(got, "X-Request-ID") {
		t.Fatalf("Access-Control-Expose-Headers=%q, expected it to include X-Request-ID", got)
	}
}

func TestOriginMiddleware_Preflight(t *testing.T) {
	cfg := config.Config{
		ListenAddr:      "127.0.0.1:0",
		LogFormat:       config.LogFormatText,
		LogLevel:        slog.LevelInfo,
		ShutdownTimeout: 2 * time.Second,
		Mode:            config.ModeDev,
		ICEServers:      []webrtc.ICEServer{{URLs: []string{"stun:stun.example.com:3478"}}},
	}

	baseURL := startTestServer(t, cfg, nil)

	req, err := http.NewRequest(http.MethodOptions, baseURL+"/webrtc/ice", nil)
	if err != nil {
		t.Fatalf("new request: %v", err)
	}
	req.Header.Set("Origin", baseURL)
	req.Header.Set("Access-Control-Request-Method", "GET")
	req.Header.Set("Access-Control-Request-Headers", "content-type")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("request failed: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusNoContent {
		t.Fatalf("expected 204, got %d", resp.StatusCode)
	}

	if got := resp.Header.Get("Access-Control-Allow-Origin"); got != baseURL {
		t.Fatalf("Access-Control-Allow-Origin=%q, want %q", got, baseURL)
	}
	if got := resp.Header.Get("Access-Control-Allow-Methods"); !strings.Contains(got, "GET") {
		t.Fatalf("Access-Control-Allow-Methods=%q, expected it to include GET", got)
	}
	if got := resp.Header.Get("Access-Control-Allow-Headers"); got != "content-type" {
		t.Fatalf("Access-Control-Allow-Headers=%q, want %q", got, "content-type")
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

	baseURL := startTestServer(t, cfg, nil)

	resp, err := http.Get(baseURL + "/readyz")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusServiceUnavailable {
		t.Fatalf("expected 503, got %d", resp.StatusCode)
	}
}

func TestRequestIDMiddleware(t *testing.T) {
	cfg := config.Config{
		ListenAddr:      "127.0.0.1:0",
		LogFormat:       config.LogFormatText,
		LogLevel:        slog.LevelInfo,
		ShutdownTimeout: 2 * time.Second,
		Mode:            config.ModeDev,
	}

	baseURL := startTestServer(t, cfg, func(srv *Server) {
		srv.Mux().HandleFunc("GET /echo-request-id", func(w http.ResponseWriter, r *http.Request) {
			WriteJSON(w, http.StatusOK, map[string]any{"requestId": r.Header.Get("X-Request-ID")})
		})
	})

	t.Run("generated when missing", func(t *testing.T) {
		resp, err := http.Get(baseURL + "/echo-request-id")
		if err != nil {
			t.Fatalf("get: %v", err)
		}
		defer resp.Body.Close()

		if resp.StatusCode != http.StatusOK {
			t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusOK)
		}

		reqID := strings.TrimSpace(resp.Header.Get("X-Request-ID"))
		if reqID == "" {
			t.Fatalf("expected X-Request-ID header to be set")
		}

		var body struct {
			RequestID string `json:"requestId"`
		}
		if err := json.NewDecoder(resp.Body).Decode(&body); err != nil {
			t.Fatalf("decode: %v", err)
		}
		if strings.TrimSpace(body.RequestID) != reqID {
			t.Fatalf("body requestId=%q, want %q", body.RequestID, reqID)
		}
	})

	t.Run("preserves provided ID", func(t *testing.T) {
		req, err := http.NewRequest(http.MethodGet, baseURL+"/echo-request-id", nil)
		if err != nil {
			t.Fatalf("new request: %v", err)
		}
		req.Header.Set("X-Request-ID", "my-custom-id")

		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatalf("do: %v", err)
		}
		defer resp.Body.Close()

		if resp.StatusCode != http.StatusOK {
			t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusOK)
		}

		if got := resp.Header.Get("X-Request-ID"); got != "my-custom-id" {
			t.Fatalf("X-Request-ID=%q, want %q", got, "my-custom-id")
		}
	})
}

func TestRecoverMiddleware(t *testing.T) {
	cfg := config.Config{
		ListenAddr:      "127.0.0.1:0",
		LogFormat:       config.LogFormatText,
		LogLevel:        slog.LevelInfo,
		ShutdownTimeout: 2 * time.Second,
		Mode:            config.ModeDev,
	}

	baseURL := startTestServer(t, cfg, func(srv *Server) {
		srv.Mux().HandleFunc("GET /panic", func(w http.ResponseWriter, r *http.Request) {
			panic("boom")
		})
	})

	resp, err := http.Get(baseURL + "/panic")
	if err != nil {
		t.Fatalf("get: %v", err)
	}
	resp.Body.Close()

	if resp.StatusCode != http.StatusInternalServerError {
		t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusInternalServerError)
	}

	// The server should still be alive after recovering.
	resp2, err := http.Get(baseURL + "/healthz")
	if err != nil {
		t.Fatalf("get healthz: %v", err)
	}
	resp2.Body.Close()
	if resp2.StatusCode != http.StatusOK {
		t.Fatalf("healthz status=%d, want %d", resp2.StatusCode, http.StatusOK)
	}
}
