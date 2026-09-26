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

	want := session{UserID: 42, Device: "iPhone", Tags: []string{"a", "b"}}
	got := roundTrip(t, openTestBucket[session](t, state, "json"), want)
	if got.UserID != 42 || got.Device != "iPhone" || strings.Join(got.Tags, ",") != "a,b" {
		t.Fatalf("a struct: %+v", got)
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

// bigEndian is a codec that writes an int64 as eight bytes, big-endian
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

// valueType is the SQLite type of a key's value in its row, at the bucket's root
func (s *testState) valueType(t *testing.T, bucket, key string) string {
	t.Helper()
	var typed string
	err := s.file.View(t.Context(), func(tx *sql.Tx) error {
		return tx.QueryRowContext(t.Context(), `select typeof(c.value) from cells as c join buckets as b on b.id = c.bucket
			where b.name = ?1 and c.path = ?2`, bucket, appendKey(nil, key)).Scan(&typed)
	})
	if err != nil {
		t.Fatal(err)
	}
	return typed
}
