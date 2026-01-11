package origin

import (
	"net/url"
	"strings"
)

// NormalizeHeader validates and normalizes a browser Origin header.
//
// It returns the normalized origin (scheme://host[:port]) and the host[:port]
// portion for same-host comparisons.
//
// The special Origin value "null" is allowed and returned as-is.
func NormalizeHeader(originHeader string) (normalizedOrigin string, host string, ok bool) {
	trimmed := strings.TrimSpace(originHeader)
	if trimmed == "null" {
		return "null", "", true
	}

	u, err := url.Parse(trimmed)
	if err != nil || u.Scheme == "" || u.Host == "" {
		return "", "", false
	}
	if u.User != nil || u.RawQuery != "" || u.Fragment != "" {
		return "", "", false
	}
	if u.Path != "" && u.Path != "/" {
		return "", "", false
	}

	scheme := strings.ToLower(u.Scheme)
	if scheme != "http" && scheme != "https" {
		return "", "", false
	}
	host = strings.ToLower(u.Host)
	return scheme + "://" + host, host, true
}

// IsAllowed returns true when the normalized origin is allowed to access the
// given request host.
//
// If allowedOrigins is empty, only same-host origins are allowed (host:port must
// match the incoming request's Host header).
// Otherwise each entry must be either "*" or a normalized origin string.
func IsAllowed(normalizedOrigin, originHost, requestHost string, allowedOrigins []string) bool {
	if len(allowedOrigins) > 0 {
		for _, allowed := range allowedOrigins {
			if allowed == "*" || allowed == normalizedOrigin {
				return true
			}
		}
		return false
	}

	// Default: same host:port only. This still allows a TLS-terminating reverse
	// proxy that forwards the request over HTTP without an X-Forwarded-Proto
	// header, because we compare only host:port here.
	return originHost == strings.ToLower(strings.TrimSpace(requestHost))
}
