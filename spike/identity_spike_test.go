package spike

import (
	"crypto/sha256"
	"database/sql"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"testing"

	_ "modernc.org/sqlite"
)

// what a series costs before one sample is stored, measured in a file rather
// than in bytes of content, because pages and not strings are what a b-tree pays

type identityLayout struct {
	name    string
	create  []string
	insert  func(tx *sql.Tx, id int64, labels map[string]string, canonical string, dictionary map[string]int64) error
	restore func(db *sql.DB, id int64, dictionary map[string]int64) (string, error)
}

func labelIDs(labels map[string]string, dictionary map[string]int64) []int64 {
	ids := make([]int64, 0, len(labels))
	for name, value := range labels {
		ids = append(ids, dictionary[name+"\x00"+value])
	}
	slices.Sort(ids)
	return ids
}

func gapCoded(ids []int64) []byte {
	out := binary.AppendUvarint(nil, uint64(len(ids)))
	previous := int64(0)
	for _, id := range ids {
		out = binary.AppendUvarint(out, uint64(id-previous))
		previous = id
	}
	return out
}

func gapDecoded(blob []byte) ([]int64, error) {
	count, read := binary.Uvarint(blob)
	if read <= 0 {
		return nil, fmt.Errorf("label count")
	}
	blob = blob[read:]
	ids := make([]int64, 0, count)
	previous := int64(0)
	for range count {
		gap, n := binary.Uvarint(blob)
		if n <= 0 {
			return nil, fmt.Errorf("label id")
		}
		blob = blob[n:]
		previous += int64(gap)
		ids = append(ids, previous)
	}
	return ids, nil
}

func openSpikeDB(t *testing.T, path string, create []string) *sql.DB {
	t.Helper()
	db, err := sql.Open("sqlite", "file:"+path+"?_dqs=0&_pragma=busy_timeout(5000)&_txlock=immediate")
	if err != nil {
		t.Fatal(err)
	}
	db.SetMaxOpenConns(1)
	db.SetConnMaxLifetime(0)
	for _, statement := range append([]string{"pragma journal_mode=WAL"}, create...) {
		if _, err = db.Exec(statement); err != nil {
			t.Fatalf("%s: %v", statement, err)
		}
	}
	return db
}

// TestIdentityLayouts measures what the registry would weigh if a series kept
// its labels as the dictionary entries it already has instead of as text
func TestIdentityLayouts(t *testing.T) {
	if os.Getenv("TINYSTORE_SPIKE") == "" {
		t.Skip("set TINYSTORE_SPIKE=1 to measure")
	}
	series := readJSONLCorpus(t)
	dictionary := map[string]int64{}
	pairs := []string{}
	for _, s := range series {
		for name, value := range s.Metric {
			key := name + "\x00" + value
			if _, seen := dictionary[key]; !seen {
				dictionary[key] = int64(len(dictionary) + 1)
				pairs = append(pairs, key)
			}
		}
	}
	samples := 0
	for _, s := range series {
		samples += len(s.Values)
	}

	layouts := []identityLayout{
		{
			name: "labels as canonical text, digest as base64 text",
			create: []string{
				`create table series(id integer primary key, identity text not null unique, labels text not null, kind text not null) strict`,
			},
			insert: func(tx *sql.Tx, id int64, _ map[string]string, canonical string, _ map[string]int64) error {
				_, err := tx.Exec(`insert into series values(?,?,?,?)`, id, digestText(canonical), canonical, "gauge")
				return err
			},
			restore: func(db *sql.DB, id int64, _ map[string]int64) (string, error) {
				var labels string
				return labels, db.QueryRow(`select labels from series where id=?`, id).Scan(&labels)
			},
		},
		{
			name: "labels as dictionary ids, digest as base64 text",
			create: []string{
				`create table series(id integer primary key, identity text not null unique, labels blob not null, kind integer not null) strict`,
			},
			insert: func(tx *sql.Tx, id int64, labels map[string]string, canonical string, dictionary map[string]int64) error {
				_, err := tx.Exec(`insert into series values(?,?,?,?)`, id, digestText(canonical), gapCoded(labelIDs(labels, dictionary)), 0)
				return err
			},
			restore: restoreFromIDs,
		},
		{
			name: "labels as dictionary ids, digest as a blob",
			create: []string{
				`create table series(id integer primary key, identity blob not null unique, labels blob not null, kind integer not null) strict`,
			},
			insert: func(tx *sql.Tx, id int64, labels map[string]string, canonical string, dictionary map[string]int64) error {
				_, err := tx.Exec(`insert into series values(?,?,?,?)`, id, digestBytes(canonical), gapCoded(labelIDs(labels, dictionary)), 0)
				return err
			},
			restore: restoreFromIDs,
		},
		{
			name: "labels as dictionary ids, digest truncated to 16 bytes",
			create: []string{
				`create table series(id integer primary key, identity blob not null unique, labels blob not null, kind integer not null) strict`,
			},
			insert: func(tx *sql.Tx, id int64, labels map[string]string, canonical string, dictionary map[string]int64) error {
				_, err := tx.Exec(`insert into series values(?,?,?,?)`, id, digestBytes(canonical)[:16], gapCoded(labelIDs(labels, dictionary)), 0)
				return err
			},
			restore: restoreFromIDs,
		},
	}

	for _, layout := range layouts {
		create := append([]string{
			`create table label_values(id integer primary key, name text not null, value text not null, unique(name, value)) strict`,
		}, layout.create...)
		db := openSpikeDB(t, filepath.Join(t.TempDir(), "identity.db"), create)
		tx, err := db.Begin()
		if err != nil {
			t.Fatal(err)
		}
		for key, id := range dictionary {
			name, value := splitPair(key)
			if _, err = tx.Exec(`insert into label_values values(?,?,?)`, id, name, value); err != nil {
				t.Fatal(err)
			}
		}
		for i, s := range series {
			if err = layout.insert(tx, int64(i+1), s.Metric, string(canonicalLabels(s.Metric)), dictionary); err != nil {
				t.Fatal(err)
			}
		}
		if err = tx.Commit(); err != nil {
			t.Fatal(err)
		}
		if _, err = db.Exec(`pragma wal_checkpoint(truncate)`); err != nil {
			t.Fatal(err)
		}
		// the labels must come back exactly, or the identity invariant is gone
		for i, s := range series {
			got, restoreErr := layout.restore(db, int64(i+1), dictionary)
			if restoreErr != nil {
				t.Fatal(restoreErr)
			}
			if want := string(canonicalLabels(s.Metric)); got != want {
				t.Fatalf("%s: labels came back as %s, not %s", layout.name, got, want)
			}
		}
		total, parts := objectSizes(t, db)
		t.Logf("%-46s file=%8d B  %7.1f B/series  %.4f B/sample", layout.name, total,
			float64(total)/float64(len(series)), float64(total)/float64(samples))
		t.Logf("   %s", parts)
		db.Close()
	}
	t.Logf("%d series, %d distinct label pairs, %d samples", len(series), len(pairs), samples)
}

// objectSizes divides the file the only way that means anything: per b-tree
func objectSizes(t *testing.T, db *sql.DB) (int64, string) {
	t.Helper()
	rows, err := db.Query(`select name, sum(pgsize) from dbstat group by name order by 2 desc`)
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()
	var total int64
	parts := ""
	for rows.Next() {
		var name string
		var size int64
		if err = rows.Scan(&name, &size); err != nil {
			t.Fatal(err)
		}
		total += size
		parts += fmt.Sprintf("%s=%d ", name, size)
	}
	if err = rows.Err(); err != nil {
		t.Fatal(err)
	}
	return total, parts
}

func restoreFromIDs(db *sql.DB, id int64, _ map[string]int64) (string, error) {
	var blob []byte
	if err := db.QueryRow(`select labels from series where id=?`, id).Scan(&blob); err != nil {
		return "", err
	}
	ids, err := gapDecoded(blob)
	if err != nil {
		return "", err
	}
	labels := map[string]string{}
	for _, labelID := range ids {
		var name, value string
		if err = db.QueryRow(`select name, value from label_values where id=?`, labelID).Scan(&name, &value); err != nil {
			return "", err
		}
		labels[name] = value
	}
	return string(canonicalLabels(labels)), nil
}

func splitPair(key string) (string, string) {
	for i := range len(key) {
		if key[i] == 0 {
			return key[:i], key[i+1:]
		}
	}
	return key, ""
}

func digestText(canonical string) string {
	return "@" + base64Digest(canonical)
}

func digestBytes(canonical string) []byte {
	sum := sha256Of(canonical)
	return sum[:]
}

func base64Digest(canonical string) string {
	sum := sha256.Sum256([]byte(canonical))
	return base64.RawURLEncoding.EncodeToString(sum[:])
}

func sha256Of(canonical string) [32]byte { return sha256.Sum256([]byte(canonical)) }

var _ = json.Marshal
