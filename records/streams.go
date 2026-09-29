package records

import (
	"context"
	"database/sql"
	"fmt"
	"sync"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// streams maps stream names to the ids the tables use. It only grows, and a
// name enters it after the transaction that stored it has committed.
type streams struct {
	mu    sync.RWMutex
	ids   map[string]int64
	names map[int64]string
}

const selectStreams = `select id, name from streams`

func (s *streams) load(ctx context.Context, file *sqlite.File) error {
	s.ids, s.names = map[string]int64{}, map[int64]string{}
	err := file.View(ctx, func(tx *sql.Tx) error {
		rows, err := tx.QueryContext(ctx, selectStreams) //nolint:rowserrcheck // EachRow checks Err
		if err != nil {
			return err
		}
		return sqlite.EachRow(rows, "streams", func(rows *sql.Rows) error {
			var id int64
			var name string
			if err := rows.Scan(&id, &name); err != nil {
				return err
			}
			s.ids[name], s.names[id] = id, name
			return nil
		})
	})
	if err != nil {
		return fmt.Errorf("records: load streams: %w", err)
	}
	return nil
}

func (s *streams) id(name string) (int64, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	id, ok := s.ids[name]
	return id, ok
}

func (s *streams) name(id int64) string {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return s.names[id]
}

func (s *streams) remember(added map[string]int64) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for name, id := range added {
		s.ids[name], s.names[id] = id, name
	}
}

const insertStream = `insert into streams (name) values (?) returning id`

// resolve gives a stream its id inside a write, storing it the first time;
// added collects the new ones for remember once the write commits
func (s *streams) resolve(ctx context.Context, tx sqlite.Writer, name string, added map[string]int64) (int64, error) {
	if id, ok := s.id(name); ok {
		return id, nil
	}
	if id, ok := added[name]; ok {
		return id, nil
	}
	var id int64
	if err := sqlite.QueryRowByKey(ctx, tx, insertStream, name).Scan(&id); err != nil {
		return 0, fmt.Errorf("records: store stream %q: %w", name, err)
	}
	added[name] = id
	return id, nil
}
