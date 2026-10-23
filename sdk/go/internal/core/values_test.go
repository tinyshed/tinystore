package core

import (
	"encoding/hex"
	"encoding/json"
	"math"
	"os"
	"reflect"
	"strings"
	"testing"
)

// A kv value's row as every language writes it (testdata/kv/values.json).
type valueVectors struct {
	Values []struct {
		Kind  string          `json:"kind"`
		Text  string          `json:"text"`
		Bytes string          `json:"bytes"`
		JSON  json.RawMessage `json:"json"`
		Float float64         `json:"float"`
		U64   uint64          `json:"u64"`
		F32   float32         `json:"f32"`
		Row   string          `json:"row"`
	} `json:"values"`
	Refused []struct {
		ReadAs string `json:"read_as"`
		Row    string `json:"row"`
		Says   string `json:"says"`
	} `json:"refused"`
}

func loadValueVectors(t *testing.T) valueVectors {
	text, err := os.ReadFile("../../../../testdata/kv/values.json")
	if err != nil {
		t.Fatal(err)
	}
	var vectors valueVectors
	if err := json.Unmarshal(text, &vectors); err != nil {
		t.Fatal(err)
	}
	return vectors
}

func unhex(t *testing.T, text string) []byte {
	bytes, err := hex.DecodeString(text)
	if err != nil {
		t.Fatal(err)
	}
	return bytes
}

func roundTrip[T any](t *testing.T, values Values[T], given T, row string) {
	t.Helper()
	written, err := values.Encode(given)
	if err != nil {
		t.Fatal(err)
	}
	if got := hex.EncodeToString(written.([]byte)); got != row {
		t.Fatalf("%v is written %s, not %s", given, got, row)
	}
	read, err := values.Decode(written)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(read, given) {
		t.Fatalf("%v reads back as %v", given, read)
	}
}

func TestEveryLanguageWritesAndReadsTheSameRows(t *testing.T) {
	vectors := loadValueVectors(t)
	for _, value := range vectors.Values {
		switch value.Kind {
		case "text":
			roundTrip(t, ValuesOf[string](), value.Text, value.Row)
		case "bytes":
			roundTrip(t, ValuesOf[[]byte](), unhex(t, value.Bytes), value.Row)
		case "json":
			var given any
			if err := json.Unmarshal(value.JSON, &given); err != nil {
				t.Fatal(err)
			}
			roundTrip(t, ValuesOf[any](), given, value.Row)
		case "float":
			roundTrip(t, ValuesOf[float64](), value.Float, value.Row)
		case "u64":
			roundTrip(t, ValuesOf[uint64](), value.U64, value.Row)
		case "f32":
			roundTrip(t, ValuesOf[float32](), value.F32, value.Row)
		default:
			t.Fatalf("a kind %s", value.Kind)
		}
	}
}

func TestARowReadAsAnotherKindThanWroteItIsCorrupt(t *testing.T) {
	vectors := loadValueVectors(t)
	for _, refused := range vectors.Refused {
		row := unhex(t, refused.Row)
		var err error
		switch refused.ReadAs {
		case "text":
			_, err = ValuesOf[string]().Decode(row)
		case "json":
			_, err = ValuesOf[map[string]int]().Decode(row)
		case "bytes":
			_, err = ValuesOf[[]byte]().Decode(row)
		case "float":
			_, err = ValuesOf[float64]().Decode(row)
		case "f32":
			_, err = ValuesOf[float32]().Decode(row)
		default:
			t.Fatalf("a kind %s", refused.ReadAs)
		}
		if err == nil || !strings.Contains(err.Error(), refused.Says) {
			t.Fatalf("%s read as %s: %v, not %q", refused.Row, refused.ReadAs, err, refused.Says)
		}
	}
}

// contract: KV-01
func TestAUint64AndAFloat32AreKeptBitForBitAsRustKeepsAU64AndAnF32(t *testing.T) {
	roundTrip(t, ValuesOf[uint](), uint(1)<<63, "058000000000000000")
	roundTrip(t, ValuesOf[uint64](), math.MaxUint64, "05ffffffffffffffff")
	// a signalling NaN keeps its bits, which a float64 between would quiet
	signalling := math.Float32frombits(0x7fa00001)
	written, err := ValuesOf[float32]().Encode(signalling)
	if err != nil || hex.EncodeToString(written.([]byte)) != "067fa00001" {
		t.Fatalf("a signalling NaN is written %x, %v", written, err)
	}
	read, err := ValuesOf[float32]().Decode(written)
	if err != nil || math.Float32bits(read) != 0x7fa00001 {
		t.Fatalf("a signalling NaN reads back %x, %v", math.Float32bits(read), err)
	}
	type celsius float32
	roundTrip(t, ValuesOf[celsius](), celsius(math.Copysign(0, -1)), "0680000000")
	// a uint32, as Rust's u32, is an integer
	if written, err := ValuesOf[uint32]().Encode(math.MaxUint32); err != nil || written != int64(math.MaxUint32) {
		t.Fatalf("the largest uint32 is written %v, %v", written, err)
	}
}

func TestARowAnOlderReleaseWroteForAUint64OrAFloat32StillReadsWhenItHoldsTheValue(t *testing.T) {
	if n, err := ValuesOf[uint64]().Decode(int64(5)); err != nil || n != 5 {
		t.Fatalf("the integer 5 as a uint64: %d, %v", n, err)
	}
	if _, err := ValuesOf[uint64]().Decode(int64(-1)); KindOf(err) != Corrupt {
		t.Fatalf("the integer -1 as a uint64: %v", err)
	}
	if f, err := ValuesOf[float32]().Decode(unhex(t, "043ff8000000000000")); err != nil || f != 1.5 {
		t.Fatalf("the float64 1.5 as a float32: %v, %v", f, err)
	}
	// 0.1 as a float64 is no float32's value
	if _, err := ValuesOf[float32]().Decode(unhex(t, "043fb999999999999a")); KindOf(err) != Corrupt {
		t.Fatalf("the float64 0.1 as a float32: %v", err)
	}
}

// contract: KV-01
func TestAnIntegerASmallerIntegerTypeDoesNotHoldIsCorrupt(t *testing.T) {
	if _, err := ValuesOf[uint8]().Decode(int64(300)); KindOf(err) != Corrupt {
		t.Fatalf("300 as a uint8: %v", err)
	}
	if _, err := ValuesOf[uint16]().Decode(int64(-1)); KindOf(err) != Corrupt {
		t.Fatalf("-1 as a uint16: %v", err)
	}
	if _, err := ValuesOf[int8]().Decode(int64(200)); KindOf(err) != Corrupt {
		t.Fatalf("200 as an int8: %v", err)
	}
	if n, err := ValuesOf[int32]().Decode(int64(-7)); err != nil || n != -7 {
		t.Fatalf("-7 as an int32: %d, %v", n, err)
	}
}
