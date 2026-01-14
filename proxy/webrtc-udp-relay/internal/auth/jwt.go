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

type jwtClaims struct {
	SID    string
	Exp    int64
	Iat    int64
	Origin *string
	Aud    *string
	Iss    *string
}

// VerifyAndExtractSID verifies token and returns its stable session ID claim
// (sid). This is used as a quota key to prevent clients from bypassing per-session
// limits by opening many parallel connections with different JWT strings that
// share the same sid.
func (v jwtVerifier) VerifyAndExtractSID(token string) (string, error) {
	claims, err := v.verifyAndExtractClaims(token)
	if err != nil {
		return "", err
	}
	return claims.SID, nil
}

func (v jwtVerifier) verifyAndExtractClaims(token string) (jwtClaims, error) {
	parts := strings.Split(token, ".")
	if len(parts) != 3 {
		return jwtClaims{}, ErrInvalidCredentials
	}

	headerJSON, err := base64.RawURLEncoding.DecodeString(parts[0])
	if err != nil {
		return jwtClaims{}, ErrInvalidCredentials
	}

	var header map[string]any
	if err := json.Unmarshal(headerJSON, &header); err != nil {
		return jwtClaims{}, ErrInvalidCredentials
	}
	algRaw, ok := header["alg"]
	if !ok {
		return jwtClaims{}, ErrInvalidCredentials
	}
	alg, ok := algRaw.(string)
	if !ok {
		return jwtClaims{}, ErrInvalidCredentials
	}
	if alg != "HS256" {
		return jwtClaims{}, ErrUnsupportedJWT
	}
	if typRaw, ok := header["typ"]; ok {
		if typRaw == nil {
			return jwtClaims{}, ErrInvalidCredentials
		}
		if _, ok := typRaw.(string); !ok {
			return jwtClaims{}, ErrInvalidCredentials
		}
	}

	payloadJSON, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return jwtClaims{}, ErrInvalidCredentials
	}

	mac := hmac.New(sha256.New, v.secret)
	_, _ = mac.Write([]byte(parts[0]))
	_, _ = mac.Write([]byte{'.'})
	_, _ = mac.Write([]byte(parts[1]))
	expectedSig := mac.Sum(nil)

	gotSig, err := base64.RawURLEncoding.DecodeString(parts[2])
	if err != nil {
		return jwtClaims{}, ErrInvalidCredentials
	}
	if !hmac.Equal(gotSig, expectedSig) {
		return jwtClaims{}, ErrInvalidCredentials
	}

	dec := json.NewDecoder(bytes.NewReader(payloadJSON))
	dec.UseNumber()
	var claims map[string]any
	if err := dec.Decode(&claims); err != nil {
		return jwtClaims{}, ErrInvalidCredentials
	}
	// json.Decoder allows trailing bytes after the first top-level value. Ensure
	// the payload is exactly one JSON object to match the strictness of other
	// implementations (Rust/JS).
	if err := dec.Decode(&struct{}{}); err != io.EOF {
		return jwtClaims{}, ErrInvalidCredentials
	}

	now := v.now().Unix()

	exp, ok := claims["exp"]
	if !ok {
		return jwtClaims{}, ErrInvalidCredentials
	}
	expUnix, err := parseUnixTimestamp(exp)
	if err != nil {
		return jwtClaims{}, ErrInvalidCredentials
	}
	if now >= expUnix {
		return jwtClaims{}, ErrInvalidCredentials
	}

	iat, ok := claims["iat"]
	if !ok {
		return jwtClaims{}, ErrInvalidCredentials
	}
	iatUnix, err := parseUnixTimestamp(iat)
	if err != nil {
		return jwtClaims{}, ErrInvalidCredentials
	}

	if nbf, ok := claims["nbf"]; ok {
		nbfUnix, err := parseUnixTimestamp(nbf)
		if err != nil {
			return jwtClaims{}, ErrInvalidCredentials
		}
		if now < nbfUnix {
			return jwtClaims{}, ErrInvalidCredentials
		}
	}

	sidRaw, ok := claims["sid"]
	if !ok {
		return jwtClaims{}, ErrInvalidCredentials
	}
	sid, ok := sidRaw.(string)
	if !ok || sid == "" {
		return jwtClaims{}, ErrInvalidCredentials
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
		return jwtClaims{}, err
	}
	aud, err := getOptString("aud")
	if err != nil {
		return jwtClaims{}, err
	}
	iss, err := getOptString("iss")
	if err != nil {
		return jwtClaims{}, err
	}

	return jwtClaims{SID: sid, Exp: expUnix, Iat: iatUnix, Origin: origin, Aud: aud, Iss: iss}, nil
}

func (v jwtVerifier) Verify(token string) error {
	_, err := v.VerifyAndExtractSID(token)
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
