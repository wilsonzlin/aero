package auth

import (
	"errors"
	"fmt"
	"net/url"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
)

type Verifier interface {
	Verify(credential string) error
}

func NewVerifier(cfg config.Config) (Verifier, error) {
	switch cfg.AuthMode {
	case config.AuthModeAPIKey:
		return APIKeyVerifier{Expected: cfg.APIKey}, nil
	case config.AuthModeJWT:
		return NewJWTVerifier(cfg.JWTSecret), nil
	default:
		return nil, fmt.Errorf("unsupported auth mode %q", cfg.AuthMode)
	}
}

var ErrMissingCredentials = errors.New("missing credentials")

func CredentialFromQuery(mode config.AuthMode, q url.Values) (string, error) {
	switch mode {
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
