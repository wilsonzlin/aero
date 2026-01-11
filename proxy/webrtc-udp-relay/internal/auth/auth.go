package auth

import (
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"strings"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
)

type Verifier interface {
	Verify(credential string) error
}

type NoopVerifier struct{}

func (NoopVerifier) Verify(string) error { return nil }

func NewVerifier(cfg config.Config) (Verifier, error) {
	switch cfg.AuthMode {
	case config.AuthModeNone:
		return NoopVerifier{}, nil
	case config.AuthModeAPIKey:
		return APIKeyVerifier{Expected: cfg.APIKey}, nil
	case config.AuthModeJWT:
		return NewJWTVerifier(cfg.JWTSecret), nil
	default:
		return nil, fmt.Errorf("unsupported auth mode %q", cfg.AuthMode)
	}
}

var ErrMissingCredentials = errors.New("missing credentials")

// CredentialFromRequest extracts credentials from an HTTP request.
//
// Order of preference:
//   - headers (preferred; avoids leaking into logs/history)
//   - query string (fallback)
func CredentialFromRequest(mode config.AuthMode, r *http.Request) (string, error) {
	if mode == config.AuthModeNone {
		return "", nil
	}
	if r == nil {
		return "", ErrMissingCredentials
	}
	if v := CredentialFromHeaders(mode, r.Header); strings.TrimSpace(v) != "" {
		return strings.TrimSpace(v), nil
	}
	return CredentialFromQuery(mode, r.URL.Query())
}

// CredentialFromHeaders extracts credentials from HTTP headers.
//
// Supported formats:
//   - AUTH_MODE=api_key: X-API-Key: ..., or Authorization: ApiKey ...
//   - AUTH_MODE=jwt:    Authorization: Bearer ...
func CredentialFromHeaders(mode config.AuthMode, h http.Header) string {
	switch mode {
	case config.AuthModeAPIKey:
		if v := strings.TrimSpace(h.Get("X-API-Key")); v != "" {
			return v
		}
		scheme, token := parseAuthHeader(h.Get("Authorization"))
		if scheme == "apikey" && token != "" {
			return token
		}
		return ""
	case config.AuthModeJWT:
		scheme, token := parseAuthHeader(h.Get("Authorization"))
		if scheme == "bearer" && token != "" {
			return token
		}
		return ""
	default:
		return ""
	}
}

func parseAuthHeader(v string) (scheme, token string) {
	v = strings.TrimSpace(v)
	if v == "" {
		return "", ""
	}
	parts := strings.SplitN(v, " ", 2)
	if len(parts) != 2 {
		return "", ""
	}
	return strings.ToLower(strings.TrimSpace(parts[0])), strings.TrimSpace(parts[1])
}

func CredentialFromQuery(mode config.AuthMode, q url.Values) (string, error) {
	switch mode {
	case config.AuthModeNone:
		return "", nil
	case config.AuthModeAPIKey:
		if apiKey := q.Get("apiKey"); apiKey != "" {
			return apiKey, nil
		}
		return "", ErrMissingCredentials
	case config.AuthModeJWT:
		if token := q.Get("token"); token != "" {
			return token, nil
		}
		return "", ErrMissingCredentials
	default:
		return "", fmt.Errorf("unsupported auth mode %q", mode)
	}
}

type WireAuthMessage struct {
	Type   string `json:"type"`
	APIKey string `json:"apiKey,omitempty"`
	Token  string `json:"token,omitempty"`
}

func CredentialFromAuthMessage(mode config.AuthMode, msg WireAuthMessage) (string, error) {
	switch mode {
	case config.AuthModeNone:
		return "", nil
	case config.AuthModeAPIKey:
		if msg.APIKey != "" {
			return msg.APIKey, nil
		}
		return "", ErrMissingCredentials
	case config.AuthModeJWT:
		if msg.Token != "" {
			return msg.Token, nil
		}
		return "", ErrMissingCredentials
	default:
		return "", fmt.Errorf("unsupported auth mode %q", mode)
	}
}
