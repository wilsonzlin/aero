package ratelimit

import "sync"

type DropReason string

const (
	DropReasonRateLimited         DropReason = "rate_limited"
	DropReasonQuotaExceeded       DropReason = "quota_exceeded"
	DropReasonTooManyDestinations DropReason = "too_many_destinations"
)

type SessionConfig struct {
	UDPPacketsPerSecond int
	UDPBytesPerSecond   int

	DataChannelBytesPerSecond int

	UDPPacketsPerSecondPerDest int
	MaxUniqueDestinations      int
}

// SessionLimiter enforces per-session rate limits and destination quotas.
type SessionLimiter struct {
	clock Clock

	udpPackets *TokenBucket
	udpBytes   *TokenBucket
	dcBytes    *TokenBucket

	maxUniqueDestinations int

	perDestRate int64

	mu       sync.Mutex
	destSeen map[string]struct{}
	perDest  map[string]*TokenBucket
}

func NewSessionLimiter(clock Clock, cfg SessionConfig) *SessionLimiter {
	var udpPackets *TokenBucket
	if cfg.UDPPacketsPerSecond > 0 {
		udpPackets = NewTokenBucket(clock, int64(cfg.UDPPacketsPerSecond), int64(cfg.UDPPacketsPerSecond))
	}

	var udpBytes *TokenBucket
	if cfg.UDPBytesPerSecond > 0 {
		udpBytes = NewTokenBucket(clock, int64(cfg.UDPBytesPerSecond), int64(cfg.UDPBytesPerSecond))
	}

	var dcBytes *TokenBucket
	if cfg.DataChannelBytesPerSecond > 0 {
		dcBytes = NewTokenBucket(clock, int64(cfg.DataChannelBytesPerSecond), int64(cfg.DataChannelBytesPerSecond))
	}

	l := &SessionLimiter{
		clock:                 clock,
		udpPackets:            udpPackets,
		udpBytes:              udpBytes,
		dcBytes:               dcBytes,
		maxUniqueDestinations: cfg.MaxUniqueDestinations,
		perDestRate:           int64(cfg.UDPPacketsPerSecondPerDest),
		destSeen:              make(map[string]struct{}),
		perDest:               make(map[string]*TokenBucket),
	}
	return l
}

func (l *SessionLimiter) AllowUDPSend(destKey string, bytes int) (bool, DropReason) {
	if l.udpPackets != nil && !l.udpPackets.Allow(1) {
		return false, DropReasonRateLimited
	}
	if l.udpBytes != nil && !l.udpBytes.Allow(int64(bytes)) {
		return false, DropReasonRateLimited
	}

	if !l.trackDestination(destKey) {
		return false, DropReasonTooManyDestinations
	}

	if l.perDestRate <= 0 {
		return true, ""
	}

	bucket := l.getOrCreateDestBucket(destKey)
	if !bucket.Allow(1) {
		return false, DropReasonRateLimited
	}

	return true, ""
}

func (l *SessionLimiter) AllowDataChannelSend(bytes int) (bool, DropReason) {
	if l.dcBytes == nil {
		return true, ""
	}
	if !l.dcBytes.Allow(int64(bytes)) {
		return false, DropReasonRateLimited
	}
	return true, ""
}

func (l *SessionLimiter) trackDestination(destKey string) bool {
	if l.maxUniqueDestinations <= 0 {
		return true
	}

	l.mu.Lock()
	defer l.mu.Unlock()

	if _, ok := l.destSeen[destKey]; ok {
		return true
	}

	if len(l.destSeen) >= l.maxUniqueDestinations {
		return false
	}

	l.destSeen[destKey] = struct{}{}
	return true
}

func (l *SessionLimiter) getOrCreateDestBucket(destKey string) *TokenBucket {
	l.mu.Lock()
	defer l.mu.Unlock()

	if bucket, ok := l.perDest[destKey]; ok {
		return bucket
	}

	bucket := NewTokenBucket(l.clock, l.perDestRate, l.perDestRate)
	l.perDest[destKey] = bucket
	return bucket
}
