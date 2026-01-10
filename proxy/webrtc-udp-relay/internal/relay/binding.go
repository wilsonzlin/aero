package relay

import (
	"errors"
	"net"
	"sync"
	"sync/atomic"
	"time"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

type remoteKey struct {
	ip   [4]byte
	port uint16
}

func makeRemoteKey(addr *net.UDPAddr) (remoteKey, bool) {
	if addr == nil {
		return remoteKey{}, false
	}
	ip4 := addr.IP.To4()
	if ip4 == nil {
		return remoteKey{}, false
	}
	var k remoteKey
	copy(k.ip[:], ip4)
	k.port = uint16(addr.Port)
	return k, true
}

type UdpPortBinding struct {
	guestPort uint16
	conn      *net.UDPConn
	cfg       Config
	codec     udpproto.Codec
	queue     *sendQueue

	lastUsed atomic.Int64

	allowedMu sync.Mutex
	allowed   map[remoteKey]time.Time

	closed atomic.Bool
	once   sync.Once
}

func newUdpPortBinding(guestPort uint16, cfg Config, codec udpproto.Codec, queue *sendQueue) (*UdpPortBinding, error) {
	conn, err := net.ListenUDP("udp4", &net.UDPAddr{IP: net.IPv4zero, Port: 0})
	if err != nil {
		return nil, err
	}
	b := &UdpPortBinding{
		guestPort: guestPort,
		conn:      conn,
		cfg:       cfg,
		codec:     codec,
		queue:     queue,
		allowed:   make(map[remoteKey]time.Time),
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
		_ = b.conn.Close()
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
	b.allowedMu.Unlock()
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
	buf := make([]byte, b.cfg.UDPReadBufferBytes)
	for {
		n, remote, err := b.conn.ReadFromUDP(buf)
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

		ip4 := remote.IP.To4()
		if ip4 == nil {
			continue
		}
		var rip [4]byte
		copy(rip[:], ip4)

		frame, err := b.codec.EncodeDatagram(udpproto.Datagram{
			GuestPort:  b.guestPort,
			RemoteIP:   rip,
			RemotePort: uint16(remote.Port),
			Payload:    buf[:n],
		}, nil)
		if err != nil {
			continue
		}
		b.queue.Enqueue(frame)
	}
}
