package ratelimit

import (
	"container/list"
	"sync"
)

// sessionLimiter enforces per-session rate limits and destination quotas.
type sessionLimiter interface {
	// AllowUDPSend reports whether a UDP datagram is allowed to be sent.
	//
	// If allowed is false, tooManyDestinations is true only when the send was
	// rejected due to MaxUniqueDestinations.
	AllowUDPSend(destKey string, bytes int) (allowed bool, tooManyDestinations bool)

	// AllowDataChannelSend reports whether a DataChannel send of the given size
	// is allowed under the session's byte/sec budget.
	AllowDataChannelSend(bytes int) bool
}

type sessionLimiterImpl struct {
	clock clock

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

type sessionLimiterConfig struct {
	udpPacketsPerSecond int
	udpBytesPerSecond   int

	dataChannelBytesPerSecond int

	udpPacketsPerSecondPerDest int
	maxUniqueDestinations      int

	// maxUDPDestBuckets bounds the number of per-destination token buckets kept
	// for udpPacketsPerSecondPerDest. When <= 0, a safe default is used.
	maxUDPDestBuckets int

	// onUDPDestBucketEvicted is invoked once per evicted per-destination bucket,
	// outside of sessionLimiterImpl's mutex.
	onUDPDestBucketEvicted func()
}

func NewSessionLimiter(
	clock clock,
	udpPacketsPerSecond int,
	udpBytesPerSecond int,
	dataChannelBytesPerSecond int,
	udpPacketsPerSecondPerDest int,
	maxUniqueDestinations int,
	maxUDPDestBuckets int,
	onUDPDestBucketEvicted func(),
) sessionLimiter {
	return newSessionLimiter(clock, sessionLimiterConfig{
		udpPacketsPerSecond:        udpPacketsPerSecond,
		udpBytesPerSecond:          udpBytesPerSecond,
		dataChannelBytesPerSecond:  dataChannelBytesPerSecond,
		udpPacketsPerSecondPerDest: udpPacketsPerSecondPerDest,
		maxUniqueDestinations:      maxUniqueDestinations,
		maxUDPDestBuckets:          maxUDPDestBuckets,
		onUDPDestBucketEvicted:     onUDPDestBucketEvicted,
	})
}

func newSessionLimiter(clock clock, cfg sessionLimiterConfig) *sessionLimiterImpl {
	var udpPackets *tokenBucket
	if cfg.udpPacketsPerSecond > 0 {
		udpPackets = newTokenBucket(clock, int64(cfg.udpPacketsPerSecond), int64(cfg.udpPacketsPerSecond))
	}

	var udpBytes *tokenBucket
	if cfg.udpBytesPerSecond > 0 {
		udpBytes = newTokenBucket(clock, int64(cfg.udpBytesPerSecond), int64(cfg.udpBytesPerSecond))
	}

	var dcBytes *tokenBucket
	if cfg.dataChannelBytesPerSecond > 0 {
		dcBytes = newTokenBucket(clock, int64(cfg.dataChannelBytesPerSecond), int64(cfg.dataChannelBytesPerSecond))
	}

	maxUDPDestBuckets := cfg.maxUDPDestBuckets
	if maxUDPDestBuckets <= 0 {
		// Default to the unique destination cap (when configured) to keep the
		// per-dest limiter state consistent with quota enforcement.
		if cfg.maxUniqueDestinations > 0 {
			maxUDPDestBuckets = cfg.maxUniqueDestinations
		} else {
			// Safe bound to prevent unbounded memory growth on destination spray.
			maxUDPDestBuckets = 1024
		}
	}

	l := &sessionLimiterImpl{
		clock:                  clock,
		udpPackets:             udpPackets,
		udpBytes:               udpBytes,
		dcBytes:                dcBytes,
		maxUniqueDestinations:  cfg.maxUniqueDestinations,
		perDestRate:            int64(cfg.udpPacketsPerSecondPerDest),
		maxUDPDestBuckets:      maxUDPDestBuckets,
		onUDPDestBucketEvicted: cfg.onUDPDestBucketEvicted,
		destSeen:               make(map[string]struct{}),
		perDest:                make(map[string]*destBucketEntry),
		perDestQ:               list.New(),
	}
	return l
}

func (l *sessionLimiterImpl) AllowUDPSend(destKey string, bytes int) (bool, bool) {
	if l.udpPackets != nil && !l.udpPackets.Allow(1) {
		return false, false
	}
	if l.udpBytes != nil && !l.udpBytes.Allow(int64(bytes)) {
		return false, false
	}

	if !l.trackDestination(destKey) {
		return false, true
	}

	if l.perDestRate <= 0 {
		return true, false
	}

	bucket := l.getOrCreateDestBucket(destKey)
	if !bucket.Allow(1) {
		return false, false
	}

	return true, false
}

func (l *sessionLimiterImpl) AllowDataChannelSend(bytes int) bool {
	if l.dcBytes == nil {
		return true
	}
	if !l.dcBytes.Allow(int64(bytes)) {
		return false
	}
	return true
}

func (l *sessionLimiterImpl) trackDestination(destKey string) bool {
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

func (l *sessionLimiterImpl) getOrCreateDestBucket(destKey string) *tokenBucket {
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

	bucket = newTokenBucket(l.clock, l.perDestRate, l.perDestRate)
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
