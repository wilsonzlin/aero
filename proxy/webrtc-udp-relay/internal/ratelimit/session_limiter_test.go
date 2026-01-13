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

