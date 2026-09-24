package records

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"log/slog"
	"time"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
)

// the most records one Read returns, and how many a zero Limit asks for
const (
	maxLimit     = 10_000
	defaultLimit = 1_000
)

// Query is records in [From, To) at MinLevel or above, oldest first, at most
// Limit of them: a zero Limit is 1000, and more than 10000 is refused. A zero
// MinLevel is slog.LevelInfo, as in slog; debug lines need slog.LevelDebug.
type Query struct {
	From, To time.Time
	MinLevel slog.Level
	Limit    int
}

const readRecords = `select at, level, message, attrs from records
	where at >= ? and at < ? and level >= ?
	order by at, id limit cast(? as integer)`

func (s *Store) Read(ctx context.Context, query Query) ([]Record, error) {
	limit, err := checkQuery(query)
	if err != nil {
		return nil, err
	}

	var out []Record
	err = s.file.View(ctx, func(tx *sql.Tx) error {
		rows, queryErr := tx.QueryContext(ctx, readRecords, //nolint:rowserrcheck // EachRow checks Err
			query.From.UnixMilli(), query.To.UnixMilli(), int(query.MinLevel), limit)
		if queryErr != nil {
			return fmt.Errorf("records: read: %w", queryErr)
		}
		return sqlite.EachRow(rows, "records", func(rows *sql.Rows) error {
			record, scanErr := scanRecord(rows)
			out = append(out, record)
			return scanErr
		})
	})
	return out, err
}

func checkQuery(query Query) (int, error) {
	if query.Limit < 0 || query.Limit > maxLimit || query.To.Before(query.From) {
		return 0, fmt.Errorf("%w: records query range or limit", tinystore.ErrInvalid)
	}
	if query.Limit == 0 {
		return defaultLimit, nil
	}
	return query.Limit, nil
}

func scanRecord(rows *sql.Rows) (Record, error) {
	var record Record
	var at int64
	var level int
	var attrs string
	if err := rows.Scan(&at, &level, &record.Message, &attrs); err != nil {
		return record, err
	}
	record.At, record.Level = time.UnixMilli(at), slog.Level(level)
	if err := json.Unmarshal([]byte(attrs), &record.Attrs); err != nil {
		return record, fmt.Errorf("%w: records attributes: %w", tinystore.ErrCorrupt, err)
	}
	return record, nil
}
