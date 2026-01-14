package httpserver

import (
	"bufio"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"log/slog"
	"net"
	"net/http"
	"runtime/debug"
	"sync/atomic"
	"time"

	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/auth"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/turnrest"
)

type BuildInfo struct {
	Commit    string `json:"commit"`
	BuildTime string `json:"buildTime"`
}

type server struct {
	log   *slog.Logger
	cfg   config.Config
	build BuildInfo

	ready atomic.Bool

	metrics *metrics.Metrics

	mux *http.ServeMux
	srv *http.Server
}

func New(cfg config.Config, logger *slog.Logger, build BuildInfo) *server {
	s := &server{
		log:   logger,
		cfg:   cfg,
		build: build,
		mux:   http.NewServeMux(),
	}

	s.registerRoutes()

	handler := chain(s.mux,
		recoverMiddleware(s.log),
		noStoreICEHeadersMiddleware(),
		requestIDMiddleware(),
		requestLoggerMiddleware(s.log),
		s.originMiddleware(),
	)

	s.srv = &http.Server{
		Addr:              cfg.ListenAddr,
		Handler:           handler,
		ReadHeaderTimeout: 5 * time.Second,
		// Note: keep other timeouts conservative/zero for now; future signaling
		// endpoints may use upgraded or long-lived connections.
	}

	return s
}

// noStoreICEHeadersMiddleware ensures that the ICE discovery endpoint
// (`GET /webrtc/ice`) is never cached by browsers or intermediaries. Responses may
// contain sensitive TURN credentials (e.g. TURN REST ephemeral creds), and stale
// caching can also cause ICE failures.
func noStoreICEHeadersMiddleware() middleware {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			// Only apply to the ICE discovery endpoint. Apply this as middleware (not
			// inside the handler) so that error responses returned by earlier
			// middleware (e.g. Origin policy) also inherit the same no-store headers.
			// Treat HEAD like GET here. Go's ServeMux may route HEAD to GET handlers,
			// and the no-store semantics should apply equally.
			if (r.Method == http.MethodGet || r.Method == http.MethodHead) && r.URL != nil && r.URL.Path == "/webrtc/ice" {
				w.Header().Set("Cache-Control", "no-store")
				w.Header().Set("Pragma", "no-cache")
				w.Header().Set("Expires", "0")
			}
			next.ServeHTTP(w, r)
		})
	}
}

// SetMetrics wires a shared metrics registry into the server. When set, certain
// endpoints (e.g. auth failures) will increment counters.
//
// It should only be called during startup before Serve is called.
func (s *server) SetMetrics(m *metrics.Metrics) {
	s.metrics = m
}

// Mux returns the underlying ServeMux for registering additional routes.
// It must only be used during startup before Serve is called.
func (s *server) Mux() *http.ServeMux {
	return s.mux
}

func (s *server) Serve(l net.Listener) error {
	s.ready.Store(true)
	s.log.Info("http server serving", "addr", l.Addr().String())
	return s.srv.Serve(l)
}

func (s *server) Shutdown(ctx context.Context) error {
	s.ready.Store(false)
	return s.srv.Shutdown(ctx)
}

func (s *server) registerRoutes() {
	s.mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, r *http.Request) {
		writeJSON(w, http.StatusOK, map[string]any{"ok": true})
	})

	s.mux.HandleFunc("GET /readyz", func(w http.ResponseWriter, r *http.Request) {
		if !s.ready.Load() {
			writeJSON(w, http.StatusServiceUnavailable, map[string]any{"ready": false})
			return
		}
		if err := s.cfg.ICEConfigError(); err != nil {
			writeJSON(w, http.StatusServiceUnavailable, map[string]any{"ready": false, "error": err.Error()})
			return
		}
		writeJSON(w, http.StatusOK, map[string]any{"ready": true})
	})

	s.mux.HandleFunc("GET /version", func(w http.ResponseWriter, r *http.Request) {
		writeJSON(w, http.StatusOK, s.build)
	})

	s.mux.HandleFunc("GET /webrtc/ice", func(w http.ResponseWriter, r *http.Request) {
		incAuthFailure := func() {
			if s.metrics != nil {
				s.metrics.Inc(metrics.AuthFailure)
			}
		}

		if s.cfg.AuthMode != config.AuthModeNone {
			cred, err := auth.CredentialFromRequest(s.cfg.AuthMode, r)
			if err != nil {
				if errors.Is(err, auth.ErrMissingCredentials) {
					incAuthFailure()
					writeJSON(w, http.StatusUnauthorized, map[string]any{"code": "unauthorized", "message": "unauthorized"})
					return
				}
				writeJSON(w, http.StatusInternalServerError, map[string]any{"code": "internal_error", "message": "internal error"})
				return
			}
			verifier, err := auth.NewVerifier(s.cfg)
			if err != nil {
				writeJSON(w, http.StatusInternalServerError, map[string]any{"code": "internal_error", "message": "internal error"})
				return
			}
			if err := verifier.Verify(cred); err != nil {
				if errors.Is(err, auth.ErrMissingCredentials) || errors.Is(err, auth.ErrInvalidCredentials) || errors.Is(err, auth.ErrUnsupportedJWT) {
					incAuthFailure()
					writeJSON(w, http.StatusUnauthorized, map[string]any{"code": "unauthorized", "message": "unauthorized"})
					return
				}
				writeJSON(w, http.StatusInternalServerError, map[string]any{"code": "internal_error", "message": "internal error"})
				return
			}
		}

		if err := s.cfg.ICEConfigError(); err != nil {
			writeJSON(w, http.StatusServiceUnavailable, map[string]any{"error": err.Error()})
			return
		}
		iceServers := s.cfg.ICEServers
		if iceServers == nil {
			iceServers = []webrtc.ICEServer{}
		}
		if s.cfg.TURNREST.Enabled() {
			gen, err := turnrest.NewGenerator(turnrest.GeneratorConfig{
				SharedSecret:   s.cfg.TURNREST.SharedSecret,
				TTLSeconds:     s.cfg.TURNREST.TTLSeconds,
				UsernamePrefix: s.cfg.TURNREST.UsernamePrefix,
				Now:            time.Now,
			})
			if err != nil {
				writeJSON(w, http.StatusInternalServerError, map[string]any{"code": "internal_error", "message": "internal error"})
				return
			}
			creds, err := gen.GenerateRandom()
			if err != nil {
				writeJSON(w, http.StatusInternalServerError, map[string]any{"code": "internal_error", "message": "internal error"})
				return
			}
			iceServers = withTURNRESTCredentials(iceServers, creds.Username, creds.Credential)
		}
		writeJSON(w, http.StatusOK, map[string]any{"iceServers": iceServers})
	})
}

type middleware func(http.Handler) http.Handler

func chain(handler http.Handler, middlewares ...middleware) http.Handler {
	h := handler
	for i := len(middlewares) - 1; i >= 0; i-- {
		h = middlewares[i](h)
	}
	return h
}

func recoverMiddleware(logger *slog.Logger) middleware {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			defer func() {
				if rec := recover(); rec != nil {
					logger.Error("panic in http handler", "recover", rec, "stack", string(debug.Stack()))
					http.Error(w, http.StatusText(http.StatusInternalServerError), http.StatusInternalServerError)
				}
			}()
			next.ServeHTTP(w, r)
		})
	}
}

func requestIDMiddleware() middleware {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			reqID := r.Header.Get("X-Request-ID")
			if reqID == "" {
				var buf [16]byte
				if _, err := rand.Read(buf[:]); err == nil {
					reqID = hex.EncodeToString(buf[:])
				}
			}
			if reqID != "" {
				r.Header.Set("X-Request-ID", reqID)
				w.Header().Set("X-Request-ID", reqID)
			}
			next.ServeHTTP(w, r)
		})
	}
}

type statusWriter struct {
	http.ResponseWriter
	status int
}

func (w *statusWriter) WriteHeader(status int) {
	w.status = status
	w.ResponseWriter.WriteHeader(status)
}

func (w *statusWriter) Flush() {
	if flusher, ok := w.ResponseWriter.(http.Flusher); ok {
		flusher.Flush()
	}
}

func (w *statusWriter) Hijack() (net.Conn, *bufio.ReadWriter, error) {
	hijacker, ok := w.ResponseWriter.(http.Hijacker)
	if !ok {
		return nil, nil, http.ErrNotSupported
	}
	// WebSocket upgrades typically bypass WriteHeader, so track 101 explicitly to
	// avoid logging these requests as 200 OK.
	if w.status == http.StatusOK {
		w.status = http.StatusSwitchingProtocols
	}
	return hijacker.Hijack()
}

func (w *statusWriter) Push(target string, opts *http.PushOptions) error {
	pusher, ok := w.ResponseWriter.(http.Pusher)
	if !ok {
		return http.ErrNotSupported
	}
	return pusher.Push(target, opts)
}

func (w *statusWriter) Unwrap() http.ResponseWriter {
	return w.ResponseWriter
}

func requestLoggerMiddleware(logger *slog.Logger) middleware {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			sw := &statusWriter{ResponseWriter: w, status: http.StatusOK}
			start := time.Now()

			next.ServeHTTP(sw, r)

			reqID := r.Header.Get("X-Request-ID")
			logger.Info("http_request",
				"method", r.Method,
				"path", r.URL.Path,
				"status", sw.status,
				"duration_ms", time.Since(start).Milliseconds(),
				"remote_addr", r.RemoteAddr,
				"request_id", reqID,
			)
		})
	}
}

// writeJSON writes a JSON response body and sets the Content-Type header.
func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)

	enc := json.NewEncoder(w)
	enc.SetEscapeHTML(true)
	_ = enc.Encode(v)
}

func (s *server) Close() error {
	s.ready.Store(false)
	return s.srv.Close()
}
