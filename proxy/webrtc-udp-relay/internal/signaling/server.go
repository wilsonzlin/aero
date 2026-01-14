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

	Authorizer authorizer

	// ICEGatheringTimeout bounds how long the relay waits for candidate gathering
	// on non-trickle HTTP endpoints (e.g. /offer and /webrtc/offer).
	ICEGatheringTimeout time.Duration

	// WebRTCSessionConnectTimeout bounds how long a newly-created PeerConnection
	// is allowed to remain unconnected before being closed. This is particularly
	// important for HTTP offer endpoints, which otherwise return immediately and
	// may leave PeerConnections running indefinitely if the client never
	// completes ICE/DTLS.
	WebRTCSessionConnectTimeout time.Duration

	// SessionPreallocTTL controls how long sessions allocated via POST /session
	// remain reserved before being automatically released.
	SessionPreallocTTL time.Duration

	// WebSocket auth timeout for AUTH_MODE!=none.
	SignalingAuthTimeout time.Duration

	// WebSocket keepalive + idle management (after auth completes).
	SignalingWSIdleTimeout  time.Duration
	SignalingWSPingInterval time.Duration

	// WebSocket inbound signaling hardening.
	MaxSignalingMessageBytes      int64
	MaxSignalingMessagesPerSecond int

	// WebRTCDataChannelMaxMessageBytes bounds inbound WebRTC DataChannel messages.
	// It should match the pion SettingEngine SCTP max message size configuration
	// (see webrtcpeer.NewAPI).
	WebRTCDataChannelMaxMessageBytes int
}

// server implements the relay's HTTP/WebSocket signaling surface.
//
// Endpoints:
//   - POST /offer          : versioned, non-trickle offer/answer exchange (used by integration tests)
//   - POST /session        : optional session pre-allocation (used by other tasks)
//   - GET  /webrtc/signal  : WebSocket signaling with trickle ICE
//   - POST /webrtc/offer   : HTTP offer -> answer (non-trickle ICE fallback)
type server struct {
	// Sessions enforces global session quotas.
	Sessions *relay.SessionManager

	// WebRTC is the server-side pion API used to construct PeerConnections.
	WebRTC *webrtc.API

	// ICEServers is the ICE server list for server-side PeerConnections.
	ICEServers []webrtc.ICEServer

	RelayConfig relay.Config
	Policy      *policy.DestinationPolicy

	AllowedOrigins []string

	Authorizer          authorizer
	ICEGatheringTimeout time.Duration
	SessionPreallocTTL  time.Duration

	WebRTCSessionConnectTimeout time.Duration

	SignalingAuthTimeout time.Duration

	SignalingWSIdleTimeout  time.Duration
	SignalingWSPingInterval time.Duration

	MaxSignalingMessageBytes      int64
	MaxSignalingMessagesPerSecond int

	WebRTCDataChannelMaxMessageBytes int

	mu             sync.Mutex
	webrtcSessions map[*webrtcpeer.Session]struct{}
	preSessions    map[string]*preSessionReservation
}

type preSessionReservation struct {
	sess  *relay.Session
	timer *time.Timer
}

var gatheringCompletePromise = webrtc.GatheringCompletePromise

func NewServer(cfg Config) *server {
	return &server{
		Sessions:                    cfg.Sessions,
		WebRTC:                      cfg.WebRTC,
		ICEServers:                  cfg.ICEServers,
		RelayConfig:                 cfg.RelayConfig,
		Policy:                      cfg.Policy,
		AllowedOrigins:              cfg.AllowedOrigins,
		Authorizer:                  cfg.Authorizer,
		ICEGatheringTimeout:         cfg.ICEGatheringTimeout,
		WebRTCSessionConnectTimeout: cfg.WebRTCSessionConnectTimeout,
		SessionPreallocTTL:          cfg.SessionPreallocTTL,
		SignalingAuthTimeout:        cfg.SignalingAuthTimeout,
		SignalingWSIdleTimeout:      cfg.SignalingWSIdleTimeout,
		SignalingWSPingInterval:     cfg.SignalingWSPingInterval,

		MaxSignalingMessageBytes:         cfg.MaxSignalingMessageBytes,
		MaxSignalingMessagesPerSecond:    cfg.MaxSignalingMessagesPerSecond,
		WebRTCDataChannelMaxMessageBytes: cfg.WebRTCDataChannelMaxMessageBytes,

		webrtcSessions: make(map[*webrtcpeer.Session]struct{}),
	}
}

func (s *server) RegisterRoutes(mux *http.ServeMux) {
	mux.HandleFunc("POST /offer", s.handleOffer)
	mux.HandleFunc("POST /session", s.handleCreateSession)

	mux.HandleFunc("GET /webrtc/signal", s.handleWebSocketSignal)
	mux.HandleFunc("POST /webrtc/offer", s.handleWebRTCOffer)
}

func (s *server) Close() {
	s.mu.Lock()
	webrtcSessions := make([]*webrtcpeer.Session, 0, len(s.webrtcSessions))
	for sess := range s.webrtcSessions {
		webrtcSessions = append(webrtcSessions, sess)
	}
	preSessions := make([]preSessionReservation, 0, len(s.preSessions))
	for _, reservation := range s.preSessions {
		if reservation == nil {
			continue
		}
		preSessions = append(preSessions, *reservation)
	}
	s.webrtcSessions = nil
	s.preSessions = nil
	s.mu.Unlock()

	for _, sess := range webrtcSessions {
		_ = sess.Close()
	}
	for _, reservation := range preSessions {
		if reservation.timer != nil {
			reservation.timer.Stop()
		}
		if reservation.sess != nil {
			reservation.sess.Close()
		}
	}
}

func (s *server) trackWebRTCSession(sess *webrtcpeer.Session) {
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

func (s *server) untrackWebRTCSession(sess *webrtcpeer.Session) {
	if sess == nil {
		return
	}
	s.mu.Lock()
	if s.webrtcSessions != nil {
		delete(s.webrtcSessions, sess)
	}
	s.mu.Unlock()
}

func (s *server) authorizer() authorizer {
	if s.Authorizer == nil {
		return allowAllAuthorizer{}
	}
	return s.Authorizer
}

func (s *server) iceGatheringTimeout() time.Duration {
	if s.ICEGatheringTimeout <= 0 {
		return 2 * time.Second
	}
	return s.ICEGatheringTimeout
}

func (s *server) webrtcSessionConnectTimeout() time.Duration {
	if s.WebRTCSessionConnectTimeout <= 0 {
		return 30 * time.Second
	}
	return s.WebRTCSessionConnectTimeout
}

func (s *server) signalingAuthTimeout() time.Duration {
	if s.SignalingAuthTimeout <= 0 {
		return 2 * time.Second
	}
	return s.SignalingAuthTimeout
}

func (s *server) signalingWSIdleTimeout() time.Duration {
	if s.SignalingWSIdleTimeout <= 0 {
		return 60 * time.Second
	}
	return s.SignalingWSIdleTimeout
}

func (s *server) signalingWSPingInterval() time.Duration {
	if s.SignalingWSPingInterval <= 0 {
		return 20 * time.Second
	}
	return s.SignalingWSPingInterval
}

func (s *server) maxSignalingMessageBytes() int64 {
	if s.MaxSignalingMessageBytes <= 0 {
		return 64 * 1024
	}
	return s.MaxSignalingMessageBytes
}

func (s *server) maxSignalingMessagesPerSecond() int {
	if s.MaxSignalingMessagesPerSecond <= 0 {
		return 50
	}
	return s.MaxSignalingMessagesPerSecond
}

const defaultSessionPreallocTTL = 60 * time.Second

func (s *server) sessionPreallocTTL() time.Duration {
	if s.SessionPreallocTTL <= 0 {
		return defaultSessionPreallocTTL
	}
	return s.SessionPreallocTTL
}

func (s *server) incMetric(name string) {
	if s.Sessions == nil {
		return
	}
	m := s.Sessions.Metrics()
	if m == nil {
		return
	}
	m.Inc(name)
}

func (s *server) handleCreateSession(w http.ResponseWriter, r *http.Request) {
	if !s.checkOrigin(r) {
		writeJSONError(w, http.StatusForbidden, "forbidden", "forbidden")
		return
	}

	if s.Sessions == nil {
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "session manager not configured")
		return
	}

	authRes, err := s.authorizer().Authorize(r, nil)
	if err != nil {
		if isUnauthorized(err) {
			s.incMetric(metrics.AuthFailure)
			writeJSONError(w, http.StatusUnauthorized, "unauthorized", "unauthorized")
			return
		}
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "internal error")
		return
	}

	session, err := s.Sessions.CreateSessionWithKey(authRes.SessionKey)
	if errors.Is(err, relay.ErrTooManySessions) {
		writeJSONError(w, http.StatusServiceUnavailable, "too_many_sessions", "too many sessions")
		return
	}
	if errors.Is(err, relay.ErrSessionAlreadyActive) {
		writeJSONError(w, http.StatusConflict, "session_already_active", "session already active")
		return
	}
	if err != nil {
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "internal error")
		return
	}

	// The /session endpoint is currently a simple pre-allocation mechanism; it
	// does not yet have a corresponding "use session" handshake. To avoid
	// permanently consuming quota, preallocated sessions automatically expire
	// after a short TTL.
	sessionID := session.ID()
	reservation := &preSessionReservation{sess: session}

	session.AddOnClose(func() {
		s.mu.Lock()
		if s.preSessions != nil {
			if cur, ok := s.preSessions[sessionID]; ok && cur == reservation {
				delete(s.preSessions, sessionID)
				if cur.timer != nil {
					cur.timer.Stop()
				}
			}
		}
		s.mu.Unlock()
	})

	s.mu.Lock()
	if s.preSessions == nil {
		s.preSessions = make(map[string]*preSessionReservation)
	}
	s.preSessions[sessionID] = reservation
	s.mu.Unlock()

	timer := time.AfterFunc(s.sessionPreallocTTL(), func() {
		session.Close()
	})

	// Install the timer reference so it can be stopped on server shutdown or when
	// the session is otherwise closed. If the reservation has already been
	// removed (e.g. due to concurrent shutdown), stop the timer immediately.
	s.mu.Lock()
	if s.preSessions != nil {
		if cur, ok := s.preSessions[sessionID]; ok && cur == reservation {
			cur.timer = timer
		} else {
			timer.Stop()
		}
	} else {
		timer.Stop()
	}
	s.mu.Unlock()

	w.WriteHeader(http.StatusCreated)
	_, _ = w.Write([]byte(sessionID))
}

func (s *server) handleOffer(w http.ResponseWriter, r *http.Request) {
	if !s.checkOrigin(r) {
		writeJSONError(w, http.StatusForbidden, "forbidden", "forbidden")
		return
	}

	var req offerRequest
	if err := json.NewDecoder(http.MaxBytesReader(w, r.Body, 2<<20)).Decode(&req); err != nil {
		writeJSONError(w, http.StatusBadRequest, "bad_message", "invalid offer")
		return
	}
	if err := req.Validate(); err != nil {
		if errors.Is(err, errUnsupportedVersion) {
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

	authRes, err := s.authorizer().Authorize(r, &clientHello{Type: messageTypeOffer})
	if err != nil {
		if isUnauthorized(err) {
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
		relaySession, err = s.Sessions.CreateSessionWithKey(authRes.SessionKey)
		if errors.Is(err, relay.ErrTooManySessions) {
			writeJSONError(w, http.StatusServiceUnavailable, "too_many_sessions", "too many sessions")
			return
		}
		if errors.Is(err, relay.ErrSessionAlreadyActive) {
			writeJSONError(w, http.StatusConflict, "session_already_active", "session already active")
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

	sess, err = webrtcpeer.NewSession(
		s.WebRTC,
		s.ICEServers,
		s.RelayConfig,
		s.Policy,
		relaySession,
		clientOrigin,
		clientCredential,
		aeroSessionCookieFromRequest(r),
		s.WebRTCDataChannelMaxMessageBytes,
		webrtcpeer.SessionOptions{
			ConnectTimeout: s.webrtcSessionConnectTimeout(),
			RemoteAddr:     r.RemoteAddr,
		},
		cleanup,
	)
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

	gatherComplete := gatheringCompletePromise(pc)
	if err := pc.SetLocalDescription(answer); err != nil {
		_ = sess.Close()
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "failed to set local description")
		return
	}

	waitCtx, cancel := context.WithTimeout(r.Context(), s.iceGatheringTimeout())
	defer cancel()
	timedOut := false
	select {
	case <-gatherComplete:
	case <-waitCtx.Done():
		timedOut = errors.Is(waitCtx.Err(), context.DeadlineExceeded)
	}
	// If the request is canceled (client disconnected), abort without writing a
	// response; otherwise return the best-effort SDP even if ICE gathering isn't
	// complete yet (matching /webrtc/offer fallback behavior).
	if r.Context().Err() != nil {
		_ = sess.Close()
		return
	}

	local := pc.LocalDescription()
	if local == nil {
		_ = sess.Close()
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "failed to gather local description")
		return
	}

	if timedOut {
		s.incMetric(metrics.ICEGatheringTimeout)
	}

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(answerResponse{
		Version: req.Version,
		Answer: sessionDescription{
			Type: "answer",
			SDP:  local.SDP,
		},
	})
}

func (s *server) handleWebRTCOffer(w http.ResponseWriter, r *http.Request) {
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

	authRes, err := s.authorizer().Authorize(r, &clientHello{Type: messageTypeOffer})
	if err != nil {
		if isUnauthorized(err) {
			s.incMetric(metrics.AuthFailure)
			writeJSONError(w, http.StatusUnauthorized, "unauthorized", "unauthorized")
			return
		}
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "internal error")
		return
	}

	clientOrigin := httpserver.NormalizedOriginFromRequest(r)
	clientCredential := authRes.Credential

	sessionID, relaySession, err := s.allocateRelaySession(authRes.SessionKey)
	if err != nil {
		if errors.Is(err, relay.ErrTooManySessions) {
			writeJSONError(w, http.StatusServiceUnavailable, "too_many_sessions", "too many sessions")
			return
		}
		if errors.Is(err, relay.ErrSessionAlreadyActive) {
			writeJSONError(w, http.StatusConflict, "session_already_active", "session already active")
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

	sess, err = webrtcpeer.NewSession(
		s.WebRTC,
		s.ICEServers,
		s.RelayConfig,
		s.Policy,
		relaySession,
		clientOrigin,
		clientCredential,
		aeroSessionCookieFromRequest(r),
		s.WebRTCDataChannelMaxMessageBytes,
		webrtcpeer.SessionOptions{
			ConnectTimeout: s.webrtcSessionConnectTimeout(),
			RemoteAddr:     r.RemoteAddr,
		},
		cleanup,
	)
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

	gatherComplete := gatheringCompletePromise(pc)
	if err := pc.SetLocalDescription(answer); err != nil {
		_ = sess.Close()
		writeJSONError(w, http.StatusInternalServerError, "internal_error", err.Error())
		return
	}

	waitCtx, cancel := context.WithTimeout(r.Context(), s.iceGatheringTimeout())
	defer cancel()
	timedOut := false
	select {
	case <-gatherComplete:
	case <-waitCtx.Done():
		timedOut = errors.Is(waitCtx.Err(), context.DeadlineExceeded)
	}
	if r.Context().Err() != nil {
		_ = sess.Close()
		return
	}

	local := pc.LocalDescription()
	if local == nil {
		_ = sess.Close()
		writeJSONError(w, http.StatusInternalServerError, "internal_error", "missing local description")
		return
	}

	if timedOut {
		s.incMetric(metrics.ICEGatheringTimeout)
	}

	writeJSON(w, http.StatusOK, httpOfferResponse{
		SessionID: sessionID,
		SDP:       sdpFromPion(*local),
	})
}

func (s *server) handleWebSocketSignal(w http.ResponseWriter, r *http.Request) {
	if !s.checkOrigin(r) {
		writeJSONError(w, http.StatusForbidden, "forbidden", "forbidden")
		return
	}

	// Gorilla websocket defaults to writing plain-text `http.Error` bodies on
	// upgrade failures. Preflight the request so callers always receive a JSON
	// response when they accidentally hit the WebSocket endpoint over HTTP.
	if !websocket.IsWebSocketUpgrade(r) {
		writeJSONError(w, http.StatusBadRequest, "bad_message", "websocket upgrade required")
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
		Error: func(w http.ResponseWriter, r *http.Request, status int, reason error) {
			code := "bad_message"
			message := "websocket upgrade failed"
			if reason != nil {
				message = reason.Error()
			}
			if status == http.StatusForbidden {
				code = "forbidden"
				message = "forbidden"
			} else if status >= 500 {
				code = "internal_error"
				message = "internal error"
			}
			writeJSONError(w, status, code, message)
		},
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

		authTimeout:  s.signalingAuthTimeout(),
		idleTimeout:  s.signalingWSIdleTimeout(),
		pingInterval: s.signalingWSPingInterval(),
		limiter: ratelimit.NewTokenBucket(
			nil,
			int64(s.maxSignalingMessagesPerSecond()),
			int64(s.maxSignalingMessagesPerSecond()),
		),

		maxMessageBytes: s.maxSignalingMessageBytes(),
	}
	ws.run()
}

func (s *server) checkOrigin(r *http.Request) bool {
	origins := r.Header.Values("Origin")
	if len(origins) == 0 {
		return true
	}
	if len(origins) > 1 {
		return false
	}

	originHeader := strings.TrimSpace(origins[0])
	if originHeader == "" {
		return true
	}
	normalizedOrigin, originHost, ok := origin.NormalizeHeader(originHeader)
	if !ok {
		return false
	}
	return origin.IsAllowed(normalizedOrigin, originHost, r.Host, s.AllowedOrigins)
}

func (s *server) allocateRelaySession(sessionKey string) (string, *relay.Session, error) {
	if s.Sessions == nil {
		id, err := newSessionID()
		if err != nil {
			return "", nil, err
		}
		return id, nil, nil
	}

	relaySession, err := s.Sessions.CreateSessionWithKey(sessionKey)
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

func aeroSessionCookieFromRequest(r *http.Request) *string {
	if r == nil {
		return nil
	}
	cookie, err := r.Cookie("aero_session")
	if err != nil {
		return nil
	}
	v := cookie.Value
	return &v
}

type httpOfferRequest struct {
	SDP sdp `json:"sdp"`
}

type httpOfferResponse struct {
	SessionID string `json:"sessionId"`
	SDP       sdp    `json:"sdp"`
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

func parseHTTPOfferRequest(body []byte) (sdp, error) {
	var req httpOfferRequest
	reqErr := decodeStrictJSON(body, &req)
	if reqErr == nil {
		return req.SDP, nil
	}

	var rawSDP sdp
	sdpErr := decodeStrictJSON(body, &rawSDP)
	if sdpErr == nil {
		return rawSDP, nil
	}

	return sdp{}, fmt.Errorf("invalid offer request body (expected {\"sdp\":{...}} or a raw SessionDescription): %w", errors.Join(reqErr, sdpErr))
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
	srv  *server
	conn *websocket.Conn
	req  *http.Request

	authorizer authorizer

	authTimeout     time.Duration
	idleTimeout     time.Duration
	pingInterval    time.Duration
	maxMessageBytes int64
	limiter         messageLimiter

	session      *webrtcpeer.Session
	relaySession *relay.Session
	origin       string
	credential   string
	sessionKey   string

	writeMu sync.Mutex

	answerMu   sync.Mutex
	answerSent bool
	candBuf    []candidate

	closeOnce sync.Once

	keepaliveOnce sync.Once
	keepaliveDone chan struct{}
}

type messageLimiter interface {
	Allow(tokens int64) bool
}

func (wss *wsSession) installPeerHandlers() {
	pc := wss.session.PeerConnection()

	pc.OnICECandidate(func(c *webrtc.ICECandidate) {
		if c == nil {
			return
		}

		cand := candidateFromPion(c.ToJSON())

		wss.answerMu.Lock()
		if !wss.answerSent {
			wss.candBuf = append(wss.candBuf, cand)
			wss.answerMu.Unlock()
			return
		}
		wss.answerMu.Unlock()

		_ = wss.send(signalMessage{
			Type:      messageTypeCandidate,
			Candidate: &cand,
		})
	})
}

const wsWriteWait = 1 * time.Second

func (wss *wsSession) startKeepalive() {
	// Defensive defaults: a zero/negative idle timeout disables the read deadline,
	// which can lead to leaked connections. The config loader validates these
	// values, but tests may construct a Server directly.
	if wss.idleTimeout <= 0 {
		wss.idleTimeout = 60 * time.Second
	}
	if wss.pingInterval <= 0 {
		wss.pingInterval = 20 * time.Second
	}

	_ = wss.conn.SetReadDeadline(time.Now().Add(wss.idleTimeout))
	wss.conn.SetPongHandler(func(string) error {
		return wss.conn.SetReadDeadline(time.Now().Add(wss.idleTimeout))
	})

	wss.keepaliveOnce.Do(func() {
		wss.keepaliveDone = make(chan struct{})
		go wss.keepaliveLoop()
	})
}

func (wss *wsSession) keepaliveLoop() {
	ticker := time.NewTicker(wss.pingInterval)
	defer ticker.Stop()
	for {
		select {
		case <-ticker.C:
			wss.writeMu.Lock()
			err := wss.conn.WriteControl(websocket.PingMessage, nil, time.Now().Add(wsWriteWait))
			wss.writeMu.Unlock()
			if err != nil {
				// Best-effort close: if the ping failed, the peer is likely gone
				// already, but attempting a clean close helps clients surface a
				// deterministic code in logs.
				wss.closeWith(websocket.CloseGoingAway, "ping failed")
				_ = wss.conn.Close()
				return
			}
		case <-wss.keepaliveDone:
			return
		}
	}
}

func (wss *wsSession) run() {
	defer wss.Close()

	wss.origin = httpserver.NormalizedOriginFromRequest(wss.req)

	wss.conn.SetReadLimit(wss.maxMessageBytes)
	// Ensure we always respond to peer pings while serializing writes with the
	// session's write mutex. The default gorilla/websocket ping handler writes a
	// pong frame directly and can race with our own concurrent writers (pings,
	// signaling messages, close frames).
	wss.conn.SetPingHandler(func(appData string) error {
		// Only extend the idle deadline once keepalive is active; we still want the
		// auth timeout to apply even if a client is pinging without authenticating.
		if wss.keepaliveDone != nil {
			_ = wss.conn.SetReadDeadline(time.Now().Add(wss.idleTimeout))
		}
		wss.writeMu.Lock()
		defer wss.writeMu.Unlock()
		return wss.conn.WriteControl(websocket.PongMessage, []byte(appData), time.Now().Add(wsWriteWait))
	})
	// Ensure close handshake responses are also serialized with writeMu. The
	// default close handler writes a close frame directly, which can race with our
	// concurrent writers (ping ticker, signaling messages).
	wss.conn.SetCloseHandler(func(code int, text string) error {
		wss.writeMu.Lock()
		defer wss.writeMu.Unlock()
		return wss.conn.WriteControl(websocket.CloseMessage, websocket.FormatCloseMessage(code, text), time.Now().Add(wsWriteWait))
	})

	var haveOffer bool

	authorized := false
	if authRes, err := wss.authorizer.Authorize(wss.req, nil); err != nil {
		if isAuthMissing(err) {
			_ = wss.conn.SetReadDeadline(time.Now().Add(wss.authTimeout))
		} else if isUnauthorized(err) {
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
		wss.sessionKey = authRes.SessionKey
		wss.startKeepalive()
	}

	for {
		msgType, data, err := wss.conn.ReadMessage()
		if err != nil {
			if !authorized && isTimeout(err) {
				wss.srv.incMetric(metrics.AuthFailure)
				wss.closeWith(websocket.ClosePolicyViolation, "authentication timeout")
			} else if authorized && isTimeout(err) {
				wss.closeWith(websocket.CloseNormalClosure, "idle timeout")
			}
			return
		}
		if authorized {
			// Any successfully read frame counts as activity; extend the idle deadline
			// in addition to handling pong frames via the PongHandler.
			_ = wss.conn.SetReadDeadline(time.Now().Add(wss.idleTimeout))
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

		msg, err := parseSignalMessage(data)
		if err != nil {
			_ = wss.fail("bad_message", err.Error(), websocket.ClosePolicyViolation, "bad message")
			return
		}

		if !authorized {
			if msg.Type != messageTypeAuth {
				wss.srv.incMetric(metrics.AuthFailure)
				_ = wss.fail("unauthorized", "authentication required", websocket.ClosePolicyViolation, "authentication required")
				return
			}

			cred := msg.APIKey
			if cred == "" {
				cred = msg.Token
			}
			authRes, err := wss.authorizer.Authorize(wss.req, &clientHello{Type: messageTypeAuth, Credential: cred})
			if err != nil {
				if isUnauthorized(err) {
					wss.srv.incMetric(metrics.AuthFailure)
					_ = wss.fail("unauthorized", unauthorizedMessage(err), websocket.ClosePolicyViolation, "unauthorized")
				} else {
					_ = wss.fail("internal_error", "internal error", websocket.CloseInternalServerErr, "internal error")
				}
				return
			}

			authorized = true
			wss.credential = authRes.Credential
			wss.sessionKey = authRes.SessionKey
			wss.startKeepalive()
			continue
		}

		switch msg.Type {
		case messageTypeAuth:
			// Be tolerant: clients may send an auth message even when already
			// authenticated (e.g. query-string fallback or AUTH_MODE=none).
			if !haveOffer {
				continue
			}
			_ = wss.fail("unexpected_message", "auth received after offer", websocket.ClosePolicyViolation, "unexpected message")
			return
		case messageTypeOffer:
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
		case messageTypeCandidate:
			if !haveOffer {
				_ = wss.fail("unexpected_message", "candidate received before offer", websocket.ClosePolicyViolation, "unexpected message")
				return
			}
			if err := wss.handleRemoteCandidate(*msg.Candidate); err != nil {
				_ = wss.fail("bad_message", err.Error(), websocket.ClosePolicyViolation, "bad message")
				return
			}
		case messageTypeClose:
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

func (wss *wsSession) handleOffer(offerWire sdp) error {
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

	_, relaySession, err := wss.srv.allocateRelaySession(wss.sessionKey)
	if errors.Is(err, relay.ErrTooManySessions) {
		return &wsProtocolError{Code: "too_many_sessions", Message: "too many sessions"}
	}
	if errors.Is(err, relay.ErrSessionAlreadyActive) {
		return &wsProtocolError{Code: "session_already_active", Message: "session already active"}
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

	sess, err = webrtcpeer.NewSession(
		wss.srv.WebRTC,
		wss.srv.ICEServers,
		wss.srv.RelayConfig,
		wss.srv.Policy,
		relaySession,
		wss.origin,
		wss.credential,
		aeroSessionCookieFromRequest(wss.req),
		wss.srv.WebRTCDataChannelMaxMessageBytes,
		webrtcpeer.SessionOptions{
			ConnectTimeout: wss.srv.webrtcSessionConnectTimeout(),
			RemoteAddr:     wss.req.RemoteAddr,
		},
		cleanup,
	)
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

	if err := wss.send(signalMessage{
		Type: messageTypeAnswer,
		SDP:  ptr(sdpFromPion(*local)),
	}); err != nil {
		_ = sess.Close()
		return err
	}

	var buffered []candidate
	wss.answerMu.Lock()
	wss.answerSent = true
	buffered = append(buffered, wss.candBuf...)
	wss.candBuf = nil
	wss.answerMu.Unlock()

	for i := range buffered {
		cand := buffered[i]
		_ = wss.send(signalMessage{
			Type:      messageTypeCandidate,
			Candidate: &cand,
		})
	}

	return nil
}

func (wss *wsSession) handleRemoteCandidate(candWire candidate) error {
	if candWire.Candidate == "" {
		return nil
	}
	return wss.session.PeerConnection().AddICECandidate(candWire.ToPion())
}

func (wss *wsSession) send(msg signalMessage) error {
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
	_ = wss.send(signalMessage{
		Type:    messageTypeError,
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
		if wss.keepaliveDone != nil {
			close(wss.keepaliveDone)
		}
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
