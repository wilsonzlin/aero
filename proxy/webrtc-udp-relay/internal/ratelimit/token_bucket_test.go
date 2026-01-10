package ratelimit

import (
	"sync"
	"testing"
	"time"
)

type fakeClock struct {
	mu  sync.Mutex
	now time.Time
}

func (c *fakeClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *fakeClock) Advance(d time.Duration) {
	c.mu.Lock()
	c.now = c.now.Add(d)
	c.mu.Unlock()
}

func TestTokenBucket_AllowAndRefill(t *testing.T) {
	clk := &fakeClock{now: time.Unix(0, 0)}
	b := NewTokenBucket(clk, 5, 5) // 5 tokens capacity, 5 tokens/sec.

	if !b.Allow(5) {
		t.Fatalf("expected initial burst to succeed")
	}
	if b.Allow(1) {
		t.Fatalf("expected bucket to be empty")
	}

	clk.Advance(200 * time.Millisecond) // 1 token refilled (5 tokens/sec).
	if !b.Allow(1) {
		t.Fatalf("expected refill after time advance")
	}
}

func TestTokenBucket_DoesNotExceedCapacity(t *testing.T) {
	clk := &fakeClock{now: time.Unix(0, 0)}
	b := NewTokenBucket(clk, 1, 1) // capacity 1 token.

	if !b.Allow(1) {
		t.Fatalf("expected initial token")
	}

	clk.Advance(10 * time.Second)
	if !b.Allow(1) {
		t.Fatalf("expected refill up to capacity")
	}
	if b.Allow(1) {
		t.Fatalf("expected capacity clamp (only 1 token available)")
	}
}
