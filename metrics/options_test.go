package metrics

import (
	"database/sql"
	"sync"
	"testing"
	"time"
)

// the pool size was a constant, so eight readers queued at two connections
func TestReadersRunAsWideAsTheOptionAllows(t *testing.T) {
	const readers = 6
	store, _ := openTestStore(t, Options{Retention: time.Hour, MaxReaders: readers})
	if err := store.Ingest(t.Context(), []Batch{{Series: testSeries(), Samples: testSamples(4)}}); err != nil {
		t.Fatal(err)
	}
	inside, release := make(chan struct{}, readers), make(chan struct{})
	var wait sync.WaitGroup
	for range readers {
		wait.Go(func() {
			_ = store.file.View(t.Context(), func(*sql.Tx) error {
				inside <- struct{}{}
				<-release
				return nil
			})
		})
	}
	deadline := time.After(10 * time.Second)
	for held := range readers {
		select {
		case <-inside:
		case <-deadline:
			close(release)
			wait.Wait()
			t.Fatalf("only %d of %d readers held a snapshot at once", held, readers)
		}
	}
	close(release)
	wait.Wait()
}

func TestAnEmptyReaderPoolIsRefused(t *testing.T) {
	for _, readers := range []int{-1, -8} {
		if _, err := Open(t.Context(), t.TempDir()+"/metrics.db", Options{Retention: time.Hour, MaxReaders: readers}); err == nil {
			t.Fatalf("opened a store with %d readers", readers)
		}
	}
}
