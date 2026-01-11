package relay

import (
	"errors"
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

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy())
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

	_ = c.SetReadDeadline(time.Now().Add(2 * time.Second))
	msgType, outPkt, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("ReadMessage: %v", err)
	}
	if msgType != websocket.BinaryMessage {
		t.Fatalf("msgType=%d, want BinaryMessage", msgType)
	}

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

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy())
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

	_ = c.SetReadDeadline(time.Now().Add(2 * time.Second))
	_, outPkt, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("ReadMessage: %v", err)
	}

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

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy())
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

	_ = c.SetReadDeadline(time.Now().Add(500 * time.Millisecond))
	_, _, err = c.ReadMessage()
	if err == nil {
		t.Fatalf("expected close error")
	}
	if !websocket.IsCloseError(err, websocket.ClosePolicyViolation) {
		t.Fatalf("expected policy violation close; got %v", err)
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

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy())
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

	_ = c.SetReadDeadline(time.Now().Add(2 * time.Second))
	_, outPkt, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("ReadMessage: %v", err)
	}

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

func TestUDPWebSocketServer_EnforcesBindingQuota(t *testing.T) {
	echo, echoPort := startUDPEchoServer(t, "udp4", net.IPv4(127, 0, 0, 1))
	defer echo.Close()

	cfg := config.Config{
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     50 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,

		MaxUDPBindingsPerSession: 1,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, nil)
	relayCfg := DefaultConfig()
	relayCfg.PreferV2 = true

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy())
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	c := dialWS(t, ts.URL, "/udp")

	guestPort1 := uint16(1111)
	in1 := udpproto.Frame{
		GuestPort:  guestPort1,
		RemoteIP:   netip.MustParseAddr("127.0.0.1"),
		RemotePort: echoPort,
		Payload:    []byte("first"),
	}
	pkt1, err := udpproto.EncodeV1(in1)
	if err != nil {
		t.Fatalf("EncodeV1: %v", err)
	}
	if err := c.WriteMessage(websocket.BinaryMessage, pkt1); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}
	_ = c.SetReadDeadline(time.Now().Add(2 * time.Second))
	_, out1, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("ReadMessage: %v", err)
	}
	got1, err := udpproto.Decode(out1)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if got1.GuestPort != guestPort1 {
		t.Fatalf("guest port mismatch: %d != %d", got1.GuestPort, guestPort1)
	}

	guestPort2 := uint16(2222)
	in2 := udpproto.Frame{
		GuestPort:  guestPort2,
		RemoteIP:   netip.MustParseAddr("127.0.0.1"),
		RemotePort: echoPort,
		Payload:    []byte("second"),
	}
	pkt2, err := udpproto.EncodeV1(in2)
	if err != nil {
		t.Fatalf("EncodeV1: %v", err)
	}
	if err := c.WriteMessage(websocket.BinaryMessage, pkt2); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}

	_ = c.SetReadDeadline(time.Now().Add(200 * time.Millisecond))
	_, _, err = c.ReadMessage()
	if err == nil {
		t.Fatalf("unexpected response for second guest port (expected quota enforcement)")
	}
	var netErr net.Error
	if !errors.As(err, &netErr) || !netErr.Timeout() {
		t.Fatalf("expected read timeout waiting for dropped packet; got %v", err)
	}

	if m.Get(metrics.DropReasonQuotaExceeded) == 0 || m.Get("too_many_bindings") == 0 {
		t.Fatalf("expected quota exceeded metrics for binding limit")
	}
	if m.Get("udp_ws_dropped_rate_limit") == 0 {
		t.Fatalf("expected udp_ws_dropped_rate_limit metric increment")
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

	srv, err := NewUDPWebSocketServer(cfg, sm, relayCfg, policy.NewDevDestinationPolicy())
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}

	mux := http.NewServeMux()
	mux.Handle("GET /udp", srv)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	// Authenticate via query-string, then send an auth message anyway.
	c := dialWS(t, ts.URL, "/udp?apiKey=secret")
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

	_ = c.SetReadDeadline(time.Now().Add(2 * time.Second))
	_, outPkt, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("ReadMessage: %v", err)
	}

	outFrame, err := udpproto.Decode(outPkt)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if string(outFrame.Payload) != "hello" {
		t.Fatalf("payload=%q, want %q", outFrame.Payload, "hello")
	}
}
