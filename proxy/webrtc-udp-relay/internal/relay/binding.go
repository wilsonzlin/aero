package relay

import (
	"bytes"
	"errors"
	"net"
	"net/netip"
	"sync"
	"sync/atomic"
	"time"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

type remoteKey netip.AddrPort

func makeRemoteKey(addr *net.UDPAddr) (remoteKey, bool) {
	if addr == nil {
		return remoteKey{}, false
	}
	ap := addr.AddrPort()
	if !ap.Addr().IsValid() {
		return remoteKey{}, false
	}
	return remoteKey(ap), true
}

type udpPortBinding struct {
	guestPort uint16
	conn4     *net.UDPConn
	conn6     *net.UDPConn
	cfg       Config
	codec     udpproto.Codec
	queue     *sendQueue
	session   *Session
	metrics   *metrics.Metrics

	lastUsed atomic.Int64

	allowedMu sync.Mutex
	allowed   map[remoteKey]time.Time
	lastPrune time.Time

	clientSupportsV2 *atomic.Bool

	closed atomic.Bool
	once   sync.Once
}

func newUdpPortBinding(guestPort uint16, cfg Config, codec udpproto.Codec, queue *sendQueue, clientSupportsV2 *atomic.Bool, session *Session, m *metrics.Metrics) (*udpPortBinding, error) {
	conn4, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4zero, Port: 0})
	if err != nil {
		return nil, err
	}

	// IPv6 is optional at runtime: if the host/kernel doesn't support it, we
	// still keep IPv4 relay working.
	conn6, err := net.ListenUDP("udp6", &net.UDPAddr{IP: net.IPv6zero, Port: 0})
	if err != nil {
		conn6 = nil
	}

	b := &udpPortBinding{
		guestPort:        guestPort,
		conn4:            conn4,
		conn6:            conn6,
		cfg:              cfg,
		codec:            codec,
		queue:            queue,
		session:          session,
		metrics:          m,
		allowed:          make(map[remoteKey]time.Time),
		clientSupportsV2: clientSupportsV2,
	}
	b.touch(time.Now())
	return b, nil
}

func (b *udpPortBinding) touch(now time.Time) {
	b.lastUsed.Store(now.UnixNano())
}

func (b *udpPortBinding) LastUsed() time.Time {
	return time.Unix(0, b.lastUsed.Load())
}

func (b *udpPortBinding) Close() {
	b.once.Do(func() {
		b.closed.Store(true)
		_ = b.conn4.Close()
		if b.conn6 != nil {
			_ = b.conn6.Close()
		}
	})
}

func (b *udpPortBinding) AllowRemote(remote *net.UDPAddr, now time.Time) {
	if b.cfg.InboundFilterMode == InboundFilterAny {
		return
	}
	k, ok := makeRemoteKey(remote)
	if !ok {
		return
	}
	b.allowedMu.Lock()
	if _, exists := b.allowed[k]; exists {
		b.allowed[k] = now
		maxAllowed := b.cfg.MaxAllowedRemotesPerBinding
		if maxAllowed > 0 && len(b.allowed) > maxAllowed {
			evicted := b.evictOldestAllowedLocked(len(b.allowed) - maxAllowed)
			if evicted > 0 && b.metrics != nil {
				b.metrics.Add(metrics.UDPRemoteAllowlistEvictionsTotal, uint64(evicted))
			}
		}
		b.pruneAllowedLocked(now)
		b.allowedMu.Unlock()
		return
	}

	maxAllowed := b.cfg.MaxAllowedRemotesPerBinding
	if maxAllowed > 0 {
		desiredSize := len(b.allowed) + 1
		if desiredSize > maxAllowed {
			// Prune expired entries before evicting due to the cap. This prevents
			// stale allowlist entries from inflating eviction metrics and avoids
			// evicting active remotes when expired entries would have been removed
			// anyway.
			//
			// Note: evictOldestAllowedLocked is already O(n), so forcing a prune in
			// this path does not change the asymptotic complexity.
			b.pruneAllowedLockedForce(now)
			desiredSize = len(b.allowed) + 1
			if desiredSize <= maxAllowed {
				// Pruning freed enough space; no eviction required.
				b.allowed[k] = now
				b.allowedMu.Unlock()
				return
			}
			evicted := b.evictOldestAllowedLocked(desiredSize - maxAllowed)
			if evicted > 0 && b.metrics != nil {
				b.metrics.Add(metrics.UDPRemoteAllowlistEvictionsTotal, uint64(evicted))
			}
		}
	}

	// Add after eviction so the allowlist never temporarily exceeds the cap.
	b.allowed[k] = now
	b.pruneAllowedLocked(now)
	b.allowedMu.Unlock()
}

func remoteKeyLess(a, b remoteKey) bool {
	apa := netip.AddrPort(a)
	apb := netip.AddrPort(b)
	aa := apa.Addr().As16()
	ab := apb.Addr().As16()
	if c := bytes.Compare(aa[:], ab[:]); c != 0 {
		return c < 0
	}
	return apa.Port() < apb.Port()
}

func (b *udpPortBinding) evictOldestAllowedLocked(n int) int {
	if n <= 0 {
		return 0
	}
	evicted := 0
	for evicted < n && len(b.allowed) > 0 {
		var oldestKey remoteKey
		var oldestTS time.Time
		oldestSet := false

		for k, ts := range b.allowed {
			if !oldestSet || ts.Before(oldestTS) || (ts.Equal(oldestTS) && remoteKeyLess(k, oldestKey)) {
				oldestKey = k
				oldestTS = ts
				oldestSet = true
			}
		}
		if !oldestSet {
			break
		}
		delete(b.allowed, oldestKey)
		evicted++
	}
	return evicted
}

func (b *udpPortBinding) pruneAllowedLocked(now time.Time) {
	b.pruneAllowedLockedInternal(now, false)
}

func (b *udpPortBinding) pruneAllowedLockedForce(now time.Time) {
	b.pruneAllowedLockedInternal(now, true)
}

func (b *udpPortBinding) pruneAllowedLockedInternal(now time.Time, force bool) {
	if b.cfg.RemoteAllowlistIdleTimeout <= 0 {
		return
	}
	// Prune at most once per RemoteAllowlistIdleTimeout to avoid turning every
	// outbound packet into an O(n) scan.
	if !force && !b.lastPrune.IsZero() && now.Sub(b.lastPrune) <= b.cfg.RemoteAllowlistIdleTimeout {
		return
	}

	cutoff := now.Add(-b.cfg.RemoteAllowlistIdleTimeout)
	for k, ts := range b.allowed {
		if ts.Before(cutoff) {
			delete(b.allowed, k)
		}
	}
	b.lastPrune = now
}

func (b *udpPortBinding) remoteAllowed(remote *net.UDPAddr, now time.Time) bool {
	if b.cfg.InboundFilterMode == InboundFilterAny {
		return true
	}
	k, ok := makeRemoteKey(remote)
	if !ok {
		return false
	}
	b.allowedMu.Lock()
	defer b.allowedMu.Unlock()
	last, ok := b.allowed[k]
	if !ok {
		return false
	}
	if b.cfg.RemoteAllowlistIdleTimeout > 0 && now.Sub(last) > b.cfg.RemoteAllowlistIdleTimeout {
		delete(b.allowed, k)
		return false
	}
	// Refresh timestamp (keeps active flows alive, while still allowing expiry).
	b.allowed[k] = now
	return true
}

func (b *udpPortBinding) readLoop() {
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		b.readLoopConn(b.conn4)
	}()
	if b.conn6 != nil {
		wg.Add(1)
		go func() {
			defer wg.Done()
			b.readLoopConn(b.conn6)
		}()
	}
	wg.Wait()
}

func (b *udpPortBinding) readLoopConn(conn *net.UDPConn) {
	var metricsSink *metrics.Metrics
	if b.session != nil {
		metricsSink = b.session.metrics
	}

	// Allocate enough space to reliably detect oversized UDP datagrams.
	//
	// Go's UDPConn.ReadFromUDP silently truncates datagrams larger than the
	// provided buffer. If we were to forward the truncated bytes, the client
	// would observe a corrupted payload. To prevent this, ensure the read buffer
	// is at least MaxDatagramPayloadBytes+1 and drop any datagram whose observed
	// length exceeds MaxDatagramPayloadBytes.
	//
	// Note: Config validation in internal/config ensures UDPReadBufferBytes is
	// never < MaxDatagramPayloadBytes+1 in production, but keep this guard to
	// avoid surprising behavior in tests or standalone usage.
	bufLen := b.cfg.UDPReadBufferBytes
	minBufLen := b.cfg.MaxDatagramPayloadBytes + 1
	if bufLen < minBufLen {
		bufLen = minBufLen
	}
	buf := make([]byte, bufLen)
	for {
		n, remote, err := conn.ReadFromUDP(buf)
		if err != nil {
			if errors.Is(err, net.ErrClosed) || b.closed.Load() {
				return
			}
			// Transient read error; keep going.
			continue
		}
		now := time.Now()
		b.touch(now)

		// Drop oversized datagrams instead of forwarding a truncated payload.
		// When bufLen == MaxDatagramPayloadBytes+1, n==bufLen implies the peer sent
		// at least MaxDatagramPayloadBytes+1 bytes, which is always oversized.
		if n > b.cfg.MaxDatagramPayloadBytes {
			if metricsSink != nil {
				metricsSink.Inc(metrics.WebRTCUDPDropped)
				metricsSink.Inc(metrics.WebRTCUDPDroppedOversized)
			}
			continue
		}

		if !b.remoteAllowed(remote, now) {
			// Record drops due to inbound filtering (i.e. the remote endpoint is not
			// currently on the allowlist).
			//
			// This metric is intentionally transport-agnostic (used by both WebRTC and
			// /udp WebSocket relays); it is not included in the WebRTC/UDPWS dropped
			// counters.
			if b.metrics != nil {
				b.metrics.Inc(metrics.UDPRemoteAllowlistOverflowDropsTotal)
			}
			continue
		}

		ap := remote.AddrPort()
		if !ap.Addr().IsValid() {
			continue
		}

		frame := udpproto.Frame{
			GuestPort:  b.guestPort,
			RemoteIP:   ap.Addr(),
			RemotePort: ap.Port(),
			Payload:    buf[:n],
		}

		var out []byte
		if frame.RemoteIP.Is6() {
			out, err = b.codec.EncodeFrameV2(frame)
		} else {
			useV2 := b.cfg.PreferV2 && b.clientSupportsV2 != nil && b.clientSupportsV2.Load()
			if useV2 {
				out, err = b.codec.EncodeFrameV2(frame)
			} else {
				out, err = b.codec.EncodeFrameV1(frame)
			}
		}
		if err != nil {
			continue
		}
		if b.session != nil && !b.session.HandleInboundToClient(out) {
			if metricsSink != nil {
				metricsSink.Inc(metrics.WebRTCUDPDropped)
				metricsSink.Inc(metrics.WebRTCUDPDroppedRateLimited)
			}
			continue
		}
		if b.queue.Enqueue(out) {
			if metricsSink != nil {
				metricsSink.Inc(metrics.WebRTCUDPDatagramsOut)
			}
		}
	}
}

func (b *udpPortBinding) WriteTo(remote *net.UDPAddr, payload []byte) error {
	if remote == nil {
		return errors.New("udp binding: remote is nil")
	}
	if ip4 := remote.IP.To4(); ip4 != nil {
		_, err := b.conn4.WriteToUDP(payload, remote)
		return err
	}
	if b.conn6 == nil {
		return errors.New("udp binding: ipv6 not supported")
	}
	_, err := b.conn6.WriteToUDP(payload, remote)
	return err
}
