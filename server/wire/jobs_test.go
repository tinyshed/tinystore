package wire_test

import (
	"reflect"
	"testing"

	"github.com/tinyshed/tinystore/server/wire"
)

func TestJobsMessagesReadBackAsTheyWereWritten(t *testing.T) {
	repeat := &wire.Repeat{Cron: "10 3 * * *", Zone: "Europe/Moscow"}
	messages := []struct {
		written interface{ Append([]byte) []byte }
		read    interface{ Decode([]byte) error }
	}{
		{wire.JobsQueue{
			Name: "q", Lease: 30_000, MaxAttempts: 3, BackoffFirst: 1000, BackoffMost: 60_000,
			MaxWaiting: 10, KeepFailed: 1, KeepDone: 2, Schedule: repeat,
		}, &wire.JobsQueue{}},
		{wire.JobsBatch{Handle: 1, Jobs: []wire.JobsJob{
			{Value: `{"a":1}`, Key: "k", At: 1_790_000_000_000, Repeat: &wire.Repeat{Every: 60_000}},
			{Value: `2`, After: 5},
		}}, &wire.JobsBatch{}},
		{wire.JobsChange{Handle: 2, JobsJob: wire.JobsJob{Value: `null`, Key: "k", Repeat: repeat}}, &wire.JobsChange{}},
		{wire.JobsKey{Handle: 3, Key: "k"}, &wire.JobsKey{}},
		{
			wire.JobsEntry{Found: true, Key: "k", Value: `{}`, At: 1, Attempt: 2, State: 3, Err: "no", Repeat: "@every 1s"},
			&wire.JobsEntry{},
		},
		{wire.JobsLease{Handle: 4, Lease: 1000}, &wire.JobsLease{}},
		{wire.JobsHeld{Found: true, Job: 9, Key: "k", Value: `[]`, At: 5, Attempt: 1}, &wire.JobsHeld{}},
		{
			wire.JobsOutcomes{Outcomes: []wire.JobsOutcome{{Job: 9, How: wire.JobRetry, Err: "later", After: 100}}},
			&wire.JobsOutcomes{},
		},
		{wire.JobsSettled{Errors: []*wire.Error{nil, {Code: wire.CodeConflict, Message: "lost"}}}, &wire.JobsSettled{}},
		{wire.JobsQuery{Handle: 5, Prefix: "chat:", State: 3, After: "chat:1", Limit: 10}, &wire.JobsQuery{}},
		{wire.JobsPage{More: true, After: "k"}, &wire.JobsPage{}},
		{wire.JobsWorkers{Handle: 6, Workers: 8, Timeout: 60_000}, &wire.JobsWorkers{}},
	}
	for _, m := range messages {
		if err := m.read.Decode(m.written.Append(nil)); err != nil {
			t.Errorf("%T: %v", m.written, err)
			continue
		}
		if got := reflect.ValueOf(m.read).Elem().Interface(); !reflect.DeepEqual(got, m.written) {
			t.Errorf("%T read as %+v", m.written, got)
		}
	}
}
