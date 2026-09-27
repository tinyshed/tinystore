package metrics

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/base64"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"slices"
	"strconv"
	"strings"
	"unicode/utf8"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// a hundred short labels and thirty enormous ones are different threats, so a
// series is bounded by four budgets rather than by one count
const (
	maxLabels          = 128
	maxLabelBytes      = 16 << 10
	maxLabelNameBytes  = 256
	maxLabelValueBytes = 4 << 10
	dictionaryChunk    = 256
)

func canonicalLabels(labels []Label, requireMetric bool) ([]Label, string, error) {
	ordered, err := orderedLabels(labels, requireMetric)
	if err != nil {
		return nil, "", err
	}
	pairs := make([][2]string, len(ordered))
	for i, label := range ordered {
		pairs[i] = [2]string{label.Name, label.Value}
	}
	encoded, err := json.Marshal(pairs)
	if err != nil {
		return nil, "", fmt.Errorf("encode labels: %w", err)
	}
	return ordered, string(encoded), nil
}

func orderedLabels(labels []Label, requireMetric bool) ([]Label, error) {
	if len(labels) == 0 || len(labels) > maxLabels {
		return nil, fmt.Errorf("%w: expected 1..%d labels", ErrInvalid, maxLabels)
	}
	ordered := slices.Clone(labels)
	slices.SortFunc(ordered, func(a, b Label) int { return strings.Compare(a.Name, b.Name) })
	bytes, hasMetric := 0, false
	for i, label := range ordered {
		if len(label.Name) > maxLabelNameBytes || len(label.Value) > maxLabelValueBytes {
			return nil, fmt.Errorf("%w: label %q is over its own budget", ErrInvalid, label.Name)
		}
		bytes += len(label.Name) + len(label.Value)
		if bytes > maxLabelBytes || label.Name == "" ||
			!utf8.ValidString(label.Name) || !utf8.ValidString(label.Value) {
			return nil, fmt.Errorf("%w: label name, encoding or size", ErrInvalid)
		}
		if i > 0 && ordered[i-1].Name == label.Name {
			return nil, fmt.Errorf("%w: duplicate label %q", ErrInvalid, label.Name)
		}
		if label.Name == "__name__" {
			hasMetric = label.Value != ""
		}
	}
	if requireMetric && !hasMetric {
		return nil, fmt.Errorf("%w: a nonempty __name__ label is required", ErrInvalid)
	}
	return ordered, nil
}

// formatLabels prints a series the way Prometheus does:
//
//	{__name__="cpu", host="web-1", zone="a"}  →  cpu{host="web-1",zone="a"}
//	{__name__="up"}                            →  up
func formatLabels(labels []Label) string {
	var name strings.Builder
	var rest []string
	for _, label := range labels {
		if label.Name == "__name__" {
			name.WriteString(label.Value)
			continue
		}
		rest = append(rest, label.Name+"="+strconv.Quote(label.Value))
	}
	if len(rest) == 0 && name.Len() > 0 {
		return name.String()
	}
	return name.String() + "{" + strings.Join(rest, ",") + "}"
}

// seriesError names the series a refusal belongs to. A cancelled call or a
// failing file is not the series' doing and keeps its own error.
func seriesError(labels []Label, err error) error {
	for _, refusal := range []error{ErrInvalid, ErrLimit, ErrTooOld, ErrTooNew, ErrConflict, ErrCorrupt, ErrSuspended} {
		if errors.Is(err, refusal) {
			return &SeriesError{Labels: slices.Clone(labels), Err: err}
		}
	}
	return err
}

type registeredSeries struct {
	id     int64
	labels []Label
	ids    []int64
	kind   Kind
}

func seriesIdentity(labels string) string {
	digest := sha256.Sum256([]byte(labels))
	return "@" + base64.RawURLEncoding.EncodeToString(digest[:])
}

func encodeLabelIDs(ids []int64) []byte {
	slices.Sort(ids)
	out := binary.AppendUvarint(make([]byte, 0, 2*len(ids)+1), uint64(len(ids)))
	previous := int64(0)
	for _, id := range ids {
		out = binary.AppendUvarint(out, uint64(id-previous)) //nolint:gosec // ascending identifiers
		previous = id
	}
	return out
}

func decodeLabelIDs(blob []byte) ([]int64, error) {
	count, read := binary.Uvarint(blob)
	if read <= 0 || count == 0 || count > maxLabels {
		return nil, fmt.Errorf("%w: label identifier count", ErrCorrupt)
	}
	ids := make([]int64, 0, count)
	previous := int64(0)
	for range count {
		gap, n := binary.Uvarint(blob[read:])
		if n <= 0 || gap == 0 || gap > uint64(math.MaxInt64-previous) {
			return nil, fmt.Errorf("%w: label identifier", ErrCorrupt)
		}
		read += n
		previous += int64(gap) //nolint:gosec // bounded above
		ids = append(ids, previous)
	}
	if read != len(blob) {
		return nil, fmt.Errorf("%w: trailing label identifiers", ErrCorrupt)
	}
	return ids, nil
}

// placeholders repeats a group of bound parameters, never a value:
//
//	placeholders("?", 3)      →  ?,?,?
//	placeholders("(?,?)", 2)  →  (?,?),(?,?)
func placeholders(group string, count int) string {
	return strings.Repeat(group+",", count-1) + group
}

const labelPairsQuery = `select id from label_values where (name,value) in (values `

// lookupLabelIDs asks for every pair at once; a short answer means the
// dictionary does not hold them all yet
func lookupLabelIDs(ctx context.Context, tx sqlite.Writer, labels []Label) ([]int64, bool, error) {
	query := labelPairsQuery + placeholders("(?,?)", len(labels)) + `)`
	arguments := make([]any, 0, 2*len(labels))
	for _, label := range labels {
		arguments = append(arguments, label.Name, label.Value)
	}
	rows, err := tx.QueryContext(ctx, query, arguments...)
	if err != nil {
		return nil, false, fmt.Errorf("look up label pairs: %w", err)
	}
	defer rows.Close()
	ids := make([]int64, 0, len(labels))
	for rows.Next() {
		var id int64
		if err = rows.Scan(&id); err != nil {
			return nil, false, fmt.Errorf("read label pair: %w", err)
		}
		ids = append(ids, id)
	}
	if err = rows.Err(); err != nil {
		return nil, false, fmt.Errorf("iterate label pairs: %w", err)
	}
	slices.Sort(ids)
	return ids, len(ids) == len(labels), nil
}

const registerLabelQuery = `insert into label_values(name,value) values(?,?) on conflict(name,value) do nothing`

func registerLabels(ctx context.Context, tx sqlite.Writer, labels []Label) ([]int64, error) {
	ids, complete, err := lookupLabelIDs(ctx, tx, labels)
	if err != nil {
		return nil, err
	}
	if complete {
		return ids, nil
	}
	for _, label := range labels {
		if _, err = tx.ExecContext(ctx, registerLabelQuery, label.Name, label.Value); err != nil {
			return nil, fmt.Errorf("register label pair: %w", err)
		}
	}
	ids, complete, err = lookupLabelIDs(ctx, tx, labels)
	if err != nil {
		return nil, err
	}
	if !complete {
		return nil, fmt.Errorf("%w: label dictionary", ErrCorrupt)
	}
	return ids, nil
}

const increasePostingsQuery = `update label_values set posting_count=posting_count+1 where id in (`

func increasePostingCounts(ctx context.Context, tx sqlite.Writer, ids []int64) error {
	query := increasePostingsQuery + placeholders("?", len(ids)) + `)`
	arguments := make([]any, len(ids))
	for i, id := range ids {
		arguments[i] = id
	}
	result, err := tx.ExecContext(ctx, query, arguments...)
	if err != nil {
		return fmt.Errorf("count new series postings: %w", err)
	}
	changed, err := result.RowsAffected()
	if err != nil {
		return fmt.Errorf("count updated postings: %w", err)
	}
	if changed != int64(len(ids)) {
		return fmt.Errorf("%w: missing posting counter", ErrCorrupt)
	}
	return nil
}

// confirmIdentity is the check a digest cannot make on its own: a digest match
// resolves a series only when the stored label ids are the batch's own
func confirmIdentity(ctx context.Context, tx sqlite.Writer, stored []byte, batch preparedBatch) error {
	ids, err := decodeLabelIDs(stored)
	if err != nil {
		return err
	}
	want, complete, err := lookupLabelIDs(ctx, tx, batch.labels)
	if err != nil {
		return err
	}
	if !complete || !slices.Equal(ids, want) {
		return fmt.Errorf("%w: series digest collision", ErrConflict)
	}
	return nil
}

const resolveSeriesQuery = `select id, kind, label_ids from series where identity = ?`

func (s *Store) resolveSeries(ctx context.Context, tx sqlite.Writer, batch preparedBatch) (int64, error) {
	identity := seriesIdentity(batch.identity)
	var id int64
	var kind Kind
	var stored []byte
	err := sqlite.QueryRow(ctx, tx, resolveSeriesQuery, identity).Scan(&id, &kind, &stored)
	if errors.Is(err, sql.ErrNoRows) {
		return s.registerSeries(ctx, tx, identity, batch)
	}
	if err != nil {
		return 0, fmt.Errorf("resolve series: %w", err)
	}

	if kind != batch.kind {
		return 0, fmt.Errorf("%w: a series cannot change kind", ErrConflict)
	}
	if err := confirmIdentity(ctx, tx, stored, batch); err != nil {
		return 0, err
	}
	return id, nil
}

// registerSeries adds a series, its postings and its state inside the
// cardinality limit.
func (s *Store) registerSeries(
	ctx context.Context, tx sqlite.Writer, identity string, batch preparedBatch,
) (int64, error) {
	if err := s.checkCardinality(ctx, tx); err != nil {
		return 0, err
	}

	ids, err := registerLabels(ctx, tx, batch.labels)
	if err != nil {
		return 0, err
	}

	id, err := insertSeries(ctx, tx, identity, ids, batch.kind)
	if err != nil {
		return 0, err
	}

	if err = indexSeries(ctx, tx, id, ids); err != nil {
		return 0, err
	}

	if err = initializeSeriesState(ctx, tx, id, batch.samples[0].At); err != nil {
		return 0, err
	}
	return id, nil
}

const cardinalityQuery = `select series_count from store_state where id=1`

func (s *Store) checkCardinality(ctx context.Context, tx sqlite.Writer) error {
	var count int
	if err := sqlite.QueryRow(ctx, tx, cardinalityQuery).Scan(&count); err != nil {
		return fmt.Errorf("read cardinality: %w", err)
	}
	if count >= s.opts.MaxSeries {
		return fmt.Errorf("%w: series cardinality", ErrLimit)
	}
	return nil
}

const insertSeriesQuery = `insert into series(identity, label_ids, kind) values(?, ?, ?)`

func insertSeries(ctx context.Context, tx sqlite.Writer, identity string, ids []int64, kind Kind) (int64, error) {
	result, err := tx.ExecContext(ctx, insertSeriesQuery, identity, encodeLabelIDs(ids), kind)
	if err != nil {
		return 0, fmt.Errorf("register series: %w", err)
	}
	id, err := result.LastInsertId()
	if err != nil {
		return 0, fmt.Errorf("read series identifier: %w", err)
	}
	return id, nil
}

const insertPostingQuery = `insert into postings(label_id,series_id) values(?,?)`

func indexSeries(ctx context.Context, tx sqlite.Writer, id int64, labelIDs []int64) error {
	for _, labelID := range labelIDs {
		if _, err := tx.ExecContext(ctx, insertPostingQuery, labelID, id); err != nil {
			return fmt.Errorf("index series label: %w", err)
		}
	}
	return increasePostingCounts(ctx, tx, labelIDs)
}

const (
	insertSeriesStateQuery   = `insert into series_state(series_id,max_seen_ts) values(?,?)`
	increaseCardinalityQuery = `update store_state set series_count=series_count+1 where id=1`
)

func initializeSeriesState(ctx context.Context, tx sqlite.Writer, id, first int64) error {
	if _, err := tx.ExecContext(ctx, insertSeriesStateQuery, id, first); err != nil {
		return fmt.Errorf("initialize series state: %w", err)
	}
	if _, err := tx.ExecContext(ctx, increaseCardinalityQuery); err != nil {
		return fmt.Errorf("increase cardinality: %w", err)
	}
	return nil
}
