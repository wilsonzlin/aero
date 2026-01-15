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

const (
	// HMAC-SHA256 output size in bytes.
	hmacSHA256SigLen = 32
	// base64url-no-pad encoding length for a 32-byte HMAC:
	// - 32 bytes => 44 chars with one '=' padding
	// - without padding => 43 chars
	hmacSHA256SigB64Len = 43
	maxJWTHeaderB64Len  = 4 * 1024
	maxJWTPayloadB64Len = 16 * 1024
	maxJWTLen           = maxJWTHeaderB64Len + 1 + maxJWTPayloadB64Len + 1 + hmacSHA256SigB64Len
)

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
	headerB64, payloadB64, sigB64, ok := splitJWTParts(token)
	if !ok {
		return jwtClaims{}, ErrInvalidCredentials
	}

	headerJSON, err := base64.RawURLEncoding.DecodeString(headerB64)
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

	gotSig, err := base64.RawURLEncoding.DecodeString(sigB64)
	if err != nil {
		return jwtClaims{}, ErrInvalidCredentials
	}
	if len(gotSig) != hmacSHA256SigLen {
		return jwtClaims{}, ErrInvalidCredentials
	}

	mac := hmac.New(sha256.New, v.secret)
	_, _ = mac.Write([]byte(headerB64))
	_, _ = mac.Write([]byte{'.'})
	_, _ = mac.Write([]byte(payloadB64))
	expectedSig := mac.Sum(nil)
	if !hmac.Equal(gotSig, expectedSig) {
		return jwtClaims{}, ErrInvalidCredentials
	}

	payloadJSON, err := base64.RawURLEncoding.DecodeString(payloadB64)
	if err != nil {
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

func splitJWTParts(token string) (headerB64, payloadB64, sigB64 string, ok bool) {
	if token == "" || len(token) > maxJWTLen {
		return "", "", "", false
	}
	headerB64, rest, found := strings.Cut(token, ".")
	if !found {
		return "", "", "", false
	}
	payloadB64, sigB64, found = strings.Cut(rest, ".")
	if !found {
		return "", "", "", false
	}
	if strings.Contains(sigB64, ".") {
		return "", "", "", false
	}
	if headerB64 == "" || payloadB64 == "" || sigB64 == "" {
		return "", "", "", false
	}
	if len(headerB64) > maxJWTHeaderB64Len || len(payloadB64) > maxJWTPayloadB64Len {
		return "", "", "", false
	}
	if len(sigB64) != hmacSHA256SigB64Len {
		return "", "", "", false
	}
	if !isBase64urlNoPad(headerB64, maxJWTHeaderB64Len) ||
		!isBase64urlNoPad(payloadB64, maxJWTPayloadB64Len) ||
		!isBase64urlNoPad(sigB64, hmacSHA256SigB64Len) {
		return "", "", "", false
	}
	return headerB64, payloadB64, sigB64, true
}

func isBase64urlNoPad(raw string, maxLen int) bool {
	if raw == "" || len(raw) > maxLen {
		return false
	}
	// Base64url without padding cannot have length mod 4 == 1.
	if len(raw)%4 == 1 {
		return false
	}
	for i := 0; i < len(raw); i++ {
		if _, ok := b64urlValue(raw[i]); !ok {
			return false
		}
	}
	// Tighten validation to canonical base64url-no-pad. Even when the length is syntactically
	// valid (mod 4 != 1), the unused bits in the final base64 quantum must be zero.
	//
	// - len % 4 == 2 => 4 unused bits (must be zero)
	// - len % 4 == 3 => 2 unused bits (must be zero)
	switch len(raw) % 4 {
	case 0:
		return true
	case 2:
		last, _ := b64urlValue(raw[len(raw)-1])
		return (last & 0x0f) == 0
	case 3:
		last, _ := b64urlValue(raw[len(raw)-1])
		return (last & 0x03) == 0
	default:
		// len%4==1 is rejected above.
		return false
	}
}

func b64urlValue(b byte) (byte, bool) {
	switch {
	case b >= 'A' && b <= 'Z':
		return b - 'A', true
	case b >= 'a' && b <= 'z':
		return b - 'a' + 26, true
	case b >= '0' && b <= '9':
		return b - '0' + 52, true
	case b == '-':
		return 62, true
	case b == '_':
		return 63, true
	default:
		return 0, false
	}
}

func parseUnixTimestamp(v any) (int64, error) {
	switch x := v.(type) {
	case json.Number:
		return x.Int64()
	default:
		return 0, fmt.Errorf("invalid timestamp %T", v)
	}
}
