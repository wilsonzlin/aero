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
	"net"
	"net/http"
	"strings"
	"sync"
	"time"

	"github.com/gorilla/websocket"
	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/httpserver"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/origin"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/ratelimit"
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

	// AllowedOrigins configures the same origin allow-list used by the outer
	// httpserver. When empty, the signaling WebSocket enforces same-host (host:port)
	// semantics for browser clients.
	AllowedOrigins []string

	Authorizer Authorizer

	// ICEGatheringTimeout bounds how long the relay waits for candidate gathering
	// on non-trickle HTTP endpoints (e.g. /webrtc/offer).
	ICEGatheringTimeout time.Duration

	// WebSocket auth timeout for AUTH_MODE!=none.
	SignalingAuthTimeout time.Duration

	// WebSocket inbound signaling hardening.
	MaxSignalingMessageBytes      int64
	MaxSignalingMessagesPerSecond int
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

	AllowedOrigins []string

	Authorizer          Authorizer
	ICEGatheringTimeout time.Duration

	SignalingAuthTimeout time.Duration

	MaxSignalingMessageBytes      int64
	MaxSignalingMessagesPerSecond int

	mu             sync.Mutex
	webrtcSessions map[*webrtcpeer.Session]struct{}
	preSessions    []*relay.Session
}

func NewServer(cfg Config) *Server {
	return &Server{
		Sessions:             cfg.Sessions,
		WebRTC:               cfg.WebRTC,
		ICEServers:           cfg.ICEServers,
		RelayConfig:          cfg.RelayConfig,
		Policy:               cfg.Policy,
		AllowedOrigins:       cfg.AllowedOrigins,
		Authorizer:           cfg.Authorizer,
		ICEGatheringTimeout:  cfg.ICEGatheringTimeout,
		SignalingAuthTimeout: cfg.SignalingAuthTimeout,

		MaxSignalingMessageBytes:      cfg.MaxSignalingMessageBytes,
		MaxSignalingMessagesPerSecond: cfg.MaxSignalingMessagesPerSecond,

		webrtcSessions: make(map[*webrtcpeer.Session]struct{}),
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
	webrtcSessions := make([]*webrtcpeer.Session, 0, len(s.webrtcSessions))
	for sess := range s.webrtcSessions {
		webrtcSessions = append(webrtcSessions, sess)
	}
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

func (s *Server) trackWebRTCSession(sess *webrtcpeer.Session) {
	if sess == nil {
		return
	}
	s.mu.Lock()
	if s.webrtcSessions == nil {
		s.webrtcSessions = make(map[*webrtcpeer.Session]struct{})
	}
	s.webrtcSessions[sess] = struct{}{}
	s.mu.Unlock()
}

func (s *Server) untrackWebRTCSession(sess *webrtcpeer.Session) {
	if sess == nil {
		return
	}
	s.mu.Lock()
	if s.webrtcSessions != nil {
		delete(s.webrtcSessions, sess)
	}
	s.mu.Unlock()
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

func (s *Server) signalingAuthTimeout() time.Duration {
	if s.SignalingAuthTimeout <= 0 {
		return 2 * time.Second
	}
	return s.SignalingAuthTimeout
}

func (s *Server) maxSignalingMessageBytes() int64 {
	if s.MaxSignalingMessageBytes <= 0 {
		return 64 * 1024
	}
	return s.MaxSignalingMessageBytes
}

func (s *Server) maxSignalingMessagesPerSecond() int {
	if s.MaxSignalingMessagesPerSecond <= 0 {
		return 50
	}
	return s.MaxSignalingMessagesPerSecond
}

func (s *Server) incMetric(name string) {
	if s.Sessions == nil {
		return
	}
	m := s.Sessions.Metrics()
	if m == nil {
		return
	}
	m.Inc(name)
}

func (s *Server) handleCreateSession(w http.ResponseWriter, r *http.Request) {
	if !s.checkOrigin(r) {
		writeJSONError(w, http.StatusForbidden, "forbidden", "forbidden")
		return
	}

	if s.Sessions == nil {
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "session manager not configured")
		return
	}

	if _, err := s.authorizer().Authorize(r, nil); err != nil {
		if IsUnauthorized(err) {
			s.incMetric(metrics.AuthFailure)
			writeJSONError(w, http.StatusUnauthorized, "unauthorized", "unauthorized")
			return
		}
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "internal error")
		return
	}

	session, err := s.Sessions.CreateSession()
	if err == relay.ErrTooManySessions {
		writeJSONError(w, http.StatusServiceUnavailable, "too_many_sessions", "too many sessions")
		return
	}
	if err != nil {
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "internal error")
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
	if !s.checkOrigin(r) {
		writeJSONError(w, http.StatusForbidden, "forbidden", "forbidden")
		return
	}

	var req OfferRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 2<<20)).Decode(&req); err != nil {
		writeJSONError(w, http.StatusBadRequest, "bad_message", "invalid offer")
		return
	}
	if err := req.Validate(); err != nil {
		if errors.Is(err, ErrUnsupportedVersion) {
			writeJSONError(w, http.StatusBadRequest, "bad_message", "unsupported protocol version")
			return
		}
		writeJSONError(w, http.StatusBadRequest, "bad_message", "invalid offer")
		return
	}
	if s.WebRTC == nil {
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "webrtc api not configured")
		return
	}

	authRes, err := s.authorizer().Authorize(r, &ClientHello{Type: MessageTypeOffer})
	if err != nil {
		if IsUnauthorized(err) {
			s.incMetric(metrics.AuthFailure)
			writeJSONError(w, http.StatusUnauthorized, "unauthorized", "unauthorized")
			return
		}
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "internal error")
		return
	}

	clientOrigin := httpserver.NormalizedOriginFromRequest(r)
	clientCredential := authRes.Credential

	var relaySession *relay.Session
	if s.Sessions != nil {
		var err error
		relaySession, err = s.Sessions.CreateSession()
		if err == relay.ErrTooManySessions {
			writeJSONError(w, http.StatusServiceUnavailable, "too_many_sessions", "too many sessions")
			return
		}
		if err != nil {
			writeJSONError(w, http.StatusInternalServerError, "internal_error", "internal error")
			return
		}
	}

	cleanupRelaySession := func() {
		if relaySession != nil {
			relaySession.Close()
		}
	}

	var sess *webrtcpeer.Session
	cleanup := func() {
		cleanupRelaySession()
		if sess != nil {
			s.untrackWebRTCSession(sess)
		}
	}

	sess, err = webrtcpeer.NewSession(s.WebRTC, s.ICEServers, s.RelayConfig, s.Policy, relaySession, clientOrigin, clientCredential, cleanup)
	if err != nil {
		cleanupRelaySession()
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "failed to create session")
		return
	}
	s.trackWebRTCSession(sess)

	pc := sess.PeerConnection()

	if err := pc.SetRemoteDescription(webrtc.SessionDescription{
		Type: webrtc.SDPTypeOffer,
		SDP:  req.Offer.SDP,
	}); err != nil {
		_ = sess.Close()
		writeJSONError(w, http.StatusBadRequest, "bad_message", "failed to set remote description")
		return
	}

	answer, err := pc.CreateAnswer(nil)
	if err != nil {
		_ = sess.Close()
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "failed to create answer")
		return
	}

	gatherComplete := webrtc.GatheringCompletePromise(pc)
	if err := pc.SetLocalDescription(answer); err != nil {
		_ = sess.Close()
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "failed to set local description")
		return
	}
	select {
	case <-gatherComplete:
	case <-r.Context().Done():
		_ = sess.Close()
		return
	}

	local := pc.LocalDescription()
	if local == nil {
		_ = sess.Close()
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "failed to gather local description")
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
	if !s.checkOrigin(r) {
		writeJSONError(w, http.StatusForbidden, "forbidden", "forbidden")
		return
	}

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

	authRes, err := s.authorizer().Authorize(r, &ClientHello{Type: MessageTypeOffer})
	if err != nil {
		if IsUnauthorized(err) {
			s.incMetric(metrics.AuthFailure)
			writeJSONError(w, http.StatusUnauthorized, "unauthorized", "unauthorized")
			return
		}
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "internal error")
		return
	}

	clientOrigin := httpserver.NormalizedOriginFromRequest(r)
	clientCredential := authRes.Credential

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

	var sess *webrtcpeer.Session
	cleanup := func() {
		cleanupRelaySession()
		if sess != nil {
			s.untrackWebRTCSession(sess)
		}
	}

	sess, err = webrtcpeer.NewSession(s.WebRTC, s.ICEServers, s.RelayConfig, s.Policy, relaySession, clientOrigin, clientCredential, cleanup)
	if err != nil {
		cleanupRelaySession()
		writeJSONError(w, http.StatusInternalServerError, "internal_error", err.Error())
		return
	}
	s.trackWebRTCSession(sess)

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
	if !s.checkOrigin(r) {
		writeJSONError(w, http.StatusForbidden, "forbidden", "forbidden")
		return
	}

	if s.WebRTC == nil {
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "webrtc api not configured")
		return
	}

	upgrader := websocket.Upgrader{
		// Origin checks are enforced by the outer httpserver origin middleware in
		// production, but we also enforce them here as defense-in-depth and to
		// protect standalone usage.
		CheckOrigin: s.checkOrigin,
	}

	conn, err := upgrader.Upgrade(w, r, nil)
	if err != nil {
		return
	}

	ws := &wsSession{
		srv:        s,
		conn:       conn,
		req:        r,
		authorizer: s.authorizer(),

		authTimeout: s.signalingAuthTimeout(),
		limiter: ratelimit.NewTokenBucket(
			ratelimit.RealClock{},
			int64(s.maxSignalingMessagesPerSecond()),
			int64(s.maxSignalingMessagesPerSecond()),
		),

		maxMessageBytes: s.maxSignalingMessageBytes(),
	}
	ws.run()
}

func (s *Server) checkOrigin(r *http.Request) bool {
	originHeader := strings.TrimSpace(r.Header.Get("Origin"))
	if originHeader == "" {
		return true
	}

	normalizedOrigin, originHost, ok := origin.NormalizeHeader(originHeader)
	if !ok {
		return false
	}
	return origin.IsAllowed(normalizedOrigin, originHost, r.Host, s.AllowedOrigins)
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
	srv  *Server
	conn *websocket.Conn
	req  *http.Request

	authorizer Authorizer

	authTimeout     time.Duration
	maxMessageBytes int64
	limiter         *ratelimit.TokenBucket

	session      *webrtcpeer.Session
	relaySession *relay.Session
	origin       string
	credential   string

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

const wsWriteWait = 1 * time.Second

func (wss *wsSession) run() {
	defer wss.Close()

	wss.origin = httpserver.NormalizedOriginFromRequest(wss.req)

	wss.conn.SetReadLimit(wss.maxMessageBytes)

	var haveOffer bool

	authorized := false
	if authRes, err := wss.authorizer.Authorize(wss.req, nil); err != nil {
		if IsAuthMissing(err) {
			_ = wss.conn.SetReadDeadline(time.Now().Add(wss.authTimeout))
		} else if IsUnauthorized(err) {
			wss.srv.incMetric(metrics.AuthFailure)
			_ = wss.fail("unauthorized", unauthorizedMessage(err), websocket.ClosePolicyViolation, "unauthorized")
			return
		} else {
			_ = wss.fail("internal_error", "internal error", websocket.CloseInternalServerErr, "internal error")
			return
		}
	} else {
		authorized = true
		wss.credential = authRes.Credential
	}

	for {
		msgType, data, err := wss.conn.ReadMessage()
		if err != nil {
			if !authorized && isTimeout(err) {
				wss.srv.incMetric(metrics.AuthFailure)
				wss.closeWith(websocket.ClosePolicyViolation, "authentication timeout")
			}
			return
		}
		// Apply the per-session signaling message rate limit *after* reading the
		// message so we consume any bytes already in the TCP receive buffer.
		//
		// If we close before reading, the OS may send an abortive close (RST) due
		// to unread data, preventing clients from reliably observing the WebSocket
		// close code/reason.
		if wss.limiter != nil && !wss.limiter.Allow(1) {
			wss.srv.incMetric(metrics.DropReasonRateLimited)
			_ = wss.fail("rate_limited", "rate limit exceeded", websocket.ClosePolicyViolation, "rate limit exceeded")
			return
		}
		if msgType != websocket.TextMessage {
			_ = wss.fail("bad_message", "expected text message", websocket.CloseUnsupportedData, "expected text message")
			return
		}

		msg, err := ParseSignalMessage(data)
		if err != nil {
			_ = wss.fail("bad_message", err.Error(), websocket.ClosePolicyViolation, "bad message")
			return
		}

		if !authorized {
			if msg.Type != MessageTypeAuth {
				wss.srv.incMetric(metrics.AuthFailure)
				_ = wss.fail("unauthorized", "authentication required", websocket.ClosePolicyViolation, "authentication required")
				return
			}

			cred := msg.APIKey
			if cred == "" {
				cred = msg.Token
			}
			authRes, err := wss.authorizer.Authorize(wss.req, &ClientHello{Type: MessageTypeAuth, Credential: cred})
			if err != nil {
				if IsUnauthorized(err) {
					wss.srv.incMetric(metrics.AuthFailure)
					_ = wss.fail("unauthorized", unauthorizedMessage(err), websocket.ClosePolicyViolation, "unauthorized")
				} else {
					_ = wss.fail("internal_error", "internal error", websocket.CloseInternalServerErr, "internal error")
				}
				return
			}

			authorized = true
			wss.credential = authRes.Credential
			_ = wss.conn.SetReadDeadline(time.Time{})
			continue
		}

		switch msg.Type {
		case MessageTypeAuth:
			// Be tolerant: clients may send an auth message even when already
			// authenticated (e.g. query-string fallback or AUTH_MODE=none).
			if !haveOffer {
				continue
			}
			_ = wss.fail("unexpected_message", "auth received after offer", websocket.ClosePolicyViolation, "unexpected message")
			return
		case MessageTypeOffer:
			if haveOffer {
				_ = wss.fail("unexpected_message", "offer already received", websocket.ClosePolicyViolation, "unexpected message")
				return
			}
			haveOffer = true
			if err := wss.handleOffer(*msg.SDP); err != nil {
				var protoErr *wsProtocolError
				if errors.As(err, &protoErr) {
					_ = wss.fail(protoErr.Code, protoErr.Message, websocket.ClosePolicyViolation, protoErr.Code)
					return
				}
				_ = wss.fail("internal_error", err.Error(), websocket.CloseInternalServerErr, "internal error")
				return
			}
		case MessageTypeCandidate:
			if !haveOffer {
				_ = wss.fail("unexpected_message", "candidate received before offer", websocket.ClosePolicyViolation, "unexpected message")
				return
			}
			if err := wss.handleRemoteCandidate(*msg.Candidate); err != nil {
				_ = wss.fail("bad_message", err.Error(), websocket.ClosePolicyViolation, "bad message")
				return
			}
		case MessageTypeClose:
			return
		default:
			_ = wss.fail("bad_message", fmt.Sprintf("unexpected message type %q", msg.Type), websocket.ClosePolicyViolation, "bad message")
			return
		}
	}
}

type wsProtocolError struct {
	Code    string
	Message string
}

func (e *wsProtocolError) Error() string { return e.Code + ": " + e.Message }

func (wss *wsSession) handleOffer(offerWire SDP) error {
	if wss.srv == nil {
		return &wsProtocolError{Code: "internal_error", Message: "server not configured"}
	}

	offer, err := offerWire.ToPion()
	if err != nil {
		return &wsProtocolError{Code: "bad_message", Message: err.Error()}
	}
	if offer.Type != webrtc.SDPTypeOffer {
		return &wsProtocolError{Code: "bad_message", Message: "sdp.type must be \"offer\""}
	}

	_, relaySession, err := wss.srv.allocateRelaySession()
	if errors.Is(err, relay.ErrTooManySessions) {
		return &wsProtocolError{Code: "too_many_sessions", Message: "too many sessions"}
	}
	if err != nil {
		return err
	}
	wss.relaySession = relaySession

	var sess *webrtcpeer.Session
	cleanupRelaySession := func() {
		if relaySession != nil {
			relaySession.Close()
		}
		_ = wss.conn.Close()
	}
	cleanup := func() {
		cleanupRelaySession()
		if sess != nil {
			wss.srv.untrackWebRTCSession(sess)
		}
	}

	sess, err = webrtcpeer.NewSession(wss.srv.WebRTC, wss.srv.ICEServers, wss.srv.RelayConfig, wss.srv.Policy, relaySession, wss.origin, wss.credential, cleanup)
	if err != nil {
		cleanupRelaySession()
		return err
	}
	wss.srv.trackWebRTCSession(sess)

	wss.session = sess
	wss.installPeerHandlers()

	pc := sess.PeerConnection()

	if err := pc.SetRemoteDescription(offer); err != nil {
		_ = sess.Close()
		return &wsProtocolError{Code: "bad_message", Message: err.Error()}
	}

	answer, err := pc.CreateAnswer(nil)
	if err != nil {
		_ = sess.Close()
		return err
	}
	if err := pc.SetLocalDescription(answer); err != nil {
		_ = sess.Close()
		return err
	}

	local := pc.LocalDescription()
	if local == nil {
		_ = sess.Close()
		return errors.New("missing local description after SetLocalDescription")
	}

	if err := wss.send(SignalMessage{
		Type: MessageTypeAnswer,
		SDP:  ptr(SDPFromPion(*local)),
	}); err != nil {
		_ = sess.Close()
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
	_ = wss.conn.SetWriteDeadline(time.Now().Add(wsWriteWait))
	return wss.conn.WriteMessage(websocket.TextMessage, data)
}

func (wss *wsSession) fail(code, message string, closeCode int, closeReason string) error {
	_ = wss.send(SignalMessage{
		Type:    MessageTypeError,
		Code:    code,
		Message: message,
	})
	wss.closeWith(closeCode, closeReason)
	return nil
}

func (wss *wsSession) closeWith(code int, reason string) {
	wss.writeMu.Lock()
	defer wss.writeMu.Unlock()
	_ = wss.conn.WriteControl(websocket.CloseMessage, websocket.FormatCloseMessage(code, reason), time.Now().Add(wsWriteWait))
}

func (wss *wsSession) Close() {
	wss.closeOnce.Do(func() {
		if wss.session != nil {
			_ = wss.session.Close()
		}
		if wss.session == nil && wss.relaySession != nil {
			wss.relaySession.Close()
		}
		_ = wss.conn.Close()
	})
}

func isTimeout(err error) bool {
	var netErr net.Error
	return errors.As(err, &netErr) && netErr.Timeout()
}

func ptr[T any](v T) *T { return &v }
