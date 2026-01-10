package signaling

import "net/http"

type ClientHello struct {
	// Type is the first message type observed for the session (e.g. "offer").
	Type MessageType `json:"type"`
}

type Authorizer interface {
	Authorize(r *http.Request, firstMsg *ClientHello) error
}

type AllowAllAuthorizer struct{}

func (AllowAllAuthorizer) Authorize(r *http.Request, firstMsg *ClientHello) error {
	return nil
}

