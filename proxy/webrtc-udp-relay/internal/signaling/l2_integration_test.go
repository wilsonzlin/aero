package signaling_test

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net"
	"net/http"
	"sync/atomic"
	"testing"
	"time"

	"github.com/gorilla/websocket"
	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/httpserver"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/signaling"
)

const (
	testL2TunnelSubprotocol = "aero-l2-tunnel-v1"
)

func startTestL2Backend(t *testing.T) (wsURL string, upgradeCount *atomic.Int64) {
	t.Helper()

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen backend: %v", err)
	}

	var upgrades atomic.Int64

	upgrader := websocket.Upgrader{
		CheckOrigin:  func(r *http.Request) bool { return true },
		Subprotocols: []string{testL2TunnelSubprotocol},
	}

	mux := http.NewServeMux()
	mux.HandleFunc("GET /l2", func(w http.ResponseWriter, r *http.Request) {
		upgrades.Add(1)
		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		defer conn.Close()

		if conn.Subprotocol() != testL2TunnelSubprotocol {
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
			if len(payload) < 4 || payload[0] != 0xA2 || payload[1] != 0x03 || payload[2] != 0x01 {
				continue
			}
			out := append([]byte(nil), payload...)
			out[2] = 0x02
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

func exchangeOffer(t *testing.T, baseURL string, pc *webrtc.PeerConnection) {
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

	resp, err := http.Post(baseURL+"/offer", "application/json", bytes.NewReader(offerBody))
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

	ordered := false
	maxRetransmits := uint16(0)
	dc, err := pc.CreateDataChannel("l2", &webrtc.DataChannelInit{
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

	ordered := false
	dc, err := pc.CreateDataChannel("l2", &webrtc.DataChannelInit{
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

	ping := []byte{0xA2, 0x03, 0x01, 0x00}
	if err := dc.Send(ping); err != nil {
		t.Fatalf("send ping: %v", err)
	}

	var got []byte
	select {
	case got = <-gotCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for pong")
	}

	want := []byte{0xA2, 0x03, 0x02, 0x00}
	if !bytes.Equal(got, want) {
		t.Fatalf("pong mismatch: %x != %x", got, want)
	}
}
