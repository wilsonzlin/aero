package relay

import (
	"sync"
	"sync/atomic"
)

// sendQueue is a byte-bounded FIFO queue.
//
// It is used to buffer outbound DataChannel frames so UDP read loops never block
// on WebRTC backpressure.
type sendQueue struct {
	mu       sync.Mutex
	notEmpty *sync.Cond
	closed   bool

	maxBytes int
	curBytes int
	frames   [][]byte

	drops atomic.Uint64
}

func newSendQueue(maxBytes int) *sendQueue {
	q := &sendQueue{maxBytes: maxBytes}
	q.notEmpty = sync.NewCond(&q.mu)
	return q
}

func (q *sendQueue) DropCount() uint64 {
	return q.drops.Load()
}

// Enqueue appends frame to the queue if it fits within the byte budget.
// It never blocks.
func (q *sendQueue) Enqueue(frame []byte) bool {
	q.mu.Lock()
	defer q.mu.Unlock()
	if q.closed {
		q.drops.Add(1)
		return false
	}
	if len(frame) > q.maxBytes {
		q.drops.Add(1)
		return false
	}
	if q.curBytes+len(frame) > q.maxBytes {
		q.drops.Add(1)
		return false
	}

	q.frames = append(q.frames, frame)
	q.curBytes += len(frame)
	q.notEmpty.Signal()
	return true
}

// Dequeue blocks until a frame is available or the queue is closed and empty.
func (q *sendQueue) Dequeue() ([]byte, bool) {
	q.mu.Lock()
	defer q.mu.Unlock()
	for len(q.frames) == 0 && !q.closed {
		q.notEmpty.Wait()
	}
	if len(q.frames) == 0 {
		return nil, false
	}
	frame := q.frames[0]
	copy(q.frames, q.frames[1:])
	q.frames[len(q.frames)-1] = nil
	q.frames = q.frames[:len(q.frames)-1]
	q.curBytes -= len(frame)
	return frame, true
}

func (q *sendQueue) Close() {
	q.mu.Lock()
	q.closed = true
	for i := range q.frames {
		q.frames[i] = nil
	}
	q.frames = nil
	q.curBytes = 0
	q.mu.Unlock()
	q.notEmpty.Broadcast()
}
