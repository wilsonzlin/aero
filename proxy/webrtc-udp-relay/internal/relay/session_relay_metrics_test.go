package relay

import (
	"encoding/binary"
	"net"
	"testing"
	"time"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

func TestSessionRelay_WebRTCUDPMetrics_MalformedFrame(t *testing.T) {
	m := metrics.New()
	sm := NewSessionManager(config.Config{}, m, nil)
	sess, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}

	dc := &fakeDataChannel{sent: make(chan []byte, 1)}
	r := NewSessionRelay(dc, DefaultConfig(), policy.NewDevDestinationPolicy(), sess, nil)
	t.Cleanup(r.Close)

	r.HandleDataChannelMessage([]byte{0x00})

	if got := m.Get(metrics.WebRTCUDPDatagramsIn); got != 1 {
		t.Fatalf("expected %s=1, got %d", metrics.WebRTCUDPDatagramsIn, got)
	}
	if got := m.Get(metrics.WebRTCUDPDroppedMalformed); got == 0 {
		t.Fatalf("expected %s metric increment", metrics.WebRTCUDPDroppedMalformed)
	}
}

func TestSessionRelay_WebRTCUDPMetrics_OversizedPayload(t *testing.T) {
	m := metrics.New()
	sm := NewSessionManager(config.Config{}, m, nil)
	sess, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}

	relayCfg := DefaultConfig()
	relayCfg.MaxDatagramPayloadBytes = 1

	dc := &fakeDataChannel{sent: make(chan []byte, 1)}
	r := NewSessionRelay(dc, relayCfg, policy.NewDevDestinationPolicy(), sess, nil)
	t.Cleanup(r.Close)

	// Construct a v1 frame whose payload is larger than MaxDatagramPayloadBytes.
	oversized := make([]byte, udpproto.HeaderLen+2)
	binary.BigEndian.PutUint16(oversized[0:2], 1234)
	copy(oversized[2:6], []byte{127, 0, 0, 1})
	binary.BigEndian.PutUint16(oversized[6:8], 53)
	copy(oversized[udpproto.HeaderLen:], []byte{0x01, 0x02})

	r.HandleDataChannelMessage(oversized[:udpproto.HeaderLen+2])

	if got := m.Get(metrics.WebRTCUDPDroppedOversized); got == 0 {
		t.Fatalf("expected %s metric increment", metrics.WebRTCUDPDroppedOversized)
	}
}

func TestSessionRelay_WebRTCUDPMetrics_BackpressureDrop(t *testing.T) {
	m := metrics.New()
	sm := NewSessionManager(config.Config{}, m, nil)
	sess, err := sm.CreateSession()
	if err != nil {
		t.Fatalf("CreateSession: %v", err)
	}

	// Ensure the outbound send queue can't fit even a single UDP frame so we can
	// deterministically force backpressure drops.
	relayCfg := DefaultConfig()
	relayCfg.DataChannelSendQueueBytes = 1

	dc := &fakeDataChannel{sent: make(chan []byte, 1)}
	r := NewSessionRelay(dc, relayCfg, policy.NewDevDestinationPolicy(), sess, nil)
	t.Cleanup(r.Close)
	r.EnableWebRTCUDPMetrics()

	remote, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 0})
	if err != nil {
		t.Fatalf("listen udp: %v", err)
	}
	t.Cleanup(func() { _ = remote.Close() })
	remoteAddr := remote.LocalAddr().(*net.UDPAddr)
	ip4 := remoteAddr.IP.To4()
	if ip4 == nil {
		t.Fatalf("expected ipv4 address, got %v", remoteAddr.IP)
	}
	var remoteIP [4]byte
	copy(remoteIP[:], ip4)

	const guestPort = uint16(1234)
	r.HandleDataChannelMessage(mustEncode(t, udpproto.Datagram{
		GuestPort:  guestPort,
		RemoteIP:   remoteIP,
		RemotePort: uint16(remoteAddr.Port),
		Payload:    []byte("ping"),
	}))

	// Discover the binding's local port so the remote can send a packet back.
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

	if _, err := remote.WriteToUDP([]byte("pong"), bindingAddr); err != nil {
		t.Fatalf("remote write: %v", err)
	}

	deadline = time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if m.Get(metrics.WebRTCUDPDroppedBackpressure) > 0 {
			return
		}
		time.Sleep(10 * time.Millisecond)
	}
	t.Fatalf("expected %s metric increment", metrics.WebRTCUDPDroppedBackpressure)
}

func TestSessionRelay_DefaultMetricsSink_UsesSessionMetrics(t *testing.T) {
	m := metrics.New()
	// Construct a minimal session with an attached metrics registry. This test is
	// intentionally focused on the metrics-sink wiring (NewSessionRelay should
	// default to session.metrics when the explicit sink is nil); it does not
	// exercise session quota enforcement.
	sess := &Session{metrics: m}

	dc := &fakeDataChannel{sent: make(chan []byte, 1)}
	r := NewSessionRelay(dc, DefaultConfig(), policy.NewDevDestinationPolicy(), sess, nil)
	t.Cleanup(r.Close)

	if r.metrics != m {
		t.Fatalf("expected relay to default metrics sink to session.metrics")
	}

	b, err := r.getOrCreateBinding(1234)
	if err != nil {
		t.Fatalf("getOrCreateBinding: %v", err)
	}
	if b.metrics != m {
		t.Fatalf("expected binding to inherit metrics sink from session.metrics")
	}
}
