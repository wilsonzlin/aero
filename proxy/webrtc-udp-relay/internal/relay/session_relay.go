package relay

import (
	"context"
	"net"
	"net/netip"
	"sync"
	"sync/atomic"
	"time"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

// DataChannelSender is the subset of pion/webrtc's DataChannel used by the relay.
type DataChannelSender interface {
	Send(data []byte) error
}

// DestinationPolicy is implemented by policy.DestinationPolicy.
type DestinationPolicy interface {
	AllowUDP(remoteIP net.IP, remotePort uint16) error
}

type SessionRelayStats struct {
	OutboundSendQueueDrops uint64
}

// SessionRelay relays UDP datagrams between a WebRTC DataChannel and the public
// network.
//
// A SessionRelay is bound to exactly one DataChannel ("udp") and multiplexes
// guest-port semantics by maintaining a UDP socket per guest port.
type SessionRelay struct {
	dc     DataChannelSender
	cfg    Config
	policy DestinationPolicy
	codec  udpproto.Codec

	ctx    context.Context
	cancel context.CancelFunc

	queue *sendQueue

	mu       sync.Mutex
	bindings map[uint16]*UdpPortBinding

	wg sync.WaitGroup

	closeOnce sync.Once
	closed    atomic.Bool

	clientSupportsV2 atomic.Bool
}

func NewSessionRelay(dc DataChannelSender, cfg Config, policy DestinationPolicy) *SessionRelay {
	cfg = cfg.withDefaults()
	codec, err := udpproto.NewCodec(cfg.MaxDatagramPayloadBytes)
	if err != nil {
		codec = udpproto.DefaultCodec
	}

	ctx, cancel := context.WithCancel(context.Background())
	s := &SessionRelay{
		dc:       dc,
		cfg:      cfg,
		policy:   policy,
		codec:    codec,
		ctx:      ctx,
		cancel:   cancel,
		queue:    newSendQueue(cfg.DataChannelSendQueueBytes),
		bindings: make(map[uint16]*UdpPortBinding),
	}

	s.wg.Add(1)
	go func() {
		defer s.wg.Done()
		s.senderLoop()
	}()

	s.wg.Add(1)
	go func() {
		defer s.wg.Done()
		s.cleanupLoop()
	}()

	return s
}

func (s *SessionRelay) Stats() SessionRelayStats {
	return SessionRelayStats{
		OutboundSendQueueDrops: s.queue.DropCount(),
	}
}

func (s *SessionRelay) Close() {
	s.closeOnce.Do(func() {
		s.closed.Store(true)
		s.cancel()
		s.queue.Close()

		var toClose []*UdpPortBinding
		s.mu.Lock()
		for _, b := range s.bindings {
			toClose = append(toClose, b)
		}
		s.bindings = make(map[uint16]*UdpPortBinding)
		s.mu.Unlock()
		for _, b := range toClose {
			b.Close()
		}

		s.wg.Wait()
	})
}

func (s *SessionRelay) senderLoop() {
	for {
		frame, ok := s.queue.Dequeue()
		if !ok {
			return
		}
		_ = s.dc.Send(frame)
	}
}

func (s *SessionRelay) cleanupLoop() {
	interval := s.cfg.UDPBindingIdleTimeout / 2
	if interval < 100*time.Millisecond {
		interval = 100 * time.Millisecond
	}
	t := time.NewTicker(interval)
	defer t.Stop()
	for {
		select {
		case <-s.ctx.Done():
			return
		case <-t.C:
			s.cleanupIdle()
		}
	}
}

func (s *SessionRelay) cleanupIdle() {
	now := time.Now()
	var toClose []*UdpPortBinding

	s.mu.Lock()
	for port, b := range s.bindings {
		if now.Sub(b.LastUsed()) > s.cfg.UDPBindingIdleTimeout {
			delete(s.bindings, port)
			toClose = append(toClose, b)
		}
	}
	s.mu.Unlock()

	for _, b := range toClose {
		b.Close()
	}
}

func (s *SessionRelay) getOrCreateBinding(guestPort uint16) (*UdpPortBinding, error) {
	s.mu.Lock()
	if s.closed.Load() {
		s.mu.Unlock()
		return nil, ErrSessionClosed
	}
	if b, ok := s.bindings[guestPort]; ok {
		s.mu.Unlock()
		b.touch(time.Now())
		return b, nil
	}

	var evicted *UdpPortBinding
	if len(s.bindings) >= s.cfg.MaxUDPBindingsPerSession {
		evicted = s.evictOneLocked()
	}
	if len(s.bindings) >= s.cfg.MaxUDPBindingsPerSession {
		s.mu.Unlock()
		if evicted != nil {
			evicted.Close()
		}
		return nil, ErrTooManyBindings
	}

	b, err := newUdpPortBinding(guestPort, s.cfg, s.codec, s.queue, &s.clientSupportsV2)
	if err != nil {
		s.mu.Unlock()
		if evicted != nil {
			evicted.Close()
		}
		return nil, err
	}
	s.bindings[guestPort] = b
	s.wg.Add(1)
	go func() {
		defer s.wg.Done()
		b.readLoop()
	}()
	s.mu.Unlock()

	if evicted != nil {
		evicted.Close()
	}
	return b, nil
}

func (s *SessionRelay) evictOneLocked() *UdpPortBinding {
	var oldestPort uint16
	var oldest *UdpPortBinding
	var oldestTime time.Time

	for port, b := range s.bindings {
		t := b.LastUsed()
		if oldest == nil || t.Before(oldestTime) {
			oldest = b
			oldestPort = port
			oldestTime = t
		}
	}
	if oldest != nil {
		delete(s.bindings, oldestPort)
	}
	return oldest
}

func (s *SessionRelay) HandleDataChannelMessage(msg []byte) {
	if s.closed.Load() || s.ctx.Err() != nil {
		return
	}

	f, err := s.codec.DecodeFrame(msg)
	if err != nil {
		return
	}

	if f.Version == 2 {
		s.clientSupportsV2.Store(true)
	}

	if s.policy == nil {
		// Fail closed: a nil policy would turn the relay into an open UDP proxy.
		return
	}

	remoteIP := net.IP(f.RemoteIP.AsSlice())
	if err := s.policy.AllowUDP(remoteIP, f.RemotePort); err != nil {
		return
	}

	remote := net.UDPAddrFromAddrPort(netip.AddrPortFrom(f.RemoteIP, f.RemotePort))

	b, err := s.getOrCreateBinding(f.GuestPort)
	if err != nil {
		return
	}

	now := time.Now()
	b.touch(now)
	b.AllowRemote(remote, now)

	_ = b.WriteTo(remote, f.Payload)
}
