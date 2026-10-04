package metrics

import (
	"cmp"
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"slices"
	"strings"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// retention is how long each series is kept: Retention, or the keep of the
// longest prefix of RetentionOf that its name starts with
//
//	RetentionOf {"api_": 7d, "api_audit_": 365d}, Retention 30d
//	api_audit_logins → 365d    api_requests → 7d    cpu → 30d
type retention struct {
	rules    []retentionRule // the longest prefix first
	fallback int64           // Retention, in milliseconds
	longest  int64           // the longest of all, where a read across series starts
}

type retentionRule struct {
	prefix string
	keep   int64
}

func newRetention(opts Options) retention {
	r := retention{fallback: opts.Retention.Milliseconds()}
	r.longest = r.fallback
	for prefix, keep := range opts.RetentionOf {
		r.rules = append(r.rules, retentionRule{prefix: prefix, keep: keep.Milliseconds()})
		r.longest = max(r.longest, keep.Milliseconds())
	}
	slices.SortFunc(r.rules, func(a, b retentionRule) int {
		return cmp.Or(cmp.Compare(len(b.prefix), len(a.prefix)), strings.Compare(a.prefix, b.prefix))
	})
	return r
}

// keepOf is a name's retention in milliseconds, and whether a rule set it
// rather than Retention.
func (r retention) keepOf(name string) (int64, bool) {
	for _, rule := range r.rules {
		if strings.HasPrefix(name, rule.prefix) {
			return rule.keep, true
		}
	}
	return r.fallback, false
}

// cutoff is the oldest time a series of the name keeps at now.
func (r retention) cutoff(now int64, name string) int64 {
	keep, _ := r.keepOf(name)
	return earlier(now, keep)
}

// stored is a name's keep as series_state holds it: null for Retention, so
// that a changed Retention reaches its series without a write.
func (r retention) stored(name string) sql.NullInt64 {
	keep, ruled := r.keepOf(name)
	return sql.NullInt64{Int64: keep, Valid: ruled}
}

// cutoffOf is a series' cutoff from its stored keep.
func (r retention) cutoffOf(now int64, keep sql.NullInt64) int64 {
	if keep.Valid {
		return earlier(now, keep.Int64)
	}
	return earlier(now, r.fallback)
}

// applied is the rules as store_state keeps them, which the next open compares.
func (r retention) applied() (string, error) {
	rules := make(map[string]int64, len(r.rules))
	for _, rule := range r.rules {
		rules[rule.prefix] = rule.keep
	}
	encoded, err := json.Marshal(rules)
	if err != nil {
		return "", fmt.Errorf("encode the retention rules: %w", err)
	}
	return string(encoded), nil
}

const (
	appliedRetentionQuery = `select retention from store_state where id=1`
	applyRetentionQuery   = `update store_state set retention=? where id=1`
	namesFromQuery        = `select id, value from label_values where name='__name__' and value>=? order by value`
	keepNameQuery         = `
		update series_state set keep=?
		where series_id in (select series_id from postings where label_id=?)`
)

// resolveRetention gives each series the keep of RetentionOf as this open
// has it, when the rules differ from those last applied. It rewrites only the
// series whose names start with a prefix that came, went or changed its keep,
// found through the names' postings, so a store without such a change, or
// with a changed Retention, writes nothing.
func (s *Store) resolveRetention(ctx context.Context) error {
	applied, err := s.retention.applied()
	if err != nil {
		return err
	}
	return s.file.Update(ctx, func(tx *sql.Tx) error {
		var stored string
		if err := tx.QueryRowContext(ctx, appliedRetentionQuery).Scan(&stored); err != nil {
			return fmt.Errorf("read the retention applied: %w", err)
		}
		if stored == applied {
			return nil
		}
		changed, err := s.retention.changedSince(stored)
		if err != nil {
			return err
		}
		for _, prefix := range changed {
			if err = s.rekeep(ctx, tx, prefix); err != nil {
				return err
			}
		}
		if _, err = tx.ExecContext(ctx, applyRetentionQuery, applied); err != nil {
			return fmt.Errorf("keep the retention applied: %w", err)
		}
		return nil
	})
}

// changedSince is every prefix whose keep differs from the rules stored.
func (r retention) changedSince(stored string) ([]string, error) {
	var before map[string]int64
	if err := json.Unmarshal([]byte(stored), &before); err != nil {
		return nil, fmt.Errorf("%w: the retention applied: %w", ErrCorrupt, err)
	}
	var changed []string
	for _, rule := range r.rules {
		if keep, found := before[rule.prefix]; !found || keep != rule.keep {
			changed = append(changed, rule.prefix)
		}
		delete(before, rule.prefix)
	}
	for prefix := range before {
		changed = append(changed, prefix)
	}
	return changed, nil
}

// rekeep stores the keep of every series whose name starts with prefix.
func (s *Store) rekeep(ctx context.Context, tx *sql.Tx, prefix string) error {
	rows, err := tx.QueryContext(ctx, namesFromQuery, prefix) //nolint:rowserrcheck // EachRow checks Err
	if err != nil {
		return fmt.Errorf("find names of %q: %w", prefix, err)
	}
	names := map[int64]string{}
	err = sqlite.EachRow(rows, "names", func(rows *sql.Rows) error {
		var id int64
		var value string
		if scanErr := rows.Scan(&id, &value); scanErr != nil {
			return scanErr
		}
		if strings.HasPrefix(value, prefix) {
			names[id] = value
		}
		return nil
	})
	if err != nil {
		return fmt.Errorf("find names of %q: %w", prefix, err)
	}
	for id, value := range names {
		if _, err = tx.ExecContext(ctx, keepNameQuery, s.retention.stored(value), id); err != nil {
			return fmt.Errorf("keep the series of %s: %w", value, err)
		}
	}
	return nil
}
