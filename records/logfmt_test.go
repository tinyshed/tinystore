package records

import (
	"log/slog"
	"slices"
	"testing"
)

// the examples in the comment on logfmtFields, and what logfmt writers write
func TestLogfmtLinesKeepTheirPairsWhenTheySpellTheLineAgain(t *testing.T) {
	for line, want := range map[string][]Field{
		`level=info msg="slow request" ms=1200`: {{"level", `"info"`}, {"msg", `"slow request"`}, {"ms", "1200"}},
		`level=info  msg=ok`:                    nil,
		`level=info msg="ok"`:                   nil,
		`t=2026-09-26T12:00:01Z path=/a?b=c empty= flag=true n=-1.5e3 zero=007`: {
			{"t", `"2026-09-26T12:00:01Z"`},
			{"path", `"/a?b=c"`},
			{"empty", `""`},
			{"flag", "true"},
			{"n", "-1.5e3"},
			{"zero", `"007"`},
		},
		`msg="say \"hi\"\tthere" dir=C:\tmp`: {{"msg", `"say \"hi\"\tthere"`}, {"dir", `"C:\\tmp"`}},
		`just text`:                          nil,
		`one=pair`:                           nil,
		`a=1 b=2 `:                           nil,
		`a=1 "b"=2`:                          nil,
		`a=1 b="\x"`:                         nil,
		`a=1 b="unclosed`:                    nil,
	} {
		got, ok := logfmtFields(line)
		if ok != (want != nil) || !slices.Equal(got, want) {
			t.Errorf("%q: %q %v, want %q", line, got, ok, want)
		}
		if ok && spellLogfmt(got) != line {
			t.Errorf("%q spells back as %q", line, spellLogfmt(got))
		}
	}
}

// a logfmt line becomes a record named logfmt with its pairs as attributes,
// found by them, and a JSON line one named json
func TestLinesKeepALogfmtLinesPairs(t *testing.T) {
	s := openRecords(t)
	got := s.linesOf(t, "logfmt", "level=warn msg=\"slow request\" ms=1200\n{\"level\":30,\"msg\":\"ok\"}\nplain\n")
	if len(got) != 3 || got[0].Name != logfmtLine || got[1].Name != jsonLine || got[2].Name != textLine {
		t.Fatalf("records %+v", got)
	}
	if got[0].Level == nil || *got[0].Level != slog.LevelWarn || !slices.Equal(got[0].Attrs, []Field{
		{"level", `"warn"`}, {"msg", `"slow request"`}, {"ms", "1200"},
	}) {
		t.Fatalf("the logfmt line became %+v", got[0])
	}
	found := s.readAll(t, Query{Streams: []string{"logfmt"}, Attrs: []Field{Int("ms", 1200)}})
	if len(found) != 1 || found[0].Name != logfmtLine {
		t.Fatalf("a read by the pair found %+v", found)
	}
}
