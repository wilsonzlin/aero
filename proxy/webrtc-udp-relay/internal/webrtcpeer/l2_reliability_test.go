package webrtcpeer_test

import (
	"bytes"
	"encoding/binary"
	"net"
	"sync"
	"testing"
	"time"

	"github.com/pion/logging"
	"github.com/pion/transport/v3/vnet"
	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/webrtcpeer"
)

func TestL2DataChannel_ReliableDeliveryUnderLoss(t *testing.T) {
	const (
		cidr      = "10.0.0.0/24"
		ipA       = "10.0.0.1"
		ipB       = "10.0.0.2"
		dropFirst = 10

		chunkPayloadBytes = 1024
		chunks            = 256
	)

	router, err := vnet.NewRouter(&vnet.RouterConfig{
		CIDR:          cidr,
		LoggerFactory: logging.NewDefaultLoggerFactory(),
	})
	if err != nil {
		t.Fatalf("new router: %v", err)
	}
	t.Cleanup(func() {
		_ = router.Stop()
	})

	netA, err := vnet.NewNet(&vnet.NetConfig{StaticIPs: []string{ipA}})
	if err != nil {
		t.Fatalf("new net A: %v", err)
	}
	netB, err := vnet.NewNet(&vnet.NetConfig{StaticIPs: []string{ipB}})
	if err != nil {
		t.Fatalf("new net B: %v", err)
	}

	if err := router.AddNet(netA); err != nil {
		t.Fatalf("add net A: %v", err)
	}
	if err := router.AddNet(netB); err != nil {
		t.Fatalf("add net B: %v", err)
	}

	if err := router.Start(); err != nil {
		t.Fatalf("start router: %v", err)
	}

	apiA, err := newVNetAPI(netA)
	if err != nil {
		t.Fatalf("new api A: %v", err)
	}
	apiB, err := newVNetAPI(netB)
	if err != nil {
		t.Fatalf("new api B: %v", err)
	}

	pcA, err := apiA.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new pc A: %v", err)
	}
	t.Cleanup(func() { _ = pcA.Close() })

	pcB, err := apiB.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new pc B: %v", err)
	}
	t.Cleanup(func() { _ = pcB.Close() })

	pcA.OnICECandidate(func(c *webrtc.ICECandidate) {
		if c == nil {
			return
		}
		_ = pcB.AddICECandidate(c.ToJSON())
	})
	pcB.OnICECandidate(func(c *webrtc.ICECandidate) {
		if c == nil {
			return
		}
		_ = pcA.AddICECandidate(c.ToJSON())
	})

	remoteDCCh := make(chan *webrtc.DataChannel, 1)
	pcB.OnDataChannel(func(dc *webrtc.DataChannel) {
		if dc.Label() != webrtcpeer.DataChannelLabelL2 {
			return
		}
		select {
		case remoteDCCh <- dc:
		default:
		}
	})

	localDC, err := webrtcpeer.CreateL2DataChannel(pcA)
	if err != nil {
		t.Fatalf("create l2 datachannel: %v", err)
	}
	if localDC.MaxRetransmits() != nil || localDC.MaxPacketLifeTime() != nil {
		t.Fatalf("l2 datachannel must be reliable (maxRetransmits/maxPacketLifeTime must be unset)")
	}

	localOpen := make(chan struct{})
	localDC.OnOpen(func() { close(localOpen) })

	offer, err := pcA.CreateOffer(nil)
	if err != nil {
		t.Fatalf("create offer: %v", err)
	}
	if err := pcA.SetLocalDescription(offer); err != nil {
		t.Fatalf("set local offer: %v", err)
	}
	if err := pcB.SetRemoteDescription(offer); err != nil {
		t.Fatalf("set remote offer: %v", err)
	}

	answer, err := pcB.CreateAnswer(nil)
	if err != nil {
		t.Fatalf("create answer: %v", err)
	}
	if err := pcB.SetLocalDescription(answer); err != nil {
		t.Fatalf("set local answer: %v", err)
	}
	if err := pcA.SetRemoteDescription(answer); err != nil {
		t.Fatalf("set remote answer: %v", err)
	}

	var remoteDC *webrtc.DataChannel
	select {
	case remoteDC = <-remoteDCCh:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for remote l2 datachannel")
	}

	if remoteDC.MaxRetransmits() != nil || remoteDC.MaxPacketLifeTime() != nil {
		t.Fatalf("remote l2 datachannel must be reliable (maxRetransmits/maxPacketLifeTime must be unset)")
	}

	remoteOpen := make(chan struct{})
	remoteDC.OnOpen(func() { close(remoteOpen) })

	select {
	case <-localOpen:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for local datachannel to open")
	}
	select {
	case <-remoteOpen:
	case <-time.After(5 * time.Second):
		t.Fatalf("timed out waiting for remote datachannel to open")
	}

	// Drop a few packets from A -> B after the DataChannel is established. A
	// reliable channel should retransmit and deliver all messages; an unreliable
	// channel (`maxRetransmits=0`) would lose some messages permanently.
	srcIP := net.ParseIP(ipA)
	dstIP := net.ParseIP(ipB)
	var dropMu sync.Mutex
	dropped := 0
	router.AddChunkFilter(func(c vnet.Chunk) bool {
		if c.Network() != "udp" {
			return true
		}

		src, ok := c.SourceAddr().(*net.UDPAddr)
		if !ok {
			return true
		}
		dst, ok := c.DestinationAddr().(*net.UDPAddr)
		if !ok {
			return true
		}

		if !src.IP.Equal(srcIP) || !dst.IP.Equal(dstIP) {
			return true
		}

		dropMu.Lock()
		defer dropMu.Unlock()
		if dropped >= dropFirst {
			return true
		}
		dropped++
		return false
	})

	want := make([]byte, chunkPayloadBytes*chunks)
	for i := range want {
		want[i] = byte(i)
	}

	got := make([][]byte, chunks)
	var gotMu sync.Mutex
	gotCount := 0
	var doneOnce sync.Once
	done := make(chan struct{})

	remoteDC.OnMessage(func(msg webrtc.DataChannelMessage) {
		if msg.IsString || len(msg.Data) < 4 {
			return
		}
		seq := int(binary.BigEndian.Uint32(msg.Data[:4]))
		if seq < 0 || seq >= chunks {
			return
		}

		payload := msg.Data[4:]
		gotMu.Lock()
		defer gotMu.Unlock()
		if got[seq] != nil {
			return
		}
		got[seq] = append([]byte(nil), payload...)
		gotCount++
		if gotCount == chunks {
			doneOnce.Do(func() { close(done) })
		}
	})

	for i := 0; i < chunks; i++ {
		msg := make([]byte, 4+chunkPayloadBytes)
		binary.BigEndian.PutUint32(msg[:4], uint32(i))
		copy(msg[4:], want[i*chunkPayloadBytes:(i+1)*chunkPayloadBytes])
		if err := localDC.Send(msg); err != nil {
			t.Fatalf("send message %d: %v", i, err)
		}
	}

	select {
	case <-done:
	case <-time.After(10 * time.Second):
		gotMu.Lock()
		gotN := gotCount
		gotMu.Unlock()
		t.Fatalf("timed out waiting for all messages (got %d/%d)", gotN, chunks)
	}

	for i := 0; i < chunks; i++ {
		if got[i] == nil {
			t.Fatalf("missing message %d", i)
		}
		wantChunk := want[i*chunkPayloadBytes : (i+1)*chunkPayloadBytes]
		if !bytes.Equal(got[i], wantChunk) {
			t.Fatalf("payload mismatch for message %d", i)
		}
	}
}

func newVNetAPI(n *vnet.Net) (*webrtc.API, error) {
	se := webrtc.SettingEngine{}
	se.SetNet(n)

	mediaEngine := &webrtc.MediaEngine{}
	if err := mediaEngine.RegisterDefaultCodecs(); err != nil {
		return nil, err
	}

	return webrtc.NewAPI(
		webrtc.WithSettingEngine(se),
		webrtc.WithMediaEngine(mediaEngine),
	), nil
}
