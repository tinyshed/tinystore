package metrics

import (
	"context"
	"encoding/json"
	"fmt"
	"slices"
	"strings"
	"unicode/utf8"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Condition is what a label's value must be for a range to take a series,
// beyond the equality of Match:
//
//	Where{"status": OneOf("500", "502")}   status is 500 or 502
//	Where{"env": NoneOf("dev", "test")}    env is neither, or the series has none
//	Where{"host": Prefix("api-")}          host begins with api-
//
// A range keeps something that finds its series, a Name, a Match, a OneOf or
// a Prefix: NoneOf only leaves series out, since alone it would scan them all.
type Condition struct {
	kind   conditionKind
	values []string
}

// Where is the conditions of a range, a label's name to its condition.
type Where map[string]Condition

type conditionKind uint8

const (
	conditionOneOf conditionKind = iota + 1
	conditionNoneOf
	conditionPrefix
)

// the values a OneOf or a NoneOf may list, each one statement's variable
const maxConditionValues = 1000

func OneOf(values ...string) Condition {
	return Condition{kind: conditionOneOf, values: values}
}

func NoneOf(values ...string) Condition {
	return Condition{kind: conditionNoneOf, values: values}
}

func Prefix(prefix string) Condition {
	return Condition{kind: conditionPrefix, values: []string{prefix}}
}

// condition is a checked Where entry, its values in byte order.
type condition struct {
	name   string
	kind   conditionKind
	values []string
}

// checkWhere refuses a condition no series could be found by, and one on a
// label Match names too.
func checkWhere(where Where, matchers []label) ([]condition, error) {
	conditions := make([]condition, 0, len(where))
	for name, given := range where {
		if err := checkCondition(name, given, matchers); err != nil {
			return nil, err
		}
		values := slices.Clone(given.values)
		slices.Sort(values)
		conditions = append(conditions, condition{name: name, kind: given.kind, values: slices.Compact(values)})
	}
	slices.SortFunc(conditions, func(a, b condition) int { return strings.Compare(a.name, b.name) })
	return conditions, nil
}

func checkCondition(name string, given Condition, matchers []label) error {
	switch {
	case strings.HasPrefix(name, "__"):
		return fmt.Errorf("%w: label %q: a name beginning with __ is the store's own", ErrInvalid, name)
	case name == "" || len(name) > maxLabelNameBytes || !utf8.ValidString(name):
		return fmt.Errorf("%w: a condition's label name, encoding or size", ErrInvalid)
	case given.kind == 0:
		return fmt.Errorf("%w: label %q: a condition is made by OneOf, NoneOf or Prefix", ErrInvalid, name)
	case len(given.values) == 0 || len(given.values) > maxConditionValues:
		return fmt.Errorf("%w: label %q: a condition lists 1 to %d values", ErrInvalid, name, maxConditionValues)
	case given.kind == conditionPrefix && given.values[0] == "":
		return fmt.Errorf("%w: label %q: a prefix of nothing", ErrInvalid, name)
	case slices.ContainsFunc(matchers, func(matcher label) bool { return matcher.Name == name }):
		return fmt.Errorf("%w: label %q is matched and has a condition", ErrInvalid, name)
	}
	for _, value := range given.values {
		if len(value) > maxLabelValueBytes || !utf8.ValidString(value) {
			return fmt.Errorf("%w: label %q: a condition's value encoding or size", ErrInvalid, name)
		}
	}
	return nil
}

// finds says whether a Where holds a condition that finds series, rather than
// only leaving them out.
func finds(where Where) bool {
	for _, given := range where {
		if given.kind == conditionOneOf || given.kind == conditionPrefix {
			return true
		}
	}
	return false
}

// postingSet is the postings a match drives from or checks a series against:
// one label pair, several values of one label, or every value with a prefix.
type postingSet struct {
	ids    []int64
	prefix *condition
	count  int // the series it holds, so the smallest drives
}

// membership is the set as SQL's test of a postings row's label_id.
func (p postingSet) membership(column string) (string, []any) {
	if p.prefix != nil {
		end := prefixEnd(p.prefix.values[0])
		return column + ` in (select id from label_values where name=? and value>=? and value<?)`,
			[]any{p.prefix.name, p.prefix.values[0], end}
	}
	arguments := make([]any, len(p.ids))
	for i, id := range p.ids {
		arguments[i] = id
	}
	return column + ` in (` + placeholders("?", len(p.ids)) + `)`, arguments
}

// prefixEnd is the least value after every value that begins with prefix, in
// SQLite's byte order. A label is UTF-8, whose bytes never reach 0xff, so the
// last byte always has one above it:
//
//	"api-"  →  "api."
func prefixEnd(prefix string) string {
	end := []byte(prefix)
	end[len(end)-1]++
	return string(end)
}

// resolveSelection turns a match's pairs and conditions into the sets that
// find its series and those that leave series out. The third result is false
// when a set holds no series, which makes the whole match empty.
func resolveSelection(
	ctx context.Context, tx sqlite.Reader, matchers []label, conditions []condition,
) (finding, leaving []postingSet, possible bool, err error) {
	if len(matchers) > 0 {
		ranked, possible, err := countEachMatcher(ctx, tx, matchers)
		if err != nil || !possible {
			return nil, nil, false, err
		}
		for _, posting := range ranked {
			finding = append(finding, postingSet{ids: []int64{posting.labelID}, count: posting.seriesCount})
		}
	}
	for i := range conditions {
		set, err := resolveCondition(ctx, tx, &conditions[i])
		switch {
		case err != nil:
			return nil, nil, false, err
		case conditions[i].kind == conditionNoneOf:
			if len(set.ids) > 0 {
				leaving = append(leaving, set)
			}
		case set.count == 0:
			return nil, nil, false, nil
		default:
			finding = append(finding, set)
		}
	}
	slices.SortStableFunc(finding, func(a, b postingSet) int { return a.count - b.count })
	return finding, leaving, true, nil
}

const prefixPostingsQuery = `select coalesce(sum(posting_count), 0) from label_values
	where name=? and value>=? and value<?`

// one program for any number of values, which json_each hands to the join
const valuesPostingsQuery = `select v.id, v.posting_count from json_each(?) j
	join label_values v on v.name=? and v.value=j.value`

func resolveCondition(ctx context.Context, tx sqlite.Reader, given *condition) (postingSet, error) {
	if given.kind == conditionPrefix {
		set := postingSet{prefix: given}
		err := sqlite.QueryRow(ctx, tx, prefixPostingsQuery, given.name, given.values[0], prefixEnd(given.values[0])).
			Scan(&set.count)
		if err != nil {
			return postingSet{}, fmt.Errorf("resolve the prefix of %q: %w", given.name, err)
		}
		return set, nil
	}

	encoded, err := json.Marshal(given.values)
	if err != nil {
		return postingSet{}, fmt.Errorf("encode the values of %q: %w", given.name, err)
	}
	rows, err := tx.QueryContext(ctx, valuesPostingsQuery, string(encoded), given.name)
	if err != nil {
		return postingSet{}, fmt.Errorf("resolve the values of %q: %w", given.name, err)
	}
	defer rows.Close()
	var set postingSet
	for rows.Next() {
		var id int64
		var count int
		if err = rows.Scan(&id, &count); err != nil {
			return postingSet{}, fmt.Errorf("read a value of %q: %w", given.name, err)
		}
		set.ids = append(set.ids, id)
		set.count += count
	}
	return set, rows.Err()
}

// selectionFilter drives from the smallest set that finds series, as
// postingsFilter does, checks each other, and leaves out the series a
// NoneOf names:
//
//	OneOf(500, 502) of 40 series, host=web-1 of 900, env NoneOf(dev)
//	  → from status's 40, each in host's set, none in env=dev's
func selectionFilter(finding, leaving []postingSet) (string, []any) {
	var query strings.Builder
	membership, arguments := finding[0].membership("p.label_id")
	query.WriteString(`select p.series_id as series_id from postings p where ` + membership)
	for _, set := range finding[1:] {
		membership, more := set.membership("q.label_id")
		query.WriteString(` and exists(select 1 from postings q where ` + membership + ` and q.series_id=p.series_id)`)
		arguments = append(arguments, more...)
	}
	for _, set := range leaving {
		membership, more := set.membership("q.label_id")
		query.WriteString(` and not exists(select 1 from postings q where ` + membership +
			` and q.series_id=p.series_id)`)
		arguments = append(arguments, more...)
	}
	return query.String(), arguments
}
