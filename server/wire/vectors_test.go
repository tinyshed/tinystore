package wire_test

import (
	"bytes"
	"cmp"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"os"
	"reflect"
	"slices"
	"strconv"
	"testing"

	"github.com/tinyshed/tinystore/server/wire"
)

// vectors is testdata/vectors.json, which every SDK is tested against too
type vectors struct {
	Values []struct {
		Name, Hex, Canonical string
		Value                any
	}
	Refused []struct {
		Name, Hex, As string
	}
	Frames []struct {
		Name, Hex, Raw string
		Header         struct{ Length, Kind, Flags, Method, Stream uint32 }
		Body           any
	}
	RefusedFrames []struct {
		Name, Hex string
		MaxBody   uint32 `json:"max body"`
	} `json:"refused frames"`
	Proofs []struct {
		Name, Secret, Challenge, Proof string
	}
}

func readVectors(t testing.TB) vectors {
	t.Helper()
	text, err := os.ReadFile("testdata/vectors.json")
	if err != nil {
		t.Fatal(err)
	}
	var v vectors
	if err := json.Unmarshal(text, &v); err != nil {
		t.Fatal(err)
	}
	return v
}

func unhex(t testing.TB, text string) []byte {
	t.Helper()
	b, err := hex.DecodeString(text)
	if err != nil {
		t.Fatal(err)
	}
	return b
}

func TestVectors(t *testing.T) {
	v := readVectors(t)
	for _, vector := range v.Values {
		t.Run(vector.Name, func(t *testing.T) {
			d := wire.NewDecoder(unhex(t, vector.Hex))
			got := read(&d, vector.Value)
			if err := d.End(); err != nil {
				t.Fatalf("refused: %v", err)
			}
			if !reflect.DeepEqual(got, vector.Value) {
				t.Fatalf("read %v, want %v", got, vector.Value)
			}
			want := cmp.Or(vector.Canonical, vector.Hex)
			if written := hex.EncodeToString(write(t, nil, vector.Value)); written != want {
				t.Fatalf("written %s, want %s", written, want)
			}
		})
	}
	for _, vector := range v.Refused {
		t.Run(vector.Name, func(t *testing.T) {
			d := wire.NewDecoder(unhex(t, vector.Hex))
			readAs(&d, vector.As)
			if err := d.End(); !errors.Is(err, wire.ErrMessage) {
				t.Fatalf("taken: %v", err)
			}
		})
	}
}

// a WELCOME's proof is the HMAC-SHA256 of the challenge under SERVE's secret,
// and nothing else proves
func TestProofVectors(t *testing.T) {
	for _, vector := range readVectors(t).Proofs {
		secret, challenge, proof := unhex(t, vector.Secret), unhex(t, vector.Challenge), unhex(t, vector.Proof)
		if got := wire.Prove(secret, challenge); !bytes.Equal(got, proof) {
			t.Errorf("%s: %x, want %x", vector.Name, got, proof)
		}
		published := wire.Published{Secret: base64.RawURLEncoding.EncodeToString(secret)}
		if !published.Proves(challenge, proof) {
			t.Errorf("%s: SERVE's secret does not take its own proof", vector.Name)
		}
		for _, wrong := range []wire.Published{{Secret: ""}, {Secret: published.Secret[1:]}, {Secret: "!"}} {
			if wrong.Proves(challenge, proof) {
				t.Errorf("%s: SERVE with the secret %q takes it", vector.Name, wrong.Secret)
			}
		}
		if published.Proves(challenge[1:], proof) || published.Proves(challenge, proof[1:]) {
			t.Errorf("%s: a short challenge or proof taken", vector.Name)
		}
	}
}

func TestFrameVectors(t *testing.T) {
	v := readVectors(t)
	for _, vector := range v.Frames {
		t.Run(vector.Name, func(t *testing.T) {
			frame := unhex(t, vector.Hex)
			r := wire.NewReader(bytes.NewReader(frame), 1<<20)
			h, err := r.Header()
			if err != nil {
				t.Fatal(err)
			}
			want := wire.Header{
				Length: vector.Header.Length, Kind: wire.Kind(vector.Header.Kind), Flags: wire.Flags(vector.Header.Flags),
				Method: wire.Method(vector.Header.Method), Stream: vector.Header.Stream,
			}
			if h != want {
				t.Fatalf("header %+v, want %+v", h, want)
			}
			body, err := r.Body(nil)
			if err != nil {
				t.Fatal(err)
			}
			if vector.Raw != "" {
				if got := hex.EncodeToString(body); got != vector.Raw {
					t.Fatalf("body %s, want %s", got, vector.Raw)
				}
				return
			}
			d := wire.NewDecoder(body)
			if got := read(&d, vector.Body); d.End() != nil || !reflect.DeepEqual(got, vector.Body) {
				t.Fatalf("body %v (%v), want %v", got, d.Err(), vector.Body)
			}
			if written := wire.AppendFrame(nil, h, write(t, nil, vector.Body)); !bytes.Equal(written, frame) {
				t.Fatalf("written %x, want %x", written, frame)
			}
		})
	}
	for _, vector := range v.RefusedFrames {
		t.Run(vector.Name, func(t *testing.T) {
			r := wire.NewReader(bytes.NewReader(unhex(t, vector.Hex)), cmp.Or(vector.MaxBody, 1<<20))
			if _, err := r.Header(); !errors.Is(err, wire.ErrProtocol) {
				t.Fatalf("taken: %v", err)
			}
		})
	}
}

// the examples of docs/wire.md are the messages the Go types write
func TestTheExamplesAreWhatTheMessagesWrite(t *testing.T) {
	v := readVectors(t)
	frames := map[string][]byte{}
	for _, vector := range v.Frames {
		frames[vector.Name] = unhex(t, vector.Hex)
	}

	hello := wire.Hello{Protocol: 1, Client: "tinystore-bun/0.1"}
	if written := wire.AppendFrame(nil, wire.Header{Kind: wire.KindHello}, hello.Append(nil)); !bytes.Equal(written,
		frames["a HELLO from Bun"]) {
		t.Errorf("HELLO %x", written)
	}
	hello.Challenge = unhex(t, "000102030405060708090a0b0c0d0e0f")
	if written := wire.AppendFrame(nil, wire.Header{Kind: wire.KindHello}, hello.Append(nil)); !bytes.Equal(written,
		frames["a HELLO from Bun that found the server through SERVE"]) {
		t.Errorf("HELLO with a challenge %x", written)
	}

	credit := wire.AppendCredit(nil, 5, 64<<10)
	if !bytes.Equal(credit, frames["64 KiB of credit on stream 5"]) {
		t.Errorf("CREDIT %x", credit)
	}

	conflict := &wire.Error{Code: wire.CodeConflict, Message: "state changed", What: map[string]string{"bucket": "sessions"}}
	header := wire.Header{Kind: wire.KindResponse, Flags: wire.FlagEnd | wire.FlagError, Stream: 7}
	if written := wire.AppendFrame(nil, header, conflict.Append(nil)); !bytes.Equal(written,
		frames["a conflict on stream 7"]) {
		t.Errorf("conflict %x", written)
	}
	var decoded wire.Error
	if err := decoded.Decode(frames["a conflict on stream 7"][wire.HeaderSize:]); err != nil ||
		!reflect.DeepEqual(&decoded, conflict) {
		t.Errorf("conflict read as %+v, %v", decoded, err)
	}

	get := wire.KVCall{Handle: 1, Owners: []string{"7"}, Key: "token"}
	header = wire.Header{Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: wire.KVGet, Stream: 1}
	if written := wire.AppendFrame(nil, header, get.Append(nil)); !bytes.Equal(written,
		frames["kv.get of a key under an owner, on handle 1"]) {
		t.Errorf("kv.get %x", written)
	}
	entry := wire.KVEntry{
		Found: true, Value: wire.KVValue{Kind: wire.KVBytes, Bytes: []byte("hello")},
		Version: []byte("1"), Expires: 1_790_000_000_000,
	}
	header = wire.Header{Kind: wire.KindResponse, Flags: wire.FlagEnd, Stream: 1}
	if written := wire.AppendFrame(nil, header, entry.Append(nil)); !bytes.Equal(written, frames["kv.get's entry"]) {
		t.Errorf("kv.get's entry %x", written)
	}

	exec := wire.SQLStatement{Handle: 1, SQL: "insert into notes (title) values (?)", Args: []any{"milk"}}
	header = wire.Header{Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: wire.SQLExec, Stream: 3}
	if written := wire.AppendFrame(nil, header, exec.Append(nil)); !bytes.Equal(written,
		frames["sql.exec of an insert with one argument, on handle 1"]) {
		t.Errorf("sql.exec %x", written)
	}
	done := wire.SQLDone{Changes: 1, LastID: 7}
	header = wire.Header{Kind: wire.KindResponse, Flags: wire.FlagEnd, Stream: 3}
	if written := wire.AppendFrame(nil, header, done.Append(nil)); !bytes.Equal(written, frames["sql.exec's answer"]) {
		t.Errorf("sql.exec's answer %x", written)
	}

	click := wire.RecordsBatch{Records: []wire.Record{{
		At: 1_790_000_000_123_456_789, Stream: "web", Name: "click",
		Attrs: []wire.RecordField{{Key: "element", Value: `"buy"`}},
	}}}
	header = wire.Header{Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: wire.RecordsAppend, Stream: 5}
	if written := wire.AppendFrame(nil, header, click.Append(nil)); !bytes.Equal(written,
		frames["records.append of one click"]) {
		t.Errorf("records.append %x", written)
	}

	cpu := wire.MetricsBatch{Series: []wire.MetricsSeries{{
		Labels: map[string]string{"__name__": "cpu", "host": "web-1"}, Kind: "gauge",
		Times: []int64{1_790_000_000_000}, Values: []float64{0.5},
	}}}
	header = wire.Header{Kind: wire.KindRequest, Flags: wire.FlagEnd, Method: wire.MetricsIngest, Stream: 9}
	if written := wire.AppendFrame(nil, header, cpu.Append(nil)); !bytes.Equal(written,
		frames["metrics.ingest of one sample"]) {
		t.Errorf("metrics.ingest %x", written)
	}
}

// every kv message reads back as it was written
func TestKVMessagesReadBackAsTheyWereWritten(t *testing.T) {
	call := wire.KVCall{
		Handle: 3, Owners: []string{"tenant", "\xff"}, Key: "k", TTL: 1000, ExpireAt: 1_790_000_000_000,
		IfVersion: []byte("1a"), IfAbsent: true, N: -4, After: "a", Limit: 10,
		Value: wire.KVValue{Kind: wire.KVInt, Int: -1},
	}
	var callRead wire.KVCall
	if err := callRead.Decode(call.Append(nil)); err != nil || !reflect.DeepEqual(callRead, call) {
		t.Errorf("a call read as %+v, %v", callRead, err)
	}

	calls := wire.KVCalls{Calls: []wire.KVOperation{{Method: wire.KVSet, KVCall: call}, {Method: wire.KVHas}}}
	var callsRead wire.KVCalls
	if err := callsRead.Decode(calls.Append(nil)); err != nil || !reflect.DeepEqual(callsRead, calls) {
		t.Errorf("calls read as %+v, %v", callsRead, err)
	}

	bucket := wire.KVBucket{Name: "attempts", Counters: true, DefaultTTL: 60_000, Sliding: 1, LoseAtMost: 1000}
	var bucketRead wire.KVBucket
	if err := bucketRead.Decode(bucket.Append(nil)); err != nil || bucketRead != bucket {
		t.Errorf("a bucket read as %+v, %v", bucketRead, err)
	}

	results := wire.KVResults{Entries: []wire.KVEntry{{Found: true, Key: "\x00k", Value: wire.KVValue{
		Kind:  wire.KVBytes,
		Bytes: []byte{},
	}}, {}}}
	var resultsRead wire.KVResults
	if err := resultsRead.Decode(results.Append(nil)); err != nil || !reflect.DeepEqual(resultsRead, results) {
		t.Errorf("results read as %+v, %v", resultsRead, err)
	}
}

// a key is a str, a bin or an integer; anything else is refused
func TestAKVKeyIsTextOrAnInteger(t *testing.T) {
	for _, c := range []struct {
		key  []byte
		want string
	}{
		{wire.AppendStr(nil, "k"), "k"},
		{wire.AppendBin(nil, []byte{0, 1}), "\x00\x01"},
		{wire.AppendInt(nil, -42), "-42"},
		{wire.AppendUint(nil, 1<<63), "9223372036854775808"},
	} {
		d := wire.NewDecoder(c.key)
		if got := d.KeyText(); got != c.want || d.End() != nil {
			t.Errorf("%x read as %q, %v", c.key, got, d.Err())
		}
	}
	for _, refused := range [][]byte{wire.AppendNil(nil), wire.AppendFloat(nil, 1), wire.AppendBool(nil, true)} {
		d := wire.NewDecoder(refused)
		d.KeyText()
		if !errors.Is(d.End(), wire.ErrMessage) {
			t.Errorf("%x taken as a key", refused)
		}
	}
}

// read reads a value as the vector's want says it is, and returns what it read
// in the same notation
func read(d *wire.Decoder, want any) any {
	switch want := want.(type) {
	case nil:
		if !d.Nil() {
			d.Fail("not nil")
		}
		return nil
	case bool:
		return d.Bool()
	case map[string]any:
		for kind, inner := range want {
			return readKind(d, kind, inner)
		}
	}
	d.Fail("a vector of %T", want)
	return nil
}

func readKind(d *wire.Decoder, kind string, inner any) any {
	switch kind {
	case "uint":
		return map[string]any{"uint": strconv.FormatUint(d.Uint(), 10)}
	case "int":
		return map[string]any{"int": strconv.FormatInt(d.Int(), 10)}
	case "float":
		return map[string]any{"float": fmt.Sprintf("%016x", math.Float64bits(d.Float()))}
	case "str":
		return map[string]any{"str": d.Str()}
	case "bin":
		return map[string]any{"bin": hex.EncodeToString(d.Bin())}
	case "array":
		return map[string]any{"array": readItems(d, inner.([]any))}
	case "map":
		return map[string]any{"map": readPairs(d, inner.([]any))}
	}
	d.Fail("a vector of kind %s", kind)
	return nil
}

func readItems(d *wire.Decoder, items []any) []any {
	got := []any{}
	for i := range d.Items() {
		if i < len(items) {
			got = append(got, read(d, items[i]))
		}
	}
	return got
}

func readPairs(d *wire.Decoder, pairs []any) []any {
	got := []any{}
	value := func(i int) any {
		if i < len(pairs) {
			return read(d, pairs[i].([]any)[1])
		}
		return nil
	}
	if isNames(pairs) {
		for name := range d.Names() {
			got = append(got, []any{map[string]any{"str": name}, value(len(got))})
		}
		return got
	}
	for key := range d.Fields() {
		got = append(got, []any{map[string]any{"uint": strconv.FormatUint(key, 10)}, value(len(got))})
	}
	return got
}

// isNames says that a vector's map has names for keys
func isNames(pairs []any) bool {
	if len(pairs) == 0 {
		return false
	}
	_, named := pairs[0].([]any)[0].(map[string]any)["str"]
	return named
}

// readAs reads a value as a refused vector names its type
func readAs(d *wire.Decoder, as string) {
	switch as {
	case "any":
		d.Skip()
	case "uint":
		d.Uint()
	case "int":
		d.Int()
	case "float":
		d.Float()
	case "str":
		d.Str()
	case "bin":
		d.Bin()
	case "map":
		for range d.Fields() {
			d.Skip()
		}
	default:
		d.Fail("a refused vector read as %s", as)
	}
}

// write writes a vector's value canonically: a map's keys ascending
func write(t testing.TB, dst []byte, value any) []byte {
	t.Helper()
	switch value := value.(type) {
	case nil:
		return wire.AppendNil(dst)
	case bool:
		return wire.AppendBool(dst, value)
	case map[string]any:
		for kind, inner := range value {
			return writeKind(t, dst, kind, inner)
		}
	}
	t.Fatalf("a vector of %T", value)
	return nil
}

func writeKind(t testing.TB, dst []byte, kind string, inner any) []byte {
	t.Helper()
	switch kind {
	case "uint":
		n, err := strconv.ParseUint(inner.(string), 10, 64)
		if err != nil {
			t.Fatal(err)
		}
		return wire.AppendUint(dst, n)
	case "int":
		n, err := strconv.ParseInt(inner.(string), 10, 64)
		if err != nil {
			t.Fatal(err)
		}
		return wire.AppendInt(dst, n)
	case "float":
		bits, err := strconv.ParseUint(inner.(string), 16, 64)
		if err != nil {
			t.Fatal(err)
		}
		return wire.AppendFloat(dst, math.Float64frombits(bits))
	case "str":
		return wire.AppendStr(dst, inner.(string))
	case "bin":
		return wire.AppendBin(dst, unhex(t, inner.(string)))
	case "array":
		items := inner.([]any)
		dst = wire.AppendArray(dst, len(items))
		for _, item := range items {
			dst = write(t, dst, item)
		}
		return dst
	case "map":
		pairs := slices.Clone(inner.([]any))
		slices.SortStableFunc(pairs, func(a, b any) int { return compareKeys(a.([]any)[0], b.([]any)[0]) })
		dst = wire.AppendMap(dst, len(pairs))
		for _, pair := range pairs {
			dst = write(t, write(t, dst, pair.([]any)[0]), pair.([]any)[1])
		}
		return dst
	}
	t.Fatalf("a vector of kind %s", kind)
	return nil
}

// compareKeys orders unsigned keys by value and names by their bytes
func compareKeys(a, b any) int {
	ka, kb := a.(map[string]any), b.(map[string]any)
	if ua, ok := ka["uint"].(string); ok {
		na, _ := strconv.ParseUint(ua, 10, 64)
		nb, _ := strconv.ParseUint(kb["uint"].(string), 10, 64)
		return cmp.Compare(na, nb)
	}
	return cmp.Compare(ka["str"].(string), kb["str"].(string))
}
