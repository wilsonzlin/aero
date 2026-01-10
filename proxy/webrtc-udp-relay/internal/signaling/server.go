package signaling

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"sync"
	"time"

	"github.com/pion/webrtc/v4"
	xwebsocket "golang.org/x/net/websocket"

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

	Authorizer Authorizer

	// ICEGatheringTimeout bounds how long the relay waits for candidate gathering
	// on non-trickle HTTP endpoints (e.g. /webrtc/offer).
	ICEGatheringTimeout time.Duration
}

// Server implements the relay's HTTP/WebSocket signaling surface.
//
// Endpoints:
//   - POST /offer          : versioned, non-trickle offer/answer exchange (used by integration tests)
//   - POST /session        : optional session pre-allocation (used by other tasks)
//   - GET  /webrtc/signal  : WebSocket signaling with trickle ICE
//   - POST /webrtc/offer   : HTTP offer -> answer (non-trickle ICE fallback)
type Server struct {
	// Sessions enforces global session quotas.
	//
	// This field is intentionally exported so tests and callers can use a simple
	// struct literal (e.g. &Server{Sessions: sm}).
	Sessions *relay.SessionManager

	// WebRTC is the server-side pion API used to construct PeerConnections.
	WebRTC *webrtc.API

	// ICEServers is the ICE server list for server-side PeerConnections.
	ICEServers []webrtc.ICEServer

	RelayConfig relay.Config
	Policy      *policy.DestinationPolicy

	Authorizer          Authorizer
	ICEGatheringTimeout time.Duration

	mu             sync.Mutex
	webrtcSessions []*webrtcpeer.Session
	preSessions    []*relay.Session
}

func NewServer(cfg Config) *Server {
	return &Server{
		Sessions:            cfg.Sessions,
		WebRTC:              cfg.WebRTC,
		ICEServers:          cfg.ICEServers,
		RelayConfig:         cfg.RelayConfig,
		Policy:              cfg.Policy,
		Authorizer:          cfg.Authorizer,
		ICEGatheringTimeout: cfg.ICEGatheringTimeout,
	}
}

func (s *Server) RegisterRoutes(mux *http.ServeMux) {
	mux.HandleFunc("POST /offer", s.handleOffer)
	mux.HandleFunc("POST /session", s.handleCreateSession)

	mux.HandleFunc("GET /webrtc/signal", s.handleWebSocketSignal)
	mux.HandleFunc("POST /webrtc/offer", s.handleWebRTCOffer)
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
	case r.Method == http.MethodGet && r.URL.Path == "/webrtc/signal":
		s.handleWebSocketSignal(w, r)
	case r.Method == http.MethodPost && r.URL.Path == "/webrtc/offer":
		s.handleWebRTCOffer(w, r)
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

func (s *Server) authorizer() Authorizer {
	if s.Authorizer == nil {
		return AllowAllAuthorizer{}
	}
	return s.Authorizer
}

func (s *Server) iceGatheringTimeout() time.Duration {
	if s.ICEGatheringTimeout <= 0 {
		return 2 * time.Second
	}
	return s.ICEGatheringTimeout
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
	var req OfferRequest
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		http.Error(w, "invalid offer", http.StatusBadRequest)
		return
	}
	if err := req.Validate(); err != nil {
		if errors.Is(err, ErrUnsupportedVersion) {
			http.Error(w, "unsupported protocol version", http.StatusBadRequest)
			return
		}
		http.Error(w, "invalid offer", http.StatusBadRequest)
		return
	}
	if s.WebRTC == nil {
		http.Error(w, "webrtc api not configured", http.StatusInternalServerError)
		return
	}

	if err := s.authorizer().Authorize(r, &ClientHello{Type: MessageTypeOffer}); err != nil {
		http.Error(w, "unauthorized", http.StatusUnauthorized)
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

	if err := pc.SetRemoteDescription(webrtc.SessionDescription{
		Type: webrtc.SDPTypeOffer,
		SDP:  req.Offer.SDP,
	}); err != nil {
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

	local := pc.LocalDescription()
	if local == nil {
		_ = sess.Close()
		http.Error(w, "failed to gather local description", http.StatusInternalServerError)
		return
	}

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(AnswerResponse{
		Version: req.Version,
		Answer: SessionDescription{
			Type: "answer",
			SDP:  local.SDP,
		},
	})
}

func (s *Server) handleWebRTCOffer(w http.ResponseWriter, r *http.Request) {
	if s.WebRTC == nil {
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "webrtc api not configured")
		return
	}

	body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, 2<<20))
	if err != nil {
		writeJSONError(w, http.StatusBadRequest, "bad_message", err.Error())
		return
	}

	offerWire, err := parseHTTPOfferRequest(body)
	if err != nil {
		writeJSONError(w, http.StatusBadRequest, "bad_message", err.Error())
		return
	}
	offer, err := offerWire.ToPion()
	if err != nil {
		writeJSONError(w, http.StatusBadRequest, "bad_message", err.Error())
		return
	}
	if offer.Type != webrtc.SDPTypeOffer {
		writeJSONError(w, http.StatusBadRequest, "bad_message", "sdp.type must be \"offer\"")
		return
	}

	if err := s.authorizer().Authorize(r, &ClientHello{Type: MessageTypeOffer}); err != nil {
		writeJSONError(w, http.StatusUnauthorized, "unauthorized", err.Error())
		return
	}

	sessionID, relaySession, err := s.allocateRelaySession()
	if err != nil {
		if errors.Is(err, relay.ErrTooManySessions) {
			writeJSONError(w, http.StatusServiceUnavailable, "too_many_sessions", "too many sessions")
			return
		}
		writeJSONError(w, http.StatusInternalServerError, "internal_error", err.Error())
		return
	}

	cleanupRelaySession := func() {
		if relaySession != nil {
			relaySession.Close()
		}
	}

	sess, err := webrtcpeer.NewSession(s.WebRTC, s.ICEServers, s.RelayConfig, s.Policy, cleanupRelaySession)
	if err != nil {
		cleanupRelaySession()
		writeJSONError(w, http.StatusInternalServerError, "internal_error", err.Error())
		return
	}
	s.mu.Lock()
	s.webrtcSessions = append(s.webrtcSessions, sess)
	s.mu.Unlock()

	pc := sess.PeerConnection()
	if err := pc.SetRemoteDescription(offer); err != nil {
		_ = sess.Close()
		writeJSONError(w, http.StatusBadRequest, "bad_message", err.Error())
		return
	}

	answer, err := pc.CreateAnswer(nil)
	if err != nil {
		_ = sess.Close()
		writeJSONError(w, http.StatusInternalServerError, "internal_error", err.Error())
		return
	}

	gatherComplete := webrtc.GatheringCompletePromise(pc)
	if err := pc.SetLocalDescription(answer); err != nil {
		_ = sess.Close()
		writeJSONError(w, http.StatusInternalServerError, "internal_error", err.Error())
		return
	}

	waitCtx, cancel := context.WithTimeout(r.Context(), s.iceGatheringTimeout())
	defer cancel()
	select {
	case <-gatherComplete:
	case <-waitCtx.Done():
	}

	local := pc.LocalDescription()
	if local == nil {
		_ = sess.Close()
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "missing local description")
		return
	}

	writeJSON(w, http.StatusOK, httpOfferResponse{
		SessionID: sessionID,
		SDP:       SDPFromPion(*local),
	})
}

func (s *Server) handleWebSocketSignal(w http.ResponseWriter, r *http.Request) {
	if s.WebRTC == nil {
		http.Error(w, "webrtc api not configured", http.StatusInternalServerError)
		return
	}

	wsSrv := &xwebsocket.Server{
		Handler: xwebsocket.Handler(func(conn *xwebsocket.Conn) {
			req := conn.Request()
			authorizer := s.authorizer()

			_, relaySession, err := s.allocateRelaySession()
			if errors.Is(err, relay.ErrTooManySessions) {
				_ = sendWSError(conn, "too_many_sessions", "too many sessions")
				_ = conn.Close()
				return
			}
			if err != nil {
				_ = sendWSError(conn, "internal_error", err.Error())
				_ = conn.Close()
				return
			}

			cleanupRelaySession := func() {
				if relaySession != nil {
					relaySession.Close()
				}
				_ = conn.Close()
			}

			sess, err := webrtcpeer.NewSession(s.WebRTC, s.ICEServers, s.RelayConfig, s.Policy, cleanupRelaySession)
			if err != nil {
				cleanupRelaySession()
				_ = sendWSError(conn, "internal_error", err.Error())
				return
			}
			s.mu.Lock()
			s.webrtcSessions = append(s.webrtcSessions, sess)
			s.mu.Unlock()

			ws := &wsSession{
				conn:       conn,
				req:        req,
				authorizer: authorizer,
				session:    sess,
			}
			ws.installPeerHandlers()
			ws.run()
		}),
	}

	wsSrv.ServeHTTP(w, r)
}

func (s *Server) allocateRelaySession() (string, *relay.Session, error) {
	if s.Sessions == nil {
		id, err := newSessionID()
		if err != nil {
			return "", nil, err
		}
		return id, nil, nil
	}

	relaySession, err := s.Sessions.CreateSession()
	if err != nil {
		return "", nil, err
	}
	return relaySession.ID(), relaySession, nil
}

func newSessionID() (string, error) {
	var buf [16]byte
	if _, err := rand.Read(buf[:]); err != nil {
		return "", fmt.Errorf("generate session id: %w", err)
	}
	return hex.EncodeToString(buf[:]), nil
}

func sendWSError(conn *xwebsocket.Conn, code, message string) error {
	data, err := json.Marshal(SignalMessage{
		Type:    MessageTypeError,
		Code:    code,
		Message: message,
	})
	if err != nil {
		return err
	}
	return xwebsocket.Message.Send(conn, string(data))
}

type httpOfferRequest struct {
	SDP SDP `json:"sdp"`
}

type httpOfferResponse struct {
	SessionID string `json:"sessionId"`
	SDP       SDP    `json:"sdp"`
}

type httpErrorResponse struct {
	Code    string `json:"code"`
	Message string `json:"message"`
}

func writeJSON(w http.ResponseWriter, status int, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

func writeJSONError(w http.ResponseWriter, status int, code, message string) {
	writeJSON(w, status, httpErrorResponse{Code: code, Message: message})
}

func parseHTTPOfferRequest(body []byte) (SDP, error) {
	var req httpOfferRequest
	reqErr := decodeStrictJSON(body, &req)
	if reqErr == nil {
		return req.SDP, nil
	}

	var sdp SDP
	sdpErr := decodeStrictJSON(body, &sdp)
	if sdpErr == nil {
		return sdp, nil
	}

	return SDP{}, fmt.Errorf("invalid offer request body (expected {\"sdp\":{...}} or a raw SessionDescription): %w", errors.Join(reqErr, sdpErr))
}

func decodeStrictJSON(data []byte, v any) error {
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.DisallowUnknownFields()
	if err := dec.Decode(v); err != nil {
		return err
	}
	return expectEOF(dec)
}

func expectEOF(dec *json.Decoder) error {
	if err := dec.Decode(&struct{}{}); err != io.EOF {
		return fmt.Errorf("unexpected trailing data")
	}
	return nil
}

type wsSession struct {
	conn *xwebsocket.Conn
	req  *http.Request

	session    *webrtcpeer.Session
	authorizer Authorizer

	writeMu sync.Mutex

	answerMu   sync.Mutex
	answerSent bool
	candBuf    []Candidate

	closeOnce sync.Once
}

func (wss *wsSession) installPeerHandlers() {
	pc := wss.session.PeerConnection()

	pc.OnICECandidate(func(c *webrtc.ICECandidate) {
		if c == nil {
			return
		}

		cand := CandidateFromPion(c.ToJSON())

		wss.answerMu.Lock()
		if !wss.answerSent {
			wss.candBuf = append(wss.candBuf, cand)
			wss.answerMu.Unlock()
			return
		}
		wss.answerMu.Unlock()

		_ = wss.send(SignalMessage{
			Type:      MessageTypeCandidate,
			Candidate: &cand,
		})
	})
}

func (wss *wsSession) run() {
	defer wss.Close()

	var haveOffer bool
	var authorized bool

	for {
		var raw string
		err := xwebsocket.Message.Receive(wss.conn, &raw)
		if err != nil {
			return
		}

		msg, err := ParseSignalMessage([]byte(raw))
		if err != nil {
			_ = wss.sendError("bad_message", err.Error())
			return
		}

		if !authorized {
			if err := wss.authorizer.Authorize(wss.req, &ClientHello{Type: msg.Type}); err != nil {
				_ = wss.sendError("unauthorized", err.Error())
				return
			}
			authorized = true
		}

		switch msg.Type {
		case MessageTypeOffer:
			if haveOffer {
				_ = wss.sendError("unexpected_message", "offer already received")
				return
			}
			haveOffer = true
			if err := wss.handleOffer(*msg.SDP); err != nil {
				_ = wss.sendError("internal_error", err.Error())
				return
			}
		case MessageTypeCandidate:
			if !haveOffer {
				_ = wss.sendError("unexpected_message", "candidate received before offer")
				return
			}
			if err := wss.handleRemoteCandidate(*msg.Candidate); err != nil {
				_ = wss.sendError("internal_error", err.Error())
				return
			}
		case MessageTypeClose:
			return
		default:
			_ = wss.sendError("bad_message", fmt.Sprintf("unexpected message type %q", msg.Type))
			return
		}
	}
}

func (wss *wsSession) handleOffer(offerWire SDP) error {
	offer, err := offerWire.ToPion()
	if err != nil {
		return err
	}
	if offer.Type != webrtc.SDPTypeOffer {
		return fmt.Errorf("sdp.type must be \"offer\"")
	}

	pc := wss.session.PeerConnection()

	if err := pc.SetRemoteDescription(offer); err != nil {
		return err
	}

	answer, err := pc.CreateAnswer(nil)
	if err != nil {
		return err
	}
	if err := pc.SetLocalDescription(answer); err != nil {
		return err
	}

	local := pc.LocalDescription()
	if local == nil {
		return errors.New("missing local description after SetLocalDescription")
	}

	if err := wss.send(SignalMessage{
		Type: MessageTypeAnswer,
		SDP:  ptr(SDPFromPion(*local)),
	}); err != nil {
		return err
	}

	var buffered []Candidate
	wss.answerMu.Lock()
	wss.answerSent = true
	buffered = append(buffered, wss.candBuf...)
	wss.candBuf = nil
	wss.answerMu.Unlock()

	for i := range buffered {
		cand := buffered[i]
		_ = wss.send(SignalMessage{
			Type:      MessageTypeCandidate,
			Candidate: &cand,
		})
	}

	return nil
}

func (wss *wsSession) handleRemoteCandidate(candWire Candidate) error {
	if candWire.Candidate == "" {
		return nil
	}
	return wss.session.PeerConnection().AddICECandidate(candWire.ToPion())
}

func (wss *wsSession) send(msg SignalMessage) error {
	data, err := json.Marshal(msg)
	if err != nil {
		return err
	}

	wss.writeMu.Lock()
	defer wss.writeMu.Unlock()
	return xwebsocket.Message.Send(wss.conn, string(data))
}

func (wss *wsSession) sendError(code, message string) error {
	_ = wss.send(SignalMessage{
		Type:    MessageTypeError,
		Code:    code,
		Message: message,
	})
	return nil
}

func (wss *wsSession) Close() {
	wss.closeOnce.Do(func() {
		if wss.session != nil {
			_ = wss.session.Close()
		}
		_ = wss.conn.Close()
	})
}

func ptr[T any](v T) *T { return &v }

