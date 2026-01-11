package relay

import (
	"encoding/json"
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
	pkt, err := udpproto.EncodeV1(in)
	if err != nil {
		t.Fatalf("EncodeV1: %v", err)
	}

	if err := c.WriteMessage(websocket.BinaryMessage, pkt); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}

	outPkt := readWSBinary(t, c, 2*time.Second)

	outFrame, err := udpproto.Decode(outPkt)
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
	inPkt, err := udpproto.EncodeV2(inFrame)
	if err != nil {
		t.Fatalf("EncodeV2: %v", err)
	}

	if err := c.WriteMessage(websocket.BinaryMessage, inPkt); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}

	outPkt := readWSBinary(t, c, 2*time.Second)

	outFrame, err := udpproto.Decode(outPkt)
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
	inPkt, err := udpproto.EncodeV2(inFrame)
	if err != nil {
		t.Fatalf("EncodeV2: %v", err)
	}

	if err := c.WriteMessage(websocket.BinaryMessage, inPkt); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}

	outPkt := readWSBinary(t, c, 2*time.Second)

	outFrame, err := udpproto.Decode(outPkt)
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
	pkt, err := udpproto.EncodeV1(in)
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
	pkt, err := udpproto.EncodeV1(in)
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
	pkt, err := udpproto.EncodeV1(in)
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
	inPkt, err := udpproto.EncodeV1(inFrame)
	if err != nil {
		t.Fatalf("EncodeV1: %v", err)
	}

	if err := c.WriteMessage(websocket.BinaryMessage, inPkt); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}

	outPkt := readWSBinary(t, c, 2*time.Second)

	outFrame, err := udpproto.Decode(outPkt)
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
	pkt, err := udpproto.EncodeV1(in)
	if err != nil {
		t.Fatalf("EncodeV1: %v", err)
	}

	if err := c.WriteMessage(websocket.BinaryMessage, pkt); err != nil {
		t.Fatalf("WriteMessage datagram: %v", err)
	}

	outPkt := readWSBinary(t, c, 2*time.Second)

	outFrame, err := udpproto.Decode(outPkt)
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
	pkt, err := udpproto.EncodeV1(in)
	if err != nil {
		t.Fatalf("EncodeV1: %v", err)
	}

	if err := c.WriteMessage(websocket.BinaryMessage, pkt); err != nil {
		t.Fatalf("WriteMessage datagram: %v", err)
	}

	outPkt := readWSBinary(t, c, 2*time.Second)

	outFrame, err := udpproto.Decode(outPkt)
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
	pkt, err := udpproto.EncodeV1(in)
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
