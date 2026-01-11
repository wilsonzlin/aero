package origin

import (
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

type vector struct {
	Raw        string  `json:"raw"`
	Normalized *string `json:"normalized"`
}

func TestNormalizeHeader(t *testing.T) {
	t.Run("normalizes scheme and host", func(t *testing.T) {
		normalized, host, ok := NormalizeHeader("HTTPS://Example.COM:443")
		if !ok {
			t.Fatalf("expected ok=true")
		}
		if normalized != "https://example.com" {
			t.Fatalf("normalized=%q, want %q", normalized, "https://example.com")
		}
		if host != "example.com" {
			t.Fatalf("host=%q, want %q", host, "example.com")
		}
	})
}

func TestNormalizeHeader_MatchesSharedVectors(t *testing.T) {
	_, file, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatalf("runtime.Caller failed")
	}
	vectorsPath := filepath.Clean(filepath.Join(filepath.Dir(file), "../../../../docs/origin-allowlist-test-vectors.json"))
	contents, err := os.ReadFile(vectorsPath)
	if err != nil {
		t.Fatalf("read %s: %v", vectorsPath, err)
	}

	var vectors []vector
	if err := json.Unmarshal(contents, &vectors); err != nil {
		t.Fatalf("unmarshal vectors: %v", err)
	}

	for _, v := range vectors {
		normalized, host, ok := NormalizeHeader(v.Raw)
		if v.Normalized == nil {
			if ok {
				t.Fatalf("raw=%q: expected invalid, got normalized=%q", v.Raw, normalized)
			}
			continue
		}

		if !ok {
			t.Fatalf("raw=%q: expected ok", v.Raw)
		}
		if normalized != *v.Normalized {
			t.Fatalf("raw=%q: normalized=%q, want %q", v.Raw, normalized, *v.Normalized)
		}

		if normalized == "null" {
			if host != "" {
				t.Fatalf("raw=%q: host=%q, want empty for null origin", v.Raw, host)
			}
			continue
		}

		wantHost := strings.TrimPrefix(normalized, "http://")
		wantHost = strings.TrimPrefix(wantHost, "https://")
		if host != wantHost {
			t.Fatalf("raw=%q: host=%q, want %q", v.Raw, host, wantHost)
		}
	}
}

func TestIsAllowed_DefaultSameHostTreatsDefaultPortsAsEquivalent(t *testing.T) {
	normalized, host, ok := NormalizeHeader("https://app.example.com")
	if !ok {
		t.Fatalf("NormalizeHeader ok=false")
	}
	if !IsAllowed(normalized, host, "app.example.com:443", nil) {
		t.Fatalf("expected host header with default port to be allowed")
	}
	if IsAllowed(normalized, host, "app.example.com:80", nil) {
		t.Fatalf("expected non-default port to be rejected")
	}
}
