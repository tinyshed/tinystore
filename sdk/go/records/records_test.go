package records_test

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"reflect"
	"slices"
	"strings"
	"sync"
	"testing"
	"testing/slogtest"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/console"
	"tinyshed.org/tinystore/internal/ways"
	"tinyshed.org/tinystore/records"
)

const (
	traceID = "4bf92f3577b34da6a3ce929d0e0e4736"
	spanID  = "00f067aa0ba902b7"
)

func is(t *testing.T, err error, kind error) {
	t.Helper()
	if !errors.Is(err, kind) {
		t.Fatalf("got %v, want %v", err, kind)
	}
}

// fields reads an event's fields or context into a map, its numbers exact.
func fields(t *testing.T, value any) map[string]any {
	t.Helper()
	raw, ok := value.(json.RawMessage)
	if !ok {
		t.Fatalf("a read gives an object's JSON, not %T", value)
	}
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.UseNumber()
	var out map[string]any
	if err := decoder.Decode(&out); err != nil {
		t.Fatal(err)
	}
	return out
}

func TestAnAppendedEventComesBackWhole(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		audit := ways.Must(records.OpenEvents(store, "audit"))
		changedAt := time.Now().Add(-time.Minute).Truncate(time.Millisecond).UTC()
		ways.Must(audit.AppendMany(ctx, []records.Event{{
			Name: "role.changed", At: changedAt, Level: slog.LevelWarn, Message: "role changed",
			Fields:  records.Fields{"user": 42, "id": int64(9007199254740993), "tags": []string{"a", "b"}},
			Context: records.Fields{"actor": 7},
		}}))
		traced := records.WithTrace(ctx, traceID, spanID)
		ways.Must(audit.Append(traced, "signed.in", records.Fields{"user": 42}))
		page := ways.Must(audit.List(ctx, records.Query{}))
		if page.Next != "" || len(page.Events) != 2 {
			t.Fatalf("page %+v", page)
		}
		changed, signed := page.Events[0], page.Events[1]
		if changed.Name != "role.changed" || !changed.At.Equal(changedAt) || changed.Stream != "audit" {
			t.Fatalf("changed %+v", changed)
		}
		if got := string(changed.Fields.(json.RawMessage)); got != `{"id":9007199254740993,"tags":["a","b"],"user":42}` {
			t.Fatalf("fields %s", got)
		}
		if got := fields(t, changed.Context); got["actor"] != json.Number("7") {
			t.Fatalf("context %v", got)
		}
		if changed.Level.Level() != slog.LevelWarn || changed.Message != "role changed" {
			t.Fatalf("level %v message %q", changed.Level, changed.Message)
		}
		if signed.TraceID != traceID || signed.SpanID != spanID || signed.Level != nil {
			t.Fatalf("signed %+v", signed)
		}
	})
}

func TestAnEventWhoseIDItsStreamHoldsIsWrittenAsNothingAndItsIDReadBack(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		deploys := ways.Must(records.OpenEvents(store, "deploys"))
		delivery := "72d3162e-cc78-11e3-81ab-4c9367dc0958"
		if !ways.Must(deploys.Append(ctx, "push", records.Fields{"sha": "26a0865"}, records.ID(delivery))) {
			t.Fatal("the first push was not written")
		}
		if ways.Must(deploys.Append(ctx, "push", records.Fields{"sha": "f00ba47"}, records.ID(delivery))) {
			t.Fatal("a push of an id the stream holds was written")
		}
		ways.Must(deploys.Append(ctx, "tick", nil))
		events := []records.Event{{Name: "push", ID: delivery}, {Name: "tag", ID: "v0.2.0"}, {Name: "tag", ID: "v0.2.0"}, {Name: "tick"}}
		if written := ways.Must(deploys.AppendMany(ctx, events)); written != 2 {
			t.Fatalf("%d of the list written, not 2", written)
		}
		page := ways.Must(deploys.List(ctx, records.Query{}))
		var got []string
		for _, event := range page.Events {
			got = append(got, event.Name+"="+event.ID)
		}
		if want := []string{"push=" + delivery, "tick=", "tag=v0.2.0", "tick="}; !slices.Equal(got, want) {
			t.Fatalf("read %v, not %v", got, want)
		}
		_, err := deploys.Append(ctx, "push", nil, records.ID(""))
		is(t, err, tinystore.ErrInvalid)
	})
}

func TestAListIsAppendedWholeOrNotAtAllAndARefusedEventNamesItsPlace(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		clicks := ways.Must(records.OpenEvents(store, "clicks"))
		_, err := clicks.AppendMany(ctx, []records.Event{{Name: "view", Fields: records.Fields{"page": 1}}, {}})
		is(t, err, tinystore.ErrInvalid)
		if !strings.Contains(err.Error(), "event 1") {
			t.Fatalf("the refusal %v names no place", err)
		}
		_, err = clicks.AppendMany(ctx, []records.Event{{Name: "view"}, {Name: "view", At: time.Now().Add(time.Hour)}})
		is(t, err, tinystore.ErrTooNew)
		_, err = clicks.Append(ctx, "view", []int{1})
		is(t, err, tinystore.ErrInvalid)
		if count := ways.Must(clicks.Count(ctx, records.Query{})); count != 0 {
			t.Fatalf("%d events of a refused list", count)
		}
	})
}

func TestAQueryFindsByWhatEventsHoldAPageAtATime(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		api := ways.Must(records.OpenEvents(store, "api"))
		// a millisecond apart: a page never splits a time
		base := time.Now().Add(-time.Minute)
		var events []records.Event
		for n := range 30 {
			route, level := "/notes", slog.LevelInfo
			if n%3 == 0 {
				route = "/login"
			}
			if n%10 == 0 {
				level = slog.LevelError
			}
			events = append(events, records.Event{
				Name: "request", At: base.Add(time.Duration(n) * time.Millisecond), Level: level,
				Fields: records.Fields{"route": route, "ms": n},
			})
		}
		ways.Must(api.AppendMany(ctx, events))
		ms := func(events []records.Event) []string {
			var out []string
			for _, event := range events {
				out = append(out, fields(t, event.Fields)["ms"].(json.Number).String())
			}
			return out
		}
		if found := ways.Must(api.List(ctx, records.Query{Where: records.Where{"route": "/login"}})); len(found.Events) != 10 {
			t.Fatalf("%d events of /login", len(found.Events))
		}
		if got := ms(ways.Must(api.List(ctx, records.Query{MinLevel: slog.LevelError})).Events); fmt.Sprint(got) != "[0 10 20]" {
			t.Fatalf("errors %v", got)
		}
		if got := ms(ways.Must(api.List(ctx, records.Query{Search: "LOGIN", Newest: true, Limit: 2})).Events); fmt.Sprint(got) != "[27 24]" {
			t.Fatalf("search %v", got)
		}
		if first := ways.Must(api.List(ctx, records.Query{Limit: 12})); len(first.Events) != 12 || first.Next == "" {
			t.Fatalf("first page %d events, next %q", len(first.Events), first.Next)
		}
		var all []string
		for event, err := range api.All(ctx, records.Query{Limit: 7}) {
			if err != nil {
				t.Fatal(err)
			}
			all = append(all, ms([]records.Event{event})...)
		}
		if len(all) != 30 || all[0] != "0" || all[29] != "29" {
			t.Fatalf("all %v", all)
		}
		every := ways.Must(records.OpenAllEvents(store))
		if found := ways.Must(every.List(ctx, records.Query{Streams: []string{"api"}, Names: []string{"request"}})); len(found.Events) != 30 {
			t.Fatalf("%d of every stream's", len(found.Events))
		}
		_, err := api.List(ctx, records.Query{TraceID: "not hex"})
		is(t, err, tinystore.ErrInvalid)
		_, err = api.Count(ctx, records.Query{Where: records.Where{"user": nil}})
		is(t, err, tinystore.ErrInvalid)
	})
}

func TestCountsAKeysTopValuesAndRowsOfASpanWithTheirStats(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		checkout := ways.Must(records.OpenEvents(store, "checkout"))
		var paid []records.Event
		for _, ms := range []int{10, 20, 30, 40} {
			method := "cash"
			if ms < 25 {
				method = "card"
			}
			paid = append(paid, records.Event{Name: "paid", Fields: records.Fields{"method": method, "ms": ms}})
		}
		ways.Must(checkout.AppendMany(ctx, paid))
		if count := ways.Must(checkout.Count(ctx, records.Query{Names: []string{"paid"}})); count != 4 {
			t.Fatalf("count %d", count)
		}
		if top := ways.Must(checkout.Top(ctx, "method", records.Query{})); !reflect.DeepEqual(top, []records.Top{{"card", 2}, {"cash", 2}}) {
			t.Fatalf("top %v", top)
		}
		rows := ways.Must(checkout.Aggregate(ctx, records.Query{By: []string{"method"}, Stats: []string{"ms"}, Distinct: []string{"ms"}}))
		want := []struct {
			method        string
			sum, min, max json.Number
			avg           float64
		}{{"card", "30", "10", "20", 15}, {"cash", "70", "30", "40", 35}}
		if len(rows) != 2 {
			t.Fatalf("rows %+v", rows)
		}
		for i, row := range rows {
			stats := row.Stats["ms"]
			if row.By["method"] != want[i].method || row.Count != 2 || stats.Samples != 2 || stats.Sum != want[i].sum ||
				stats.Min != want[i].min || stats.Max != want[i].max || stats.Avg != want[i].avg || row.Distinct["ms"] != 2 {
				t.Fatalf("row %d %+v", i, row)
			}
		}
	})
}

func TestAStoreSaysItsStreamsWhatEachKeepsAndTheKeysEventsHold(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		kept := ways.Must(records.OpenEvents(store, "kept", records.Keep(30*24*time.Hour)))
		ways.Must(kept.Append(ctx, "signup", records.Fields{"plan": "pro", "user": 1}))
		every := ways.Must(records.OpenAllEvents(store))
		var found []records.Stream
		for _, stream := range ways.Must(every.Streams(ctx)) {
			if stream.Name == "kept" {
				found = append(found, stream)
			}
		}
		if len(found) != 1 || found[0].Keep != 30*24*time.Hour || found[0].RetainedFrom.After(found[0].Newest) {
			t.Fatalf("streams %+v", found)
		}
		if names := ways.Must(kept.FieldNames(ctx, records.Query{Prefix: "p"})); !reflect.DeepEqual(names, []string{"plan"}) {
			t.Fatalf("names %v", names)
		}
	})
}

func TestAPollGivesACursorAndNothingNotPackedYet(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		every := ways.Must(records.OpenAllEvents(store))
		batch := ways.Must(every.Poll(ctx, ""))
		if len(batch.Events) != 0 || batch.Cursor == "" {
			t.Fatalf("batch %+v", batch)
		}
		again := ways.Must(every.Poll(ctx, batch.Cursor))
		if again.Cursor != batch.Cursor {
			t.Fatalf("a poll with nothing past its cursor moved it to %q", again.Cursor)
		}
	})
}

func TestATailGivesTheEventsWrittenAfterItABatchAsTheyCome(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		live := ways.Must(records.OpenEvents(store, "live"))
		ticking, stop := context.WithCancel(ctx)
		defer stop()
		go func() {
			for n := 0; ticking.Err() == nil; n++ {
				_, _ = live.Append(ticking, "tick", records.Fields{"n": n})
				time.Sleep(20 * time.Millisecond)
			}
		}()
		for batch, err := range live.Tail(ctx, records.Query{Names: []string{"tick"}}) {
			if err != nil {
				t.Fatal(err)
			}
			if batch.Missed != 0 || len(batch.Events) == 0 || batch.Events[0].Stream != "live" || batch.Events[0].Name != "tick" {
				t.Fatalf("batch %+v", batch)
			}
			break
		}
	})
}

func TestATailEndsWhenItsStoreCloses(t *testing.T) {
	ways.EachWay(t, func(t *testing.T, way ways.Way) {
		store := way.Open(t, t.TempDir())
		every := ways.Must(records.OpenAllEvents(store))
		ended := make(chan error, 1)
		go func() {
			var failed error
			for _, err := range every.Tail(context.Background(), records.Query{}) {
				failed = err
			}
			ended <- failed
		}()
		time.Sleep(100 * time.Millisecond)
		ways.Must(0, store.Close())
		select {
		case err := <-ended:
			if err != nil {
				t.Fatalf("a tail ended by its store's close says %v", err)
			}
		case <-time.After(10 * time.Second):
			t.Fatal("a tail outlived its store")
		}
	})
}

func TestATailOpenedAsItsStoreClosesEndsQuietly(t *testing.T) {
	ways.EachWay(t, func(t *testing.T, way ways.Way) {
		store := way.Open(t, t.TempDir())
		every := ways.Must(records.OpenAllEvents(store))
		ended := make(chan error, 1)
		go func() {
			var failed error
			for _, err := range every.Tail(context.Background(), records.Query{}) {
				failed = err
			}
			ended <- failed
		}()
		// no wait: the close may come before the tail is open on the server
		ways.Must(0, store.Close())
		select {
		case err := <-ended:
			if err != nil {
				t.Fatalf("a tail opened as its store closed says %v", err)
			}
		case <-time.After(10 * time.Second):
			t.Fatal("a tail outlived its store")
		}
	})
}

func TestAnotherProgramsOutputBecomesEventsHoweverItWasCut(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		child := ways.Must(records.OpenEvents(store, "child"))
		out := ways.Must(child.Lines(ctx))
		ways.Must(io.WriteString(out, `level=warn msg="slow request" ms=12`))
		ways.Must(io.WriteString(out, "00\n{\"level\":30,\"msg\":\"ok\"}\nthe last line"))
		ways.Must(0, out.Close())
		if out.Dropped() != 0 {
			t.Fatalf("%d lines dropped", out.Dropped())
		}
		events := ways.Must(child.List(ctx, records.Query{})).Events
		var got []string
		for _, event := range events {
			level := "none"
			if event.Level != nil {
				level = event.Level.Level().String()
			}
			got = append(got, event.Name+" "+level)
		}
		if strings.Join(got, ", ") != "logfmt WARN, json INFO, log none" || events[2].Message != "the last line" {
			t.Fatalf("events %v, the last %q", got, events[len(events)-1].Message)
		}
		_, err := io.WriteString(out, "late\n")
		is(t, err, tinystore.ErrClosed)
	})
}

func TestNothingDamagedIsListedAndWhatStillReadsIsNeverDropped(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		audit := ways.Must(records.OpenEvents(store, "audit"))
		ways.Must(audit.Append(ctx, "signed.in", records.Fields{"user": 42}))
		every := ways.Must(records.OpenAllEvents(store))
		if damaged := ways.Must(every.Damaged(ctx)); len(damaged) != 0 {
			t.Fatalf("damaged %+v", damaged)
		}
		now := time.Now()
		is(t, every.Drop(ctx, records.Damage{Part: "head row", ID: 1, Stream: "audit", First: now, Last: now}), tinystore.ErrInvalid)
	})
}

func TestAStoresHandlerNeverWaitsAFullBufferDropsAndCounts(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		handler := records.NewHandler(store, "busy", console.Off, records.Buffer(2))
		log := slog.New(handler)
		for n := range 6 {
			log.Info("a line", "n", n)
		}
		if handler.Dropped() == 0 {
			t.Fatal("a buffer of two held six lines")
		}
		ways.Must(0, handler.Flush(ctx))
		busy := ways.Must(records.OpenEvents(store, "busy"))
		if kept := ways.Must(busy.Count(ctx, records.Query{})); kept+handler.Dropped() != 6 {
			t.Fatalf("%d kept and %d dropped of six", kept, handler.Dropped())
		}
	})
}

func TestAStoresHandlerKeepsABufferOfMoreTextThanOneRequestHolds(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		handler := records.NewHandler(store, "stacks", console.Off, records.Buffer(64))
		log := slog.New(handler)
		stack := strings.Repeat("x", 100<<10)
		for range 24 {
			log.Error("failed", "stack", stack)
		}
		ways.Must(0, handler.Flush(ctx))
		stacks := ways.Must(records.OpenEvents(store, "stacks"))
		if kept := ways.Must(stacks.Count(ctx, records.Query{})); kept != 24 || handler.Dropped() != 0 {
			t.Fatalf("%d kept and %d dropped of 24 lines of 100 KiB", kept, handler.Dropped())
		}
	})
}

func TestAStoresHandlerKeepsItsLinesTheirFieldsAndTheContextOfWith(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		handler := records.NewHandler(store, "worker", console.Off)
		log := slog.New(handler)
		log.Info("started", "port", 3000)
		log.With("job", 7).WithGroup("req").WarnContext(records.WithTrace(ctx, traceID, spanID), "slow", "ms", 1200)
		handler.Event("signup", "plan", "pro")
		ways.Must(0, handler.Flush(ctx))
		events := ways.Must(ways.Must(records.OpenEvents(store, "worker")).List(ctx, records.Query{})).Events
		var got []string
		for _, event := range events {
			level := "none"
			if event.Level != nil {
				level = event.Level.Level().String()
			}
			got = append(got, fmt.Sprintf("%s %s %q", event.Name, level, event.Message))
		}
		if strings.Join(got, ", ") != `log INFO "started", log WARN "slow", signup none ""` {
			t.Fatalf("events %v", got)
		}
		slow := events[1]
		if string(slow.Fields.(json.RawMessage)) != `{"req.ms":1200}` || string(slow.Context.(json.RawMessage)) != `{"job":7}` {
			t.Fatalf("fields %s context %s", slow.Fields, slow.Context)
		}
		if slow.TraceID != traceID || slow.SpanID != spanID {
			t.Fatalf("trace %q span %q", slow.TraceID, slow.SpanID)
		}
		if handler.Dropped() != 0 {
			t.Fatalf("%d lines dropped", handler.Dropped())
		}
	})
}

func TestAStoresHandlerHandsOverItsLinesAsTheStoreCloses(t *testing.T) {
	ways.EachWay(t, func(t *testing.T, way ways.Way) {
		if !way.Shared {
			t.Skip("a second open of the directory starts another store")
		}
		dir := t.TempDir()
		store := way.Open(t, dir)
		slog.New(records.NewHandler(store, "closing", console.Off)).Info("the last line")
		ways.Must(0, store.Close())
		again := way.Open(t, dir)
		t.Cleanup(func() { _ = again.Close() })
		closing := ways.Must(records.OpenEvents(again, "closing"))
		if count := ways.Must(closing.Count(context.Background(), records.Query{})); count != 1 {
			t.Fatalf("%d lines kept of one", count)
		}
	})
}

// a store's handler keeps every promise testing/slogtest checks of a
// slog.Handler, its console's lines read as slog's handlers write them
func TestAStoresHandlerKeepsSlogsContract(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		var out bytes.Buffer
		slogtest.Run(t, func(*testing.T) slog.Handler {
			out.Reset()
			return records.NewHandler(store, "contract", console.JSON, console.NoEnv, console.To(&out))
		}, func(t *testing.T) map[string]any {
			return grouped(t, out.Bytes())
		})
	})
}

// grouped is a JSON line as slogtest reads one: a group's keys, written
// group.key, nested under the group, and the stream left out
func grouped(t *testing.T, line []byte) map[string]any {
	t.Helper()
	var flat map[string]any
	if err := json.Unmarshal(line, &flat); err != nil {
		t.Fatalf("%q: %v", line, err)
	}
	delete(flat, "stream")
	out := map[string]any{}
	for key, value := range flat {
		at := out
		path := strings.Split(key, ".")
		for _, group := range path[:len(path)-1] {
			inner, ok := at[group].(map[string]any)
			if !ok {
				inner = map[string]any{}
				at[group] = inner
			}
			at = inner
		}
		at[path[len(path)-1]] = value
	}
	return out
}

func TestAStoresHandlerTakesItsLevelFromTheConfigLogWhichAChangeSetsAndResetGivesBack(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		t.Setenv("APP_LOG_LEVEL", "warning")
		var written bytes.Buffer
		var mu sync.Mutex
		log := slog.New(records.NewHandler(store, "logged", console.Level(slog.LevelDebug), console.JSON,
			console.To(lockedWriter{&written, &mu}), console.FromEnv("APP")))
		printed := func(message string) bool {
			mu.Lock()
			defer mu.Unlock()
			return strings.Contains(written.String(), `"msg":"`+message+`"`)
		}
		probes := 0
		probed := func() bool {
			probes++
			log.Debug(fmt.Sprintf("probe %d", probes))
			return printed(fmt.Sprintf("probe %d", probes))
		}
		if probed() {
			t.Fatal("APP_LOG_LEVEL=warning let a debug line through")
		}
		type Log struct {
			Level string `json:"level" config:"live"`
		}
		settings := ways.Must(tinystore.OpenConfig(ctx, store, "log", Log{Level: "info"}))
		ways.Must(0, settings.Update(ctx, func(l *Log) { l.Level = "debug" }))
		ways.Until(t, probed)
		err := settings.Update(ctx, func(l *Log) { l.Level = "verbose" })
		if err == nil || !strings.Contains(err.Error(), `"verbose" is not one of debug, trace, info, warn, error`) {
			t.Fatalf("a level no handler means: %v", err)
		}
		ways.Must(0, settings.Reset(ctx, "level"))
		ways.Until(t, func() bool { return !probed() })
		log.Warn("warned")
		if !printed("warned") {
			t.Fatal("a warning was not printed")
		}
	})
}

// lockedWriter is a writer the test reads while the handler writes it.
type lockedWriter struct {
	out *bytes.Buffer
	mu  *sync.Mutex
}

func (w lockedWriter) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	return w.out.Write(p)
}

// printedMessages is the messages a JSON console printed, an event's name in place of one.
func printedMessages(t *testing.T, printed []byte) []string {
	t.Helper()
	var messages []string
	for _, line := range strings.Split(strings.TrimSuffix(string(printed), "\n"), "\n") {
		if line == "" {
			continue
		}
		var said struct{ Msg, Event string }
		if err := json.Unmarshal([]byte(line), &said); err != nil {
			t.Fatalf("%q: %v", line, err)
		}
		messages = append(messages, said.Msg+said.Event)
	}
	return messages
}

// keptMessages is the messages a stream kept, an event's name in place of one.
func keptMessages(t *testing.T, store *tinystore.Store, stream string) []string {
	t.Helper()
	events := ways.Must(ways.Must(records.OpenEvents(store, stream)).List(context.Background(), records.Query{})).Events
	var messages []string
	for _, event := range events {
		if event.Name == "log" {
			messages = append(messages, event.Message)
		} else {
			messages = append(messages, event.Name)
		}
	}
	return messages
}

func fourLevels(log *slog.Logger) {
	log.Debug("debug")
	log.Info("info")
	log.Warn("warn")
	log.Error("error")
}

// contract: LOG-08
func TestOneLevelSetsBothWhatAStoresHandlerKeepsAndWhatItPrints(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		var out bytes.Buffer
		handler := records.NewHandler(store, "both", console.Level(slog.LevelWarn), console.JSON, console.NoEnv,
			console.To(&out))
		fourLevels(slog.New(handler))
		ways.Must(0, handler.Flush(context.Background()))
		if got := printedMessages(t, out.Bytes()); !reflect.DeepEqual(got, []string{"warn", "error"}) {
			t.Fatalf("printed %q", got)
		}
		if got := keptMessages(t, store, "both"); !reflect.DeepEqual(got, []string{"warn", "error"}) {
			t.Fatalf("kept %q", got)
		}
	})
}

// contract: LOG-08
func TestAConsolePartAloneKeepsEveryLevelAndPrintsFromItsOwn(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		var out bytes.Buffer
		handler := records.NewHandler(store, "everything", records.Levels{Console: slog.LevelWarn}, console.JSON,
			console.NoEnv, console.To(&out))
		log := slog.New(handler)
		log.Log(context.Background(), slog.Level(-8), "trace")
		fourLevels(log)
		ways.Must(0, handler.Flush(context.Background()))
		if got := printedMessages(t, out.Bytes()); !reflect.DeepEqual(got, []string{"warn", "error"}) {
			t.Fatalf("printed %q", got)
		}
		every := []string{"trace", "debug", "info", "warn", "error"}
		if got := keptMessages(t, store, "everything"); !reflect.DeepEqual(got, every) {
			t.Fatalf("kept %q", got)
		}
	})
}

// contract: LOG-08
func TestAStoresHandlerWithAConsolePartPrintsLessThanItKeeps(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		var out bytes.Buffer
		handler := records.NewHandler(store, "quiet", records.Levels{Store: slog.LevelInfo, Console: slog.LevelWarn},
			console.JSON, console.NoEnv, console.To(&out))
		fourLevels(slog.New(handler))
		handler.Event("signup", "plan", "pro")
		ways.Must(0, handler.Flush(context.Background()))
		if got := printedMessages(t, out.Bytes()); !reflect.DeepEqual(got, []string{"warn", "error", "signup"}) {
			t.Fatalf("printed %q", got)
		}
		kept := []string{"info", "warn", "error", "signup"}
		if got := keptMessages(t, store, "quiet"); !reflect.DeepEqual(got, kept) {
			t.Fatalf("kept %q", got)
		}
	})
}

// contract: LOG-08
func TestAStoresHandlerMayPrintBelowWhatItKeepsAndIsEnabledWhereEitherTakesALine(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		var out bytes.Buffer
		handler := records.NewHandler(store, "louder", records.Levels{Store: slog.LevelInfo, Console: slog.LevelDebug},
			console.JSON, console.NoEnv, console.To(&out))
		fourLevels(slog.New(handler))
		ways.Must(0, handler.Flush(context.Background()))
		if got := printedMessages(t, out.Bytes()); !reflect.DeepEqual(got, []string{"debug", "info", "warn", "error"}) {
			t.Fatalf("printed %q", got)
		}
		if got := keptMessages(t, store, "louder"); !reflect.DeepEqual(got, []string{"info", "warn", "error"}) {
			t.Fatalf("kept %q", got)
		}
		off := records.NewHandler(store, "louder", records.Levels{Store: slog.LevelWarn, Console: slog.LevelDebug},
			console.Off, console.NoEnv)
		if off.Enabled(context.Background(), slog.LevelDebug) || !off.Enabled(context.Background(), slog.LevelWarn) {
			t.Fatal("a console turned off took part in what the handler takes")
		}
	})
}

// contract: LOG-05
func TestEachVariableWinsOverItsOwnPartAndLeavesTheOtherAsTheCodeGaveIt(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		said := map[string]string{"LOG_CONSOLE_LEVEL": "error", "LOG_LEVEL": "debug"}
		lookup := func(name string) (string, bool) { value, ok := said[name]; return value, ok }
		var out bytes.Buffer
		handler := records.NewHandler(store, "told", records.Levels{Store: slog.LevelWarn, Console: slog.LevelInfo},
			console.JSON, console.FromLookup("", lookup), console.To(&out))
		fourLevels(slog.New(handler))
		ways.Must(0, handler.Flush(context.Background()))
		if got := printedMessages(t, out.Bytes()); !reflect.DeepEqual(got, []string{"error"}) {
			t.Fatalf("LOG_CONSOLE_LEVEL over the console part: printed %q", got)
		}
		if got := keptMessages(t, store, "told"); !reflect.DeepEqual(got, []string{"debug", "info", "warn", "error"}) {
			t.Fatalf("LOG_LEVEL over the store part: kept %q", got)
		}

		delete(said, "LOG_CONSOLE_LEVEL")
		out.Reset()
		handler = records.NewHandler(store, "told", records.Levels{Console: slog.LevelWarn}, console.JSON,
			console.FromLookup("", lookup), console.To(&out))
		fourLevels(slog.New(handler))
		if got := printedMessages(t, out.Bytes()); !reflect.DeepEqual(got, []string{"warn", "error"}) {
			t.Fatalf("LOG_LEVEL moved the console part the code gave: printed %q", got)
		}
	})
}

// contract: LOG-07
func TestAChangeOfTheConfigLogMovesWhatIsKeptAndLeavesAConsolePartAsTheCodeGaveIt(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		var written bytes.Buffer
		var mu sync.Mutex
		levels := records.Levels{Store: slog.LevelError, Console: slog.LevelWarn}
		handler := records.NewHandler(store, "followed", levels, console.JSON, console.NoEnv,
			console.To(lockedWriter{&written, &mu}))
		log := slog.New(handler)
		type Log struct {
			Level string `json:"level" config:"live"`
		}
		settings := ways.Must(tinystore.OpenConfig(ctx, store, "log", Log{Level: "info"}))
		ways.Must(0, settings.Update(ctx, func(l *Log) { l.Level = "debug" }))
		ways.Until(t, func() bool { return handler.Enabled(ctx, slog.LevelDebug) })
		log.Debug("kept, not printed")
		ways.Must(0, handler.Flush(ctx))
		mu.Lock()
		printed := written.String()
		mu.Unlock()
		if printed != "" {
			t.Fatalf("the config log moved the console part: printed %q", printed)
		}
		if got := keptMessages(t, store, "followed"); !reflect.DeepEqual(got, []string{"kept, not printed"}) {
			t.Fatalf("kept %q", got)
		}
		ways.Must(0, settings.Reset(ctx, "level"))
	})
}
