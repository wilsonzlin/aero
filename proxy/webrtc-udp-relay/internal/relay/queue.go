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
	head     int

	drops atomic.Uint64

	onDrop func()
}

func newSendQueue(maxBytes int) *sendQueue {
	q := &sendQueue{maxBytes: maxBytes}
	q.notEmpty = sync.NewCond(&q.mu)
	return q
}

func (q *sendQueue) SetOnDrop(fn func()) {
	q.mu.Lock()
	q.onDrop = fn
	q.mu.Unlock()
}

func (q *sendQueue) DropCount() uint64 {
	return q.drops.Load()
}

// Enqueue appends frame to the queue if it fits within the byte budget.
// It never blocks.
func (q *sendQueue) Enqueue(frame []byte) bool {
	q.mu.Lock()
	if q.closed || len(frame) > q.maxBytes || q.curBytes+len(frame) > q.maxBytes {
		q.drops.Add(1)
		onDrop := q.onDrop
		q.mu.Unlock()
		if onDrop != nil {
			onDrop()
		}
		return false
	}

	q.frames = append(q.frames, frame)
	q.curBytes += len(frame)
	q.notEmpty.Signal()
	q.mu.Unlock()
	return true
}

// Dequeue blocks until a frame is available or the queue is closed and empty.
func (q *sendQueue) Dequeue() ([]byte, bool) {
	q.mu.Lock()
	defer q.mu.Unlock()
	for q.head >= len(q.frames) && !q.closed {
		// Reset to keep the slice compact when drained.
		if q.head > 0 {
			q.frames = q.frames[:0]
			q.head = 0
		}
		q.notEmpty.Wait()
	}
	if q.head >= len(q.frames) {
		return nil, false
	}
	frame := q.frames[q.head]
	q.frames[q.head] = nil
	q.head++
	q.curBytes -= len(frame)

	// Compact occasionally to avoid unbounded growth in the underlying array.
	//
	// This keeps Dequeue O(1) amortized instead of shifting the slice on every
	// call.
	if q.head >= len(q.frames) {
		q.frames = q.frames[:0]
		q.head = 0
	} else if q.head > 1024 && q.head*2 >= len(q.frames) {
		n := copy(q.frames, q.frames[q.head:])
		for i := n; i < len(q.frames); i++ {
			q.frames[i] = nil
		}
		q.frames = q.frames[:n]
		q.head = 0
	}
	return frame, true
}

func (q *sendQueue) Close() {
	q.mu.Lock()
	q.closed = true
	for i := range q.frames {
		q.frames[i] = nil
	}
	q.frames = nil
	q.head = 0
	q.curBytes = 0
	q.mu.Unlock()
	q.notEmpty.Broadcast()
}
