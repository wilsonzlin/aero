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
		// Support common variations for API-key auth so clients don't have to know
		// the server's auth mode a priori.
		if (strings.EqualFold(scheme, "apikey") || strings.EqualFold(scheme, "bearer")) && token != "" {
			return token
		}
		return ""
	case config.AuthModeJWT:
		scheme, token := parseAuthHeader(h.Get("Authorization"))
		if (strings.EqualFold(scheme, "bearer") || strings.EqualFold(scheme, "apikey")) && token != "" {
			return token
		}
		// Forward/compat: accept X-API-Key as a token carrier for mode-agnostic
		// clients.
		if v := strings.TrimSpace(h.Get("X-API-Key")); v != "" {
			return v
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
	sep := strings.IndexByte(v, ' ')
	if sep == -1 {
		return "", ""
	}
	scheme = strings.TrimSpace(v[:sep])
	token = strings.TrimSpace(v[sep+1:])
	if scheme == "" || token == "" {
		return "", ""
	}
	return scheme, token
}

func CredentialFromQuery(mode config.AuthMode, q url.Values) (string, error) {
	switch mode {
	case config.AuthModeNone:
		return "", nil
	case config.AuthModeAPIKey:
		if apiKey := q.Get("apiKey"); apiKey != "" {
			return apiKey, nil
		}
		// Forward/compat: treat token as an alias for apiKey so mode-agnostic
		// clients can use a single parameter name.
		if token := q.Get("token"); token != "" {
			return token, nil
		}
		return "", ErrMissingCredentials
	case config.AuthModeJWT:
		if token := q.Get("token"); token != "" {
			return token, nil
		}
		// Forward/compat: treat apiKey as an alias for token so mode-agnostic
		// clients can use a single parameter name.
		if apiKey := q.Get("apiKey"); apiKey != "" {
			return apiKey, nil
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
		// Forward/compat: allow using the token field in api_key mode for
		// mode-agnostic clients.
		if msg.Token != "" {
			return msg.Token, nil
		}
		return "", ErrMissingCredentials
	case config.AuthModeJWT:
		if msg.Token != "" {
			return msg.Token, nil
		}
		// Forward/compat: allow using the apiKey field in jwt mode for
		// mode-agnostic clients.
		if msg.APIKey != "" {
			return msg.APIKey, nil
		}
		return "", ErrMissingCredentials
	default:
		return "", fmt.Errorf("unsupported auth mode %q", mode)
	}
}
