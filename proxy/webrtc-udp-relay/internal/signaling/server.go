package signaling

import (
	"encoding/json"
	"net/http"
	"sync"

	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/webrtcpeer"
)

// Config wires together the runtime dependencies for the signaling service.
type Config struct {
	// Sessions enforces global session quotas. If nil, sessions are unlimited.
	Sessions *relay.SessionManager

	// WebRTC is the server-side pion API to use for constructing PeerConnections.
	// It is recommended to use webrtcpeer.NewAPI(cfg) so SettingEngine restrictions
	// (port ranges, NAT 1:1 IPs, listen IP filters) apply.
	WebRTC *webrtc.API

	// ICEServers is the list of ICE servers (STUN/TURN) to use when constructing
	// server-side PeerConnections.
	ICEServers []webrtc.ICEServer

	RelayConfig relay.Config
	Policy      *policy.DestinationPolicy
}

// Server implements the v1 HTTP signaling surface.
//
// Endpoints:
//   - POST /offer   : exchange SDP offer/answer (non-trickle ICE)
//   - POST /session : optional session pre-allocation (used by other tasks)
type Server struct {
	// Sessions enforces global session quotas.
	//
	// This field is intentionally exported so legacy tests and callers can use a
	// simple struct literal (e.g. &Server{Sessions: sm}).
	Sessions *relay.SessionManager

	mu             sync.Mutex
	webrtcSessions []*webrtcpeer.Session
	preSessions    []*relay.Session

	// WebRTC is the server-side pion API used to construct PeerConnections.
	WebRTC *webrtc.API
	// ICEServers is the ICE server list for server-side PeerConnections.
	ICEServers []webrtc.ICEServer

	RelayConfig relay.Config
	Policy      *policy.DestinationPolicy
}

func NewServer(cfg Config) *Server {
	return &Server{
		Sessions:    cfg.Sessions,
		WebRTC:      cfg.WebRTC,
		ICEServers:  cfg.ICEServers,
		RelayConfig: cfg.RelayConfig,
		Policy:      cfg.Policy,
	}
}

func (s *Server) RegisterRoutes(mux *http.ServeMux) {
	mux.HandleFunc("POST /offer", s.handleOffer)
	mux.HandleFunc("POST /session", s.handleCreateSession)
}

func (s *Server) Handler() http.Handler {
	mux := http.NewServeMux()
	s.RegisterRoutes(mux)
	return mux
}

// ServeHTTP provides minimal routing for tests and simple deployments.
//
// The production binary typically wires routes through httpserver.Server.Mux()
// using RegisterRoutes.
func (s *Server) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	switch {
	case r.Method == http.MethodPost && r.URL.Path == "/session":
		s.handleCreateSession(w, r)
	case r.Method == http.MethodPost && r.URL.Path == "/offer":
		s.handleOffer(w, r)
	default:
		http.NotFound(w, r)
	}
}

func (s *Server) Close() {
	s.mu.Lock()
	webrtcSessions := s.webrtcSessions
	preSessions := s.preSessions
	s.webrtcSessions = nil
	s.preSessions = nil
	s.mu.Unlock()

	for _, sess := range webrtcSessions {
		_ = sess.Close()
	}
	for _, sess := range preSessions {
		sess.Close()
	}
}

func (s *Server) handleCreateSession(w http.ResponseWriter, r *http.Request) {
	if s.Sessions == nil {
		http.Error(w, "session manager not configured", http.StatusInternalServerError)
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

	// The /session endpoint is currently a simple pre-allocation mechanism; it
	// does not yet have a corresponding "use session" handshake. Track the
	// sessions so they can be cleaned up on shutdown.
	s.mu.Lock()
	s.preSessions = append(s.preSessions, session)
	s.mu.Unlock()

	w.WriteHeader(http.StatusCreated)
	_, _ = w.Write([]byte(session.ID()))
}

func (s *Server) handleOffer(w http.ResponseWriter, r *http.Request) {
	type offerRequest struct {
		Version int                       `json:"version"`
		Offer   webrtc.SessionDescription `json:"offer"`
	}
	type answerResponse struct {
		Version int                       `json:"version"`
		Answer  webrtc.SessionDescription `json:"answer"`
	}

	var req offerRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, "invalid offer", http.StatusBadRequest)
		return
	}
	if req.Version != 1 {
		http.Error(w, "unsupported protocol version", http.StatusBadRequest)
		return
	}
	if s.WebRTC == nil {
		http.Error(w, "webrtc api not configured", http.StatusInternalServerError)
		return
	}

	var relaySession *relay.Session
	if s.Sessions != nil {
		var err error
		relaySession, err = s.Sessions.CreateSession()
		if err == relay.ErrTooManySessions {
			w.WriteHeader(http.StatusServiceUnavailable)
			return
		}
		if err != nil {
			w.WriteHeader(http.StatusInternalServerError)
			return
		}
	}

	cleanupRelaySession := func() {
		if relaySession != nil {
			relaySession.Close()
		}
	}

	sess, err := webrtcpeer.NewSession(s.WebRTC, s.ICEServers, s.RelayConfig, s.Policy, cleanupRelaySession)
	if err != nil {
		cleanupRelaySession()
		http.Error(w, "failed to create session", http.StatusInternalServerError)
		return
	}
	s.mu.Lock()
	s.webrtcSessions = append(s.webrtcSessions, sess)
	s.mu.Unlock()

	pc := sess.PeerConnection()

	if err := pc.SetRemoteDescription(req.Offer); err != nil {
		_ = sess.Close()
		http.Error(w, "failed to set remote description", http.StatusBadRequest)
		return
	}

	answer, err := pc.CreateAnswer(nil)
	if err != nil {
		_ = sess.Close()
		http.Error(w, "failed to create answer", http.StatusInternalServerError)
		return
	}

	gatherComplete := webrtc.GatheringCompletePromise(pc)
	if err := pc.SetLocalDescription(answer); err != nil {
		_ = sess.Close()
		http.Error(w, "failed to set local description", http.StatusInternalServerError)
		return
	}
	<-gatherComplete

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(answerResponse{
		Version: req.Version,
		Answer:  *pc.LocalDescription(),
	})
}
