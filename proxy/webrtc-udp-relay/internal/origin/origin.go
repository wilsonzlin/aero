package origin

import (
	"net/url"
	"strconv"
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
	if trimmed == "" {
		return "", "", false
	}
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

	rawHostname, rawPort, ok := splitHostPort(u.Host)
	if !ok {
		return "", "", false
	}

	hostname := strings.ToLower(rawHostname)
	if hostname == "" {
		return "", "", false
	}

	var port uint64
	if rawPort != "" {
		n, err := strconv.ParseUint(rawPort, 10, 16)
		if err != nil || n == 0 || n > 65535 {
			return "", "", false
		}
		port = n
	}

	if (scheme == "http" && port == 80) || (scheme == "https" && port == 443) {
		port = 0
	}

	host = hostname
	if strings.Contains(hostname, ":") {
		host = "[" + hostname + "]"
	}
	if port != 0 {
		host = host + ":" + strconv.FormatUint(port, 10)
	}
	return scheme + "://" + host, host, true
}

// IsAllowed returns true when the normalized origin is allowed to access the
// given request host.
//
// If allowedOrigins is non-empty, each entry must be either "*" or a normalized
// origin string (as produced by NormalizeHeader).
//
// Otherwise the default policy is same-host only (host[:port] must match the
// incoming request's Host header; default ports are treated as equivalent).
func IsAllowed(normalizedOrigin, originHost, requestHost string, allowedOrigins []string) bool {
	if len(allowedOrigins) > 0 {
		for _, allowed := range allowedOrigins {
			if allowed == "*" || allowed == normalizedOrigin {
				return true
			}
		}
		return false
	}

	// Default: same host:port. We intentionally don't compare scheme because the
	// relay may sit behind a TLS-terminating reverse proxy and see the request as
	// HTTP while the browser Origin is HTTPS.
	scheme := ""
	switch {
	case strings.HasPrefix(normalizedOrigin, "http://"):
		scheme = "http"
	case strings.HasPrefix(normalizedOrigin, "https://"):
		scheme = "https"
	default:
		// "null" cannot match a host-based request, and anything else indicates a
		// bug since callers should normalize/validate first.
		return false
	}

	normalizedRequestHost, ok := normalizeRequestHost(requestHost, scheme)
	if !ok {
		return false
	}
	return originHost == normalizedRequestHost
}

func normalizeRequestHost(requestHost, scheme string) (string, bool) {
	trimmed := strings.ToLower(strings.TrimSpace(requestHost))
	if trimmed == "" {
		return "", false
	}

	rawHostname, rawPort, ok := splitHostPort(trimmed)
	if !ok {
		return "", false
	}

	hostname := strings.ToLower(rawHostname)
	if hostname == "" {
		return "", false
	}

	var port uint64
	if rawPort != "" {
		n, err := strconv.ParseUint(rawPort, 10, 16)
		if err != nil || n == 0 || n > 65535 {
			return "", false
		}
		port = n
	}

	if (scheme == "http" && port == 80) || (scheme == "https" && port == 443) {
		port = 0
	}

	host := hostname
	if strings.Contains(hostname, ":") {
		host = "[" + hostname + "]"
	}
	if port != 0 {
		host = host + ":" + strconv.FormatUint(port, 10)
	}
	return host, true
}

// splitHostPort splits an authority host[:port] string.
//
// The hostname is returned without brackets for IPv6 literals. The port is
// returned as-is (not validated) and will be empty when absent.
func splitHostPort(rawHost string) (hostname, port string, ok bool) {
	if rawHost == "" {
		return "", "", false
	}

	if strings.HasPrefix(rawHost, "[") {
		end := strings.IndexByte(rawHost, ']')
		if end < 0 {
			return "", "", false
		}
		hostname = rawHost[1:end]
		rest := rawHost[end+1:]
		if rest == "" {
			return hostname, "", true
		}
		if !strings.HasPrefix(rest, ":") {
			return "", "", false
		}
		port = rest[1:]
		if port == "" {
			return "", "", false
		}
		return hostname, port, true
	}

	switch strings.Count(rawHost, ":") {
	case 0:
		return rawHost, "", true
	case 1:
		parts := strings.SplitN(rawHost, ":", 2)
		if parts[0] == "" || parts[1] == "" {
			return "", "", false
		}
		return parts[0], parts[1], true
	default:
		// Unbracketed IPv6 literals are not valid in the authority component.
		return "", "", false
	}
}
