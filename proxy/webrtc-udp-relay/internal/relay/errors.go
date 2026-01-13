package relay

import "errors"

var (
	ErrTooManySessions = errors.New("too many sessions")
	// ErrSessionAlreadyActive is returned when a caller attempts to create a new
	// relay session for a stable quota key that already has an active session.
	//
	// This is primarily used for AUTH_MODE=jwt, where the quota key is the JWT
	// `sid` claim.
	ErrSessionAlreadyActive = errors.New("session already active")
	ErrSessionClosed        = errors.New("session closed")
	ErrTooManyBindings      = errors.New("too many udp bindings")
)
