package httpserver

import (
	"context"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"io"
	"log/slog"
	"net"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

func startUDPEchoServer(t *testing.T) (*net.UDPConn, *net.UDPAddr) {
	t.Helper()

	conn, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 0})
	if err != nil {
		t.Fatalf("ListenUDP: %v", err)
	}
	addr := conn.LocalAddr().(*net.UDPAddr)

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

	return conn, addr
}

func registerUDPWS(t *testing.T, cfg config.Config, srv *server) {
	t.Helper()

	destPolicy, err := policy.NewDestinationPolicyFromEnv()
	if err != nil {
		t.Fatalf("NewDestinationPolicyFromEnv: %v", err)
	}

	sessionMgr := relay.NewSessionManager(cfg, nil, nil)
	relayCfg := relay.Config{
		MaxUDPBindingsPerSession:  cfg.MaxUDPBindingsPerSession,
		UDPBindingIdleTimeout:     cfg.UDPBindingIdleTimeout,
		UDPReadBufferBytes:        cfg.UDPReadBufferBytes,
		DataChannelSendQueueBytes: cfg.DataChannelSendQueueBytes,
		MaxDatagramPayloadBytes:   cfg.MaxDatagramPayloadBytes,
		L2BackendWSURL:            cfg.L2BackendWSURL,
		L2MaxMessageBytes:         cfg.L2MaxMessageBytes,
		PreferV2:                  cfg.PreferV2,
	}

	udpWS, err := relay.NewUDPWebSocketServer(cfg, sessionMgr, relayCfg, destPolicy, srv.log)
	if err != nil {
		t.Fatalf("NewUDPWebSocketServer: %v", err)
	}
	srv.Mux().Handle("GET /udp", udpWS)
}

func readWSJSON(t *testing.T, c *websocket.Conn) map[string]any {
	t.Helper()

	_, msg, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("ReadMessage: %v", err)
	}
	var out map[string]any
	if err := json.Unmarshal(msg, &out); err != nil {
		t.Fatalf("unmarshal: %v", err)
	}
	return out
}

func readWSFrame(t *testing.T, c *websocket.Conn) udpproto.Frame {
	t.Helper()

	deadline := time.Now().Add(2 * time.Second)
	for {
		_ = c.SetReadDeadline(deadline)
		msgType, msg, err := c.ReadMessage()
		if err != nil {
			t.Fatalf("ReadMessage: %v", err)
		}
		if msgType != websocket.BinaryMessage {
			// Ignore control messages like {"type":"ready"}.
			continue
		}
		f, err := udpproto.DefaultCodec.DecodeFrame(msg)
		if err != nil {
			t.Fatalf("DecodeFrame: %v", err)
		}
		return f
	}
}

func TestUDPWebSocket_RoundTripV1AndV2(t *testing.T) {
	t.Setenv("DESTINATION_POLICY_PRESET", "dev")

	echoConn, echoAddr := startUDPEchoServer(t)
	defer echoConn.Close()

	cfg := config.Config{
		ListenAddr:               "127.0.0.1:0",
		ShutdownTimeout:          2 * time.Second,
		Mode:                     config.ModeDev,
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     2 * time.Second,
		MaxSignalingMessageBytes: 64 * 1024,
		PreferV2:                 true,
	}

	baseURL := startTestServer(t, cfg, func(srv *server) {
		registerUDPWS(t, cfg, srv)
	})

	wsURL := "ws" + strings.TrimPrefix(baseURL, "http") + "/udp"
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })

	readyMsg := readWSJSON(t, c)
	if readyMsg["type"] != "ready" {
		t.Fatalf("expected ready message, got %#v", readyMsg)
	}

	ip4 := echoAddr.IP.To4()
	if ip4 == nil {
		t.Fatalf("echo server must be ipv4")
	}
	var ip4Arr [4]byte
	copy(ip4Arr[:], ip4)

	pktV1, err := udpproto.DefaultCodec.EncodeDatagram(udpproto.Datagram{
		GuestPort:  1234,
		RemoteIP:   ip4Arr,
		RemotePort: uint16(echoAddr.Port),
		Payload:    []byte("hello-v1"),
	}, nil)
	if err != nil {
		t.Fatalf("EncodeDatagram(v1): %v", err)
	}
	if err := c.WriteMessage(websocket.BinaryMessage, pktV1); err != nil {
		t.Fatalf("WriteMessage(v1): %v", err)
	}

	out1 := readWSFrame(t, c)
	if out1.Version != 1 {
		t.Fatalf("expected v1 response before v2 is negotiated, got v%d", out1.Version)
	}
	if got := string(out1.Payload); got != "hello-v1" {
		t.Fatalf("payload=%q, want %q", got, "hello-v1")
	}

	pktV2, err := udpproto.DefaultCodec.EncodeFrameV2(udpproto.Frame{
		GuestPort:  2345,
		RemoteIP:   echoAddr.AddrPort().Addr(),
		RemotePort: uint16(echoAddr.Port),
		Payload:    []byte("hello-v2"),
	})
	if err != nil {
		t.Fatalf("EncodeV2: %v", err)
	}
	if err := c.WriteMessage(websocket.BinaryMessage, pktV2); err != nil {
		t.Fatalf("WriteMessage(v2): %v", err)
	}

	out2 := readWSFrame(t, c)
	if out2.Version != 2 {
		t.Fatalf("expected v2 response after v2 negotiation, got v%d", out2.Version)
	}
	if got := string(out2.Payload); got != "hello-v2" {
		t.Fatalf("payload=%q, want %q", got, "hello-v2")
	}
}

func TestUDPWebSocket_APIKeyAuth(t *testing.T) {
	t.Setenv("DESTINATION_POLICY_PRESET", "dev")

	echoConn, echoAddr := startUDPEchoServer(t)
	defer echoConn.Close()

	cfg := config.Config{
		ListenAddr:               "127.0.0.1:0",
		ShutdownTimeout:          2 * time.Second,
		Mode:                     config.ModeDev,
		AuthMode:                 config.AuthModeAPIKey,
		APIKey:                   "secret",
		SignalingAuthTimeout:     200 * time.Millisecond,
		MaxSignalingMessageBytes: 64 * 1024,
	}

	baseURL := startTestServer(t, cfg, func(srv *server) {
		registerUDPWS(t, cfg, srv)
	})

	t.Run("unauthenticated rejected", func(t *testing.T) {
		wsURL := "ws" + strings.TrimPrefix(baseURL, "http") + "/udp"
		c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
		if err != nil {
			t.Fatalf("dial: %v", err)
		}
		t.Cleanup(func() { _ = c.Close() })

		_ = c.SetReadDeadline(time.Now().Add(2 * time.Second))
		msg := readWSJSON(t, c)
		if msg["type"] != "error" {
			t.Fatalf("expected error message, got %#v", msg)
		}
		if msg["code"] != "unauthorized" {
			t.Fatalf("expected unauthorized code, got %#v", msg)
		}
	})

	t.Run("first message auth succeeds", func(t *testing.T) {
		wsURL := "ws" + strings.TrimPrefix(baseURL, "http") + "/udp"
		c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
		if err != nil {
			t.Fatalf("dial: %v", err)
		}
		t.Cleanup(func() { _ = c.Close() })

		if err := c.WriteJSON(map[string]any{"type": "auth", "apiKey": "secret"}); err != nil {
			t.Fatalf("WriteJSON(auth): %v", err)
		}

		readyMsg := readWSJSON(t, c)
		if readyMsg["type"] != "ready" {
			t.Fatalf("expected ready message, got %#v", readyMsg)
		}

		ip4 := echoAddr.IP.To4()
		var ip4Arr [4]byte
		copy(ip4Arr[:], ip4)
		pkt, err := udpproto.DefaultCodec.EncodeDatagram(udpproto.Datagram{
			GuestPort:  1234,
			RemoteIP:   ip4Arr,
			RemotePort: uint16(echoAddr.Port),
			Payload:    []byte("auth-ok"),
		}, nil)
		if err != nil {
			t.Fatalf("EncodeDatagram: %v", err)
		}
		if err := c.WriteMessage(websocket.BinaryMessage, pkt); err != nil {
			t.Fatalf("WriteMessage: %v", err)
		}

		out := readWSFrame(t, c)
		if got := string(out.Payload); got != "auth-ok" {
			t.Fatalf("payload=%q, want %q", got, "auth-ok")
		}
	})
}

func TestUDPWebSocket_JWTAuth_QueryParam(t *testing.T) {
	t.Setenv("DESTINATION_POLICY_PRESET", "dev")

	echoConn, echoAddr := startUDPEchoServer(t)
	defer echoConn.Close()

	cfg := config.Config{
		ListenAddr:               "127.0.0.1:0",
		ShutdownTimeout:          2 * time.Second,
		Mode:                     config.ModeDev,
		AuthMode:                 config.AuthModeJWT,
		JWTSecret:                "secret",
		SignalingAuthTimeout:     2 * time.Second,
		MaxSignalingMessageBytes: 64 * 1024,
	}

	baseURL := startTestServer(t, cfg, func(srv *server) {
		registerUDPWS(t, cfg, srv)
	})

	now := time.Now()
	token := mustHS256JWT(t, "secret", map[string]any{
		"iat": now.Unix(),
		"exp": now.Add(5 * time.Minute).Unix(),
		"sid": "sess_test",
	})
	wsURL := "ws" + strings.TrimPrefix(baseURL, "http") + "/udp?token=" + token
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })

	readyMsg := readWSJSON(t, c)
	if readyMsg["type"] != "ready" {
		t.Fatalf("expected ready message, got %#v", readyMsg)
	}

	ip4 := echoAddr.IP.To4()
	var ip4Arr [4]byte
	copy(ip4Arr[:], ip4)
	pkt, err := udpproto.DefaultCodec.EncodeDatagram(udpproto.Datagram{
		GuestPort:  1234,
		RemoteIP:   ip4Arr,
		RemotePort: uint16(echoAddr.Port),
		Payload:    []byte("jwt-ok"),
	}, nil)
	if err != nil {
		t.Fatalf("EncodeDatagram: %v", err)
	}
	if err := c.WriteMessage(websocket.BinaryMessage, pkt); err != nil {
		t.Fatalf("WriteMessage: %v", err)
	}

	out := readWSFrame(t, c)
	if got := string(out.Payload); got != "jwt-ok" {
		t.Fatalf("payload=%q, want %q", got, "jwt-ok")
	}
}

func mustHS256JWT(t *testing.T, secret string, claims map[string]any) string {
	t.Helper()

	headerJSON, err := json.Marshal(map[string]any{"alg": "HS256", "typ": "JWT"})
	if err != nil {
		t.Fatalf("marshal header: %v", err)
	}
	payloadJSON, err := json.Marshal(claims)
	if err != nil {
		t.Fatalf("marshal claims: %v", err)
	}

	enc := base64.RawURLEncoding
	header := enc.EncodeToString(headerJSON)
	payload := enc.EncodeToString(payloadJSON)
	signingInput := header + "." + payload

	mac := hmac.New(sha256.New, []byte(secret))
	_, _ = mac.Write([]byte(signingInput))
	sig := enc.EncodeToString(mac.Sum(nil))
	return signingInput + "." + sig
}

func TestUDPWebSocket_ShutdownClosesConnections(t *testing.T) {
	// Sanity check that long-lived /udp connections don't hang server shutdown.
	t.Setenv("DESTINATION_POLICY_PRESET", "dev")

	cfg := config.Config{
		ListenAddr:               "127.0.0.1:0",
		ShutdownTimeout:          250 * time.Millisecond,
		Mode:                     config.ModeDev,
		AuthMode:                 config.AuthModeNone,
		SignalingAuthTimeout:     2 * time.Second,
		MaxSignalingMessageBytes: 64 * 1024,
	}

	log := newTestLogger(t)
	srv := New(cfg, log, "abc", "time")
	registerUDPWS(t, cfg, srv)

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	errCh := make(chan error, 1)
	go func() { errCh <- srv.Serve(ln) }()
	t.Cleanup(func() {
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		_ = srv.Shutdown(ctx)
		<-errCh
	})

	baseURL := "http://" + ln.Addr().String()
	wsURL := "ws" + strings.TrimPrefix(baseURL, "http") + "/udp"
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer c.Close()

	_ = readWSJSON(t, c)

	ctx, cancel := context.WithTimeout(context.Background(), cfg.ShutdownTimeout)
	defer cancel()
	if err := srv.Shutdown(ctx); err != nil {
		t.Fatalf("Shutdown: %v", err)
	}
}

func newTestLogger(t *testing.T) *slog.Logger {
	t.Helper()
	return slog.New(slog.NewTextHandler(io.Discard, nil))
}
