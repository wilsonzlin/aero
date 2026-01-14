package signaling

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/gorilla/websocket"
	"github.com/pion/webrtc/v4"

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
		ICEGatheringTimeout: 2 * time.Second,
		Authorizer:          allowAllAuthorizer{},
	})

	mux := http.NewServeMux()
	s.RegisterRoutes(mux)
	ts := httptest.NewServer(mux)
	defer ts.Close()

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/webrtc/signal"
	ws, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
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
			_, raw, err := ws.ReadMessage()
			if err != nil {
				readErr <- err
				return
			}

			msg, err := parseSignalMessage(raw)
			if err != nil {
				readErr <- err
				return
			}

			switch msg.Type {
			case messageTypeAnswer:
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
			case messageTypeCandidate:
				cand := msg.Candidate.ToPion()
				select {
				case <-remoteDescSet:
					_ = clientPC.AddICECandidate(cand)
				default:
					remoteCandidateBufMu.Lock()
					remoteCandidateBuf = append(remoteCandidateBuf, cand)
					remoteCandidateBufMu.Unlock()
				}
			case messageTypeError:
				readErr <- &protocolError{Code: msg.Code, Message: msg.Message}
				return
			default:
				readErr <- &protocolError{Code: "bad_message", Message: "unexpected server message"}
				return
			}
		}
	}()

	offerSent := make(chan struct{})
	var wsWriteMu sync.Mutex
	var localCandidateBufMu sync.Mutex
	var localCandidateBuf []webrtc.ICECandidateInit

	clientPC.OnICECandidate(func(c *webrtc.ICECandidate) {
		if c == nil {
			return
		}
		init := c.ToJSON()

		select {
		case <-offerSent:
			_ = sendWS(ws, &wsWriteMu, signalMessage{
				Type:      messageTypeCandidate,
				Candidate: ptr(candidateFromPion(init)),
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

	if err := sendWS(ws, &wsWriteMu, signalMessage{
		Type: messageTypeOffer,
		SDP:  ptr(sdpFromPion(offer)),
	}); err != nil {
		t.Fatalf("send offer: %v", err)
	}
	close(offerSent)

	localCandidateBufMu.Lock()
	buf := localCandidateBuf
	localCandidateBuf = nil
	localCandidateBufMu.Unlock()
	for _, cand := range buf {
		_ = sendWS(ws, &wsWriteMu, signalMessage{
			Type:      messageTypeCandidate,
			Candidate: ptr(candidateFromPion(cand)),
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

func sendWS(conn *websocket.Conn, writeMu *sync.Mutex, msg signalMessage) error {
	data, err := json.Marshal(msg)
	if err != nil {
		return err
	}
	writeMu.Lock()
	defer writeMu.Unlock()
	return conn.WriteMessage(websocket.TextMessage, data)
}
