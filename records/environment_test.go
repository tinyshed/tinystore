package records

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
	logger := slog.New(Handler("api", ConsoleJSON, Level(slog.LevelDebug), TimeFull, writeTo{&console}))
	logger.Info("below the level")
	logger.Warn("kept", "ms", 1200)
	if want := "WARN  api  kept  ms=1200\n"; console.String() != want {
		t.Fatalf("the console has %q, want %q", console.String(), want)
	}

	var events bytes.Buffer
	slog.New(Handler("events", ConsoleOff, writeTo{&events})).Warn("viewed")
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
	slog.New(Handler("api", ConsoleJSON, writeTo{&first})).Debug("d")
	slog.New(Handler("api", ConsoleJSON, writeTo{&second})).Debug("d")

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
