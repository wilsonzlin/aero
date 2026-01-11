package webrtcpeer

import (
	"context"
	"fmt"
	"net/http"
	"net/url"
	"sync"
	"time"

	"github.com/gorilla/websocket"
	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
)

const (
	l2TunnelSubprotocol      = "aero-l2-tunnel-v1"
	l2TokenSubprotocolPrefix = "aero-l2-token."
	l2DialTimeout            = 5 * time.Second
	l2WriteTimeout           = 5 * time.Second
)

type l2BackendDialConfig struct {
	BackendWSURL string

	// ClientOrigin is the normalized Origin associated with the client signaling
	// request that created this WebRTC session.
	ClientOrigin string

	// Credential is the credential (JWT/API key) that authenticated the client to
	// this relay.
	Credential string

	ForwardOrigin bool

	AuthForwardMode config.L2BackendAuthForwardMode

	// BackendOriginOverride, when non-empty, is used as the Origin header value
	// for backend dials instead of forwarding ClientOrigin.
	BackendOriginOverride string

	// BackendToken is an optional token presented to the L2 backend via an
	// additional offered WebSocket subprotocol entry (`aero-l2-token.<token>`).
	//
	// The negotiated subprotocol is still required to be `aero-l2-tunnel-v1`.
	BackendToken string

	MaxMessageBytes int
}

// l2Bridge forwards binary messages between a WebRTC DataChannel ("l2") and a
// backend WebSocket that speaks subprotocol "aero-l2-tunnel-v1".
//
// The bridge does not interpret the payloads; it is purely a transport adapter.
type l2Bridge struct {
	dc *webrtc.DataChannel

	dialCfg l2BackendDialConfig
	quota   *relay.Session

	ctx    context.Context
	cancel context.CancelFunc

	closeOnce sync.Once

	wsMu sync.Mutex
	ws   *websocket.Conn

	// toBackend buffers client -> backend messages. When full,
	// HandleDataChannelMessage blocks (backpressure) rather than dropping frames.
	toBackend chan []byte

	sendMu sync.Mutex
}

func newL2Bridge(dc *webrtc.DataChannel, dialCfg l2BackendDialConfig, quota *relay.Session) *l2Bridge {
	ctx, cancel := context.WithCancel(context.Background())
	b := &l2Bridge{
		dc:        dc,
		dialCfg:   dialCfg,
		quota:     quota,
		ctx:       ctx,
		cancel:    cancel,
		toBackend: make(chan []byte, 256),
	}
	go b.run()
	return b
}

func (b *l2Bridge) run() {
	ws, err := b.dialBackend()
	if err != nil {
		b.shutdown(true)
		return
	}
	b.wsMu.Lock()
	b.ws = ws
	b.wsMu.Unlock()

	errCh := make(chan error, 2)
	go func() { errCh <- b.wsReadLoop(ws) }()
	go func() { errCh <- b.wsWriteLoop(ws) }()

	select {
	case <-b.ctx.Done():
	case <-errCh:
	}

	// Ensure both sides are torn down if either direction fails.
	b.shutdown(true)
}

func (b *l2Bridge) dialBackend() (*websocket.Conn, error) {
	dialCtx, cancel := context.WithTimeout(b.ctx, l2DialTimeout)
	defer cancel()

	return dialL2Backend(dialCtx, b.dialCfg)
}

func dialL2Backend(ctx context.Context, cfg l2BackendDialConfig) (*websocket.Conn, error) {
	dialer := websocket.Dialer{
		HandshakeTimeout: l2DialTimeout,
		Subprotocols:     []string{l2TunnelSubprotocol},
	}

	if cfg.BackendToken != "" {
		dialer.Subprotocols = append(dialer.Subprotocols, l2TokenSubprotocolPrefix+cfg.BackendToken)
	}

	dialURL := cfg.BackendWSURL

	switch cfg.AuthForwardMode {
	case config.L2BackendAuthForwardModeQuery:
		if cfg.Credential != "" {
			u, err := url.Parse(dialURL)
			if err != nil {
				return nil, err
			}
			q := u.Query()
			q.Set("token", cfg.Credential)
			q.Set("apiKey", cfg.Credential)
			u.RawQuery = q.Encode()
			dialURL = u.String()
		}
	case config.L2BackendAuthForwardModeSubprotocol:
		// Avoid sending multiple aero-l2-token.* entries if a fixed backend token is
		// configured; prefer the explicit backend token over the per-session
		// credential in that case.
		if cfg.Credential != "" && cfg.BackendToken == "" {
			dialer.Subprotocols = append(dialer.Subprotocols, l2TokenSubprotocolPrefix+cfg.Credential)
		}
	}

	header := http.Header{}
	origin := cfg.BackendOriginOverride
	if origin == "" && cfg.ForwardOrigin {
		origin = cfg.ClientOrigin
	}
	if origin != "" {
		header.Set("Origin", origin)
	}

	conn, resp, err := dialer.DialContext(ctx, dialURL, header)
	if err != nil {
		if resp != nil && resp.Body != nil {
			_ = resp.Body.Close()
		}
		return nil, err
	}
	if resp != nil && resp.Body != nil {
		_ = resp.Body.Close()
	}
	if got := conn.Subprotocol(); got != l2TunnelSubprotocol {
		_ = conn.Close()
		return nil, fmt.Errorf("l2 backend did not negotiate required subprotocol %q (got %q)", l2TunnelSubprotocol, got)
	}
	if cfg.MaxMessageBytes > 0 {
		conn.SetReadLimit(int64(cfg.MaxMessageBytes))
	}
	return conn, nil
}

func (b *l2Bridge) wsWriteLoop(ws *websocket.Conn) error {
	for {
		select {
		case <-b.ctx.Done():
			return b.ctx.Err()
		case msg := <-b.toBackend:
			if b.dialCfg.MaxMessageBytes > 0 && len(msg) > b.dialCfg.MaxMessageBytes {
				return fmt.Errorf("l2 message too large: %d bytes (max %d)", len(msg), b.dialCfg.MaxMessageBytes)
			}
			_ = ws.SetWriteDeadline(time.Now().Add(l2WriteTimeout))
			if err := ws.WriteMessage(websocket.BinaryMessage, msg); err != nil {
				return err
			}
		}
	}
}

func (b *l2Bridge) wsReadLoop(ws *websocket.Conn) error {
	for {
		msgType, payload, err := ws.ReadMessage()
		if err != nil {
			return err
		}
		if msgType != websocket.BinaryMessage {
			continue
		}
		if b.dialCfg.MaxMessageBytes > 0 && len(payload) > b.dialCfg.MaxMessageBytes {
			return fmt.Errorf("l2 message too large: %d bytes (max %d)", len(payload), b.dialCfg.MaxMessageBytes)
		}

		if b.quota != nil && !b.quota.HandleInboundToClient(payload) {
			continue
		}

		b.sendMu.Lock()
		err = b.dc.Send(payload)
		b.sendMu.Unlock()
		if err != nil {
			return err
		}
	}
}

func (b *l2Bridge) HandleDataChannelMessage(msg []byte) {
	if b.dialCfg.MaxMessageBytes > 0 && len(msg) > b.dialCfg.MaxMessageBytes {
		b.shutdown(true)
		return
	}
	select {
	case <-b.ctx.Done():
		return
	case b.toBackend <- msg:
	}
}

func (b *l2Bridge) Close() {
	b.shutdown(false)
}

func (b *l2Bridge) shutdown(closeDataChannel bool) {
	b.closeOnce.Do(func() {
		b.cancel()

		b.wsMu.Lock()
		ws := b.ws
		b.ws = nil
		b.wsMu.Unlock()
		if ws != nil {
			_ = ws.Close()
		}

		if closeDataChannel && b.dc != nil {
			_ = b.dc.Close()
		}
	})
}
