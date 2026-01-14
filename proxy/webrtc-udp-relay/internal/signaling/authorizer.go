package signaling

import "net/http"

type clientHello struct {
	// Type is the first message type observed for the session (e.g. "offer").
	Type messageType `json:"type"`

	// Credential carries the apiKey/token from a WebSocket `{type:"auth"}` message.
	// For HTTP requests, credentials are read from headers/query parameters instead.
	Credential string `json:"-"`
}

// authResult carries metadata about an authorized signaling request/session.
//
// Today this is used to plumb the authenticated credential (JWT/API key) into
// the WebRTC session so downstream components (e.g. the L2 backend bridge) can
// reuse it when dialing other services.
type authResult struct {
	Credential string

	// SessionKey is an optional, stable identifier for enforcing per-session
	// quotas/rate limits across multiple connections.
	//
	// When AUTH_MODE=jwt, this is the JWT `sid` claim. For other auth modes it is
	// empty.
	SessionKey string
}

type authorizer interface {
	Authorize(r *http.Request, firstMsg *clientHello) (authResult, error)
}

type allowAllAuthorizer struct{}

func (allowAllAuthorizer) Authorize(r *http.Request, firstMsg *clientHello) (authResult, error) {
	return authResult{}, nil
}
