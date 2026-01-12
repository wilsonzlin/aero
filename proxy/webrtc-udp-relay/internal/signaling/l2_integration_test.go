package signaling_test

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net"
	"net/http"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gorilla/websocket"
	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/httpserver"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/l2tunnel"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/signaling"
)

func startTestL2Backend(t *testing.T) (wsURL string, upgradeCount *atomic.Int64) {
	return startTestL2BackendWithToken(t, "")
}

func startTestL2BackendWithToken(t *testing.T, token string) (wsURL string, upgradeCount *atomic.Int64) {
	t.Helper()

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen backend: %v", err)
	}

	var upgrades atomic.Int64

	upgrader := websocket.Upgrader{
		CheckOrigin:  func(r *http.Request) bool { return true },
		Subprotocols: []string{l2tunnel.Subprotocol},
	}

	mux := http.NewServeMux()
	mux.HandleFunc("GET /l2", func(w http.ResponseWriter, r *http.Request) {
		if token != "" {
			want := l2tunnel.TokenSubprotocolPrefix + token
			ok := false
			for _, raw := range strings.Split(r.Header.Get("Sec-WebSocket-Protocol"), ",") {
				if strings.TrimSpace(raw) == want {
					ok = true
					break
				}
			}
			if !ok {
				http.Error(w, "missing or invalid token", http.StatusUnauthorized)
				return
			}
		}

		upgrades.Add(1)
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		defer conn.Close()

		if conn.Subprotocol() != l2tunnel.Subprotocol {
			_ = conn.WriteControl(websocket.CloseMessage, websocket.FormatCloseMessage(websocket.CloseProtocolError, "missing subprotocol"), time.Now().Add(time.Second))
			return
		}

		for {
			msgType, payload, err := conn.ReadMessage()
			if err != nil {
				return
			}
			if msgType != websocket.BinaryMessage {
				continue
			}
			// Minimal subset of docs/l2-tunnel-protocol.md: PING (0xA2 0x03 0x01 0x00) -> PONG.
			if len(payload) < l2tunnel.HeaderLen || payload[0] != l2tunnel.Magic || payload[1] != l2tunnel.Version || payload[2] != l2tunnel.TypePing {
				continue
			}
			out := append([]byte(nil), payload...)
			out[2] = l2tunnel.TypePong
			_ = conn.WriteMessage(websocket.BinaryMessage, out)
		}
	})

	srv := &http.Server{
		Handler:           mux,
		ReadHeaderTimeout: 5 * time.Second,
	}

	errCh := make(chan error, 1)
	go func() {
		errCh <- srv.Serve(ln)
	}()
	t.Cleanup(func() {
		_ = srv.Close()
		<-errCh
	})

	return "ws://" + ln.Addr().String() + "/l2", &upgrades
}

type l2BackendDialInfo struct {
	Origin            string
	TokenQuery        string
	SubprotocolHeader string
	Cookie            string
}

func startTestL2BackendWithQueryToken(t *testing.T, expectedToken string) (wsURL string, upgradeCount *atomic.Int64, dialInfo *atomic.Value) {
	t.Helper()

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen backend: %v", err)
	}

	var upgrades atomic.Int64
	var info atomic.Value
	info.Store(l2BackendDialInfo{})

	upgrader := websocket.Upgrader{
		CheckOrigin:  func(r *http.Request) bool { return true },
		Subprotocols: []string{l2tunnel.Subprotocol},
	}

	mux := http.NewServeMux()
	mux.HandleFunc("GET /l2", func(w http.ResponseWriter, r *http.Request) {
		info.Store(l2BackendDialInfo{
			Origin:            strings.TrimSpace(r.Header.Get("Origin")),
			TokenQuery:        r.URL.Query().Get("token"),
			SubprotocolHeader: r.Header.Get("Sec-WebSocket-Protocol"),
			Cookie:            strings.TrimSpace(r.Header.Get("Cookie")),
		})

		if expectedToken != "" && r.URL.Query().Get("token") != expectedToken {
			http.Error(w, "missing or invalid token", http.StatusUnauthorized)
			return
		}

		upgrades.Add(1)
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		defer conn.Close()

		if conn.Subprotocol() != l2tunnel.Subprotocol {
			_ = conn.WriteControl(websocket.CloseMessage, websocket.FormatCloseMessage(websocket.CloseProtocolError, "missing subprotocol"), time.Now().Add(time.Second))
			return
		}

		for {
			msgType, payload, err := conn.ReadMessage()
			if err != nil {
				return
			}
			if msgType != websocket.BinaryMessage {
				continue
			}
			// Minimal subset of docs/l2-tunnel-protocol.md: PING (0xA2 0x03 0x01 0x00) -> PONG.
			if len(payload) < l2tunnel.HeaderLen || payload[0] != l2tunnel.Magic || payload[1] != l2tunnel.Version || payload[2] != l2tunnel.TypePing {
				continue
			}
			out := append([]byte(nil), payload...)
			out[2] = l2tunnel.TypePong
			_ = conn.WriteMessage(websocket.BinaryMessage, out)
		}
	})

	srv := &http.Server{
		Handler:           mux,
		ReadHeaderTimeout: 5 * time.Second,
	}

	errCh := make(chan error, 1)
	go func() {
		errCh <- srv.Serve(ln)
	}()
	t.Cleanup(func() {
		_ = srv.Close()
		<-errCh
	})

	return "ws://" + ln.Addr().String() + "/l2", &upgrades, &info
}

func startTestRelayServer(t *testing.T, relayCfg relay.Config, destPolicy *policy.DestinationPolicy) (baseURL string) {
	t.Helper()

	cfg := config.Config{
		ListenAddr:      "127.0.0.1:0",
		LogFormat:       config.LogFormatText,
		LogLevel:        slog.LevelInfo,
		ShutdownTimeout: 2 * time.Second,
		Mode:            config.ModeDev,
	}
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	httpSrv := httpserver.New(cfg, log, httpserver.BuildInfo{})

	sessionMgr := relay.NewSessionManager(cfg, nil, nil)
	signalingSrv := signaling.NewServer(signaling.Config{
		Sessions:    sessionMgr,
		WebRTC:      webrtc.NewAPI(),
		ICEServers:  cfg.ICEServers,
		RelayConfig: relayCfg,
		Policy:      destPolicy,
	})
	signalingSrv.RegisterRoutes(httpSrv.Mux())

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen http: %v", err)
	}
	errCh := make(chan error, 1)
	go func() {
		errCh <- httpSrv.Serve(ln)
	}()
	t.Cleanup(func() {
		signalingSrv.Close()
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		_ = httpSrv.Shutdown(ctx)
		<-errCh
	})

	return "http://" + ln.Addr().String()
}

func startTestRelayServerWithAuth(t *testing.T, relayCfg relay.Config, destPolicy *policy.DestinationPolicy, authMode config.AuthMode, apiKey string) (baseURL string) {
	t.Helper()

	cfg := config.Config{
		ListenAddr:      "127.0.0.1:0",
		LogFormat:       config.LogFormatText,
		LogLevel:        slog.LevelInfo,
		ShutdownTimeout: 2 * time.Second,
		Mode:            config.ModeDev,
		AuthMode:        authMode,
		APIKey:          apiKey,
	}
	log := slog.New(slog.NewTextHandler(io.Discard, nil))
	httpSrv := httpserver.New(cfg, log, httpserver.BuildInfo{})

	sessionMgr := relay.NewSessionManager(cfg, nil, nil)
	authz, err := signaling.NewAuthAuthorizer(cfg)
	if err != nil {
		t.Fatalf("configure auth: %v", err)
	}
	signalingSrv := signaling.NewServer(signaling.Config{
		Sessions:    sessionMgr,
		WebRTC:      webrtc.NewAPI(),
		ICEServers:  cfg.ICEServers,
		RelayConfig: relayCfg,
		Policy:      destPolicy,
		Authorizer:  authz,
	})
	signalingSrv.RegisterRoutes(httpSrv.Mux())

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen http: %v", err)
	}
	errCh := make(chan error, 1)
	go func() {
		errCh <- httpSrv.Serve(ln)
	}()
	t.Cleanup(func() {
		signalingSrv.Close()
		ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		_ = httpSrv.Shutdown(ctx)
		<-errCh
	})

	return "http://" + ln.Addr().String()
}

func exchangeOffer(t *testing.T, baseURL string, pc *webrtc.PeerConnection) {
	exchangeOfferWithHeaders(t, baseURL+"/offer", nil, pc)
}

func exchangeOfferWithHeaders(t *testing.T, offerURL string, headers http.Header, pc *webrtc.PeerConnection) {
	t.Helper()

	offer, err := pc.CreateOffer(nil)
	if err != nil {
		t.Fatalf("create offer: %v", err)
	}
	gatherComplete := webrtc.GatheringCompletePromise(pc)
	if err := pc.SetLocalDescription(offer); err != nil {
		t.Fatalf("set local description: %v", err)
	}
	<-gatherComplete

	type offerRequest struct {
		Version int                       `json:"version"`
		Offer   webrtc.SessionDescription `json:"offer"`
	}
	offerBody, err := json.Marshal(offerRequest{Version: 1, Offer: *pc.LocalDescription()})
	if err != nil {
		t.Fatalf("marshal offer: %v", err)
	}

	req, err := http.NewRequest(http.MethodPost, offerURL, bytes.NewReader(offerBody))
	if err != nil {
		t.Fatalf("build offer request: %v", err)
	}
	req.Header.Set("Content-Type", "application/json")
	for k, vv := range headers {
		for _, v := range vv {
			req.Header.Add(k, v)
		}
	}

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("post offer: %v", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("unexpected status: %s", resp.Status)
	}

	type answerResponse struct {
		Version int                       `json:"version"`
		Answer  webrtc.SessionDescription `json:"answer"`
	}
	var answer answerResponse
	if err := json.NewDecoder(resp.Body).Decode(&answer); err != nil {
		t.Fatalf("decode answer: %v", err)
	}
	if answer.Version != 1 {
		t.Fatalf("unexpected answer version: %d", answer.Version)
	}
	if err := pc.SetRemoteDescription(answer.Answer); err != nil {
		t.Fatalf("set remote description: %v", err)
	}
}

func TestWebRTCUDPRelay_L2TunnelRejectsPartialReliability(t *testing.T) {
	backendURL, upgrades := startTestL2Backend(t)

	relayCfg := relay.DefaultConfig()
	relayCfg.L2BackendWSURL = backendURL
	destPolicy := policy.NewDevDestinationPolicy()
	baseURL := startTestRelayServer(t, relayCfg, destPolicy)

	pc, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peer connection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := true
	maxRetransmits := uint16(0)
	dc, err := pc.CreateDataChannel(l2tunnel.DataChannelLabel, &webrtc.DataChannelInit{
		Ordered:        &ordered,
		MaxRetransmits: &maxRetransmits,
	})
	if err != nil {
		t.Fatalf("create data channel: %v", err)
	}

	closedCh := make(chan struct{})
	dc.OnClose(func() { close(closedCh) })

	exchangeOffer(t, baseURL, pc)

	select {
	case <-closedCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for server to close partially reliable l2 channel")
	}

	if got := upgrades.Load(); got != 0 {
		t.Fatalf("backend websocket should not be dialed for rejected l2 channel (got %d upgrades)", got)
	}
}

func TestWebRTCUDPRelay_L2TunnelRejectsUnordered(t *testing.T) {
	backendURL, upgrades := startTestL2Backend(t)

	relayCfg := relay.DefaultConfig()
	relayCfg.L2BackendWSURL = backendURL
	destPolicy := policy.NewDevDestinationPolicy()
	baseURL := startTestRelayServer(t, relayCfg, destPolicy)

	pc, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peer connection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := false
	dc, err := pc.CreateDataChannel(l2tunnel.DataChannelLabel, &webrtc.DataChannelInit{
		Ordered: &ordered,
	})
	if err != nil {
		t.Fatalf("create data channel: %v", err)
	}

	closedCh := make(chan struct{})
	dc.OnClose(func() { close(closedCh) })

	exchangeOffer(t, baseURL, pc)

	select {
	case <-closedCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for server to close unordered l2 channel")
	}

	if got := upgrades.Load(); got != 0 {
		t.Fatalf("backend websocket should not be dialed for rejected l2 channel (got %d upgrades)", got)
	}
}

func TestWebRTCUDPRelay_L2TunnelPingPongRoundTrip(t *testing.T) {
	backendURL, _ := startTestL2Backend(t)

	relayCfg := relay.DefaultConfig()
	relayCfg.L2BackendWSURL = backendURL
	destPolicy := policy.NewDevDestinationPolicy()
	baseURL := startTestRelayServer(t, relayCfg, destPolicy)

	pc, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peer connection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := true
	dc, err := pc.CreateDataChannel(l2tunnel.DataChannelLabel, &webrtc.DataChannelInit{
		Ordered: &ordered,
	})
	if err != nil {
		t.Fatalf("create data channel: %v", err)
	}

	openCh := make(chan struct{})
	gotCh := make(chan []byte, 1)

	dc.OnOpen(func() { close(openCh) })
	dc.OnMessage(func(msg webrtc.DataChannelMessage) {
		if msg.IsString {
			return
		}
		select {
		case gotCh <- append([]byte(nil), msg.Data...):
		default:
		}
	})

	exchangeOffer(t, baseURL, pc)

	select {
	case <-openCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for l2 datachannel open")
	}

	ping := []byte{l2tunnel.Magic, l2tunnel.Version, l2tunnel.TypePing, 0x00}
	if err := dc.Send(ping); err != nil {
		t.Fatalf("send ping: %v", err)
	}

	var got []byte
	select {
	case got = <-gotCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for pong")
	}

	want := []byte{l2tunnel.Magic, l2tunnel.Version, l2tunnel.TypePong, 0x00}
	if !bytes.Equal(got, want) {
		t.Fatalf("pong mismatch: %x != %x", got, want)
	}
}

func TestWebRTCUDPRelay_L2TunnelBackendTokenViaSubprotocol(t *testing.T) {
	backendURL, upgrades := startTestL2BackendWithToken(t, "sekrit")

	relayCfg := relay.DefaultConfig()
	relayCfg.L2BackendWSURL = backendURL
	relayCfg.L2BackendWSToken = "sekrit"
	destPolicy := policy.NewDevDestinationPolicy()
	baseURL := startTestRelayServer(t, relayCfg, destPolicy)

	pc, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peer connection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := true
	dc, err := pc.CreateDataChannel(l2tunnel.DataChannelLabel, &webrtc.DataChannelInit{
		Ordered: &ordered,
	})
	if err != nil {
		t.Fatalf("create data channel: %v", err)
	}

	openCh := make(chan struct{})
	gotCh := make(chan []byte, 1)

	dc.OnOpen(func() { close(openCh) })
	dc.OnMessage(func(msg webrtc.DataChannelMessage) {
		if msg.IsString {
			return
		}
		select {
		case gotCh <- append([]byte(nil), msg.Data...):
		default:
		}
	})

	exchangeOffer(t, baseURL, pc)

	select {
	case <-openCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for l2 datachannel open")
	}

	ping := []byte{l2tunnel.Magic, l2tunnel.Version, l2tunnel.TypePing, 0x00}
	if err := dc.Send(ping); err != nil {
		t.Fatalf("send ping: %v", err)
	}

	var got []byte
	select {
	case got = <-gotCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for pong")
	}

	want := []byte{l2tunnel.Magic, l2tunnel.Version, l2tunnel.TypePong, 0x00}
	if !bytes.Equal(got, want) {
		t.Fatalf("pong mismatch: %x != %x", got, want)
	}

	if gotUpgrades := upgrades.Load(); gotUpgrades == 0 {
		t.Fatalf("expected relay to dial backend websocket (got %d upgrades)", gotUpgrades)
	}
}

func TestWebRTCUDPRelay_L2TunnelBackendTokenViaQueryForwarding(t *testing.T) {
	backendURL, upgrades, dialInfo := startTestL2BackendWithQueryToken(t, "relay-secret")

	relayCfg := relay.DefaultConfig()
	relayCfg.L2BackendWSURL = backendURL
	relayCfg.L2BackendForwardOrigin = true
	relayCfg.L2BackendAuthForwardMode = config.L2BackendAuthForwardModeQuery

	destPolicy := policy.NewDevDestinationPolicy()
	baseURL := startTestRelayServerWithAuth(t, relayCfg, destPolicy, config.AuthModeAPIKey, "relay-secret")

	pc, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peer connection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := true
	dc, err := pc.CreateDataChannel(l2tunnel.DataChannelLabel, &webrtc.DataChannelInit{
		Ordered: &ordered,
	})
	if err != nil {
		t.Fatalf("create data channel: %v", err)
	}

	openCh := make(chan struct{})
	gotCh := make(chan []byte, 1)

	dc.OnOpen(func() { close(openCh) })
	dc.OnMessage(func(msg webrtc.DataChannelMessage) {
		if msg.IsString {
			return
		}
		select {
		case gotCh <- append([]byte(nil), msg.Data...):
		default:
		}
	})

	headers := http.Header{
		"Origin": []string{baseURL},
	}
	exchangeOfferWithHeaders(t, baseURL+"/offer?token=relay-secret", headers, pc)

	select {
	case <-openCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for l2 datachannel open")
	}

	ping := []byte{l2tunnel.Magic, l2tunnel.Version, l2tunnel.TypePing, 0x00}
	if err := dc.Send(ping); err != nil {
		t.Fatalf("send ping: %v", err)
	}

	var got []byte
	select {
	case got = <-gotCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for pong")
	}

	want := []byte{l2tunnel.Magic, l2tunnel.Version, l2tunnel.TypePong, 0x00}
	if !bytes.Equal(got, want) {
		t.Fatalf("pong mismatch: %x != %x", got, want)
	}

	if gotUpgrades := upgrades.Load(); gotUpgrades == 0 {
		t.Fatalf("expected relay to dial backend websocket (got %d upgrades)", gotUpgrades)
	}

	infoAny := dialInfo.Load()
	info, ok := infoAny.(l2BackendDialInfo)
	if !ok {
		t.Fatalf("unexpected dialInfo type: %T", infoAny)
	}
	if info.TokenQuery != "relay-secret" {
		t.Fatalf("backend query token=%q, want %q", info.TokenQuery, "relay-secret")
	}
	if info.Origin != strings.ToLower(baseURL) {
		t.Fatalf("backend Origin=%q, want %q", info.Origin, strings.ToLower(baseURL))
	}
	if strings.Contains(info.SubprotocolHeader, l2tunnel.TokenSubprotocolPrefix) {
		t.Fatalf("expected no aero-l2-token subprotocol when backend token is unset (got %q)", info.SubprotocolHeader)
	}
}

func TestWebRTCUDPRelay_L2TunnelForwardsDerivedOriginWhenMissing(t *testing.T) {
	backendURL, upgrades, dialInfo := startTestL2BackendWithQueryToken(t, "")

	relayCfg := relay.DefaultConfig()
	relayCfg.L2BackendWSURL = backendURL
	relayCfg.L2BackendForwardOrigin = true

	destPolicy := policy.NewDevDestinationPolicy()
	baseURL := startTestRelayServer(t, relayCfg, destPolicy)

	pc, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peer connection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := true
	dc, err := pc.CreateDataChannel(l2tunnel.DataChannelLabel, &webrtc.DataChannelInit{
		Ordered: &ordered,
	})
	if err != nil {
		t.Fatalf("create data channel: %v", err)
	}

	openCh := make(chan struct{})
	gotCh := make(chan []byte, 1)

	dc.OnOpen(func() { close(openCh) })
	dc.OnMessage(func(msg webrtc.DataChannelMessage) {
		if msg.IsString {
			return
		}
		select {
		case gotCh <- append([]byte(nil), msg.Data...):
		default:
		}
	})

	// No Origin header is provided; the relay should derive a deterministic Origin
	// from the request host/scheme.
	exchangeOffer(t, baseURL, pc)

	select {
	case <-openCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for l2 datachannel open")
	}

	ping := []byte{l2tunnel.Magic, l2tunnel.Version, l2tunnel.TypePing, 0x00}
	if err := dc.Send(ping); err != nil {
		t.Fatalf("send ping: %v", err)
	}

	select {
	case <-gotCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for pong")
	}

	if gotUpgrades := upgrades.Load(); gotUpgrades == 0 {
		t.Fatalf("expected relay to dial backend websocket (got %d upgrades)", gotUpgrades)
	}

	infoAny := dialInfo.Load()
	info, ok := infoAny.(l2BackendDialInfo)
	if !ok {
		t.Fatalf("unexpected dialInfo type: %T", infoAny)
	}
	if info.Origin != strings.ToLower(baseURL) {
		t.Fatalf("backend Origin=%q, want %q", info.Origin, strings.ToLower(baseURL))
	}
}

func TestWebRTCUDPRelay_L2TunnelBackendOriginOverrideTakesPrecedence(t *testing.T) {
	backendURL, upgrades, dialInfo := startTestL2BackendWithQueryToken(t, "")

	relayCfg := relay.DefaultConfig()
	relayCfg.L2BackendWSURL = backendURL
	relayCfg.L2BackendForwardOrigin = true
	relayCfg.L2BackendWSOrigin = "https://backend.example.com"

	destPolicy := policy.NewDevDestinationPolicy()
	baseURL := startTestRelayServer(t, relayCfg, destPolicy)

	pc, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peer connection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := true
	dc, err := pc.CreateDataChannel(l2tunnel.DataChannelLabel, &webrtc.DataChannelInit{
		Ordered: &ordered,
	})
	if err != nil {
		t.Fatalf("create data channel: %v", err)
	}

	openCh := make(chan struct{})
	gotCh := make(chan []byte, 1)

	dc.OnOpen(func() { close(openCh) })
	dc.OnMessage(func(msg webrtc.DataChannelMessage) {
		if msg.IsString {
			return
		}
		select {
		case gotCh <- append([]byte(nil), msg.Data...):
		default:
		}
	})

	headers := http.Header{
		"Origin": []string{baseURL},
	}
	exchangeOfferWithHeaders(t, baseURL+"/offer", headers, pc)

	select {
	case <-openCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for l2 datachannel open")
	}

	ping := []byte{l2tunnel.Magic, l2tunnel.Version, l2tunnel.TypePing, 0x00}
	if err := dc.Send(ping); err != nil {
		t.Fatalf("send ping: %v", err)
	}

	select {
	case <-gotCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for pong")
	}

	if gotUpgrades := upgrades.Load(); gotUpgrades == 0 {
		t.Fatalf("expected relay to dial backend websocket (got %d upgrades)", gotUpgrades)
	}

	infoAny := dialInfo.Load()
	info, ok := infoAny.(l2BackendDialInfo)
	if !ok {
		t.Fatalf("unexpected dialInfo type: %T", infoAny)
	}
	if info.Origin != "https://backend.example.com" {
		t.Fatalf("backend Origin=%q, want %q", info.Origin, "https://backend.example.com")
	}
}

func TestWebRTCUDPRelay_L2TunnelBackendTokenViaSubprotocolForwarding(t *testing.T) {
	backendURL, upgrades, dialInfo := startTestL2BackendWithQueryToken(t, "")

	relayCfg := relay.DefaultConfig()
	relayCfg.L2BackendWSURL = backendURL
	relayCfg.L2BackendAuthForwardMode = config.L2BackendAuthForwardModeSubprotocol

	destPolicy := policy.NewDevDestinationPolicy()
	baseURL := startTestRelayServerWithAuth(t, relayCfg, destPolicy, config.AuthModeAPIKey, "relay-secret")

	pc, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peer connection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := true
	dc, err := pc.CreateDataChannel(l2tunnel.DataChannelLabel, &webrtc.DataChannelInit{
		Ordered: &ordered,
	})
	if err != nil {
		t.Fatalf("create data channel: %v", err)
	}

	openCh := make(chan struct{})
	gotCh := make(chan []byte, 1)

	dc.OnOpen(func() { close(openCh) })
	dc.OnMessage(func(msg webrtc.DataChannelMessage) {
		if msg.IsString {
			return
		}
		select {
		case gotCh <- append([]byte(nil), msg.Data...):
		default:
		}
	})

	exchangeOfferWithHeaders(t, baseURL+"/offer?token=relay-secret", nil, pc)

	select {
	case <-openCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for l2 datachannel open")
	}

	ping := []byte{l2tunnel.Magic, l2tunnel.Version, l2tunnel.TypePing, 0x00}
	if err := dc.Send(ping); err != nil {
		t.Fatalf("send ping: %v", err)
	}

	var got []byte
	select {
	case got = <-gotCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for pong")
	}

	want := []byte{l2tunnel.Magic, l2tunnel.Version, l2tunnel.TypePong, 0x00}
	if !bytes.Equal(got, want) {
		t.Fatalf("pong mismatch: %x != %x", got, want)
	}

	if gotUpgrades := upgrades.Load(); gotUpgrades == 0 {
		t.Fatalf("expected relay to dial backend websocket (got %d upgrades)", gotUpgrades)
	}

	infoAny := dialInfo.Load()
	info, ok := infoAny.(l2BackendDialInfo)
	if !ok {
		t.Fatalf("unexpected dialInfo type: %T", infoAny)
	}
	if info.TokenQuery != "" {
		t.Fatalf("backend query token=%q, want empty", info.TokenQuery)
	}
	if !strings.Contains(info.SubprotocolHeader, l2tunnel.TokenSubprotocolPrefix+"relay-secret") {
		t.Fatalf("expected Sec-WebSocket-Protocol to include forwarded credential (got %q)", info.SubprotocolHeader)
	}
}

func TestWebRTCUDPRelay_L2TunnelBackendTokenOverridesForwardedCredential(t *testing.T) {
	backendURL, upgrades, dialInfo := startTestL2BackendWithQueryToken(t, "")

	relayCfg := relay.DefaultConfig()
	relayCfg.L2BackendWSURL = backendURL
	relayCfg.L2BackendWSToken = "backend-secret"
	relayCfg.L2BackendAuthForwardMode = config.L2BackendAuthForwardModeSubprotocol

	destPolicy := policy.NewDevDestinationPolicy()
	baseURL := startTestRelayServerWithAuth(t, relayCfg, destPolicy, config.AuthModeAPIKey, "client-secret")

	pc, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peer connection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := true
	dc, err := pc.CreateDataChannel(l2tunnel.DataChannelLabel, &webrtc.DataChannelInit{
		Ordered: &ordered,
	})
	if err != nil {
		t.Fatalf("create data channel: %v", err)
	}

	openCh := make(chan struct{})
	gotCh := make(chan []byte, 1)

	dc.OnOpen(func() { close(openCh) })
	dc.OnMessage(func(msg webrtc.DataChannelMessage) {
		if msg.IsString {
			return
		}
		select {
		case gotCh <- append([]byte(nil), msg.Data...):
		default:
		}
	})

	exchangeOfferWithHeaders(t, baseURL+"/offer?token=client-secret", nil, pc)

	select {
	case <-openCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for l2 datachannel open")
	}

	ping := []byte{l2tunnel.Magic, l2tunnel.Version, l2tunnel.TypePing, 0x00}
	if err := dc.Send(ping); err != nil {
		t.Fatalf("send ping: %v", err)
	}

	var got []byte
	select {
	case got = <-gotCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for pong")
	}

	want := []byte{l2tunnel.Magic, l2tunnel.Version, l2tunnel.TypePong, 0x00}
	if !bytes.Equal(got, want) {
		t.Fatalf("pong mismatch: %x != %x", got, want)
	}

	if gotUpgrades := upgrades.Load(); gotUpgrades == 0 {
		t.Fatalf("expected relay to dial backend websocket (got %d upgrades)", gotUpgrades)
	}

	infoAny := dialInfo.Load()
	info, ok := infoAny.(l2BackendDialInfo)
	if !ok {
		t.Fatalf("unexpected dialInfo type: %T", infoAny)
	}
	if info.TokenQuery != "" {
		t.Fatalf("backend query token=%q, want empty", info.TokenQuery)
	}
	if !strings.Contains(info.SubprotocolHeader, l2tunnel.TokenSubprotocolPrefix+"backend-secret") {
		t.Fatalf("expected Sec-WebSocket-Protocol to include backend token (got %q)", info.SubprotocolHeader)
	}
	if strings.Contains(info.SubprotocolHeader, l2tunnel.TokenSubprotocolPrefix+"client-secret") {
		t.Fatalf("expected Sec-WebSocket-Protocol not to include forwarded credential when backend token is configured (got %q)", info.SubprotocolHeader)
	}
}

func TestWebRTCUDPRelay_L2TunnelAuthForwardModeNoneDoesNotForwardCredential(t *testing.T) {
	backendURL, upgrades, dialInfo := startTestL2BackendWithQueryToken(t, "")

	relayCfg := relay.DefaultConfig()
	relayCfg.L2BackendWSURL = backendURL
	relayCfg.L2BackendAuthForwardMode = config.L2BackendAuthForwardModeNone

	destPolicy := policy.NewDevDestinationPolicy()
	baseURL := startTestRelayServerWithAuth(t, relayCfg, destPolicy, config.AuthModeAPIKey, "relay-secret")

	pc, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peer connection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := true
	dc, err := pc.CreateDataChannel(l2tunnel.DataChannelLabel, &webrtc.DataChannelInit{
		Ordered: &ordered,
	})
	if err != nil {
		t.Fatalf("create data channel: %v", err)
	}

	openCh := make(chan struct{})
	gotCh := make(chan []byte, 1)

	dc.OnOpen(func() { close(openCh) })
	dc.OnMessage(func(msg webrtc.DataChannelMessage) {
		if msg.IsString {
			return
		}
		select {
		case gotCh <- append([]byte(nil), msg.Data...):
		default:
		}
	})

	exchangeOfferWithHeaders(t, baseURL+"/offer?token=relay-secret", nil, pc)

	select {
	case <-openCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for l2 datachannel open")
	}

	ping := []byte{l2tunnel.Magic, l2tunnel.Version, l2tunnel.TypePing, 0x00}
	if err := dc.Send(ping); err != nil {
		t.Fatalf("send ping: %v", err)
	}

	var got []byte
	select {
	case got = <-gotCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for pong")
	}

	want := []byte{l2tunnel.Magic, l2tunnel.Version, l2tunnel.TypePong, 0x00}
	if !bytes.Equal(got, want) {
		t.Fatalf("pong mismatch: %x != %x", got, want)
	}

	if gotUpgrades := upgrades.Load(); gotUpgrades == 0 {
		t.Fatalf("expected relay to dial backend websocket (got %d upgrades)", gotUpgrades)
	}

	infoAny := dialInfo.Load()
	info, ok := infoAny.(l2BackendDialInfo)
	if !ok {
		t.Fatalf("unexpected dialInfo type: %T", infoAny)
	}
	if info.TokenQuery != "" {
		t.Fatalf("backend query token=%q, want empty", info.TokenQuery)
	}
	if strings.Contains(info.SubprotocolHeader, l2tunnel.TokenSubprotocolPrefix) {
		t.Fatalf("expected no aero-l2-token.* subprotocol when auth forward mode is none (got %q)", info.SubprotocolHeader)
	}
}

func TestWebRTCUDPRelay_L2TunnelSubprotocolForwardingRejectsInvalidCredential(t *testing.T) {
	backendURL, upgrades := startTestL2Backend(t)

	relayCfg := relay.DefaultConfig()
	relayCfg.L2BackendWSURL = backendURL
	relayCfg.L2BackendAuthForwardMode = config.L2BackendAuthForwardModeSubprotocol

	destPolicy := policy.NewDevDestinationPolicy()
	baseURL := startTestRelayServerWithAuth(t, relayCfg, destPolicy, config.AuthModeAPIKey, "bad=secret")

	pc, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peer connection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := true
	dc, err := pc.CreateDataChannel(l2tunnel.DataChannelLabel, &webrtc.DataChannelInit{
		Ordered: &ordered,
	})
	if err != nil {
		t.Fatalf("create data channel: %v", err)
	}

	closedCh := make(chan struct{})
	dc.OnClose(func() { close(closedCh) })

	headers := http.Header{
		"X-API-Key": []string{"bad=secret"},
	}
	exchangeOfferWithHeaders(t, baseURL+"/offer", headers, pc)

	select {
	case <-closedCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for server to close l2 channel with invalid subprotocol credential")
	}

	if gotUpgrades := upgrades.Load(); gotUpgrades != 0 {
		t.Fatalf("backend websocket should not be dialed when subprotocol credential is invalid (got %d upgrades)", gotUpgrades)
	}
}

func TestWebRTCUDPRelay_L2TunnelForwardsAeroSessionCookie(t *testing.T) {
	backendURL, upgrades, dialInfo := startTestL2BackendWithQueryToken(t, "")

	relayCfg := relay.DefaultConfig()
	relayCfg.L2BackendWSURL = backendURL
	relayCfg.L2BackendForwardAeroSession = true
	destPolicy := policy.NewDevDestinationPolicy()
	baseURL := startTestRelayServer(t, relayCfg, destPolicy)

	pc, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peer connection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := true
	dc, err := pc.CreateDataChannel(l2tunnel.DataChannelLabel, &webrtc.DataChannelInit{
		Ordered: &ordered,
	})
	if err != nil {
		t.Fatalf("create data channel: %v", err)
	}

	openCh := make(chan struct{})
	gotCh := make(chan []byte, 1)

	dc.OnOpen(func() { close(openCh) })
	dc.OnMessage(func(msg webrtc.DataChannelMessage) {
		if msg.IsString {
			return
		}
		select {
		case gotCh <- append([]byte(nil), msg.Data...):
		default:
		}
	})

	headers := http.Header{
		"Cookie": []string{"aero_session=sess123; other=ignored"},
	}
	exchangeOfferWithHeaders(t, baseURL+"/offer", headers, pc)

	select {
	case <-openCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for l2 datachannel open")
	}

	ping := []byte{l2tunnel.Magic, l2tunnel.Version, l2tunnel.TypePing, 0x00}
	if err := dc.Send(ping); err != nil {
		t.Fatalf("send ping: %v", err)
	}

	var got []byte
	select {
	case got = <-gotCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for pong")
	}

	want := []byte{l2tunnel.Magic, l2tunnel.Version, l2tunnel.TypePong, 0x00}
	if !bytes.Equal(got, want) {
		t.Fatalf("pong mismatch: %x != %x", got, want)
	}

	if gotUpgrades := upgrades.Load(); gotUpgrades == 0 {
		t.Fatalf("expected relay to dial backend websocket (got %d upgrades)", gotUpgrades)
	}

	infoAny := dialInfo.Load()
	info, ok := infoAny.(l2BackendDialInfo)
	if !ok {
		t.Fatalf("unexpected dialInfo type: %T", infoAny)
	}
	if info.Cookie != "aero_session=sess123" {
		t.Fatalf("backend Cookie=%q, want %q", info.Cookie, "aero_session=sess123")
	}
}
