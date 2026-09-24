# sqldb

The application's own SQLite databases, inside a `tinystore.Store`: each is
`sql/<name>.db`, the application writes the schema and the SQL, and the engine
owns the file, its connections, its migrations and its transactions.

```go
//go:embed migrations/*.sql
var files embed.FS

migrations, _ := fs.Sub(files, "migrations")
app, err := sqldb.Open(ctx, store, "app", migrations) // data/sql/app.db

note, err := sqldb.One[Note](ctx, app, `select id, title from notes where id = ?`, id)
notes, err := sqldb.All[Note](ctx, app, `select id, title from notes order by id limit ?`, 50)
count, err := sqldb.Scalar[int](ctx, app, `select count(*) from notes`)

id, err := sqldb.ExecScalar[int64](ctx, app, `insert into notes (title) values (?) returning id`, title)
note, err = sqldb.ExecOne[Note](ctx, app, `update notes set title = ? where id = ? returning id, title`, title, id)
result, err := app.Exec(ctx, `delete from notes where id = ?`, id)

err = app.Tx(ctx, func(tx *sqldb.Tx) error {
	id, err := sqldb.ExecScalar[int64](ctx, tx, `insert into notes (title) values (?) returning id`, title)
	if err != nil {
		return err
	}
	_, err = tx.Exec(ctx, `insert into audit (note_id) values (?)`, id)
	return err
})
```

## Contracts

- What a call's name starts with says where it runs. `Exec`, `ExecOne`,
  `ExecAll` and `ExecScalar` may write and run on the file's one writer, each
  in a transaction of its own unless given a `*Tx`. `One`, `All` and `Scalar`
  read, each from its own snapshot, on readers that refuse to write: a write
  sent to them fails before writing anything, with `tinystore.ErrInvalid`
  naming the call to use.
- `One` and `ExecOne`: no row is `sql.ErrNoRows`, two are `ErrManyRows`.
  `Scalar` reads one column; asked for a struct it is `ErrShape`, as is a
  column no field takes.
- A struct takes each column by its `db` tag, or by its field name in
  snake_case (`CreatedAt` → `created_at`); embedded structs' fields count, and
  `db:"-"` skips a field. `time.Time` and any `sql.Scanner` are one column.
- `Tx`: nil commits, an error or a panic rolls back. Calls inside take the
  `*Tx`, not the `*DB`.
- Migrations are the `*.sql` files of the given `fs.FS`, applied in name order,
  all pending ones in one transaction, their checksums verified on every open.
  A migration changed after it was applied, a database newer than the binary's
  migrations, or another engine's file refuses to open.
- A name is `[a-z0-9][a-z0-9_-]{0,63}`; a second `Open` of a name in one store
  is `tinystore.ErrInUse`. After the store closes, every call is
  `tinystore.ErrClosed`.
- The typed calls are package functions taking a `*DB` or a `*Tx`, like pgx's
  and sqlc's, so no importer needs generic methods.
