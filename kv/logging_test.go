package kv

import (
	"strings"
	"testing"
	"time"
)

func TestMaintenanceFailureNamesItsStageAndCounterBucket(t *testing.T) {
	state := openTestState(t, t.TempDir())
	attempts := openTestCounters(t, state, "attempts", LoseAtMost(time.Hour))
	if _, err := attempts.Add(t.Context(), "one", 1); err != nil {
		t.Fatal(err)
	}
	state.exec(t, `create trigger refuse_flush before insert on _tinystore_kv_cells begin select raise(abort, 'a refused flush'); end`)
	_, err := state.Maintain(t.Context())
	if err == nil || !strings.Contains(err.Error(), "flush counters") ||
		!strings.Contains(err.Error(), `counter bucket "attempts"`) {
		t.Fatalf("unattributed maintenance error: %v", err)
	}
	state.exec(t, `drop trigger refuse_flush`)
}
