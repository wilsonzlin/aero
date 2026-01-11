package webrtcpeer

import (
	"context"
	"testing"
	"time"
)

func TestL2Bridge_HandleDataChannelMessageBlocksWhenQueueFull(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	t.Cleanup(cancel)

	b := &l2Bridge{
		ctx:       ctx,
		cancel:    cancel,
		toBackend: make(chan []byte, 1),
	}

	// Fill the buffer so the next send must block (unless context is canceled).
	b.toBackend <- []byte{0x01}

	done := make(chan struct{})
	go func() {
		b.HandleDataChannelMessage([]byte{0x02})
		close(done)
	}()

	select {
	case <-done:
		t.Fatalf("HandleDataChannelMessage returned while toBackend was full; expected backpressure")
	case <-time.After(100 * time.Millisecond):
	}

	cancel()

	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatalf("HandleDataChannelMessage did not unblock after context cancellation")
	}
}
