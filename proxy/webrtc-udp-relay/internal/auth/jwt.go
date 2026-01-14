package auth

import (
	"bytes"
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strings"
	"time"
)

var ErrUnsupportedJWT = errors.New("unsupported jwt")

type jwtVerifier struct {
	secret []byte
	now    func() time.Time
}

func newJWTVerifier(secret string) jwtVerifier {
	return jwtVerifier{
		secret: []byte(secret),
		now:    time.Now,
	}
}

type JWTClaims struct {
	SID    string
	Exp    int64
	Iat    int64
	Origin *string
	Aud    *string
	Iss    *string
}

func (v jwtVerifier) VerifyAndExtractClaims(token string) (JWTClaims, error) {
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return JWTClaims{}, ErrInvalidCredentials
	}

	headerJSON, err := base64.RawURLEncoding.DecodeString(parts[0])
	if err != nil {
		return JWTClaims{}, ErrInvalidCredentials
	}

	var header map[string]any
	if err := json.Unmarshal(headerJSON, &header); err != nil {
		return JWTClaims{}, ErrInvalidCredentials
	}
	algRaw, ok := header["alg"]
	if !ok {
		return JWTClaims{}, ErrInvalidCredentials
	}
	alg, ok := algRaw.(string)
	if !ok {
		return JWTClaims{}, ErrInvalidCredentials
	}
	if alg != "HS256" {
		return JWTClaims{}, ErrUnsupportedJWT
	}
	if typRaw, ok := header["typ"]; ok {
		if typRaw == nil {
			return JWTClaims{}, ErrInvalidCredentials
		}
		if _, ok := typRaw.(string); !ok {
			return JWTClaims{}, ErrInvalidCredentials
		}
	}

	payloadJSON, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return JWTClaims{}, ErrInvalidCredentials
	}

	mac := hmac.New(sha256.New, v.secret)
	_, _ = mac.Write([]byte(parts[0]))
	_, _ = mac.Write([]byte{'.'})
	_, _ = mac.Write([]byte(parts[1]))
	expectedSig := mac.Sum(nil)

	gotSig, err := base64.RawURLEncoding.DecodeString(parts[2])
	if err != nil {
		return JWTClaims{}, ErrInvalidCredentials
	}
	if !hmac.Equal(gotSig, expectedSig) {
		return JWTClaims{}, ErrInvalidCredentials
	}

	dec := json.NewDecoder(bytes.NewReader(payloadJSON))
	dec.UseNumber()
	var claims map[string]any
	if err := dec.Decode(&claims); err != nil {
		return JWTClaims{}, ErrInvalidCredentials
	}
	// json.Decoder allows trailing bytes after the first top-level value. Ensure
	// the payload is exactly one JSON object to match the strictness of other
	// implementations (Rust/JS).
	if err := dec.Decode(&struct{}{}); err != io.EOF {
		return JWTClaims{}, ErrInvalidCredentials
	}

	now := v.now().Unix()

	exp, ok := claims["exp"]
	if !ok {
		return JWTClaims{}, ErrInvalidCredentials
	}
	expUnix, err := parseUnixTimestamp(exp)
	if err != nil {
		return JWTClaims{}, ErrInvalidCredentials
	}
	if now >= expUnix {
		return JWTClaims{}, ErrInvalidCredentials
	}

	iat, ok := claims["iat"]
	if !ok {
		return JWTClaims{}, ErrInvalidCredentials
	}
	iatUnix, err := parseUnixTimestamp(iat)
	if err != nil {
		return JWTClaims{}, ErrInvalidCredentials
	}

	if nbf, ok := claims["nbf"]; ok {
		nbfUnix, err := parseUnixTimestamp(nbf)
		if err != nil {
			return JWTClaims{}, ErrInvalidCredentials
		}
		if now < nbfUnix {
			return JWTClaims{}, ErrInvalidCredentials
		}
	}

	sidRaw, ok := claims["sid"]
	if !ok {
		return JWTClaims{}, ErrInvalidCredentials
	}
	sid, ok := sidRaw.(string)
	if !ok || sid == "" {
		return JWTClaims{}, ErrInvalidCredentials
	}

	getOptString := func(key string) (*string, error) {
		raw, ok := claims[key]
		if !ok {
			return nil, nil
		}
		s, ok := raw.(string)
		if !ok {
			return nil, ErrInvalidCredentials
		}
		return &s, nil
	}

	origin, err := getOptString("origin")
	if err != nil {
		return JWTClaims{}, err
	}
	aud, err := getOptString("aud")
	if err != nil {
		return JWTClaims{}, err
	}
	iss, err := getOptString("iss")
	if err != nil {
		return JWTClaims{}, err
	}

	return JWTClaims{SID: sid, Exp: expUnix, Iat: iatUnix, Origin: origin, Aud: aud, Iss: iss}, nil
}

func (v jwtVerifier) Verify(token string) error {
	_, err := v.VerifyAndExtractClaims(token)
	return err
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
