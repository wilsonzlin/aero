package relay

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

func makeTestJWTWithIat(secret, sid string, iat int64) string {
	header := base64.RawURLEncoding.EncodeToString([]byte(`{"alg":"HS256","typ":"JWT"}`))
	payloadJSON, _ := json.Marshal(struct {
		Iat int64  `json:"iat"`
		Exp int64  `json:"exp"`
		SID string `json:"sid"`
	}{
		Iat: iat,
		Exp: iat + 60,
		SID: sid,
	})
	payload := base64.RawURLEncoding.EncodeToString(payloadJSON)
	unsigned := header + "." + payload

	mac := hmac.New(sha256.New, []byte(secret))
	_, _ = mac.Write([]byte(unsigned))
	sig := base64.RawURLEncoding.EncodeToString(mac.Sum(nil))
	return unsigned + "." + sig
}

func startUDPEchoServer(t *testing.T, network string, ip net.IP) (*net.UDPConn, uint16) {
	t.Helper()

	conn, err := net.ListenUDP(network, &net.UDPAddr{IP: ip, Port: 0})
	if err != nil {
		if network == "udp6" {
			t.Skipf("ipv6 not supported: %v", err)
		}
		t.Fatalf("ListenUDP: %v", err)
	}

	go func() {
		buf := make([]byte, 64*1024)
		for {
			n, peer, err := conn.ReadFromUDP(buf)
			if err != nil {
				return
			}
			_, _ = conn.WriteToUDP(buf[:n], peer)
		}
	}()

	return conn, uint16(conn.LocalAddr().(*net.UDPAddr).Port)
}

func dialWS(t *testing.T, baseURL, path string) *websocket.Conn {
	t.Helper()
	wsURL := "ws" + strings.TrimPrefix(baseURL, "http") + path
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })
	return c
}

func readWSJSON(t *testing.T, c *websocket.Conn, timeout time.Duration) map[string]any {
	t.Helper()

	_ = c.SetReadDeadline(time.Now().Add(timeout))
	msgType, msg, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("ReadMessage: %v", err)
	}
	if msgType != websocket.TextMessage {
		t.Fatalf("msgType=%d, want TextMessage", msgType)
	}

	var out map[string]any
	if err := json.Unmarshal(msg, &out); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	return out
}

func readWSBinary(t *testing.T, c *websocket.Conn, timeout time.Duration) []byte {
	t.Helper()

	deadline := time.Now().Add(timeout)
	for {
		_ = c.SetReadDeadline(deadline)
		msgType, msg, err := c.ReadMessage()
		if err != nil {
			t.Fatalf("ReadMessage: %v", err)
		}
		if msgType == websocket.BinaryMessage {
			return msg
		}
		// Ignore text control messages like {"type":"ready"}.
	}
}

func TestUDPWebSocketServer_IdleTimeoutClosesWithoutPong(t *testing.T) {
	idleTimeout := 500 * time.Millisecond
	pingInterval := 50 * time.Millisecond

	cfg := config.Config{
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
		UDPWSIdleTimeout:         idleTimeout,
		UDPWSPingInterval:        pingInterval,
	}
	relayCfg := DefaultConfig()

	srv, err := NewUDPWebSocketServer(cfg, nil, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")
	pingSeen := make(chan struct{}, 1)
	c.SetPingHandler(func(string) error {
		select {
		case pingSeen <- struct{}{}:
		default:
		}
		// Intentionally do not respond with pong.
		return nil
	})

	_ = readWSJSON(t, c, 2*time.Second) // ready

	errCh := make(chan error, 1)
	go func() {
		_, _, err := c.ReadMessage()
		errCh <- err
	}()

	select {
	case <-pingSeen:
	case err := <-errCh:
		t.Fatalf("connection closed before receiving ping: %v", err)
	case <-time.After(2 * time.Second):
		t.Fatalf("timeout waiting for server ping")
	}

	select {
	case err := <-errCh:
		if err == nil {
			t.Fatalf("expected server to close the websocket")
		}
		if !websocket.IsCloseError(err, websocket.CloseNormalClosure) {
			t.Fatalf("expected close normal closure, got %v", err)
		}
	case <-time.After(2 * time.Second):
		t.Fatalf("timeout waiting for server to close idle websocket")
	}
}

func TestUDPWebSocketServer_PongKeepsConnectionOpenBeyondIdleTimeout(t *testing.T) {
	idleTimeout := 500 * time.Millisecond
	pingInterval := 50 * time.Millisecond
	cfg := config.Config{
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
		UDPWSIdleTimeout:         idleTimeout,
		UDPWSPingInterval:        pingInterval,
	}
	relayCfg := DefaultConfig()

	srv, err := NewUDPWebSocketServer(cfg, nil, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")
	pingSeen := make(chan struct{}, 1)
	c.SetPingHandler(func(appData string) error {
		select {
		case pingSeen <- struct{}{}:
		default:
		}
		return c.WriteControl(websocket.PongMessage, []byte(appData), time.Now().Add(1*time.Second))
	})

	_ = readWSJSON(t, c, 2*time.Second) // ready

	errCh := make(chan error, 1)
	go func() {
		_, _, err := c.ReadMessage()
		errCh <- err
	}()

	select {
	case <-pingSeen:
	case err := <-errCh:
		t.Fatalf("connection closed before receiving ping: %v", err)
	case <-time.After(2 * time.Second):
		t.Fatalf("timeout waiting for server ping")
	}

	time.Sleep(idleTimeout + 2*pingInterval)

	select {
	case err := <-errCh:
		t.Fatalf("unexpected close before idle timeout elapsed: %v", err)
	default:
	}

	_ = c.Close()
	select {
	case <-errCh:
	case <-time.After(2 * time.Second):
		t.Fatalf("timeout waiting for read goroutine to exit")
	}
}

func TestUDPWebSocketServer_ReadyIncludesSessionIDWithoutSessionManager(t *testing.T) {
	cfg := config.Config{
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	relayCfg := DefaultConfig()

	srv, err := NewUDPWebSocketServer(cfg, nil, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")
	ready := readWSJSON(t, c, 2*time.Second)
	if ready["type"] != "ready" {
		t.Fatalf("expected ready message, got %#v", ready)
	}
	sessionID, _ := ready["sessionId"].(string)
	if sessionID == "" {
		t.Fatalf("expected non-empty sessionId, got %#v", ready["sessionId"])
	}
}

func TestUDPWebSocketServer_JWTRejectsConcurrentSessionsWithSameSID(t *testing.T) {
	cfg := config.Config{
		AuthMode:                 config.AuthModeJWT,
		JWTSecret:                "supersecret",
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	now := time.Now().Unix()
	tokenA := makeTestJWTWithIat(cfg.JWTSecret, "sess_test", now-10)
	tokenB := makeTestJWTWithIat(cfg.JWTSecret, "sess_test", now-9)

	c1 := dialWS(t, ts.URL, "/udp?token="+tokenA)
	ready := readWSJSON(t, c1, 2*time.Second)
	if ready["type"] != "ready" {
		t.Fatalf("expected ready message, got %#v", ready)
	}

	c2 := dialWS(t, ts.URL, "/udp?token="+tokenB)
	errMsg := readWSJSON(t, c2, 2*time.Second)
	if errMsg["type"] != "error" || errMsg["code"] != "session_already_active" {
		t.Fatalf("expected session_already_active error, got %#v", errMsg)
	}
}

func TestUDPWebSocketServer_JWTRejectsConcurrentSessionsWithSameSID_FirstMessageAuth(t *testing.T) {
	cfg := config.Config{
		AuthMode:                 config.AuthModeJWT,
		JWTSecret:                "supersecret",
		SignalingAuthTimeout:     2 * time.Second,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	now := time.Now().Unix()
	tokenA := makeTestJWTWithIat(cfg.JWTSecret, "sess_test", now-10)
	tokenB := makeTestJWTWithIat(cfg.JWTSecret, "sess_test", now-9)

	c1 := dialWS(t, ts.URL, "/udp")
	if err := c1.WriteJSON(map[string]any{"type": "auth", "token": tokenA}); err != nil {
		t.Fatalf("WriteJSON(auth): %v", err)
	}
	ready := readWSJSON(t, c1, 2*time.Second)
	if ready["type"] != "ready" {
		t.Fatalf("expected ready message, got %#v", ready)
	}

	c2 := dialWS(t, ts.URL, "/udp")
	if err := c2.WriteJSON(map[string]any{"type": "auth", "token": tokenB}); err != nil {
		t.Fatalf("WriteJSON(auth): %v", err)
	}
	errMsg := readWSJSON(t, c2, 2*time.Second)
	if errMsg["type"] != "error" || errMsg["code"] != "session_already_active" {
		t.Fatalf("expected session_already_active error, got %#v", errMsg)
	}
}

func TestUDPWebSocketServer_RelaysV1IPv4(t *testing.T) {
	echo, echoPort := startUDPEchoServer(t, "udp4", net.IPv4(127, 0, 0, 1))
	defer echo.Close()

	cfg := config.Config{
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()
	relayCfg.PreferV2 = true

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")

	in := udpproto.Frame{
		GuestPort:  1234,
		RemoteIP:   netip.MustParseAddr("127.0.0.1"),
		RemotePort: echoPort,
		Payload:    []byte("hello"),
	}
	pkt, err := udpproto.DefaultCodec.EncodeFrameV1(in)
	if err != nil {
		t.Fatalf("EncodeV1: %v", err)
	}

	if err := c.WriteMessage(websocket.BinaryMessage, pkt); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}

	outPkt := readWSBinary(t, c, 2*time.Second)

	outFrame, err := udpproto.DefaultCodec.DecodeFrame(outPkt)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if outFrame.Version != 1 {
		t.Fatalf("outFrame.Version=%d, want 1", outFrame.Version)
	}
	if outFrame.GuestPort != in.GuestPort {
		t.Fatalf("outFrame.GuestPort=%d, want %d", outFrame.GuestPort, in.GuestPort)
	}
	if outFrame.RemotePort != echoPort {
		t.Fatalf("outFrame.RemotePort=%d, want %d", outFrame.RemotePort, echoPort)
	}
	if string(outFrame.Payload) != "hello" {
		t.Fatalf("payload=%q, want %q", outFrame.Payload, "hello")
	}
}

func TestUDPWebSocketServer_RelaysV2IPv4WhenNegotiated(t *testing.T) {
	echo, echoPort := startUDPEchoServer(t, "udp4", net.IPv4(127, 0, 0, 1))
	defer echo.Close()

	cfg := config.Config{
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()
	relayCfg.PreferV2 = true

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")

	inFrame := udpproto.Frame{
		GuestPort:  1234,
		RemoteIP:   netip.MustParseAddr("127.0.0.1"),
		RemotePort: echoPort,
		Payload:    []byte("hello v2"),
	}
	inPkt, err := udpproto.DefaultCodec.EncodeFrameV2(inFrame)
	if err != nil {
		t.Fatalf("EncodeV2: %v", err)
	}

	if err := c.WriteMessage(websocket.BinaryMessage, inPkt); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}

	outPkt := readWSBinary(t, c, 2*time.Second)

	outFrame, err := udpproto.DefaultCodec.DecodeFrame(outPkt)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if outFrame.Version != 2 {
		t.Fatalf("outFrame.Version=%d, want 2", outFrame.Version)
	}
	if outFrame.GuestPort != inFrame.GuestPort {
		t.Fatalf("outFrame.GuestPort=%d, want %d", outFrame.GuestPort, inFrame.GuestPort)
	}
	if outFrame.RemoteIP != inFrame.RemoteIP || outFrame.RemotePort != inFrame.RemotePort {
		t.Fatalf("remote mismatch: got %s:%d, want %s:%d", outFrame.RemoteIP, outFrame.RemotePort, inFrame.RemoteIP, inFrame.RemotePort)
	}
	if string(outFrame.Payload) != "hello v2" {
		t.Fatalf("payload=%q, want %q", outFrame.Payload, "hello v2")
	}
}

func TestUDPWebSocketServer_RelaysV2IPv6(t *testing.T) {
	echo, echoPort := startUDPEchoServer(t, "udp6", net.IPv6loopback)
	defer echo.Close()

	cfg := config.Config{
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")

	inFrame := udpproto.Frame{
		GuestPort:  1234,
		RemoteIP:   netip.MustParseAddr("::1"),
		RemotePort: echoPort,
		Payload:    []byte("hello ipv6"),
	}
	inPkt, err := udpproto.DefaultCodec.EncodeFrameV2(inFrame)
	if err != nil {
		t.Fatalf("EncodeV2: %v", err)
	}

	if err := c.WriteMessage(websocket.BinaryMessage, inPkt); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}

	outPkt := readWSBinary(t, c, 2*time.Second)

	outFrame, err := udpproto.DefaultCodec.DecodeFrame(outPkt)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if outFrame.Version != 2 {
		t.Fatalf("outFrame.Version=%d, want 2", outFrame.Version)
	}
	if outFrame.GuestPort != inFrame.GuestPort {
		t.Fatalf("outFrame.GuestPort=%d, want %d", outFrame.GuestPort, inFrame.GuestPort)
	}
	if outFrame.RemoteIP != inFrame.RemoteIP || outFrame.RemotePort != inFrame.RemotePort {
		t.Fatalf("remote mismatch: got %s:%d, want %s:%d", outFrame.RemoteIP, outFrame.RemotePort, inFrame.RemoteIP, inFrame.RemotePort)
	}
	if string(outFrame.Payload) != "hello ipv6" {
		t.Fatalf("payload=%q, want %q", outFrame.Payload, "hello ipv6")
	}
}

func TestUDPWebSocketServer_DroppedByPolicyIncrementsMetric(t *testing.T) {
	echo, echoPort := startUDPEchoServer(t, "udp4", net.IPv4(127, 0, 0, 1))
	defer echo.Close()

	cfg := config.Config{
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()

	// Production policy denies 127.0.0.0/8 by default.
	p := policy.NewProductionDestinationPolicy()

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, p, nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")

	in := udpproto.Frame{
		GuestPort:  1234,
		RemoteIP:   netip.MustParseAddr("127.0.0.1"),
		RemotePort: echoPort,
		Payload:    []byte("hello"),
	}
	pkt, err := udpproto.DefaultCodec.EncodeFrameV1(in)
	if err != nil {
		t.Fatalf("EncodeV1: %v", err)
	}

	if err := c.WriteMessage(websocket.BinaryMessage, pkt); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}

	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if m.Get(metrics.UDPWSDroppedDeniedByPolicy) > 0 {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("expected %s metric increment", metrics.UDPWSDroppedDeniedByPolicy)
}

func TestUDPWebSocketServer_RateLimitedIncrementsMetric(t *testing.T) {
	echo, echoPort := startUDPEchoServer(t, "udp4", net.IPv4(127, 0, 0, 1))
	defer echo.Close()

	clk := &ratelimitTestClock{now: time.Unix(0, 0)}
	cfg := config.Config{
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,

		MaxUDPPpsPerSession: 1,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, clk)
	relayCfg := DefaultConfig()

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")

	in := udpproto.Frame{
		GuestPort:  1234,
		RemoteIP:   netip.MustParseAddr("127.0.0.1"),
		RemotePort: echoPort,
		Payload:    []byte("hello"),
	}
	pkt, err := udpproto.DefaultCodec.EncodeFrameV1(in)
	if err != nil {
		t.Fatalf("EncodeV1: %v", err)
	}

	// First datagram is allowed.
	if err := c.WriteMessage(websocket.BinaryMessage, pkt); err != nil {
		t.Fatalf("WriteMessage #1: %v", err)
	}
	_ = readWSBinary(t, c, 2*time.Second)

	// Second datagram at the same fake clock timestamp should be dropped.
	if err := c.WriteMessage(websocket.BinaryMessage, pkt); err != nil {
		t.Fatalf("WriteMessage #2: %v", err)
	}

	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if m.Get(metrics.UDPWSDroppedRateLimited) > 0 {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("expected %s metric increment", metrics.UDPWSDroppedRateLimited)
}

func TestUDPWebSocketServer_QuotaExceededIncrementsMetric(t *testing.T) {
	echo, echoPort := startUDPEchoServer(t, "udp4", net.IPv4(127, 0, 0, 1))
	defer echo.Close()

	cfg := config.Config{
		AuthMode:                        config.AuthModeNone,
		SignalingAuthTimeout:            50 * time.Millisecond,
		MaxSignalingMessageBytes:        64 * 1024,
		MaxUniqueDestinationsPerSession: 1,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")

	in := udpproto.Frame{
		GuestPort:  1234,
		RemoteIP:   netip.MustParseAddr("127.0.0.1"),
		RemotePort: echoPort,
		Payload:    []byte("hello"),
	}
	pkt, err := udpproto.DefaultCodec.EncodeFrameV1(in)
	if err != nil {
		t.Fatalf("EncodeV1: %v", err)
	}

	// First destination is allowed and should echo.
	if err := c.WriteMessage(websocket.BinaryMessage, pkt); err != nil {
		t.Fatalf("WriteMessage #1: %v", err)
	}
	_ = readWSBinary(t, c, 2*time.Second)

	// Second unique destination should exceed MaxUniqueDestinationsPerSession and be dropped.
	otherPort := echoPort + 1
	if otherPort == 0 {
		otherPort = echoPort - 1
	}
	in.RemotePort = otherPort
	pkt2, err := udpproto.DefaultCodec.EncodeFrameV1(in)
	if err != nil {
		t.Fatalf("EncodeV1 #2: %v", err)
	}
	if err := c.WriteMessage(websocket.BinaryMessage, pkt2); err != nil {
		t.Fatalf("WriteMessage #2: %v", err)
	}

	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if m.Get(metrics.UDPWSDroppedQuotaExceeded) > 0 {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("expected %s metric increment", metrics.UDPWSDroppedQuotaExceeded)
}

func TestUDPWebSocketServer_DroppedOversizedIncrementsMetric(t *testing.T) {
	echo, echoPort := startUDPEchoServer(t, "udp4", net.IPv4(127, 0, 0, 1))
	defer echo.Close()

	cfg := config.Config{
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()
	relayCfg.MaxDatagramPayloadBytes = 5

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")
	_ = readWSJSON(t, c, 2*time.Second) // ready

	in := udpproto.Frame{
		GuestPort:  1234,
		RemoteIP:   netip.MustParseAddr("127.0.0.1"),
		RemotePort: echoPort,
		Payload:    []byte("0123456789"), // 10 bytes, exceeds MaxDatagramPayloadBytes=5
	}
	pkt, err := udpproto.DefaultCodec.EncodeFrameV1(in)
	if err != nil {
		t.Fatalf("EncodeV1 oversize: %v", err)
	}

	if err := c.WriteMessage(websocket.BinaryMessage, pkt); err != nil {
		t.Fatalf("WriteMessage oversize: %v", err)
	}

	// A valid frame should still be processed after the oversized drop.
	in.Payload = []byte("hello") // exactly 5 bytes
	pkt2, err := udpproto.DefaultCodec.EncodeFrameV1(in)
	if err != nil {
		t.Fatalf("EncodeV1 valid: %v", err)
	}
	if err := c.WriteMessage(websocket.BinaryMessage, pkt2); err != nil {
		t.Fatalf("WriteMessage valid: %v", err)
	}
	outPkt := readWSBinary(t, c, 2*time.Second)
	outFrame, err := udpproto.DefaultCodec.DecodeFrame(outPkt)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if string(outFrame.Payload) != "hello" {
		t.Fatalf("payload=%q, want %q", outFrame.Payload, "hello")
	}

	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if m.Get(metrics.UDPWSDroppedOversized) > 0 && m.Get(metrics.UDPWSDropped) > 0 {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("expected %s/%s metric increment; got dropped=%d oversized=%d",
		metrics.UDPWSDropped,
		metrics.UDPWSDroppedOversized,
		m.Get(metrics.UDPWSDropped),
		m.Get(metrics.UDPWSDroppedOversized),
	)
}

func TestUDPWebSocketServer_FramesInOutMetrics(t *testing.T) {
	echo, echoPort := startUDPEchoServer(t, "udp4", net.IPv4(127, 0, 0, 1))
	defer echo.Close()

	cfg := config.Config{
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")

	in := udpproto.Frame{
		GuestPort:  1234,
		RemoteIP:   netip.MustParseAddr("127.0.0.1"),
		RemotePort: echoPort,
		Payload:    []byte("hello"),
	}
	pkt, err := udpproto.DefaultCodec.EncodeFrameV1(in)
	if err != nil {
		t.Fatalf("EncodeV1: %v", err)
	}

	if err := c.WriteMessage(websocket.BinaryMessage, pkt); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}

	_ = readWSBinary(t, c, 2*time.Second)

	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if m.Get(metrics.UDPWSDatagramsIn) > 0 && m.Get(metrics.UDPWSDatagramsOut) > 0 {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf(
		"expected %s and %s metric increments (got in=%d out=%d)",
		metrics.UDPWSDatagramsIn,
		metrics.UDPWSDatagramsOut,
		m.Get(metrics.UDPWSDatagramsIn),
		m.Get(metrics.UDPWSDatagramsOut),
	)
}

func TestUDPWebSocketServer_AuthMessageRequired(t *testing.T) {
	cfg := config.Config{
		AuthMode:                 config.AuthModeAPIKey,
		APIKey:                   "secret",
		SignalingAuthTimeout:     2 * time.Second,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")

	if err := c.WriteMessage(websocket.BinaryMessage, []byte{0x00}); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}

	msg := readWSJSON(t, c, 500*time.Millisecond)
	if msg["type"] != "error" {
		t.Fatalf("expected error message, got %#v", msg)
	}
	if msg["code"] != "unauthorized" {
		t.Fatalf("expected unauthorized code, got %#v", msg)
	}

	_ = c.SetReadDeadline(time.Now().Add(500 * time.Millisecond))
	_, _, err = c.ReadMessage()
	if err == nil {
		t.Fatalf("expected close error")
	}
	if !websocket.IsCloseError(err, websocket.ClosePolicyViolation) {
		t.Fatalf("expected policy violation close; got %v", err)
	}
	if m.Get(metrics.AuthFailure) == 0 {
		t.Fatalf("expected auth_failure metric increment")
	}
}

func TestUDPWebSocketServer_AuthTimeoutClosesWithoutAuthMessage(t *testing.T) {
	cfg := config.Config{
		AuthMode:                 config.AuthModeAPIKey,
		APIKey:                   "secret",
		SignalingAuthTimeout:     200 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")

	msg := readWSJSON(t, c, 2*time.Second)
	if msg["type"] != "error" {
		t.Fatalf("expected error message, got %#v", msg)
	}
	if msg["code"] != "unauthorized" {
		t.Fatalf("expected unauthorized code, got %#v", msg)
	}
	if msg["message"] != "authentication timeout" {
		t.Fatalf("expected authentication timeout message, got %#v", msg)
	}

	_ = c.SetReadDeadline(time.Now().Add(500 * time.Millisecond))
	_, _, err = c.ReadMessage()
	if err == nil {
		t.Fatalf("expected close error")
	}
	if !websocket.IsCloseError(err, websocket.ClosePolicyViolation) {
		t.Fatalf("expected policy violation close; got %v", err)
	}
	if m.Get(metrics.AuthFailure) == 0 {
		t.Fatalf("expected auth_failure metric increment")
	}
}

func TestUDPWebSocketServer_AuthMessageThenRelay(t *testing.T) {
	echo, echoPort := startUDPEchoServer(t, "udp4", net.IPv4(127, 0, 0, 1))
	defer echo.Close()

	cfg := config.Config{
		AuthMode:                 config.AuthModeAPIKey,
		APIKey:                   "secret",
		SignalingAuthTimeout:     2 * time.Second,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()
	relayCfg.PreferV2 = true

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")

	if err := c.WriteMessage(websocket.TextMessage, []byte(`{"type":"auth","apiKey":"secret"}`)); err != nil {
		t.Fatalf("WriteMessage auth: %v", err)
	}

	ready := readWSJSON(t, c, 2*time.Second)
	if ready["type"] != "ready" {
		t.Fatalf("expected ready message, got %#v", ready)
	}

	inFrame := udpproto.Frame{
		GuestPort:  1234,
		RemoteIP:   netip.MustParseAddr("127.0.0.1"),
		RemotePort: echoPort,
		Payload:    []byte("hello after auth"),
	}
	inPkt, err := udpproto.DefaultCodec.EncodeFrameV1(inFrame)
	if err != nil {
		t.Fatalf("EncodeV1: %v", err)
	}

	if err := c.WriteMessage(websocket.BinaryMessage, inPkt); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}

	outPkt := readWSBinary(t, c, 2*time.Second)

	outFrame, err := udpproto.DefaultCodec.DecodeFrame(outPkt)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if outFrame.GuestPort != inFrame.GuestPort {
		t.Fatalf("guest port mismatch: %d != %d", outFrame.GuestPort, inFrame.GuestPort)
	}
	if string(outFrame.Payload) != "hello after auth" {
		t.Fatalf("payload=%q, want %q", outFrame.Payload, "hello after auth")
	}
}

func TestUDPWebSocketServer_AuthMessageRejectsMismatchedKeys(t *testing.T) {
	cfg := config.Config{
		AuthMode:                 config.AuthModeAPIKey,
		APIKey:                   "secret",
		SignalingAuthTimeout:     2 * time.Second,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")
	if err := c.WriteMessage(websocket.TextMessage, []byte(`{"type":"auth","token":"t1","apiKey":"t2"}`)); err != nil {
		t.Fatalf("WriteMessage auth: %v", err)
	}

	msg := readWSJSON(t, c, 500*time.Millisecond)
	if msg["type"] != "error" {
		t.Fatalf("expected error message, got %#v", msg)
	}
	if msg["code"] != "bad_message" {
		t.Fatalf("expected bad_message code, got %#v", msg)
	}

	_ = c.SetReadDeadline(time.Now().Add(500 * time.Millisecond))
	_, _, err = c.ReadMessage()
	if err == nil {
		t.Fatalf("expected close error")
	}
	if !websocket.IsCloseError(err, websocket.ClosePolicyViolation) {
		t.Fatalf("expected policy violation close; got %v", err)
	}
	if m.Get(metrics.AuthFailure) == 0 {
		t.Fatalf("expected auth_failure metric increment")
	}
}

func TestUDPWebSocketServer_IgnoresRedundantAuthMessage(t *testing.T) {
	echo, echoPort := startUDPEchoServer(t, "udp4", net.IPv4(127, 0, 0, 1))
	defer echo.Close()

	cfg := config.Config{
		AuthMode:                 config.AuthModeAPIKey,
		APIKey:                   "secret",
		SignalingAuthTimeout:     2 * time.Second,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()
	relayCfg.PreferV2 = true

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	// Authenticate via query-string, then send an auth message anyway.
	c := dialWS(t, ts.URL, "/udp?apiKey=secret")
	ready := readWSJSON(t, c, 2*time.Second)
	if ready["type"] != "ready" {
		t.Fatalf("expected ready message, got %#v", ready)
	}
	if err := c.WriteMessage(websocket.TextMessage, []byte(`{"type":"auth","apiKey":"secret"}`)); err != nil {
		t.Fatalf("WriteMessage auth: %v", err)
	}

	in := udpproto.Frame{
		GuestPort:  1234,
		RemoteIP:   netip.MustParseAddr("127.0.0.1"),
		RemotePort: echoPort,
		Payload:    []byte("hello"),
	}
	pkt, err := udpproto.DefaultCodec.EncodeFrameV1(in)
	if err != nil {
		t.Fatalf("EncodeV1: %v", err)
	}

	if err := c.WriteMessage(websocket.BinaryMessage, pkt); err != nil {
		t.Fatalf("WriteMessage datagram: %v", err)
	}

	outPkt := readWSBinary(t, c, 2*time.Second)

	outFrame, err := udpproto.DefaultCodec.DecodeFrame(outPkt)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if string(outFrame.Payload) != "hello" {
		t.Fatalf("payload=%q, want %q", outFrame.Payload, "hello")
	}
}

func TestUDPWebSocketServer_QueryTokenAlias(t *testing.T) {
	echo, echoPort := startUDPEchoServer(t, "udp4", net.IPv4(127, 0, 0, 1))
	defer echo.Close()

	cfg := config.Config{
		AuthMode:                 config.AuthModeAPIKey,
		APIKey:                   "secret",
		SignalingAuthTimeout:     2 * time.Second,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()
	relayCfg.PreferV2 = true

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	// Query-string auth with token works in api_key mode for compatibility.
	c := dialWS(t, ts.URL, "/udp?token=secret")

	in := udpproto.Frame{
		GuestPort:  1234,
		RemoteIP:   netip.MustParseAddr("127.0.0.1"),
		RemotePort: echoPort,
		Payload:    []byte("hello via token alias"),
	}
	pkt, err := udpproto.DefaultCodec.EncodeFrameV1(in)
	if err != nil {
		t.Fatalf("EncodeV1: %v", err)
	}

	if err := c.WriteMessage(websocket.BinaryMessage, pkt); err != nil {
		t.Fatalf("WriteMessage datagram: %v", err)
	}

	outPkt := readWSBinary(t, c, 2*time.Second)

	outFrame, err := udpproto.DefaultCodec.DecodeFrame(outPkt)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if string(outFrame.Payload) != "hello via token alias" {
		t.Fatalf("payload=%q, want %q", outFrame.Payload, "hello via token alias")
	}
}

func TestUDPWebSocketServer_RecordsBackpressureDrops(t *testing.T) {
	echo, echoPort := startUDPEchoServer(t, "udp4", net.IPv4(127, 0, 0, 1))
	defer echo.Close()

	cfg := config.Config{
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)

	// Ensure the outbound send queue can't fit even a single UDP frame so we can
	// deterministically force backpressure drops.
	relayCfg := DefaultConfig()
	relayCfg.DataChannelSendQueueBytes = 1

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")

	in := udpproto.Frame{
		GuestPort:  1234,
		RemoteIP:   netip.MustParseAddr("127.0.0.1"),
		RemotePort: echoPort,
		Payload:    []byte("hello"),
	}
	pkt, err := udpproto.DefaultCodec.EncodeFrameV1(in)
	if err != nil {
		t.Fatalf("EncodeV1: %v", err)
	}
	if err := c.WriteMessage(websocket.BinaryMessage, pkt); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}

	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if m.Get(metrics.UDPWSDroppedBackpressure) > 0 {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("expected %s metric increment", metrics.UDPWSDroppedBackpressure)
}

func TestUDPWebSocketServer_OriginChecks(t *testing.T) {
	cfg := config.Config{
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/udp"

	t.Run("rejects cross-origin by default", func(t *testing.T) {
		h := http.Header{}
		h.Set("Origin", "https://evil.example.com")
		_, resp, err := websocket.DefaultDialer.Dial(wsURL, h)
		if resp != nil && resp.Body != nil {
			resp.Body.Close()
		}
		if err == nil {
			t.Fatalf("expected dial error")
		}
		if resp == nil || resp.StatusCode != http.StatusForbidden {
			status := 0
			if resp != nil {
				status = resp.StatusCode
			}
			t.Fatalf("status=%d, want %d (err=%v)", status, http.StatusForbidden, err)
		}
	})

	t.Run("rejects multiple Origin headers", func(t *testing.T) {
		h := http.Header{}
		h.Add("Origin", ts.URL)
		h.Add("Origin", "https://evil.example.com")
		_, resp, err := websocket.DefaultDialer.Dial(wsURL, h)
		if resp != nil && resp.Body != nil {
			resp.Body.Close()
		}
		if err == nil {
			t.Fatalf("expected dial error")
		}
		if resp == nil || resp.StatusCode != http.StatusForbidden {
			status := 0
			if resp != nil {
				status = resp.StatusCode
			}
			t.Fatalf("status=%d, want %d (err=%v)", status, http.StatusForbidden, err)
		}
	})

	t.Run("allows same-origin", func(t *testing.T) {
		h := http.Header{}
		h.Set("Origin", ts.URL)
		c, resp, err := websocket.DefaultDialer.Dial(wsURL, h)
		if err != nil {
			if resp != nil && resp.Body != nil {
				resp.Body.Close()
			}
			t.Fatalf("dial: %v", err)
		}
		t.Cleanup(func() { _ = c.Close() })
	})
}

func TestUDPWebSocketServer_OriginChecks_AllowsConfiguredOrigin(t *testing.T) {
	cfg := config.Config{
		AuthMode:                 config.AuthModeNone,
		AllowedOrigins:           []string{"https://app.example.com"},
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/udp"

	h := http.Header{}
	h.Set("Origin", "https://app.example.com")
	c, resp, err := websocket.DefaultDialer.Dial(wsURL, h)
	if err != nil {
		if resp != nil && resp.Body != nil {
			resp.Body.Close()
		}
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })
}

func TestUDPWebSocketServer_NonWebSocketRequestReturnsJSONError(t *testing.T) {
	cfg := config.Config{
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
	}

	srv, err := NewUDPWebSocketServer(cfg, nil, DefaultConfig(), policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	resp, err := http.Get(ts.URL + "/udp")
	if err != nil {
		t.Fatalf("http.Get: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("status=%d, want %d", resp.StatusCode, http.StatusBadRequest)
	}
	if ct := resp.Header.Get("Content-Type"); !strings.Contains(ct, "application/json") {
		t.Fatalf("content-type=%q, want application/json", ct)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatalf("ReadAll: %v", err)
	}
	var out map[string]any
	if err := json.Unmarshal(body, &out); err != nil {
		t.Fatalf("unmarshal: %v (body=%q)", err, body)
	}
	if out["code"] != "bad_message" {
		t.Fatalf("code=%#v, want %q (body=%q)", out["code"], "bad_message", body)
	}
}

func TestUDPWebSocketServer_ForbiddenOriginReturnsJSONError(t *testing.T) {
	cfg := config.Config{
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
	}

	srv, err := NewUDPWebSocketServer(cfg, nil, DefaultConfig(), policy.NewDevDestinationPolicy(), nil)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/udp"

	h := http.Header{}
	h.Set("Origin", "https://evil.example.com")
	_, resp, err := websocket.DefaultDialer.Dial(wsURL, h)
	if err == nil {
		if resp != nil && resp.Body != nil {
			resp.Body.Close()
		}
		t.Fatalf("expected dial error")
	}
	if resp == nil {
		t.Fatalf("expected HTTP response with status %d", http.StatusForbidden)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusForbidden {
		t.Fatalf("status=%d, want %d (err=%v)", resp.StatusCode, http.StatusForbidden, err)
	}
	if ct := resp.Header.Get("Content-Type"); !strings.Contains(ct, "application/json") {
		t.Fatalf("content-type=%q, want application/json", ct)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatalf("ReadAll: %v", err)
	}
	var out map[string]any
	if err := json.Unmarshal(body, &out); err != nil {
		t.Fatalf("unmarshal: %v (body=%q)", err, body)
	}
	if out["code"] != "forbidden" {
		t.Fatalf("code=%#v, want %q (body=%q)", out["code"], "forbidden", body)
	}
}
