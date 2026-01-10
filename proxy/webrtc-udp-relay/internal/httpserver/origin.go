package httpserver

import (
	"net/http"
	"net/url"
	"strings"
)

func (s *Server) withOriginPolicy(next http.HandlerFunc) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if !isSameOrigin(r) {
			http.Error(w, "forbidden", http.StatusForbidden)
			return
		}
		next(w, r)
	}
}

// isSameOrigin blocks browser cross-origin reads of endpoints that may contain secrets
// (for example TURN credentials).
//
// Requests without an Origin header (for example curl) are allowed.
func isSameOrigin(r *http.Request) bool {
	originHeader := strings.TrimSpace(r.Header.Get("Origin"))
	if originHeader == "" {
		return true
	}

	originURL, err := url.Parse(originHeader)
	if err != nil || originURL.Scheme == "" || originURL.Host == "" {
		return false
	}
	normalizedOrigin := originURL.Scheme + "://" + originURL.Host
	expectedOrigin := requestOrigin(r)

	// Prefer exact origin match.
	if normalizedOrigin == expectedOrigin {
		return true
	}

	// Be tolerant of reverse proxies that terminate TLS but don't set X-Forwarded-Proto.
	// In that case, the browser will send `Origin: https://...` but the relay will see
	// an HTTP request. As long as the host matches, treat it as same-origin.
	return originURL.Host == r.Host
}

func requestOrigin(r *http.Request) string {
	scheme := "http"
	if r.TLS != nil {
		scheme = "https"
	}
	if forwarded := strings.TrimSpace(r.Header.Get("X-Forwarded-Proto")); forwarded != "" {
		if comma := strings.IndexByte(forwarded, ','); comma >= 0 {
			forwarded = forwarded[:comma]
		}
		forwarded = strings.TrimSpace(forwarded)
		if forwarded == "http" || forwarded == "https" {
			scheme = forwarded
		}
	}

	return scheme + "://" + r.Host
}
