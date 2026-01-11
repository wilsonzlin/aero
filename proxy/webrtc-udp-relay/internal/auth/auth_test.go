package auth

import (
	"net/http"
	"net/url"
	"testing"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
)

func TestCredentialFromQuery(t *testing.T) {
	t.Run("none", func(t *testing.T) {
		cred, err := CredentialFromQuery(config.AuthModeNone, url.Values{"apiKey": {"x"}, "token": {"y"}})
		if err != nil {
			t.Fatalf("err=%v", err)
		}
		if cred != "" {
			t.Fatalf("cred=%q, want empty", cred)
		}
	})

	t.Run("api_key prefers apiKey but accepts token", func(t *testing.T) {
		cred, err := CredentialFromQuery(config.AuthModeAPIKey, url.Values{"apiKey": {"a"}})
		if err != nil {
			t.Fatalf("err=%v", err)
		}
		if cred != "a" {
			t.Fatalf("cred=%q, want %q", cred, "a")
		}

		cred, err = CredentialFromQuery(config.AuthModeAPIKey, url.Values{"token": {"t"}})
		if err != nil {
			t.Fatalf("err=%v", err)
		}
		if cred != "t" {
			t.Fatalf("cred=%q, want %q", cred, "t")
		}
	})

	t.Run("jwt prefers token but accepts apiKey", func(t *testing.T) {
		cred, err := CredentialFromQuery(config.AuthModeJWT, url.Values{"token": {"t"}})
		if err != nil {
			t.Fatalf("err=%v", err)
		}
		if cred != "t" {
			t.Fatalf("cred=%q, want %q", cred, "t")
		}

		cred, err = CredentialFromQuery(config.AuthModeJWT, url.Values{"apiKey": {"a"}})
		if err != nil {
			t.Fatalf("err=%v", err)
		}
		if cred != "a" {
			t.Fatalf("cred=%q, want %q", cred, "a")
		}
	})

	t.Run("missing", func(t *testing.T) {
		_, err := CredentialFromQuery(config.AuthModeAPIKey, url.Values{})
		if err == nil {
			t.Fatalf("expected error")
		}
		if err != ErrMissingCredentials {
			t.Fatalf("err=%v, want %v", err, ErrMissingCredentials)
		}
	})
}

func TestCredentialFromAuthMessage(t *testing.T) {
	t.Run("api_key prefers apiKey but accepts token", func(t *testing.T) {
		cred, err := CredentialFromAuthMessage(config.AuthModeAPIKey, WireAuthMessage{Type: "auth", APIKey: "a"})
		if err != nil {
			t.Fatalf("err=%v", err)
		}
		if cred != "a" {
			t.Fatalf("cred=%q, want %q", cred, "a")
		}

		cred, err = CredentialFromAuthMessage(config.AuthModeAPIKey, WireAuthMessage{Type: "auth", Token: "t"})
		if err != nil {
			t.Fatalf("err=%v", err)
		}
		if cred != "t" {
			t.Fatalf("cred=%q, want %q", cred, "t")
		}
	})

	t.Run("jwt prefers token but accepts apiKey", func(t *testing.T) {
		cred, err := CredentialFromAuthMessage(config.AuthModeJWT, WireAuthMessage{Type: "auth", Token: "t"})
		if err != nil {
			t.Fatalf("err=%v", err)
		}
		if cred != "t" {
			t.Fatalf("cred=%q, want %q", cred, "t")
		}

		cred, err = CredentialFromAuthMessage(config.AuthModeJWT, WireAuthMessage{Type: "auth", APIKey: "a"})
		if err != nil {
			t.Fatalf("err=%v", err)
		}
		if cred != "a" {
			t.Fatalf("cred=%q, want %q", cred, "a")
		}
	})
}

func TestCredentialFromRequest(t *testing.T) {
	t.Run("jwt accepts Authorization header", func(t *testing.T) {
		req, _ := http.NewRequest(http.MethodGet, "http://example.com", nil)
		req.Header.Set("Authorization", "Bearer t")

		cred, err := CredentialFromRequest(config.AuthModeJWT, req)
		if err != nil {
			t.Fatalf("err=%v", err)
		}
		if cred != "t" {
			t.Fatalf("cred=%q, want %q", cred, "t")
		}
	})

	t.Run("api_key accepts X-API-Key header", func(t *testing.T) {
		req, _ := http.NewRequest(http.MethodGet, "http://example.com", nil)
		req.Header.Set("X-API-Key", "k")

		cred, err := CredentialFromRequest(config.AuthModeAPIKey, req)
		if err != nil {
			t.Fatalf("err=%v", err)
		}
		if cred != "k" {
			t.Fatalf("cred=%q, want %q", cred, "k")
		}
	})

	t.Run("api_key accepts Authorization apikey header", func(t *testing.T) {
		req, _ := http.NewRequest(http.MethodGet, "http://example.com", nil)
		req.Header.Set("Authorization", "ApiKey k")

		cred, err := CredentialFromRequest(config.AuthModeAPIKey, req)
		if err != nil {
			t.Fatalf("err=%v", err)
		}
		if cred != "k" {
			t.Fatalf("cred=%q, want %q", cred, "k")
		}
	})

	t.Run("api_key accepts Authorization bearer header", func(t *testing.T) {
		req, _ := http.NewRequest(http.MethodGet, "http://example.com", nil)
		req.Header.Set("Authorization", "Bearer k")

		cred, err := CredentialFromRequest(config.AuthModeAPIKey, req)
		if err != nil {
			t.Fatalf("err=%v", err)
		}
		if cred != "k" {
			t.Fatalf("cred=%q, want %q", cred, "k")
		}
	})

	t.Run("jwt accepts X-API-Key header as alias", func(t *testing.T) {
		req, _ := http.NewRequest(http.MethodGet, "http://example.com", nil)
		req.Header.Set("X-API-Key", "t")

		cred, err := CredentialFromRequest(config.AuthModeJWT, req)
		if err != nil {
			t.Fatalf("err=%v", err)
		}
		if cred != "t" {
			t.Fatalf("cred=%q, want %q", cred, "t")
		}
	})

	t.Run("jwt accepts Authorization apikey header as alias", func(t *testing.T) {
		req, _ := http.NewRequest(http.MethodGet, "http://example.com", nil)
		req.Header.Set("Authorization", "ApiKey t")

		cred, err := CredentialFromRequest(config.AuthModeJWT, req)
		if err != nil {
			t.Fatalf("err=%v", err)
		}
		if cred != "t" {
			t.Fatalf("cred=%q, want %q", cred, "t")
		}
	})
}
