package relay

import "errors"

var (
	ErrTooManySessions = errors.New("too many sessions")
	ErrSessionClosed   = errors.New("session closed")
	ErrTooManyBindings = errors.New("too many udp bindings")
)
