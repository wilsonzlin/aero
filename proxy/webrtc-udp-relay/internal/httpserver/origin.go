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
		origins := r.Header.Values("Origin")
		if len(origins) == 0 {
			next(w, r)
			return
		}
		if len(origins) > 1 {
			WriteJSON(w, http.StatusForbidden, map[string]any{"code": "forbidden", "message": "forbidden"})
			return
		}

		originHeader := strings.TrimSpace(origins[0])
		if originHeader == "" {
			next(w, r)
			return
		}

		normalizedOrigin, originHost, ok := origin.NormalizeHeader(originHeader)
		if !ok || !origin.IsAllowed(normalizedOrigin, originHost, r.Host, s.cfg.AllowedOrigins) {
			WriteJSON(w, http.StatusForbidden, map[string]any{"code": "forbidden", "message": "forbidden"})
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

// NormalizedOriginFromRequest returns a canonical Origin value for r.
//
// If r includes an Origin header and it parses as a valid browser Origin, the
// returned value is normalized (lower-cased scheme/host, no path/query/fragment,
// default ports removed).
//
// If r has no Origin header, this derives an origin from the request's Host and
// scheme. This is primarily used for L2 backend WebSocket dialing, so the relay
// can provide a deterministic Origin to a backend that enforces Origin checks.
func NormalizedOriginFromRequest(r *http.Request) string {
	if r == nil {
		return ""
	}

	if len(r.Header.Values("Origin")) > 1 {
		return ""
	}

	originHeader := strings.TrimSpace(r.Header.Get("Origin"))
	if originHeader != "" {
		if normalized, _, ok := origin.NormalizeHeader(originHeader); ok {
			return normalized
		}
		// Fall back to the raw value; production deployments typically enforce and
		// normalize Origin in the outer middleware, but unit tests may bypass it.
		return originHeader
	}

	host := strings.TrimSpace(r.Host)
	if host == "" {
		return ""
	}
	host = asciiLowerIfNeeded(host)

	scheme := ""
	if xfProto := strings.TrimSpace(r.Header.Get("X-Forwarded-Proto")); xfProto != "" {
		// Use the first value in the X-Forwarded-Proto list.
		if i := strings.IndexByte(xfProto, ','); i >= 0 {
			xfProto = xfProto[:i]
		}
		xfProto = strings.TrimSpace(xfProto)
		switch {
		case strings.EqualFold(xfProto, "http"):
			scheme = "http"
		case strings.EqualFold(xfProto, "https"):
			scheme = "https"
		}
	}
	if scheme == "" {
		if r.TLS != nil {
			scheme = "https"
		} else {
			scheme = "http"
		}
	}

	candidate := scheme + "://" + host
	if normalized, _, ok := origin.NormalizeHeader(candidate); ok {
		return normalized
	}
	return candidate
}

func asciiLowerIfNeeded(s string) string {
	for i := 0; i < len(s); i++ {
		c := s[i]
		if c >= 'A' && c <= 'Z' {
			b := []byte(s)
			for j := i; j < len(b); j++ {
				c := b[j]
				if c >= 'A' && c <= 'Z' {
					b[j] = c + ('a' - 'A')
				}
			}
			return string(b)
		}
	}
	return s
}
