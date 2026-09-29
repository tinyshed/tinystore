package blobs

import (
	"bytes"
	"context"
	"database/sql"
	"errors"
	"log/slog"
	"os"
	"strings"
	"sync"
	"testing"

	"github.com/tinyshed/tinystore"
)

// logged keeps the records a store logs, for a test to read
type logged struct {
	mu      sync.Mutex
	records []slog.Record
}

func (l *logged) Enabled(context.Context, slog.Level) bool { return true }

func (l *logged) Handle(_ context.Context, record slog.Record) error {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.records = append(l.records, record.Clone())
	return nil
}

func (l *logged) WithAttrs([]slog.Attr) slog.Handler { return l }

func (l *logged) WithGroup(string) slog.Handler { return l }

// at is every record at level, its message and attributes as text
func (l *logged) at(level slog.Level) []string {
	l.mu.Lock()
	defer l.mu.Unlock()
	var lines []string
	for _, record := range l.records {
		if record.Level != level {
			continue
		}
		var line strings.Builder
		line.WriteString(record.Message)
		record.Attrs(func(attr slog.Attr) bool {
			line.WriteString(" " + attr.String())
			return true
		})
		lines = append(lines, line.String())
	}
	return lines
}

// The scrub reads every content, finds one changed and one missing, marks them
// so that their next Open is ErrCorrupt, and logs each once at Error with the
// keys that name it. Writes over them work as over any other.
func TestTheScrubNamesTheKeysOfWhatChanged(t *testing.T) {
	log := &logged{}
	s := openTestStoreWith(t, t.TempDir(), tinystore.Options{Logger: slog.New(log)}, Options{})
	media := openTestBucket(t, s, "media")
	mustPut(t, media, "users/1/changed", randomBytes(1, 1<<20))
	if _, err := media.Copy(t.Context(), "users/1/changed", "users/2/copy"); err != nil {
		t.Fatal(err)
	}
	mustPut(t, media, "users/1/healthy", randomBytes(2, 64<<10))
	mustPut(t, media, "users/1/inline", randomBytes(3, 100))
	mustPut(t, media, "users/3/missing", randomBytes(4, 1<<20))
	if err := flipLastByte(s.pathOf(t, media, "users/1/changed")); err != nil {
		t.Fatal(err)
	}
	if err := os.Remove(s.pathOf(t, media, "users/3/missing")); err != nil {
		t.Fatal(err)
	}

	done := s.scrubPass(t)
	if done.Damaged != 2 {
		t.Fatalf("the scrub found %d contents damaged", done.Damaged)
	}
	lines := log.at(slog.LevelError)
	both := strings.Join(lines, " | ")
	if len(lines) != 2 || !strings.Contains(both, "media/users/1/changed") ||
		!strings.Contains(both, "media/users/2/copy") || !strings.Contains(both, "media/users/3/missing") {
		t.Fatalf("the scrub logged %q", lines)
	}
	for _, key := range []string{"users/1/changed", "users/2/copy", "users/3/missing"} {
		if _, _, err := media.Open(t.Context(), key); !isCorrupt(err) {
			t.Fatalf("an Open of %s after the scrub: %v", key, err)
		}
	}
	mustHold(t, media, "users/1/inline", randomBytes(3, 100))
	if again := s.scrubPass(t); again.Damaged != 0 || len(log.at(slog.LevelError)) != 2 {
		t.Fatalf("a second pass found %d more and logged %d", again.Damaged, len(log.at(slog.LevelError)))
	}
	mustPut(t, media, "users/1/changed", randomBytes(5, 1<<20))
	mustHold(t, media, "users/1/changed", randomBytes(5, 1<<20))
	if err := media.Of("users", 3).Clear(t.Context()); err != nil {
		t.Fatal(err)
	}
}

func isCorrupt(err error) bool {
	return errors.Is(err, tinystore.ErrCorrupt)
}

// pathOf is where the file of the content an object names lies
func (s *testStore) pathOf(t *testing.T, bucket *Bucket, key string) string {
	t.Helper()
	found, err := bucket.lookup(t.Context(), call{path: bucket.folder + key})
	if err != nil || found == nil {
		t.Fatal(found, err)
	}
	_, name := objectName(found.content)
	return s.Store.dir + string(os.PathSeparator) + name
}

// scrubPass runs maintenance until the scrub has read every content once
func (s *testStore) scrubPass(t *testing.T) Maintenance {
	t.Helper()
	var total Maintenance
	for range 100 {
		done := s.maintain(t)
		total.Scrubbed += done.Scrubbed
		total.Damaged += done.Damaged
		var content int64
		if err := s.file.View(t.Context(), func(tx *sql.Tx) error {
			return tx.QueryRowContext(t.Context(), `select content from scrub`).Scan(&content)
		}); err != nil {
			t.Fatal(err)
		}
		if content == 0 {
			return total
		}
	}
	t.Fatal("the scrub did not end its pass in a hundred slices")
	return total
}

// the scrub keeps its place and its hash's state in blobs.db: a pass cut by a
// reopen goes on where it was, and still finds a byte changed after the cut
func TestTheScrubKeepsItsPlaceAcrossReopens(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	mustPut(t, media, "film", randomBytes(1, 3*scrubBuffer+100))
	first := s.maintain(t)
	if first.Scrubbed != scrubBuffer {
		t.Fatalf("the first slice read %d bytes", first.Scrubbed)
	}
	if err := flipLastByte(s.pathOf(t, media, "film")); err != nil {
		t.Fatal(err)
	}
	s = s.reopen(t)
	media = openTestBucket(t, s, "media")
	rest := s.scrubPass(t)
	if rest.Scrubbed != 2*scrubBuffer+100 || rest.Damaged != 1 {
		t.Fatalf("after the reopen the pass read %d bytes and found %d damaged", rest.Scrubbed, rest.Damaged)
	}
	if _, _, err := media.Open(t.Context(), "film"); !isCorrupt(err) {
		t.Fatalf("an Open after the pass: %v", err)
	}
	data := bytes.Repeat([]byte{1}, 10)
	mustPut(t, media, "film", data)
	mustHold(t, media, "film", data)
}
