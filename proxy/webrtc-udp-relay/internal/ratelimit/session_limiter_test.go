package ratelimit

import (
	"fmt"
	"testing"
)

func TestSessionLimiter_BoundsPerDestBucketCount(t *testing.T) {
	var evictions int
	l := NewSessionLimiter(nil, 0, 0, 0, 100, 0, 4, func() {
		evictions++
	})
	impl := l.(*sessionLimiterImpl)

	for i := 0; i < 10; i++ {
		dest := fmt.Sprintf("dest-%d", i)
		allowed, tooMany := l.AllowUDPSend(dest, 1)
		if !allowed {
			t.Fatalf("dest=%q unexpectedly rejected (too_many_destinations=%v)", dest, tooMany)
		}

		impl.mu.Lock()
		got := len(impl.perDest)
		impl.mu.Unlock()
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
	l := NewSessionLimiter(nil, 0, 0, 0, 100, 0, 2, func() {
		evictions++
	})
	impl := l.(*sessionLimiterImpl)

	for _, dest := range []string{"a", "b"} {
		allowed, tooMany := l.AllowUDPSend(dest, 1)
		if !allowed {
			t.Fatalf("dest=%q unexpectedly rejected (too_many_destinations=%v)", dest, tooMany)
		}
	}

	// Touch "a" again so that "b" becomes the LRU entry.
	if allowed, tooMany := l.AllowUDPSend("a", 1); !allowed {
		t.Fatalf("dest=%q unexpectedly rejected (too_many_destinations=%v)", "a", tooMany)
	}

	// Creating a new destination should evict "b" (the least recently used).
	if allowed, tooMany := l.AllowUDPSend("c", 1); !allowed {
		t.Fatalf("dest=%q unexpectedly rejected (too_many_destinations=%v)", "c", tooMany)
	}

	impl.mu.Lock()
	_, hasA := impl.perDest["a"]
	_, hasB := impl.perDest["b"]
	_, hasC := impl.perDest["c"]
	gotLen := len(impl.perDest)
	impl.mu.Unlock()

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
