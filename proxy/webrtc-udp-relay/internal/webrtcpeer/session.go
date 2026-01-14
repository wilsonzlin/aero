package webrtcpeer

import (
	"log/slog"
	"sync"
	"time"

	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/udpproto"
)

// webrtcDataChannelUDPFrameOverheadBytes is the worst-case overhead (in bytes)
// for a single UDP relay DataChannel message on top of MAX_DATAGRAM_PAYLOAD_BYTES.
//
// This matches the v2 header carrying an IPv6 address (see udpproto.Codec.EncodeFrameV2):
//
//	magic+version+af+type (4) + guest_port (2) + ipv6 (16) + remote_port (2) = 24
const webrtcDataChannelUDPFrameOverheadBytes = udpproto.MaxFrameOverheadBytes

type SessionOptions struct {
	// ConnectTimeout bounds how long the session is allowed to remain in a
	// non-connected state before being closed. Values <= 0 disable the timeout.
	ConnectTimeout time.Duration

	// RemoteAddr is optional caller-provided connection info used for logging.
	RemoteAddr string
}

// Session owns a server-side PeerConnection and binds relay adapters to specific
// DataChannel labels:
//   - "udp": WebRTC UDP relay
//   - "l2":  L2 tunnel transport bridge (DataChannel <-> backend WebSocket)
type Session struct {
	pc         *webrtc.PeerConnection
	relayCfg   relay.Config
	destPolicy *policy.DestinationPolicy
	quota      *relay.Session
	origin     string
	credential string
	onClose    func()

	remoteAddr string

	maxDataChannelMessageBytes int

	aeroSessionCookie    string
	hasAeroSessionCookie bool

	mu    sync.Mutex
	r     udpRelay
	l2    *l2Bridge
	close sync.Once

	connectTimerMu sync.Mutex
	connectTimer   *time.Timer
}

type udpRelay interface {
	EnableWebRTCUDPMetrics()
	HandleDataChannelMessage(msg []byte)
	Close()
}

func (s *Session) incMetric(name string) {
	if s.quota == nil {
		return
	}
	m := s.quota.Metrics()
	if m == nil {
		return
	}
	m.Inc(name)
}

func rejectDataChannel(dc *webrtc.DataChannel) {
	// Ensure the channel is closed even if it's not yet fully open on this side.
	dc.OnOpen(func() {
		_ = dc.Close()
	})
	_ = dc.Close()
}

func NewSession(api *webrtc.API, iceServers []webrtc.ICEServer, relayCfg relay.Config, destPolicy *policy.DestinationPolicy, quota *relay.Session, origin, credential string, aeroSessionCookie *string, maxDataChannelMessageBytes int, opts SessionOptions, onClose func()) (*Session, error) {
	if api == nil {
		api = webrtc.NewAPI()
	}

	relayCfg = relayCfg.WithDefaults()

	pc, err := api.NewPeerConnection(webrtc.Configuration{ICEServers: iceServers})
	if err != nil {
		return nil, err
	}

	maxMessageBytes := configuredSCTPMaxMessageSize(api)
	s := &Session{
		pc:                         pc,
		relayCfg:                   relayCfg,
		destPolicy:                 destPolicy,
		quota:                      quota,
		origin:                     origin,
		credential:                 credential,
		remoteAddr:                 opts.RemoteAddr,
		maxDataChannelMessageBytes: maxDataChannelMessageBytes,
		onClose:                    onClose,
	}
	if aeroSessionCookie != nil {
		s.aeroSessionCookie = *aeroSessionCookie
		s.hasAeroSessionCookie = true
	}

	// If the SCTP association errors (e.g. due to an oversized inbound message),
	// ensure we tear down the whole session so relay goroutines exit and resources
	// are released.
	var sctpOnce sync.Once
	installSCTPHandlers := func() {
		st := pc.SCTP()
		if st == nil {
			return
		}
		sctpOnce.Do(func() {
			st.OnError(func(err error) {
				var sessionID any
				if s.quota != nil {
					sessionID = s.quota.ID()
				}
				slog.Warn("sctp transport error", "session_id", sessionID, "err", err)
				go func() { _ = s.Close() }()
			})
			st.OnClose(func(err error) {
				var sessionID any
				if s.quota != nil {
					sessionID = s.quota.ID()
				}
				slog.Warn("sctp transport closed", "session_id", sessionID, "err", err)
				go func() { _ = s.Close() }()
			})
		})
	}

	if quota != nil {
		quota.OnHardClose(func() {
			// Close asynchronously so we never block a UDP read loop on pion teardown.
			go func() {
				_ = s.Close()
			}()
		})
	}

	pc.OnDataChannel(func(dc *webrtc.DataChannel) {
		installSCTPHandlers()
		switch dc.Label() {
		case dataChannelLabelUDP:
			if err := validateUDPDataChannel(dc); err != nil {
				_ = dc.Close()
				return
			}

			// Bind exactly one active "udp" DataChannel per PeerConnection.
			s.mu.Lock()
			if s.r != nil {
				s.mu.Unlock()
				s.incMetric(metrics.WebRTCDataChannelRejectedDuplicateUDP)
				rejectDataChannel(dc)
				return
			}

			udpCfg := relayCfg
			r := relay.NewSessionRelay(dc, udpCfg, destPolicy, quota, nil)
			r.EnableWebRTCUDPMetrics()
			s.r = r
			s.mu.Unlock()

			dc.OnError(func(err error) {
				var sessionID any
				if s.quota != nil {
					sessionID = s.quota.ID()
				}
				slog.Warn("udp datachannel error", "session_id", sessionID, "err", err)
				// Close asynchronously so we never block a pion callback on teardown.
				go func() { _ = s.Close() }()
			})

			// Defensive cap on inbound DataChannel message size. The UDP relay frames
			// themselves are bounded by the application-level payload limit plus the
			// framing overhead (v2 IPv6 is the worst case at udpproto.MaxFrameOverheadBytes bytes).
			//
			// Note: This runs after pion has already allocated msg.Data, so it is not
			// a replacement for SCTP-level receive caps. It is still valuable to
			// quickly tear down misbehaving peers that send oversized messages.
			udpMaxMsgBytes := udpCfg.MaxDatagramPayloadBytes + webrtcDataChannelUDPFrameOverheadBytes
			if udpMaxMsgBytes < 0 {
				udpMaxMsgBytes = 0
			}

			dc.OnMessage(func(msg webrtc.DataChannelMessage) {
				effectiveMax := s.maxDataChannelMessageBytes
				if effectiveMax <= 0 {
					effectiveMax = maxMessageBytes
				}
				if effectiveMax > 0 && len(msg.Data) > effectiveMax {
					var sessionID any
					if s.quota != nil {
						sessionID = s.quota.ID()
					}
					s.incMetric(metrics.WebRTCDataChannelMessageTooLargeUDP)
					slog.Warn("udp datachannel message too large",
						"session_id", sessionID,
						"msg_bytes", len(msg.Data),
						"max_bytes", effectiveMax,
					)
					// Close asynchronously so we never block a pion callback on teardown.
					go func() { _ = s.Close() }()
					return
				}
				if msg.IsString {
					return
				}
				if udpMaxMsgBytes > 0 && len(msg.Data) > udpMaxMsgBytes {
					var sessionID any
					if s.quota != nil {
						sessionID = s.quota.ID()
					}
					s.incMetric(metrics.WebRTCUDPDatagramsIn)
					s.incMetric(metrics.WebRTCUDPDropped)
					s.incMetric(metrics.WebRTCUDPDroppedOversized)
					slog.Warn("rejecting oversized udp datachannel message",
						"session_id", sessionID,
						"msg_bytes", len(msg.Data),
						"max_bytes", udpMaxMsgBytes,
					)
					// Close asynchronously so we never block a pion callback on teardown.
					go func() { _ = dc.Close() }()
					return
				}
				r.HandleDataChannelMessage(msg.Data)
			})

			cleanup := func() {
				s.mu.Lock()
				if s.r == r {
					s.r = nil
				}
				s.mu.Unlock()
				r.Close()
			}
			dc.OnClose(cleanup)
			if dc.ReadyState() == webrtc.DataChannelStateClosed {
				cleanup()
			}
		case dataChannelLabelL2:
			if err := validateL2DataChannel(dc); err != nil {
				reason := "invalid_datachannel"

				maxRetransmits := dc.MaxRetransmits()
				maxPacketLifeTime := dc.MaxPacketLifeTime()
				if maxRetransmits != nil || maxPacketLifeTime != nil {
					reason = "partial_reliability"
				} else if !dc.Ordered() {
					reason = "unordered"
				}

				var maxRetransmitsValue any
				if maxRetransmits != nil {
					maxRetransmitsValue = int(*maxRetransmits)
				}
				var maxPacketLifeTimeValue any
				if maxPacketLifeTime != nil {
					maxPacketLifeTimeValue = int(*maxPacketLifeTime)
				}
				var sessionID any
				if s.quota != nil {
					sessionID = s.quota.ID()
				}
				slog.Warn("rejecting l2 datachannel",
					"reason", reason,
					"session_id", sessionID,
					"label", dc.Label(),
					"ordered", dc.Ordered(),
					"max_retransmits", maxRetransmitsValue,
					"max_packet_life_time", maxPacketLifeTimeValue,
					"err", err,
				)
				_ = dc.Close()
				return
			}

			cfg := relayCfg
			if cfg.L2BackendWSURL == "" {
				_ = dc.Close()
				return
			}

			s.mu.Lock()
			if s.l2 != nil {
				s.mu.Unlock()
				s.incMetric(metrics.WebRTCDataChannelRejectedDuplicateL2)
				rejectDataChannel(dc)
				return
			}

			dialCfg := l2BackendDialConfig{
				BackendWSURL:          cfg.L2BackendWSURL,
				ClientOrigin:          s.origin,
				Credential:            s.credential,
				ForwardOrigin:         cfg.L2BackendForwardOrigin,
				AuthForwardMode:       cfg.L2BackendAuthForwardMode,
				BackendOriginOverride: cfg.L2BackendWSOrigin,
				BackendToken:          cfg.L2BackendWSToken,
				ForwardAeroSession:    cfg.L2BackendForwardAeroSession,
				AeroSessionCookie:     s.aeroSessionCookie,
				HasAeroSessionCookie:  s.hasAeroSessionCookie,
				MaxMessageBytes:       cfg.L2MaxMessageBytes,
			}
			b := newL2Bridge(dc, dialCfg, quota)
			s.l2 = b
			s.mu.Unlock()

			dc.OnError(func(err error) {
				var sessionID any
				if s.quota != nil {
					sessionID = s.quota.ID()
				}
				slog.Warn("l2 datachannel error", "session_id", sessionID, "err", err)
				// Close asynchronously so we never block a pion callback on teardown.
				go func() { _ = s.Close() }()
			})

			maxL2MessageBytes := dialCfg.MaxMessageBytes
			dc.OnMessage(func(msg webrtc.DataChannelMessage) {
				effectiveMax := s.maxDataChannelMessageBytes
				if effectiveMax <= 0 {
					effectiveMax = maxMessageBytes
				}
				if effectiveMax > 0 && len(msg.Data) > effectiveMax {
					var sessionID any
					if s.quota != nil {
						sessionID = s.quota.ID()
					}
					s.incMetric(metrics.WebRTCDataChannelMessageTooLargeL2)
					slog.Warn("l2 datachannel message too large",
						"session_id", sessionID,
						"msg_bytes", len(msg.Data),
						"max_bytes", effectiveMax,
					)
					// Close asynchronously so we never block a pion callback on teardown.
					go func() { _ = s.Close() }()
					return
				}
				if msg.IsString {
					return
				}
				if maxL2MessageBytes > 0 && len(msg.Data) > maxL2MessageBytes {
					// Let the bridge handle oversize accounting and shutdown. We pass the
					// pion-managed buffer directly because HandleDataChannelMessage does
					// not retain it on the oversize path.
					b.HandleDataChannelMessage(msg.Data)
					return
				}
				// Copy because pion reuses internal buffers.
				data := append([]byte(nil), msg.Data...)
				b.HandleDataChannelMessage(data)
			})

			cleanup := func() {
				s.mu.Lock()
				if s.l2 == b {
					s.l2 = nil
				}
				s.mu.Unlock()
				b.Close()
			}
			dc.OnClose(cleanup)
			if dc.ReadyState() == webrtc.DataChannelStateClosed {
				cleanup()
			}
		default:
			s.incMetric(metrics.WebRTCDataChannelRejectedUnknownLabel)
			rejectDataChannel(dc)
			return
		}
	})

	pc.OnConnectionStateChange(func(state webrtc.PeerConnectionState) {
		switch state {
		case webrtc.PeerConnectionStateConnected:
			s.stopConnectTimer()
		case webrtc.PeerConnectionStateFailed, webrtc.PeerConnectionStateClosed:
			_ = s.Close()
		}
	})

	pc.OnICEConnectionStateChange(func(state webrtc.ICEConnectionState) {
		switch state {
		case webrtc.ICEConnectionStateConnected, webrtc.ICEConnectionStateCompleted:
			s.stopConnectTimer()
		}
	})

	if opts.ConnectTimeout > 0 {
		connectTimeout := opts.ConnectTimeout
		s.connectTimerMu.Lock()
		s.connectTimer = time.AfterFunc(connectTimeout, func() {
			// Avoid tearing down sessions that did manage to connect just before the
			// timeout fired but after any state-change callbacks were scheduled.
			if s.pc.ConnectionState() == webrtc.PeerConnectionStateConnected {
				return
			}
			switch s.pc.ICEConnectionState() {
			case webrtc.ICEConnectionStateConnected, webrtc.ICEConnectionStateCompleted:
				return
			}

			var sessionID any
			if s.quota != nil {
				sessionID = s.quota.ID()
				s.incMetric(metrics.WebRTCSessionConnectTimeout)
			}

			slog.Warn("webrtc session connect timeout",
				"session_id", sessionID,
				"origin", s.origin,
				"remote_addr", s.remoteAddr,
				"timeout", connectTimeout.String(),
			)
			_ = s.Close()
		})
		s.connectTimerMu.Unlock()
	}

	return s, nil
}

func (s *Session) PeerConnection() *webrtc.PeerConnection {
	return s.pc
}

func (s *Session) stopConnectTimer() {
	s.connectTimerMu.Lock()
	if s.connectTimer != nil {
		s.connectTimer.Stop()
		s.connectTimer = nil
	}
	s.connectTimerMu.Unlock()
}

func (s *Session) Close() error {
	var err error
	s.close.Do(func() {
		s.stopConnectTimer()
		s.mu.Lock()
		if s.r != nil {
			s.r.Close()
		}
		if s.l2 != nil {
			s.l2.Close()
		}
		s.mu.Unlock()
		if s.onClose != nil {
			s.onClose()
		}
		err = s.pc.Close()
	})
	return err
}
