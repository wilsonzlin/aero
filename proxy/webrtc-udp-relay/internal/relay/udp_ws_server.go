package relay

import (
	"encoding/json"
	"errors"
	"net"
	"net/http"
	"net/netip"
	"sync"
	"time"

	"github.com/gorilla/websocket"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/auth"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

const (
	wsUDPMetricFramesIn          = "udp_ws_frames_in"
	wsUDPMetricFramesOut         = "udp_ws_frames_out"
	wsUDPMetricDroppedByPolicy   = "udp_ws_dropped_policy"
	wsUDPMetricDroppedByRate     = "udp_ws_dropped_rate_limit"
	wsUDPMetricDroppedBackpress  = "udp_ws_dropped_backpressure"
	wsUDPWriteWait               = 1 * time.Second
	wsUDPAuthCloseReason         = "authentication required"
	wsUDPAuthTimeoutCloseReason  = "authentication timeout"
	wsUDPInvalidCredsCloseReason = "invalid credentials"
)

// UDPWebSocketServer implements GET /udp, a WebSocket-based UDP relay fallback
// that uses the same binary datagram framing as the WebRTC DataChannel.
//
// Each binary WebSocket message is treated as exactly one UDP relay datagram
// frame (v1 or v2), as defined in PROTOCOL.md.
type UDPWebSocketServer struct {
	cfg      config.Config
	verifier auth.Verifier

	sessions *SessionManager
	relayCfg Config
	policy   *policy.DestinationPolicy

	upgrader websocket.Upgrader
}

func NewUDPWebSocketServer(cfg config.Config, sessions *SessionManager, relayCfg Config, pol *policy.DestinationPolicy) (*UDPWebSocketServer, error) {
	verifier, err := auth.NewVerifier(cfg)
	if err != nil {
		return nil, err
	}
	return &UDPWebSocketServer{
		cfg:      cfg,
		verifier: verifier,
		sessions: sessions,
		relayCfg: relayCfg.WithDefaults(),
		policy:   pol,
		upgrader: websocket.Upgrader{
			CheckOrigin: func(r *http.Request) bool { return true },
		},
	}, nil
}

func (s *UDPWebSocketServer) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	conn, err := s.upgrader.Upgrade(w, r, nil)
	if err != nil {
		return
	}
	defer conn.Close()

	var writeMu sync.Mutex
	closeConn := func(code int, reason string) {
		writeMu.Lock()
		defer writeMu.Unlock()
		_ = conn.WriteControl(websocket.CloseMessage, websocket.FormatCloseMessage(code, reason), time.Now().Add(wsUDPWriteWait))
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

	authenticated := false
	if cred, err := auth.CredentialFromQuery(s.cfg.AuthMode, r.URL.Query()); err == nil {
		if err := s.verifier.Verify(cred); err != nil {
			incAuthFailure()
			closeConn(websocket.ClosePolicyViolation, wsUDPInvalidCredsCloseReason)
			return
		}
		authenticated = true
	} else if err != nil && !errors.Is(err, auth.ErrMissingCredentials) {
		closeConn(websocket.CloseInternalServerErr, "invalid auth configuration")
		return
	}

	if !authenticated {
		_ = conn.SetReadDeadline(time.Now().Add(s.cfg.SignalingAuthTimeout))
		conn.SetReadLimit(s.cfg.MaxSignalingMessageBytes)

		msgType, msg, err := conn.ReadMessage()
		if err != nil {
			if isTimeout(err) {
				incAuthFailure()
				closeConn(websocket.ClosePolicyViolation, wsUDPAuthTimeoutCloseReason)
			}
			return
		}
		if msgType != websocket.TextMessage {
			incAuthFailure()
			closeConn(websocket.ClosePolicyViolation, wsUDPAuthCloseReason)
			return
		}

		var envelope struct {
			Type string `json:"type"`
		}
		if err := json.Unmarshal(msg, &envelope); err != nil || envelope.Type != "auth" {
			incAuthFailure()
			closeConn(websocket.ClosePolicyViolation, wsUDPAuthCloseReason)
			return
		}

		var authMsg auth.WireAuthMessage
		if err := json.Unmarshal(msg, &authMsg); err != nil {
			incAuthFailure()
			closeConn(websocket.CloseUnsupportedData, "invalid auth message")
			return
		}
		if authMsg.APIKey != "" && authMsg.Token != "" && authMsg.APIKey != authMsg.Token {
			incAuthFailure()
			closeConn(websocket.ClosePolicyViolation, "invalid auth message")
			return
		}
		cred, err := auth.CredentialFromAuthMessage(s.cfg.AuthMode, authMsg)
		if err != nil {
			incAuthFailure()
			closeConn(websocket.ClosePolicyViolation, "missing credentials")
			return
		}
		if err := s.verifier.Verify(cred); err != nil {
			incAuthFailure()
			closeConn(websocket.ClosePolicyViolation, wsUDPInvalidCredsCloseReason)
			return
		}

		authenticated = true
		_ = conn.SetReadDeadline(time.Time{})
	}

	var sess *Session
	if s.sessions != nil {
		var err error
		sess, err = s.sessions.CreateSession()
		if errors.Is(err, ErrTooManySessions) {
			closeConn(websocket.CloseTryAgainLater, "too many sessions")
			return
		}
		if err != nil {
			closeConn(websocket.CloseInternalServerErr, "failed to allocate session")
			return
		}
		defer sess.Close()
	}

	// Enforce binary datagram frame size limits at the WebSocket layer to avoid
	// large allocations. v2's max header length is 24 bytes (IPv6).
	maxFrameBytes := int64(s.relayCfg.MaxDatagramPayloadBytes + 24)
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
	relay := NewSessionRelay(sender, s.relayCfg, s.policy, nil)
	if metricsSink != nil && relay.queue != nil {
		relay.queue.SetOnDrop(func() {
			metricsSink.Inc(wsUDPMetricDroppedBackpress)
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
			return
		}
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
			closeConn(websocket.CloseUnsupportedData, "expected binary message")
			return
		}

		if metricsSink != nil {
			metricsSink.Inc(wsUDPMetricFramesIn)
		}

		// Decode once in the HTTP server so we can apply policy/rate limiting and
		// count drops. The relay engine will decode again internally; that
		// duplication is acceptable because policy/limiting needs access to the
		// decoded header.
		frame, err := codec.DecodeFrame(msg)
		if err != nil {
			continue
		}

		if s.policy == nil {
			// Fail closed: a nil policy would turn the relay into an open UDP proxy.
			if metricsSink != nil {
				metricsSink.Inc(wsUDPMetricDroppedByPolicy)
			}
			continue
		}
		if err := s.policy.AllowUDP(net.IP(frame.RemoteIP.AsSlice()), frame.RemotePort); err != nil {
			if metricsSink != nil {
				metricsSink.Inc(wsUDPMetricDroppedByPolicy)
			}
			continue
		}

		if sess != nil {
			destKey := netip.AddrPortFrom(frame.RemoteIP, frame.RemotePort).String()
			if !sess.HandleClientDatagram(frame.GuestPort, destKey, frame.Payload) {
				if metricsSink != nil {
					metricsSink.Inc(wsUDPMetricDroppedByRate)
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
			d.metrics.Inc(wsUDPMetricDroppedByRate)
		}
		return nil
	}

	d.writeMu.Lock()
	defer d.writeMu.Unlock()

	_ = d.conn.SetWriteDeadline(time.Now().Add(wsUDPWriteWait))
	err := d.conn.WriteMessage(websocket.BinaryMessage, data)
	if err == nil && d.metrics != nil {
		d.metrics.Inc(wsUDPMetricFramesOut)
	}
	return err
}

func isTimeout(err error) bool {
	var netErr net.Error
	return errors.As(err, &netErr) && netErr.Timeout()
}
