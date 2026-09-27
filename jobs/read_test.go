package jobs

import (
	"bytes"
	"errors"
	"fmt"
	"slices"
	"strings"
	"testing"
	"time"
)

// Scan walks the keys under a prefix in the byte order of their text, a page
// at a time, waiting, leased and failed jobs alike, and All meets each once
func TestScanWalksTheKeysUnderAPrefixAPageAtATime(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[string](t, queues, "later", MaxAttempts(1))
	for _, key := range []string{"chat:42:c", "chat:42:a", "chat:421:x", "chat:42:b", "chat:43:a", "chat:42:d"} {
		mustEnqueue(t, queue, "draft "+key, Key(key), After(time.Duration(len(key))*time.Minute))
	}
	queues.clock.advance(time.Hour)
	failing := mustClaim(t, queue)
	if err := failing.Fail(t.Context(), errors.New("the chat is gone")); err != nil {
		t.Fatal(err)
	}
	leased := mustClaim(t, queue)

	page, err := queue.Scan(t.Context(), Query{Prefix: "chat:42:", Limit: 2})
	if err != nil || !page.More || keysOf(page.Entries) != "chat:42:a chat:42:b" {
		t.Fatalf("the first page: %v, more %v, %v", keysOf(page.Entries), page.More, err)
	}
	page, err = queue.Scan(t.Context(), page.Next)
	if err != nil || page.More || keysOf(page.Entries) != "chat:42:c chat:42:d" {
		t.Fatalf("the second page: %v, more %v, %v", keysOf(page.Entries), page.More, err)
	}

	states := map[string]State{}
	for entry, err := range queue.All(t.Context(), Query{Prefix: "chat:4", Limit: 1}) {
		if err != nil {
			t.Fatal(err)
		}
		states[entry.Key] = entry.State
	}
	if len(states) != 6 || states[failing.Key] != Failed || states[leased.Key] != Leased {
		t.Fatalf("All under chat:4 met %v", states)
	}

	page, err = queue.Scan(t.Context(), Query{Prefix: "chat:42:", State: Leased})
	if err != nil || keysOf(page.Entries) != leased.Key {
		t.Fatalf("the leased jobs: %v, %v", keysOf(page.Entries), err)
	}
}

// with no prefix, State Failed lists the failed jobs the last failed first,
// keyed or not, and a page's cursor reads on where it ended
func TestScanListsTheFailedJobsTheLastFailedFirst(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[int](t, queues, "flaky", MaxAttempts(1))
	for n := range 5 {
		var options []EnqueueOption
		if n%2 == 0 {
			options = append(options, Key(fmt.Sprint("job:", n)))
		}
		mustEnqueue(t, queue, n, options...)
	}
	for range 5 {
		job := mustClaim(t, queue)
		if err := job.Fail(t.Context(), fmt.Errorf("job %d failed", job.Value)); err != nil {
			t.Fatal(err)
		}
		queues.clock.advance(time.Second)
	}

	var failed []int
	query := Query{State: Failed, Limit: 2}
	for {
		page, err := queue.Scan(t.Context(), query)
		if err != nil {
			t.Fatal(err)
		}
		for _, entry := range page.Entries {
			if entry.State != Failed || entry.Err != fmt.Sprintf("job %d failed", entry.Value) {
				t.Fatalf("a failed job reads %+v", entry)
			}
			failed = append(failed, entry.Value)
		}
		if !page.More {
			break
		}
		query = page.Next
	}
	if !slices.Equal(failed, []int{4, 3, 2, 1, 0}) {
		t.Fatalf("the failed jobs came as %v", failed)
	}
}

// a page ends before the value that would take it past 4 MiB, spilled values
// counted, and the next page goes on from there without losing one
func TestAScanPageHoldsAtMostItsBytes(t *testing.T) {
	queues := openTestQueues(t, t.TempDir())
	queue := openTestQueue[[]byte](t, queues, "uploads")
	const value = 900_000
	for n := range 6 {
		mustEnqueue(t, queue, bytes.Repeat([]byte{byte('a' + n)}, value), Key(fmt.Sprint("upload:", n)))
	}

	var pages []int
	query := Query{Prefix: "upload:"}
	for {
		page, err := queue.Scan(t.Context(), query)
		if err != nil {
			t.Fatal(err)
		}
		held := 0
		for _, entry := range page.Entries {
			held += len(entry.Value)
		}
		if held > scanBytes {
			t.Fatalf("a page held %d bytes of values", held)
		}
		pages = append(pages, len(page.Entries))
		if !page.More {
			break
		}
		query = page.Next
	}
	if !slices.Equal(pages, []int{4, 2}) {
		t.Fatalf("six values of %d bytes came in pages of %v", value, pages)
	}
}

func keysOf[V any](entries []Entry[V]) string {
	keys := make([]string, len(entries))
	for i, entry := range entries {
		keys[i] = entry.Key
	}
	return strings.Join(keys, " ")
}
