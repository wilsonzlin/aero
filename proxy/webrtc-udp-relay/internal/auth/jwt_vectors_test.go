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
	Token  string        `json:"token"`
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

func TestJWTVerifierVectors(t *testing.T) {
	vectors := loadJWTVectors(t)
	if vectors.Version != 1 {
		t.Fatalf("unexpected vectors version: %d", vectors.Version)
	}

	verifier := NewJWTVerifier(vectors.RelayJWT.Secret)
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
