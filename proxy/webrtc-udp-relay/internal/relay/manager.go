package relay

import (
	"errors"
	"sync"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
)

type SessionManager struct {
	cfg     config.Config
	metrics *metrics.Metrics
	clock   clock

	mu       sync.Mutex
	sessions map[string]*Session
}

const (
	sessionMapKeyRandomPrefix = "id:"
	sessionMapKeySIDPrefix    = "sid:"
)

func randomSessionMapKey(id string) string { return sessionMapKeyRandomPrefix + id }
func sidSessionMapKey(sid string) string   { return sessionMapKeySIDPrefix + sid }

func (sm *SessionManager) sessionIDInUseLocked(id string) bool {
	for _, sess := range sm.sessions {
		if sess.ID() == id {
			return true
		}
	}
	return false
}

func NewSessionManager(cfg config.Config, m *metrics.Metrics, clock clock) *SessionManager {
	if m == nil {
		m = &metrics.Metrics{}
	}
	return &SessionManager{
		cfg:      cfg,
		metrics:  m,
		clock:    clock,
		sessions: make(map[string]*Session),
	}
}

func (sm *SessionManager) Metrics() *metrics.Metrics { return sm.metrics }

func (sm *SessionManager) createSession() (*Session, error) {
	for attempt := 0; attempt < 3; attempt++ {
		id, err := newSessionID()
		if err != nil {
			return nil, err
		}
		mapKey := randomSessionMapKey(id)

		sm.mu.Lock()
		if sm.cfg.MaxSessions > 0 && len(sm.sessions) >= sm.cfg.MaxSessions {
			sm.metrics.Inc(metrics.DropReasonTooManySessions)
			sm.mu.Unlock()
			return nil, ErrTooManySessions
		}
		if sm.sessionIDInUseLocked(id) {
			// Extremely unlikely (16 bytes of crypto-random entropy). Try again.
			sm.mu.Unlock()
			continue
		}

		session := newSession(id, sm.cfg, sm.metrics, sm.clock, func() {
			sm.deleteSession(mapKey)
		})
		sm.sessions[mapKey] = session
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

// CreateSessionWithKey creates a new session using key as a stable quota key.
//
// When key is non-empty, only one active session may exist for that key at a
// time. This is used to prevent clients from bypassing per-session rate limits
// by creating many parallel sessions using distinct credentials that map to the
// same stable identity (e.g. AUTH_MODE=jwt with stable `sid` claim).
//
// The session's public ID (Session.ID) remains a random value; key is used only
// for quota bookkeeping and uniqueness enforcement.
func (sm *SessionManager) CreateSessionWithKey(key string) (*Session, error) {
	if key == "" {
		return sm.createSession()
	}
	mapKey := sidSessionMapKey(key)

	for attempt := 0; attempt < 3; attempt++ {
		// Allocate a public session identifier for observability/debugging.
		id, err := newSessionID()
		if err != nil {
			return nil, err
		}

		sm.mu.Lock()
		if _, ok := sm.sessions[mapKey]; ok {
			sm.mu.Unlock()
			return nil, ErrSessionAlreadyActive
		}
		if sm.cfg.MaxSessions > 0 && len(sm.sessions) >= sm.cfg.MaxSessions {
			sm.metrics.Inc(metrics.DropReasonTooManySessions)
			sm.mu.Unlock()
			return nil, ErrTooManySessions
		}
		if sm.sessionIDInUseLocked(id) {
			// Extremely unlikely (16 bytes of crypto-random entropy). Try again.
			sm.mu.Unlock()
			continue
		}

		session := newSession(id, sm.cfg, sm.metrics, sm.clock, func() {
			sm.deleteSession(mapKey)
		})
		sm.sessions[mapKey] = session
		sm.mu.Unlock()
		return session, nil
	}

	return nil, errors.New("failed to allocate unique session id")
}
