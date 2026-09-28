package sqldb

import (
	"context"
	"fmt"
	"strings"

	"github.com/tinyshed/tinystore"
	"github.com/tinyshed/tinystore/internal/sqlite"
	"github.com/tinyshed/tinystore/sqldb/internal/catalog"
)

// check compares the file with schema as SQLite describes both, the schema in
// a database in memory made from its SQL: a difference of structure refuses
// the file, and text, a CHECK or a default's expression, is never compared
func (d *DB) check(ctx context.Context, schema *SchemaDef) error {
	declared, err := catalog.Declare(ctx, schema.SQL())
	if err != nil {
		return fmt.Errorf("%w: sql %q: %w", tinystore.ErrInvalid, d.name, err)
	}
	var file *catalog.Catalog
	err = d.file.ViewPrepared(ctx, func(r sqlite.Reader) (readErr error) {
		file, readErr = catalog.Read(ctx, r)
		return readErr
	})
	if err != nil {
		return fmt.Errorf("sql %q: read the file's schema: %w", d.name, err)
	}

	var differences []string
	for _, difference := range catalog.Compare(declared, file) {
		if difference.Structural() {
			differences = append(differences, "  "+difference.Sentence(schema.model(difference.Table), catalog.File))
		}
	}
	if len(differences) > 0 {
		return fmt.Errorf("%w: sql %q: the file does not match the schema:\n%s",
			tinystore.ErrInvalid, d.name, strings.Join(differences, "\n"))
	}
	return nil
}
