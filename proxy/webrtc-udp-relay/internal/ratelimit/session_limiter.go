package ratelimit

import (
	"container/list"
	"sync"
)

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

	// MaxUDPDestBuckets bounds the number of per-destination token buckets kept
	// for UDPPacketsPerSecondPerDest.
	//
	// When <= 0, a safe default is used (see NewSessionLimiter).
	MaxUDPDestBuckets int

	// OnUDPDestBucketEvicted is invoked once per evicted per-destination bucket.
	//
	// It is invoked outside of SessionLimiter's mutex.
	OnUDPDestBucketEvicted func()
}

// SessionLimiter enforces per-session rate limits and destination quotas.
type SessionLimiter struct {
	clock Clock

	udpPackets *tokenBucket
	udpBytes   *tokenBucket
	dcBytes    *tokenBucket

	maxUniqueDestinations int

	perDestRate int64

	maxUDPDestBuckets int

	onUDPDestBucketEvicted func()

	mu       sync.Mutex
	destSeen map[string]struct{}
	perDest  map[string]*destBucketEntry
	perDestQ *list.List
}

type destBucketEntry struct {
	bucket *tokenBucket
	elem   *list.Element
}

func NewSessionLimiter(clock Clock, cfg SessionConfig) *SessionLimiter {
	var udpPackets *tokenBucket
	if cfg.UDPPacketsPerSecond > 0 {
		udpPackets = NewTokenBucket(clock, int64(cfg.UDPPacketsPerSecond), int64(cfg.UDPPacketsPerSecond))
	}

	var udpBytes *tokenBucket
	if cfg.UDPBytesPerSecond > 0 {
		udpBytes = NewTokenBucket(clock, int64(cfg.UDPBytesPerSecond), int64(cfg.UDPBytesPerSecond))
	}

	var dcBytes *tokenBucket
	if cfg.DataChannelBytesPerSecond > 0 {
		dcBytes = NewTokenBucket(clock, int64(cfg.DataChannelBytesPerSecond), int64(cfg.DataChannelBytesPerSecond))
	}

	maxUDPDestBuckets := cfg.MaxUDPDestBuckets
	if maxUDPDestBuckets <= 0 {
		// Default to the unique destination cap (when configured) to keep the
		// per-dest limiter state consistent with quota enforcement.
		if cfg.MaxUniqueDestinations > 0 {
			maxUDPDestBuckets = cfg.MaxUniqueDestinations
		} else {
			// Safe bound to prevent unbounded memory growth on destination spray.
			maxUDPDestBuckets = 1024
		}
	}

	l := &SessionLimiter{
		clock:                  clock,
		udpPackets:             udpPackets,
		udpBytes:               udpBytes,
		dcBytes:                dcBytes,
		maxUniqueDestinations:  cfg.MaxUniqueDestinations,
		perDestRate:            int64(cfg.UDPPacketsPerSecondPerDest),
		maxUDPDestBuckets:      maxUDPDestBuckets,
		onUDPDestBucketEvicted: cfg.OnUDPDestBucketEvicted,
		destSeen:               make(map[string]struct{}),
		perDest:                make(map[string]*destBucketEntry),
		perDestQ:               list.New(),
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

func (l *SessionLimiter) getOrCreateDestBucket(destKey string) *tokenBucket {
	var (
		bucket  *tokenBucket
		onEvict func()
	)

	l.mu.Lock()

	if entry, ok := l.perDest[destKey]; ok {
		l.perDestQ.MoveToFront(entry.elem)
		bucket = entry.bucket
		l.mu.Unlock()
		return bucket
	}

	if l.maxUDPDestBuckets > 0 && len(l.perDest) >= l.maxUDPDestBuckets {
		// Evict least-recently used entry (oldest at the back).
		if elem := l.perDestQ.Back(); elem != nil {
			evictKey := elem.Value.(string)
			l.perDestQ.Remove(elem)
			delete(l.perDest, evictKey)
			onEvict = l.onUDPDestBucketEvicted
		}
	}

	bucket = NewTokenBucket(l.clock, l.perDestRate, l.perDestRate)
	elem := l.perDestQ.PushFront(destKey)
	l.perDest[destKey] = &destBucketEntry{
		bucket: bucket,
		elem:   elem,
	}

	l.mu.Unlock()

	if onEvict != nil {
		onEvict()
	}
	return bucket
}
