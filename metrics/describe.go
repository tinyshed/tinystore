package metrics

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"unicode/utf8"

	"github.com/tinyshed/tinystore/internal/sqlite"
)

// Description is what the values of a metric's name mean: their unit, such
// as "ms", "bytes" or "%", and a line of help. It belongs to the name, so
// every series of the name shares it, and it outlives them.
type Description struct {
	Unit string
	Help string
}

// DescribeOption is a part of a Description, given to Describe or to an
// instrument.
type DescribeOption func(*Description)

func Unit(unit string) DescribeOption { return func(d *Description) { d.Unit = unit } }
func Help(help string) DescribeOption { return func(d *Description) { d.Help = help } }

const (
	maxUnitBytes = 32
	maxHelpBytes = 1024
)

const (
	describeQuery = `insert into descriptions(name, unit, help) values (?, ?, ?)
		on conflict(name) do update set unit = excluded.unit, help = excluded.help`
	undescribeQuery  = `delete from descriptions where name = ?`
	descriptionQuery = `select unit, help from descriptions where name = ?`
)

// Describe keeps a name's description in place of the one it had; without a
// unit or help it removes it.
func (s *Store) Describe(ctx context.Context, name string, options ...DescribeOption) error {
	if err := s.enter(ctx); err != nil {
		return err
	}
	defer s.leave()

	description := describedBy(options)
	if err := checkDescription(name, description); err != nil {
		return err
	}
	if err := s.keepDescriptions(ctx, map[string]Description{name: description}); err != nil {
		return fmt.Errorf("describe %s: %w", name, err)
	}
	return nil
}

// writeDescriptions writes what instruments were described with since the
// last flush, in one transaction; a failure keeps them for the next flush.
func (s *Store) writeDescriptions(ctx context.Context) error {
	taken := s.instruments.takeDescriptions()
	if len(taken) == 0 {
		return nil
	}
	err := ctx.Err() // an interrupted transaction would not say why
	if err == nil {
		err = s.keepDescriptions(ctx, taken)
	}
	if err != nil {
		s.instruments.giveBackDescriptions(taken)
		return fmt.Errorf("describe instruments: %w", err)
	}
	return nil
}

// keepDescriptions writes checked descriptions in one transaction.
func (s *Store) keepDescriptions(ctx context.Context, descriptions map[string]Description) error {
	return s.file.Update(ctx, func(tx *sql.Tx) error {
		for name, description := range descriptions {
			var err error
			if description == (Description{}) {
				_, err = tx.ExecContext(ctx, undescribeQuery, name)
			} else {
				_, err = tx.ExecContext(ctx, describeQuery, name, description.Unit, description.Help)
			}
			if err != nil {
				return err
			}
		}
		return nil
	})
}

// Description is what Describe kept for the name, or none.
func (s *Store) Description(ctx context.Context, name string) (Description, error) {
	if err := s.enter(ctx); err != nil {
		return Description{}, err
	}
	defer s.leave()

	if err := checkDescription(name, Description{}); err != nil {
		return Description{}, err
	}
	var description Description
	err := s.file.Lookup(ctx, func(r sqlite.Reader) error {
		return sqlite.QueryRowByKey(ctx, r, descriptionQuery, name).Scan(&description.Unit, &description.Help)
	})
	if errors.Is(err, sql.ErrNoRows) {
		return Description{}, nil
	}
	if err != nil {
		return Description{}, fmt.Errorf("description of %s: %w", name, err)
	}
	return description, nil
}

func describedBy(options []DescribeOption) Description {
	var description Description
	for _, option := range options {
		option(&description)
	}
	return description
}

func checkDescription(name string, description Description) error {
	switch {
	case name == "" || len(name) > maxLabelValueBytes || !utf8.ValidString(name):
		return fmt.Errorf("%w: metric name %q", ErrInvalid, name)
	case len(description.Unit) > maxUnitBytes || !utf8.ValidString(description.Unit):
		return fmt.Errorf("%w: the unit of %s: %d bytes of UTF-8 at most", ErrInvalid, name, maxUnitBytes)
	case len(description.Help) > maxHelpBytes || !utf8.ValidString(description.Help):
		return fmt.Errorf("%w: the help of %s: %d bytes of UTF-8 at most", ErrInvalid, name, maxHelpBytes)
	}
	return nil
}
