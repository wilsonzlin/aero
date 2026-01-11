package signaling

import (
	"errors"
	"fmt"
	"net/http"
	"strings"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/auth"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
)

// AuthAuthorizer enforces AUTH_MODE=none|api_key|jwt for signaling endpoints.
//
// Credential sources:
//   - HTTP: headers (preferred) and query string (fallback).
//   - WebSocket: first message `{type:"auth", apiKey:"..."}` / `{type:"auth", token:"..."}`
//     (preferred) and query string (fallback).
type AuthAuthorizer struct {
	mode     config.AuthMode
	verifier auth.Verifier
}

func NewAuthAuthorizer(cfg config.Config) (AuthAuthorizer, error) {
	v, err := auth.NewVerifier(cfg)
	if err != nil {
		return AuthAuthorizer{}, err
	}
	return AuthAuthorizer{
		mode:     cfg.AuthMode,
		verifier: v,
	}, nil
}

func (a AuthAuthorizer) Authorize(r *http.Request, firstMsg *ClientHello) error {
	if a.mode == config.AuthModeNone {
		return nil
	}
	if a.verifier == nil {
		return errors.New("auth verifier not configured")
	}

	cred, err := credentialFromHelloAndRequest(a.mode, firstMsg, r)
	if err != nil {
		return err
	}
	if err := a.verifier.Verify(cred); err != nil {
		return err
	}
	return nil
}

func credentialFromHelloAndRequest(mode config.AuthMode, hello *ClientHello, r *http.Request) (string, error) {
	if hello != nil {
		if v := strings.TrimSpace(hello.Credential); v != "" {
			return v, nil
		}
	}
	return auth.CredentialFromRequest(mode, r)
}

// IsAuthMissing reports whether err represents missing credentials (as opposed to
// invalid credentials).
func IsAuthMissing(err error) bool {
	return errors.Is(err, auth.ErrMissingCredentials)
}

// IsUnauthorized reports whether err should be treated as an authentication failure.
func IsUnauthorized(err error) bool {
	if err == nil {
		return false
	}
	return errors.Is(err, auth.ErrMissingCredentials) || errors.Is(err, auth.ErrInvalidCredentials) || errors.Is(err, auth.ErrUnsupportedJWT)
}

func unauthorizedMessage(err error) string {
	if err == nil {
		return "unauthorized"
	}
	// Avoid leaking server configuration details (e.g. "invalid auth mode").
	if IsUnauthorized(err) {
		return "unauthorized"
	}
	return fmt.Sprintf("authorization failed: %v", err)
}
