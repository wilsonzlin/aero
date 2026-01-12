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

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/l2tunnel"
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
		Subprotocols: []string{l2tunnel.Subprotocol},
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
		qAPIKey := r.URL.Query().Get("apiKey")
		qToken := r.URL.Query().Get("token")
		protocols := splitHeaderTokens(r.Header.Values("Sec-WebSocket-Protocol"))
		pToken := tokenFromSubprotocols(protocols)

		// Match `crates/aero-l2-proxy` precedence: check query-string credentials
		// (token/apiKey) before `aero-l2-token.*` subprotocol tokens.
		//
		// Note: `crates/aero-l2-proxy` treats `apiKey` as a compatibility alias but
		// prefers `token` when both are present.
		token := qToken
		tokenSource := ""
		if token != "" {
			tokenSource = "query"
		} else if qAPIKey != "" {
			token = qAPIKey
			tokenSource = "query"
		} else if pToken != "" {
			token = pToken
			tokenSource = "subprotocol"
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

		if conn.Subprotocol() != l2tunnel.Subprotocol {
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
			if len(payload) < l2tunnel.HeaderLen || payload[0] != l2tunnel.Magic || payload[1] != l2tunnel.Version || payload[2] != l2tunnel.TypePing {
				continue
			}
			// Echo PING payload (including header flags) but swap type.
			out := append([]byte(nil), payload...)
			out[2] = l2tunnel.TypePong
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
		if strings.HasPrefix(proto, l2tunnel.TokenSubprotocolPrefix) {
			return strings.TrimPrefix(proto, l2tunnel.TokenSubprotocolPrefix)
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
