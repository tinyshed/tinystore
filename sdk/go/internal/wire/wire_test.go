package wire

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"go/format"
	"math"
	"os"
	"reflect"
	"strconv"
	"strings"
	"testing"
)

// typed is a value in the vectors' notation: {"uint": "207"}, {"str": "a"},
// {"list": [...]}, {"fields": {...}} and the rest.
type typed map[string]json.RawMessage

func load(t *testing.T, name string, into any) {
	t.Helper()
	bytes, err := os.ReadFile("../../../../testdata/wire/" + name)
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(bytes, into); err != nil {
		t.Fatal(err)
	}
}

// TestEveryMessageOfTheSchemaWritesItsVectorAndReadsItBack reads every
// message of protocol.json, which crates/protocol writes from the schema
// beside this codec: each vector's fields write its bytes, and its bytes
// read back to what writes them again.
func TestEveryMessageOfTheSchemaWritesItsVectorAndReadsItBack(t *testing.T) {
	var vectors struct {
		Methods  map[string]uint16
		Messages []struct {
			Name, Message, Hex string
			Fields             map[string]typed
		}
	}
	load(t, "protocol.json", &vectors)
	if !reflect.DeepEqual(Methods(), vectors.Methods) {
		t.Fatalf("the methods are not the schema's:\n%v\n%v", Methods(), vectors.Methods)
	}
	for _, vector := range vectors.Messages {
		t.Run(vector.Name, func(t *testing.T) {
			message := NewMessage(vector.Message)
			if message == nil {
				t.Fatalf("no message %s", vector.Message)
			}
			setFields(t, reflect.ValueOf(message).Elem(), vector.Fields)
			if got := hex.EncodeToString(Encode(message)); got != vector.Hex {
				t.Fatalf("written %s, the vector %s", got, vector.Hex)
			}
			body, _ := hex.DecodeString(vector.Hex)
			read := NewMessage(vector.Message)
			if err := Decode(body, read); err != nil {
				t.Fatal(err)
			}
			if got := hex.EncodeToString(Encode(read)); got != vector.Hex {
				t.Fatalf("read back and written %s, the vector %s", got, vector.Hex)
			}
		})
	}
}

// setFields gives a message's struct the vector's fields, each found by the
// schema's name in its `wire` tag.
func setFields(t *testing.T, message reflect.Value, fields map[string]typed) {
	t.Helper()
	byName := map[string]reflect.Value{}
	for i := 0; i < message.NumField(); i++ {
		tag := message.Type().Field(i).Tag.Get("wire")
		byName[tag[strings.IndexByte(tag, ',')+1:]] = message.Field(i)
	}
	for name, value := range fields {
		field, ok := byName[name]
		if !ok {
			t.Fatalf("%s has no field %s", message.Type(), name)
		}
		set(t, field, value)
	}
}

// set gives a place of the struct the value a vector spells, through a
// pointer where the field is optional.
func set(t *testing.T, place reflect.Value, value typed) {
	t.Helper()
	if place.Kind() == reflect.Pointer {
		place.Set(reflect.New(place.Type().Elem()))
		place = place.Elem()
	}
	for kind, raw := range value {
		switch kind {
		case "fields":
			var fields map[string]typed
			mustRead(t, raw, &fields)
			setFields(t, place, fields)
		case "list":
			var items []typed
			mustRead(t, raw, &items)
			list := reflect.MakeSlice(place.Type(), len(items), len(items))
			for i, item := range items {
				set(t, list.Index(i), item)
			}
			place.Set(list)
		case "names":
			var named map[string]typed
			mustRead(t, raw, &named)
			names := reflect.MakeMap(place.Type())
			for name, item := range named {
				element := reflect.New(place.Type().Elem()).Elem()
				set(t, element, item)
				names.SetMapIndex(reflect.ValueOf(name), element)
			}
			place.Set(names)
		default:
			place.Set(reflect.ValueOf(scalar(t, kind, raw)).Convert(place.Type()))
		}
	}
}

// scalar is a value the vectors spell by its type: integers as int64 or
// uint64 as they fit, a float by its bits, bytes by their hex.
func scalar(t *testing.T, kind string, raw json.RawMessage) any {
	t.Helper()
	var text string
	switch kind {
	case "bool":
		var v bool
		mustRead(t, raw, &v)
		return v
	case "uint":
		mustRead(t, raw, &text)
		v, err := strconv.ParseUint(text, 10, 64)
		if err != nil {
			t.Fatal(err)
		}
		if v <= math.MaxInt64 {
			return int64(v)
		}
		return v
	case "int":
		mustRead(t, raw, &text)
		v, err := strconv.ParseInt(text, 10, 64)
		if err != nil {
			t.Fatal(err)
		}
		return v
	case "float":
		mustRead(t, raw, &text)
		bits, err := strconv.ParseUint(text, 16, 64)
		if err != nil {
			t.Fatal(err)
		}
		return math.Float64frombits(bits)
	case "str":
		mustRead(t, raw, &text)
		return text
	case "bin":
		mustRead(t, raw, &text)
		v, err := hex.DecodeString(text)
		if err != nil {
			t.Fatal(err)
		}
		return v
	}
	t.Fatalf("no kind %s", kind)
	return nil
}

func mustRead(t *testing.T, raw json.RawMessage, into any) {
	t.Helper()
	if err := json.Unmarshal(raw, into); err != nil {
		t.Fatal(err)
	}
}

// TestTheProfileReadsEveryValueAndRefusesWhatItRefuses reads vectors.json's
// values and refusals, which every SDK and the server share.
func TestTheProfileReadsEveryValueAndRefusesWhatItRefuses(t *testing.T) {
	var vectors struct {
		Values []struct {
			Name, Hex string
			Value     json.RawMessage
		}
		Refused []struct{ Name, Hex, As string }
	}
	load(t, "vectors.json", &vectors)
	for _, vector := range vectors.Values {
		t.Run(vector.Name, func(t *testing.T) {
			body, _ := hex.DecodeString(vector.Hex)
			r := NewReader(body)
			got := readAny(r, decodeExpected(t, vector.Value))
			if err := r.End(); err != nil {
				t.Fatal(err)
			}
			var want any
			mustRead(t, vector.Value, &want)
			if !reflect.DeepEqual(got, want) {
				t.Fatalf("read %v, the vector %v", got, want)
			}
		})
	}
	for _, vector := range vectors.Refused {
		t.Run(vector.Name, func(t *testing.T) {
			body, _ := hex.DecodeString(vector.Hex)
			r := NewReader(body)
			switch vector.As {
			case "uint":
				r.Uint()
			case "int":
				r.Int()
			case "float":
				r.Float()
			case "str":
				r.Str()
			case "bin":
				r.Bin()
			case "map":
				var seen Seen
				for n := r.Map(); n > 0; n-- {
					r.Field(&seen)
					r.Skip()
				}
				r.Leave()
			default:
				r.Skip()
			}
			if r.End() == nil {
				t.Fatal("taken")
			}
		})
	}
}

// decodeExpected is the vector's value as plain JSON, which says what a
// float is where the bytes hold an integer, as JavaScript writes 3.0.
func decodeExpected(t *testing.T, raw json.RawMessage) any {
	var value any
	mustRead(t, raw, &value)
	return value
}

// readAny reads a value into the vectors' notation, guided by what the
// vector expects where the bytes alone cannot say: a float written as an
// integer.
func readAny(r *Reader, want any) any {
	switch r.Type() {
	case TypeNil:
		r.Nil()
		return nil
	case TypeBool:
		return r.Bool()
	case TypeInteger:
		if spelled, ok := want.(map[string]any); ok {
			if _, isFloat := spelled["float"]; isFloat {
				return map[string]any{"float": strconv.FormatUint(math.Float64bits(r.Float()), 16)}
			}
		}
		v, negative := r.integer("an integer")
		if negative {
			return map[string]any{"int": strconv.FormatInt(int64(v), 10)}
		}
		return map[string]any{"uint": strconv.FormatUint(v, 10)}
	case TypeFloat:
		return map[string]any{"float": strconv.FormatUint(math.Float64bits(r.Float()), 16)}
	case TypeStr:
		return map[string]any{"str": r.Str()}
	case TypeBin:
		return map[string]any{"bin": hex.EncodeToString(r.Bin())}
	case TypeArray:
		items := []any{}
		wanted, _ := want.(map[string]any)
		wantedItems, _ := wanted["array"].([]any)
		for i, n := 0, r.Array(); i < n; i++ {
			var each any
			if i < len(wantedItems) {
				each = wantedItems[i]
			}
			items = append(items, readAny(r, each))
		}
		r.Leave()
		return map[string]any{"array": items}
	case TypeMap:
		pairs := []any{}
		for n := r.Map(); n > 0; n-- {
			key := readAny(r, nil)
			pairs = append(pairs, []any{key, readAny(r, nil)})
		}
		r.Leave()
		return map[string]any{"map": pairs}
	}
	r.Skip()
	return nil
}

// TestTheCodecIsLaidOutAsGofmtLaysItOut holds crates/protocol's Go to gofmt,
// which nothing runs after it.
func TestTheCodecIsLaidOutAsGofmtLaysItOut(t *testing.T) {
	for _, file := range []string{"protocol.go", "msgpack.go", "codec.go", "wire_test.go"} {
		source, err := os.ReadFile(file)
		if err != nil {
			t.Fatal(err)
		}
		formatted, err := format.Source(source)
		if err != nil {
			t.Fatalf("%s: %v", file, err)
		}
		if !bytes.Equal(source, formatted) {
			t.Errorf("%s is not laid out as gofmt lays it out", file)
		}
	}
}

// TestAWrittenReaderRefusesAKeyTwice is the profile's rule in a message's
// own reader.
func TestAWrittenReaderRefusesAKeyTwice(t *testing.T) {
	body, _ := hex.DecodeString("8201010102")
	var branch KvBranch
	if err := Decode(body, &branch); err == nil || !strings.Contains(err.Error(), "twice") {
		t.Fatalf("taken: %v", err)
	}
}

// TestAMapPastFifteenFieldsWidensItsHead writes a map whose count is known
// only once its sixteen entries are.
func TestAMapPastFifteenFieldsWidensItsHead(t *testing.T) {
	var w Writer
	head := w.OpenMap()
	for key := uint64(1); key <= 16; key++ {
		w.Uint(key)
		w.Bool(true)
	}
	w.CloseMap(head, 16)
	if got := hex.EncodeToString(w.Bytes()[:3]); got != "de0010" {
		t.Fatalf("head %s", got)
	}
	r := NewReader(w.Bytes())
	r.Skip()
	if err := r.End(); err != nil {
		t.Fatal(err)
	}
}
