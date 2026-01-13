package relay

import (
	"net"
	"testing"
	"time"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
)

func TestUdpPortBinding_AllowRemote_Capped(t *testing.T) {
	cfg := DefaultConfig()
	cfg.InboundFilterMode = InboundFilterAddressAndPort
	cfg.RemoteAllowlistIdleTimeout = time.Minute
	cfg.MaxAllowedRemotesPerBinding = 1024

	b := &UdpPortBinding{
		cfg:     cfg,
		allowed: make(map[remoteKey]time.Time),
	}

	const total = 10_000
	for i := 0; i < total; i++ {
		remote := &net.UDPAddr{
			IP:   net.IPv4(127, 0, 0, 1),
			Port: 10000 + i,
		}
		b.AllowRemote(remote, time.Unix(0, int64(i)))

		b.allowedMu.Lock()
		n := len(b.allowed)
		b.allowedMu.Unlock()
		if n > cfg.MaxAllowedRemotesPerBinding {
			t.Fatalf("allowlist size exceeded cap: got %d, cap %d", n, cfg.MaxAllowedRemotesPerBinding)
		}
	}
}

func TestUdpPortBinding_AllowRemote_EvictsOldest(t *testing.T) {
	cfg := DefaultConfig()
	cfg.InboundFilterMode = InboundFilterAddressAndPort
	cfg.RemoteAllowlistIdleTimeout = time.Minute
	cfg.MaxAllowedRemotesPerBinding = 3

	m := metrics.New()
	b := &UdpPortBinding{
		cfg:     cfg,
		metrics: m,
		allowed: make(map[remoteKey]time.Time),
	}

	remoteA := &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 10001}
	remoteB := &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 10002}
	remoteC := &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 10003}
	remoteD := &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 10004}

	base := time.Unix(0, 0)
	b.AllowRemote(remoteA, base)
	b.AllowRemote(remoteB, base.Add(1*time.Second))
	b.AllowRemote(remoteC, base.Add(2*time.Second))

	// Refresh A so it is no longer the oldest.
	b.AllowRemote(remoteA, base.Add(3*time.Second))

	// Adding D should evict the oldest entry (B).
	b.AllowRemote(remoteD, base.Add(4*time.Second))

	keyA, _ := makeRemoteKey(remoteA)
	keyB, _ := makeRemoteKey(remoteB)
	keyC, _ := makeRemoteKey(remoteC)
	keyD, _ := makeRemoteKey(remoteD)

	b.allowedMu.Lock()
	defer b.allowedMu.Unlock()
	if len(b.allowed) != cfg.MaxAllowedRemotesPerBinding {
		t.Fatalf("allowlist size=%d, want %d", len(b.allowed), cfg.MaxAllowedRemotesPerBinding)
	}
	if _, ok := b.allowed[keyB]; ok {
		t.Fatalf("expected oldest remote (B) to be evicted")
	}
	if _, ok := b.allowed[keyA]; !ok {
		t.Fatalf("expected refreshed remote (A) to be retained")
	}
	if _, ok := b.allowed[keyC]; !ok {
		t.Fatalf("expected remote (C) to be retained")
	}
	if _, ok := b.allowed[keyD]; !ok {
		t.Fatalf("expected new remote (D) to be added")
	}

	if got := m.Get(metrics.UDPRemoteAllowlistEvictionsTotal); got != 1 {
		t.Fatalf("eviction metric=%d, want 1", got)
	}
}
