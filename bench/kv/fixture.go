package main

import (
	"encoding/base32"
	"encoding/binary"
	"math/rand/v2"
	"sync/atomic"
	"time"
)

const day = 24 * time.Hour

// the fixture's streams: each draws from its own seed, so that one case's
// input does not move with another's
const (
	seedTokens uint64 = iota + 1
	seedSessionAges
	seedSessionRequests
	seedCodes
	seedCodeAges
	seedLinkRequests
	seedCountAges
	seedAttemptRequests
	seedEvents
	seedEventAges
	seedClaimRequests
	seedDraftBodies
	seedDraftAges
	seedDraftRequests
)

// seeded is one stream of the fixture, the same in every run, so that both
// implementations get identical input
func seeded(seed uint64, stream int64) *rand.Rand {
	return rand.New(rand.NewPCG(seed, uint64(stream))) //nolint:gosec // a seeded fixture; streams are not negative
}

var tokenText = base32.StdEncoding.WithPadding(base32.NoPadding)

// randomToken is 128 random bits as crypto/rand.Text spells them, in 26 letters and digits
func randomToken(random *rand.Rand) string {
	var raw [16]byte
	binary.LittleEndian.PutUint64(raw[:8], random.Uint64())
	binary.LittleEndian.PutUint64(raw[8:], random.Uint64())
	return tokenText.EncodeToString(raw[:])
}

// token is the i-th token of a stream
func token(seed uint64, i int64) string {
	return randomToken(seeded(seed, i))
}

// spread is i's share of a duration, an age or the life left to it, the same in every run
func spread(seed uint64, i int64, over time.Duration) time.Duration {
	return time.Duration(seeded(seed, i).Int64N(int64(over)))
}

// scatter is 0…count-1 in the order users arrive, each once and far from the
// last: n·step mod count, with step near count/φ and coprime with count
//
//	count 10, step 7 → 0 7 4 1 8 5 2 9 6 3
type scatter struct {
	count, step int
}

func newScatter(count int) scatter {
	step := max(int(float64(count)*0.618), 1)
	for gcd(step, count) != 1 {
		step++
	}
	return scatter{count: count, step: step}
}

func (s scatter) at(n int) int {
	return n * s.step % s.count
}

func gcd(a, b int) int {
	for b != 0 {
		a, b = b, a%b
	}
	return a
}

// the rows a preparation writes in one transaction
const prepareBatch = 10_000

// inBatches hands write the ranges of 0…count-1 that fill one transaction each
func inBatches(count int, write func(from, to int) error) error {
	for from := 0; from < count; from += prepareBatch {
		if err := write(from, min(from+prepareBatch, count)); err != nil {
			return err
		}
	}
	return nil
}

// once marks an index at most once, so that a code taken twice or an event
// handled twice shows. Its ring is larger than the state a phase starts from by
// a million indices, which a phase of seconds does not outgrow
type once struct {
	slots []atomic.Int64 // an index plus one, at the index modulo the ring
}

func newOnce(keys int) *once {
	size := 1
	for size < keys+1<<20 {
		size <<= 1
	}
	return &once{slots: make([]atomic.Int64, size)}
}

// mark says whether index was not marked before
func (o *once) mark(index int64) bool {
	slot := &o.slots[index&int64(len(o.slots)-1)]
	for {
		held := slot.Load()
		if held == index+1 {
			return false
		}
		if slot.CompareAndSwap(held, index+1) {
			return true
		}
	}
}
