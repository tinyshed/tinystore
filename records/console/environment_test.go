package console

import (
	"bytes"
	"encoding/json"
	"log/slog"
	"os"
	"strings"
	"testing"
)

// TestMain clears what a developer's shell may set, which would change the lines the tests compare.
func TestMain(m *testing.M) {
	for _, name := range []string{envLevel, envFormat, envTime} {
		_ = os.Unsetenv(name)
	}
	os.Exit(m.Run())
}

func TestTheEnvironmentWinsOverTheOptions(t *testing.T) {
	t.Setenv(envLevel, "WARN")
	t.Setenv(envFormat, "pretty")
	t.Setenv(envTime, "off")
	var console bytes.Buffer
	logger := slog.New(Handler("api", JSON, Level(slog.LevelDebug), TimeFull, writeTo{&console}))
	logger.Info("below the level")
	logger.Warn("kept", "ms", 1200)
	if want := "WARN  api  kept  ms=1200\n"; console.String() != want {
		t.Fatalf("the console has %q, want %q", console.String(), want)
	}

	var events bytes.Buffer
	slog.New(Handler("events", Off, writeTo{&events})).Warn("viewed")
	if events.Len() != 0 {
		t.Fatalf("LOG_FORMAT turned on a console the code turned off: %q", events.String())
	}

	t.Setenv(envFormat, "off")
	var silenced bytes.Buffer
	slog.New(Handler("api", writeTo{&silenced})).Warn("w")
	if silenced.Len() != 0 {
		t.Fatalf("LOG_FORMAT=off left %q", silenced.String())
	}
}

func TestAValueTheEnvironmentCannotMeanIsIgnoredAndSaidOnce(t *testing.T) {
	t.Setenv(envLevel, "verbose")
	saidIgnored.Delete(envLevel + "=verbose")
	var first, second bytes.Buffer
	slog.New(Handler("api", JSON, writeTo{&first})).Debug("d")
	slog.New(Handler("api", JSON, writeTo{&second})).Debug("d")

	lines := strings.Split(strings.TrimSuffix(first.String(), "\n"), "\n")
	if len(lines) != 2 {
		t.Fatalf("the first logger wrote %q", first.String())
	}
	var said map[string]any
	if err := json.Unmarshal([]byte(lines[0]), &said); err != nil {
		t.Fatal(err)
	}
	if said["level"] != "WARN" || said["stream"] != "tinystore" || said["msg"] != "LOG_LEVEL is ignored" ||
		said["value"] != "verbose" || said["expected"] != "debug, info, warn or error" {
		t.Fatalf("said %v", said)
	}
	if strings.Count(second.String(), "\n") != 1 || !strings.Contains(second.String(), `"msg":"d"`) {
		t.Fatalf("the second logger wrote %q", second.String())
	}
}

// a compose file sets LOG_LEVEL=debug for a neighbour; a host that named its
// prefix reads its own variables and not that one
func TestAHostThatNamedItsPrefixIgnoresTheBareVariables(t *testing.T) {
	t.Setenv(envLevel, "debug")
	t.Setenv(envFormat, "pretty")
	t.Setenv("DASHBIN_LOG_FORMAT", "json")
	var console bytes.Buffer
	logger := slog.New(Handler("api", Level(slog.LevelInfo), FromEnv("DASHBIN"), writeTo{&console}))
	logger.Debug("hidden")
	logger.Info("shown")
	if lines := strings.Split(strings.TrimSuffix(console.String(), "\n"), "\n"); len(lines) != 1 ||
		!strings.HasPrefix(lines[0], `{"time":`) || !strings.Contains(lines[0], `"msg":"shown"`) {
		t.Fatalf("the console has %q", console.String())
	}

	t.Setenv("DASHBIN_LOG_LEVEL", "loud")
	saidIgnored.Delete("DASHBIN_LOG_LEVEL=loud")
	var said bytes.Buffer
	slog.New(Handler("api", FromEnv("DASHBIN"), writeTo{&said}))
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
	logger := slog.New(Handler("api", JSON, FromLookup("APP", lookup), writeTo{&console}))
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
	t.Setenv(envLevel, "error")
	t.Setenv(envFormat, "off")
	var console bytes.Buffer
	slog.New(Handler("api", JSON, NoEnv, writeTo{&console})).Info("shown")
	if !strings.Contains(console.String(), `"msg":"shown"`) {
		t.Fatalf("the console has %q", console.String())
	}
}
