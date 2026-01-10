package ratelimit

import (
	"sync"
	"time"
)

const nanoTokensPerToken int64 = int64(time.Second) // 1e9

const maxInt64 = int64(^uint64(0) >> 1)

// TokenBucket is a deterministic token bucket that refills at an integer
// rate (tokens/sec) using a provided Clock.
//
// The implementation uses fixed-point "nano-tokens" to avoid float rounding.
// One token is represented as 1e9 nano-tokens, so a rate of X tokens/sec adds
// X nano-tokens per nanosecond elapsed.
type TokenBucket struct {
	mu sync.Mutex

	clock Clock

	capacityTokens int64 // tokens
	fillRate       int64 // tokens/sec

	availableNanoTokens int64
	last                time.Time
}

func NewTokenBucket(clock Clock, capacityTokens, fillRate int64) *TokenBucket {
	if clock == nil {
		clock = RealClock{}
	}
	if capacityTokens < 0 {
		capacityTokens = 0
	}
	if fillRate < 0 {
		fillRate = 0
	}

	now := clock.Now()
	capacityNano := mulTokenToNano(capacityTokens)
	return &TokenBucket{
		clock:              clock,
		capacityTokens:     capacityTokens,
		fillRate:           fillRate,
		availableNanoTokens: capacityNano,
		last:               now,
	}
}

// Allow consumes the provided number of tokens if available.
//
// tokens <= 0 always succeeds.
func (b *TokenBucket) Allow(tokens int64) bool {
	if tokens <= 0 {
		return true
	}

	cost := mulTokenToNano(tokens)

	b.mu.Lock()
	defer b.mu.Unlock()

	b.refillLocked()

	if b.availableNanoTokens < cost {
		return false
	}

	b.availableNanoTokens -= cost
	return true
}

func (b *TokenBucket) refillLocked() {
	now := b.clock.Now()
	if now.Before(b.last) {
		// Time went backwards. Avoid refilling and move the reference point.
		b.last = now
		return
	}

	elapsed := now.Sub(b.last)
	if elapsed <= 0 {
		return
	}
	b.last = now

	if b.fillRate <= 0 || b.capacityTokens <= 0 {
		return
	}

	capacityNano := mulTokenToNano(b.capacityTokens)
	if b.availableNanoTokens >= capacityNano {
		b.availableNanoTokens = capacityNano
		return
	}

	need := capacityNano - b.availableNanoTokens
	elapsedNanos := elapsed.Nanoseconds()
	if elapsedNanos <= 0 {
		return
	}

	// fillRate is tokens/sec, which equals nanoTokens/ns when using the
	// nano-token fixed-point representation.
	rate := b.fillRate

	// Avoid overflow in elapsedNanos*rate: if we have enough time to fill the
	// bucket, just clamp to capacity.
	maxElapsedToFill := need / rate
	if maxElapsedToFill <= 0 || elapsedNanos >= maxElapsedToFill {
		b.availableNanoTokens = capacityNano
		return
	}

	b.availableNanoTokens += elapsedNanos * rate
	if b.availableNanoTokens > capacityNano {
		b.availableNanoTokens = capacityNano
	}
}

func mulTokenToNano(tokens int64) int64 {
	if tokens <= 0 {
		return 0
	}
	if tokens > maxInt64/nanoTokensPerToken {
		return maxInt64
	}
	return tokens * nanoTokensPerToken
}

