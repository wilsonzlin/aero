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

	b := &udpPortBinding{
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
	b := &udpPortBinding{
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

func TestUdpPortBinding_RemoteAllowlist_ExpiresByIdleTimeout(t *testing.T) {
	cfg := DefaultConfig()
	cfg.InboundFilterMode = InboundFilterAddressAndPort
	cfg.RemoteAllowlistIdleTimeout = 1 * time.Second

	b := &udpPortBinding{
		cfg:     cfg,
		allowed: make(map[remoteKey]time.Time),
	}

	remote := &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 10001}
	key, ok := makeRemoteKey(remote)
	if !ok {
		t.Fatalf("makeRemoteKey failed")
	}

	start := time.Unix(0, 0)
	b.AllowRemote(remote, start)

	// Allowed shortly after creation (and refreshes the timestamp).
	refreshAt := start.Add(500 * time.Millisecond)
	if ok := b.remoteAllowed(remote, refreshAt); !ok {
		t.Fatalf("expected remote to be allowed before TTL expiry")
	}

	// Refresh should extend TTL. If the timestamp were not refreshed at 500ms, this
	// would be denied (1.4s since initial allowlist entry).
	if ok := b.remoteAllowed(remote, start.Add(1400*time.Millisecond)); !ok {
		t.Fatalf("expected remote to remain allowed after refresh")
	}

	// Expires after idle timeout.
	if ok := b.remoteAllowed(remote, start.Add(2500*time.Millisecond)); ok {
		t.Fatalf("expected remote to be denied after TTL expiry")
	}

	b.allowedMu.Lock()
	_, present := b.allowed[key]
	b.allowedMu.Unlock()
	if present {
		t.Fatalf("expected remote allowlist entry to be removed after TTL expiry")
	}
}

func TestUdpPortBinding_AllowRemote_PrunesExpiredBeforeEvicting(t *testing.T) {
	cfg := DefaultConfig()
	cfg.InboundFilterMode = InboundFilterAddressAndPort
	cfg.RemoteAllowlistIdleTimeout = 1 * time.Second
	cfg.MaxAllowedRemotesPerBinding = 2

	m := metrics.New()
	b := &udpPortBinding{
		cfg:     cfg,
		metrics: m,
		allowed: make(map[remoteKey]time.Time),
	}

	remoteA := &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 10001}
	remoteB := &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 10002}
	remoteC := &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 10003}

	start := time.Unix(0, 0)
	b.AllowRemote(remoteA, start)
	b.AllowRemote(remoteB, start.Add(500*time.Millisecond))

	// Advance time so A is expired, but B is still (barely) valid. If AllowRemote
	// counts the stale A entry toward the cap, it would evict and increment the
	// eviction metric. Instead, we expect expired entries to be pruned before any
	// eviction occurs.
	b.AllowRemote(remoteC, start.Add(1500*time.Millisecond))

	keyA, _ := makeRemoteKey(remoteA)
	keyB, _ := makeRemoteKey(remoteB)
	keyC, _ := makeRemoteKey(remoteC)

	b.allowedMu.Lock()
	defer b.allowedMu.Unlock()
	if _, ok := b.allowed[keyA]; ok {
		t.Fatalf("expected expired remote (A) to be pruned before eviction")
	}
	if _, ok := b.allowed[keyB]; !ok {
		t.Fatalf("expected remote (B) to be retained")
	}
	if _, ok := b.allowed[keyC]; !ok {
		t.Fatalf("expected new remote (C) to be added")
	}

	if len(b.allowed) != cfg.MaxAllowedRemotesPerBinding {
		t.Fatalf("allowlist size=%d, want %d", len(b.allowed), cfg.MaxAllowedRemotesPerBinding)
	}
	if got := m.Get(metrics.UDPRemoteAllowlistEvictionsTotal); got != 0 {
		t.Fatalf("eviction metric=%d, want 0 (expired entry should be pruned, not evicted)", got)
	}
}

func TestUdpPortBinding_InboundFilterAny_IgnoresAllowlist(t *testing.T) {
	cfg := DefaultConfig()
	cfg.InboundFilterMode = InboundFilterAny
	cfg.RemoteAllowlistIdleTimeout = 1 * time.Second
	cfg.MaxAllowedRemotesPerBinding = 1

	m := metrics.New()
	b := &udpPortBinding{
		cfg:     cfg,
		metrics: m,
		allowed: make(map[remoteKey]time.Time),
	}

	remote := &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1), Port: 10001}
	now := time.Unix(0, 0)

	// In InboundFilterAny mode, outbound traffic should not mutate the allowlist.
	b.AllowRemote(remote, now)

	b.allowedMu.Lock()
	n := len(b.allowed)
	b.allowedMu.Unlock()
	if n != 0 {
		t.Fatalf("allowlist size=%d, want 0 (InboundFilterAny should not track remotes)", n)
	}

	// In InboundFilterAny mode, any inbound remote should be accepted regardless
	// of allowlist contents.
	if ok := b.remoteAllowed(remote, now.Add(2*time.Second)); !ok {
		t.Fatalf("expected remote to be allowed in InboundFilterAny mode")
	}

	b.allowedMu.Lock()
	n = len(b.allowed)
	b.allowedMu.Unlock()
	if n != 0 {
		t.Fatalf("allowlist size=%d after remoteAllowed, want 0 (InboundFilterAny should not track remotes)", n)
	}

	if got := m.Get(metrics.UDPRemoteAllowlistEvictionsTotal); got != 0 {
		t.Fatalf("eviction metric=%d, want 0 (InboundFilterAny should not evict)", got)
	}
	if got := m.Get(metrics.UDPRemoteAllowlistOverflowDropsTotal); got != 0 {
		t.Fatalf("drop metric=%d, want 0 (InboundFilterAny should not drop)", got)
	}
}
