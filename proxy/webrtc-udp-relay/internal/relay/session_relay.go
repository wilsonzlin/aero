package relay

import (
	"context"
	"errors"
	"net"
	"net/netip"
	"sync"
	"sync/atomic"
	"time"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/ratelimit"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

// dataChannelSender is the subset of pion/webrtc's DataChannel used by the relay.
type dataChannelSender interface {
	Send(data []byte) error
}

// destinationPolicy is implemented by policy.DestinationPolicy.
type destinationPolicy interface {
	AllowUDP(remoteIP net.IP, remotePort uint16) error
}

// SessionRelay relays UDP datagrams between a WebRTC DataChannel and the public
// network.
//
// A SessionRelay is bound to exactly one DataChannel ("udp") and multiplexes
// guest-port semantics by maintaining a UDP socket per guest port.
type SessionRelay struct {
	dc      dataChannelSender
	cfg     Config
	policy  destinationPolicy
	codec   udpproto.Codec
	session *Session
	metrics *metrics.Metrics

	ctx    context.Context
	cancel context.CancelFunc

	queue *sendQueue

	mu       sync.Mutex
	bindings map[uint16]*udpPortBinding

	wg sync.WaitGroup

	closeOnce sync.Once
	closed    atomic.Bool

	clientSupportsV2 atomic.Bool
}

func NewSessionRelay(dc dataChannelSender, cfg Config, policy destinationPolicy, session *Session, m *metrics.Metrics) *SessionRelay {
	cfg = cfg.withDefaults()
	codec, err := udpproto.NewCodec(cfg.MaxDatagramPayloadBytes)
	if err != nil {
		codec = udpproto.DefaultCodec
	}

	ctx, cancel := context.WithCancel(context.Background())

	if m == nil && session != nil {
		m = session.metrics
	}
	s := &SessionRelay{
		dc:       dc,
		cfg:      cfg,
		policy:   policy,
		codec:    codec,
		session:  session,
		metrics:  m,
		ctx:      ctx,
		cancel:   cancel,
		queue:    newSendQueue(cfg.DataChannelSendQueueBytes),
		bindings: make(map[uint16]*udpPortBinding),
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

// EnableWebRTCUDPMetrics configures the send queue drop hook used to track
// backpressure drops on the primary WebRTC DataChannel relay path.
//
// When the relay is running without a quota/session manager (s.session == nil),
// this method is a no-op.
func (s *SessionRelay) EnableWebRTCUDPMetrics() {
	if s == nil || s.queue == nil || s.session == nil || s.session.metrics == nil {
		return
	}
	m := s.session.metrics
	s.queue.SetOnDrop(func() {
		m.Inc(metrics.WebRTCUDPDropped)
		m.Inc(metrics.WebRTCUDPDroppedBackpressure)
	})
}

func (s *SessionRelay) Close() {
	s.closeOnce.Do(func() {
		s.closed.Store(true)
		s.cancel()
		s.queue.Close()

		var toClose []*udpPortBinding
		s.mu.Lock()
		for _, b := range s.bindings {
			toClose = append(toClose, b)
		}
		s.bindings = make(map[uint16]*udpPortBinding)
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
	var toClose []*udpPortBinding

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

func (s *SessionRelay) getOrCreateBinding(guestPort uint16) (*udpPortBinding, error) {
	s.mu.Lock()
	if s.closed.Load() {
		s.mu.Unlock()
		return nil, errSessionClosed
	}
	if b, ok := s.bindings[guestPort]; ok {
		s.mu.Unlock()
		b.touch(time.Now())
		return b, nil
	}

	var evicted *udpPortBinding
	if len(s.bindings) >= s.cfg.MaxUDPBindingsPerSession {
		evicted = s.evictOneLocked()
	}
	if len(s.bindings) >= s.cfg.MaxUDPBindingsPerSession {
		s.mu.Unlock()
		if evicted != nil {
			evicted.Close()
		}
		return nil, errTooManyBindings
	}

	b, err := newUdpPortBinding(guestPort, s.cfg, s.codec, s.queue, &s.clientSupportsV2, s.session, s.metrics)
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

func (s *SessionRelay) evictOneLocked() *udpPortBinding {
	var oldestPort uint16
	var oldest *udpPortBinding
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

	var metricsSink *metrics.Metrics
	if s.session != nil {
		metricsSink = s.session.metrics
	}
	if metricsSink != nil {
		metricsSink.Inc(metrics.WebRTCUDPDatagramsIn)
	}

	f, err := s.codec.DecodeFrame(msg)
	if err != nil {
		if metricsSink != nil {
			metricsSink.Inc(metrics.WebRTCUDPDropped)
			if errors.Is(err, udpproto.ErrPayloadTooLarge) {
				metricsSink.Inc(metrics.WebRTCUDPDroppedOversized)
			} else {
				metricsSink.Inc(metrics.WebRTCUDPDroppedMalformed)
			}
		}
		return
	}

	if f.Version == 2 {
		s.clientSupportsV2.Store(true)
	}

	if s.policy == nil {
		// Fail closed: a nil policy would turn the relay into an open UDP proxy.
		if metricsSink != nil {
			metricsSink.Inc(metrics.WebRTCUDPDropped)
			metricsSink.Inc(metrics.WebRTCUDPDroppedDeniedByPolicy)
		}
		return
	}

	remoteIP := net.IP(f.RemoteIP.AsSlice())
	if err := s.policy.AllowUDP(remoteIP, f.RemotePort); err != nil {
		if metricsSink != nil {
			metricsSink.Inc(metrics.WebRTCUDPDropped)
			metricsSink.Inc(metrics.WebRTCUDPDroppedDeniedByPolicy)
		}
		return
	}

	remote := net.UDPAddrFromAddrPort(netip.AddrPortFrom(f.RemoteIP, f.RemotePort))

	if s.session != nil {
		destKey := netip.AddrPortFrom(f.RemoteIP, f.RemotePort).String()
		allowed, reason := s.session.AllowClientDatagramWithReason(destKey, f.Payload)
		if !allowed {
			if metricsSink != nil {
				metricsSink.Inc(metrics.WebRTCUDPDropped)
				if reason == ratelimit.DropReasonTooManyDestinations {
					metricsSink.Inc(metrics.WebRTCUDPDroppedQuotaExceeded)
				} else {
					metricsSink.Inc(metrics.WebRTCUDPDroppedRateLimited)
				}
			}
			return
		}
	}

	b, err := s.getOrCreateBinding(f.GuestPort)
	if err != nil {
		if metricsSink != nil {
			metricsSink.Inc(metrics.WebRTCUDPDropped)
			if errors.Is(err, errTooManyBindings) {
				metricsSink.Inc(metrics.WebRTCUDPDroppedTooManyBindings)
			}
		}
		return
	}

	now := time.Now()
	b.touch(now)
	b.AllowRemote(remote, now)

	_ = b.WriteTo(remote, f.Payload)
}
