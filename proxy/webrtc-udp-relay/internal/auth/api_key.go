package auth

import (
	"crypto/subtle"
	"errors"
)

var ErrInvalidCredentials = errors.New("invalid credentials")

type apiKeyVerifier struct {
	Expected string
}

func (v apiKeyVerifier) Verify(apiKey string) error {
	if apiKey == "" || v.Expected == "" {
		return ErrInvalidCredentials
	}
	if subtle.ConstantTimeCompare([]byte(apiKey), []byte(v.Expected)) != 1 {
		return ErrInvalidCredentials
	}
	return nil
}
