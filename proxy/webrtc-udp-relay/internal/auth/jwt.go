package auth

import (
	"bytes"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"
)

var ErrUnsupportedJWT = errors.New("unsupported jwt")

type JWTVerifier struct {
	secret []byte
	now    func() time.Time
}

func NewJWTVerifier(secret string) JWTVerifier {
	return JWTVerifier{
		secret: []byte(secret),
		now:    time.Now,
	}
}

func (v JWTVerifier) Verify(token string) error {
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return ErrInvalidCredentials
	}

	headerJSON, err := base64.RawURLEncoding.DecodeString(parts[0])
	if err != nil {
		return ErrInvalidCredentials
	}

	var header struct {
		Alg string `json:"alg"`
		Typ string `json:"typ,omitempty"`
	}
	if err := json.Unmarshal(headerJSON, &header); err != nil {
		return ErrInvalidCredentials
	}
	if header.Alg != "HS256" {
		return ErrUnsupportedJWT
	}

	payloadJSON, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return ErrInvalidCredentials
	}

	mac := hmac.New(sha256.New, v.secret)
	_, _ = mac.Write([]byte(parts[0]))
	_, _ = mac.Write([]byte{'.'})
	_, _ = mac.Write([]byte(parts[1]))
	expectedSig := mac.Sum(nil)

	gotSig, err := base64.RawURLEncoding.DecodeString(parts[2])
	if err != nil {
		return ErrInvalidCredentials
	}
	if !hmac.Equal(gotSig, expectedSig) {
		return ErrInvalidCredentials
	}

	dec := json.NewDecoder(bytes.NewReader(payloadJSON))
	dec.UseNumber()
	var claims map[string]any
	if err := dec.Decode(&claims); err != nil {
		return ErrInvalidCredentials
	}

	now := v.now().Unix()
	if exp, ok := claims["exp"]; ok {
		expUnix, err := parseUnixTimestamp(exp)
		if err != nil {
			return ErrInvalidCredentials
		}
		if now >= expUnix {
			return ErrInvalidCredentials
		}
	}
	if nbf, ok := claims["nbf"]; ok {
		nbfUnix, err := parseUnixTimestamp(nbf)
		if err != nil {
			return ErrInvalidCredentials
		}
		if now < nbfUnix {
			return ErrInvalidCredentials
		}
	}

	return nil
}

func parseUnixTimestamp(v any) (int64, error) {
	switch x := v.(type) {
	case json.Number:
		return x.Int64()
	case float64:
		return int64(x), nil
	default:
		return 0, fmt.Errorf("invalid timestamp %T", v)
	}
}
