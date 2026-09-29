package server

import (
	"fmt"
	"sync"

	"github.com/tinyshed/tinystore"
)

// handles are what a session's open calls answered with a number, as a file
// descriptor stands for a file. The calls after them carry the number, and the
// session's end drops them all.
type handles[T any] struct {
	mu   sync.Mutex
	last uint64
	open map[uint64]T
}

func (h *handles[T]) add(handle T) uint64 {
	h.mu.Lock()
	defer h.mu.Unlock()
	if h.open == nil {
		h.open = map[uint64]T{}
	}
	h.last++
	h.open[h.last] = handle
	return h.last
}

func (h *handles[T]) get(number uint64) (T, error) {
	h.mu.Lock()
	defer h.mu.Unlock()
	handle, found := h.open[number]
	if !found {
		return handle, fmt.Errorf("%w: no handle %d is open on this connection", tinystore.ErrInvalid, number)
	}
	return handle, nil
}

func (h *handles[T]) remove(number uint64) {
	h.mu.Lock()
	defer h.mu.Unlock()
	delete(h.open, number)
}
