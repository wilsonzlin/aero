package auth

import (
	"encoding/json"
	"os"
	"path/filepath"
	"runtime"
	"testing"
	"time"
)

type jwtVectorsFile struct {
	Version  int `json:"version"`
	RelayJWT struct {
		Secret  string `json:"secret"`
		NowUnix int64  `json:"nowUnix"`
		Tokens  struct {
			Valid        jwtTokenVector `json:"valid"`
			Expired      jwtTokenVector `json:"expired"`
			BadSignature jwtTokenVector `json:"badSignature"`
		} `json:"tokens"`
	} `json:"aero-udp-relay-jwt-hs256"`
}

type jwtTokenVector struct {
	Token  string         `json:"token"`
	Claims jwtTokenClaims `json:"claims"`
}

type jwtTokenClaims struct {
	Iat    int64   `json:"iat"`
	Exp    int64   `json:"exp"`
	SID    string  `json:"sid"`
	Origin *string `json:"origin,omitempty"`
	Aud    *string `json:"aud,omitempty"`
	Iss    *string `json:"iss,omitempty"`
}

type authTokenVectorsFile struct {
	Schema    int `json:"schema"`
	JWTTokens struct {
		TestSecret string      `json:"testSecret"`
		Vectors    []jwtVector `json:"vectors"`
	} `json:"jwtTokens"`
}

type jwtVector struct {
	Name   string `json:"name"`
	Secret string `json:"secret"`
	Token  string `json:"token"`
	NowSec int64  `json:"nowSec"`

	SID *string `json:"sid,omitempty"`
	Exp *int64  `json:"exp,omitempty"`
	Iat *int64  `json:"iat,omitempty"`

	Origin *string `json:"origin,omitempty"`
	Aud    *string `json:"aud,omitempty"`
	Iss    *string `json:"iss,omitempty"`

	ExpectError bool `json:"expectError"`
}

func loadJWTVectors(t *testing.T) jwtVectorsFile {
	t.Helper()

	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatalf("runtime.Caller failed")
	}
	path := filepath.Join(filepath.Dir(thisFile), "../../../..", "crates/conformance/test-vectors/aero-vectors-v1.json")
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read vectors file: %v", err)
	}
	var vectors jwtVectorsFile
	if err := json.Unmarshal(raw, &vectors); err != nil {
		t.Fatalf("parse vectors json: %v", err)
	}
	return vectors
}

func loadAuthTokenVectors(t *testing.T) authTokenVectorsFile {
	t.Helper()

	_, thisFile, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatalf("runtime.Caller failed")
	}
	path := filepath.Join(filepath.Dir(thisFile), "../../../..", "protocol-vectors/auth-tokens.json")
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read vectors file: %v", err)
	}
	var vectors authTokenVectorsFile
	if err := json.Unmarshal(raw, &vectors); err != nil {
		t.Fatalf("parse vectors json: %v", err)
	}
	return vectors
}

func TestJWTVerifierVectors(t *testing.T) {
	vectors := loadJWTVectors(t)
	if vectors.Version != 1 {
		t.Fatalf("unexpected vectors version: %d", vectors.Version)
	}

	verifier := newJWTVerifier(vectors.RelayJWT.Secret)
	verifier.now = func() time.Time {
		return time.Unix(vectors.RelayJWT.NowUnix, 0)
	}

	claims, err := verifier.VerifyAndExtractClaims(vectors.RelayJWT.Tokens.Valid.Token)
	if err != nil {
		t.Fatalf("valid token rejected: %v", err)
	}
	if claims.SID != vectors.RelayJWT.Tokens.Valid.Claims.SID {
		t.Fatalf("sid: got %q want %q", claims.SID, vectors.RelayJWT.Tokens.Valid.Claims.SID)
	}
	if claims.Exp != vectors.RelayJWT.Tokens.Valid.Claims.Exp {
		t.Fatalf("exp: got %d want %d", claims.Exp, vectors.RelayJWT.Tokens.Valid.Claims.Exp)
	}
	if claims.Iat != vectors.RelayJWT.Tokens.Valid.Claims.Iat {
		t.Fatalf("iat: got %d want %d", claims.Iat, vectors.RelayJWT.Tokens.Valid.Claims.Iat)
	}
	assertOptStringEqual(t, "origin", claims.Origin, vectors.RelayJWT.Tokens.Valid.Claims.Origin)
	assertOptStringEqual(t, "aud", claims.Aud, vectors.RelayJWT.Tokens.Valid.Claims.Aud)
	assertOptStringEqual(t, "iss", claims.Iss, vectors.RelayJWT.Tokens.Valid.Claims.Iss)

	if err := verifier.Verify(vectors.RelayJWT.Tokens.Expired.Token); err == nil {
		t.Fatalf("expected expired token to be rejected")
	}
	if err := verifier.Verify(vectors.RelayJWT.Tokens.BadSignature.Token); err == nil {
		t.Fatalf("expected bad-signature token to be rejected")
	}
}

func TestJWTVerifierProtocolVectors(t *testing.T) {
	vectors := loadAuthTokenVectors(t)
	if vectors.Schema != 1 {
		t.Fatalf("unexpected vectors schema: %d", vectors.Schema)
	}

	for _, v := range vectors.JWTTokens.Vectors {
		t.Run(v.Name, func(t *testing.T) {
			verifier := newJWTVerifier(v.Secret)
			verifier.now = func() time.Time {
				return time.Unix(v.NowSec, 0)
			}

			claims, err := verifier.VerifyAndExtractClaims(v.Token)
			if v.ExpectError {
				if err == nil {
					t.Fatalf("expected error, got nil")
				}
				return
			}
			if err != nil {
				t.Fatalf("verify: %v", err)
			}

			if v.SID == nil || v.Exp == nil || v.Iat == nil {
				t.Fatalf("vector missing required claims fields (sid/exp/iat)")
			}

			if claims.SID != *v.SID {
				t.Fatalf("sid: got %q want %q", claims.SID, *v.SID)
			}
			if claims.Exp != *v.Exp {
				t.Fatalf("exp: got %d want %d", claims.Exp, *v.Exp)
			}
			if claims.Iat != *v.Iat {
				t.Fatalf("iat: got %d want %d", claims.Iat, *v.Iat)
			}
			assertOptStringEqual(t, "origin", claims.Origin, v.Origin)
			assertOptStringEqual(t, "aud", claims.Aud, v.Aud)
			assertOptStringEqual(t, "iss", claims.Iss, v.Iss)
		})
	}
}

func TestJWTProtocolVectorsMatchConformanceVectors(t *testing.T) {
	conformance := loadJWTVectors(t)
	if conformance.Version != 1 {
		t.Fatalf("unexpected conformance vectors version: %d", conformance.Version)
	}

	protocol := loadAuthTokenVectors(t)
	if protocol.Schema != 1 {
		t.Fatalf("unexpected protocol vectors schema: %d", protocol.Schema)
	}

	if protocol.JWTTokens.TestSecret != conformance.RelayJWT.Secret {
		t.Fatalf(
			"secret mismatch: protocol=%q conformance=%q",
			protocol.JWTTokens.TestSecret,
			conformance.RelayJWT.Secret,
		)
	}

	find := func(name string) jwtVector {
		for _, v := range protocol.JWTTokens.Vectors {
			if v.Name == name {
				return v
			}
		}
		t.Fatalf("missing protocol vector %q", name)
		return jwtVector{}
	}

	if got := find("valid").Token; got != conformance.RelayJWT.Tokens.Valid.Token {
		t.Fatalf("valid token mismatch")
	}
	if got := find("expired").Token; got != conformance.RelayJWT.Tokens.Expired.Token {
		t.Fatalf("expired token mismatch")
	}
	if got := find("badSignature").Token; got != conformance.RelayJWT.Tokens.BadSignature.Token {
		t.Fatalf("badSignature token mismatch")
	}
}

func assertOptStringEqual(t *testing.T, name string, got, want *string) {
	t.Helper()
	if got == nil && want == nil {
		return
	}
	if got == nil && want != nil {
		t.Fatalf("%s: got nil want %q", name, *want)
	}
	if got != nil && want == nil {
		t.Fatalf("%s: got %q want nil", name, *got)
	}
	if *got != *want {
		t.Fatalf("%s: got %q want %q", name, *got, *want)
	}
}
