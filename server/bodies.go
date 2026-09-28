package server

import "sync"

// the bodies frames are read into come from pools of two sizes, a call's and
// a transfer's chunk, so that neither allocates a body of its own; a larger
// body is allocated and left to the collector
const (
	callBody  = 4 << 10
	chunkBody = 64 << 10
)

var callBodies, chunkBodies sync.Pool

// takeBody lends a buffer that holds n bytes
func takeBody(n uint32) []byte {
	switch {
	case n <= callBody:
		return lend(&callBodies, callBody)
	case n <= chunkBody:
		return lend(&chunkBodies, chunkBody)
	}
	return make([]byte, n)
}

func lend(pool *sync.Pool, size int) []byte {
	if held, ok := pool.Get().(*[]byte); ok {
		return *held
	}
	return make([]byte, size)
}

// giveBody returns a buffer takeBody lent; one it did not lend is dropped
func giveBody(body []byte) {
	switch cap(body) {
	case callBody:
		body = body[:callBody]
		callBodies.Put(&body)
	case chunkBody:
		body = body[:chunkBody]
		chunkBodies.Put(&body)
	}
}
