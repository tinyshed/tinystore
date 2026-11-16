package blobs_test

import (
	"bytes"
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"tinyshed.org/tinystore"
	"tinyshed.org/tinystore/blobs"
	"tinyshed.org/tinystore/internal/ways"
)

// held is a run's function that says it started and waits for go, then gives data.
func held(started chan<- struct{}, release <-chan struct{}, data string) func(context.Context) (blobs.Fill, error) {
	return func(context.Context) (blobs.Fill, error) {
		started <- struct{}{}
		<-release
		return blobs.Fill{Data: []byte(data)}, nil
	}
}

// settle gives the calls just started time to ask, and to wait for the run under way.
func settle() { time.Sleep(300 * time.Millisecond) }

func TestAMissRunsTheFunctionOnceAndTheNextCallReadsItsFile(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		pages := ways.Must(blobs.OpenFiles(store, "pages"))
		var runs atomic.Int32
		convert := func(context.Context) (blobs.Fill, error) {
			runs.Add(1)
			return blobs.Fill{Data: []byte("%PDF"), ContentType: "application/pdf"}, nil
		}
		page := ways.Must(pages.GetOrCreate(ctx, "a.pdf", convert, blobs.ContentType("text/plain"), blobs.Meta(map[string]string{"doc": "42"})))
		if page.ContentType != "application/pdf" || page.Meta["doc"] != "42" || !bytes.Equal(readAll(t, page), []byte("%PDF")) {
			t.Fatalf("page %+v", page.FileInfo)
		}
		again := ways.Must(pages.GetOrCreate(ctx, "a.pdf", convert))
		if again.ETag != page.ETag || runs.Load() != 1 {
			t.Fatalf("the second call ran the function: %d runs", runs.Load())
		}
		typed := ways.Must(pages.GetOrCreate(ctx, "b.txt", func(context.Context) (blobs.Fill, error) {
			return blobs.Fill{Data: []byte("b")}, nil
		}, blobs.ContentType("text/plain"), blobs.Timeout(time.Minute)))
		if typed.ContentType != "text/plain" {
			t.Fatalf("the call's type: %+v", typed.FileInfo)
		}
	})
}

// contract: BLOB-21
func TestConcurrentGetOrCreatesOfOnePathRunTheFunctionOnceAndAllGetItsFile(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		pages := ways.Must(blobs.OpenFiles(store, "pages"))
		var runs atomic.Int32
		started, release := make(chan struct{}, 1), make(chan struct{})
		convert := func(ctx context.Context) (blobs.Fill, error) {
			runs.Add(1)
			return held(started, release, "made once")(ctx)
		}
		var wg sync.WaitGroup
		got := make([][]byte, 6)
		errs := make([]error, 6)
		for n := range got {
			wg.Add(1)
			go func() {
				defer wg.Done()
				file, err := pages.GetOrCreate(ctx, "a.pdf", convert)
				if errs[n] = err; err == nil {
					got[n] = readAll(t, file)
				}
			}()
			if n == 0 {
				<-started
			}
		}
		settle()
		close(release)
		wg.Wait()
		for n := range got {
			if errs[n] != nil || string(got[n]) != "made once" {
				t.Fatalf("call %d: %q, %v", n, got[n], errs[n])
			}
		}
		if runs.Load() != 1 {
			t.Fatalf("the function ran %d times", runs.Load())
		}
	})
}

// contract: BLOB-22
func TestAFailureIsTheCallersOwnAndEveryWaitingCallsAndTheNextCallRunsAgain(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		pages := ways.Must(blobs.OpenFiles(store, "pages"))
		parse := errors.New("the document does not parse")
		started, release := make(chan struct{}, 1), make(chan struct{})
		failing := make(chan error, 1)
		go func() {
			_, err := pages.GetOrCreate(ctx, "bad.pdf", func(context.Context) (blobs.Fill, error) {
				started <- struct{}{}
				<-release
				return blobs.Fill{}, parse
			})
			failing <- err
		}()
		<-started
		waited := make(chan error, 1)
		go func() {
			_, err := pages.GetOrCreate(ctx, "bad.pdf", func(context.Context) (blobs.Fill, error) {
				return blobs.Fill{Data: []byte("never")}, nil
			})
			waited <- err
		}()
		settle()
		close(release)
		if err := <-failing; err != parse {
			t.Fatalf("the caller's own error: %v", err)
		}
		err := <-waited
		is(t, err, tinystore.ErrConflict)
		if !strings.Contains(err.Error(), "the document does not parse") {
			t.Fatalf("the waiting call's error: %v", err)
		}
		page := ways.Must(pages.GetOrCreate(ctx, "bad.pdf", func(context.Context) (blobs.Fill, error) {
			return blobs.Fill{Data: []byte("parsed")}, nil
		}))
		if string(readAll(t, page)) != "parsed" {
			t.Fatal("the next call ran it again")
		}
	})
}

// contract: BLOB-22
func TestAPutDuringARunWinsAndTheRunsCallReturnsThePutsFile(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		pages := ways.Must(blobs.OpenFiles(store, "pages"))
		started, release := make(chan struct{}, 1), make(chan struct{})
		run := make(chan []byte, 1)
		go func() {
			file, err := pages.GetOrCreate(ctx, "a.pdf", held(started, release, "made"))
			if err != nil {
				t.Error(err)
				run <- nil
				return
			}
			run <- readAll(t, file)
		}()
		<-started
		ways.Must(pages.Put(ctx, "a.pdf", strings.NewReader("put")))
		close(release)
		if got := <-run; string(got) != "put" {
			t.Fatalf("the run's call got %q", got)
		}
	})
}

func TestAReaderAndAFileOfThisMachinePastOneMessageLandAndReadBack(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		pages := ways.Must(blobs.OpenFiles(store, "pages"))
		data := pattern(300_000+17, 3)
		source := filepath.Join(t.TempDir(), "source.bin")
		if err := os.WriteFile(source, data, 0o600); err != nil {
			t.Fatal(err)
		}
		read := ways.Must(pages.GetOrCreate(ctx, "read.bin", func(context.Context) (blobs.Fill, error) {
			return blobs.Fill{Reader: bytes.NewReader(data)}, nil
		}))
		fromFile := ways.Must(pages.GetOrCreate(ctx, "file.bin", func(context.Context) (blobs.Fill, error) {
			return blobs.Fill{Path: source}, nil
		}))
		if read.Size != int64(len(data)) || fromFile.ETag != read.ETag || !bytes.Equal(readAll(t, fromFile), data) {
			t.Fatalf("read %+v, file %+v", read.FileInfo, fromFile.FileInfo)
		}
		_ = read.Close()
		_ = fromFile.Close()
	})
}

func TestACallWhoseContextEndsWhileItWaitsStopsAndTheRunGoesOn(t *testing.T) {
	ways.Each(t, func(t *testing.T, store *tinystore.Store) {
		ctx := context.Background()
		pages := ways.Must(blobs.OpenFiles(store, "pages"))
		started, release := make(chan struct{}, 1), make(chan struct{})
		run := make(chan error, 1)
		go func() {
			_, err := pages.GetOrCreate(ctx, "a.pdf", held(started, release, "made"))
			run <- err
		}()
		<-started
		waiting, cancel := context.WithTimeout(ctx, 200*time.Millisecond)
		defer cancel()
		_, err := pages.GetOrCreate(waiting, "a.pdf", func(context.Context) (blobs.Fill, error) {
			return blobs.Fill{Data: []byte("never")}, nil
		})
		if err == nil {
			t.Fatal("a call whose context ended while it waited")
		}
		close(release)
		if err := <-run; err != nil {
			t.Fatal(err)
		}
		page, found := ways.Must2(pages.Get(ctx, "a.pdf"))
		if !found || string(readAll(t, page)) != "made" {
			t.Fatal("the run went on and landed")
		}
	})
}
