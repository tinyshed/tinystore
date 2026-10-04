package sqldb

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"testing"
	"time"

	"github.com/tinyshed/tinystore"
)

var accountsMigrations = mapFS(map[string]string{
	"001_accounts.sql": `create table users (login text primary key, password text not null) strict;
create table sessions (token text primary key, login text not null) strict;
insert into users values ('ann', 'old');
insert into sessions values ('t1', 'ann'), ('t2', 'ann');`,
})

// TestGuestOwnerProcess is the owner's half of TestAGuestWritesBesideTheOwner:
// it holds the store in a process of its own and answers read until quit.
func TestGuestOwnerProcess(t *testing.T) {
	dir := os.Getenv("TINYSTORE_GUEST_OWNER")
	if dir == "" {
		t.Skip("the owner's half of TestAGuestWritesBesideTheOwner")
	}
	ctx := t.Context()
	db, err := Open(ctx, openStore(t, dir), "app", accountsMigrations, nil)
	if err != nil {
		t.Fatal(err)
	}
	fmt.Println("ready")
	for lines := bufio.NewScanner(os.Stdin); lines.Scan() && lines.Text() == "read"; {
		password, _ := Scalar[string](ctx, db, `select password from users where login = 'ann'`)
		sessions, _ := Scalar[int](ctx, db, `select count(*) from sessions where login = 'ann'`)
		fmt.Println(password, sessions)
	}
}

// a second process opens the store another holds, as a guest, and its batch
// is one commit the owner reads at once: a password changed and the sessions
// it ends, as a recovery command beside a running server does
func TestAGuestWritesBesideTheOwner(t *testing.T) {
	dir := t.TempDir()
	owner := exec.CommandContext(t.Context(), os.Args[0], "-test.run=^TestGuestOwnerProcess$", "-test.count=1")
	owner.Env = append(os.Environ(), "TINYSTORE_GUEST_OWNER="+dir)
	owner.Stderr = os.Stderr
	toOwner, err := owner.StdinPipe()
	if err != nil {
		t.Fatal(err)
	}
	fromOwner, err := owner.StdoutPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err = owner.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		_ = toOwner.Close()
		_ = owner.Wait()
	})
	answers := ownerLines(fromOwner)
	if line := nextLine(t, answers); line != "ready" {
		t.Fatalf("the owner said %q", line)
	}

	if _, err = tinystore.Open(t.Context(), dir, tinystore.Options{}); !errors.Is(err, tinystore.ErrInUse) {
		t.Fatalf("a second owner: %v", err)
	}
	guest := openStoreWith(t, dir, tinystore.Options{Guest: true})
	db, err := Open(t.Context(), guest, "app", accountsMigrations, nil)
	if err != nil {
		t.Fatal(err)
	}
	err = db.Batch(t.Context(), func(b *Batch) error {
		b.Exec(`update users set password = 'new' where login = 'ann'`)
		b.Exec(`delete from sessions where login = 'ann'`)
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if _, err = fmt.Fprintln(toOwner, "read"); err != nil {
		t.Fatal(err)
	}
	if line := nextLine(t, answers); line != "new 0" {
		t.Fatalf("the owner reads %q, want the new password and no sessions", line)
	}
}

func ownerLines(r io.Reader) <-chan string {
	lines := make(chan string)
	go func() {
		defer close(lines)
		for scanner := bufio.NewScanner(r); scanner.Scan(); {
			if text := strings.TrimSpace(scanner.Text()); text != "" && !strings.HasPrefix(text, "=== ") {
				lines <- text
			}
		}
	}()
	return lines
}

func nextLine(t *testing.T, lines <-chan string) string {
	t.Helper()
	select {
	case line, ok := <-lines:
		if !ok {
			t.Fatal("the owner ended")
		}
		return line
	case <-time.After(30 * time.Second):
		t.Fatal("the owner said nothing for 30 seconds")
	}
	return ""
}

// a guest applies no migration: a binary newer than the owner's is told to
// let the owner apply it first
func TestAGuestIsRefusedWhileAMigrationIsPending(t *testing.T) {
	dir := t.TempDir()
	if _, err := Open(t.Context(), openStore(t, dir), "app", accountsMigrations, nil); err != nil {
		t.Fatal(err)
	}
	guest := openStoreWith(t, dir, tinystore.Options{Guest: true})
	newer := mapFS(map[string]string{
		"001_accounts.sql": string(must(accountsMigrations.ReadFile("001_accounts.sql"))),
		"002_owners.sql":   `alter table users add column owner integer not null default 0;`,
	})
	if _, err := Open(t.Context(), guest, "app", newer, nil); !errors.Is(err, ErrPending) || !errors.Is(err, tinystore.ErrInvalid) {
		t.Fatalf("a guest with a migration the owner has not applied: %v", err)
	}
	if _, err := Open(t.Context(), guest, "missing", accountsMigrations, nil); err == nil {
		t.Fatal("a guest made a database")
	}
}

// a guest changes rows, not the schema the owner checked, nor the tables the
// store keeps for itself
func TestAGuestWritesNoneOfTheStoresTables(t *testing.T) {
	dir := t.TempDir()
	if _, err := Open(t.Context(), openStore(t, dir), "app", accountsMigrations, nil); err != nil {
		t.Fatal(err)
	}
	db, err := Open(t.Context(), openStoreWith(t, dir, tinystore.Options{Guest: true}), "app", accountsMigrations, nil)
	if err != nil {
		t.Fatal(err)
	}
	for _, refused := range []string{
		`delete from _tinystore_migrations`,
		`create table more (id integer)`,
		`drop table sessions`,
		`create index users_password on users (password)`,
	} {
		if _, err = db.Exec(t.Context(), refused); !errors.Is(err, tinystore.ErrInvalid) ||
			!strings.Contains(err.Error(), "a guest changes no schema") {
			t.Errorf("%s: %v", refused, err)
		}
	}
	if _, err = db.Exec(context.WithoutCancel(t.Context()), `insert into users values ('bob', 'x')`); err != nil {
		t.Fatalf("a guest's write of the application's rows: %v", err)
	}
}

func TestAGuestCannotRewriteTheSchema(t *testing.T) {
	dir := t.TempDir()
	owner := openStore(t, dir)
	if _, err := Open(t.Context(), owner, "app", accountsMigrations, nil); err != nil {
		t.Fatal(err)
	}
	guest := openStoreWith(t, dir, tinystore.Options{Guest: true})
	db, err := Open(t.Context(), guest, "app", accountsMigrations, nil)
	if err != nil {
		t.Fatal(err)
	}
	for _, query := range []string{
		"pragma writable_schema=on",
		"pragma schema_version=999",
		"pragma application_id=0",
		"pragma user_version=999",
		"delete from sqlite_schema where name='users'",
	} {
		if _, execErr := db.Exec(t.Context(), query); !errors.Is(execErr, tinystore.ErrInvalid) {
			t.Errorf("%s: %v, want ErrInvalid", query, execErr)
		}
	}
	count, err := Scalar[int](t.Context(), db, "select count(*) from sqlite_schema where name='users'")
	if err != nil || count != 1 {
		t.Fatalf("the guest changed the owner's schema: %d tables: %v", count, err)
	}
}

func must[T any](value T, err error) T {
	if err != nil {
		panic(err)
	}
	return value
}
