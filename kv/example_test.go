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

func check(err error) {
	if err != nil {
		panic(err)
	}
}

type Session struct {
	Device string
}

// Sessions live thirty days from their last request and are kept by a digest
// of their token, so that a copy of kv.db signs nobody in. A token is rotated
// in one write, a user sees where they are signed in, and signs out
// everywhere at once.
func Example_sessions() {
	ctx := context.Background()
	state, clock, done := exampleState()
	defer done()
	sessions, err := kv.OpenBucket[Session](ctx, state, "sessions", kv.Sliding(30*24*time.Hour))
	check(err)

	check(sessions.Of(42).Set(ctx, digest("t1"), Session{Device: "phone"}))
	check(sessions.Of(42).Set(ctx, digest("t2"), Session{Device: "laptop"}))

	clock.advance(20 * 24 * time.Hour)
	s, found, err := sessions.Of(42).Get(ctx, digest("t1")) // and thirty more days from now
	check(err)
	fmt.Println(s.Device, found)

	// after a password change: a new token in the old one's place, in one write
	check(state.Tx(ctx, func(tx *kv.Tx) error {
		session, there, takeErr := sessions.WithTx(tx).Of(42).Take(ctx, digest("t1"))
		if takeErr != nil || !there {
			return takeErr
		}
		return sessions.WithTx(tx).Of(42).Set(ctx, digest("t3"), session)
	}))
	_, found, err = sessions.Of(42).Get(ctx, digest("t1"))
	check(err)
	fmt.Println("the old token signs in:", found)

	for entry, err := range sessions.Of(42).All(ctx) {
		check(err)
		fmt.Println("signed in:", entry.Value.Device)
	}

	check(sessions.Of(42).Clear(ctx))
	_, found, err = sessions.Of(42).Get(ctx, digest("t3"))
	check(err)
	fmt.Println(found)
	// Unordered output:
	// phone true
	// the old token signs in: false
	// signed in: phone
	// signed in: laptop
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

// Sign-in attempts reserve their quota before the password is checked.
// A correct password clears the login's attempts; the address keeps its own.
func Example_signInAttempts() {
	ctx := context.Background()
	state, clock, done := exampleState()
	defer done()
	attempts, err := kv.OpenQuota(ctx, state, "signin-attempts",
		kv.Window("burst", 5, 15*time.Minute), kv.Window("day", 50, 24*time.Hour))
	check(err)
	byLogin, byAddress := attempts.Of("login"), attempts.Of("address")

	signIn := func(login, address string, correct bool) string {
		for _, refused := range []struct {
			quota *kv.Quota
			key   string
		}{{byLogin, login}, {byAddress, address}} {
			usage, allowErr := refused.quota.Allow(ctx, refused.key)
			check(allowErr)
			if !usage.OK {
				return fmt.Sprintf("429, retry after %s", usage.RetryAfter)
			}
		}
		if !correct { // the password was hashed and did not match
			return "401"
		}
		check(byLogin.Delete(ctx, login))
		return "200"
	}
	for range 5 {
		signIn("ann", "10.0.0.1", false)
	}
	fmt.Println(signIn("ann", "10.0.0.1", true))
	fmt.Println(signIn("ann", "10.0.0.2", true))
	clock.advance(15 * time.Minute)
	fmt.Println(signIn("ann", "10.0.0.1", true))
	fmt.Println(signIn("ann", "10.0.0.1", false))
	// Output:
	// 429, retry after 15m0s
	// 429, retry after 15m0s
	// 200
	// 401
}

// A provider delivers an event until it gets a 200, so an event may arrive
// twice. A claim's version keeps a handler slower than its claim from finishing
// the next one's.
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
