package signaling

import (
	"errors"
	"fmt"
	"net/http"
	"strings"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/auth"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
)

// authAuthorizer enforces AUTH_MODE=none|api_key|jwt for signaling endpoints.
//
// Credential sources:
//   - HTTP: headers (preferred) and query string (fallback).
//   - WebSocket: first message `{type:"auth", apiKey:"..."}` / `{type:"auth", token:"..."}`
//     (preferred) and query string (fallback).
type authAuthorizer struct {
	mode     config.AuthMode
	verifier auth.Verifier
}

func NewAuthAuthorizer(cfg config.Config) (authorizer, error) {
	v, err := auth.NewVerifier(cfg)
	if err != nil {
		return nil, err
	}
	return authAuthorizer{
		mode:     cfg.AuthMode,
		verifier: v,
	}, nil
}

func (a authAuthorizer) Authorize(r *http.Request, firstMsg *clientHello) (authResult, error) {
	if a.mode == config.AuthModeNone {
		return authResult{}, nil
	}
	if a.verifier == nil {
		return authResult{}, errors.New("auth verifier not configured")
	}

	cred, err := credentialFromHelloAndRequest(a.mode, firstMsg, r)
	if err != nil {
		return authResult{}, err
	}

	res := authResult{Credential: cred}

	// For AUTH_MODE=jwt we use the JWT session id (`sid`) as a stable quota key so
	// clients cannot bypass per-session rate limits by opening many parallel
	// connections with the same token.
	if a.mode == config.AuthModeJWT {
		cv, ok := a.verifier.(auth.ClaimsVerifier)
		if !ok {
			return authResult{}, errors.New("jwt verifier does not support claims extraction")
		}
		claims, err := cv.VerifyAndExtractClaims(cred)
		if err != nil {
			return authResult{}, err
		}
		res.SessionKey = claims.SID
		return res, nil
	}

	if err := a.verifier.Verify(cred); err != nil {
		return authResult{}, err
	}
	return res, nil
}

func credentialFromHelloAndRequest(mode config.AuthMode, hello *clientHello, r *http.Request) (string, error) {
	if hello != nil {
		if v := strings.TrimSpace(hello.Credential); v != "" {
			return v, nil
		}
	}
	return auth.CredentialFromRequest(mode, r)
}

// isAuthMissing reports whether err represents missing credentials (as opposed to
// invalid credentials).
func isAuthMissing(err error) bool {
	return errors.Is(err, auth.ErrMissingCredentials)
}

// isUnauthorized reports whether err should be treated as an authentication failure.
func isUnauthorized(err error) bool {
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
	if isUnauthorized(err) {
		return "unauthorized"
	}
	return fmt.Sprintf("authorization failed: %v", err)
}
