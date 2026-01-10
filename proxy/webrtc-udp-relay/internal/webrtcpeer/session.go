package webrtcpeer

import (
	"sync"

	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
)

// Session owns a server-side PeerConnection and (optionally) a SessionRelay bound
// to the "udp" DataChannel.
type Session struct {
	pc         *webrtc.PeerConnection
	relayCfg   relay.Config
	destPolicy *policy.DestinationPolicy
	onClose    func()

	mu    sync.Mutex
	r     *relay.SessionRelay
	close sync.Once
}

func NewSession(api *webrtc.API, relayCfg relay.Config, destPolicy *policy.DestinationPolicy, onClose func()) (*Session, error) {
	if api == nil {
		api = webrtc.NewAPI()
	}

	pc, err := api.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		return nil, err
	}
	s := &Session{
		pc:         pc,
		relayCfg:   relayCfg,
		destPolicy: destPolicy,
		onClose:    onClose,
	}

	pc.OnDataChannel(func(dc *webrtc.DataChannel) {
		if dc.Label() != "udp" {
			return
		}

		r := relay.NewSessionRelay(dc, relayCfg, destPolicy)

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
		s.mu.Unlock()
		if s.onClose != nil {
			s.onClose()
		}
		err = s.pc.Close()
	})
	return err
}
