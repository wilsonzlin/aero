package relay

import (
	"bytes"
	"net"
	"net/netip"
	"testing"
	"time"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

func TestEngineIPv6EchoV2(t *testing.T) {
	echoConn, echoAddr := startIPv6UDPEchoServer(t)
	defer echoConn.Close()

	outCh := make(chan []byte, 1)
	engine := NewEngine(EngineConfig{
		PreferV2: true,
		Policy:   policy.NewDevDestinationPolicy(),
	}, func(b []byte) error {
		cp := append([]byte(nil), b...)
		outCh <- cp
		return nil
	})
	defer engine.Close()

	payload := []byte("hello over ipv6")
	inFrame := udpproto.Frame{
		GuestPort:  4321,
		RemoteIP:   echoAddr.Addr(),
		RemotePort: echoAddr.Port(),
		Payload:    payload,
	}
	inPkt, err := udpproto.EncodeV2(inFrame)
	if err != nil {
		t.Fatalf("EncodeV2: %v", err)
	}

	if err := engine.HandleClientFrame(inPkt); err != nil {
		t.Fatalf("HandleClientFrame: %v", err)
	}

	var outPkt []byte
	select {
	case outPkt = <-outCh:
	case <-time.After(2 * time.Second):
		t.Fatalf("timed out waiting for relay response")
	}

	gotFrame, err := udpproto.Decode(outPkt)
	if err != nil {
		t.Fatalf("Decode: %v", err)
	}
	if gotFrame.Version != 2 {
		t.Fatalf("gotFrame.Version = %d, want 2", gotFrame.Version)
	}
	if gotFrame.GuestPort != inFrame.GuestPort {
		t.Fatalf("gotFrame.GuestPort = %d, want %d", gotFrame.GuestPort, inFrame.GuestPort)
	}
	if gotFrame.RemoteIP != inFrame.RemoteIP || gotFrame.RemotePort != inFrame.RemotePort {
		t.Fatalf("gotFrame remote = %s:%d, want %s:%d", gotFrame.RemoteIP, gotFrame.RemotePort, inFrame.RemoteIP, inFrame.RemotePort)
	}
	if !bytes.Equal(gotFrame.Payload, payload) {
		t.Fatalf("got payload %q, want %q", gotFrame.Payload, payload)
	}
}

func startIPv6UDPEchoServer(t *testing.T) (*net.UDPConn, netip.AddrPort) {
	t.Helper()

	conn, err := net.ListenUDP("udp6", &net.UDPAddr{IP: net.IPv6loopback, Port: 0})
	if err != nil {
		t.Skipf("ipv6 not supported: %v", err)
	}
	addr := conn.LocalAddr().(*net.UDPAddr).AddrPort()

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
