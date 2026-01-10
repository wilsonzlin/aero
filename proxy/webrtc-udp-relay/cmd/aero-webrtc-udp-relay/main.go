package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"net"
	"os"
	"os/signal"
	"runtime/debug"
	"syscall"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/httpserver"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/signaling"
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
	api, err := webrtcpeer.NewAPI(cfg)
	if err != nil {
		logger.Error("failed to configure webrtc", "err", err)
		os.Exit(2)
	}

	logger.Info("starting aero-webrtc-udp-relay",
		"listen_addr", cfg.ListenAddr,
		"public_base_url", cfg.PublicBaseURL,
		"mode", cfg.Mode,
	)

	destPolicy, err := policy.NewDestinationPolicyFromEnv()
	if err != nil {
		logger.Error("failed to load destination policy", "err", err)
		os.Exit(2)
	}

	ln, err := net.Listen("tcp", cfg.ListenAddr)
	if err != nil {
		logger.Error("failed to listen", "err", err)
		os.Exit(1)
	}

	build := resolveBuildInfo(buildCommit, buildTime)

	srv := httpserver.New(cfg, logger, build)
	sessionMgr := relay.NewSessionManager(cfg, nil, nil)
	relayCfg := relay.Config{
		MaxUDPBindingsPerSession:  cfg.MaxUDPBindingsPerSession,
		UDPBindingIdleTimeout:     cfg.UDPBindingIdleTimeout,
		UDPReadBufferBytes:        cfg.UDPReadBufferBytes,
		DataChannelSendQueueBytes: cfg.DataChannelSendQueueBytes,
		PreferV2:                  cfg.PreferV2,
	}
	sig := signaling.NewServer(signaling.Config{
		Sessions:            sessionMgr,
		WebRTC:              api,
		ICEServers:          cfg.PeerConnectionICEServers(),
		RelayConfig:         relayCfg,
		Policy:              destPolicy,
		Authorizer:          signaling.AllowAllAuthorizer{},
		ICEGatheringTimeout: cfg.ICEGatheringTimeout,
	})
	sig.RegisterRoutes(srv.Mux())

	errCh := make(chan error, 1)
	go func() {
		errCh <- srv.Serve(ln)
	}()

	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()

	select {
	case err := <-errCh:
		sig.Close()
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
	sig.Close()

	if err := <-errCh; err != nil && !errors.Is(err, httpserver.ErrServerClosed) {
		logger.Error("http server exited after shutdown", "err", err)
		os.Exit(1)
	}
}

func resolveBuildInfo(commit, buildTime string) httpserver.BuildInfo {
	// Prefer ldflags-injected values (production builds) but fall back to the Go
	// build info when available (useful for `go run` / dev builds).
	if bi, ok := debug.ReadBuildInfo(); ok {
		for _, s := range bi.Settings {
			switch s.Key {
			case "vcs.revision":
				if commit == "" {
					commit = s.Value
				}
			case "vcs.time":
				if buildTime == "" {
					buildTime = s.Value
				}
			}
		}
	}

	return httpserver.BuildInfo{
		Commit:    commit,
		BuildTime: buildTime,
	}
}
