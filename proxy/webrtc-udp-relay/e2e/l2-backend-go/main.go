package main

import (
	"context"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/signal"
	"strconv"
	"syscall"
	"time"

	"github.com/gorilla/websocket"
)

const (
	subprotocol = "aero-l2-tunnel-v1"

	// Minimal subset of docs/l2-tunnel-protocol.md used by the E2E harness.
	msgMagic   = 0xA2
	msgVersion = 0x03
	msgPing    = 0x01
	msgPong    = 0x02
)

func main() {
	bindHost := envOrDefault("BIND_HOST", "127.0.0.1")
	port := envIntOrDefault("PORT", 0)

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

	mux := http.NewServeMux()
	mux.HandleFunc("GET /l2", func(w http.ResponseWriter, r *http.Request) {
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
