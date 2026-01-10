package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"net"
	"os"
	"os/signal"
	"syscall"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/httpserver"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/webrtcpeer"
)

var (
	// Set via -ldflags at build time. Values may be empty in local/dev builds.
	buildCommit = ""
	buildTime   = ""
)

func main() {
	cfg, err := config.Load(os.Args[1:])
	if err != nil {
		if errors.Is(err, flag.ErrHelp) {
			return
		}
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}

	logger, err := config.NewLogger(cfg)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}

	// Construct the WebRTC API early so misconfigurations are caught on startup.
	// This does not start any networking; ICE sockets are only created once we
	// start creating PeerConnections.
	if _, err := webrtcpeer.NewAPI(cfg); err != nil {
		logger.Error("failed to configure webrtc", "err", err)
		os.Exit(2)
	}

	logger.Info("starting aero-webrtc-udp-relay",
		"listen_addr", cfg.ListenAddr,
		"public_base_url", cfg.PublicBaseURL,
		"mode", cfg.Mode,
	)

	ln, err := net.Listen("tcp", cfg.ListenAddr)
	if err != nil {
		logger.Error("failed to listen", "err", err)
		os.Exit(1)
	}

	build := httpserver.BuildInfo{
		Commit:    buildCommit,
		BuildTime: buildTime,
	}

	srv := httpserver.New(cfg, logger, build)

	errCh := make(chan error, 1)
	go func() {
		errCh <- srv.Serve(ln)
	}()

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	select {
	case err := <-errCh:
		if err != nil && !errors.Is(err, httpserver.ErrServerClosed) {
			logger.Error("http server exited", "err", err)
			os.Exit(1)
		}
		return
	case <-ctx.Done():
		logger.Info("shutdown signal received")
	}

	shutdownCtx, cancel := context.WithTimeout(context.Background(), cfg.ShutdownTimeout)
	defer cancel()

	if err := srv.Shutdown(shutdownCtx); err != nil {
		logger.Error("http server shutdown failed", "err", err)
	}

	if err := <-errCh; err != nil && !errors.Is(err, httpserver.ErrServerClosed) {
		logger.Error("http server exited after shutdown", "err", err)
		os.Exit(1)
	}
}
