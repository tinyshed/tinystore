package records

import (
	"encoding/hex"
	"encoding/json"
	"flag"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"
)

var update = flag.Bool("update", false, "rewrite the golden vectors in testdata")

// goldenRows reads a golden file, after rewriting it from written when -update is set
func goldenRows[T any](t *testing.T, name string, written T) T {
	t.Helper()
	path := filepath.Join("testdata", name)
	if *update {
		encoded, err := json.MarshalIndent(written, "", "  ")
		if err != nil {
			t.Fatal(err)
		}
		if err = os.WriteFile(path, append(encoded, '\n'), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	var stored T
	if err = json.Unmarshal(data, &stored); err != nil {
		t.Fatal(err)
	}
	return stored
}

// goldenSegment is one segment row and its blocks, as hex
type goldenSegment struct {
	Segment string   `json:"segment"`
	Blocks  []string `json:"blocks"`
}

func goldenFixtures() map[string][]Record {
	return map[string][]Record{
		"frontend": frontendRecords(300), "backend": backendRecords(300), "edge": edgeRecords(), "text": textRecords(300),
	}
}

// the first format's bytes still read as the records they were written from,
// and the encoder still writes them: a change to either is a new format
// version, not an edit
func TestSegmentsWrittenBeforeStillRead(t *testing.T) {
	e, d := testCoders(t)
	written := map[string]goldenSegment{}
	for name, records := range goldenFixtures() {
		segment := e.encodeSegment(records[0].Stream, slices.Clone(records))
		golden := goldenSegment{Segment: hex.EncodeToString(segment.row)}
		for _, block := range segment.blocks {
			golden.Blocks = append(golden.Blocks, hex.EncodeToString(block.body))
		}
		written[name] = golden
	}
	stored := goldenRows(t, "segments-v1.json", written)
	for name, records := range goldenFixtures() {
		golden := stored[name]
		segment := encodedSegment{row: unhex(t, golden.Segment)}
		for _, block := range golden.Blocks {
			segment.blocks = append(segment.blocks, encodedBlock{body: unhex(t, block)})
		}
		sameRecords(t, sortedByTime(records), decodeSegment(t, d, segment))
		if !slices.Equal(golden.Blocks, written[name].Blocks) || golden.Segment != written[name].Segment {
			t.Errorf("%s: the encoder no longer writes the stored bytes", name)
		}
	}
}

func unhex(t *testing.T, text string) []byte {
	t.Helper()
	data, err := hex.DecodeString(text)
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func decodeSegment(t testing.TB, d *decoder, segment encodedSegment) []Record {
	t.Helper()
	s, err := d.parseSchema(segment.row)
	if err != nil {
		t.Fatal(err)
	}
	var decoded []Record
	for _, block := range segment.blocks {
		opened, err := d.openBlock(s, block.body)
		if err != nil {
			t.Fatal(err)
		}
		records, err := d.records(opened, nil)
		if err != nil {
			t.Fatal(err)
		}
		decoded = append(decoded, records...)
	}
	return decoded
}

func TestSegmentsRoundTripInEventTimeOrder(t *testing.T) {
	e, d := testCoders(t)
	for name, records := range map[string][]Record{
		"frontend": frontendRecords(5000),
		"backend":  backendRecords(3000),
		"edge":     edgeRecords(),
	} {
		t.Run(name, func(t *testing.T) {
			stored := slices.Clone(records)
			segment := e.encodeSegment(records[0].Stream, stored)
			want := slices.Clone(records)
			slices.SortStableFunc(want, func(a, b Record) int { return a.At.Compare(b.At) })
			sameRecords(t, want, decodeSegment(t, d, segment))
			if segment.first != want[0].At.UnixNano() || segment.last != want[len(want)-1].At.UnixNano() {
				t.Errorf("segment spans %d to %d", segment.first, segment.last)
			}
		})
	}
}

// the example in the comment on encodeSegment
func TestEqualTimesKeepTheirArrivalOrder(t *testing.T) {
	e, d := testCoders(t)
	at := time.Unix(0, fixtureBase).UTC()
	var records []Record
	for _, click := range []struct {
		offset time.Duration
		name   string
	}{{300, "buy"}, {100, "menu"}, {200, "save"}, {100, "back"}, {100, "next"}} {
		records = append(records, Record{At: at.Add(click.offset), Stream: "web", Name: click.name})
	}
	decoded := decodeSegment(t, d, e.encodeSegment("web", records))
	var names []string
	for _, record := range decoded {
		names = append(names, record.Name)
	}
	if want := []string{"menu", "back", "next", "save", "buy"}; !slices.Equal(names, want) {
		t.Fatalf("stored %v, want %v", names, want)
	}
}

func BenchmarkSegment(b *testing.B) {
	for name, records := range map[string][]Record{
		"frontend": frontendRecords(maxSegmentRecords),
		"backend":  backendRecords(maxSegmentRecords),
		"text":     textRecords(maxSegmentRecords),
	} {
		e, d := testCoders(b)
		segment := e.encodeSegment(name, slices.Clone(records))
		b.Run(name+"/encode", func(b *testing.B) {
			for b.Loop() {
				e.encodeSegment(name, slices.Clone(records))
			}
			b.ReportMetric(float64(b.N*len(records))/b.Elapsed().Seconds(), "records/s")
		})
		b.Run(name+"/decode", func(b *testing.B) {
			for b.Loop() {
				decodeSegment(b, d, segment)
			}
			b.ReportMetric(float64(b.N*len(records))/b.Elapsed().Seconds(), "records/s")
		})
	}
}
