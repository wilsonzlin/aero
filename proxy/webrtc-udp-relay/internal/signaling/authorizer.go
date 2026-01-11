package signaling

import "net/http"

type ClientHello struct {
	// Type is the first message type observed for the session (e.g. "offer").
	Type MessageType `json:"type"`

	// Credential carries the apiKey/token from a WebSocket `{type:"auth"}` message.
	// For HTTP requests, credentials are read from headers/query parameters instead.
	Credential string `json:"-"`
}

// AuthResult carries metadata about an authorized signaling request/session.
//
// Today this is used to plumb the authenticated credential (JWT/API key) into
// the WebRTC session so downstream components (e.g. the L2 backend bridge) can
// reuse it when dialing other services.
type AuthResult struct {
	Credential string
}

type Authorizer interface {
	Authorize(r *http.Request, firstMsg *ClientHello) (AuthResult, error)
}

type AllowAllAuthorizer struct{}

func (AllowAllAuthorizer) Authorize(r *http.Request, firstMsg *ClientHello) (AuthResult, error) {
	return AuthResult{}, nil
}
