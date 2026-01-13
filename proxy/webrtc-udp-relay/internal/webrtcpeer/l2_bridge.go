package webrtcpeer

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"

	"github.com/gorilla/websocket"
	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/l2tunnel"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
)

const (
	l2DialTimeout  = 5 * time.Second
	l2WriteTimeout = 5 * time.Second
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

	// ForwardAeroSession controls whether the caller's `aero_session` cookie is
	// forwarded to the backend as `Cookie: aero_session=<value>`.
	ForwardAeroSession bool

	AeroSessionCookie    string
	HasAeroSessionCookie bool

	MaxMessageBytes int
}

type l2BackendDialHTTPError struct {
	statusCode int
	err        error
}

func (e *l2BackendDialHTTPError) Error() string {
	if e == nil || e.err == nil {
		return "l2 backend dial failed"
	}
	return e.err.Error()
}

func (e *l2BackendDialHTTPError) Unwrap() error {
	if e == nil {
		return nil
	}
	return e.err
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

	rateLimitedLogOnce sync.Once
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
		// dialBackend already emitted metrics/logs.
		b.shutdown(true, l2BridgeShutdownInfo{})
		return
	}
	b.wsMu.Lock()
	b.ws = ws
	b.wsMu.Unlock()

	readErrCh := make(chan error, 1)
	writeErrCh := make(chan error, 1)
	go func() { readErrCh <- b.wsReadLoop(ws) }()
	go func() { writeErrCh <- b.wsWriteLoop(ws) }()

	var shutdown l2BridgeShutdownInfo

	select {
	case <-b.ctx.Done():
		// Shutdown is already in progress (Close/oversize/etc).
	case err := <-readErrCh:
		shutdown = b.shutdownInfoForLoopError(err, "ws_read")
	case err := <-writeErrCh:
		shutdown = b.shutdownInfoForLoopError(err, "ws_write")
	}

	// Ensure both sides are torn down if either direction fails.
	b.shutdown(true, shutdown)
}

func (b *l2Bridge) dialBackend() (*websocket.Conn, error) {
	if m := b.metrics(); m != nil {
		m.Inc(metrics.L2BridgeDialsTotal)
	}

	dialCtx, cancel := context.WithTimeout(b.ctx, l2DialTimeout)
	defer cancel()

	start := time.Now()
	ws, err := dialL2Backend(dialCtx, b.dialCfg)
	if err != nil {
		var httpErr *l2BackendDialHTTPError
		var statusCode any
		if errors.As(err, &httpErr) && httpErr.statusCode != 0 {
			statusCode = httpErr.statusCode
		}
		if m := b.metrics(); m != nil {
			m.Inc(metrics.L2BridgeDialErrorsTotal)
		}
		attrs := []any{
			"session_id", b.sessionID(),
			"backend_ws_url", sanitizeWSURLForLog(b.dialCfg.BackendWSURL),
			"dial_duration_ms", time.Since(start).Milliseconds(),
		}
		if statusCode != nil {
			attrs = append(attrs, "status_code", statusCode)
		}
		attrs = append(attrs, "err", b.sanitizeErrorForLog(err))
		slog.Warn("l2_bridge_backend_dial_failed", attrs...)
		return nil, err
	}

	slog.Info("l2_bridge_backend_dial_succeeded",
		"session_id", b.sessionID(),
		"backend_ws_url", sanitizeWSURLForLog(b.dialCfg.BackendWSURL),
		"dial_duration_ms", time.Since(start).Milliseconds(),
	)
	return ws, nil
}

func dialL2Backend(ctx context.Context, cfg l2BackendDialConfig) (*websocket.Conn, error) {
	dialer := websocket.Dialer{
		HandshakeTimeout: l2DialTimeout,
		Subprotocols:     []string{l2tunnel.Subprotocol},
	}

	if cfg.BackendToken != "" {
		tokenProto := l2tunnel.TokenSubprotocolPrefix + cfg.BackendToken
		if !isWebSocketSubprotocolToken(tokenProto) {
			return nil, fmt.Errorf("l2 backend token is not valid for Sec-WebSocket-Protocol; use query-string auth instead")
		}
		dialer.Subprotocols = append(dialer.Subprotocols, tokenProto)
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
			tokenProto := l2tunnel.TokenSubprotocolPrefix + cfg.Credential
			if !isWebSocketSubprotocolToken(tokenProto) {
				return nil, fmt.Errorf("l2 auth forwarding mode %q requires a credential that is valid for Sec-WebSocket-Protocol; use %q instead", cfg.AuthForwardMode, config.L2BackendAuthForwardModeQuery)
			}
			dialer.Subprotocols = append(dialer.Subprotocols, tokenProto)
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
	if cfg.ForwardAeroSession && cfg.HasAeroSessionCookie {
		header.Set("Cookie", "aero_session="+cfg.AeroSessionCookie)
	}

	conn, resp, err := dialer.DialContext(ctx, dialURL, header)
	if err != nil {
		if resp != nil {
			err = &l2BackendDialHTTPError{statusCode: resp.StatusCode, err: err}
		}
		if resp != nil && resp.Body != nil {
			_ = resp.Body.Close()
		}
		return nil, err
	}
	if resp != nil && resp.Body != nil {
		_ = resp.Body.Close()
	}
	if got := conn.Subprotocol(); got != l2tunnel.Subprotocol {
		_ = conn.Close()
		return nil, fmt.Errorf("l2 backend did not negotiate required subprotocol %q (got %q)", l2tunnel.Subprotocol, got)
	}
	if cfg.MaxMessageBytes > 0 {
		conn.SetReadLimit(int64(cfg.MaxMessageBytes))
	}
	return conn, nil
}

// isWebSocketSubprotocolToken reports whether raw is a valid WebSocket
// subprotocol token per RFC 6455, which uses the HTTP token grammar (RFC 7230
// tchar).
func isWebSocketSubprotocolToken(raw string) bool {
	if raw == "" {
		return false
	}
	for i := 0; i < len(raw); i++ {
		c := raw[i]
		switch {
		case c >= 'a' && c <= 'z':
			continue
		case c >= 'A' && c <= 'Z':
			continue
		case c >= '0' && c <= '9':
			continue
		}
		switch c {
		case '!', '#', '$', '%', '&', '\'', '*', '+', '-', '.', '^', '_', '`', '|', '~':
			continue
		default:
			return false
		}
	}
	return true
}

func (b *l2Bridge) wsWriteLoop(ws *websocket.Conn) error {
	for {
		select {
		case <-b.ctx.Done():
			return b.ctx.Err()
		case msg := <-b.toBackend:
			if b.dialCfg.MaxMessageBytes > 0 && len(msg) > b.dialCfg.MaxMessageBytes {
				if m := b.metrics(); m != nil {
					m.Inc(metrics.L2BridgeDroppedOversizedTotal)
				}
				return &l2BridgeOversizedError{
					direction: "from_client",
					size:      len(msg),
					max:       b.dialCfg.MaxMessageBytes,
				}
			}
			_ = ws.SetWriteDeadline(time.Now().Add(l2WriteTimeout))
			if err := ws.WriteMessage(websocket.BinaryMessage, msg); err != nil {
				return err
			}
			if m := b.metrics(); m != nil {
				m.Inc(metrics.L2BridgeMessagesFromClientTotal)
				m.Add(metrics.L2BridgeBytesFromClientTotal, uint64(len(msg)))
			}
		}
	}
}

func (b *l2Bridge) wsReadLoop(ws *websocket.Conn) error {
	for {
		msgType, payload, err := ws.ReadMessage()
		if err != nil {
			if errors.Is(err, websocket.ErrReadLimit) && b.dialCfg.MaxMessageBytes > 0 {
				if m := b.metrics(); m != nil {
					m.Inc(metrics.L2BridgeDroppedOversizedTotal)
				}
				return &l2BridgeOversizedError{
					direction: "to_client",
					max:       b.dialCfg.MaxMessageBytes,
				}
			}
			return err
		}
		if msgType != websocket.BinaryMessage {
			continue
		}
		if b.dialCfg.MaxMessageBytes > 0 && len(payload) > b.dialCfg.MaxMessageBytes {
			if m := b.metrics(); m != nil {
				m.Inc(metrics.L2BridgeDroppedOversizedTotal)
			}
			return &l2BridgeOversizedError{
				direction: "to_client",
				size:      len(payload),
				max:       b.dialCfg.MaxMessageBytes,
			}
		}

		if b.quota != nil && !b.quota.HandleInboundToClient(payload) {
			if m := b.metrics(); m != nil {
				m.Inc(metrics.L2BridgeDroppedRateLimitedTotal)
			}
			b.rateLimitedLogOnce.Do(func() {
				slog.Warn("l2_bridge_dropped_rate_limited",
					"session_id", b.sessionID(),
					"msg_bytes", len(payload),
				)
			})
			continue
		}

		b.sendMu.Lock()
		err = b.dc.Send(payload)
		b.sendMu.Unlock()
		if err != nil {
			return &l2BridgeDataChannelSendError{err: err}
		}
		if m := b.metrics(); m != nil {
			m.Inc(metrics.L2BridgeMessagesToClientTotal)
			m.Add(metrics.L2BridgeBytesToClientTotal, uint64(len(payload)))
		}
	}
}

func (b *l2Bridge) HandleDataChannelMessage(msg []byte) {
	if b.dialCfg.MaxMessageBytes > 0 && len(msg) > b.dialCfg.MaxMessageBytes {
		if m := b.metrics(); m != nil {
			m.Inc(metrics.L2BridgeDroppedOversizedTotal)
		}
		b.shutdown(true, l2BridgeShutdownInfo{
			reason:    l2BridgeShutdownReasonOversizedMessage,
			direction: "from_client",
			msgBytes:  len(msg),
			maxBytes:  b.dialCfg.MaxMessageBytes,
		})
		return
	}
	select {
	case <-b.ctx.Done():
		return
	case b.toBackend <- msg:
	}
}

func (b *l2Bridge) Close() {
	b.shutdown(false, l2BridgeShutdownInfo{reason: l2BridgeShutdownReasonDataChannelClosed})
}

type l2BridgeShutdownReason string

const (
	l2BridgeShutdownReasonBackendClosed     l2BridgeShutdownReason = "backend_closed"
	l2BridgeShutdownReasonDataChannelClosed l2BridgeShutdownReason = "datachannel_closed"
	l2BridgeShutdownReasonOversizedMessage  l2BridgeShutdownReason = "oversized_message"
)

type l2BridgeShutdownInfo struct {
	reason    l2BridgeShutdownReason
	direction string // "from_client" or "to_client" when applicable
	msgBytes  int
	maxBytes  int
	err       error
}

type l2BridgeOversizedError struct {
	direction string
	size      int
	max       int
}

func (e *l2BridgeOversizedError) Error() string {
	if e == nil {
		return "l2 message too large"
	}
	if e.size > 0 {
		return fmt.Sprintf("l2 message too large: %d bytes (max %d)", e.size, e.max)
	}
	return fmt.Sprintf("l2 message too large (max %d)", e.max)
}

type l2BridgeDataChannelSendError struct{ err error }

func (e *l2BridgeDataChannelSendError) Error() string {
	if e == nil || e.err == nil {
		return "datachannel send error"
	}
	return e.err.Error()
}

func (e *l2BridgeDataChannelSendError) Unwrap() error {
	if e == nil {
		return nil
	}
	return e.err
}

func (b *l2Bridge) shutdownInfoForLoopError(err error, source string) l2BridgeShutdownInfo {
	if err == nil {
		return l2BridgeShutdownInfo{reason: l2BridgeShutdownReasonBackendClosed}
	}

	var oversized *l2BridgeOversizedError
	if errors.As(err, &oversized) {
		return l2BridgeShutdownInfo{
			reason:    l2BridgeShutdownReasonOversizedMessage,
			direction: oversized.direction,
			msgBytes:  oversized.size,
			maxBytes:  oversized.max,
			err:       err,
		}
	}

	var dcSendErr *l2BridgeDataChannelSendError
	if errors.As(err, &dcSendErr) {
		return l2BridgeShutdownInfo{
			reason: l2BridgeShutdownReasonDataChannelClosed,
			err:    err,
		}
	}

	// Treat any other read/write loop error as a backend closure. This captures
	// both read and write errors, including graceful WebSocket close frames.
	return l2BridgeShutdownInfo{
		reason: l2BridgeShutdownReasonBackendClosed,
		err:    fmt.Errorf("%s: %w", source, err),
	}
}

func (b *l2Bridge) shutdown(closeDataChannel bool, info l2BridgeShutdownInfo) {
	b.closeOnce.Do(func() {
		if info.reason != "" {
			attrs := []any{
				"reason", string(info.reason),
				"session_id", b.sessionID(),
			}
			if info.direction != "" {
				attrs = append(attrs, "direction", info.direction)
			}
			if info.msgBytes > 0 {
				attrs = append(attrs, "msg_bytes", info.msgBytes)
			}
			if info.maxBytes > 0 {
				attrs = append(attrs, "max_bytes", info.maxBytes)
			}
			if info.err != nil {
				var closeErr *websocket.CloseError
				if errors.As(info.err, &closeErr) {
					attrs = append(attrs, "ws_close_code", closeErr.Code, "ws_close_text", b.sanitizeStringForLog(closeErr.Text))
				}
				attrs = append(attrs, "err", b.sanitizeErrorForLog(info.err))
			}
			level := slog.LevelInfo
			if info.reason == l2BridgeShutdownReasonOversizedMessage {
				level = slog.LevelWarn
			}
			slog.Default().Log(context.Background(), level, "l2_bridge_shutdown", attrs...)
		}

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

func (b *l2Bridge) metrics() *metrics.Metrics {
	if b.quota == nil {
		return nil
	}
	return b.quota.Metrics()
}

func (b *l2Bridge) sessionID() any {
	if b.quota == nil {
		return nil
	}
	return b.quota.ID()
}

func (b *l2Bridge) sanitizeErrorForLog(err error) string {
	if err == nil {
		return ""
	}
	return b.sanitizeStringForLog(err.Error())
}

func (b *l2Bridge) sanitizeStringForLog(msg string) string {
	if msg == "" {
		return ""
	}

	// Best-effort redaction. Most dial failures include the request URL; when
	// auth forwarding uses query parameters, that URL may contain the client's
	// credential.
	for _, secret := range []string{
		strings.TrimSpace(b.dialCfg.Credential),
		strings.TrimSpace(b.dialCfg.BackendToken),
		strings.TrimSpace(b.dialCfg.AeroSessionCookie),
	} {
		if secret == "" {
			continue
		}
		msg = strings.ReplaceAll(msg, secret, "<redacted>")
		if esc := url.QueryEscape(secret); esc != "" {
			msg = strings.ReplaceAll(msg, esc, "<redacted>")
		}
	}

	// Redact common credential-shaped query parameters even when we don't know the
	// value in advance (e.g. when L2_BACKEND_WS_URL embeds a static token).
	msg = redactQueryParamValue(msg, "token")
	msg = redactQueryParamValue(msg, "apiKey")
	msg = redactQueryParamValue(msg, "aero_session")

	return msg
}

func redactQueryParamValue(msg, key string) string {
	if msg == "" || key == "" {
		return msg
	}

	needle := key + "="
	const replacement = "<redacted>"

	i := 0
	for {
		j := strings.Index(msg[i:], needle)
		if j == -1 {
			return msg
		}
		j += i

		start := j + len(needle)
		end := start
		for end < len(msg) {
			switch msg[end] {
			case '&', ' ', '\n', '\r', '\t', '"', '\'', '>', '<', ')', ']', '}', ';':
				goto replace
			default:
				end++
			}
		}

	replace:
		msg = msg[:start] + replacement + msg[end:]
		i = start + len(replacement)
	}
}

func sanitizeWSURLForLog(raw string) string {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return ""
	}

	u, err := url.Parse(raw)
	if err != nil {
		// Fall back to stripping query/fragment, avoiding accidental token leaks.
		raw = strings.SplitN(raw, "?", 2)[0]
		raw = strings.SplitN(raw, "#", 2)[0]
		return raw
	}

	// Explicitly strip potentially sensitive components.
	u.User = nil
	u.RawQuery = ""
	u.Fragment = ""
	return u.String()
}
