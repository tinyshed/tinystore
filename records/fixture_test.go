package records

import (
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"log/slog"
	"math/rand/v2"
	"slices"
	"strconv"
	"testing"
	"time"
)

// the fixtures of the research rounds, seeded as spike/ seeds them, so that a
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
func frontendRecords(count int) []Record {
	random := rand.New(rand.NewPCG(17, 23))
	records := make([]Record, count)
	for i := range records {
		records[i] = frontendClick(random, i)
	}
	return records
}

func frontendClick(random *rand.Rand, index int) Record {
	session := random.IntN(500)
	digest := sha256.Sum256(fmt.Appendf(nil, "session:%d", session))
	context := []Field{
		{"browser", jsonText([]string{"Chrome", "Safari", "Firefox"}[session%3])},
		{"browser_version", jsonText(strconv.Itoa(150 + session%5))},
		{"viewport_w", strconv.Itoa([]int{1920, 1366, 390}[session%3])},
		{"viewport_h", strconv.Itoa([]int{1080, 768, 844}[session%3])},
		{"session_id", jsonText(fmt.Sprintf("%x-%x-%x-%x-%x",
			digest[:4], digest[4:6], digest[6:8], digest[8:10], digest[10:16]))},
		{"sdk", `"web/1.2.0"`},
	}
	at := fixtureBase + int64(index)*1_234_567 + int64(random.IntN(1_000_000_000))
	attrs := []Field{
		{"element", jsonText([]string{"buy", "save", "cancel", "menu"}[random.IntN(4)])},
		{"url", jsonText([]string{"/catalog", "/checkout", "/profile"}[random.IntN(3)])},
		{"x", strconv.Itoa(random.IntN(1920))},
		{"y", strconv.Itoa(random.IntN(1080))},
	}
	if random.IntN(17) == 0 {
		attrs = append(attrs, Field{"experiment", `"button-b"`})
	}
	return Record{
		At: time.Unix(0, at).UTC(), Stream: "frontend", Name: "ui.click", Context: context, Attrs: attrs,
	}
}

// backendRecords are requests with a random trace id each, an error in sixteen
func backendRecords(count int) []Record {
	random := rand.New(rand.NewPCG(17, 23))
	records := make([]Record, count)
	for i := range records {
		var trace TraceID
		for j := range trace {
			trace[j] = byte(random.Uint32())
		}
		level, body := slog.LevelInfo, "request finished"
		if i%16 == 0 {
			level, body = slog.LevelError, "upstream refused connection"
		}
		records[i] = Record{
			At: time.Unix(0, fixtureBase+int64(i)*1_000_003).UTC(), Stream: "backend", Name: "http.request",
			Level: &level, Body: &body, TraceID: trace,
			Context: []Field{{"service", `"api"`}, {"environment", `"production"`}, {"version", `"1.2.3"`}},
			Attrs: []Field{
				{"method", `"GET"`},
				{"path", jsonText([]string{"/notes", "/users", "/health"}[i%3])},
				{"status", strconv.Itoa([]int{200, 200, 200, 404, 500}[random.IntN(5)])},
				{"duration_ms", strconv.Itoa(random.IntN(400))},
				{"user_id", strconv.Itoa(random.IntN(5000))},
			},
		}
	}
	return records
}

// edgeRecords are what a store must not normalise away
func edgeRecords() []Record {
	empty, text, lines := "", "plain text", "first line\nsecond line\n"
	debug, extreme, low := slog.LevelDebug, slog.Level(maxLevel), slog.Level(minLevel)
	at := time.Unix(0, fixtureBase).UTC()
	return []Record{
		{At: at, Stream: "edge", Name: "values", Attrs: []Field{
			{"decimal", "1.2300"},
			{"zero", "-0"},
			{"big", "123456789012345678901234567890"},
			{"null", "null"},
			{"empty", `""`},
			{"nested", `{"a":[1,2,{"b":null}],"c":"d"}`},
			{"tag", `"x"`},
			{"tag", `"y"`},
			{"space", ` 1 `},
		}},
		{At: at, Stream: "edge", Name: "bodies", Body: &empty, Level: &debug},
		{At: at.Add(-time.Nanosecond), Stream: "edge", Name: "bodies", Body: &lines, Level: &extreme},
		{At: at, Stream: "edge", Name: "bodies", Body: &text, Level: &low, SpanID: SpanID{1, 2, 3}},
		{At: time.Unix(0, -1<<63).UTC(), Stream: "edge", Name: "ends of time"},
		{At: time.Unix(0, 1<<63-1).UTC(), Stream: "edge", Name: "ends of time", TraceID: TraceID{0xff}},
		{At: at, Stream: "edge", Name: "unicode", Attrs: []Field{{"ключ", `"значение ✓"`}, {"", `"empty key"`}}},
		{At: at, Stream: "edge", Name: "context", Context: []Field{{"host", `"a"`}, {"host", `"b"`}}},
	}
}

// sameRecords compares what a store promises to keep: every field, absent and
// empty bodies apart, and the time as an instant
func sameRecords(t testing.TB, want, got []Record) {
	t.Helper()
	if len(want) != len(got) {
		t.Fatalf("%d records, want %d", len(got), len(want))
	}
	for i := range want {
		if difference := differ(&want[i], &got[i]); difference != "" {
			t.Fatalf("record %d: %s\n got %+v\nwant %+v", i, difference, got[i], want[i])
		}
	}
}

func differ(want, got *Record) string {
	switch {
	case !want.At.Equal(got.At):
		return "time"
	case want.Stream != got.Stream || want.Name != got.Name:
		return "stream or name"
	case (want.Level == nil) != (got.Level == nil) || (want.Level != nil && *want.Level != *got.Level):
		return "level"
	case (want.Body == nil) != (got.Body == nil) || (want.Body != nil && *want.Body != *got.Body):
		return "body"
	case want.TraceID != got.TraceID || want.SpanID != got.SpanID:
		return "trace or span"
	case !slices.Equal(want.Context, got.Context):
		return "context"
	case !slices.Equal(want.Attrs, got.Attrs):
		return "attributes"
	}
	return ""
}
