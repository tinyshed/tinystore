package kv

import (
	"bytes"
	"database/sql"
	"encoding/binary"
	"errors"
	"math"
	"strings"
	"testing"

	"github.com/tinyshed/tinystore"
)

type celsius float64

type session struct {
	UserID int64
	Device string
	Tags   []string
}

func roundTrip[V any](t *testing.T, bucket *Bucket[V], value V) V {
	t.Helper()
	if err := bucket.Set(t.Context(), "k", value); err != nil {
		t.Fatal(err)
	}
	got, found, err := bucket.Get(t.Context(), "k")
	if err != nil || !found {
		t.Fatalf("%v, %v", found, err)
	}
	return got
}

// a value comes back as it went in, by its type; a float keeps its bits,
// -0 and a NaN's payload included
func TestAValueComesBackAsItWentIn(t *testing.T) {
	state := openTestState(t, t.TempDir())

	if got := roundTrip(t, openTestBucket[[]byte](t, state, "bytes"), []byte{0, 1, 0xff}); !bytes.Equal(got, []byte{0, 1, 0xff}) {
		t.Fatalf("bytes: %v", got)
	}
	if got := roundTrip(t, openTestBucket[label](t, state, "label"), label("iPad / Pro")); got != "iPad / Pro" {
		t.Fatalf("a named string: %q", got)
	}
	if got := roundTrip(t, openTestBucket[bool](t, state, "bool"), true); !got {
		t.Fatal("a bool")
	}
	if got := roundTrip(t, openTestBucket[int64](t, state, "int64"), int64(math.MinInt64)); got != math.MinInt64 {
		t.Fatalf("an int64: %d", got)
	}
	if got := roundTrip(t, openTestBucket[uint64](t, state, "uint64"), uint64(math.MaxUint64)); got != math.MaxUint64 {
		t.Fatalf("a uint64: %d", got)
	}
	if got := roundTrip(t, openTestBucket[userID](t, state, "named"), userID(-7)); got != -7 {
		t.Fatalf("a named integer: %d", got)
	}

	payload := math.Float64frombits(0x7ff8_0000_dead_beef)
	for _, value := range []float64{math.Copysign(0, -1), payload, math.Inf(-1), 12.02} {
		got := roundTrip(t, openTestBucket[float64](t, state, "float64"), value)
		if math.Float64bits(got) != math.Float64bits(value) {
			t.Fatalf("float64 bits %x came back as %x", math.Float64bits(value), math.Float64bits(got))
		}
	}
	if got := roundTrip(t, openTestBucket[float32](t, state, "float32"), float32(-0.1)); got != -0.1 {
		t.Fatalf("a float32: %v", got)
	}
	if got := roundTrip(t, openTestBucket[celsius](t, state, "celsius"), celsius(36.6)); got != 36.6 {
		t.Fatalf("a named float: %v", got)
	}
	expectBits32(t, openTestBucket[float32](t, state, "float32-bits"))
	expectBits32(t, openTestBucket[number](t, state, "number"))
	roundTrip(t, openTestBucket[number](t, state, "number"), number(math.Float32frombits(0x7f80_0001)))
	if row := state.valueBytes(t, "number", "k"); !bytes.Equal(row, []byte{0x7f, 0x80, 0x00, 0x01}) {
		t.Fatalf("a float32 NaN that signals is kept as %x", row)
	}
	expectBits64(t, openTestBucket[float64](t, state, "float64-bits"))
	expectBits64(t, openTestBucket[celsius](t, state, "celsius-bits"))

	want := session{UserID: 42, Device: "iPhone", Tags: []string{"a", "b"}}
	got := roundTrip(t, openTestBucket[session](t, state, "json"), want)
	if got.UserID != 42 || got.Device != "iPhone" || strings.Join(got.Tags, ",") != "a,b" {
		t.Fatalf("a struct: %+v", got)
	}
}

type number float32

// the float32 bits a value keeps: a NaN that signals and one that does not,
// each with a payload, -0 and the largest finite
var floats32 = []uint32{0x7f80_0001, 0xffc0_beef, 0x8000_0000, 0x7f7f_ffff}

// the float64 bits: a NaN that signals, one that does not, -0, -Inf
var floats64 = []uint64{0x7ff0_0000_0000_0001, 0xfff8_0000_dead_beef, 0x8000_0000_0000_0000, 0xfff0_0000_0000_0000}

// expectBits32 writes each of floats32 through the bucket, named type or not,
// and finds its bits in the value that comes back and in the row
func expectBits32[V ~float32](t *testing.T, bucket *Bucket[V]) {
	t.Helper()
	for _, bits := range floats32 {
		got := roundTrip(t, bucket, V(math.Float32frombits(bits)))
		if back := math.Float32bits(float32(got)); back != bits {
			t.Fatalf("%T bits %08x came back as %08x", got, bits, back)
		}
	}
}

func expectBits64[V ~float64](t *testing.T, bucket *Bucket[V]) {
	t.Helper()
	for _, bits := range floats64 {
		got := roundTrip(t, bucket, V(math.Float64frombits(bits)))
		if back := math.Float64bits(float64(got)); back != bits {
			t.Fatalf("%T bits %016x came back as %016x", got, bits, back)
		}
	}
}

// a bucket of struct{} is a set, and its rows keep no value
func TestASetKeepsNoValue(t *testing.T) {
	state := openTestState(t, t.TempDir())
	revoked := openTestBucket[struct{}](t, state, "revoked")
	if err := revoked.Set(t.Context(), "jti", struct{}{}); err != nil {
		t.Fatal(err)
	}
	if found, err := revoked.Has(t.Context(), "jti"); err != nil || !found {
		t.Fatalf("a member: %v, %v", found, err)
	}
	if typed := state.valueType(t, "revoked", "jti"); typed != "null" {
		t.Fatalf("a member's value is %s", typed)
	}
}

func TestAValueJSONCannotWriteIsRefused(t *testing.T) {
	state := openTestState(t, t.TempDir())
	bucket := openTestBucket[map[string]float64](t, state, "json")
	err := bucket.Set(t.Context(), "k", map[string]float64{"x": math.NaN()})
	if !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a NaN in JSON: %v", err)
	}
}

// A bucket of Raw reads what a bucket of each type wrote as its row holds it,
// an empty string as empty bytes and not as nothing. What it writes reads in
// the bucket of the type it came from.
func TestARawValueIsWhatItsRowHolds(t *testing.T) {
	state := openTestState(t, t.TempDir())
	raw := openTestBucket[Raw](t, state, "shared")
	float := math.Float64frombits(0x7ff0_0000_0000_0001)

	written := []struct {
		key  string
		set  func() error
		want Raw
	}{
		{
			"text", func() error { return openTestBucket[string](t, state, "shared").Set(t.Context(), "text", "héllo") },
			Raw{Kind: RawBytes, Bytes: []byte("héllo")},
		},
		{
			"empty", func() error { return openTestBucket[string](t, state, "shared").Set(t.Context(), "empty", "") },
			Raw{Kind: RawBytes, Bytes: []byte{}},
		},
		{
			"int", func() error { return openTestBucket[int64](t, state, "shared").Set(t.Context(), "int", -5) },
			Raw{Kind: RawInt, Int: -5},
		},
		{
			"bool", func() error { return openTestBucket[bool](t, state, "shared").Set(t.Context(), "bool", true) },
			Raw{Kind: RawInt, Int: 1},
		},
		{
			"float", func() error { return openTestBucket[float64](t, state, "shared").Set(t.Context(), "float", float) },
			Raw{Kind: RawBytes, Bytes: []byte{0x7f, 0xf0, 0, 0, 0, 0, 0, 1}},
		},
		{
			"member", func() error {
				return openTestBucket[struct{}](t, state, "shared").Set(t.Context(), "member", struct{}{})
			},
			Raw{},
		},
		{"json", func() error {
			return openTestBucket[session](t, state, "shared").Set(t.Context(), "json", session{UserID: 7})
		}, Raw{Kind: RawBytes, Bytes: []byte(`{"UserID":7,"Device":"","Tags":null}`)}},
	}
	for _, w := range written {
		if err := w.set(); err != nil {
			t.Fatal(err)
		}
		got, found, err := raw.Get(t.Context(), w.key)
		if err != nil || !found || got.Kind != w.want.Kind || got.Int != w.want.Int || !bytes.Equal(got.Bytes, w.want.Bytes) ||
			(got.Kind == RawBytes) != (got.Bytes != nil) {
			t.Errorf("%s read as %+v, %v, %v; want %+v", w.key, got, found, err, w.want)
		}
	}

	if err := raw.Set(t.Context(), "seven", Raw{Kind: RawInt, Int: 7}); err != nil {
		t.Fatal(err)
	}
	if n, _, err := openTestBucket[int64](t, state, "shared").Get(t.Context(), "seven"); n != 7 || err != nil {
		t.Errorf("an integer Raw read as %d, %v", n, err)
	}
	if err := raw.Set(t.Context(), "nothing", Raw{}); err != nil {
		t.Fatal(err)
	}
	if typed := state.valueType(t, "shared", "nothing"); typed != "null" {
		t.Errorf("nothing is kept as %s", typed)
	}
	if err := raw.Set(t.Context(), "none", Raw{Kind: RawBytes}); err != nil {
		t.Fatal(err)
	}
	if typed := state.valueType(t, "shared", "none"); typed != "blob" {
		t.Errorf("empty bytes are kept as %s", typed)
	}
	if err := raw.Set(t.Context(), "odd", Raw{Kind: 9}); !errors.Is(err, tinystore.ErrInvalid) {
		t.Errorf("a Raw of no kind: %v", err)
	}
}

type bigEndian struct{}

func (bigEndian) Encode(n int64) ([]byte, error) {
	return binary.BigEndian.AppendUint64(nil, uint64(n)), nil
}

func (bigEndian) Decode(b []byte) (int64, error) {
	if len(b) != 8 {
		return 0, errors.New("not eight bytes")
	}
	return int64(binary.BigEndian.Uint64(b)), nil
}

func TestABucketWritesThroughItsCodec(t *testing.T) {
	state := openTestState(t, t.TempDir())
	bucket := openTestBucket[int64](t, state, "coded", WithCodec[int64](bigEndian{}))
	if got := roundTrip(t, bucket, 1<<40); got != 1<<40 {
		t.Fatalf("through the codec: %d", got)
	}
	if typed := state.valueType(t, "coded", "k"); typed != "blob" {
		t.Fatalf("a coded value is %s", typed)
	}
	if _, err := OpenBucket[string](t.Context(), state.Store, "wrong", WithCodec[int64](bigEndian{})); !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a codec of another type: %v", err)
	}
}

// valueBytes is a key's value as its row keeps it, at the bucket's root
func (s *testState) valueBytes(t *testing.T, bucket, key string) []byte {
	t.Helper()
	var kept []byte
	err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select c.value from _tinystore_kv_cells as c join _tinystore_kv_buckets as b on b.id = c.bucket
			where b.name = ?1 and c.path = ?2`, bucket, appendKey(nil, key)).Scan(&kept)
	})
	if err != nil {
		t.Fatal(err)
	}
	return kept
}

// valueType is the SQLite type of a key's value in its row, at the bucket's root
func (s *testState) valueType(t *testing.T, bucket, key string) string {
	t.Helper()
	var typed string
	err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select typeof(c.value) from _tinystore_kv_cells as c join _tinystore_kv_buckets as b on b.id = c.bucket
			where b.name = ?1 and c.path = ?2`, bucket, appendKey(nil, key)).Scan(&typed)
	})
	if err != nil {
		t.Fatal(err)
	}
	return typed
}
