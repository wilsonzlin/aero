package origin

import (
	"net/url"
	"strings"
	"testing"
)

func FuzzNormalizeHeader(f *testing.F) {
	// Known-good cases from unit tests.
	f.Add("HTTPS://Example.COM:443")
	f.Add("http://010.0.0.1")
	f.Add("http://[::FFFF:192.0.2.1]")
	f.Add("null")

	// Known-bad / edge cases.
	f.Add("")
	f.Add("   ")
	f.Add("ftp://example.com")
	f.Add("https://example.com/path")
	f.Add("https://example.com?query")
	f.Add("https://example.com#frag")
	f.Add("https://example.com,https://evil.example.com")

	f.Fuzz(func(t *testing.T, originHeader string) {
		normalized1, host1, ok1 := NormalizeHeader(originHeader)
		normalized2, host2, ok2 := NormalizeHeader(originHeader)
		if ok1 != ok2 || normalized1 != normalized2 || host1 != host2 {
			t.Fatalf("non-deterministic result: ok1=%v ok2=%v normalized1=%q normalized2=%q host1=%q host2=%q", ok1, ok2, normalized1, normalized2, host1, host2)
		}

		if !ok1 {
			return
		}

		if strings.TrimSpace(normalized1) != normalized1 {
			t.Fatalf("normalized origin has leading/trailing whitespace: %q", normalized1)
		}
		if strings.ContainsAny(normalized1, " \t\r\n") {
			t.Fatalf("normalized origin contains whitespace: %q", normalized1)
		}

		if normalized1 == "null" {
			if host1 != "" {
				t.Fatalf("null origin must have empty host, got %q", host1)
			}
			// Round-trip: "null" should stay stable.
			n3, h3, ok := NormalizeHeader(normalized1)
			if !ok || n3 != "null" || h3 != "" {
				t.Fatalf("NormalizeHeader(null) unstable: ok=%v normalized=%q host=%q", ok, n3, h3)
			}
			return
		}

		if !(strings.HasPrefix(normalized1, "http://") || strings.HasPrefix(normalized1, "https://")) {
			t.Fatalf("normalized origin missing scheme: %q", normalized1)
		}
		if host1 == "" {
			t.Fatalf("normalized non-null origin must have non-empty host")
		}

		// Normalized outputs must not include path/query/fragment delimiters.
		if strings.ContainsAny(normalized1, "?#") || strings.ContainsAny(host1, "/?#") {
			t.Fatalf("normalized origin/host contains path/query/fragment delimiters: origin=%q host=%q", normalized1, host1)
		}

		wantHost := strings.TrimPrefix(normalized1, "http://")
		wantHost = strings.TrimPrefix(wantHost, "https://")
		if host1 != wantHost {
			t.Fatalf("host mismatch: normalized=%q host=%q wantHost=%q", normalized1, host1, wantHost)
		}

		// net/url parsing should succeed and reflect the normalized form.
		u, err := url.Parse(normalized1)
		if err != nil {
			t.Fatalf("url.Parse(%q): %v", normalized1, err)
		}
		if u.Scheme != "http" && u.Scheme != "https" {
			t.Fatalf("unexpected url scheme: %q", u.Scheme)
		}
		if u.Host != host1 {
			t.Fatalf("url host mismatch: parsed=%q want=%q", u.Host, host1)
		}
		if u.Path != "" || u.RawQuery != "" || u.Fragment != "" || u.User != nil {
			t.Fatalf("normalized origin parsed with unexpected components: %#v", u)
		}

		// The normalized output should be idempotent when re-parsed.
		n3, h3, ok := NormalizeHeader(normalized1)
		if !ok || n3 != normalized1 || h3 != host1 {
			t.Fatalf("NormalizeHeader not idempotent: input=%q ok=%v normalized=%q host=%q", normalized1, ok, n3, h3)
		}
	})
}

func FuzzIsAllowed(f *testing.F) {
	f.Add("https://app.example.com", "app.example.com:443", "")
	f.Add("http://010.0.0.1", "010.0.0.1", "")
	f.Add("http://[::FFFF:192.0.2.1]", "[::FFFF:192.0.2.1]", "")
	f.Add("null", "app.example.com", "")
	f.Add("https://good.example.com", "app.example.com", "*")

	f.Fuzz(func(t *testing.T, originHeader, requestHost, allowedList string) {
		allowedOrigins := splitAllowedOriginsForFuzz(allowedList)

		normalized, originHost, ok := NormalizeHeader(originHeader)
		if ok {
			// Explicit allow-list behavior must be consistent.
			if !IsAllowed(normalized, originHost, requestHost, []string{"*"}) {
				t.Fatalf("expected wildcard allow-list to allow all origins (normalized=%q)", normalized)
			}
			if !IsAllowed(normalized, originHost, requestHost, []string{normalized}) {
				t.Fatalf("expected exact allow-list match to allow origin (normalized=%q)", normalized)
			}
			if IsAllowed(normalized, originHost, requestHost, []string{normalized + "x"}) {
				t.Fatalf("expected mismatched allow-list to reject origin (normalized=%q)", normalized)
			}

			// Default policy: same-host only, with default ports treated as equivalent.
			if normalized == "null" {
				if IsAllowed(normalized, originHost, requestHost, nil) {
					t.Fatalf("expected null origin to be rejected under default policy")
				}
			} else {
				if !IsAllowed(normalized, originHost, originHost, nil) {
					t.Fatalf("expected origin host to match itself under default policy (normalized=%q host=%q)", normalized, originHost)
				}

				scheme := ""
				defaultPort := ""
				switch {
				case strings.HasPrefix(normalized, "http://"):
					scheme = "http"
					defaultPort = "80"
				case strings.HasPrefix(normalized, "https://"):
					scheme = "https"
					defaultPort = "443"
				}

				if scheme != "" {
					_, port, ok := splitHostPort(originHost)
					if ok && port == "" {
						if !IsAllowed(normalized, originHost, originHost+":"+defaultPort, nil) {
							t.Fatalf("expected default port to be treated as equivalent (normalized=%q host=%q requestHost=%q)", normalized, originHost, originHost+":"+defaultPort)
						}
					}
				}
			}
		}

		// Panic-safety: IsAllowed should be safe even for malformed inputs.
		_ = IsAllowed(normalized, originHost, requestHost, allowedOrigins)
		_ = IsAllowed(originHeader, originHeader, requestHost, allowedOrigins)
	})
}

func splitAllowedOriginsForFuzz(s string) []string {
	if s == "" {
		return nil
	}
	parts := strings.Split(s, ",")
	if len(parts) > 8 {
		parts = parts[:8]
	}
	return parts
}
