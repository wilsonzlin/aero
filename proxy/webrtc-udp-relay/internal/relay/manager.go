package relay

import (
	"strconv"
	"sync"
	"sync/atomic"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/ratelimit"
)

type SessionManager struct {
	cfg     config.Config
	metrics *metrics.Metrics
	clock   ratelimit.Clock

	mu       sync.Mutex
	sessions map[string]*Session

	nextID atomic.Uint64
}

func NewSessionManager(cfg config.Config, m *metrics.Metrics, clock ratelimit.Clock) *SessionManager {
	if m == nil {
		m = metrics.New()
	}
	if clock == nil {
		clock = ratelimit.RealClock{}
	}
	return &SessionManager{
		cfg:      cfg,
		metrics:  m,
		clock:    clock,
		sessions: make(map[string]*Session),
	}
}

func (sm *SessionManager) Metrics() *metrics.Metrics { return sm.metrics }

func (sm *SessionManager) ActiveSessions() int {
	sm.mu.Lock()
	defer sm.mu.Unlock()
	return len(sm.sessions)
}

func (sm *SessionManager) CreateSession() (*Session, error) {
	sm.mu.Lock()
	defer sm.mu.Unlock()

	if sm.cfg.MaxSessions > 0 && len(sm.sessions) >= sm.cfg.MaxSessions {
		sm.metrics.Inc(metrics.DropReasonTooManySessions)
		return nil, ErrTooManySessions
	}

	id := strconv.FormatUint(sm.nextID.Add(1), 10)
	session := newSession(id, sm.cfg, sm.metrics, sm.clock, func() {
		sm.deleteSession(id)
	})
	sm.sessions[id] = session
	return session, nil
}

func (sm *SessionManager) deleteSession(id string) {
	sm.mu.Lock()
	delete(sm.sessions, id)
	sm.mu.Unlock()
}
