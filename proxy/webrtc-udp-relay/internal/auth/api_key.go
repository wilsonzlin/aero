package auth

import (
	"crypto/subtle"
	"errors"
)

var ErrInvalidCredentials = errors.New("invalid credentials")

type APIKeyVerifier struct {
	Expected string
}

func (v APIKeyVerifier) Verify(apiKey string) error {
	if apiKey == "" || v.Expected == "" {
		return ErrInvalidCredentials
	}
	if subtle.ConstantTimeCompare([]byte(apiKey), []byte(v.Expected)) != 1 {
		return ErrInvalidCredentials
	}
	return nil
}
