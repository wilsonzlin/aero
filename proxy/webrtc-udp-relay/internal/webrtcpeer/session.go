package webrtcpeer

import (
	"log/slog"
	"sync"

	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/metrics"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
)

// webrtcDataChannelUDPFrameOverheadBytes is the worst-case overhead (in bytes)
// for a single UDP relay DataChannel message on top of MAX_DATAGRAM_PAYLOAD_BYTES.
//
// This matches the v2 header carrying an IPv6 address (see udpproto.EncodeV2):
//
//	magic+version+af+type (4) + guest_port (2) + ipv6 (16) + remote_port (2) = 24
const webrtcDataChannelUDPFrameOverheadBytes = 24

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

	maxDataChannelMessageBytes int

	aeroSessionCookie    string
	hasAeroSessionCookie bool

	mu    sync.Mutex
	r     *relay.SessionRelay
	l2    *l2Bridge
	close sync.Once
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

func NewSession(api *webrtc.API, iceServers []webrtc.ICEServer, relayCfg relay.Config, destPolicy *policy.DestinationPolicy, quota *relay.Session, origin, credential string, aeroSessionCookie *string, maxDataChannelMessageBytes int, onClose func()) (*Session, error) {
	if api == nil {
		api = webrtc.NewAPI()
	}

	relayCfg = relayCfg.WithDefaults()

	pc, err := api.NewPeerConnection(webrtc.Configuration{ICEServers: iceServers})
	if err != nil {
		return nil, err
	}
	s := &Session{
		pc:                         pc,
		relayCfg:                   relayCfg,
		destPolicy:                 destPolicy,
		quota:                      quota,
		origin:                     origin,
		credential:                 credential,
		maxDataChannelMessageBytes: maxDataChannelMessageBytes,
		onClose:                    onClose,
	}
	if aeroSessionCookie != nil {
		s.aeroSessionCookie = *aeroSessionCookie
		s.hasAeroSessionCookie = true
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
		switch dc.Label() {
		case DataChannelLabelUDP:
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
			// framing overhead (v2 IPv6 is the worst case at 24 bytes).
			//
			// Note: This runs after pion has already allocated msg.Data, so it is not
			// a replacement for SCTP-level receive caps. It is still valuable to
			// quickly tear down misbehaving peers that send oversized messages.
			udpMaxMsgBytes := udpCfg.MaxDatagramPayloadBytes + webrtcDataChannelUDPFrameOverheadBytes
			if udpMaxMsgBytes < 0 {
				udpMaxMsgBytes = 0
			}

			dc.OnMessage(func(msg webrtc.DataChannelMessage) {
				if s.maxDataChannelMessageBytes > 0 && len(msg.Data) > s.maxDataChannelMessageBytes {
					var sessionID any
					if s.quota != nil {
						sessionID = s.quota.ID()
					}
					slog.Warn("udp datachannel message too large",
						"session_id", sessionID,
						"msg_bytes", len(msg.Data),
						"max_bytes", s.maxDataChannelMessageBytes,
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
		case DataChannelLabelL2:
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
				if s.maxDataChannelMessageBytes > 0 && len(msg.Data) > s.maxDataChannelMessageBytes {
					var sessionID any
					if s.quota != nil {
						sessionID = s.quota.ID()
					}
					slog.Warn("l2 datachannel message too large",
						"session_id", sessionID,
						"msg_bytes", len(msg.Data),
						"max_bytes", s.maxDataChannelMessageBytes,
					)
					// Close asynchronously so we never block a pion callback on teardown.
					go func() { _ = s.Close() }()
					return
				}
				if msg.IsString {
					return
				}
				if maxL2MessageBytes > 0 && len(msg.Data) > maxL2MessageBytes {
					// Close asynchronously so we never block a pion callback on teardown.
					go func() { _ = dc.Close() }()
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
		case webrtc.PeerConnectionStateFailed, webrtc.PeerConnectionStateClosed:
			_ = s.Close()
		}
	})

	return s, nil
}

func (s *Session) PeerConnection() *webrtc.PeerConnection {
	return s.pc
}

func (s *Session) Close() error {
	var err error
	s.close.Do(func() {
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
