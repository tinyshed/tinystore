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
	"strings"
	"unicode/utf8"
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
	if len(labels) == 0 || len(labels) > maxLabels {
		return nil, "", fmt.Errorf("%w: expected 1..%d labels", ErrInvalid, maxLabels)
	}
	ordered := slices.Clone(labels)
	slices.SortFunc(ordered, func(a, b Label) int { return strings.Compare(a.Name, b.Name) })
	bytes, hasMetric := 0, false
	for i, label := range ordered {
		if len(label.Name) > maxLabelNameBytes || len(label.Value) > maxLabelValueBytes {
			return nil, "", fmt.Errorf("%w: label %q is over its own budget", ErrInvalid, label.Name)
		}
		bytes += len(label.Name) + len(label.Value)
		if bytes > maxLabelBytes || label.Name == "" || !utf8.ValidString(label.Name) || !utf8.ValidString(label.Value) {
			return nil, "", fmt.Errorf("%w: label name, encoding or size", ErrInvalid)
		}
		if i > 0 && ordered[i-1].Name == label.Name {
			return nil, "", fmt.Errorf("%w: duplicate label %q", ErrInvalid, label.Name)
		}
		if label.Name == "__name__" {
			hasMetric = label.Value != ""
		}
	}
	if requireMetric && !hasMetric {
		return nil, "", fmt.Errorf("%w: a nonempty __name__ label is required", ErrInvalid)
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
	out := binary.AppendUvarint(make([]byte, 0, 2*len(ids)+1), uint64(len(ids))) //nolint:gosec // a checked label count
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

// lookupLabelIDs asks for every pair at once; a short answer means the
// dictionary does not hold them all yet
func lookupLabelIDs(ctx context.Context, tx *sql.Tx, labels []Label) ([]int64, bool, error) {
	query := `select id from label_values where (name,value) in (values ` + //nolint:gosec // only placeholders are concatenated, every value is bound
		strings.Repeat("(?,?),", len(labels)-1) + `(?,?))`
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

func registerLabels(ctx context.Context, tx *sql.Tx, labels []Label) ([]int64, error) {
	ids, complete, err := lookupLabelIDs(ctx, tx, labels)
	if err != nil {
		return nil, err
	}
	if complete {
		return ids, nil
	}
	for _, label := range labels {
		if _, err = tx.ExecContext(ctx, `insert into label_values(name,value) values(?,?) on conflict(name,value) do nothing`, label.Name, label.Value); err != nil {
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

type storedIdentity struct {
	identity string
	text     sql.NullString
	ids      []byte
}

// confirms is the invariant a digest cannot carry on its own: a lookup key may
// only resolve a series once the labels themselves have been compared in full
func (s storedIdentity) confirms(ctx context.Context, tx *sql.Tx, batch preparedBatch) error {
	if s.identity == batch.identity || (s.text.Valid && s.text.String == batch.identity) {
		return nil
	}
	if s.ids != nil {
		stored, err := decodeLabelIDs(s.ids)
		if err != nil {
			return err
		}
		want, complete, err := lookupLabelIDs(ctx, tx, batch.labels)
		if err != nil {
			return err
		}
		if complete && slices.Equal(stored, want) {
			return nil
		}
		return fmt.Errorf("%w: series digest collision", ErrConflict)
	}
	// an older file kept the labels in the identity column itself
	text := s.identity
	if s.text.Valid {
		text = s.text.String
	}
	decoded, err := decodeLabels(text)
	if err != nil {
		return err
	}
	_, canonical, err := canonicalLabels(decoded, true)
	if err != nil {
		return err
	}
	if canonical != batch.identity {
		return fmt.Errorf("%w: series digest collision", ErrConflict)
	}
	return nil
}

func (s *Store) resolveSeries(ctx context.Context, tx *sql.Tx, batch preparedBatch) (int64, error) {
	legacy, err := json.Marshal(batch.labels)
	if err != nil {
		return 0, fmt.Errorf("encode legacy identity: %w", err)
	}
	identity := seriesIdentity(batch.identity)
	var id int64
	var kind Kind
	var stored storedIdentity
	err = tx.QueryRowContext(ctx, `select id,kind,identity,labels,label_ids from series where identity in (?,?,?)`,
		identity, batch.identity, string(legacy)).Scan(&id, &kind, &stored.identity, &stored.text, &stored.ids)
	if err == nil {
		if kind != batch.kind {
			return 0, fmt.Errorf("%w: a series cannot change kind", ErrConflict)
		}
		if err = stored.confirms(ctx, tx, batch); err != nil {
			return 0, err
		}
		if stored.ids == nil || stored.identity != identity {
			ids, registerErr := registerLabels(ctx, tx, batch.labels)
			if registerErr != nil {
				return 0, registerErr
			}
			if _, err = tx.ExecContext(ctx, `update series set identity=?,labels=null,label_ids=? where id=?`, identity, encodeLabelIDs(ids), id); err != nil {
				return 0, fmt.Errorf("compact series identity: %w", err)
			}
		}
		return id, nil
	}
	if !errors.Is(err, sql.ErrNoRows) {
		return 0, fmt.Errorf("resolve series: %w", err)
	}
	var count int
	if err = tx.QueryRowContext(ctx, `select series_count from store_state where id=1`).Scan(&count); err != nil {
		return 0, fmt.Errorf("read cardinality: %w", err)
	}
	if count >= s.opts.MaxSeries {
		return 0, fmt.Errorf("%w: series cardinality", ErrLimit)
	}
	ids, err := registerLabels(ctx, tx, batch.labels)
	if err != nil {
		return 0, err
	}
	result, err := tx.ExecContext(ctx, `insert into series(identity,labels,label_ids,kind) values(?,null,?,?)`, identity, encodeLabelIDs(ids), batch.kind)
	if err != nil {
		return 0, fmt.Errorf("register series: %w", err)
	}
	if id, err = result.LastInsertId(); err != nil {
		return 0, fmt.Errorf("read series identifier: %w", err)
	}
	for _, labelID := range ids {
		if _, err = tx.ExecContext(ctx, `insert into postings(label_id,series_id) values(?,?)`, labelID, id); err != nil {
			return 0, fmt.Errorf("index series label: %w", err)
		}
	}
	if _, err = tx.ExecContext(ctx, `insert into series_state(series_id,max_seen_ts) values(?,?)`, id, batch.samples[0].At); err != nil {
		return 0, fmt.Errorf("initialize series state: %w", err)
	}
	if _, err = tx.ExecContext(ctx, `update store_state set series_count=series_count+1 where id=1`); err != nil {
		return 0, fmt.Errorf("increase cardinality: %w", err)
	}
	return id, nil
}

// the probe stops at the cap because the matchers are being ranked, not counted
const selectivityProbe = 1024

type matcherPosting struct {
	labelID int64
	names   int
}

const rankShape = `select v.id,(select count(*) from (select 1 from postings p where p.label_id=v.id limit ?)) from label_values v where v.name=? and v.value=?`

// rankMatchers resolves every matcher to its dictionary id, shortest posting
// list first. The second result is false when a matcher names no series at all,
// which makes the whole match empty without running it.
func rankMatchers(ctx context.Context, tx *sql.Tx, matchers []Label) ([]matcherPosting, bool, error) {
	ranked := make([]matcherPosting, 0, len(matchers))
	for _, matcher := range matchers {
		var posting matcherPosting
		row := tx.QueryRowContext(ctx, rankShape, selectivityProbe, matcher.Name, matcher.Value)
		switch err := row.Scan(&posting.labelID, &posting.names); {
		case errors.Is(err, sql.ErrNoRows):
			return nil, false, nil
		case err != nil:
			return nil, false, fmt.Errorf("rank matcher %q: %w", matcher.Name, err)
		}
		ranked = append(ranked, posting)
	}
	slices.SortStableFunc(ranked, func(a, b matcherPosting) int { return a.names - b.names })
	return ranked, true, nil
}

// postingsFilter drives from the shortest posting list; driving from the
// alphabetically first matcher scanned a hundred times the rows at 100k series
func postingsFilter(ranked []matcherPosting) (string, []any) {
	var query strings.Builder
	query.WriteString(`select p.series_id as series_id from postings p where p.label_id=?`)
	arguments := []any{ranked[0].labelID}
	for _, posting := range ranked[1:] {
		query.WriteString(` and exists(select 1 from postings q where q.label_id=? and q.series_id=p.series_id)`)
		arguments = append(arguments, posting.labelID)
	}
	return query.String(), arguments
}

// the source is named once, so the budget can refuse it before SQLite
// materializes it and the reader is not asked to follow three coalesces
const matchShape = `select id,kind,compact,length(source),case when length(source)<=? then source else null end from (
	select s.id as id,s.kind as kind,s.label_ids is not null as compact,
	       coalesce(s.label_ids,cast(coalesce(s.labels,s.identity) as blob)) as source
	from (:postings) m join series s on s.id=m.series_id
) order by id limit ?`

func matchSeries(ctx context.Context, tx *sql.Tx, matchers []Label, budget *queryBudget) ([]registeredSeries, error) {
	ranked, possible, err := rankMatchers(ctx, tx, matchers)
	if err != nil {
		return nil, err
	}
	if !possible {
		return nil, nil
	}
	filter, filterArguments := postingsFilter(ranked)
	query := strings.Replace(matchShape, ":postings", filter, 1)
	arguments := append([]any{budget.limits.PayloadBytes - budget.bytes}, filterArguments...)
	arguments = append(arguments, budget.limits.Series+1)
	rows, err := tx.QueryContext(ctx, query, arguments...)
	if err != nil {
		return nil, fmt.Errorf("match series: %w", err)
	}
	defer rows.Close()
	var matched []registeredSeries
	for rows.Next() {
		if len(matched) == budget.limits.Series {
			return nil, fmt.Errorf("%w: matched series", ErrLimit)
		}
		var series registeredSeries
		var compact int
		var source []byte
		var size int
		if err = rows.Scan(&series.id, &series.kind, &compact, &size, &source); err != nil {
			return nil, fmt.Errorf("read matched series: %w", err)
		}
		if err = budget.takeBytes(size); err != nil {
			return nil, err
		}
		if source == nil || (series.kind != Gauge && series.kind != Counter) {
			return nil, fmt.Errorf("%w: series metadata", ErrCorrupt)
		}
		if compact != 0 {
			if series.ids, err = decodeLabelIDs(source); err != nil {
				return nil, err
			}
		} else if series.labels, err = decodeLabels(string(source)); err != nil {
			return nil, fmt.Errorf("%w: registry labels: %w", ErrCorrupt, err)
		}
		matched = append(matched, series)
	}
	if err = rows.Err(); err != nil {
		return nil, fmt.Errorf("iterate matched series: %w", err)
	}
	if err = fillLabels(ctx, tx, matched, budget); err != nil {
		return nil, err
	}
	return matched, nil
}

// fillLabels turns the dictionary entries a series was registered with back
// into its labels, one query for the whole match rather than one per series
func fillLabels(ctx context.Context, tx *sql.Tx, matched []registeredSeries, budget *queryBudget) error {
	wanted := map[int64]Label{}
	for _, series := range matched {
		for _, id := range series.ids {
			wanted[id] = Label{}
		}
	}
	if len(wanted) == 0 {
		return nil
	}
	pending := make([]int64, 0, len(wanted))
	for id := range wanted {
		pending = append(pending, id)
	}
	slices.Sort(pending)
	for start := 0; start < len(pending); start += dictionaryChunk {
		chunk := pending[start:min(start+dictionaryChunk, len(pending))]
		query := `select id,name,value from label_values where id in (?` + strings.Repeat(",?", len(chunk)-1) + `)` //nolint:gosec // only placeholders are concatenated
		arguments := make([]any, len(chunk))
		for i, id := range chunk {
			arguments[i] = id
		}
		if err := func() error {
			rows, err := tx.QueryContext(ctx, query, arguments...)
			if err != nil {
				return fmt.Errorf("read label dictionary: %w", err)
			}
			defer rows.Close()
			for rows.Next() {
				var id int64
				var label Label
				if err = rows.Scan(&id, &label.Name, &label.Value); err != nil {
					return fmt.Errorf("read label pair: %w", err)
				}
				if err = budget.takeBytes(len(label.Name) + len(label.Value)); err != nil {
					return err
				}
				wanted[id] = label
			}
			return rows.Err()
		}(); err != nil {
			return err
		}
	}
	for i := range matched {
		if matched[i].ids == nil {
			continue
		}
		labels := make([]Label, 0, len(matched[i].ids))
		for _, id := range matched[i].ids {
			label, found := wanted[id]
			if !found || label.Name == "" {
				return fmt.Errorf("%w: label %d is missing from the dictionary", ErrCorrupt, id)
			}
			labels = append(labels, label)
		}
		ordered, _, err := canonicalLabels(labels, true)
		if err != nil {
			return fmt.Errorf("%w: registry labels: %w", ErrCorrupt, err)
		}
		matched[i].labels = ordered
	}
	return nil
}

func decodeLabels(encoded string) ([]Label, error) {
	var labels []Label
	if strings.HasPrefix(encoded, "[[") {
		var pairs [][]string
		if err := json.Unmarshal([]byte(encoded), &pairs); err != nil {
			return nil, fmt.Errorf("decode label pairs: %w", err)
		}
		for _, pair := range pairs {
			if len(pair) != 2 {
				return nil, fmt.Errorf("%w: label pair", ErrCorrupt)
			}
			labels = append(labels, Label{Name: pair[0], Value: pair[1]})
		}
	} else if err := json.Unmarshal([]byte(encoded), &labels); err != nil {
		return nil, fmt.Errorf("decode legacy labels: %w", err)
	}
	if _, _, err := canonicalLabels(labels, true); err != nil {
		return nil, fmt.Errorf("%w: stored labels: %w", ErrCorrupt, err)
	}
	return labels, nil
}
