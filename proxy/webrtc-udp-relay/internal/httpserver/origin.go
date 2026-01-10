package httpserver

import (
	"net/http"
	"net/url"
	"strings"
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

		normalizedOrigin, originHost, ok := normalizeOriginHeader(originHeader)
		if !ok || !s.isOriginAllowed(normalizedOrigin, originHost, r.Host) {
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

func normalizeOriginHeader(originHeader string) (normalizedOrigin string, host string, ok bool) {
	trimmed := strings.TrimSpace(originHeader)
	if trimmed == "null" {
		return "null", "", true
	}

	u, err := url.Parse(trimmed)
	if err != nil || u.Scheme == "" || u.Host == "" {
		return "", "", false
	}

	scheme := strings.ToLower(u.Scheme)
	host = strings.ToLower(u.Host)
	return scheme + "://" + host, host, true
}

func (s *Server) isOriginAllowed(normalizedOrigin, originHost, requestHost string) bool {
	if len(s.cfg.AllowedOrigins) > 0 {
		for _, allowed := range s.cfg.AllowedOrigins {
			if allowed == "*" || allowed == normalizedOrigin {
				return true
			}
		}
		return false
	}

	// Default: same host only. This still allows a TLS-terminating reverse proxy
	// that forwards the request over HTTP without an X-Forwarded-Proto header,
	// because we compare only host:port here.
	return originHost == strings.ToLower(strings.TrimSpace(requestHost))
}
