package signaling

import "net/http"

type ClientHello struct {
	// Type is the first message type observed for the session (e.g. "offer").
	Type MessageType `json:"type"`

	// Credential carries the apiKey/token from a WebSocket `{type:"auth"}` message.
	// For HTTP requests, credentials are read from headers/query parameters instead.
	Credential string `json:"-"`
}

type Authorizer interface {
	Authorize(r *http.Request, firstMsg *ClientHello) error
}

type AllowAllAuthorizer struct{}

func (AllowAllAuthorizer) Authorize(r *http.Request, firstMsg *ClientHello) error {
	return nil
}
