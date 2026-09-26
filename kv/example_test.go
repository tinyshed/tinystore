package kv_test

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"os"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/kv"
)

// clock is the store's clock in the examples, moved rather than waited for
type clock struct{ now time.Time }

func (c *clock) Now() time.Time { return c.now }

func (c *clock) advance(d time.Duration) { c.now = c.now.Add(d) }

// exampleState opens kv in a Manual store in a new directory, so that an
// example moves its clock instead of sleeping; done closes and removes it
func exampleState() (state *kv.Store, at *clock, done func()) {
	ctx := context.Background()
	directory, err := os.MkdirTemp("", "tinystore-kv-")
	check(err)
	at = &clock{now: time.Date(2026, 9, 26, 12, 0, 0, 0, time.UTC)}
	store, err := tinystore.Open(ctx, directory, tinystore.Options{Manual: true, Clock: at.Now})
	check(err)
	state, err = kv.Open(ctx, store, kv.Options{})
	check(err)
	return state, at, func() {
		_ = store.Close(ctx)
		_ = os.RemoveAll(directory)
	}
}

// check ends an example that meets an error
func check(err error) {
	if err != nil {
		panic(err)
	}
}

type Session struct {
	Device string
}

// Sessions live thirty days from their last request, a user sees where they
// are signed in, and signs out everywhere at once.
func Example_sessions() {
	ctx := context.Background()
	state, clock, done := exampleState()
	defer done()
	sessions, err := kv.OpenBucket[Session](ctx, state, "sessions", kv.Sliding(30*24*time.Hour))
	check(err)

	check(sessions.Of(42).Set(ctx, "t1", Session{Device: "phone"}))
	check(sessions.Of(42).Set(ctx, "t2", Session{Device: "laptop"}))

	clock.advance(20 * 24 * time.Hour)
	s, found, err := sessions.Of(42).Get(ctx, "t1") // and thirty more days from now
	check(err)
	fmt.Println(s.Device, found)

	for entry, err := range sessions.Of(42).All(ctx) {
		check(err)
		fmt.Println("signed in:", entry.Key, entry.Value.Device)
	}

	check(sessions.Of(42).Clear(ctx))
	_, found, err = sessions.Of(42).Get(ctx, "t2")
	check(err)
	fmt.Println(found)
	// Output:
	// phone true
	// signed in: t1 phone
	// signed in: t2 laptop
	// false
}

// A sign-in link works once, for fifteen minutes.
func Example_signInLink() {
	ctx := context.Background()
	state, clock, done := exampleState()
	defer done()
	codes, err := kv.OpenBucket[int64](ctx, state, "login-codes", kv.DefaultTTL(15*time.Minute))
	check(err)

	check(codes.Set(ctx, digest("7Q2X"), 42)) // a digest, so a copy of the file holds no working link
	userID, found, err := codes.Take(ctx, digest("7Q2X"))
	check(err)
	fmt.Println(userID, found)
	_, found, err = codes.Take(ctx, digest("7Q2X")) // the second click
	check(err)
	fmt.Println(found)

	check(codes.Set(ctx, digest("K9ZP"), 43))
	clock.advance(16 * time.Minute)
	_, found, err = codes.Take(ctx, digest("K9ZP")) // too late
	check(err)
	fmt.Println(found)
	// Output:
	// 42 true
	// false
	// false
}

func digest(code string) string {
	sum := sha256.Sum256([]byte(code))
	return hex.EncodeToString(sum[:])
}

// Attempts are counted in memory, fifteen minutes from the first; a crash
// may forget the last second of them, which a limit can afford.
func Example_attemptLimits() {
	ctx := context.Background()
	state, clock, done := exampleState()
	defer done()
	attempts, err := kv.OpenCounters(ctx, state, "login-attempts",
		kv.DefaultTTL(15*time.Minute), kv.LoseAtMost(time.Second))
	check(err)

	refused := func(ip string) bool {
		n, addErr := attempts.Of("ip").Add(ctx, ip, 1)
		check(addErr)
		return n > 3
	}
	var answers []bool
	for range 4 {
		answers = append(answers, refused("10.0.0.1"))
	}
	fmt.Println(answers)
	clock.advance(15 * time.Minute)
	fmt.Println(refused("10.0.0.1"))
	// Output:
	// [false false false true]
	// false
}

// A provider delivers an event until it gets a 200, so an event may arrive
// twice; a claim's version keeps a handler slower than its claim from
// finishing the next one's.
func Example_webhookClaims() {
	ctx := context.Background()
	state, clock, done := exampleState()
	defer done()
	seen, err := kv.OpenBucket[struct{}](ctx, state, "stripe-events")
	check(err)

	handle := func(eventID string) string {
		claim, first, claimErr := seen.SetEntryIfAbsent(ctx, eventID, struct{}{}, kv.TTL(10*time.Minute))
		check(claimErr)
		if !first {
			return "handled before, or being handled now"
		}
		check(seen.Set(ctx, eventID, struct{}{}, kv.IfVersion(claim.Version), kv.TTL(7*24*time.Hour)))
		return "handled"
	}
	fmt.Println(handle("evt_1"))
	fmt.Println(handle("evt_1"))

	slow, _, err := seen.SetEntryIfAbsent(ctx, "evt_2", struct{}{}, kv.TTL(10*time.Minute))
	check(err)
	clock.advance(11 * time.Minute)
	fmt.Println(handle("evt_2"))
	err = seen.Set(ctx, "evt_2", struct{}{}, kv.IfVersion(slow.Version))
	fmt.Println(errors.Is(err, tinystore.ErrConflict))
	// Output:
	// handled
	// handled before, or being handled now
	// handled
	// true
}

// Two tabs edit one draft: a save goes through only if nobody saved since the
// tab read it.
func Example_drafts() {
	ctx := context.Background()
	state, _, done := exampleState()
	defer done()
	drafts, err := kv.OpenBucket[string](ctx, state, "drafts")
	check(err)
	check(drafts.Of(42).Set(ctx, "note-1", "milk"))

	first, _, err := drafts.Of(42).GetEntry(ctx, "note-1")
	check(err)
	second, _, err := drafts.Of(42).GetEntry(ctx, "note-1")
	check(err)

	_, err = drafts.Of(42).SetEntry(ctx, "note-1", "milk, bread", kv.IfVersion(first.Version))
	fmt.Println(err)
	_, err = drafts.Of(42).SetEntry(ctx, "note-1", "milk, eggs", kv.IfVersion(second.Version))
	fmt.Println(errors.Is(err, tinystore.ErrConflict))
	// Output:
	// <nil>
	// true
}
