package signaling

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/pion/webrtc/v4"
	xwebsocket "golang.org/x/net/websocket"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/policy"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/relay"
)

func TestWebSocketSignaling_TrickleICE_Connects(t *testing.T) {
	mediaEngine := &webrtc.MediaEngine{}
	if err := mediaEngine.RegisterDefaultCodecs(); err != nil {
		t.Fatalf("register codecs: %v", err)
	}

	api := webrtc.NewAPI(webrtc.WithMediaEngine(mediaEngine))

	s := NewServer(Config{
		WebRTC:              api,
		RelayConfig:         relay.DefaultConfig(),
		Policy:              policy.NewDevDestinationPolicy(),
		Authorizer:          AllowAllAuthorizer{},
		ICEGatheringTimeout: 2 * time.Second,
	})

	mux := http.NewServeMux()
	s.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal"
	ws, err := xwebsocket.Dial(wsURL, "", ts.URL)
	if err != nil {
		t.Fatalf("dial websocket: %v", err)
	}
	defer ws.Close()

	clientPC, err := api.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("new pc: %v", err)
	}
	defer clientPC.Close()

	pcConnected := make(chan struct{})
	var pcConnectedOnce sync.Once
	clientPC.OnConnectionStateChange(func(state webrtc.PeerConnectionState) {
		if state != webrtc.PeerConnectionStateConnected {
			return
		}
		pcConnectedOnce.Do(func() { close(pcConnected) })
	})

	dc, err := clientPC.CreateDataChannel("udp", nil)
	if err != nil {
		t.Fatalf("create data channel: %v", err)
	}
	dcOpen := make(chan struct{})
	dc.OnOpen(func() { close(dcOpen) })

	remoteDescSet := make(chan struct{})
	var remoteCandidateBufMu sync.Mutex
	var remoteCandidateBuf []webrtc.ICECandidateInit

	readErr := make(chan error, 1)
	go func() {
		for {
			var raw string
			err := xwebsocket.Message.Receive(ws, &raw)
			if err != nil {
				readErr <- err
				return
			}

			msg, err := ParseSignalMessage([]byte(raw))
			if err != nil {
				readErr <- err
				return
			}

			switch msg.Type {
			case MessageTypeAnswer:
				answer, err := msg.SDP.ToPion()
				if err != nil {
					readErr <- err
					return
				}
				if err := clientPC.SetRemoteDescription(answer); err != nil {
					readErr <- err
					return
				}

				close(remoteDescSet)

				remoteCandidateBufMu.Lock()
				buf := remoteCandidateBuf
				remoteCandidateBuf = nil
				remoteCandidateBufMu.Unlock()

				for _, cand := range buf {
					_ = clientPC.AddICECandidate(cand)
				}
			case MessageTypeCandidate:
				cand := msg.Candidate.ToPion()
				select {
				case <-remoteDescSet:
					_ = clientPC.AddICECandidate(cand)
				default:
					remoteCandidateBufMu.Lock()
					remoteCandidateBuf = append(remoteCandidateBuf, cand)
					remoteCandidateBufMu.Unlock()
				}
			case MessageTypeError:
				readErr <- &protocolError{Code: msg.Code, Message: msg.Message}
				return
			default:
				readErr <- &protocolError{Code: "bad_message", Message: "unexpected server message"}
				return
			}
		}
	}()

	offerSent := make(chan struct{})
	var localCandidateBufMu sync.Mutex
	var localCandidateBuf []webrtc.ICECandidateInit

	clientPC.OnICECandidate(func(c *webrtc.ICECandidate) {
		if c == nil {
			return
		}
		init := c.ToJSON()

		select {
		case <-offerSent:
			_ = sendWS(ws, SignalMessage{
				Type:      MessageTypeCandidate,
				Candidate: ptr(CandidateFromPion(init)),
			})
		default:
			localCandidateBufMu.Lock()
			localCandidateBuf = append(localCandidateBuf, init)
			localCandidateBufMu.Unlock()
		}
	})

	offer, err := clientPC.CreateOffer(nil)
	if err != nil {
		t.Fatalf("create offer: %v", err)
	}
	if err := clientPC.SetLocalDescription(offer); err != nil {
		t.Fatalf("set local offer: %v", err)
	}

	if err := sendWS(ws, SignalMessage{
		Type: MessageTypeOffer,
		SDP:  ptr(SDPFromPion(offer)),
	}); err != nil {
		t.Fatalf("send offer: %v", err)
	}
	close(offerSent)

	localCandidateBufMu.Lock()
	buf := localCandidateBuf
	localCandidateBuf = nil
	localCandidateBufMu.Unlock()
	for _, cand := range buf {
		_ = sendWS(ws, SignalMessage{
			Type:      MessageTypeCandidate,
			Candidate: ptr(CandidateFromPion(cand)),
		})
	}

	select {
	case <-pcConnected:
	case <-dcOpen:
	case err := <-readErr:
		t.Fatalf("signaling failed: %v", err)
	case <-time.After(10 * time.Second):
		t.Fatalf("timeout waiting for peer connection to become connected")
	}
}

type protocolError struct {
	Code    string
	Message string
}

func (e *protocolError) Error() string {
	b, _ := json.Marshal(e)
	return string(b)
}

func sendWS(conn *xwebsocket.Conn, msg SignalMessage) error {
	data, err := json.Marshal(msg)
	if err != nil {
		return err
	}
	return xwebsocket.Message.Send(conn, string(data))
}
