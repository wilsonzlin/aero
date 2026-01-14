package relay

import (
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/netip"
	"strings"
	"sync"
	"time"

	"github.com/gorilla/websocket"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/auth"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/origin"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/ratelimit"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

const (
	wsUDPWriteWait = 1 * time.Second
)

type udpWSControlMessage struct {
	Type      string `json:"type"`
	SessionID string `json:"sessionId,omitempty"`
	Code      string `json:"code,omitempty"`
	Message   string `json:"message,omitempty"`
}

// UDPWebSocketServer implements GET /udp, a WebSocket-based UDP relay fallback
// that uses the same binary datagram framing as the WebRTC DataChannel.
//
// Each binary WebSocket message is treated as exactly one UDP relay datagram
// frame (v1 or v2), as defined in PROTOCOL.md.
type UDPWebSocketServer struct {
	cfg      config.Config
	verifier auth.Verifier
	log      *slog.Logger

	sessions *SessionManager
	relayCfg Config
	policy   *policy.DestinationPolicy

	upgrader websocket.Upgrader
}

type claimsVerifier interface {
	VerifyAndExtractClaims(credential string) (auth.JWTClaims, error)
}

func NewUDPWebSocketServer(cfg config.Config, sessions *SessionManager, relayCfg Config, pol *policy.DestinationPolicy, logger *slog.Logger) (*UDPWebSocketServer, error) {
	verifier, err := auth.NewVerifier(cfg)
	if err != nil {
		return nil, err
	}
	if logger == nil {
		logger = slog.New(slog.NewTextHandler(io.Discard, nil))
	}
	srv := &UDPWebSocketServer{
		cfg:      cfg,
		verifier: verifier,
		log:      logger,
		sessions: sessions,
		relayCfg: relayCfg.WithDefaults(),
		policy:   pol,
		upgrader: websocket.Upgrader{},
	}
	srv.upgrader.CheckOrigin = srv.checkOrigin
	srv.upgrader.Error = func(w http.ResponseWriter, r *http.Request, status int, reason error) {
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
		writeUDPWSJSONError(w, status, code, message)
	}
	return srv, nil
}

func (s *UDPWebSocketServer) checkOrigin(r *http.Request) bool {
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
	return origin.IsAllowed(normalizedOrigin, originHost, r.Host, s.cfg.AllowedOrigins)
}

func (s *UDPWebSocketServer) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	// Match /webrtc/signal: always return a JSON error to accidental HTTP callers
	// and avoid Gorilla's default plain-text `http.Error` responses.
	if !s.checkOrigin(r) {
		writeUDPWSJSONError(w, http.StatusForbidden, "forbidden", "forbidden")
		return
	}
	if !websocket.IsWebSocketUpgrade(r) {
		writeUDPWSJSONError(w, http.StatusBadRequest, "bad_message", "websocket upgrade required")
		return
	}

	conn, err := s.upgrader.Upgrade(w, r, nil)
	if err != nil {
		return
	}
	defer conn.Close()

	var writeMu sync.Mutex
	done := make(chan struct{})
	defer close(done)
	closeConn := func(code int, reason string) {
		writeMu.Lock()
		defer writeMu.Unlock()
		_ = conn.WriteControl(websocket.CloseMessage, websocket.FormatCloseMessage(code, reason), time.Now().Add(wsUDPWriteWait))
		_ = conn.Close()
	}

	// Keepalive / idle detection for long-lived /udp connections.
	//
	// We install the ping handler immediately (before auth) to ensure we serialize
	// any pong writes with our write mutex. The default gorilla/websocket ping
	// handler writes a pong frame directly and can race with our concurrent writers
	// (pings, ready/error messages, close frames).
	authenticated := s.cfg.AuthMode == config.AuthModeNone
	sessionKey := ""
	idleTimeout := s.cfg.UDPWSIdleTimeout
	if idleTimeout <= 0 {
		idleTimeout = config.DefaultUDPWSIdleTimeout
	}
	pingInterval := s.cfg.UDPWSPingInterval
	if pingInterval <= 0 {
		pingInterval = config.DefaultUDPWSPingInterval
	}
	conn.SetPingHandler(func(appData string) error {
		// Only extend the idle deadline once authenticated; we still want the auth
		// timeout to apply even if a client is pinging without authenticating.
		if authenticated {
			_ = conn.SetReadDeadline(time.Now().Add(idleTimeout))
		}
		writeMu.Lock()
		defer writeMu.Unlock()
		return conn.WriteControl(websocket.PongMessage, []byte(appData), time.Now().Add(wsUDPWriteWait))
	})
	// Serialize close handshake responses with the write mutex. The default close
	// handler writes a close frame directly and can race with our concurrent
	// writers (ping ticker, ready/error messages).
	conn.SetCloseHandler(func(code int, text string) error {
		writeMu.Lock()
		defer writeMu.Unlock()
		return conn.WriteControl(websocket.CloseMessage, websocket.FormatCloseMessage(code, text), time.Now().Add(wsUDPWriteWait))
	})

	sendErrorAndClose := func(wsCloseCode int, code, message string) {
		writeMu.Lock()
		defer writeMu.Unlock()

		_ = conn.SetWriteDeadline(time.Now().Add(wsUDPWriteWait))
		_ = conn.WriteJSON(udpWSControlMessage{Type: "error", Code: code, Message: message})
		_ = conn.WriteControl(websocket.CloseMessage, websocket.FormatCloseMessage(wsCloseCode, message), time.Now().Add(wsUDPWriteWait))
		_ = conn.Close()
	}

	metricsSink := func() *metrics.Metrics {
		if s.sessions == nil {
			return nil
		}
		return s.sessions.Metrics()
	}()
	incAuthFailure := func() {
		if metricsSink != nil {
			metricsSink.Inc(metrics.AuthFailure)
		}
	}

	if metricsSink != nil {
		metricsSink.Inc(metrics.UDPWSConnections)
	}

	if !authenticated {
		if cred, err := auth.CredentialFromQuery(s.cfg.AuthMode, r.URL.Query()); err == nil {
			var verifyErr error
			if s.cfg.AuthMode == config.AuthModeJWT {
				cv, ok := s.verifier.(claimsVerifier)
				if !ok {
					sendErrorAndClose(websocket.CloseInternalServerErr, "internal_error", "invalid auth configuration")
					return
				}
				claims, err := cv.VerifyAndExtractClaims(cred)
				if err != nil {
					verifyErr = err
				} else {
					sessionKey = claims.SID
				}
			} else {
				verifyErr = s.verifier.Verify(cred)
			}
			if verifyErr != nil {
				incAuthFailure()
				sendErrorAndClose(websocket.ClosePolicyViolation, "unauthorized", "invalid credentials")
				return
			}
			authenticated = true
		} else if err != nil && !errors.Is(err, auth.ErrMissingCredentials) {
			sendErrorAndClose(websocket.CloseInternalServerErr, "internal_error", "invalid auth configuration")
			return
		}
	}

	if !authenticated {
		authTimeout := s.cfg.SignalingAuthTimeout
		if authTimeout <= 0 {
			authTimeout = 2 * time.Second
		}
		_ = conn.SetReadDeadline(time.Now().Add(authTimeout))
		maxAuthBytes := s.cfg.MaxSignalingMessageBytes
		if maxAuthBytes <= 0 {
			maxAuthBytes = 64 * 1024
		}
		conn.SetReadLimit(maxAuthBytes)

		msgType, msg, err := conn.ReadMessage()
		if err != nil {
			if isTimeout(err) {
				incAuthFailure()
				sendErrorAndClose(websocket.ClosePolicyViolation, "unauthorized", "authentication timeout")
			}
			return
		}
		if msgType != websocket.TextMessage {
			incAuthFailure()
			sendErrorAndClose(websocket.ClosePolicyViolation, "unauthorized", "authentication required")
			return
		}

		var envelope struct {
			Type string `json:"type"`
		}
		if err := json.Unmarshal(msg, &envelope); err != nil || envelope.Type != "auth" {
			incAuthFailure()
			sendErrorAndClose(websocket.ClosePolicyViolation, "unauthorized", "authentication required")
			return
		}

		var authMsg auth.WireAuthMessage
		if err := json.Unmarshal(msg, &authMsg); err != nil {
			incAuthFailure()
			sendErrorAndClose(websocket.CloseUnsupportedData, "bad_message", "invalid auth message")
			return
		}
		if authMsg.APIKey != "" && authMsg.Token != "" && authMsg.APIKey != authMsg.Token {
			incAuthFailure()
			sendErrorAndClose(websocket.ClosePolicyViolation, "bad_message", "invalid auth message")
			return
		}
		cred, err := auth.CredentialFromAuthMessage(s.cfg.AuthMode, authMsg)
		if err != nil {
			incAuthFailure()
			sendErrorAndClose(websocket.ClosePolicyViolation, "unauthorized", "missing credentials")
			return
		}
		var verifyErr error
		if s.cfg.AuthMode == config.AuthModeJWT {
			cv, ok := s.verifier.(claimsVerifier)
			if !ok {
				sendErrorAndClose(websocket.CloseInternalServerErr, "internal_error", "invalid auth configuration")
				return
			}
			claims, err := cv.VerifyAndExtractClaims(cred)
			if err != nil {
				verifyErr = err
			} else {
				sessionKey = claims.SID
			}
		} else {
			verifyErr = s.verifier.Verify(cred)
		}
		if verifyErr != nil {
			incAuthFailure()
			sendErrorAndClose(websocket.ClosePolicyViolation, "unauthorized", "invalid credentials")
			return
		}

		authenticated = true
	}

	// Activate the idle timeout + keepalive ticker after authentication.
	_ = conn.SetReadDeadline(time.Now().Add(idleTimeout))
	conn.SetPongHandler(func(string) error {
		return conn.SetReadDeadline(time.Now().Add(idleTimeout))
	})
	go func() {
		ticker := time.NewTicker(pingInterval)
		defer ticker.Stop()
		for {
			select {
			case <-ticker.C:
				writeMu.Lock()
				err := conn.WriteControl(websocket.PingMessage, nil, time.Now().Add(wsUDPWriteWait))
				writeMu.Unlock()
				if err != nil {
					// Best-effort close; the connection may already be gone.
					closeConn(websocket.CloseGoingAway, "ping failed")
					return
				}
			case <-done:
				return
			}
		}
	}()

	var sess *Session
	sessionID := ""
	if s.sessions != nil {
		var err error
		sess, err = s.sessions.CreateSessionWithKey(sessionKey)
		if errors.Is(err, ErrTooManySessions) {
			sendErrorAndClose(websocket.CloseTryAgainLater, "too_many_sessions", "too many sessions")
			return
		}
		if errors.Is(err, ErrSessionAlreadyActive) {
			sendErrorAndClose(websocket.CloseTryAgainLater, "session_already_active", "session already active")
			return
		}
		if err != nil {
			sendErrorAndClose(websocket.CloseInternalServerErr, "internal_error", "failed to allocate session")
			return
		}
		defer sess.Close()
		sessionID = sess.ID()
	} else {
		// Still emit a sessionId for better observability when quota enforcement is
		// disabled (e.g. standalone testing). This ID is informational only.
		id, err := newSessionID()
		if err != nil {
			s.log.Warn("udp_ws_session_id_generation_failed", "err", err)
			sessionID = "unknown"
		} else {
			sessionID = id
		}
	}

	s.log.Info("udp_ws_connected", "session_id", sessionID, "remote_addr", r.RemoteAddr)
	defer s.log.Info("udp_ws_disconnected", "session_id", sessionID, "remote_addr", r.RemoteAddr)

	// Signal readiness for clients that need an explicit auth acknowledgement.
	// Clients that don't understand control messages should ignore this text frame.
	writeMu.Lock()
	_ = conn.SetWriteDeadline(time.Now().Add(wsUDPWriteWait))
	_ = conn.WriteJSON(udpWSControlMessage{Type: "ready", SessionID: sessionID})
	writeMu.Unlock()

	// Enforce binary datagram frame size limits at the WebSocket layer to avoid
	// large allocations. v2's max header length is 24 bytes (IPv6).
	maxFrameBytes := int64(s.relayCfg.MaxDatagramPayloadBytes) + 24
	if maxFrameBytes < 0 {
		maxFrameBytes = 0
	}
	conn.SetReadLimit(maxFrameBytes)

	codec, err := udpproto.NewCodec(s.relayCfg.MaxDatagramPayloadBytes)
	if err != nil {
		codec = udpproto.DefaultCodec
	}

	sender := &wsUDPDataChannel{
		conn:      conn,
		writeMu:   &writeMu,
		session:   sess,
		metrics:   metricsSink,
		closeConn: closeConn,
	}
	// We enforce per-session quotas/rate limits for the /udp endpoint in this
	// HTTP handler so we can increment WebSocket-specific metrics. The
	// SessionRelay itself still enforces protocol decoding, binding management,
	// and outbound framing/negotiation.
	relay := NewSessionRelay(sender, s.relayCfg, s.policy, nil, metricsSink)
	if metricsSink != nil && relay.queue != nil {
		relay.queue.SetOnDrop(func() {
			metricsSink.Inc(metrics.UDPWSDropped)
			metricsSink.Inc(metrics.UDPWSDroppedBackpressure)
		})
	}
	defer relay.Close()

	if sess != nil {
		go func() {
			<-sess.Done()
			closeConn(websocket.ClosePolicyViolation, "session closed")
		}()
	}

	for {
		msgType, msg, err := conn.ReadMessage()
		if err != nil {
			if isTimeout(err) {
				closeConn(websocket.CloseNormalClosure, "idle timeout")
			}
			if metricsSink != nil && errors.Is(err, websocket.ErrReadLimit) {
				metricsSink.Inc(metrics.UDPWSDropped)
				metricsSink.Inc(metrics.UDPWSDroppedOversized)
			}
			return
		}
		// Any successfully read frame counts as activity. Extend the idle deadline
		// in addition to handling pong frames via the PongHandler.
		_ = conn.SetReadDeadline(time.Now().Add(idleTimeout))
		if msgType != websocket.BinaryMessage {
			// Be tolerant: some clients may send an auth message even when already
			// authenticated (e.g. query-string auth with a first-message auth
			// fallback). Ignore redundant auth messages; reject any other non-binary
			// payloads to keep the data plane simple.
			if msgType == websocket.TextMessage {
				var envelope struct {
					Type string `json:"type"`
				}
				if err := json.Unmarshal(msg, &envelope); err == nil && envelope.Type == "auth" {
					continue
				}
			}
			sendErrorAndClose(websocket.CloseUnsupportedData, "bad_message", "expected binary message")
			return
		}

		if metricsSink != nil {
			metricsSink.Inc(metrics.UDPWSDatagramsIn)
		}

		// Decode once in the HTTP server so we can apply policy/rate limiting and
		// count drops. The relay engine will decode again internally; that
		// duplication is acceptable because policy/limiting needs access to the
		// decoded header.
		frame, err := codec.DecodeFrame(msg)
		if err != nil {
			if metricsSink != nil {
				metricsSink.Inc(metrics.UDPWSDropped)
				if errors.Is(err, udpproto.ErrPayloadTooLarge) {
					metricsSink.Inc(metrics.UDPWSDroppedOversized)
				} else {
					metricsSink.Inc(metrics.UDPWSDroppedMalformed)
				}
			}
			continue
		}

		if s.policy == nil {
			// Fail closed: a nil policy would turn the relay into an open UDP proxy.
			if metricsSink != nil {
				metricsSink.Inc(metrics.UDPWSDropped)
				metricsSink.Inc(metrics.UDPWSDroppedDeniedByPolicy)
			}
			continue
		}
		if err := s.policy.AllowUDP(net.IP(frame.RemoteIP.AsSlice()), frame.RemotePort); err != nil {
			if metricsSink != nil {
				metricsSink.Inc(metrics.UDPWSDropped)
				metricsSink.Inc(metrics.UDPWSDroppedDeniedByPolicy)
			}
			continue
		}

		if sess != nil {
			destKey := netip.AddrPortFrom(frame.RemoteIP, frame.RemotePort).String()
			allowed, reason := sess.AllowClientDatagramWithReason(destKey, frame.Payload)
			if !allowed {
				if metricsSink != nil {
					metricsSink.Inc(metrics.UDPWSDropped)
					if reason == ratelimit.DropReasonTooManyDestinations {
						metricsSink.Inc(metrics.UDPWSDroppedQuotaExceeded)
					} else {
						metricsSink.Inc(metrics.UDPWSDroppedRateLimited)
					}
				}
				continue
			}
		}

		relay.HandleDataChannelMessage(msg)
	}
}

type wsUDPDataChannel struct {
	conn      *websocket.Conn
	writeMu   *sync.Mutex
	session   *Session
	metrics   *metrics.Metrics
	closeConn func(code int, reason string)
}

func (d *wsUDPDataChannel) Send(data []byte) error {
	if d.session != nil && !d.session.HandleInboundToClient(data) {
		if d.metrics != nil {
			d.metrics.Inc(metrics.UDPWSDropped)
			d.metrics.Inc(metrics.UDPWSDroppedRateLimited)
		}
		return nil
	}

	d.writeMu.Lock()
	defer d.writeMu.Unlock()

	_ = d.conn.SetWriteDeadline(time.Now().Add(wsUDPWriteWait))
	err := d.conn.WriteMessage(websocket.BinaryMessage, data)
	if err == nil && d.metrics != nil {
		d.metrics.Inc(metrics.UDPWSDatagramsOut)
	}
	return err
}

func isTimeout(err error) bool {
	var netErr net.Error
	return errors.As(err, &netErr) && netErr.Timeout()
}

func writeUDPWSJSONError(w http.ResponseWriter, status int, code, message string) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(udpWSControlMessage{
		Type:    "error",
		Code:    code,
		Message: message,
	})
}
