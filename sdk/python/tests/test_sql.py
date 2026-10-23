"""sql as docs/guides/sql/README.md has it, through each way a store is reached.

Migrations checked at every open, the verbs that say what comes back, writes,
batches and transactions, tables and their queries, and the values between
Python and SQLite.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass, field
from datetime import UTC, datetime
from typing import TYPE_CHECKING, Any, TypedDict

import pytest

import tinystore
from tinystore import sql
from ways import until

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable
    from pathlib import Path

NOTES = """create table notes (
    id         text primary key,
    author_id  integer not null,
    title      text not null,
    done       integer not null default 0 check (done in (0, 1)),
    tags       text not null default '[]' check (json_valid(tags)),
    created_at integer not null default 0,
    raw        blob
) strict"""


@dataclass
class Note:
    id: str
    author_id: int
    title: str
    done: bool = False
    tags: list[str] = field(default_factory=list[str])
    created_at: datetime = datetime(1970, 1, 1, tzinfo=UTC)
    raw: bytes | None = None


class Title(TypedDict):
    title: str


@pytest.fixture
async def app(store: tinystore.Store, directory: Path) -> tinystore.Database:
    migrations = directory / "migrations"
    migrations.mkdir()
    (migrations / "0001_notes.sql").write_text(NOTES)
    return await store.database("app", migrations=migrations)


async def test_a_write_returns_once_it_is_durable_and_the_verbs_say_what_comes_back(app: tinystore.Database) -> None:
    written = await app.exec("insert into notes (id, author_id, title) values (?, ?, ?)", "n1", 42, "Buy milk")
    assert written.changes == 1
    await app.exec("insert into notes (id, author_id, title) values (?, ?, ?)", "n2", 42, "Call mom")
    mine = await app.all(Note, "select * from notes where author_id = ? order by id", 42)
    assert [note.title for note in mine] == ["Buy milk", "Call mom"]
    assert await app.one("select * from notes where id = ?", "none") is None
    assert await app.scalar(int, "select count(*) from notes") == 2
    named = await app.one("select * from notes where id = :id and author_id = :author", {"id": "n2", "author": 42})
    assert named is not None and named["title"] == "Call mom"
    done = await app.one(Note, "update notes set done = 1 where id = ? returning *", "n1")
    assert done is not None and done.done is True
    with pytest.raises(tinystore.InvalidError):
        await app.one("update notes set done = 1 returning id")
    assert await app.scalar("select count(*) from notes where done = 1") == 1


async def test_a_key_already_held_is_a_conflict_naming_the_database_and_the_constraint(
    app: tinystore.Database,
) -> None:
    await app.exec("insert into notes (id, author_id, title) values ('k1', 1, 'a')")
    with pytest.raises(tinystore.ConflictError, match="sql app") as taken:
        await app.exec("insert into notes (id, author_id, title) values ('k1', 1, 'b')")
    assert taken.value.constraint == tinystore.Constraint("primaryKey", "notes", ["id"], None)

    async def bad(tx: tinystore.SqlTx) -> None:
        await tx.exec("insert into notes (id, author_id, title, done) values ('k2', 1, 'b', 7)")

    with pytest.raises(tinystore.InvalidError) as checked:
        await app.tx(bad)
    assert checked.value.constraint == tinystore.Constraint("check", None, [], "done in (0, 1)")
    with pytest.raises(tinystore.TinystoreError, match="no_such_table") as missing:
        await app.all("select * from no_such_table")
    assert missing.value.constraint is None


async def test_a_batch_writes_all_of_its_statements_or_none(app: tinystore.Database) -> None:
    done = await app.batch(
        [
            sql("insert into notes (id, author_id, title) values (?, 7, 'a')", "b1"),
            sql("insert into notes (id, author_id, title) values (?, ?, ?)", "b2", 7, "b"),
        ]
    )
    assert [each.changes for each in done] == [1, 1]
    with pytest.raises(tinystore.ConflictError):
        await app.batch(
            [
                sql("insert into notes (id, author_id, title) values ('b3', 7, 'c')"),
                sql("insert into notes (id, author_id, title) values ('b1', 7, 'taken')"),
            ]
        )
    assert await app.scalar(int, "select count(*) from notes where author_id = 7") == 2


async def test_a_statement_changes_nothing_of_the_connection_it_runs_on(app: tinystore.Database) -> None:
    before = await app.scalar(int, "select count(*) from notes")
    for text in ("attach database 'other.db' as other", "commit", "pragma query_only = off"):
        with pytest.raises(tinystore.InvalidError):
            await app.all(sql(text))
        with pytest.raises(tinystore.InvalidError):
            await app.exec(sql(text))
    kept = sql("insert into notes (id, author_id, title) values ('g1', 11, 'before the commit')")
    with pytest.raises(tinystore.InvalidError):
        await app.batch([kept, sql("commit")])
    assert await app.scalar(int, "select count(*) from notes") == before
    assert len(await app.all("pragma table_info(notes)")) > 0


async def test_a_transaction_sees_its_own_writes_commits_what_it_returns_and_rolls_back_an_exception(
    app: tinystore.Database,
) -> None:
    async def write(tx: tinystore.SqlTx) -> list[Title]:
        await tx.exec("insert into notes (id, author_id, title) values ('t1', 9, 'a')")
        with pytest.raises(tinystore.ConflictError):
            await tx.exec("insert into notes (id, author_id, title) values ('t1', 9, 'b')")
        return await tx.all(Title, "select title from notes where author_id = 9")

    assert await app.tx(write) == [{"title": "a"}]
    assert await app.scalar(int, "select count(*) from notes where author_id = 9") == 1

    async def sold_out(tx: tinystore.SqlTx) -> None:
        await tx.exec("delete from notes where author_id = 9")
        raise RuntimeError("sold out")

    with pytest.raises(RuntimeError, match="sold out"):
        await app.tx(sold_out)
    assert await app.scalar(int, "select count(*) from notes where author_id = 9") == 1


async def test_a_value_goes_as_sqlite_keeps_it_and_one_it_would_change_is_refused(app: tinystore.Database) -> None:
    raw = bytes([0, 255, 7])
    at = datetime(2026, 10, 10, 12, 0, 0, 123000, tzinfo=UTC)
    await app.exec(
        "insert into notes (id, author_id, title, done, tags, created_at, raw) values (?, ?, ?, ?, ?, ?, ?)",
        "v1",
        2**60,
        "x",
        True,
        sql.json(["home"]),
        at,
        raw,
    )
    kept = await app.one("select * from notes where id = ?", "v1")
    assert kept is not None
    assert (kept["author_id"], kept["done"], kept["tags"], kept["raw"]) == (2**60, 1, '["home"]', raw)
    assert kept["created_at"] == 1791633600123
    typed = await app.one(Note, "select * from notes where id = ?", "v1")
    assert typed is not None and (typed.done, typed.tags, typed.created_at) == (True, ["home"], at)
    for refused in (["a"], {"a": 1}, 2**64, float("nan"), datetime(2026, 1, 1)):
        with pytest.raises(tinystore.InvalidError):
            await app.all("select ?", refused)
    with pytest.raises(tinystore.InvalidError):
        await app.all("select ?, ?", 1)


async def test_a_database_opens_without_migrations_and_refuses_a_migration_changed_after_it_was_applied(
    store: tinystore.Store, app: tinystore.Database
) -> None:
    scratch = await store.database("scratch")
    assert await scratch.scalar("select 1") == 1
    with pytest.raises(tinystore.InvalidError, match=r"0001_notes\.sql changed after it was applied"):
        await store.database("app", migrations={"0001_notes.sql": f"{NOTES};"})


async def test_each_reads_a_query_past_one_message_a_part_at_a_time(app: tinystore.Database) -> None:
    for batch in range(3):
        await app.batch(
            [
                sql("insert into notes (id, author_id, title) values (?, 3, ?)", f"e{batch}-{n}", "x" * 1000)
                for n in range(1000)
            ]
        )
    count = 0
    async for note in app.each(Note, "select * from notes where author_id = 3 order by id"):
        assert len(note.title) == 1000
        count += 1
    assert count == 3000


async def test_a_statement_gives_by_its_columns_what_it_gives_by_its_rows(app: tinystore.Database) -> None:
    await app.exec("create table samples (ts integer not null, cpu real, hits integer, host text, raw blob) strict")
    await app.batch(
        [
            sql("insert into samples values (?, ?, ?, ?, ?)", 1000, 0.5, 7, "a", b"\x01"),
            sql("insert into samples values (?, ?, ?, ?, ?)", 2000, None, None, None, None),
            sql("insert into samples values (?, ?, ?, ?, ?)", 3000, 2.5, 9, "c", None),
        ]
    )
    read = sql("select ts, cpu, hits, host, raw, coalesce(cpu, 0) as either from samples order by ts")
    columns = await app.columns(read)
    assert columns == {
        "ts": [1000, 2000, 3000],
        "cpu": [0.5, None, 2.5],
        "hits": [7, None, 9],
        "host": ["a", None, "c"],
        "raw": [b"\x01", None, None],
        "either": [0.5, 0, 2.5],
    }
    rows = await app.all(read)
    for name, column in columns.items():
        assert column == [row[name] for row in rows]
    assert await app.columns("insert into samples (ts, cpu) values (4000, 4.5) returning ts, cpu") == {
        "ts": [4000],
        "cpu": [4.5],
    }
    with pytest.raises(tinystore.InvalidError, match="two columns named ts"):
        await app.columns("select ts, ts from samples")
    assert await app.columns("select ts, cpu from samples where 0") == {"ts": [], "cpu": []}
    for integer in (0, 127, -33, 65_536, 2**53, -(2**63), 2**63 - 1):
        assert await app.scalar("select ?", integer) == integer


async def test_columns_past_one_message_come_in_parts_and_are_whole(app: tinystore.Database) -> None:
    rows = 150_000
    columns = await app.columns(
        """with recursive n(i) as (select 1 union all select i + 1 from n where i < ?)
        select i, i / 2.0 as half, iif(i % 2, null, i) as even, 'r' || i as name from n""",
        rows,
    )
    assert [len(column) for column in columns.values()] == [rows] * 4
    for row in (0, 1, 77, 65_535, 65_536, 149_998, 149_999):
        assert columns["i"][row] == row + 1
        assert columns["half"][row] == (row + 1) / 2
        assert columns["even"][row] == (None if row % 2 == 0 else row + 1)
        assert columns["name"][row] == f"r{row + 1}"


async def test_a_query_past_its_bound_is_a_limit_which_names_the_bound(app: tinystore.Database) -> None:
    with pytest.raises(tinystore.LimitError) as error:
        await app.all("select zeroblob(70 * 1024 * 1024)")
    assert (error.value.limit, error.value.bound) == ("bytes of a call's rows", 64 * 1024 * 1024)
    assert error.value.wanted is not None and error.value.wanted > 64 * 1024 * 1024


async def test_a_table_inserts_rows_and_gives_them_back_in_their_types(app: tinystore.Database) -> None:
    notes = app.table("notes", Note)
    at = datetime.fromtimestamp(1_791_547_200.123, tz=UTC)
    note = await notes.insert(Note("tb1", 5, "Buy milk", tags=["home"], created_at=at))
    assert note == Note("tb1", 5, "Buy milk", False, ["home"], at, None)
    two = await notes.insert([Note("tb2", 5, "Call mom", True, [], at), Note("tb3", 5, "Read", False, ["books"], at)])
    assert [row.done for row in two] == [True, False]
    assert await notes.where(author_id=5, done=False).count() == 2
    titles = await notes.where(author_id=5).order_by("id", "desc").select(Title, "title").all()
    assert titles == [{"title": "Read"}, {"title": "Call mom"}, {"title": "Buy milk"}]
    with pytest.raises(tinystore.ConflictError):
        await notes.insert(Note("tb1", 5, "again", created_at=at))
    seen = [row async for row in notes.where(author_id=5).where(sql.has("tags", "home")).each()]
    assert [row.tags for row in seen] == [["home"]]


async def test_an_update_or_a_delete_changes_what_its_condition_says_and_refuses_every_row(
    app: tinystore.Database,
) -> None:
    notes = app.table("notes", Note)
    await notes.insert(Note("ud1", 6, "a"))
    await notes.insert(Note("ud2", 6, "b"))
    updated = await notes.where(id="ud1").update(done=True, title=sql("title || '!'"))
    assert updated.changes == 1
    changed = await notes.where(id="ud1").one()
    assert changed is not None and (changed.done, changed.title) == (True, "a!")
    with pytest.raises(tinystore.InvalidError):
        await notes.delete()
    with pytest.raises(tinystore.InvalidError):
        await notes.where({}).delete()
    with pytest.raises(tinystore.InvalidError):
        await notes.where(sql.and_()).update(title="x")
    assert (await notes.where(author_id=6, done=True).delete()).changes == 1
    assert await notes.where(author_id=6).count() == 1


async def test_an_upsert_sets_only_what_it_names_on_the_row_its_owner_holds(app: tinystore.Database) -> None:
    notes = app.table("notes", Note)
    made = await notes.upsert(Note("up1", 1, "mine"), conflict="id", update=["title"], where={"author_id": 1})
    assert made is not None and made.title == "mine"
    changed = await notes.upsert(
        Note("up1", 1, "still mine", done=True), conflict="id", update=["title"], where={"author_id": 1}
    )
    assert changed is not None and (changed.title, changed.done) == ("still mine", False)
    stolen = await notes.upsert(Note("up1", 2, "stolen"), conflict="id", update=["title"], where={"author_id": 2})
    assert stolen is None
    kept = await notes.where(id="up1").one()
    assert kept is not None and kept.title == "still mine"


async def test_a_page_reads_after_the_last_row_of_the_one_before_and_only_its_own_query(
    app: tinystore.Database,
) -> None:
    notes = app.table("notes", Note)
    for n in range(5):
        await notes.insert(Note(f"pg{n}", 8, str(n), created_at=datetime.fromtimestamp(1 + n // 2, tz=UTC)))
    newest = notes.where(author_id=8).order_by("created_at", "desc").order_by("id", "desc")
    first = await newest.list(limit=2)
    second = await newest.list(limit=2, after=first.next)
    third = await newest.list(limit=2, after=second.next)
    assert [row.id for page in (first, second, third) for row in page.rows] == ["pg4", "pg3", "pg2", "pg1", "pg0"]
    assert third.next is None
    other = notes.where(author_id=9).order_by("created_at", "desc").order_by("id", "desc")
    with pytest.raises(tinystore.InvalidError):
        await other.list(limit=2, after=first.next)


async def test_a_join_reads_its_first_table_a_query_in_a_piece_is_a_subquery_and_a_plan_explains_it(
    store: tinystore.Store,
) -> None:
    library = await store.database(
        "library",
        migrations={
            "0001_books.sql": """create table authors (id integer primary key, name text not null,
                active integer not null) strict;
            create table books (id integer primary key, author_id integer not null, title text not null) strict;"""
        },
    )
    await library.batch(
        [
            sql("insert into authors (id, name, active) values (1, 'Le Guin', 1), (2, 'Lem', 0)"),
            sql("insert into books (id, author_id, title) values (1, 1, 'The Dispossessed'), (2, 2, 'Solaris')"),
        ]
    )
    books = await library.table("books as b").join("authors as a", "a.id = b.author_id").where({"a.active": 1}).all()
    assert books == [{"id": 1, "author_id": 1, "title": "The Dispossessed"}]
    written = library.table("books as b").select("1").where("b.author_id = a.id")
    writers = await library.table("authors as a").where(sql("exists ?", written)).order_by("name").all()
    assert [writer["name"] for writer in writers] == ["Le Guin", "Lem"]
    assert len(await library.table("books").where(author_id=1).explain()) > 0


async def test_a_table_inside_a_transaction_writes_with_it_and_rolls_back_with_it(app: tinystore.Database) -> None:
    async def change_my_mind(tx: tinystore.SqlTx) -> None:
        await tx.table("notes", Note).insert(Note("tx1", 4, "a"))
        assert await tx.table("notes").where(id="tx1").count() == 1
        raise RuntimeError("changed my mind")

    with pytest.raises(RuntimeError, match="changed my mind"):
        await app.tx(change_my_mind)
    assert await app.table("notes").where(id="tx1").count() == 0


@dataclass
class Post:
    id: int
    title: str
    score: float | None
    big: int | None
    raw: bytes | None
    done: bool


@dataclass
class Author:
    id: int
    name: str
    posts: list[Post]


async def test_an_included_query_gives_each_row_its_rows_every_value_as_it_is_kept(store: tinystore.Store) -> None:
    db = await store.database(
        "included",
        migrations={
            "0001_included.sql": """create table authors (id integer primary key, name text not null) strict;
            create table posts (id integer primary key, author_id integer not null, title text not null,
                score real, big integer, raw blob, done integer not null default 0) strict;"""
        },
    )
    odd = 'it\'s, (odd)\0 "text"'
    await db.exec("insert into authors (id, name) values (1, 'ann'), (2, 'bob'), (3, 'eve')")
    await db.exec(
        """insert into posts (id, author_id, title, score, big, raw, done) values
        (1, 1, 'first', ?, ?, ?, 1), (2, 1, ?, 1e308, null, null, 0), (3, 1, 'third', null, -5, x'', 0),
        (4, 2, 'only', 2.5, 7, null, 1)""",
        0.1 + 0.2,
        2**60 + 1,
        bytes([0, 255, 39]),
        odd,
    )
    latest = (
        db.table("posts as p")
        .select(Post, "p.id, p.title, p.score, p.big, p.raw, p.done")
        .where("p.author_id = a.id")
        .order_by("p.id", "desc")
        .limit(2)
    )
    authors = await db.table("authors as a", Author).include("posts", latest).order_by("a.id").all()
    assert [(author.name, [post.id for post in author.posts]) for author in authors] == [
        ("ann", [3, 2]),
        ("bob", [4]),
        ("eve", []),
    ]
    third, second = authors[0].posts
    assert second == Post(2, odd, 1e308, None, None, False)
    assert third == Post(3, "third", None, -5, b"", False)
    ann = await db.table("authors as a").select("a.name").include("posts", latest.limit(3)).where({"a.id": 1}).one()
    assert ann is not None and ann["posts"][2] == Post(1, "first", 0.1 + 0.2, 2**60 + 1, bytes([0, 255, 39]), True)
    posts = db.table("posts as p")
    authors_table = db.table("authors as a")
    for wrong in (posts.limit(1), posts.select("p.id"), posts.select("count(*)").limit(1)):
        with pytest.raises(tinystore.InvalidError):
            authors_table.include("posts", wrong)


async def test_a_bucket_and_a_queue_opened_from_the_database_commit_with_its_rows(
    store: tinystore.Store, app: tinystore.Database
) -> None:
    sessions = app.bucket("sessions", dict[str, str])
    emails = app.queue("emails", dict[str, str])
    await sessions.set("t-0", {"user": "ann"})
    assert await sessions.get("t-0") == {"user": "ann"}

    async def declined(tx: tinystore.SqlTx) -> None:
        await tx.exec("insert into notes (id, author_id, title) values ('q1', 5, 'a')")
        await tx.with_(sessions).set("t-1", {"user": "bob"})
        assert await tx.with_(emails).add({"note": "q1"}, id="q1")
        assert await tx.with_(sessions).get("t-1") == {"user": "bob"}
        job = await tx.with_(emails).get("q1")
        assert job is not None and job.state == "waiting"
        raise RuntimeError("declined")

    with pytest.raises(RuntimeError, match="declined"):
        await app.tx(declined)
    assert not await sessions.has("t-1")
    assert await emails.get("q1") is None
    assert await app.scalar(int, "select count(*) from notes where id = 'q1'") == 0

    async def placed(tx: tinystore.SqlTx) -> None:
        await tx.exec("insert into notes (id, author_id, title) values ('q1', 5, 'a')")
        await tx.with_(sessions).set("t-1", {"user": "bob"})
        await tx.with_(emails).add({"note": "q1"}, id="q1")

    await app.tx(placed)
    assert await sessions.get("t-1") == {"user": "bob"}
    job = await emails.get("q1")
    assert job is not None and job.value == {"note": "q1"}
    assert not await store.bucket("sessions").has("t-1")


async def test_a_transaction_refuses_a_handle_kept_elsewhere_or_one_made_after_it_began(
    store: tinystore.Store, app: tinystore.Database
) -> None:
    async def elsewhere(tx: tinystore.SqlTx) -> None:
        await tx.with_(store.bucket("sessions", int)).set("k", 1)

    with pytest.raises(tinystore.InvalidError, match="kept in the store, outside this transaction of sql app"):
        await app.tx(elsewhere)

    async def late(tx: tinystore.SqlTx) -> None:
        await tx.with_(app.bucket("late", int)).set("k", 1)

    with pytest.raises(tinystore.InvalidError, match=r"make it before db\.tx"):
        await app.tx(late)
    in_database = app.bucket("sessions", int)

    async def in_store(tx: tinystore.Tx) -> None:
        await tx.with_(in_database).set("k", 1)

    with pytest.raises(tinystore.InvalidError):
        await store.tx(in_store)


async def test_a_call_around_a_transaction_from_inside_it_is_refused_and_one_beside_it_waits_its_turn(
    app: tinystore.Database,
) -> None:
    emails = app.queue("around", dict[str, str])
    sessions = app.bucket("around", dict[str, str])
    notes = app.table("notes")
    around: list[Any] = [
        lambda: app.exec("delete from notes where id = 'none'"),
        lambda: app.all("select 1"),
        lambda: app.batch([sql("delete from notes where id = 'none'")]),
        lambda: app.tx(nothing),
        lambda: notes.count(),
        lambda: emails.add({"note": "around"}),
        lambda: sessions.get("k"),
    ]
    gate = asyncio.Event()

    async def held(tx: tinystore.SqlTx) -> None:
        for call in around:
            with pytest.raises(tinystore.InvalidError, match="make it through the transaction"):
                await call()
        await tx.with_(emails).add({"note": "inside"}, id="inside")
        await gate.wait()

    holding = asyncio.ensure_future(app.tx(held))
    await asyncio.sleep(0.05)
    # a call of another task is no call from inside: it waits for the writer, and is made
    beside = asyncio.ensure_future(emails.add({"note": "beside"}, id="beside"))
    gate.set()
    await holding
    assert await beside
    inside = await emails.get("inside")
    assert inside is not None and inside.state == "waiting"
    for call in around:
        await call()


async def nothing(tx: tinystore.SqlTx) -> None:
    return None


async def test_transactions_past_the_threads_of_the_store_wait_their_turn_and_the_one_holding_the_writer_goes_on(
    store: tinystore.Store, app: tinystore.Database
) -> None:
    other = await store.database("other")
    gate = asyncio.Event()

    async def held(tx: tinystore.SqlTx) -> None:
        await tx.exec("insert into notes (id, author_id, title) values ('turn-0', 1, 'held')")
        await gate.wait()
        # another database's query runs on one of the store's threads, while the others wait for the writer
        assert await other.scalar(int, "select 1") == 1

    def waited(n: int) -> Callable[[tinystore.SqlTx], Awaitable[None]]:
        async def insert(tx: tinystore.SqlTx) -> None:
            await tx.exec("insert into notes (id, author_id, title) values (?, 1, 'waited')", f"turn-{n}")

        return insert

    holding = asyncio.ensure_future(app.tx(held))
    waiting = [asyncio.ensure_future(app.tx(waited(n))) for n in range(1, 41)]
    # time for every transaction to reach the store before the one holding the writer goes on
    await asyncio.sleep(0.1)
    gate.set()
    await holding
    await asyncio.gather(*waiting)
    assert await app.scalar(int, "select count(*) from notes where id like 'turn-%'") == 41


async def test_a_worker_runs_a_job_its_transaction_added_once_the_transaction_commits(
    app: tinystore.Database,
) -> None:
    sent = app.queue("sent", dict[str, str])
    seen: list[str] = []

    async def send(email: dict[str, str], run: tinystore.Run) -> None:
        seen.append(email["note"])

    worker = sent.work(send)

    async def add(tx: tinystore.SqlTx) -> None:
        await tx.with_(sent).add({"note": "w1"})

    await app.tx(add)
    await until(lambda: len(seen) == 1)
    await worker.stop()
    assert seen == ["w1"]


# contract: WIRE-10
async def test_queries_whose_tasks_are_cancelled_stop_on_the_server(app: tinystore.Database) -> None:
    # more queries of minutes than the server has threads and the file readers
    slow = "with recursive c(x) as (select 1 union all select x + 1 from c where x < 1000000000) select count(*) from c"
    panels = [asyncio.create_task(app.scalar(int, slow)) for _ in range(20)]
    await asyncio.sleep(0.1)
    for panel in panels:
        panel.cancel()
    for panel in panels:
        with pytest.raises(asyncio.CancelledError):
            await panel
    with pytest.raises(TimeoutError):
        async with asyncio.timeout(0.1):
            await app.scalar(int, slow)
    loop = asyncio.get_running_loop()
    started = loop.time()
    assert await app.scalar(int, "select 1") == 1
    assert loop.time() - started < 2


# contract: JOB-02
async def test_a_batch_writes_its_statements_and_a_queues_adds_together_or_none_of_them(
    store: tinystore.Store, app: tinystore.Database
) -> None:
    indexing = app.queue("indexing", dict[str, str])

    def insert(note_id: str) -> tinystore.Sql:
        return sql("insert into notes (id, author_id, title) values (?, 13, 'a')", note_id)

    done = await app.batch(
        [insert("j1"), indexing.adding({"note": "j1"}, id="j1"), indexing.adding({"note": "again"}, id="j1")]
    )
    assert [each.changes for each in done] == [1, 1, 0]
    job = await indexing.get("j1")
    assert job is not None and job.value == {"note": "j1"}

    with pytest.raises(tinystore.ConflictError):
        await app.batch([indexing.adding({"note": "j2"}, id="j2"), insert("j1")])
    assert await indexing.get("j2") is None

    in_jobs = store.queue("indexing", dict[str, str])
    with pytest.raises(tinystore.InvalidError, match=r"kept in jobs\.db, outside this batch of sql app"):
        await app.batch([insert("j3"), in_jobs.adding({"note": "j3"})])
    assert await app.scalar(int, "select count(*) from notes where author_id = 13") == 1


# contract: JOB-02
async def test_a_worker_runs_a_job_its_batch_added_once_the_batch_commits(app: tinystore.Database) -> None:
    sent = app.queue("batched", dict[str, str])
    seen: list[str] = []

    async def send(email: dict[str, str], run: tinystore.Run) -> None:
        seen.append(email["note"])

    worker = sent.work(send)
    await app.batch(
        [sql("insert into notes (id, author_id, title) values ('w2', 14, 'a')"), sent.adding({"note": "w2"})]
    )
    await until(lambda: len(seen) == 1)
    await worker.stop()
    assert seen == ["w2"]
