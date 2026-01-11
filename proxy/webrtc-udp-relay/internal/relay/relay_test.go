package relay

import (
	"net"
	"net/netip"
	"testing"
	"time"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

type fakeDataChannel struct {
	sent chan []byte
}

func (f *fakeDataChannel) Send(data []byte) error {
	cp := append([]byte(nil), data...)
	f.sent <- cp
	return nil
}

func mustEncode(t *testing.T, d udpproto.Datagram) []byte {
	t.Helper()
	b, err := udpproto.EncodeDatagram(d, nil)
	if err != nil {
		t.Fatalf("encode datagram: %v", err)
	}
	return b
}

func TestSessionRelay_BindingEviction(t *testing.T) {
	dc := &fakeDataChannel{sent: make(chan []byte, 128)}
	p := policy.NewDevDestinationPolicy()
	cfg := Config{
		MaxUDPBindingsPerSession:  2,
		UDPBindingIdleTimeout:     time.Minute,
		UDPReadBufferBytes:        2048,
		DataChannelSendQueueBytes: 1 << 20,
	}
	r := NewSessionRelay(dc, cfg, p, nil)
	t.Cleanup(r.Close)

	send := func(guestPort uint16) {
		r.HandleDataChannelMessage(mustEncode(t, udpproto.Datagram{
			GuestPort:  guestPort,
			RemoteIP:   [4]byte{127, 0, 0, 1},
			RemotePort: 9999,
			Payload:    []byte("x"),
		}))
	}

	send(1)
	time.Sleep(5 * time.Millisecond)
	send(2)
	time.Sleep(5 * time.Millisecond)
	send(3)

	deadline := time.Now().Add(500 * time.Millisecond)
	for time.Now().Before(deadline) {
		r.mu.Lock()
		_, ok1 := r.bindings[1]
		_, ok2 := r.bindings[2]
		_, ok3 := r.bindings[3]
		r.mu.Unlock()
		if !ok1 && ok2 && ok3 && len(r.bindings) == 2 {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("expected binding 1 to be evicted; bindings=%v", func() []uint16 {
		r.mu.Lock()
		defer r.mu.Unlock()
		ports := make([]uint16, 0, len(r.bindings))
		for p := range r.bindings {
			ports = append(ports, p)
		}
		return ports
	}())
}

func TestSessionRelay_IdleTimeoutCleanup(t *testing.T) {
	dc := &fakeDataChannel{sent: make(chan []byte, 128)}
	p := policy.NewDevDestinationPolicy()
	cfg := Config{
		MaxUDPBindingsPerSession:  8,
		UDPBindingIdleTimeout:     50 * time.Millisecond,
		UDPReadBufferBytes:        2048,
		DataChannelSendQueueBytes: 1 << 20,
	}
	r := NewSessionRelay(dc, cfg, p, nil)
	t.Cleanup(r.Close)

	r.HandleDataChannelMessage(mustEncode(t, udpproto.Datagram{
		GuestPort:  1,
		RemoteIP:   [4]byte{127, 0, 0, 1},
		RemotePort: 9999,
		Payload:    []byte("x"),
	}))

	deadline := time.Now().Add(750 * time.Millisecond)
	for time.Now().Before(deadline) {
		r.mu.Lock()
		_, ok := r.bindings[1]
		r.mu.Unlock()
		if !ok {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("binding was not cleaned up after idle timeout")
}

func TestUdpPortBinding_RemoteAllowlist(t *testing.T) {
	dc := &fakeDataChannel{sent: make(chan []byte, 128)}
	p := policy.NewDevDestinationPolicy()
	cfg := DefaultConfig()
	cfg.InboundFilterMode = InboundFilterAddressAndPort
	cfg.RemoteAllowlistIdleTimeout = time.Minute
	cfg.UDPBindingIdleTimeout = time.Minute
	cfg.UDPReadBufferBytes = 2048
	cfg.DataChannelSendQueueBytes = 1 << 20

	r := NewSessionRelay(dc, cfg, p, nil)
	t.Cleanup(r.Close)

	remote1, err := net.ListenUDP("udp", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 0})
	if err != nil {
		t.Fatalf("listen remote1: %v", err)
	}
	defer remote1.Close()
	remote1Addr := remote1.LocalAddr().(*net.UDPAddr)

	const guestPort = uint16(1234)
	ip4 := remote1Addr.IP.To4()
	if ip4 == nil {
		t.Fatalf("remote1 must be ipv4: %v", remote1Addr.IP)
	}
	var remote1IP [4]byte
	copy(remote1IP[:], ip4)
	r.HandleDataChannelMessage(mustEncode(t, udpproto.Datagram{
		GuestPort:  guestPort,
		RemoteIP:   remote1IP,
		RemotePort: uint16(remote1Addr.Port),
		Payload:    []byte("ping"),
	}))

	var bindingAddr *net.UDPAddr
	deadline := time.Now().Add(500 * time.Millisecond)
	for time.Now().Before(deadline) {
		r.mu.Lock()
		b := r.bindings[guestPort]
		r.mu.Unlock()
		if b != nil {
			localPort := b.conn4.LocalAddr().(*net.UDPAddr).Port
			bindingAddr = &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: localPort}
			break
		}
		time.Sleep(10 * time.Millisecond)
	}
	if bindingAddr == nil {
		t.Fatalf("binding was not created")
	}

	if _, err := remote1.WriteToUDP([]byte("pong"), bindingAddr); err != nil {
		t.Fatalf("remote1 write: %v", err)
	}

	var got []byte
	select {
	case got = <-dc.sent:
	case <-time.After(500 * time.Millisecond):
		t.Fatalf("timed out waiting for forwarded packet from allowed remote")
	}

	d, err := udpproto.DecodeDatagram(got)
	if err != nil {
		t.Fatalf("decode forwarded packet: %v", err)
	}
	if d.GuestPort != guestPort {
		t.Fatalf("guest port mismatch: %d != %d", d.GuestPort, guestPort)
	}
	if d.RemotePort != uint16(remote1Addr.Port) {
		t.Fatalf("remote port mismatch: %d != %d", d.RemotePort, remote1Addr.Port)
	}
	if d.RemoteIP != ([4]byte{127, 0, 0, 1}) {
		t.Fatalf("remote ip mismatch: %v", d.RemoteIP)
	}
	if string(d.Payload) != "pong" {
		t.Fatalf("payload mismatch: %q", d.Payload)
	}

	remote2, err := net.ListenUDP("udp", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 0})
	if err != nil {
		t.Fatalf("listen remote2: %v", err)
	}
	defer remote2.Close()

	if _, err := remote2.WriteToUDP([]byte("nope"), bindingAddr); err != nil {
		t.Fatalf("remote2 write: %v", err)
	}

	select {
	case <-dc.sent:
		t.Fatalf("unexpected packet forwarded from disallowed remote")
	case <-time.After(150 * time.Millisecond):
		// ok
	}
}

func TestSessionRelay_IPv6EchoV2(t *testing.T) {
	echoConn, echoAddr := startIPv6UDPEchoServer(t)
	defer echoConn.Close()

	dc := &fakeDataChannel{sent: make(chan []byte, 128)}
	p := policy.NewDevDestinationPolicy()
	cfg := DefaultConfig()
	cfg.UDPBindingIdleTimeout = time.Minute
	cfg.RemoteAllowlistIdleTimeout = time.Minute
	cfg.UDPReadBufferBytes = 2048
	cfg.DataChannelSendQueueBytes = 1 << 20

	r := NewSessionRelay(dc, cfg, p, nil)
	t.Cleanup(r.Close)

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

	r.HandleDataChannelMessage(inPkt)

	var outPkt []byte
	select {
	case outPkt = <-dc.sent:
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
	if string(gotFrame.Payload) != string(payload) {
		t.Fatalf("got payload %q, want %q", gotFrame.Payload, payload)
	}
}

func TestSessionRelay_PreferV2NegotiatedForIPv4(t *testing.T) {
	dc := &fakeDataChannel{sent: make(chan []byte, 128)}
	p := policy.NewDevDestinationPolicy()
	cfg := DefaultConfig()
	cfg.PreferV2 = true
	cfg.InboundFilterMode = InboundFilterAddressAndPort
	cfg.RemoteAllowlistIdleTimeout = time.Minute
	cfg.UDPBindingIdleTimeout = time.Minute
	cfg.UDPReadBufferBytes = 2048
	cfg.DataChannelSendQueueBytes = 1 << 20

	r := NewSessionRelay(dc, cfg, p, nil)
	t.Cleanup(r.Close)

	remote, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 0})
	if err != nil {
		t.Fatalf("listen remote: %v", err)
	}
	defer remote.Close()
	remoteAddr := remote.LocalAddr().(*net.UDPAddr)

	// Send a v2 frame (IPv4) to demonstrate v2 support.
	inFrame := udpproto.Frame{
		GuestPort:  1234,
		RemoteIP:   remoteAddr.AddrPort().Addr(),
		RemotePort: uint16(remoteAddr.Port),
		Payload:    []byte("ping"),
	}
	inPkt, err := udpproto.EncodeV2(inFrame)
	if err != nil {
		t.Fatalf("EncodeV2: %v", err)
	}
	r.HandleDataChannelMessage(inPkt)

	var bindingAddr *net.UDPAddr
	deadline := time.Now().Add(500 * time.Millisecond)
	for time.Now().Before(deadline) {
		r.mu.Lock()
		b := r.bindings[inFrame.GuestPort]
		r.mu.Unlock()
		if b != nil {
			localPort := b.conn4.LocalAddr().(*net.UDPAddr).Port
			bindingAddr = &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: localPort}
			break
		}
		time.Sleep(10 * time.Millisecond)
	}
	if bindingAddr == nil {
		t.Fatalf("binding was not created")
	}

	if _, err := remote.WriteToUDP([]byte("pong"), bindingAddr); err != nil {
		t.Fatalf("remote write: %v", err)
	}

	var got []byte
	select {
	case got = <-dc.sent:
	case <-time.After(500 * time.Millisecond):
		t.Fatalf("timed out waiting for forwarded packet")
	}

	f, err := udpproto.Decode(got)
	if err != nil {
		t.Fatalf("decode forwarded packet: %v", err)
	}
	if f.Version != 2 {
		t.Fatalf("expected v2 frame, got v%d", f.Version)
	}
	if f.GuestPort != inFrame.GuestPort {
		t.Fatalf("guest port mismatch: %d != %d", f.GuestPort, inFrame.GuestPort)
	}
	if f.RemotePort != uint16(remoteAddr.Port) {
		t.Fatalf("remote port mismatch: %d != %d", f.RemotePort, remoteAddr.Port)
	}
	if f.RemoteIP != netip.MustParseAddr("127.0.0.1") {
		t.Fatalf("remote ip mismatch: %v", f.RemoteIP)
	}
	if string(f.Payload) != "pong" {
		t.Fatalf("payload mismatch: %q", f.Payload)
	}
}
