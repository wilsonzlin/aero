package signaling_test

import (
	"encoding/json"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/gorilla/websocket"
	"github.com/pion/webrtc/v4"

	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/config"
	"github.com/wilsonzlin/aero/proxy/webrtc-udp-relay/internal/signaling"
)

func TestWebSocket_QueryParamAuthOfferAnswer(t *testing.T) {
	cfg := config.Config{
		AuthMode:                      config.AuthModeAPIKey,
		APIKey:                        "secret",
		SignalingAuthTimeout:          2 * time.Second,
		MaxSignalingMessageBytes:      64 * 1024,
		MaxSignalingMessagesPerSecond: 50,
	}
	srv, err := signaling.NewWebSocketServer(cfg)
	if err != nil {
		t.Fatalf("NewWebSocketServer: %v", err)
	}

	ts := httptest.NewServer(srv)
	defer ts.Close()

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/?apiKey=secret"
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })

	pc, err := webrtc.NewPeerConnection(webrtc.Configuration{})
	if err != nil {
		t.Fatalf("NewPeerConnection: %v", err)
	}
	t.Cleanup(func() { _ = pc.Close() })

	ordered := false
	maxRetransmits := uint16(0)
	if _, err := pc.CreateDataChannel("udp", &webrtc.DataChannelInit{Ordered: &ordered, MaxRetransmits: &maxRetransmits}); err != nil {
		t.Fatalf("CreateDataChannel: %v", err)
	}

	offer, err := pc.CreateOffer(nil)
	if err != nil {
		t.Fatalf("CreateOffer: %v", err)
	}
	if err := pc.SetLocalDescription(offer); err != nil {
		t.Fatalf("SetLocalDescription: %v", err)
	}
	<-webrtc.GatheringCompletePromise(pc)

	localOffer := pc.LocalDescription()
	if localOffer == nil {
		t.Fatalf("missing local offer")
	}

	req := map[string]any{
		"version": 1,
		"offer": map[string]any{
			"type": localOffer.Type.String(),
			"sdp":  localOffer.SDP,
		},
	}
	if err := c.WriteJSON(req); err != nil {
		t.Fatalf("WriteJSON offer: %v", err)
	}

	_ = c.SetReadDeadline(time.Now().Add(5 * time.Second))
	_, msg, err := c.ReadMessage()
	if err != nil {
		t.Fatalf("ReadMessage: %v", err)
	}

	var resp struct {
		Version int `json:"version"`
		Answer  struct {
			Type string `json:"type"`
			SDP  string `json:"sdp"`
		} `json:"answer"`
	}
	if err := json.Unmarshal(msg, &resp); err != nil {
		t.Fatalf("unmarshal answer: %v", err)
	}
	if resp.Version != 1 {
		t.Fatalf("version=%d, want 1", resp.Version)
	}
	if resp.Answer.Type != "answer" {
		t.Fatalf("answer.type=%q, want %q", resp.Answer.Type, "answer")
	}
	if resp.Answer.SDP == "" {
		t.Fatalf("answer.sdp empty")
	}

	if err := pc.SetRemoteDescription(webrtc.SessionDescription{Type: webrtc.SDPTypeAnswer, SDP: resp.Answer.SDP}); err != nil {
		t.Fatalf("SetRemoteDescription(answer): %v", err)
	}
}

func TestWebSocket_UnauthenticatedClosesAfterTimeout(t *testing.T) {
	cfg := config.Config{
		AuthMode:                      config.AuthModeAPIKey,
		APIKey:                        "secret",
		SignalingAuthTimeout:          50 * time.Millisecond,
		MaxSignalingMessageBytes:      64 * 1024,
		MaxSignalingMessagesPerSecond: 50,
	}
	srv, err := signaling.NewWebSocketServer(cfg)
	if err != nil {
		t.Fatalf("NewWebSocketServer: %v", err)
	}

	ts := httptest.NewServer(srv)
	defer ts.Close()

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/"
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })

	_ = c.SetReadDeadline(time.Now().Add(500 * time.Millisecond))
	_, _, err = c.ReadMessage()
	if err == nil {
		t.Fatalf("expected close error")
	}
	if !websocket.IsCloseError(err, websocket.ClosePolicyViolation) {
		t.Fatalf("expected policy violation close; got %v", err)
	}
}

func TestWebSocket_OversizedMessageIsRejected(t *testing.T) {
	cfg := config.Config{
		AuthMode:                      config.AuthModeAPIKey,
		APIKey:                        "secret",
		SignalingAuthTimeout:          2 * time.Second,
		MaxSignalingMessageBytes:      32,
		MaxSignalingMessagesPerSecond: 50,
	}
	srv, err := signaling.NewWebSocketServer(cfg)
	if err != nil {
		t.Fatalf("NewWebSocketServer: %v", err)
	}

	ts := httptest.NewServer(srv)
	defer ts.Close()

	wsURL := "ws" + strings.TrimPrefix(ts.URL, "http") + "/?apiKey=secret"
	c, _, err := websocket.DefaultDialer.Dial(wsURL, nil)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })

	oversized := `{"version":1,"offer":{"type":"offer","sdp":"` + strings.Repeat("a", 128) + `"}}`
	if err := c.WriteMessage(websocket.TextMessage, []byte(oversized)); err != nil {
		t.Fatalf("WriteMessage oversized: %v", err)
	}

	_ = c.SetReadDeadline(time.Now().Add(500 * time.Millisecond))
	_, _, err = c.ReadMessage()
	if err == nil {
		t.Fatalf("expected close error")
	}
	if !websocket.IsCloseError(err, websocket.CloseMessageTooBig) {
		t.Fatalf("expected message too big close; got %v", err)
	}
}
