package origin

import (
	"net/netip"
	"net/url"
	"strconv"
	"strings"
	"unicode"
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
	if !isASCIIOriginString(trimmed) {
		return "", "", false
	}
	if strings.Contains(trimmed, "%") {
		return "", "", false
	}
	if strings.Contains(trimmed, ",") {
		return "", "", false
	}
	// Some URL libraries (notably Go's net/url) reject additional host codepoints
	// that WHATWG URL parsers accept. Reject them here so Origin validation stays
	// consistent across components.
	if strings.ContainsAny(trimmed, "{}`") {
		return "", "", false
	}
	// Reject query and fragment delimiters even when empty. WHATWG URL parsers
	// normalize `https://example.com?` or `https://example.com#` to the same origin,
	// but browsers don't emit those in Origin headers.
	if strings.Contains(trimmed, "?") || strings.Contains(trimmed, "#") {
		return "", "", false
	}
	schemePrefix := ""
	switch {
	case asciiHasPrefixFold(trimmed, "http://"):
		schemePrefix = "http://"
	case asciiHasPrefixFold(trimmed, "https://"):
		schemePrefix = "https://"
	default:
		return "", "", false
	}
	// Reject `http:///` / `https:///` (extra slash after the authority prefix).
	if len(trimmed) > len(schemePrefix) && trimmed[len(schemePrefix)] == '/' {
		return "", "", false
	}
	if strings.Contains(trimmed, "\\") {
		return "", "", false
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

	scheme := ""
	switch {
	case strings.EqualFold(u.Scheme, "http"):
		scheme = "http"
	case strings.EqualFold(u.Scheme, "https"):
		scheme = "https"
	default:
		return "", "", false
	}

	rawHostname, rawPort, ok := splitHostPort(u.Host)
	if !ok {
		return "", "", false
	}

	hostname, ok := canonicalizeHostname(rawHostname)
	if !ok {
		return "", "", false
	}
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
	trimmed := strings.TrimSpace(requestHost)
	if trimmed == "" {
		return "", false
	}
	if !isASCIIOriginString(trimmed) {
		return "", false
	}
	if strings.Contains(trimmed, "%") {
		return "", false
	}
	if strings.Contains(trimmed, ",") {
		return "", false
	}
	if strings.Contains(trimmed, "\\") {
		return "", false
	}
	if strings.Contains(trimmed, "@") {
		return "", false
	}
	if strings.Contains(trimmed, "/") || strings.Contains(trimmed, "?") || strings.Contains(trimmed, "#") {
		return "", false
	}

	rawHostname, rawPort, ok := splitHostPort(trimmed)
	if !ok {
		return "", false
	}

	hostname, ok := canonicalizeHostname(rawHostname)
	if !ok {
		return "", false
	}
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

func isASCIIOriginString(s string) bool {
	for _, r := range s {
		if r <= 0x20 || r >= 0x7f || unicode.IsSpace(r) || unicode.IsControl(r) {
			return false
		}
	}
	return true
}

const ipv4SaturationLimit = uint64(1) << 32

func canonicalizeHostname(hostname string) (string, bool) {
	if hostname == "" {
		return "", false
	}
	if strings.ContainsAny(hostname, "[]<>") {
		return "", false
	}

	// Bracketed IPv6 literals are stripped of brackets by splitHostPort but will
	// still contain ":".
	if strings.Contains(hostname, ":") {
		// WHATWG URLs (and browser Origin headers) do not include IPv6 zone
		// identifiers.
		if strings.Contains(hostname, "%") {
			return "", false
		}
		addr, err := netip.ParseAddr(hostname)
		if err != nil || !addr.Is6() {
			return "", false
		}
		return serializeIPv6(addr), true
	}

	// Match WHATWG host parsing: if the host ends in a number, it must parse as an
	// IPv4 address (including octal/hex and shorthand forms). Otherwise the URL is
	// invalid.
	if endsInIPv4Number(hostname) {
		addr, ok := parseIPv4Address(hostname)
		if !ok {
			return "", false
		}
		return serializeIPv4(addr), true
	}

	// Domain name: normalize to lowercase. (IP-literals are already in their canonical form above.)
	return asciiLowerIfNeeded(hostname), true
}

func asciiHasPrefixFold(s, prefix string) bool {
	if len(s) < len(prefix) {
		return false
	}
	for i := 0; i < len(prefix); i++ {
		c := s[i]
		if c >= 'A' && c <= 'Z' {
			c = c + ('a' - 'A')
		}
		if c != prefix[i] {
			return false
		}
	}
	return true
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

func endsInIPv4Number(host string) bool {
	parts := strings.Split(host, ".")
	if len(parts) > 0 && parts[len(parts)-1] == "" {
		// Remove exactly one trailing dot component (WHATWG "ends in a number").
		parts = parts[:len(parts)-1]
	}
	if len(parts) == 0 {
		return false
	}
	return isIPv4NumberCandidate(parts[len(parts)-1])
}

func isIPv4NumberCandidate(part string) bool {
	if part == "" {
		return false
	}

	if len(part) >= 2 && part[0] == '0' && (part[1] == 'x' || part[1] == 'X') {
		// `0x` (empty hex) is treated as 0, but non-hex digits should be rejected.
		for i := 2; i < len(part); i++ {
			digit, ok := digitValue(part[i])
			if !ok || digit >= 16 {
				return false
			}
		}
		return true
	}

	for i := 0; i < len(part); i++ {
		if part[i] < '0' || part[i] > '9' {
			return false
		}
	}
	return true
}

func parseIPv4Address(input string) (uint32, bool) {
	parts := strings.Split(input, ".")
	if len(parts) > 0 && parts[len(parts)-1] == "" {
		// IPv4 parser removes exactly one trailing empty segment.
		parts = parts[:len(parts)-1]
	}
	if len(parts) == 0 || len(parts) > 4 {
		return 0, false
	}

	nums := make([]uint64, 0, len(parts))
	for _, part := range parts {
		if part == "" {
			return 0, false
		}
		n, ok := parseIPv4Number(part)
		if !ok {
			return 0, false
		}
		nums = append(nums, n)
	}

	if len(nums) > 1 {
		for _, n := range nums[:len(nums)-1] {
			if n > 255 {
				return 0, false
			}
		}
	}

	// The last component may use the remaining bytes (e.g. "127.1").
	last := nums[len(nums)-1]
	remainingBytes := 5 - len(nums)
	maxLast := (uint64(1) << uint(8*remainingBytes)) - 1
	if last > maxLast {
		return 0, false
	}

	value := last
	for i, n := range nums[:len(nums)-1] {
		shift := uint(8 * (3 - i))
		value += n << shift
	}
	if value >= ipv4SaturationLimit {
		return 0, false
	}
	return uint32(value), true
}

func parseIPv4Number(part string) (uint64, bool) {
	if part == "" {
		return 0, false
	}

	base := uint64(10)
	input := part
	if len(part) >= 2 && part[0] == '0' && (part[1] == 'x' || part[1] == 'X') {
		base = 16
		input = part[2:]
		// `0x` is treated as 0 by WHATWG parsing (and Node's URL implementation).
		if input == "" {
			return 0, true
		}
	} else if len(part) > 1 && part[0] == '0' {
		base = 8
		input = part[1:]
	}

	var out uint64
	for i := 0; i < len(input); i++ {
		c := input[i]
		digit, ok := digitValue(c)
		if !ok || digit >= base {
			return 0, false
		}

		if out < ipv4SaturationLimit {
			out = out*base + digit
			if out >= ipv4SaturationLimit {
				out = ipv4SaturationLimit
			}
		}
	}

	return out, true
}

func digitValue(c byte) (uint64, bool) {
	switch {
	case c >= '0' && c <= '9':
		return uint64(c - '0'), true
	case c >= 'a' && c <= 'f':
		return uint64(10 + (c - 'a')), true
	case c >= 'A' && c <= 'F':
		return uint64(10 + (c - 'A')), true
	default:
		return 0, false
	}
}

func serializeIPv4(addr uint32) string {
	return strconv.FormatUint(uint64(addr>>24), 10) + "." +
		strconv.FormatUint(uint64((addr>>16)&0xff), 10) + "." +
		strconv.FormatUint(uint64((addr>>8)&0xff), 10) + "." +
		strconv.FormatUint(uint64(addr&0xff), 10)
}

func serializeIPv6(addr netip.Addr) string {
	b := addr.As16()
	segments := [8]uint16{
		uint16(b[0])<<8 | uint16(b[1]),
		uint16(b[2])<<8 | uint16(b[3]),
		uint16(b[4])<<8 | uint16(b[5]),
		uint16(b[6])<<8 | uint16(b[7]),
		uint16(b[8])<<8 | uint16(b[9]),
		uint16(b[10])<<8 | uint16(b[11]),
		uint16(b[12])<<8 | uint16(b[13]),
		uint16(b[14])<<8 | uint16(b[15]),
	}

	bestStart, bestLen := -1, 0
	curStart, curLen := -1, 0
	for i := 0; i < len(segments); i++ {
		if segments[i] == 0 {
			if curStart == -1 {
				curStart, curLen = i, 1
			} else {
				curLen++
			}
			continue
		}

		if curLen >= 2 && curLen > bestLen {
			bestStart, bestLen = curStart, curLen
		}
		curStart, curLen = -1, 0
	}
	if curLen >= 2 && curLen > bestLen {
		bestStart, bestLen = curStart, curLen
	}

	var out strings.Builder
	needSep := false
	for i := 0; i < len(segments); i++ {
		if bestStart != -1 && i == bestStart {
			out.WriteString("::")
			needSep = false
			i += bestLen - 1
			continue
		}
		if needSep {
			out.WriteByte(':')
		}
		out.WriteString(strconv.FormatUint(uint64(segments[i]), 16))
		needSep = true
	}

	return out.String()
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
		addr, err := netip.ParseAddr(hostname)
		if err != nil || !addr.Is6() {
			return "", "", false
		}
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
