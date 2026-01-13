package ratelimit

import (
	"fmt"
	"testing"
)

func TestSessionLimiter_BoundsPerDestBucketCount(t *testing.T) {
	var evictions int
	l := NewSessionLimiter(nil, SessionConfig{
		UDPPacketsPerSecondPerDest: 100,
		MaxUniqueDestinations:      0, // unlimited
		MaxUDPDestBuckets:          4,
		OnUDPDestBucketEvicted: func() {
			evictions++
		},
	})

	for i := 0; i < 10; i++ {
		dest := fmt.Sprintf("dest-%d", i)
		allowed, reason := l.AllowUDPSend(dest, 1)
		if !allowed {
			t.Fatalf("dest=%q unexpectedly rejected (reason=%q)", dest, reason)
		}

		l.mu.Lock()
		got := len(l.perDest)
		l.mu.Unlock()
		if got > 4 {
			t.Fatalf("perDest buckets=%d, want <= 4", got)
		}
	}

	if evictions != 6 {
		t.Fatalf("evictions=%d, want %d", evictions, 6)
	}
}

func TestSessionLimiter_PerDestBucketEvictionLRU(t *testing.T) {
	var evictions int
	l := NewSessionLimiter(nil, SessionConfig{
		UDPPacketsPerSecondPerDest: 100,
		MaxUniqueDestinations:      0, // unlimited
		MaxUDPDestBuckets:          2,
		OnUDPDestBucketEvicted: func() {
			evictions++
		},
	})

	for _, dest := range []string{"a", "b"} {
		allowed, reason := l.AllowUDPSend(dest, 1)
		if !allowed {
			t.Fatalf("dest=%q unexpectedly rejected (reason=%q)", dest, reason)
		}
	}

	// Touch "a" again so that "b" becomes the LRU entry.
	if allowed, reason := l.AllowUDPSend("a", 1); !allowed {
		t.Fatalf("dest=%q unexpectedly rejected (reason=%q)", "a", reason)
	}

	// Creating a new destination should evict "b" (the least recently used).
	if allowed, reason := l.AllowUDPSend("c", 1); !allowed {
		t.Fatalf("dest=%q unexpectedly rejected (reason=%q)", "c", reason)
	}

	l.mu.Lock()
	_, hasA := l.perDest["a"]
	_, hasB := l.perDest["b"]
	_, hasC := l.perDest["c"]
	gotLen := len(l.perDest)
	l.mu.Unlock()

	if gotLen != 2 {
		t.Fatalf("perDest buckets=%d, want 2", gotLen)
	}
	if !hasA || !hasC || hasB {
		t.Fatalf("LRU eviction mismatch: hasA=%v hasB=%v hasC=%v", hasA, hasB, hasC)
	}
	if evictions != 1 {
		t.Fatalf("evictions=%d, want %d", evictions, 1)
	}
}
