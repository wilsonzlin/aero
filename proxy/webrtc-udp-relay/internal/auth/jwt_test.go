package auth

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"testing"
	"time"
)

func mustJWT(t *testing.T, secret string, header, claims map[string]any) string {
	t.Helper()

	headerJSON, err := json.Marshal(header)
	if err != nil {
		t.Fatalf("marshal header: %v", err)
	}
	payloadJSON, err := json.Marshal(claims)
	if err != nil {
		t.Fatalf("marshal claims: %v", err)
	}

	enc := base64.RawURLEncoding
	headerPart := enc.EncodeToString(headerJSON)
	payloadPart := enc.EncodeToString(payloadJSON)
	signingInput := headerPart + "." + payloadPart

	mac := hmac.New(sha256.New, []byte(secret))
	_, _ = mac.Write([]byte(signingInput))
	signaturePart := enc.EncodeToString(mac.Sum(nil))

	return signingInput + "." + signaturePart
}

func TestJWTVerifier_Verify_AcceptsValidHS256(t *testing.T) {
	now := time.Unix(1_000_000, 0)
	v := jwtVerifier{
		secret: []byte("secret"),
		now:    func() time.Time { return now },
	}

	token := mustJWT(t, "secret", map[string]any{"alg": "HS256", "typ": "JWT"}, map[string]any{
		"iat": now.Unix(),
		"exp": now.Add(5 * time.Minute).Unix(),
		"sid": "sess_test",
	})

	if err := v.Verify(token); err != nil {
		t.Fatalf("Verify: %v", err)
	}
}

func TestJWTVerifier_Verify_RejectsExpiredToken(t *testing.T) {
	now := time.Unix(1_000_000, 0)
	v := jwtVerifier{
		secret: []byte("secret"),
		now:    func() time.Time { return now },
	}

	token := mustJWT(t, "secret", map[string]any{"alg": "HS256"}, map[string]any{
		"iat": now.Unix(),
		"exp": now.Add(-1 * time.Second).Unix(),
		"sid": "sess_test",
	})

	err := v.Verify(token)
	if !errors.Is(err, ErrInvalidCredentials) {
		t.Fatalf("err=%v, want ErrInvalidCredentials", err)
	}
}

func TestJWTVerifier_Verify_RejectsTokenNotYetValid(t *testing.T) {
	now := time.Unix(1_000_000, 0)
	v := jwtVerifier{
		secret: []byte("secret"),
		now:    func() time.Time { return now },
	}

	token := mustJWT(t, "secret", map[string]any{"alg": "HS256"}, map[string]any{
		"nbf": now.Add(10 * time.Second).Unix(),
		"iat": now.Unix(),
		"exp": now.Add(5 * time.Minute).Unix(),
		"sid": "sess_test",
	})

	err := v.Verify(token)
	if !errors.Is(err, ErrInvalidCredentials) {
		t.Fatalf("err=%v, want ErrInvalidCredentials", err)
	}
}

func TestJWTVerifier_Verify_RejectsUnsupportedAlg(t *testing.T) {
	now := time.Unix(1_000_000, 0)
	v := jwtVerifier{
		secret: []byte("secret"),
		now:    func() time.Time { return now },
	}

	token := mustJWT(t, "secret", map[string]any{"alg": "none"}, map[string]any{})

	err := v.Verify(token)
	if !errors.Is(err, ErrUnsupportedJWT) {
		t.Fatalf("err=%v, want ErrUnsupportedJWT", err)
	}
}

func TestJWTVerifier_Verify_RejectsBadSignature(t *testing.T) {
	now := time.Unix(1_000_000, 0)
	v := jwtVerifier{
		secret: []byte("secret"),
		now:    func() time.Time { return now },
	}

	// Sign the token with a different secret.
	token := mustJWT(t, "wrong", map[string]any{"alg": "HS256"}, map[string]any{
		"iat": now.Unix(),
		"exp": now.Add(5 * time.Minute).Unix(),
		"sid": "sess_test",
	})

	err := v.Verify(token)
	if !errors.Is(err, ErrInvalidCredentials) {
		t.Fatalf("err=%v, want ErrInvalidCredentials", err)
	}
}

func TestJWTVerifier_Verify_RejectsMalformedToken(t *testing.T) {
	v := jwtVerifier{
		secret: []byte("secret"),
		now:    time.Now,
	}

	err := v.Verify("not-a-jwt")
	if !errors.Is(err, ErrInvalidCredentials) {
		t.Fatalf("err=%v, want ErrInvalidCredentials", err)
	}
}
