package records

import (
	"bytes"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"math/rand/v2"
	"slices"
	"strings"
	"testing"

	"github.com/tinyshed/tinystore"
)

func roundTripValues(t *testing.T, e *encoder, d *decoder, values []string) byte {
	t.Helper()
	encoded := e.appendValues(nil, values, nil)
	c := cursor{data: encoded, budget: &expansion{limit: maxExpandedText}}
	decoded := d.values(&c, len(values), nil)
	if err := c.finish(); err != nil {
		t.Fatalf("%q: %v", values, err)
	}
	if !slices.Equal(decoded, values) && len(values) > 0 {
		t.Fatalf("decoded %q, want %q", decoded, values)
	}
	return encoded[0]
}

// each value comes back as it was spelled, and the column says which kind it
// was typed as
func TestValueColumnsKeepEverySpelling(t *testing.T) {
	e, d := testCoders(t)
	uuids, hexes, quotedInts, excepted := make([]string, 50), make([]string, 50), make([]string, 50), make([]string, 50)
	random := rand.New(rand.NewPCG(5, 6))
	for i := range 50 {
		uuids[i] = fmt.Sprintf(`"%08x-%04x-4%03x-8%03x-%012x"`, random.Uint32(), random.Uint32()&0xffff,
			random.Uint32()&0xfff, random.Uint32()&0xfff, random.Uint64()&0xffffffffffff)
		hexes[i] = fmt.Sprintf(`"%016x"`, random.Uint64())
		quotedInts[i] = fmt.Sprintf(`"%d"`, random.IntN(1000))
		excepted[i] = fmt.Sprint(random.IntN(1920))
	}
	excepted[7], excepted[31] = "null", "1.5"
	cases := []struct {
		name   string
		values []string
		kind   byte
	}{
		{"integers", []string{"1920", "1366", "-1", "0", "9223372036854775807", "-9223372036854775808"}, valueInteger},
		{"not quite integers", []string{"007", "+7", "-0", "1.0", "1e3", "9223372036854775808"}, valueText},
		{"quoted integers", quotedInts, valueQuoted | valueInteger},
		{"uuids", uuids, valueQuoted | valueUUID},
		{"upper case uuid", []string{`"9CBAF3D1-0C27-47BC-8FED-CB6E0763B9B2"`}, valueQuoted},
		{"hex", hexes, valueQuoted | valueHex},
		{"integers and exceptions", excepted, valueInteger | valueExceptions},
		{"json", []string{"1.2300", "-0", "null", `""`, `{"a":[1,{"b":null}]}`, "true", ` 1 `}, valueText},
		{"strings", []string{`"GET"`, `"POST"`, `"GET"`, `"GET"`}, valueQuoted},
		{"a lone quote", []string{`"`, `""`}, valueText},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			if kind := roundTripValues(t, e, d, test.values); kind != test.kind {
				t.Errorf("typed as %#x, want %#x", kind, test.kind)
			}
		})
	}
}

func TestTextColumnsChooseTheirLayout(t *testing.T) {
	e, d := testCoders(t)
	random := rand.New(rand.NewPCG(7, 8))
	randomText := make([]string, 40)
	for i := range randomText {
		chunk := make([]byte, 200)
		for j := range chunk {
			chunk[j] = byte(random.Uint32())
		}
		randomText[i] = string(chunk)
	}
	sentences := make([]string, 300)
	for i := range sentences {
		sentences[i] = fmt.Sprintf("GET /notes/%d finished in %d ms with status 200", i, i*7%400)
	}
	cases := map[string][]string{
		"lines":           sentences,
		"one length":      {"abcd", "efgh", "ijkl", "mnop"},
		"newlines":        {"a\nb", "c", "", "d\n"},
		"dictionary":      slices.Repeat([]string{"GET", "POST", "PUT"}, 30),
		"random":          randomText,
		"empty":           {},
		"one empty value": {""},
	}
	for name, values := range cases {
		t.Run(name, func(t *testing.T) {
			encoded := e.appendTexts(nil, values)
			c := cursor{data: encoded, budget: &expansion{limit: maxExpandedText}}
			decoded := d.texts(&c, len(values))
			if err := c.finish(); err != nil || (len(values) > 0 && !slices.Equal(decoded, values)) {
				t.Fatalf("decoded %d values, %v", len(decoded), err)
			}
		})
	}
}

// text a block decompresses counts against its bound before it is allocated
func TestExpandedTextIsBounded(t *testing.T) {
	e, d := testCoders(t)
	values := make([]string, 50)
	for i := range values {
		values[i] = strings.Repeat("a", 100_000+i)
	}
	encoded := e.appendTexts(nil, values)
	c := cursor{data: encoded, budget: &expansion{limit: maxExpandedText}}
	d.texts(&c, len(values))
	if err := c.finish(); !errors.Is(err, tinystore.ErrCorrupt) {
		t.Fatalf("5 MB of text expanded past a 4 MiB bound: %v", err)
	}
}

// String quotes as encoding/json does, so a value reads back with any JSON parser
func TestJSONHelpersSpellAsJSON(t *testing.T) {
	var control []byte
	for b := range 0x20 {
		control = append(control, byte(b))
	}
	for _, text := range []string{
		"plain", `a "b" \ c`, "<&>", string(control), string([]byte{0x7f, 0xff, 'x', 0xc3}),
		string([]rune{0x2028, 0x2029, 0x1f600}), "",
	} {
		var want bytes.Buffer
		encoder := json.NewEncoder(&want)
		encoder.SetEscapeHTML(false)
		if err := encoder.Encode(text); err != nil {
			t.Fatal(err)
		}
		if got := String("k", text).Value; got != strings.TrimSpace(want.String()) {
			t.Errorf("String(%q) = %s, want %s", text, got, want.String())
		}
	}
	for _, test := range []struct {
		field Field
		want  string
	}{
		{Int("k", -42), "-42"},
		{Float("k", 1.25), "1.25"},
		{Float("k", 1e21), "1e+21"},
		{Float("k", math.Copysign(0, -1)), "-0"},
		{Float("k", math.Inf(-1)), `"-Inf"`},
		{Bool("k", true), "true"},
		{JSON("k", []byte(`{"a":1}`)), `{"a":1}`},
	} {
		if test.field.Value != test.want {
			t.Errorf("%s, want %s", test.field.Value, test.want)
		}
	}
}

// the examples in the comments on value columns and raw text
func TestValueAndTextColumnsTakeTheLayoutsTheirCommentsShow(t *testing.T) {
	e, _ := testCoders(t)
	for _, test := range []struct {
		values []string
		kind   byte
	}{
		{[]string{`"9cbaf3d1-0c27-47bc-8fed-cb6e0763b9b2"`}, valueQuoted | valueUUID},
		{[]string{`"154"`}, valueQuoted | valueInteger},
		{[]string{"1920", "1366", "390", "1920", "1366", "390", "1920", "null"}, valueInteger | valueExceptions},
	} {
		if kind := e.appendValues(nil, test.values, nil)[0]; kind != test.kind {
			t.Errorf("%q typed as %#x, want %#x", test.values, kind, test.kind)
		}
	}
	lines := e.appendRaw(nil, []string{"GET /a", "POST /b"})
	if lines[0] != rawLines || string(lines[3:]) != "GET /a\nPOST /b\n" {
		t.Errorf("text without newlines: %q", lines)
	}
	ids := e.appendRaw(nil, []string{string(make([]byte, 16)), string(make([]byte, 16))})
	if ids[0] != rawLengths {
		t.Errorf("ids of one length: %q", ids)
	}
}
