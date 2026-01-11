package relay

import (
	"errors"
	"sync"

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
	for attempt := 0; attempt < 3; attempt++ {
		id, err := newSessionID()
		if err != nil {
			return nil, err
		}

		sm.mu.Lock()
		if sm.cfg.MaxSessions > 0 && len(sm.sessions) >= sm.cfg.MaxSessions {
			sm.metrics.Inc(metrics.DropReasonTooManySessions)
			sm.mu.Unlock()
			return nil, ErrTooManySessions
		}
		if _, ok := sm.sessions[id]; ok {
			// Extremely unlikely (16 bytes of crypto-random entropy). Try again.
			sm.mu.Unlock()
			continue
		}

		session := newSession(id, sm.cfg, sm.metrics, sm.clock, func() {
			sm.deleteSession(id)
		})
		sm.sessions[id] = session
		sm.mu.Unlock()
		return session, nil
	}

	return nil, errors.New("failed to allocate unique session id")
}

func (sm *SessionManager) deleteSession(id string) {
	sm.mu.Lock()
	delete(sm.sessions, id)
	sm.mu.Unlock()
}
