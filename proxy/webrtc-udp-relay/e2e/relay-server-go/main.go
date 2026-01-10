package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/signal"
	"strconv"
	"sync"
	"syscall"
	"time"

	"github.com/pion/webrtc/v4"
	"golang.org/x/net/websocket"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/signaling"
)

func main() {
	bindHost := envOrDefault("BIND_HOST", "127.0.0.1")
	port := envIntOrDefault("PORT", 0)

	if v := os.Getenv("AUTH_MODE"); v != "" && v != "none" {
		fmt.Fprintf(os.Stderr, "unsupported AUTH_MODE=%s\n", v)
		os.Exit(2)
	}

	listenAddr := net.JoinHostPort(bindHost, strconv.Itoa(port))
	ln, err := net.Listen("tcp", listenAddr)
	if err != nil {
		fmt.Fprintf(os.Stderr, "listen %s: %v\n", listenAddr, err)
		os.Exit(1)
	}

	mux := http.NewServeMux()

	mux.HandleFunc("GET /webrtc/ice", func(w http.ResponseWriter, r *http.Request) {
		// This endpoint is intentionally permissive for local E2E tests.
		w.Header().Set("Access-Control-Allow-Origin", "*")
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte("[]"))
	})

	wsSrv := websocket.Server{
		Handshake: func(cfg *websocket.Config, r *http.Request) error {
			// Accept all origins for E2E.
			origin, _ := websocket.Origin(cfg, r)
			if origin == nil {
				origin = &url.URL{Scheme: "http", Host: "localhost"}
			}
			cfg.Origin = origin
			return nil
		},
		Handler: websocket.Handler(func(ws *websocket.Conn) {
			serveSignaling(ws)
		}),
	}
	mux.Handle("/webrtc/signal", wsSrv)

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

func serveSignaling(ws *websocket.Conn) {
	defer ws.Close()

	var raw string
	if err := websocket.Message.Receive(ws, &raw); err != nil {
		return
	}

	offerReq, err := signaling.ParseOfferRequestJSON([]byte(raw))
	if err != nil {
		return
	}

	api := webrtc.NewAPI()

	pc, err := api.NewPeerConnection(webrtc.Configuration{
		ICEServers: nil,
	})
	if err != nil {
		return
	}
	defer pc.Close()

	pc.OnDataChannel(func(dc *webrtc.DataChannel) {
		if dc.Label() != "udp" {
			return
		}

		destPolicy := policy.NewDevDestinationPolicy()
		var sendMu sync.Mutex
		engine := relay.NewEngine(relay.EngineConfig{
			PreferV2: true,
			Policy:   destPolicy,
		}, func(pkt []byte) error {
			// Pion's DataChannel is safe for concurrent Send calls, but we keep
			// sends serialized anyway to reduce the chance of surprising races in
			// the E2E harness.
			sendMu.Lock()
			defer sendMu.Unlock()
			return dc.Send(pkt)
		})

		dc.OnMessage(func(msg webrtc.DataChannelMessage) {
			if msg.IsString {
				return
			}
			// Handle each frame in its own goroutine so we don't block pion internals.
			data := append([]byte(nil), msg.Data...)
			go func() {
				_ = engine.HandleClientFrame(data)
			}()
		})

		dc.OnClose(func() {
			_ = engine.Close()
		})
	})

	if err := pc.SetRemoteDescription(webrtc.SessionDescription{
		Type: webrtc.SDPTypeOffer,
		SDP:  offerReq.Offer.SDP,
	}); err != nil {
		return
	}

	answer, err := pc.CreateAnswer(nil)
	if err != nil {
		return
	}

	gatherComplete := webrtc.GatheringCompletePromise(pc)
	if err := pc.SetLocalDescription(answer); err != nil {
		return
	}
	<-gatherComplete

	local := pc.LocalDescription()
	if local == nil {
		return
	}

	resp := signaling.AnswerResponse{
		Version: signaling.Version1,
		Answer: signaling.SessionDescription{
			Type: "answer",
			SDP:  local.SDP,
		},
	}

	out, err := json.Marshal(resp)
	if err != nil {
		return
	}
	_ = websocket.Message.Send(ws, string(out))

	// Keep the WebSocket open until the client disconnects (the WebRTC transport
	// doesn't strictly require this, but it simplifies lifecycle management).
	for {
		var discard string
		if err := websocket.Message.Receive(ws, &discard); err != nil {
			return
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
