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

func (a AuthAuthorizer) Authorize(r *http.Request, firstMsg *ClientHello) (AuthResult, error) {
	if a.mode == config.AuthModeNone {
		return AuthResult{}, nil
	}
	if a.verifier == nil {
		return AuthResult{}, errors.New("auth verifier not configured")
	}

	cred, err := credentialFromHelloAndRequest(a.mode, firstMsg, r)
	if err != nil {
		return AuthResult{}, err
	}

	res := AuthResult{Credential: cred}

	// For AUTH_MODE=jwt we use the JWT session id (`sid`) as a stable quota key so
	// clients cannot bypass per-session rate limits by opening many parallel
	// connections with the same token.
	if a.mode == config.AuthModeJWT {
		cv, ok := a.verifier.(auth.ClaimsVerifier)
		if !ok {
			return AuthResult{}, errors.New("jwt verifier does not support claims extraction")
		}
		claims, err := cv.VerifyAndExtractClaims(cred)
		if err != nil {
			return AuthResult{}, err
		}
		res.SessionKey = claims.SID
		return res, nil
	}

	if err := a.verifier.Verify(cred); err != nil {
		return AuthResult{}, err
	}
	return res, nil
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
