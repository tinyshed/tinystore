"""Every engine through a real tinystore serve, the one conftest.py built."""

from __future__ import annotations

import asyncio
import logging
import struct
import time
from array import array
from dataclasses import dataclass
from typing import TYPE_CHECKING

import pytest

import tinystore
from tinystore import ConflictError, CorruptError, InvalidError, TooOldError

if TYPE_CHECKING:
    from collections.abc import AsyncIterator
    from pathlib import Path


@pytest.fixture
async def store(tmp_path: Path) -> AsyncIterator[tinystore.Store]:
    async with tinystore.open(tmp_path / "data", private=True) as opened:
        yield opened


@dataclass
class Session:
    device: str
    seen_at: int


async def test_a_bucket_keeps_values_under_their_owners(store: tinystore.Store) -> None:
    sessions = store.kv.bucket("sessions", Session)
    await sessions.of(7).set("token", Session("phone", 1))
    assert await sessions.of("7").get("token") == Session("phone", 1)
    assert await sessions.of(7).has("token")
    assert await sessions.get("token") is None

    first = await sessions.set_entry("k", Session("a", 1))
    await sessions.set("k", Session("b", 2), if_version=first.version)
    with pytest.raises(ConflictError) as err:
        await sessions.set("k", Session("c", 3), if_version=first.version)
    assert err.value.what == {"bucket": "sessions", "key": "k"}


async def test_every_kind_keeps_its_values_as_go_keeps_its_types(store: tinystore.Store) -> None:
    await store.kv.bucket("strings", str).set("s", "héllo " + chr(0x1F600))
    assert await store.kv.bucket("strings", str).get("s") == "héllo " + chr(0x1F600)
    await store.kv.bucket("floats", float).set("f", -0.0)
    zero = await store.kv.bucket("floats", float).get("f")
    assert zero is not None and struct.pack(">d", zero) == struct.pack(">d", -0.0)
    await store.kv.bucket("flags", bool).set("on", True)
    assert await store.kv.bucket("flags", bool).get("on") is True
    await store.kv.bucket("big", int).set("n", 2**62)
    assert await store.kv.bucket("big", int).get("n") == 2**62
    seen = store.kv.bucket("events", None)
    assert await seen.set_if_absent("evt_1", None, ttl=3600)
    assert not await seen.set_if_absent("evt_1", None)
    with pytest.raises(CorruptError):
        await store.kv.bucket("strings", int).get("s")


async def test_counters_clear_scan_batch_and_view(store: tinystore.Store) -> None:
    attempts = store.kv.counters("login-attempts", default_ttl=900)
    assert await attempts.of("ip").add("10.0.0.1") == 1
    assert await attempts.of("ip").add("10.0.0.1", 4) == 5
    assert await attempts.of("ip").max("10.0.0.1", 9) == 9

    pages = store.kv.bucket("pages", int).of("p")
    for i in range(25):
        await pages.set(f"k{i:02d}", i)
    first, after = await pages.scan(limit=10)
    assert [e.key for e in first] == [f"k{i:02d}" for i in range(10)]
    assert after == "k09"
    assert [e.value async for e in pages.all(limit=7)] == list(range(25))
    await store.kv.bucket("pages", int).clear()
    assert await pages.get("k00") is None

    accounts, log = store.kv.bucket("accounts", int), store.kv.bucket("log", str)
    async with store.kv.batch() as tx:
        accounts.with_tx(tx).set("a", 10)
        log.with_tx(tx).set("1", "opened a")
    assert await accounts.get("a") == 10
    with pytest.raises(ConflictError) as err:
        async with store.kv.batch() as tx:
            log.with_tx(tx).set("2", "moved a")
            accounts.with_tx(tx).set("a", 12, if_version="stale")
    assert err.value.what.get("call") == "1"
    assert await log.get("2") is None
    async with store.kv.view() as tx:
        a = accounts.with_tx(tx).get("a")
        none = accounts.with_tx(tx).has("nothing")
    assert (await a, await none) == (10, False)


@dataclass
class Receipt:
    receipt: str


async def test_a_once_key_runs_once_and_a_call_meanwhile_waits_for_its_answer(store: tinystore.Store) -> None:
    charges = store.kv.once("charges", Receipt)
    ran, started, release = [0], asyncio.Event(), asyncio.Event()

    async def charge() -> Receipt:
        ran[0] += 1
        started.set()
        await release.wait()
        return Receipt(f"r_{ran[0]}")

    first = asyncio.ensure_future(charges.run("req-7", charge))
    await started.wait()
    second = asyncio.ensure_future(charges.run("req-7", charge))
    await asyncio.sleep(0.05)
    assert ran[0] == 1
    release.set()
    assert await first == await second == Receipt("r_1")
    assert await charges.get("req-7") == Receipt("r_1")

    async def decline() -> Receipt:
        raise LookupError("the card was declined")

    with pytest.raises(LookupError):
        await charges.run("req-8", decline)
    assert await charges.get("req-8") is None

    async def receipt_8() -> Receipt:
        return Receipt("r_8")

    assert await charges.run("req-8", receipt_8) == Receipt("r_8")
    await charges.delete("req-7")
    assert await charges.run("req-7", charge) == Receipt("r_2")
    assert await charges.of("tenant-7").run("req-7", charge) == Receipt("r_3")


async def test_a_quota_counts_a_use_in_every_window_or_in_none(store: tinystore.Store) -> None:
    ai = store.kv.quota("ai", session="2/5h", weekly="3/7d")
    first = await ai.allow("user-1")
    assert (first.ok, first.left, first.windows["session"].used, first.windows["weekly"].used) == (True, 1, 1, 1)
    assert list(first.windows) == ["session", "weekly"]
    assert (await ai.allow("user-1")).left == 0
    refused = await ai.allow("user-1")
    assert (refused.ok, refused.windows["session"].used, refused.windows["weekly"].used) == (False, 2, 2)
    assert refused.retry_after > 4 * 3600
    assert refused.windows["session"].reset_at is not None
    assert not (await ai.get("user-1")).ok

    await ai.refund("user-1")
    back = await ai.get("user-1")
    assert (back.ok, back.left, back.windows["weekly"].used) == (True, 1, 1)
    await ai.delete("user-1")
    assert (await ai.get("user-1")).windows["session"].reset_at is None
    assert (await ai.of("tenant-7").allow("user-1", 2)).windows["session"].used == 2
    with pytest.raises(InvalidError):
        await ai.allow("user-1", 3)
    with pytest.raises(InvalidError):
        store.kv.quota("bad")
    with pytest.raises(InvalidError):
        store.kv.quota("bad", Session="1/h")


async def test_a_queue_waits_changes_cancels_and_works(store: tinystore.Store) -> None:
    later = store.jobs.queue("send-later", dict[str, int])
    await later.enqueue({"user": 42}, key="chat:42:a", after=3600)
    waiting = await later.get("chat:42:a")
    assert waiting is not None and waiting.state == "waiting" and waiting.value == {"user": 42}
    await later.update("chat:42:a", {"user": 43})
    assert await later.cancel("chat:42:a")
    assert not await later.cancel("chat:42:a")

    q = store.jobs.queue("outcomes", str, backoff=(0.001, 0.001))
    await q.enqueue_all(tinystore.Enqueue(v, key=v) for v in ("ok", "flaky", "broken", "later"))
    attempts: dict[str, list[int]] = {}

    async def handle(job: tinystore.Job[str]) -> None:
        attempts.setdefault(job.key, []).append(job.attempt)
        if job.value == "flaky" and job.attempt == 1:
            raise RuntimeError("try again")
        if job.value == "broken":
            job.fail("bad input")
        if job.value == "later":
            job.snooze(after=3600)

    await q.work(handle, workers=2, until_idle=True)
    assert attempts["flaky"] == [1, 2]
    broken = await q.get("broken")
    assert broken is not None and (broken.state, broken.error) == ("failed", "bad input")
    later_job = await q.get("later")
    assert later_job is not None and (later_job.state, later_job.attempt) == ("waiting", 0)

    claimed = store.jobs.queue("claimed", str)
    await claimed.enqueue("x", key="k")
    job = await claimed.claim(lease=60)
    assert job is not None and job.value == "x"
    await job.ack()
    assert await claimed.get("k") is None


async def test_a_cancelled_work_loop_gives_its_jobs_back_uncounted(store: tinystore.Store) -> None:
    q = store.jobs.queue("slow", int)
    await q.enqueue(1, key="one")
    started = asyncio.Event()

    async def handle(_: tinystore.Job[int]) -> None:
        started.set()
        await asyncio.sleep(60)

    loop = asyncio.ensure_future(q.work(handle))
    await started.wait()
    loop.cancel()
    with pytest.raises(asyncio.CancelledError):
        await loop
    entry: tinystore.JobEntry[int] | None = None
    for _ in range(50):
        entry = await q.get("one")
        if entry is not None and entry.state == "waiting":
            break
        await asyncio.sleep(0.05)
    assert entry is not None and (entry.state, entry.attempt) == ("waiting", 0)


async def test_objects_come_back_byte_for_byte(store: tinystore.Store) -> None:
    files = store.blobs.bucket("files").of("users", 7)
    for size in (0, 1, 70_000, (3 << 20) + 17):
        data = bytes((i * 31 + 7) & 0xFF for i in range(size))
        put = await files.put(f"f{size}", data, content_type="application/octet-stream")
        got = await files.get(f"f{size}")
        assert got is not None and got.object.etag == put.etag
        assert await got.read() == data
    assert await files.get("nothing") is None
    ranged = await files.get("f70000", offset=1024, length=4096)
    assert ranged is not None and len(await ranged.read()) == 4096

    first = await files.put("doc", "v1", if_none_match=True)
    with pytest.raises(ConflictError):
        await files.put("doc", "v2", if_none_match=True)
    await files.put("doc", b"v2", if_match=first.etag)
    moved = await files.move("doc", "archive/doc")
    assert moved.key == "archive/doc"
    assert [o.key async for o in files.all(prefix="archive/")] == ["archive/doc"]
    await files.clear()
    assert (await files.usage()).objects == 0


async def test_sql_statements_batches_and_views(store: tinystore.Store, tmp_path: Path) -> None:
    migrations = tmp_path / "migrations"
    migrations.mkdir()
    (migrations / "001_notes.sql").write_text(
        "create table notes (id integer primary key, author_id integer not null, "
        "title text not null unique, done integer not null default 0, weight real) strict;"
    )
    app = await store.sql("app", migrations=migrations)
    assert (
        await app.exec("insert into notes (author_id, title, weight) values (?, ?, ?)", 7, "milk", 2.5)
    ).last_id == 1
    await app.exec("insert into notes (author_id, title) values (:author, :title)", author=7, title="bread")

    @dataclass
    class Note:
        id: int
        author_id: int
        title: str
        done: bool
        weight: float | None

    mine = await app.all(Note, "select * from notes where author_id = ? order by id", 7)
    assert [n.title for n in mine] == ["milk", "bread"]
    assert mine[0].done is False and mine[1].weight is None
    assert await app.scalar("select count(*) from notes") == 2
    assert await app.scalar("select 9007199254740993") == 9007199254740993
    with pytest.raises(ConflictError):
        await app.exec("insert into notes (author_id, title) values (1, 'milk')")
    with pytest.raises(InvalidError):
        await app.all("select nothing from nowhere")
    assert [row["title"] async for row in app.each("select title from notes order by id")] == [
        "milk",
        "bread",
    ]

    async with app.batch() as tx:
        inserted = tx.exec("insert into notes (author_id, title) values (1, 'tea')")
        count = tx.scalar("select count(*) from notes")
    assert ((await inserted).last_id, await count) == (3, 3)
    with pytest.raises(ConflictError):
        async with app.batch() as tx:
            tx.exec("insert into notes (author_id, title) values (1, 'coffee')")
            tx.exec("insert into notes (author_id, title) values (1, 'tea')")
    assert await app.one("select * from notes where title = 'coffee'") is None


async def test_records_come_back_as_they_went_in(store: tinystore.Store) -> None:
    at = time.time_ns() // 1000 * 1000 + 123
    await store.records.append(
        {
            "at": at,
            "stream": "web",
            "name": "click",
            "level": "info",
            "body": "bought",
            "trace_id": "0102030405060708090a0b0c0d0e0f10",
            "attrs": [("x", 812), ("x", 813)],
        }
    )
    page = await store.records.scan(streams=["web"])
    assert page.next is None and len(page.items) == 1
    r = page.items[0]
    assert (r.at, r.stream, r.name, r.level, r.body) == (at, "web", "click", 0, "bought")
    assert r.attrs == [("x", "812"), ("x", "813")]
    with pytest.raises(TooOldError):
        await store.records.append({"at": time.time_ns() - 100 * 86_400 * 10**9, "stream": "web", "name": "old"})

    logger = logging.getLogger("tinystore-test")
    logger.addHandler(store.records.handler("app", console="off"))
    logger.warning("slow request", extra={"ms": 1200})
    found: list[tinystore.Record] = []
    for _ in range(60):
        found, _ = await store.records.scan(streams=["app"])
        if found:
            break
        await asyncio.sleep(0.05)
    assert found and (found[0].body, found[0].level) == ("slow request", 4)
    assert ("ms", "1200") in found[0].attrs


async def test_records_page_on_from_where_the_last_page_ended(store: tinystore.Store) -> None:
    base = time.time_ns()
    await store.records.append(
        *({"at": base - (30 - i) * 10**9, "stream": "api", "name": "log", "body": f"line {i}"} for i in range(30))
    )
    first = await store.records.scan(streams=["api"], since="1m", limit=10)
    assert len(first.items) == 10 and first.next is not None
    second = await store.records.scan(streams=["api"], since="1m", limit=10, after=first.next)
    assert second.items[0].body == "line 10"
    assert [r.body async for r in store.records.all(streams=["api"], limit=7)] == [f"line {i}" for i in range(30)]
    with pytest.raises(InvalidError):
        await store.records.scan(after="not a page")


async def test_samples_come_back_bit_for_bit_and_aggregate_exactly(store: tinystore.Store) -> None:
    now = time.time_ns() // 1_000_000
    values = array("d", struct.unpack("<3d", struct.pack("<QQd", 0x8000000000000000, 0x7FF8000000000001, 0.5)))
    await store.metrics.ingest(
        {
            "name": "cpu",
            "kind": "gauge",
            "labels": {"host": "web-1"},
            "times": [now - 2000, now - 1000, now],
            "values": values,
        }
    )
    [series] = await store.metrics.read(name="cpu", since="1m")
    assert (series.name, series.labels) == ("cpu", {"host": "web-1"})
    assert series.times == [now - 2000, now - 1000, now]
    assert series.values.tobytes() == values.tobytes()

    start = now - 50 * 60_000
    await store.metrics.ingest(
        {
            "name": "requests_total",
            "kind": "counter",
            "samples": [
                (start, 100),
                (start + 60_000, 110),
                (start + 120_000, 5),
                (start + 180_000, 20),
            ],
        }
    )
    [agg] = await store.metrics.aggregate(
        name="requests_total", from_=start, to=start + 3_600_000, width="1h", op="increase"
    )
    assert [(b.value, b.resets, b.count) for b in agg.buckets] == [(30.0, 1, 4)]
    assert agg.buckets[0].from_ is not None and agg.buckets[0].from_.timestamp() * 1000 == start

    store.metrics.counter("http_requests_total").labels(route="/notes").inc()
    store.metrics.counter("http_requests_total").labels(route="/notes").inc(2)
    await store.metrics.flush()
    [posts] = await store.metrics.read(name="http_requests_total", match={"route": "/notes"}, since=60)
    assert posts.values[0] == 3
    assert await store.metrics.drop("cpu", {"host": "web-1"}) == (True, 0)
    assert await store.metrics.read(name="cpu") == []

    for refused in (
        store.metrics.read(name="cpu", match={"__name__": "other"}),
        store.metrics.read(),
        store.metrics.read(name="cpu", since="1h", from_=0),
        store.metrics.ingest({"name": "cpu", "kind": "gauge", "labels": {"__x": "y"}, "samples": []}),
    ):
        with pytest.raises(InvalidError):
            await refused


async def test_a_second_open_finds_the_sidecar_the_first_started(tmp_path: Path) -> None:
    async with tinystore.open(tmp_path / "shared", idle=0.3) as first:
        await first.kv.bucket("notes", str).set("a", "from the first")
        async with tinystore.open(tmp_path / "shared") as second:
            assert await second.kv.bucket("notes", str).get("a") == "from the first"
    serve = tmp_path / "shared" / "server" / "SERVE"
    for _ in range(100):
        if not serve.exists():
            break
        await asyncio.sleep(0.05)
    assert not serve.exists()
    assert "msg=serving" in (tmp_path / "shared" / "server" / "serve.log").read_text()


async def test_a_watch_follows_a_job_through_its_progress_to_a_cancel_its_handler_sees(
    store: tinystore.Store,
) -> None:
    q = store.jobs.queue("videos", str, max_running=1)
    await q.enqueue("a", key="a")
    await q.enqueue("b", key="b")
    seen: list[str] = []

    async def watching() -> None:
        async for s in q.watch("b"):
            # each entry as it comes, which the test waits for, not the list once the watch ends
            seen.append(f"{s.state} {s.ahead}" + ("" if s.progress is None else f" {s.progress}"))  # noqa: PERF401

    async def until(holds: object, what: str) -> None:
        for _ in range(500):
            if callable(holds) and holds():
                return
            await asyncio.sleep(0.01)
        raise AssertionError(f"waited five seconds for {what}")

    watcher = asyncio.ensure_future(watching())
    await until(lambda: "waiting 1" in seen, "b waiting behind a")
    release = asyncio.Event()
    cancelled: list[str] = []

    async def handle(job: tinystore.Job[str]) -> None:
        if job.key == "a":
            await release.wait()
            return
        job.progress({"done": 1})
        try:
            await asyncio.sleep(60)
        except asyncio.CancelledError:
            cancelled.append(job.key)
            raise

    loop = asyncio.ensure_future(q.work(handle, workers=2))
    await until(lambda: "waiting 0" in seen, "a running, b next")
    b = await q.get("b")
    assert b is not None and b.state == "waiting"  # max_running holds it though a worker is free
    release.set()
    await until(lambda: "running 0 {'done': 1}" in seen, "b's progress")
    assert await q.cancel("b")
    await asyncio.wait_for(watcher, 5)
    assert seen[-1] == "cancelled 0"
    await until(lambda: cancelled == ["b"], "the handler's task to be cancelled")
    loop.cancel()
    with pytest.raises(asyncio.CancelledError):
        await loop
    assert await q.get("b") is None
    assert [s async for s in q.watch("b")] == []
    with pytest.raises(InvalidError):
        tinystore.Job("k", "v", None, 1).progress(object())
