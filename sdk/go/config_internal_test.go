package tinystore

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"tinyshed.org/tinystore/internal/core"
)

// envVectors is testdata/config/env.json, which every language reads alike.
type envVectors struct {
	Names [][3]string `json:"names"`
	Spans [][2]any    `json:"spans"`
}

func readEnvVectors(t *testing.T) envVectors {
	t.Helper()
	text, err := os.ReadFile(filepath.Join("..", "..", "testdata", "config", "env.json"))
	if err != nil {
		t.Fatal(err)
	}
	var vectors envVectors
	if err := json.Unmarshal(text, &vectors); err != nil {
		t.Fatal(err)
	}
	return vectors
}

func TestASettingsVariableIsItsPrefixAndItsPathInUpperSnakeCase(t *testing.T) {
	for _, name := range readEnvVectors(t).Names {
		if got := envName(name[0], name[1]); got != name[2] {
			t.Errorf("%q %q is %s, want %s", name[0], name[1], got, name[2])
		}
	}
}

func TestASpanReadsAndSpellsBackAsTheCoreReadsOne(t *testing.T) {
	for _, span := range readEnvVectors(t).Spans {
		text := span[0].(string)
		ms, err := core.ParseSpan(text, "a span")
		if span[1] == nil {
			if core.KindOf(err) != core.Invalid {
				t.Errorf("%q read as %d, %v", text, ms, err)
			}
			continue
		}
		want := uint64(span[1].(float64))
		if err != nil || ms != want {
			t.Errorf("%q is %d %v, want %d", text, ms, err, want)
		}
		if again, err := core.ParseSpan(core.Spelled(want), "a span"); err != nil || again != want {
			t.Errorf("%q spelled %q reads %d %v", text, core.Spelled(want), again, err)
		}
	}
}
