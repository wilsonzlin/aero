package httpserver

import (
	"net/http"
	"strings"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/origin"
)

func (s *Server) originMiddleware() Middleware {
	return func(next http.Handler) http.Handler {
		return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			s.withOriginPolicy(func(w http.ResponseWriter, r *http.Request) {
				next.ServeHTTP(w, r)
			})(w, r)
		})
	}
}

func (s *Server) withOriginPolicy(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		originHeader := strings.TrimSpace(r.Header.Get("Origin"))
		if originHeader == "" {
			next(w, r)
			return
		}

		normalizedOrigin, originHost, ok := origin.NormalizeHeader(originHeader)
		if !ok || !origin.IsAllowed(normalizedOrigin, originHost, r.Host, s.cfg.AllowedOrigins) {
			http.Error(w, "forbidden", http.StatusForbidden)
			return
		}

		// Only send CORS headers when the browser sends an Origin header. Same-origin
		// requests don't require them, but setting them is harmless and makes it
		// possible to run the frontend on a separate origin during development.
		w.Header().Set("Access-Control-Allow-Origin", normalizedOrigin)
		w.Header().Set("Access-Control-Allow-Credentials", "true")
		w.Header().Set("Access-Control-Expose-Headers", "X-Request-ID")
		w.Header().Add("Vary", "Origin")

		// Basic preflight support for browser clients. The per-route handler doesn't
		// need to run for preflight.
		if r.Method == http.MethodOptions && r.Header.Get("Access-Control-Request-Method") != "" {
			w.Header().Set("Access-Control-Allow-Methods", "GET,POST,PUT,PATCH,DELETE,OPTIONS")
			if requestHeaders := strings.TrimSpace(r.Header.Get("Access-Control-Request-Headers")); requestHeaders != "" {
				w.Header().Set("Access-Control-Allow-Headers", requestHeaders)
			}
			w.Header().Set("Access-Control-Max-Age", "600")
			w.WriteHeader(http.StatusNoContent)
			return
		}

		next(w, r)
	}
}
