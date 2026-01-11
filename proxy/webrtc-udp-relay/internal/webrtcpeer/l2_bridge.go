package webrtcpeer

import (
	"context"
	"fmt"
	"net/http"
	"sync"
	"time"

	"github.com/gorilla/websocket"
	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
)

const (
	l2TunnelSubprotocol = "aero-l2-tunnel-v1"
	l2DialTimeout       = 5 * time.Second
	l2WriteTimeout      = 5 * time.Second
)

// l2Bridge forwards binary messages between a WebRTC DataChannel ("l2") and a
// backend WebSocket that speaks subprotocol "aero-l2-tunnel-v1".
//
// The bridge does not interpret the payloads; it is purely a transport adapter.
type l2Bridge struct {
	dc *webrtc.DataChannel

	backendURL      string
	maxMessageBytes int
	quota           *relay.Session

	ctx    context.Context
	cancel context.CancelFunc

	closeOnce sync.Once

	wsMu sync.Mutex
	ws   *websocket.Conn

	// toBackend buffers client -> backend messages so pion callbacks don't block
	// on WebSocket I/O.
	toBackend chan []byte

	sendMu sync.Mutex
}

func newL2Bridge(dc *webrtc.DataChannel, backendURL string, maxMessageBytes int, quota *relay.Session) *l2Bridge {
	ctx, cancel := context.WithCancel(context.Background())
	b := &l2Bridge{
		dc:              dc,
		backendURL:      backendURL,
		maxMessageBytes: maxMessageBytes,
		quota:           quota,
		ctx:             ctx,
		cancel:          cancel,
		toBackend:       make(chan []byte, 256),
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

	dialer := websocket.Dialer{
		HandshakeTimeout: l2DialTimeout,
		Subprotocols:     []string{l2TunnelSubprotocol},
	}
	conn, resp, err := dialer.DialContext(dialCtx, b.backendURL, http.Header{})
	if err != nil {
		return nil, err
	}
	if resp != nil && resp.Body != nil {
		_ = resp.Body.Close()
	}
	if got := conn.Subprotocol(); got != l2TunnelSubprotocol {
		_ = conn.Close()
		return nil, fmt.Errorf("l2 backend did not negotiate required subprotocol %q (got %q)", l2TunnelSubprotocol, got)
	}
	if b.maxMessageBytes > 0 {
		conn.SetReadLimit(int64(b.maxMessageBytes))
	}
	return conn, nil
}

func (b *l2Bridge) wsWriteLoop(ws *websocket.Conn) error {
	for {
		select {
		case <-b.ctx.Done():
			return b.ctx.Err()
		case msg := <-b.toBackend:
			if b.maxMessageBytes > 0 && len(msg) > b.maxMessageBytes {
				return fmt.Errorf("l2 message too large: %d bytes (max %d)", len(msg), b.maxMessageBytes)
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
		if b.maxMessageBytes > 0 && len(payload) > b.maxMessageBytes {
			return fmt.Errorf("l2 message too large: %d bytes (max %d)", len(payload), b.maxMessageBytes)
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
	if b.maxMessageBytes > 0 && len(msg) > b.maxMessageBytes {
		b.shutdown(true)
		return
	}
	select {
	case <-b.ctx.Done():
		return
	case b.toBackend <- msg:
	default:
		// Drop rather than block pion internals. Clients should use an unordered
		// unreliable channel (ordered=false, maxRetransmits=0), so loss is
		// expected under congestion anyway.
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
