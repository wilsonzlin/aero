package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/gorilla/websocket"
)

const (
	subprotocol = "aero-l2-tunnel-v1"
	tokenPrefix = "aero-l2-token."

	// Minimal subset of docs/l2-tunnel-protocol.md used by the E2E harness.
	msgMagic   = 0xA2
	msgVersion = 0x03
	msgPing    = 0x01
	msgPong    = 0x02
)

type lastHandshake struct {
	Origin      string `json:"origin"`
	Token       string `json:"token"`
	TokenSource string `json:"tokenSource"`
}

func main() {
	bindHost := envOrDefault("BIND_HOST", "127.0.0.1")
	port := envIntOrDefault("PORT", 0)

	requiredOrigin := os.Getenv("REQUIRE_ORIGIN")
	requiredToken := os.Getenv("REQUIRE_TOKEN")
	requireCookieName := os.Getenv("REQUIRE_COOKIE_NAME")
	requireCookieValue := os.Getenv("REQUIRE_COOKIE_VALUE")
	if requireCookieValue != "" && requireCookieName == "" {
		fmt.Fprintln(os.Stderr, "REQUIRE_COOKIE_VALUE set but REQUIRE_COOKIE_NAME is empty")
		os.Exit(2)
	}

	listenAddr := net.JoinHostPort(bindHost, strconv.Itoa(port))
	ln, err := net.Listen("tcp", listenAddr)
	if err != nil {
		fmt.Fprintf(os.Stderr, "listen %s: %v\n", listenAddr, err)
		os.Exit(1)
	}

	upgrader := websocket.Upgrader{
		CheckOrigin:  func(r *http.Request) bool { return true },
		Subprotocols: []string{subprotocol},
	}

	var lastMu sync.Mutex
	var last lastHandshake

	mux := http.NewServeMux()
	mux.HandleFunc("GET /debug", func(w http.ResponseWriter, r *http.Request) {
		lastMu.Lock()
		snapshot := last
		lastMu.Unlock()
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(snapshot)
	})
	mux.HandleFunc("GET /l2", func(w http.ResponseWriter, r *http.Request) {
		origin := r.Header.Get("Origin")
		qToken := r.URL.Query().Get("token")
		protocols := splitHeaderTokens(r.Header.Values("Sec-WebSocket-Protocol"))
		pToken := tokenFromSubprotocols(protocols)

		token := pToken
		tokenSource := ""
		if token != "" {
			tokenSource = "subprotocol"
		} else if qToken != "" {
			token = qToken
			tokenSource = "query"
		}

		if requiredOrigin != "" && origin != requiredOrigin {
			http.Error(w, "origin mismatch", http.StatusForbidden)
			return
		}
		if requiredToken != "" && token != requiredToken {
			http.Error(w, "token mismatch", http.StatusForbidden)
			return
		}

		if requireCookieName != "" {
			cookie, err := r.Cookie(requireCookieName)
			if err != nil {
				http.Error(w, "unauthorized", http.StatusUnauthorized)
				return
			}
			if requireCookieValue != "" && cookie.Value != requireCookieValue {
				http.Error(w, "forbidden", http.StatusForbidden)
				return
			}
		}

		lastMu.Lock()
		last = lastHandshake{
			Origin:      origin,
			Token:       token,
			TokenSource: tokenSource,
		}
		lastMu.Unlock()

		conn, err := upgrader.Upgrade(w, r, nil)
		if err != nil {
			return
		}
		defer conn.Close()

		if conn.Subprotocol() != subprotocol {
			_ = conn.WriteControl(websocket.CloseMessage, websocket.FormatCloseMessage(websocket.CloseProtocolError, "missing subprotocol"), time.Now().Add(time.Second))
			return
		}

		for {
			msgType, payload, err := conn.ReadMessage()
			if err != nil {
				return
			}
			if msgType != websocket.BinaryMessage {
				continue
			}
			if len(payload) < 4 || payload[0] != msgMagic || payload[1] != msgVersion || payload[2] != msgPing {
				continue
			}
			// Echo PING payload (including header flags) but swap type.
			out := append([]byte(nil), payload...)
			out[2] = msgPong
			_ = conn.WriteMessage(websocket.BinaryMessage, out)
		}
	})

	srv := &http.Server{
		Handler:           mux,
		ReadHeaderTimeout: 5 * time.Second,
	}

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	errCh := make(chan error, 1)
	go func() {
		errCh <- srv.Serve(ln)
	}()

	actualPort := ln.Addr().(*net.TCPAddr).Port
	fmt.Printf("READY %d\n", actualPort)

	select {
	case <-ctx.Done():
		_ = srv.Shutdown(context.Background())
		<-errCh
	case err := <-errCh:
		if err != nil && err != http.ErrServerClosed {
			fmt.Fprintf(os.Stderr, "http server error: %v\n", err)
			os.Exit(1)
		}
	}
}

func splitHeaderTokens(values []string) []string {
	var out []string
	for _, v := range values {
		for _, part := range strings.Split(v, ",") {
			part = strings.TrimSpace(part)
			if part == "" {
				continue
			}
			out = append(out, part)
		}
	}
	return out
}

func tokenFromSubprotocols(protocols []string) string {
	for _, proto := range protocols {
		if strings.HasPrefix(proto, tokenPrefix) {
			return strings.TrimPrefix(proto, tokenPrefix)
		}
	}
	return ""
}

func envOrDefault(key, fallback string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return fallback
}

func envIntOrDefault(key string, fallback int) int {
	if v := os.Getenv(key); v != "" {
		n, err := strconv.Atoi(v)
		if err == nil {
			return n
		}
	}
	return fallback
}
