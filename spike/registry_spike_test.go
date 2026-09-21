package spike

// one million series in two shapes, so the numbers pick between holding the
// registry in memory and letting SQLite hold it

import (
	"database/sql"
	"encoding/binary"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"testing"
	"time"

	_ "modernc.org/sqlite"
)

const (
	installations   = 50_000
	perInstallation = 20
	seriesTotal     = installations * perInstallation
)

// label is one name and value of a series; a series is its metric plus these
type label struct{ name, value string }

type series struct {
	metric string
	labels []label
}

var metrics = []string{
	"sources_total", "dashboards_total", "panels_total", "accounts_total",
	"agents_total", "queries_total", "sources_by_kind", "agents_connected",
	"uptime_seconds", "queries_failed_total", "rows_read_total", "alerts_total",
	"panels_drawn_total", "sessions_total", "sources_unavailable", "schema_reads_total",
	"cache_hits_total", "cache_misses_total", "bytes_sent_total", "restarts_total",
}

var kinds = []string{"postgres", "mysql", "prometheus", "http-json"}

var versions = []string{
	"v0.1.0", "v0.2.0", "v0.2.1", "v0.3.0", "v0.3.1",
	"v0.4.0", "v0.4.1", "v0.5.0", "v0.5.1", "v0.6.0",
}

var oses = []string{"linux", "darwin", "windows"}

// generate makes the shape telemetry would really send: one label of enormous
// cardinality and a handful of small ones beside it
func generate() []series {
	all := make([]series, 0, seriesTotal)
	for i := range installations {
		install := fmt.Sprintf("inst-%07d", i)
		version := versions[i%len(versions)]
		host := oses[i%len(oses)]
		for m, name := range metrics {
			labels := []label{
				{"installation", install},
				{"version", version},
				{"os", host},
			}
			if name == "sources_by_kind" || m%5 == 0 {
				labels = append(labels, label{"kind", kinds[(i+m)%len(kinds)]})
			}
			all = append(all, series{metric: name, labels: labels})
		}
	}
	return all
}

func heapMiB() float64 {
	runtime.GC()
	var stats runtime.MemStats
	runtime.ReadMemStats(&stats)
	return float64(stats.HeapAlloc) / (1 << 20)
}

// residentMiB is what the operating system thinks we hold, which a page cache reaches and a heap does not
func residentMiB() string {
	raw, err := os.ReadFile("/proc/self/status")
	if err != nil {
		return "unavailable on this platform"
	}
	for line := range strings.SplitSeq(string(raw), "\n") {
		if !strings.HasPrefix(line, "VmRSS:") {
			continue
		}
		fields := strings.Fields(line)
		if len(fields) < 2 {
			break
		}
		kib, err := strconv.ParseFloat(fields[1], 64)
		if err != nil {
			break
		}
		return fmt.Sprintf("%.1f MiB", kib/1024)
	}
	return "unknown"
}

func TestAMillionSeriesInMemory(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}

	all := generate()
	before := heapMiB()

	start := time.Now()
	reg := newMemRegistry()
	for _, s := range all {
		reg.put(s)
	}
	built := time.Since(start)
	after := heapMiB()

	t.Logf("series             %d", len(reg.ids))
	t.Logf("register           %s (%.0f series/s)", built.Round(time.Millisecond),
		float64(len(reg.ids))/built.Seconds())
	t.Logf("heap               %.1f MiB (registry and postings)", after-before)
	t.Logf("rss                %s", residentMiB())
	t.Logf("distinct strings   %d", len(reg.values))
	t.Logf("posting lists      %d", len(reg.postings))

	sample := all[len(all)/2]
	start = time.Now()
	const lookups = 100_000
	for range lookups {
		if _, held := reg.lookup(sample); !held {
			t.Fatal("the series it just stored is not there")
		}
	}
	t.Logf("lookup             %s per sample", (time.Since(start) / lookups).Round(time.Nanosecond))

	start = time.Now()
	one := reg.selection(label{"kind", "mysql"})
	t.Logf("select kind=mysql  %s for %d series", time.Since(start).Round(time.Microsecond), len(one))

	start = time.Now()
	two := reg.intersect(label{"version", "v0.3.0"}, label{"os", "linux"})
	t.Logf("select two labels  %s for %d series", time.Since(start).Round(time.Microsecond), len(two))

	start = time.Now()
	narrow := reg.selection(label{"installation", "inst-0002500"})
	t.Logf("select one install %s for %d series", time.Since(start).Round(time.Microsecond), len(narrow))
}

type memRegistry struct {
	intern   map[string]uint32
	values   []string
	ids      map[string]uint32 // canonical key of interned ids, to a series
	labels   [][]uint32        // per series, the metric then name and value id pairs
	postings map[uint64][]uint32
}

func newMemRegistry() *memRegistry {
	return &memRegistry{
		intern:   make(map[string]uint32, 1<<16),
		ids:      make(map[string]uint32, seriesTotal),
		postings: make(map[uint64][]uint32, 1<<16),
	}
}

func (r *memRegistry) id(s string) uint32 {
	if held, ok := r.intern[s]; ok {
		return held
	}
	next := uint32(len(r.values))
	r.values = append(r.values, s)
	r.intern[s] = next
	return next
}

// key is what makes two identical label sets one series, without holding their text
func (r *memRegistry) key(s series) (string, []uint32) {
	pairs := make([]uint32, 0, 1+2*len(s.labels))
	pairs = append(pairs, r.id(s.metric))
	for _, l := range s.labels {
		pairs = append(pairs, r.id(l.name), r.id(l.value))
	}
	raw := make([]byte, 4*len(pairs))
	for i, v := range pairs {
		binary.LittleEndian.PutUint32(raw[4*i:], v)
	}
	return string(raw), pairs
}

func (r *memRegistry) put(s series) uint32 {
	key, pairs := r.key(s)
	if held, ok := r.ids[key]; ok {
		return held
	}

	ref := uint32(len(r.labels))
	r.ids[key] = ref
	r.labels = append(r.labels, pairs)
	for i := 1; i < len(pairs); i += 2 {
		slot := uint64(pairs[i])<<32 | uint64(pairs[i+1])
		r.postings[slot] = append(r.postings[slot], ref)
	}
	return ref
}

func (r *memRegistry) lookup(s series) (uint32, bool) {
	key, _ := r.key(s)
	held, ok := r.ids[key]
	return held, ok
}

func (r *memRegistry) slot(l label) (uint64, bool) {
	name, ok := r.intern[l.name]
	if !ok {
		return 0, false
	}
	value, ok := r.intern[l.value]
	if !ok {
		return 0, false
	}
	return uint64(name)<<32 | uint64(value), true
}

func (r *memRegistry) selection(l label) []uint32 {
	slot, ok := r.slot(l)
	if !ok {
		return nil
	}
	return r.postings[slot]
}

func (r *memRegistry) intersect(a, b label) []uint32 {
	left, right := r.selection(a), r.selection(b)
	if len(left) > len(right) {
		left, right = right, left
	}

	in := make(map[uint32]struct{}, len(left))
	for _, ref := range left {
		in[ref] = struct{}{}
	}
	out := make([]uint32, 0, len(left))
	for _, ref := range right {
		if _, held := in[ref]; held {
			out = append(out, ref)
		}
	}
	return out
}

func TestAMillionSeriesInSQLite(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}

	dir := t.TempDir()
	path := filepath.Join(dir, "series.db")
	dsn := "file:" + path + "?_dqs=0&_defensive=1&_pragma=busy_timeout(5000)" +
		"&_pragma=synchronous(NORMAL)&_pragma=temp_store(MEMORY)&_txlock=immediate" +
		"&_pragma=journal_mode(WAL)"

	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()
	db.SetMaxOpenConns(1)

	for _, statement := range []string{
		`create table strings (id integer primary key, text text not null unique) strict`,
		`create table series (id integer primary key, metric_id integer not null,
		  fingerprint blob not null unique) strict`,
		`create table postings (name_id integer not null, value_id integer not null,
		  series_id integer not null, primary key (name_id, value_id, series_id))
		  strict, without rowid`,
	} {
		if _, err = db.Exec(statement); err != nil {
			t.Fatal(err)
		}
	}

	all := generate()
	before := heapMiB()
	interned := map[string]int64{}

	// the strings and the series first, so the file says what each half costs
	start := time.Now()
	tx, err := db.Begin()
	if err != nil {
		t.Fatal(err)
	}
	putString, err := tx.Prepare(`insert into strings(text) values(?) returning id`)
	if err != nil {
		t.Fatal(err)
	}
	defer putString.Close()
	putSeries, err := tx.Prepare(`insert into series(metric_id, fingerprint) values(?, ?)`)
	if err != nil {
		t.Fatal(err)
	}
	defer putSeries.Close()

	id := func(s string) int64 {
		if held, ok := interned[s]; ok {
			return held
		}
		var next int64
		if err = putString.QueryRow(s).Scan(&next); err != nil {
			t.Fatal(err)
		}
		interned[s] = next
		return next
	}

	for _, s := range all {
		metric := id(s.metric)
		print := binary.LittleEndian.AppendUint64(make([]byte, 0, 8+8*len(s.labels)), uint64(metric))
		for _, l := range s.labels {
			print = binary.LittleEndian.AppendUint32(print, uint32(id(l.name)))
			print = binary.LittleEndian.AppendUint32(print, uint32(id(l.value)))
		}
		if _, err = putSeries.Exec(metric, print); err != nil {
			t.Fatal(err)
		}
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
	registered := time.Since(start)
	checkpoint(t, db)
	seriesOnly := fileMiB(t, path)

	// then the postings, which is the half a selector reads
	start = time.Now()
	if tx, err = db.Begin(); err != nil {
		t.Fatal(err)
	}
	putPosting, err := tx.Prepare(`insert into postings(name_id, value_id, series_id) values(?, ?, ?)`)
	if err != nil {
		t.Fatal(err)
	}
	defer putPosting.Close()
	written := 0
	for ref, s := range all {
		for _, l := range s.labels {
			if _, err = putPosting.Exec(interned[l.name], interned[l.value], int64(ref)+1); err != nil {
				t.Fatal(err)
			}
			written++
		}
	}
	if err = tx.Commit(); err != nil {
		t.Fatal(err)
	}
	indexed := time.Since(start)
	checkpoint(t, db)
	if _, err = db.Exec(`analyze`); err != nil {
		t.Fatal(err)
	}
	whole := fileMiB(t, path)

	t.Logf("register           %s (%.0f series/s)", registered.Round(time.Millisecond),
		float64(len(all))/registered.Seconds())
	t.Logf("index              %s (%.0f postings/s)", indexed.Round(time.Millisecond),
		float64(written)/indexed.Seconds())
	t.Logf("heap               %.1f MiB (the intern cache the writer kept)", heapMiB()-before)
	t.Logf("rss                %s", residentMiB())
	t.Logf("file               %.1f MiB total: %.1f strings and series, %.1f postings",
		whole, seriesOnly, whole-seriesOnly)
	t.Logf("distinct strings   %d", len(interned))
	t.Logf("postings           %d", written)

	sample := all[len(all)/2]
	print := binary.LittleEndian.AppendUint64(make([]byte, 0, 8+8*len(sample.labels)),
		uint64(interned[sample.metric]))
	for _, l := range sample.labels {
		print = binary.LittleEndian.AppendUint32(print, uint32(interned[l.name]))
		print = binary.LittleEndian.AppendUint32(print, uint32(interned[l.value]))
	}

	lookup, err := db.Prepare(`select id from series where fingerprint = ?`)
	if err != nil {
		t.Fatal(err)
	}
	defer lookup.Close()
	start = time.Now()
	const lookups = 20_000
	for range lookups {
		var got int64
		if err = lookup.QueryRow(print).Scan(&got); err != nil {
			t.Fatal(err)
		}
	}
	t.Logf("lookup             %s per sample", (time.Since(start) / lookups).Round(time.Nanosecond))

	t.Logf("select kind=mysql  %s", timeSelect(t, db,
		`select series_id from postings where name_id = ? and value_id = ?`,
		interned["kind"], interned["mysql"]))
	t.Logf("select two labels  %s", timeSelect(t, db,
		`select series_id from postings where name_id = ? and value_id = ?
		 intersect select series_id from postings where name_id = ? and value_id = ?`,
		interned["version"], interned["v0.3.0"], interned["os"], interned["linux"]))
	t.Logf("select one install %s", timeSelect(t, db,
		`select series_id from postings where name_id = ? and value_id = ?`,
		interned["installation"], interned["inst-0002500"]))

	// what a restart costs when nothing at all is held in memory
	if err = db.Close(); err != nil {
		t.Fatal(err)
	}
	start = time.Now()
	cold, err := sql.Open("sqlite", dsn+"&_query_only=1")
	if err != nil {
		t.Fatal(err)
	}
	defer cold.Close()

	var count int64
	if err = cold.QueryRow(`select count(*) from series`).Scan(&count); err != nil {
		t.Fatal(err)
	}
	t.Logf("cold open + count  %s for %d series", time.Since(start).Round(time.Millisecond), count)
	t.Logf("cold select        %s", timeSelect(t, cold,
		`select series_id from postings where name_id = ? and value_id = ?`,
		interned["installation"], interned["inst-0033333"]))

	// and what it would cost to warm the whole thing into memory instead
	warm := heapMiB()
	start = time.Now()
	rows, err := cold.Query(`select name_id, value_id, series_id from postings`)
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()

	held := make(map[uint64][]uint32, 1<<16)
	for rows.Next() {
		var name, value, ref int64
		if err = rows.Scan(&name, &value, &ref); err != nil {
			t.Fatal(err)
		}
		slot := uint64(name)<<32 | uint64(value)
		held[slot] = append(held[slot], uint32(ref))
	}
	if err = rows.Err(); err != nil {
		t.Fatal(err)
	}
	t.Logf("warm all postings  %s, %.1f MiB, %d lists",
		time.Since(start).Round(time.Millisecond), heapMiB()-warm, len(held))
}

func checkpoint(t *testing.T, db *sql.DB) {
	t.Helper()

	if _, err := db.Exec(`pragma wal_checkpoint(truncate)`); err != nil {
		t.Fatal(err)
	}
}

func timeSelect(t *testing.T, db *sql.DB, query string, args ...any) string {
	t.Helper()

	start := time.Now()
	rows, err := db.Query(query, args...)
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()

	found := 0
	for rows.Next() {
		found++
	}
	if err = rows.Err(); err != nil {
		t.Fatal(err)
	}
	return fmt.Sprintf("%s for %d series", time.Since(start).Round(time.Microsecond), found)
}

func fileMiB(t *testing.T, path string) float64 {
	t.Helper()

	info, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	return float64(info.Size()) / (1 << 20)
}
