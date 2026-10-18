package console

import (
	"bytes"
	"encoding/json"
	"io"
	"log/slog"
	"os"
	"os/exec"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"
	"testing/slogtest"
	"time"

	"github.com/tinyshed/tinystore/internal/logline"
)

// writeTo sends a handler's lines to a test's writer, in UTC and without colours
type writeTo struct{ io.Writer }

func (w writeTo) Apply(s *logline.Settings) { s.To, s.Zone = w.Writer, time.UTC }

func TestToWritesTheConsoleWhereItIsTold(t *testing.T) {
	t.Setenv("FORCE_COLOR", "")
	var buffer, pretty bytes.Buffer
	slog.New(NewHandler("api", To(&buffer))).Info("started")
	if !strings.HasPrefix(buffer.String(), `{"time":`) {
		t.Fatalf("a buffer, which is no terminal, has %q", buffer.String())
	}
	slog.New(NewHandler("api", To(&pretty), Pretty, TimeOff, HideStream)).Info("started", "port", 3000)
	if want := "INFO  started  port=3000\n"; pretty.String() != want {
		t.Fatalf("pretty %q, want %q", pretty.String(), want)
	}
}

// a line reaches its console as it is logged: pretty with a duration as a
// person reads it, JSON with the nanoseconds a record keeps
func TestEachLineReachesTheConsoleAsItIsLogged(t *testing.T) {
	var pretty, lines bytes.Buffer
	line := slog.NewRecord(time.Date(2026, 10, 2, 11, 2, 11, 123_000_000, time.UTC), slog.LevelWarn, "slow request", 0)
	line.AddAttrs(slog.Duration("took", 1500*time.Millisecond))
	with := []slog.Attr{slog.String("requestId", "7f3a")}
	for _, handler := range []slog.Handler{
		NewHandler("api", Pretty, writeTo{&pretty}).WithAttrs(with),
		NewHandler("api", JSON, writeTo{&lines}).WithAttrs(with),
	} {
		if err := handler.Handle(t.Context(), line); err != nil {
			t.Fatal(err)
		}
	}
	if want := "11:02:11.123 WARN  api  slow request  requestId=7f3a took=1.5s\n"; pretty.String() != want {
		t.Errorf("pretty %q, want %q", pretty.String(), want)
	}
	want := `{"time":"2026-10-02T11:02:11.123Z","level":"WARN","stream":"api","msg":"slow request",` +
		`"requestId":"7f3a","took":1500000000}` + "\n"
	if lines.String() != want {
		t.Errorf("JSON %q, want %q", lines.String(), want)
	}
}

// lines logged from many goroutines at once each reach the console whole
func TestConsoleLinesFromManyGoroutinesDoNotInterleave(t *testing.T) {
	var console bytes.Buffer
	logger := slog.New(NewHandler("api", JSON, writeTo{&console})).With("worker", "w")
	var group sync.WaitGroup
	for worker := range 8 {
		group.Go(func() {
			for i := range 100 {
				logger.Info("step", "worker", worker, "i", i, "text", strings.Repeat("x", 200))
			}
		})
	}
	group.Wait()
	lines := strings.Split(strings.TrimSuffix(console.String(), "\n"), "\n")
	if len(lines) != 800 {
		t.Fatalf("%d lines", len(lines))
	}
	for _, line := range lines {
		if !json.Valid([]byte(line)) {
			t.Fatalf("a line is not whole: %q", line)
		}
	}
}

// a handler of the console alone writes its lines as a store's handler writes
// them, and takes none when its console is off
func TestAHandlerWritesTheConsoleAlone(t *testing.T) {
	var console bytes.Buffer
	logger := slog.New(NewHandler("app", Pretty, writeTo{&console}, Redact("password"))).With("module", "billing")
	logger.Info("charged", "password", "hunter2", "amount", 25)
	if want := " INFO  app  charged  module=billing password=[redacted] amount=25\n"; !strings.HasSuffix(console.String(), want) {
		t.Fatalf("the console has %q, want it to end %q", console.String(), want)
	}
	if NewHandler("app", Off).Enabled(t.Context(), slog.LevelError) {
		t.Fatal("a handler with neither a store nor a console takes lines")
	}
}

func TestAURLsPasswordIsHiddenUnlessKept(t *testing.T) {
	var hidden, kept bytes.Buffer
	slog.New(NewHandler("api", JSON, writeTo{&hidden})).Info("connect", "source", "postgres://ann:hunter2@db/app")
	slog.New(NewHandler("api", JSON, KeepURLPasswords, writeTo{&kept})).Info("connect", "source", "postgres://ann:hunter2@db/app")
	if !strings.Contains(hidden.String(), `"source":"postgres://ann:[redacted]@db/app"`) {
		t.Errorf("hidden: %s", hidden.String())
	}
	if !strings.Contains(kept.String(), `"source":"postgres://ann:hunter2@db/app"`) {
		t.Errorf("kept: %s", kept.String())
	}
}

// ReplaceAttr sees each attribute with its groups, as slog's handlers call it
func TestReplaceAttrChangesAndDropsAttributes(t *testing.T) {
	var console bytes.Buffer
	var seen []string
	replace := func(groups []string, a slog.Attr) slog.Attr {
		seen = append(seen, strings.Join(append(slices.Clone(groups), a.Key), "."))
		switch a.Key {
		case "email":
			return slog.String(a.Key, "a***@example.com")
		case "internal":
			return slog.Attr{}
		}
		return a
	}
	logger := slog.New(NewHandler("api", JSON, ReplaceAttr(replace), writeTo{&console})).With("email", "ann@example.com")
	logger.WithGroup("req").Info("signed in", "internal", 1, slog.Group("user", "id", 7))
	want := `"email":"a***@example.com","req.user.id":7}`
	if !strings.HasSuffix(strings.TrimSpace(console.String()), want) {
		t.Errorf("the console has %s, want it to end %s", console.String(), want)
	}
	if strings.Join(seen, " ") != "email req.internal req.user.id" {
		t.Errorf("ReplaceAttr saw %q", seen)
	}
}

// AddSource says where the line was logged: the record keeps slog's spelling,
// a pretty line its directory, file and line
func TestASourceIsWhereTheLineWasLogged(t *testing.T) {
	var lines, pretty bytes.Buffer
	slog.New(NewHandler("api", JSON, AddSource, writeTo{&lines})).Info("started")
	slog.New(NewHandler("api", Pretty, TimeOff, AddSource, writeTo{&pretty})).Info("started")
	if !strings.Contains(lines.String(), `"source":{"function":"github.com/tinyshed/tinystore/console.TestASourceIsWhereTheLineWasLogged","file":"`) ||
		!strings.Contains(lines.String(), `console_test.go","line":`) {
		t.Errorf("JSON: %s", lines.String())
	}
	if !strings.HasPrefix(pretty.String(), "INFO  api  started  source=console/console_test.go:") {
		t.Errorf("pretty: %s", pretty.String())
	}
}

// TestMain clears what a developer's shell may set, which would change the lines the tests compare.
func TestMain(m *testing.M) {
	for _, name := range []string{"LOG_LEVEL", "LOG_FORMAT", "LOG_TIME"} {
		_ = os.Unsetenv(name)
	}
	os.Exit(m.Run())
}

func TestTheEnvironmentWinsOverTheOptions(t *testing.T) {
	t.Setenv("LOG_LEVEL", "WARN")
	t.Setenv("LOG_FORMAT", "pretty")
	t.Setenv("LOG_TIME", "off")
	var console bytes.Buffer
	logger := slog.New(NewHandler("api", JSON, Level(slog.LevelDebug), TimeFull, writeTo{&console}))
	logger.Info("below the level")
	logger.Warn("kept", "ms", 1200)
	if want := "WARN  api  kept  ms=1200\n"; console.String() != want {
		t.Fatalf("the console has %q, want %q", console.String(), want)
	}

	var events bytes.Buffer
	slog.New(NewHandler("events", Off, writeTo{&events})).Warn("viewed")
	if events.Len() != 0 {
		t.Fatalf("LOG_FORMAT turned on a console the code turned off: %q", events.String())
	}

	t.Setenv("LOG_FORMAT", "off")
	var silenced bytes.Buffer
	slog.New(NewHandler("api", writeTo{&silenced})).Warn("w")
	if silenced.Len() != 0 {
		t.Fatalf("LOG_FORMAT=off left %q", silenced.String())
	}
}

func TestAValueTheEnvironmentCannotMeanIsIgnoredAndSaidOnce(t *testing.T) {
	// a value of this run's own, which no logger of the process said before
	verbose := "verbose-" + strconv.FormatInt(time.Now().UnixNano(), 36)
	t.Setenv("LOG_LEVEL", verbose)
	var first, second bytes.Buffer
	slog.New(NewHandler("api", JSON, writeTo{&first})).Debug("d")
	slog.New(NewHandler("api", JSON, writeTo{&second})).Debug("d")

	lines := strings.Split(strings.TrimSuffix(first.String(), "\n"), "\n")
	if len(lines) != 2 {
		t.Fatalf("the first logger wrote %q", first.String())
	}
	var said map[string]any
	if err := json.Unmarshal([]byte(lines[0]), &said); err != nil {
		t.Fatal(err)
	}
	if said["level"] != "WARN" || said["stream"] != "tinystore" || said["msg"] != "LOG_LEVEL is ignored" ||
		said["value"] != verbose || said["expected"] != "trace, debug, info, warn or error" {
		t.Fatalf("said %v", said)
	}
	if strings.Count(second.String(), "\n") != 1 || !strings.Contains(second.String(), `"msg":"d"`) {
		t.Fatalf("the second logger wrote %q", second.String())
	}
}

// a compose file sets LOG_LEVEL=debug for a neighbour; a host that named its
// prefix reads its own variables and not that one
func TestAHostThatNamedItsPrefixIgnoresTheBareVariables(t *testing.T) {
	t.Setenv("LOG_LEVEL", "debug")
	t.Setenv("LOG_FORMAT", "pretty")
	t.Setenv("DASHBIN_LOG_FORMAT", "json")
	var console bytes.Buffer
	logger := slog.New(NewHandler("api", Level(slog.LevelInfo), FromEnv("DASHBIN"), writeTo{&console}))
	logger.Debug("hidden")
	logger.Info("shown")
	if lines := strings.Split(strings.TrimSuffix(console.String(), "\n"), "\n"); len(lines) != 1 ||
		!strings.HasPrefix(lines[0], `{"time":`) || !strings.Contains(lines[0], `"msg":"shown"`) {
		t.Fatalf("the console has %q", console.String())
	}

	t.Setenv("DASHBIN_LOG_LEVEL", "loud-"+strconv.FormatInt(time.Now().UnixNano(), 36))
	var said bytes.Buffer
	slog.New(NewHandler("api", FromEnv("DASHBIN"), writeTo{&said}))
	if !strings.Contains(said.String(), `"msg":"DASHBIN_LOG_LEVEL is ignored"`) {
		t.Fatalf("an unreadable variable said %q", said.String())
	}
}

// FromLookup reads the host's function, and nothing of the process
func TestALookupIsTheOnlyEnvironmentRead(t *testing.T) {
	t.Setenv("APP_LOG_LEVEL", "error")
	var asked []string
	lookup := func(name string) (string, bool) {
		asked = append(asked, name)
		if name == "APP_LOG_LEVEL" {
			return "warn", true
		}
		return "", false
	}
	var console bytes.Buffer
	logger := slog.New(NewHandler("api", JSON, FromLookup("APP", lookup), writeTo{&console}))
	logger.Info("hidden")
	logger.Warn("shown")
	if strings.Count(console.String(), "\n") != 1 || !strings.Contains(console.String(), `"msg":"shown"`) {
		t.Fatalf("the console has %q", console.String())
	}
	if want := []string{"APP_LOG_LEVEL", "APP_LOG_FORMAT", "APP_LOG_TIME"}; strings.Join(asked, " ") != strings.Join(want, " ") {
		t.Fatalf("asked for %q, want %q", asked, want)
	}
}

func TestNoEnvReadsNothing(t *testing.T) {
	t.Setenv("LOG_LEVEL", "error")
	t.Setenv("LOG_FORMAT", "off")
	var console bytes.Buffer
	slog.New(NewHandler("api", JSON, NoEnv, writeTo{&console})).Info("shown")
	if !strings.Contains(console.String(), `"msg":"shown"`) {
		t.Fatalf("the console has %q", console.String())
	}
}

// a handler keeps every promise testing/slogtest checks of a slog.Handler:
// groups, WithAttrs and WithGroup, empty attributes and groups, LogValuers,
// a record of the zero time
func TestAHandlerKeepsSlogsContract(t *testing.T) {
	var out bytes.Buffer
	slogtest.Run(t, func(*testing.T) slog.Handler {
		out.Reset()
		return NewHandler("api", JSON, NoEnv, writeTo{&out})
	}, func(t *testing.T) map[string]any {
		return grouped(t, out.Bytes())
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

// a program logging to the console alone links no store, no wire and no cgo
func TestTheConsoleImportsNoStore(t *testing.T) {
	out, err := exec.Command("go", "list", "-deps", ".").Output()
	if err != nil {
		t.Fatal(err)
	}
	for _, path := range strings.Fields(string(out)) {
		switch {
		case path == "github.com/tinyshed/tinystore", path == "runtime/cgo",
			strings.HasPrefix(path, "github.com/tinyshed/tinystore/internal/core"),
			strings.HasPrefix(path, "github.com/tinyshed/tinystore/internal/wire"):
			t.Errorf("console links %s", path)
		}
	}
}
