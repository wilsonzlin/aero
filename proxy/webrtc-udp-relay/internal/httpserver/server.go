package httpserver

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"log/slog"
	"net"
	"net/http"
	"runtime/debug"
	"sync/atomic"
	"time"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
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

	s.mux.HandleFunc("GET /webrtc/ice", s.withOriginPolicy(func(w http.ResponseWriter, r *http.Request) {
		if err := s.cfg.ICEConfigError(); err != nil {
			WriteJSON(w, http.StatusServiceUnavailable, map[string]any{"error": err.Error()})
			return
		}
		WriteJSON(w, http.StatusOK, map[string]any{"iceServers": s.cfg.ICEServers})
	}))
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
