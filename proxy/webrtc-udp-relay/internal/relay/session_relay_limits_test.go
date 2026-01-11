package relay

import (
	"net"
	"testing"
	"time"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

func TestSessionRelay_EnforcesOutboundUDPRateLimit(t *testing.T) {
	clk := &ratelimitTestClock{now: time.Unix(0, 0)}
	cfg := config.Config{
		MaxUDPPpsPerSession: 1,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, clk)
	sess, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}

	remote, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 0})
	if err != nil {
		t.Fatalf("listen udp: %v", err)
	}
	t.Cleanup(func() { _ = remote.Close() })
	remoteAddr := remote.LocalAddr().(*net.UDPAddr)

	recv := make(chan struct{}, 4)
	go func() {
		defer close(recv)
		buf := make([]byte, 2048)
		for {
			_ = remote.SetReadDeadline(time.Now().Add(250 * time.Millisecond))
			_, _, err := remote.ReadFromUDP(buf)
			if err != nil {
				return
			}
			recv <- struct{}{}
		}
	}()

	dc := &fakeDataChannel{sent: make(chan []byte, 16)}
	r := NewSessionRelay(dc, DefaultConfig(), policy.NewDevDestinationPolicy(), sess)
	t.Cleanup(r.Close)

	send := func() {
		r.HandleDataChannelMessage(mustEncode(t, udpproto.Datagram{
			GuestPort:  1234,
			RemoteIP:   [4]byte{127, 0, 0, 1},
			RemotePort: uint16(remoteAddr.Port),
			Payload:    []byte("x"),
		}))
	}

	send()
	send()

	select {
	case _, ok := <-recv:
		if !ok {
			t.Fatalf("receiver closed before first forwarded packet (expected one packet)")
		}
		// ok
	case <-time.After(750 * time.Millisecond):
		t.Fatalf("timed out waiting for first forwarded packet")
	}

	select {
	case _, ok := <-recv:
		// The receiver goroutine closes the channel once it hits its read deadline
		// (i.e. no packet arrived for the deadline window). A closed channel means
		// "no more packets", which is acceptable for this assertion. Treat only an
		// actual receive (ok=true) as a forwarded packet.
		if ok {
			t.Fatalf("unexpected second forwarded packet (expected rate limiting)")
		}
		// ok (channel closed)
	case <-time.After(250 * time.Millisecond):
		// ok
	}

	if m.Get(metrics.DropReasonRateLimited) == 0 {
		t.Fatalf("expected rate_limited metric increment")
	}
}

func TestSessionRelay_EnforcesInboundDataChannelRateLimit(t *testing.T) {
	clk := &ratelimitTestClock{now: time.Unix(0, 0)}

	// For an IPv4 v1 frame, length is 8 (header) + payload length.
	const payloadLen = 1
	const frameLen = udpproto.HeaderLen + payloadLen

	cfg := config.Config{
		MaxDataChannelBpsPerSession: frameLen,
	}
	m := metrics.New()
	sm := NewSessionManager(cfg, m, clk)
	sess, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}

	remote, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 0})
	if err != nil {
		t.Fatalf("listen udp: %v", err)
	}
	t.Cleanup(func() { _ = remote.Close() })
	remoteAddr := remote.LocalAddr().(*net.UDPAddr)

	dc := &fakeDataChannel{sent: make(chan []byte, 16)}
	r := NewSessionRelay(dc, DefaultConfig(), policy.NewDevDestinationPolicy(), sess)
	t.Cleanup(r.Close)

	// Create the UDP binding and allowlist the remote endpoint.
	r.HandleDataChannelMessage(mustEncode(t, udpproto.Datagram{
		GuestPort:  1234,
		RemoteIP:   [4]byte{127, 0, 0, 1},
		RemotePort: uint16(remoteAddr.Port),
		Payload:    []byte("p"),
	}))

	var bindingAddr *net.UDPAddr
	deadline := time.Now().Add(500 * time.Millisecond)
	for time.Now().Before(deadline) {
		r.mu.Lock()
		b := r.bindings[1234]
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

	if _, err := remote.WriteToUDP([]byte("a"), bindingAddr); err != nil {
		t.Fatalf("remote write a: %v", err)
	}
	if _, err := remote.WriteToUDP([]byte("b"), bindingAddr); err != nil {
		t.Fatalf("remote write b: %v", err)
	}

	select {
	case <-dc.sent:
		// ok
	case <-time.After(750 * time.Millisecond):
		t.Fatalf("timed out waiting for first relay->client frame")
	}

	select {
	case <-dc.sent:
		t.Fatalf("unexpected second relay->client frame (expected rate limiting)")
	case <-time.After(250 * time.Millisecond):
		// ok
	}

	if m.Get(metrics.DropReasonRateLimited) == 0 {
		t.Fatalf("expected rate_limited metric increment")
	}
}
