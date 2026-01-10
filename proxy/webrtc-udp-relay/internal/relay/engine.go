package relay

import (
	"errors"
	"net"
	"net/netip"
	"sync"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

type EngineConfig struct {
	PreferV2 bool
	Policy   *policy.DestinationPolicy
}

// Engine relays UDP frames (carried over WebRTC) to real UDP sockets, and
// forwards responses back to the client.
//
// It supports both v1 (IPv4-only) and v2 (IPv4/IPv6) frames.
type Engine struct {
	cfg          EngineConfig
	sendToClient func([]byte) error

	mu               sync.Mutex
	flows            map[flowKey]*udpFlow
	clientSupportsV2 bool
	closed           bool
}

type flowKey struct {
	guestPort uint16
	remote    netip.AddrPort
}

type udpFlow struct {
	key  flowKey
	conn *net.UDPConn
}

func NewEngine(cfg EngineConfig, sendToClient func([]byte) error) *Engine {
	return &Engine{
		cfg:          cfg,
		sendToClient: sendToClient,
		flows:        make(map[flowKey]*udpFlow),
	}
}

func (e *Engine) Close() error {
	e.mu.Lock()
	if e.closed {
		e.mu.Unlock()
		return nil
	}
	e.closed = true
	flows := e.flows
	e.flows = nil
	e.mu.Unlock()

	for _, f := range flows {
		_ = f.conn.Close()
	}
	return nil
}

// HandleClientFrame consumes a single datagram received from the client over the
// WebRTC data channel.
func (e *Engine) HandleClientFrame(pkt []byte) error {
	frame, err := udpproto.Decode(pkt)
	if err != nil {
		return err
	}

	if frame.Version == 2 {
		e.mu.Lock()
		e.clientSupportsV2 = true
		e.mu.Unlock()
	}

	if e.cfg.Policy == nil {
		return errors.New("relay: destination policy is nil")
	}
	if err := e.cfg.Policy.AllowUDP(net.IP(frame.RemoteIP.AsSlice()), frame.RemotePort); err != nil {
		return err
	}

	remote := netip.AddrPortFrom(frame.RemoteIP, frame.RemotePort)
	key := flowKey{guestPort: frame.GuestPort, remote: remote}

	flow, err := e.getOrCreateFlow(key)
	if err != nil {
		return err
	}

	_, err = flow.conn.Write(frame.Payload)
	return err
}

func (e *Engine) getOrCreateFlow(key flowKey) (*udpFlow, error) {
	e.mu.Lock()
	if e.closed {
		e.mu.Unlock()
		return nil, net.ErrClosed
	}
	if f, ok := e.flows[key]; ok {
		e.mu.Unlock()
		return f, nil
	}
	e.mu.Unlock()

	network := "udp4"
	if key.remote.Addr().Is6() {
		network = "udp6"
	}
	conn, err := net.DialUDP(network, nil, net.UDPAddrFromAddrPort(key.remote))
	if err != nil {
		return nil, err
	}

	f := &udpFlow{key: key, conn: conn}

	e.mu.Lock()
	if e.closed {
		e.mu.Unlock()
		_ = conn.Close()
		return nil, net.ErrClosed
	}
	if existing, ok := e.flows[key]; ok {
		e.mu.Unlock()
		_ = conn.Close()
		return existing, nil
	}
	e.flows[key] = f
	e.mu.Unlock()

	go e.readLoop(f)
	return f, nil
}

func (e *Engine) readLoop(f *udpFlow) {
	buf := make([]byte, 64*1024)
	for {
		n, err := f.conn.Read(buf)
		if err != nil {
			return
		}

		payload := make([]byte, n)
		copy(payload, buf[:n])

		frame := udpproto.Frame{
			GuestPort:  f.key.guestPort,
			RemoteIP:   f.key.remote.Addr(),
			RemotePort: f.key.remote.Port(),
			Payload:    payload,
		}

		out, err := e.encodeOutbound(frame)
		if err != nil {
			continue
		}
		if err := e.sendToClient(out); err != nil {
			return
		}
	}
}

func (e *Engine) encodeOutbound(frame udpproto.Frame) ([]byte, error) {
	// IPv6 can only be represented by v2.
	if frame.RemoteIP.Is6() {
		return udpproto.EncodeV2(frame)
	}

	e.mu.Lock()
	preferV2 := e.cfg.PreferV2 || udpproto.PreferV2FromEnv()
	useV2 := preferV2 && e.clientSupportsV2
	e.mu.Unlock()
	if useV2 {
		return udpproto.EncodeV2(frame)
	}
	return udpproto.EncodeV1(frame)
}
