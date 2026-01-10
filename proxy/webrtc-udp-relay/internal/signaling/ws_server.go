package signaling

import (
	"encoding/json"
	"errors"
	"io"
	"net"
	"net/http"
	"time"

	"github.com/gorilla/websocket"
	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/auth"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/webrtcpeer"
)

const wsWriteWait = 1 * time.Second

// WebSocketServer implements the signaling handshake used by browser clients.
//
// It enforces authentication (api_key/jwt) plus per-connection limits to avoid
// idle unauthenticated connections and large or high-rate signaling messages.
type WebSocketServer struct {
	cfg      config.Config
	verifier auth.Verifier
	api      *webrtc.API
	relayCfg relay.Config
	policy   *policy.DestinationPolicy
	upgrader websocket.Upgrader
}

func NewWebSocketServer(cfg config.Config) (*WebSocketServer, error) {
	verifier, err := auth.NewVerifier(cfg)
	if err != nil {
		return nil, err
	}

	api, err := webrtcpeer.NewAPI(cfg)
	if err != nil {
		return nil, err
	}

	destPolicy, err := policy.NewDestinationPolicyFromEnv()
	if err != nil {
		return nil, err
	}

	relayCfg := relay.Config{
		MaxUDPBindingsPerSession:  cfg.MaxUDPBindingsPerSession,
		UDPBindingIdleTimeout:     cfg.UDPBindingIdleTimeout,
		UDPReadBufferBytes:        cfg.UDPReadBufferBytes,
		DataChannelSendQueueBytes: cfg.DataChannelSendQueueBytes,
		PreferV2:                  cfg.PreferV2,
	}.WithDefaults()

	return &WebSocketServer{
		cfg:      cfg,
		verifier: verifier,
		api:      api,
		relayCfg: relayCfg,
		policy:   destPolicy,
		upgrader: websocket.Upgrader{
			CheckOrigin: func(r *http.Request) bool { return true },
		},
	}, nil
}

func (s *WebSocketServer) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	conn, err := s.upgrader.Upgrade(w, r, nil)
	if err != nil {
		return
	}
	defer conn.Close()

	authenticated := false
	if cred, err := auth.CredentialFromQuery(s.cfg.AuthMode, r.URL.Query()); err == nil {
		if err := s.verifier.Verify(cred); err != nil {
			writeClose(conn, websocket.ClosePolicyViolation, "invalid credentials")
			return
		}
		authenticated = true
	} else if err != nil && !errors.Is(err, auth.ErrMissingCredentials) {
		writeClose(conn, websocket.CloseInternalServerErr, "invalid auth configuration")
		return
	}

	if !authenticated {
		_ = conn.SetReadDeadline(time.Now().Add(s.cfg.SignalingAuthTimeout))
	}

	limiter := newRateLimiter(s.cfg.MaxSignalingMessagesPerSecond)

	var sess *webrtcpeer.Session
	defer func() {
		if sess != nil {
			_ = sess.Close()
		}
	}()

	for {
		if !limiter.Allow(time.Now()) {
			writeClose(conn, websocket.ClosePolicyViolation, "rate limit exceeded")
			return
		}

		msgType, msgReader, err := conn.NextReader()
		if err != nil {
			if !authenticated && isTimeout(err) {
				writeClose(conn, websocket.ClosePolicyViolation, "authentication timeout")
			}
			return
		}
		if msgType != websocket.TextMessage {
			writeClose(conn, websocket.CloseUnsupportedData, "expected text message")
			return
		}

		msg, err := readLimited(msgReader, s.cfg.MaxSignalingMessageBytes)
		if err != nil {
			if errors.Is(err, errMessageTooLarge) {
				writeClose(conn, websocket.CloseMessageTooBig, "message too large")
				return
			}
			writeClose(conn, websocket.CloseInternalServerErr, "failed to read message")
			return
		}

		if !authenticated {
			var envelope struct {
				Type string `json:"type"`
			}
			if err := json.Unmarshal(msg, &envelope); err != nil || envelope.Type != "auth" {
				writeClose(conn, websocket.ClosePolicyViolation, "authentication required")
				return
			}

			var authMsg auth.WireAuthMessage
			if err := json.Unmarshal(msg, &authMsg); err != nil {
				writeClose(conn, websocket.CloseUnsupportedData, "invalid auth message")
				return
			}

			cred, err := auth.CredentialFromAuthMessage(s.cfg.AuthMode, authMsg)
			if err != nil {
				writeClose(conn, websocket.ClosePolicyViolation, "missing credentials")
				return
			}
			if err := s.verifier.Verify(cred); err != nil {
				writeClose(conn, websocket.ClosePolicyViolation, "invalid credentials")
				return
			}

			authenticated = true
			_ = conn.SetReadDeadline(time.Time{})
			continue
		}

		var offerReq offerRequest
		if err := json.Unmarshal(msg, &offerReq); err != nil {
			writeClose(conn, websocket.CloseUnsupportedData, "invalid message")
			return
		}
		if offerReq.Version != 1 {
			writeClose(conn, websocket.ClosePolicyViolation, "unsupported signaling version")
			return
		}
		if offerReq.Offer.Type != "offer" || offerReq.Offer.SDP == "" {
			writeClose(conn, websocket.CloseUnsupportedData, "invalid offer")
			return
		}
		if sess != nil {
			writeClose(conn, websocket.ClosePolicyViolation, "offer already received")
			return
		}

		sess, err = webrtcpeer.NewSession(s.api, s.cfg.ICEServers, s.relayCfg, s.policy, nil)
		if err != nil {
			writeClose(conn, websocket.CloseInternalServerErr, "failed to create peer connection")
			return
		}

		pc := sess.PeerConnection()

		if err := pc.SetRemoteDescription(webrtc.SessionDescription{Type: webrtc.SDPTypeOffer, SDP: offerReq.Offer.SDP}); err != nil {
			writeClose(conn, websocket.ClosePolicyViolation, "invalid offer")
			return
		}

		answer, err := pc.CreateAnswer(nil)
		if err != nil {
			writeClose(conn, websocket.CloseInternalServerErr, "failed to create answer")
			return
		}
		if err := pc.SetLocalDescription(answer); err != nil {
			writeClose(conn, websocket.CloseInternalServerErr, "failed to set local description")
			return
		}

		<-webrtc.GatheringCompletePromise(pc)

		local := pc.LocalDescription()
		if local == nil {
			writeClose(conn, websocket.CloseInternalServerErr, "missing local description")
			return
		}

		resp := answerResponse{
			Version: 1,
			Answer: sessionDescription{
				Type: "answer",
				SDP:  local.SDP,
			},
		}
		payload, err := json.Marshal(resp)
		if err != nil {
			writeClose(conn, websocket.CloseInternalServerErr, "failed to encode answer")
			return
		}

		conn.SetWriteDeadline(time.Now().Add(wsWriteWait))
		if err := conn.WriteMessage(websocket.TextMessage, payload); err != nil {
			return
		}
	}
}

type offerRequest struct {
	Version int                `json:"version"`
	Offer   sessionDescription `json:"offer"`
}

type answerResponse struct {
	Version int                `json:"version"`
	Answer  sessionDescription `json:"answer"`
}

type sessionDescription struct {
	Type string `json:"type"`
	SDP  string `json:"sdp"`
}

func writeClose(conn *websocket.Conn, code int, reason string) {
	_ = conn.WriteControl(websocket.CloseMessage, websocket.FormatCloseMessage(code, reason), time.Now().Add(wsWriteWait))
}

func isTimeout(err error) bool {
	var netErr net.Error
	return errors.As(err, &netErr) && netErr.Timeout()
}

var errMessageTooLarge = errors.New("message too large")

func readLimited(r io.Reader, max int64) ([]byte, error) {
	if max <= 0 {
		return nil, errMessageTooLarge
	}
	b, err := io.ReadAll(io.LimitReader(r, max+1))
	if err != nil {
		return nil, err
	}
	if int64(len(b)) > max {
		return nil, errMessageTooLarge
	}
	return b, nil
}

type rateLimiter struct {
	rate     float64
	capacity float64
	tokens   float64
	last     time.Time
}

func newRateLimiter(messagesPerSecond int) *rateLimiter {
	rate := float64(messagesPerSecond)
	return &rateLimiter{
		rate:     rate,
		capacity: rate,
		tokens:   rate,
		last:     time.Now(),
	}
}

func (rl *rateLimiter) Allow(now time.Time) bool {
	elapsed := now.Sub(rl.last).Seconds()
	rl.tokens += elapsed * rl.rate
	if rl.tokens > rl.capacity {
		rl.tokens = rl.capacity
	}
	rl.last = now

	if rl.tokens < 1 {
		return false
	}
	rl.tokens--
	return true
}
