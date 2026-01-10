package relay

import (
	"errors"
	"net"
	"net/netip"
	"sync"
	"sync/atomic"
	"time"

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

type UdpPortBinding struct {
	guestPort uint16
	conn4     *net.UDPConn
	conn6     *net.UDPConn
	cfg       Config
	codec     udpproto.Codec
	queue     *sendQueue

	lastUsed atomic.Int64

	allowedMu sync.Mutex
	allowed   map[remoteKey]time.Time
	lastPrune time.Time

	clientSupportsV2 *atomic.Bool

	closed atomic.Bool
	once   sync.Once
}

func newUdpPortBinding(guestPort uint16, cfg Config, codec udpproto.Codec, queue *sendQueue, clientSupportsV2 *atomic.Bool) (*UdpPortBinding, error) {
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

	b := &UdpPortBinding{
		guestPort:        guestPort,
		conn4:            conn4,
		conn6:            conn6,
		cfg:              cfg,
		codec:            codec,
		queue:            queue,
		allowed:          make(map[remoteKey]time.Time),
		clientSupportsV2: clientSupportsV2,
	}
	b.touch(time.Now())
	return b, nil
}

func (b *UdpPortBinding) touch(now time.Time) {
	b.lastUsed.Store(now.UnixNano())
}

func (b *UdpPortBinding) LastUsed() time.Time {
	return time.Unix(0, b.lastUsed.Load())
}

func (b *UdpPortBinding) Close() {
	b.once.Do(func() {
		b.closed.Store(true)
		_ = b.conn4.Close()
		if b.conn6 != nil {
			_ = b.conn6.Close()
		}
	})
}

func (b *UdpPortBinding) AllowRemote(remote *net.UDPAddr, now time.Time) {
	if b.cfg.InboundFilterMode == InboundFilterAny {
		return
	}
	k, ok := makeRemoteKey(remote)
	if !ok {
		return
	}
	b.allowedMu.Lock()
	b.allowed[k] = now
	b.pruneAllowedLocked(now)
	b.allowedMu.Unlock()
}

const maxAllowedRemotesBeforePrune = 1024

func (b *UdpPortBinding) pruneAllowedLocked(now time.Time) {
	if b.cfg.RemoteAllowlistIdleTimeout <= 0 {
		return
	}
	// Prune at most once per RemoteAllowlistIdleTimeout to avoid turning every
	// outbound packet into an O(n) scan. Also trigger pruning when the allowlist
	// grows large to avoid unbounded memory growth when the guest sprays packets
	// at many destinations on the same guest port.
	if len(b.allowed) <= maxAllowedRemotesBeforePrune && !b.lastPrune.IsZero() && now.Sub(b.lastPrune) <= b.cfg.RemoteAllowlistIdleTimeout {
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

func (b *UdpPortBinding) remoteAllowed(remote *net.UDPAddr, now time.Time) bool {
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

func (b *UdpPortBinding) readLoop() {
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

func (b *UdpPortBinding) readLoopConn(conn *net.UDPConn) {
	buf := make([]byte, b.cfg.UDPReadBufferBytes)
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

		if !b.remoteAllowed(remote, now) {
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
		b.queue.Enqueue(out)
	}
}

func (b *UdpPortBinding) WriteTo(remote *net.UDPAddr, payload []byte) error {
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
