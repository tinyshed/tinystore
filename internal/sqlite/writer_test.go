package sqlite

import (
	"database/sql"
	"errors"
	"fmt"
	"path/filepath"
	"testing"
)

func TestPreparedWriterUsesOneTransactionAndRetainsPrograms(t *testing.T) {
	file, err := Open(t.Context(), filepath.Join(t.TempDir(), "writer.db"), Config{Readers: 1})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := file.Close(); err != nil {
			t.Error(err)
		}
	})
	if err := file.Migrate(t.Context(), 1234, testMigrations(`create table example(n integer) strict;`)); err != nil {
		t.Fatal(err)
	}
	const insert = `insert into example values(?)`
	failure := errors.New("reject transaction")
	if err := file.UpdatePrepared(t.Context(), func(writer Writer) error {
		if _, err := writer.ExecContext(t.Context(), insert, 1); err != nil {
			return err
		}
		return failure
	}); !errors.Is(err, failure) {
		t.Fatalf("rollback: %v", err)
	}
	statement := file.writerConn.kept(insert)
	if statement == nil {
		t.Fatal("writer did not retain the prepared insert")
	}
	if err := file.UpdatePrepared(t.Context(), func(writer Writer) error {
		if _, err := writer.ExecContext(t.Context(), insert, 2); err != nil {
			return err
		}
		var count int
		if err := QueryRow(t.Context(), writer, `select count(*) from example`).Scan(&count); err != nil {
			return err
		}
		if count != 1 {
			t.Fatalf("writer saw %d rows inside its transaction", count)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
	if statement != file.writerConn.kept(insert) {
		t.Fatal("writer prepared the same insert again")
	}
	if err := file.View(t.Context(), func(tx *sql.Tx) error {
		var count, value int
		if err := tx.QueryRowContext(t.Context(), `select count(*),max(n) from example`).Scan(&count, &value); err != nil {
			return err
		}
		if count != 1 || value != 2 {
			t.Fatalf("prepared write crossed transaction boundary: %d rows, max %d", count, value)
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}

func TestPreparedWriterCacheStaysBounded(t *testing.T) {
	file, err := Open(t.Context(), filepath.Join(t.TempDir(), "writer-cache.db"), Config{Readers: 1})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := file.Close(); err != nil {
			t.Error(err)
		}
	})
	if err := file.Migrate(t.Context(), 1234, testMigrations(`create table example(n integer) strict;`)); err != nil {
		t.Fatal(err)
	}
	if err := file.UpdatePrepared(t.Context(), func(writer Writer) error {
		for i := range 3 * keptStatements {
			query := fmt.Sprintf(`select ? + %d`, i)
			var value int
			if err := QueryRow(t.Context(), writer, query, 7).Scan(&value); err != nil {
				return err
			}
			if value != 7+i || len(file.writerConn.statements) > keptStatements {
				t.Fatalf("writer cache: value %d, statements %d", value, len(file.writerConn.statements))
			}
		}
		return nil
	}); err != nil {
		t.Fatal(err)
	}
}
