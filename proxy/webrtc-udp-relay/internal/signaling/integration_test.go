package signaling_test

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/netip"
	"testing"
	"time"

	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/httpserver"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/signaling"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

func TestWebRTCUDPRelay_UDPDatagramRoundTrip(t *testing.T) {
	echoConn, err := net.ListenUDP("udp", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 0})
	if err != nil {
		t.Fatalf("listen udp echo: %v", err)
	}
	t.Cleanup(func() { _ = echoConn.Close() })

	go func() {
		buf := make([]byte, 65535)
		for {
			n, addr, err := echoConn.ReadFromUDP(buf)
			if err != nil {
				return
			}
			_, _ = echoConn.WriteToUDP(buf[:n], addr)
		}
	}()

	relayCfg := relay.DefaultConfig()
	destPolicy := policy.NewDevDestinationPolicy()

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
	baseURL := "http://" + ln.Addr().String()

	pc, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new peer connection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	openCh := make(chan struct{})
	gotCh := make(chan udpproto.Frame, 1)

	ordered := false
	maxRetransmits := uint16(0)
	dc, err := pc.CreateDataChannel("udp", &webrtc.DataChannelInit{
		Ordered:        &ordered,
		MaxRetransmits: &maxRetransmits,
	})
	if err != nil {
		t.Fatalf("create data channel: %v", err)
	}

	dc.OnOpen(func() {
		close(openCh)
	})
	dc.OnMessage(func(msg webrtc.DataChannelMessage) {
		if msg.IsString {
			return
		}
		d, err := udpproto.Decode(msg.Data)
		if err != nil {
			return
		}
		select {
		case gotCh <- d:
		default:
		}
	})

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

	select {
	case <-openCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for datachannel open")
	}

	echoAddr := echoConn.LocalAddr().(*net.UDPAddr)
	ip4 := echoAddr.IP.To4()
	if ip4 == nil {
		t.Fatalf("echo addr must be ipv4: %v", echoAddr.IP)
	}
	var echoIP [4]byte
	copy(echoIP[:], ip4)
	const guestPort = uint16(4242)
	wantPayload := []byte("hello")
	frame, err := udpproto.EncodeV2(udpproto.Frame{
		Version:    2,
		GuestPort:  guestPort,
		RemoteIP:   netip.AddrFrom4(echoIP),
		RemotePort: uint16(echoAddr.Port),
		Payload:    wantPayload,
	})
	if err != nil {
		t.Fatalf("encode datagram: %v", err)
	}
	if err := dc.Send(frame); err != nil {
		t.Fatalf("send datagram: %v", err)
	}

	var got udpproto.Frame
	select {
	case got = <-gotCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for echoed datagram")
	}

	if got.GuestPort != guestPort {
		t.Fatalf("guest port mismatch: %d != %d", got.GuestPort, guestPort)
	}
	if got.RemotePort != uint16(echoAddr.Port) {
		t.Fatalf("remote port mismatch: %d != %d", got.RemotePort, echoAddr.Port)
	}
	if got.RemoteIP != netip.AddrFrom4(echoIP) {
		t.Fatalf("remote ip mismatch: %v != %v", got.RemoteIP, netip.AddrFrom4(echoIP))
	}
	if !bytes.Equal(got.Payload, wantPayload) {
		t.Fatalf("payload mismatch: %q != %q", got.Payload, wantPayload)
	}
}
