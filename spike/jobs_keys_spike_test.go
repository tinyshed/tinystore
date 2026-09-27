package spike

import (
	"database/sql"
	"errors"
	"fmt"
	"testing"
	"time"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// the tables of a job ordered by time whose keys live in a table of their own:
// an acknowledgement deletes the job's row and leaves its key behind, which
// maintenance drops later in the order of the keys, so that a burst of keyed
// jobs writes pages in the order of their time only
var jobsLazySchema = append([]string{
	jobsTimeSchema[0],
	`create table keys (
		queue integer not null,
		key   text    not null,
		next  integer not null,
		id    integer not null,
		primary key (queue, key)
	) strict, without rowid`,
	`create trigger jobs_keyed after insert on jobs when new.key is not null begin
		insert or replace into keys (queue, key, next, id) values (new.queue, new.key, new.next, new.id);
	end`,
}, jobsTimeSchema[2:]...)

func init() {
	jobsLayouts["time-lazy"] = jobsLayout{
		name: "time-lazy", schema: jobsLazySchema, insert: jobsTimeInsert,
		claim: jobsClaimInTable, ack: jobsAckInTable, next: jobsNextTabled, recover: jobsTableRecall,
	}
}

// what a lazy layout reads by key, and what maintenance drops: the keys whose
// job's row is gone, in the order of the keys after where the last batch ended
const (
	jobsLazyByKey = `select j.next, j.id from keys k join jobs j on j.queue = k.queue and j.next = k.next and j.id = k.id
		where k.queue = ?1 and k.key = ?2`
	jobsLazyDrop = `delete from keys where (queue, key) in (
			select k.queue, k.key from keys k
			where k.queue = ?1 and k.key > ?2
				and not exists (select 1 from jobs j where j.queue = k.queue and j.next = k.next and j.id = k.id)
			order by k.key limit cast(?3 as integer))
		returning key`
)

// TestJobsKeys drains a burst of a million keyed jobs due within one minute,
// enqueued shuffled, from the layout whose key index lives on the jobs' table
// and the one whose keys live in a table of their own and are dropped later;
// then times that later drop, 10,000 keys a transaction, and reads a key
func TestJobsKeys(t *testing.T) {
	kvMeasuring(t)
	count := jobsCount()
	for _, name := range []string{"time-table", "time-lazy"} {
		layout := jobsLayouts[name]
		file, path := jobsOpen(t, layout)
		fixture := jobsFixture{count: count, from: jobsFrom, span: time.Minute, keyed: true}
		filled := jobsFill(t, file, layout, fixture)
		t.Logf("%-10s enqueued %d keyed in %s, %8.0f a second, file %6.1f MiB", name, count, jobsMillis(filled),
			float64(count)/filled.Seconds(), float64(kvFileBytes(path))/(1<<20))
		jobsKeyLookups(t, file, name, count)
		now := jobsFrom + 61_000
		t.Logf("%-10s batches of 1000  %s", name, jobsDrain(t, file, layout, now, 1000, 60*time.Second))
		if name == "time-lazy" {
			jobsDropKeys(t, file, count)
		}
		t.Logf("%-10s file %6.1f MiB after the drain", name, float64(kvFileBytes(path))/(1<<20))
	}
}

// jobsKeyLookups times a key read, as an Enqueue, Update or Cancel under a key
// reads it, over 20,000 keys of the burst
func jobsKeyLookups(t *testing.T, file *sqlite.File, name string, count int) {
	t.Helper()
	ctx := t.Context()
	statement := `select next, id from jobs where queue = ?1 and key = ?2`
	if name == "time-lazy" {
		statement = jobsLazyByKey
	}
	const calls = 20_000
	began := time.Now()
	for i := range calls {
		err := file.Lookup(ctx, func(r sqlite.Reader) error {
			var next, id int64
			return sqlite.QueryRow(ctx, r, statement, jobsQueue, jobsKey(i*(count/calls))).Scan(&next, &id)
		})
		if err != nil {
			t.Fatal(err)
		}
	}
	t.Logf("%-10s a key read: %5.1f µs over %d", name, float64(time.Since(began).Microseconds())/calls, calls)
}

// jobsDropKeys drops the keys the drain left behind, 10,000 a transaction in
// the order of the keys, and times it
func jobsDropKeys(t *testing.T, file *sqlite.File, count int) {
	t.Helper()
	ctx := t.Context()
	began := time.Now()
	dropped, batches, after := 0, 0, ""
	var slowest time.Duration
	for {
		batchBegan := time.Now()
		n := 0
		err := file.UpdatePrepared(ctx, func(w sqlite.Writer) error {
			rows, err := w.QueryContext(ctx, jobsLazyDrop, jobsQueue, after, 10_000) //nolint:rowserrcheck // EachRow checks Err
			if err != nil {
				return err
			}
			return sqlite.EachRow(rows, "dropped keys", func(rows *sql.Rows) error {
				var key string
				if scanErr := rows.Scan(&key); scanErr != nil {
					return scanErr
				}
				n, after = n+1, max(after, key)
				return nil
			})
		})
		if err != nil && !errors.Is(err, sql.ErrNoRows) {
			t.Fatal(err)
		}
		slowest = max(slowest, time.Since(batchBegan))
		dropped, batches = dropped+n, batches+1
		if n < 10_000 {
			break
		}
	}
	elapsed := time.Since(began)
	t.Logf("time-lazy  dropped %d of %d keys in %d transactions, %s, %8.0f a second, the slowest %s", dropped,
		count, batches, jobsMillis(elapsed), float64(dropped)/elapsed.Seconds(), jobsMillis(slowest))
	if dropped != count {
		t.Fatal(fmt.Errorf("%d keys dropped of %d", dropped, count))
	}
}
