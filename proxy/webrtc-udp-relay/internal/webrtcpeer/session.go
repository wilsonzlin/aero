package webrtcpeer

import (
	"log/slog"
	"sync"

	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
)

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

	mu    sync.Mutex
	r     *relay.SessionRelay
	l2    *l2Bridge
	close sync.Once
}

func NewSession(api *webrtc.API, iceServers []webrtc.ICEServer, relayCfg relay.Config, destPolicy *policy.DestinationPolicy, quota *relay.Session, origin, credential string, onClose func()) (*Session, error) {
	if api == nil {
		api = webrtc.NewAPI()
	}

	pc, err := api.NewPeerConnection(webrtc.Configuration{ICEServers: iceServers})
	if err != nil {
		return nil, err
	}
	s := &Session{
		pc:         pc,
		relayCfg:   relayCfg,
		destPolicy: destPolicy,
		quota:      quota,
		origin:     origin,
		credential: credential,
		onClose:    onClose,
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

			r := relay.NewSessionRelay(dc, relayCfg, destPolicy, quota)

			s.mu.Lock()
			if s.r != nil {
				s.r.Close()
			}
			s.r = r
			s.mu.Unlock()

			dc.OnMessage(func(msg webrtc.DataChannelMessage) {
				if msg.IsString {
					return
				}
				r.HandleDataChannelMessage(msg.Data)
			})
			dc.OnClose(func() {
				r.Close()
			})
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

			cfg := relayCfg.WithDefaults()
			if cfg.L2BackendWSURL == "" {
				_ = dc.Close()
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
				MaxMessageBytes:       cfg.L2MaxMessageBytes,
			}
			b := newL2Bridge(dc, dialCfg, quota)

			s.mu.Lock()
			if s.l2 != nil {
				s.l2.Close()
			}
			s.l2 = b
			s.mu.Unlock()

			dc.OnMessage(func(msg webrtc.DataChannelMessage) {
				if msg.IsString {
					return
				}
				// Copy because pion reuses internal buffers.
				data := append([]byte(nil), msg.Data...)
				b.HandleDataChannelMessage(data)
			})
			dc.OnClose(func() {
				s.mu.Lock()
				if s.l2 == b {
					s.l2 = nil
				}
				s.mu.Unlock()
				b.Close()
			})
		default:
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
