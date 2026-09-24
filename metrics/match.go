package metrics

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"slices"
	"strings"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

type matcherPosting struct {
	labelID int64
	names   int
}

// rankMatchers resolves every matcher to its dictionary id, shortest posting
// list first. The second result is false when a matcher names no series at all,
// which makes the whole match empty without running it.
func rankMatchers(ctx context.Context, tx sqlite.Reader, matchers []Label) ([]matcherPosting, bool, error) {
	if len(matchers) == 1 {
		var posting matcherPosting
		err := sqlite.QueryRow(ctx, tx, `select id from label_values where name=? and value=?`, matchers[0].Name, matchers[0].Value).Scan(&posting.labelID)
		if errors.Is(err, sql.ErrNoRows) {
			return nil, false, nil
		}
		if err != nil {
			return nil, false, fmt.Errorf("resolve matcher %q: %w", matchers[0].Name, err)
		}
		return []matcherPosting{posting}, true, nil
	}
	if len(matchers) < 8 {
		ranked := make([]matcherPosting, 0, len(matchers))
		for _, matcher := range matchers {
			var posting matcherPosting
			err := sqlite.QueryRow(ctx, tx, `select id,posting_count from label_values where name=? and value=?`, matcher.Name, matcher.Value).Scan(&posting.labelID, &posting.names)
			if errors.Is(err, sql.ErrNoRows) || posting.names == 0 && err == nil {
				return nil, false, nil
			}
			if err != nil {
				return nil, false, fmt.Errorf("rank matcher %q: %w", matcher.Name, err)
			}
			ranked = append(ranked, posting)
		}
		slices.SortStableFunc(ranked, func(a, b matcherPosting) int { return a.names - b.names })
		return ranked, true, nil
	}
	pairs := make([][2]string, len(matchers))
	for i, matcher := range matchers {
		pairs[i] = [2]string{matcher.Name, matcher.Value}
	}
	encoded, err := json.Marshal(pairs)
	if err != nil {
		return nil, false, fmt.Errorf("encode matchers: %w", err)
	}
	rows, err := tx.QueryContext(ctx, `select v.id,v.posting_count from json_each(?) j left join label_values v on v.name=json_extract(j.value,'$[0]') and v.value=json_extract(j.value,'$[1]') order by cast(j.key as integer)`, string(encoded))
	if err != nil {
		return nil, false, fmt.Errorf("resolve matchers: %w", err)
	}
	defer rows.Close()
	ranked := make([]matcherPosting, 0, len(matchers))
	for rows.Next() {
		var id, names sql.NullInt64
		if err = rows.Scan(&id, &names); err != nil {
			return nil, false, fmt.Errorf("read matcher: %w", err)
		}
		if !id.Valid || !names.Valid || names.Int64 == 0 {
			return nil, false, nil
		}
		if names.Int64 < 0 || names.Int64 > math.MaxInt {
			return nil, false, fmt.Errorf("%w: posting count", ErrCorrupt)
		}
		ranked = append(ranked, matcherPosting{labelID: id.Int64, names: int(names.Int64)})
	}
	if err = rows.Err(); err != nil {
		return nil, false, fmt.Errorf("iterate matchers: %w", err)
	}
	if len(ranked) != len(matchers) {
		return nil, false, fmt.Errorf("%w: matcher resolution count", ErrCorrupt)
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

// the ids are named once, so the budget can refuse them before SQLite materializes them
const matchShape = `select id,kind,length(label_ids),case when length(label_ids)<=? then label_ids else null end from (
	select s.id as id,s.kind as kind,s.label_ids as label_ids
	from (:postings) m join series s on s.id=m.series_id
) order by id limit cast(? as integer)`

func matchSeries(ctx context.Context, tx sqlite.Reader, matchers []Label, budget *queryBudget) ([]registeredSeries, error) {
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
		var source []byte
		var size int
		if err = rows.Scan(&series.id, &series.kind, &size, &source); err != nil {
			return nil, fmt.Errorf("read matched series: %w", err)
		}
		if err = budget.takeBytes(size); err != nil {
			return nil, err
		}
		if source == nil || (series.kind != Gauge && series.kind != Counter) {
			return nil, fmt.Errorf("%w: series metadata", ErrCorrupt)
		}
		if series.ids, err = decodeLabelIDs(source); err != nil {
			return nil, err
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
func fillLabels(ctx context.Context, tx sqlite.Reader, matched []registeredSeries, budget *queryBudget) error {
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
		labels := make([]Label, 0, len(matched[i].ids))
		for _, id := range matched[i].ids {
			label, found := wanted[id]
			if !found || label.Name == "" {
				return fmt.Errorf("%w: label %d is missing from the dictionary", ErrCorrupt, id)
			}
			labels = append(labels, label)
		}
		ordered, err := orderedLabels(labels, true)
		if err != nil {
			return fmt.Errorf("%w: registry labels: %w", ErrCorrupt, err)
		}
		matched[i].labels = ordered
	}
	return nil
}
