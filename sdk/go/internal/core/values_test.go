package core

import (
	"encoding/hex"
	"encoding/json"
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
		}
		if err == nil || !strings.Contains(err.Error(), refused.Says) {
			t.Fatalf("%s read as %s: %v, not %q", refused.Row, refused.ReadAs, err, refused.Says)
		}
	}
}
