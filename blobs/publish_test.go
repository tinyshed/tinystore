package blobs

import (
	"bytes"
	"database/sql"
	"errors"
	"testing"
)

// doomCommits makes every commit that writes the object doomed fail, after
// its statements ran: a trigger inserts a row that a deferred foreign key
// refuses only when the transaction commits
func (s *testStore) doomCommits(t *testing.T) {
	t.Helper()
	err := s.file.Update(t.Context(), func(tx *sql.Tx) error {
		for _, statement := range []string{
			`create table doomed_parent (id integer primary key)`,
			`create table doomed (parent integer references doomed_parent (id) deferrable initially deferred)`,
			`create trigger doom after insert on objects when new.path = 'doomed' begin
				insert into doomed (parent) values (1);
			end`,
		} {
			if _, err := tx.ExecContext(t.Context(), statement); err != nil {
				return err
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}

// a commit whose outcome is unknown asks the file whether its content is
// there before it lets go of its bytes: none is, so no file stays behind,
// and no id stays held
func TestAnUnknownCommitLeavesNoFileBehind(t *testing.T) {
	s := openTestStore(t, t.TempDir())
	media := openTestBucket(t, s, "media")
	s.doomCommits(t)
	for _, size := range []int{100, 1 << 20} {
		_, err := media.Put(t.Context(), "doomed", bytes.NewReader(randomBytes(1, size)))
		if !errors.Is(err, ErrOutcomeUnknown) {
			t.Fatalf("a Put of %d bytes whose commit fails: %v", size, err)
		}
		mustBeAbsent(t, media, "doomed")
		s.mustHoldFiles(t, 0)
		if len(s.ids.held) != 0 {
			t.Fatalf("ids held after the outcome was learnt: %v", s.ids.held)
		}
	}
	mustPut(t, media, "kept", randomBytes(2, 1<<20))
	s.mustHoldFiles(t, 1)
}
