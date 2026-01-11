package origin

import "testing"

func TestNormalizeHeader(t *testing.T) {
	t.Run("normalizes scheme and host", func(t *testing.T) {
		normalized, host, ok := NormalizeHeader("HTTPS://Example.COM:443")
		if !ok {
			t.Fatalf("expected ok=true")
		}
		if normalized != "https://example.com:443" {
			t.Fatalf("normalized=%q, want %q", normalized, "https://example.com:443")
		}
		if host != "example.com:443" {
			t.Fatalf("host=%q, want %q", host, "example.com:443")
		}
	})

	t.Run("allows trailing slash", func(t *testing.T) {
		normalized, host, ok := NormalizeHeader("http://localhost:5173/")
		if !ok {
			t.Fatalf("expected ok=true")
		}
		if normalized != "http://localhost:5173" {
			t.Fatalf("normalized=%q, want %q", normalized, "http://localhost:5173")
		}
		if host != "localhost:5173" {
			t.Fatalf("host=%q, want %q", host, "localhost:5173")
		}
	})

	t.Run("allows null origin", func(t *testing.T) {
		normalized, host, ok := NormalizeHeader("null")
		if !ok {
			t.Fatalf("expected ok=true")
		}
		if normalized != "null" || host != "" {
			t.Fatalf("normalized=%q host=%q, want normalized=%q host=%q", normalized, host, "null", "")
		}
	})

	t.Run("rejects scheme other than http/https", func(t *testing.T) {
		if _, _, ok := NormalizeHeader("ftp://example.com"); ok {
			t.Fatalf("expected ok=false")
		}
	})

	t.Run("rejects path, query, credentials, fragment", func(t *testing.T) {
		cases := []string{
			"https://example.com/path",
			"https://example.com/?q=1",
			"https://user@example.com",
			"https://example.com/#frag",
		}
		for _, c := range cases {
			if _, _, ok := NormalizeHeader(c); ok {
				t.Fatalf("expected ok=false for %q", c)
			}
		}
	})
}

func TestIsAllowed(t *testing.T) {
	t.Run("default is same host:port only", func(t *testing.T) {
		normalized, host, ok := NormalizeHeader("https://app.example.com")
		if !ok {
			t.Fatalf("NormalizeHeader ok=false")
		}
		if IsAllowed(normalized, host, "app.example.com", nil) != true {
			t.Fatalf("expected same-host to be allowed")
		}
		if IsAllowed(normalized, host, "app.example.com:443", nil) != false {
			t.Fatalf("expected different host header to be rejected")
		}
	})

	t.Run("allows star", func(t *testing.T) {
		normalized, host, ok := NormalizeHeader("https://app.example.com")
		if !ok {
			t.Fatalf("NormalizeHeader ok=false")
		}
		if !IsAllowed(normalized, host, "whatever:1234", []string{"*"}) {
			t.Fatalf("expected * to allow any origin")
		}
	})

	t.Run("allows explicit origin", func(t *testing.T) {
		normalized, host, ok := NormalizeHeader("https://app.example.com")
		if !ok {
			t.Fatalf("NormalizeHeader ok=false")
		}
		if !IsAllowed(normalized, host, "relay.example.com", []string{"https://app.example.com"}) {
			t.Fatalf("expected explicit origin to be allowed")
		}
		if IsAllowed(normalized, host, "relay.example.com", []string{"https://other.example.com"}) {
			t.Fatalf("expected non-matching origin to be rejected")
		}
	})

	t.Run("allows null origin when configured", func(t *testing.T) {
		normalized, host, ok := NormalizeHeader("null")
		if !ok {
			t.Fatalf("NormalizeHeader ok=false")
		}
		if !IsAllowed(normalized, host, "relay.example.com", []string{"null"}) {
			t.Fatalf("expected null origin to be allowed when configured")
		}
	})
}
