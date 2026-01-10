package signaling

import (
	"net/http"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
)

// Server is a minimal signaling surface that enforces MAX_SESSIONS.
//
// The full WebRTC signaling protocol is out of scope for this task; the relay
// just needs an integration point to reject new sessions when the global quota
// is exceeded.
type Server struct {
	Sessions *relay.SessionManager
}

func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodPost || r.URL.Path != "/session" {
		http.NotFound(w, r)
		return
	}

	session, err := s.Sessions.CreateSession()
	if err == relay.ErrTooManySessions {
		w.WriteHeader(http.StatusServiceUnavailable)
		return
	}
	if err != nil {
		w.WriteHeader(http.StatusInternalServerError)
		return
	}

	w.WriteHeader(http.StatusCreated)
	_, _ = w.Write([]byte(session.ID()))
}

