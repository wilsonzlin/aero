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

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/auth"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/turnrest"
)

var ErrServerClosed = http.ErrServerClosed

type BuildInfo struct {
	Commit    string `json:"commit"`
	BuildTime string `json:"buildTime"`
}

type Server struct {
	log   *slog.Logger
	cfg   config.Config
	build BuildInfo

	ready atomic.Bool

	metrics *metrics.Metrics

	mux *http.ServeMux
	srv *http.Server
}

func New(cfg config.Config, logger *slog.Logger, build BuildInfo) *Server {
	s := &Server{
		log:   logger,
		cfg:   cfg,
		build: build,
		mux:   http.NewServeMux(),
	}

	s.registerRoutes()

	handler := chain(s.mux,
		recoverMiddleware(s.log),
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

// SetMetrics wires a shared metrics registry into the server. When set, certain
// endpoints (e.g. auth failures) will increment counters.
//
// It should only be called during startup before Serve is called.
func (s *Server) SetMetrics(m *metrics.Metrics) {
	s.metrics = m
}

// Mux returns the underlying ServeMux for registering additional routes.
// It must only be used during startup before Serve is called.
func (s *Server) Mux() *http.ServeMux {
	return s.mux
}

func (s *Server) Serve(l net.Listener) error {
	s.ready.Store(true)
	s.log.Info("http server serving", "addr", l.Addr().String())
	return s.srv.Serve(l)
}

func (s *Server) Shutdown(ctx context.Context) error {
	s.ready.Store(false)
	return s.srv.Shutdown(ctx)
}

func (s *Server) registerRoutes() {
	s.mux.HandleFunc("GET /healthz", func(w http.ResponseWriter, r *http.Request) {
		WriteJSON(w, http.StatusOK, map[string]any{"ok": true})
	})

	s.mux.HandleFunc("GET /readyz", func(w http.ResponseWriter, r *http.Request) {
		if !s.ready.Load() {
			WriteJSON(w, http.StatusServiceUnavailable, map[string]any{"ready": false})
			return
		}
		if err := s.cfg.ICEConfigError(); err != nil {
			WriteJSON(w, http.StatusServiceUnavailable, map[string]any{"ready": false, "error": err.Error()})
			return
		}
		WriteJSON(w, http.StatusOK, map[string]any{"ready": true})
	})

	s.mux.HandleFunc("GET /version", func(w http.ResponseWriter, r *http.Request) {
		WriteJSON(w, http.StatusOK, s.build)
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
					http.Error(w, "unauthorized", http.StatusUnauthorized)
					return
				}
				http.Error(w, http.StatusText(http.StatusInternalServerError), http.StatusInternalServerError)
				return
			}
			verifier, err := auth.NewVerifier(s.cfg)
			if err != nil {
				http.Error(w, http.StatusText(http.StatusInternalServerError), http.StatusInternalServerError)
				return
			}
			if err := verifier.Verify(cred); err != nil {
				if errors.Is(err, auth.ErrMissingCredentials) || errors.Is(err, auth.ErrInvalidCredentials) || errors.Is(err, auth.ErrUnsupportedJWT) {
					incAuthFailure()
					http.Error(w, "unauthorized", http.StatusUnauthorized)
					return
				}
				http.Error(w, http.StatusText(http.StatusInternalServerError), http.StatusInternalServerError)
				return
			}
		}

		if err := s.cfg.ICEConfigError(); err != nil {
			WriteJSON(w, http.StatusServiceUnavailable, map[string]any{"error": err.Error()})
			return
		}
		iceServers := s.cfg.ICEServers
		if s.cfg.TURNREST.Enabled() {
			gen, err := turnrest.NewGenerator(turnrest.GeneratorConfig{
				SharedSecret:    s.cfg.TURNREST.SharedSecret,
				TTLSeconds:      s.cfg.TURNREST.TTLSeconds,
				UsernamePrefix:  s.cfg.TURNREST.UsernamePrefix,
				Now:             time.Now,
				SessionIDSource: turnrest.CryptoRandomSessionID,
			})
			if err != nil {
				http.Error(w, http.StatusText(http.StatusInternalServerError), http.StatusInternalServerError)
				return
			}
			creds, err := gen.GenerateRandom()
			if err != nil {
				http.Error(w, http.StatusText(http.StatusInternalServerError), http.StatusInternalServerError)
				return
			}
			iceServers = withTURNRESTCredentials(iceServers, creds.Username, creds.Credential)
		}
		WriteJSON(w, http.StatusOK, map[string]any{"iceServers": iceServers})
	})
}

type Middleware func(http.Handler) http.Handler

func chain(handler http.Handler, middlewares ...Middleware) http.Handler {
	h := handler
	for i := len(middlewares) - 1; i >= 0; i-- {
		h = middlewares[i](h)
	}
	return h
}

func recoverMiddleware(logger *slog.Logger) Middleware {
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

func requestIDMiddleware() Middleware {
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

func requestLoggerMiddleware(logger *slog.Logger) Middleware {
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

// WriteJSON writes a JSON response body and sets the Content-Type header.
func WriteJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)

	enc := json.NewEncoder(w)
	enc.SetEscapeHTML(true)
	_ = enc.Encode(v)
}

func (s *Server) Close() error {
	s.ready.Store(false)
	return s.srv.Close()
}
