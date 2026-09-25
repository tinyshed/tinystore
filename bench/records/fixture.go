package main

import (
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"math/rand/v2"
	"strconv"
	"time"

	"github.com/tinyshed/tinystore/records"
)

// the research rounds' frontend fixture, seeded as spike/ seeds it, so that a
// figure measured here stands beside the one measured there
const fixtureBase = int64(1_790_000_000_000_000_000)

func jsonText(value any) string {
	encoded, err := json.Marshal(value)
	if err != nil {
		panic(err)
	}
	return string(encoded)
}

// frontendRecords are clicks of 500 sessions, each with its browser context,
// at random nanoseconds within a second of their turn
func frontendRecords(count int) []records.Record {
	random := rand.New(rand.NewPCG(17, 23)) //nolint:gosec // a seeded fixture, the research round's
	out := make([]records.Record, count)
	for i := range out {
		out[i] = frontendClick(random, i)
	}
	return out
}

func frontendClick(random *rand.Rand, index int) records.Record {
	session := random.IntN(500)
	digest := sha256.Sum256(fmt.Appendf(nil, "session:%d", session))
	context := []records.Field{
		{Key: "browser", Value: jsonText([]string{"Chrome", "Safari", "Firefox"}[session%3])},
		{Key: "browser_version", Value: jsonText(strconv.Itoa(150 + session%5))},
		{Key: "viewport_w", Value: strconv.Itoa([]int{1920, 1366, 390}[session%3])},
		{Key: "viewport_h", Value: strconv.Itoa([]int{1080, 768, 844}[session%3])},
		{Key: "session_id", Value: jsonText(fmt.Sprintf("%x-%x-%x-%x-%x",
			digest[:4], digest[4:6], digest[6:8], digest[8:10], digest[10:16]))},
		{Key: "sdk", Value: `"web/1.2.0"`},
	}
	at := fixtureBase + int64(index)*1_234_567 + int64(random.IntN(1_000_000_000))
	attrs := []records.Field{
		{Key: "element", Value: jsonText([]string{"buy", "save", "cancel", "menu"}[random.IntN(4)])},
		{Key: "url", Value: jsonText([]string{"/catalog", "/checkout", "/profile"}[random.IntN(3)])},
		{Key: "x", Value: strconv.Itoa(random.IntN(1920))},
		{Key: "y", Value: strconv.Itoa(random.IntN(1080))},
	}
	if random.IntN(17) == 0 {
		attrs = append(attrs, records.Field{Key: "experiment", Value: `"button-b"`})
	}
	return records.Record{
		At: time.Unix(0, at).UTC(), Stream: "frontend", Name: "ui.click", Context: context, Attrs: attrs,
	}
}

// withLateRecords moves one record in a hundred up to ten minutes into the
// past, as a client that was offline delivers it, seeded as the spike's round
func withLateRecords(fixture []records.Record) []records.Record {
	random := rand.New(rand.NewPCG(29, 31)) //nolint:gosec // a seeded fixture, the research round's
	for i := range fixture {
		if random.IntN(100) == 0 {
			fixture[i].At = fixture[i].At.Add(-time.Duration(random.Int64N(int64(10 * time.Minute))))
		}
	}
	return fixture
}
