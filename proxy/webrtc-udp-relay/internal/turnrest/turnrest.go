package turnrest

import (
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha1"
	"encoding/base64"
	"encoding/hex"
	"errors"
	"fmt"
	"time"
)

// This package implements coturn-compatible TURN REST credentials.
//
// See:
// - https://github.com/coturn/coturn/wiki/turnserver
// - https://datatracker.ietf.org/doc/html/draft-uberti-behave-turn-rest
//
// Algorithm (coturn-compatible):
//
//	username   = <unix_expiry_timestamp>:<username_prefix>:<session_id_or_random>
//	credential = base64(hmac_sha1(shared_secret, username))
//
// Expiry is computed using the server clock in UTC:
//
//	unix_expiry_timestamp = now_utc_unix + ttl_seconds
type Generator struct {
	sharedSecret   []byte
	ttlSeconds     int64
	usernamePrefix string
	now            func() time.Time

	sessionIDSource func() (string, error)
}

type GeneratorConfig struct {
	SharedSecret    string
	TTLSeconds      int64
	UsernamePrefix  string
	Now             func() time.Time
	SessionIDSource func() (string, error)
}

func NewGenerator(cfg GeneratorConfig) (*Generator, error) {
	if cfg.SharedSecret == "" {
		return nil, errors.New("shared secret is required")
	}
	if cfg.TTLSeconds <= 0 {
		return nil, errors.New("TTLSeconds must be > 0")
	}
	if cfg.UsernamePrefix == "" {
		return nil, errors.New("UsernamePrefix is required")
	}
	if containsColon(cfg.UsernamePrefix) {
		return nil, errors.New("UsernamePrefix must not contain ':'")
	}
	if cfg.Now == nil {
		cfg.Now = time.Now
	}
	if cfg.SessionIDSource == nil {
		cfg.SessionIDSource = CryptoRandomSessionID
	}
	return &Generator{
		sharedSecret:    []byte(cfg.SharedSecret),
		ttlSeconds:      cfg.TTLSeconds,
		usernamePrefix:  cfg.UsernamePrefix,
		now:             cfg.Now,
		sessionIDSource: cfg.SessionIDSource,
	}, nil
}

type Credentials struct {
	Username   string
	Credential string
	ExpiryUnix int64
}

func (g *Generator) Generate(sessionID string) (Credentials, error) {
	if sessionID == "" {
		return Credentials{}, errors.New("sessionID is required")
	}
	if containsColon(sessionID) {
		return Credentials{}, errors.New("sessionID must not contain ':'")
	}
	expiryUnix := g.now().UTC().Unix() + g.ttlSeconds
	username := fmt.Sprintf("%d:%s:%s", expiryUnix, g.usernamePrefix, sessionID)
	cred := signUsername(g.sharedSecret, username)
	return Credentials{
		Username:   username,
		Credential: cred,
		ExpiryUnix: expiryUnix,
	}, nil
}

func (g *Generator) GenerateRandom() (Credentials, error) {
	sessionID, err := g.sessionIDSource()
	if err != nil {
		return Credentials{}, err
	}
	return g.Generate(sessionID)
}

func CryptoRandomSessionID() (string, error) {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "", err
	}
	return hex.EncodeToString(b[:]), nil
}

func signUsername(sharedSecret []byte, username string) string {
	mac := hmac.New(sha1.New, sharedSecret)
	_, _ = mac.Write([]byte(username))
	sum := mac.Sum(nil)
	return base64.StdEncoding.EncodeToString(sum)
}

func containsColon(s string) bool {
	for i := 0; i < len(s); i++ {
		if s[i] == ':' {
			return true
		}
	}
	return false
}
