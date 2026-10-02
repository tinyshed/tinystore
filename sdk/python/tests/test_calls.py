"""The calls the engine tests leave out, each through a real tinystore serve."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING

import pytest

import tinystore
from tinystore import InvalidError

if TYPE_CHECKING:
    from collections.abc import AsyncIterator
    from pathlib import Path


@pytest.fixture
async def store(tmp_path: Path) -> AsyncIterator[tinystore.Store]:
    async with tinystore.open(tmp_path / "data", private=True) as opened:
        yield opened


async def test_kv_expiry_take_and_delete(store: tinystore.Store) -> None:
    drafts = store.kv.bucket("drafts", str, default_ttl=timedelta(hours=1))
    await drafts.set("d", "hello")
    entry = await drafts.get_entry("d")
    assert entry is not None and entry.expires is not None
    assert entry.expires > datetime.now(UTC) + timedelta(minutes=59)
    assert await drafts.touch("d", ttl=timedelta(hours=2))
    assert not await drafts.touch("missing", ttl=60)
    assert await drafts.take("d") == "hello"
    assert await drafts.take("d") is None
    await drafts.set("gone", "x", expire_at=datetime.now(UTC) - timedelta(seconds=1))
    assert await drafts.get("gone") is None
    await drafts.set("e", "y")
    await drafts.delete("e")
    assert not await drafts.has("e")
    created, entry = await drafts.set_entry_if_absent("f", "first")
    assert created and entry.value == "first"
    created, entry = await drafts.set_entry_if_absent("f", "second")
    assert not created and entry.value == "first"
    with pytest.raises(InvalidError):
        store.kv.bucket("Bad Name")
    with pytest.raises(InvalidError):
        await store.kv.bucket("ints", int).set("k", 1 << 64)
    await store.kv.counters("hits").of("x").clear()
    assert await store.kv.counters("hits").get("k") == 0


async def test_a_claimed_job_retries_snoozes_extends_and_fails(store: tinystore.Store) -> None:
    q = store.jobs.queue("claims", str, backoff=(3600, 3600))
    await q.enqueue("x", key="k")
    job = await q.claim(lease=60)
    assert job is not None
    await job.extend(120)
    await job.retry("busy", after=0)
    job = await q.claim()
    assert job is not None and job.attempt == 2
    await job.snooze(after=3600)
    entry = await q.get("k")
    assert entry is not None and entry.state == "waiting"
    await q.update("k", "y", at=datetime.now(UTC) - timedelta(seconds=1))
    job = await q.claim()
    assert job is not None and job.value == "y"
    await job.fail("broken")
    failed, after = await q.scan(state="failed")
    assert [e.key for e in failed] == ["k"] and after is None
    assert [e.key async for e in q.all(prefix="k")] == ["k"]
    schedule = store.jobs.schedule("tick", tinystore.every(3600))
    assert (await schedule.get("tick")) is not None
    assert await schedule.cancel("tick")


async def test_blobs_stat_copy_delete_and_pages(store: tinystore.Store) -> None:
    files = store.blobs.bucket("files", max_size=1 << 20)
    for i in range(5):
        await files.put(f"p/{i}", bytes([i]) * (i + 1), meta={"i": str(i)})
    stat = await files.stat("p/3")
    assert stat is not None and (stat.size, stat.meta) == (4, {"i": "3"})
    copied = await files.copy("p/3", "q/3", meta={"copied": "yes"})
    assert copied.meta == {"copied": "yes"}
    await files.delete("p/0")
    assert await files.stat("p/0") is None
    first, after = await files.scan(prefix="p/", limit=2)
    assert [o.key for o in first] == ["p/1", "p/2"] and after == "p/2"
    got = await files.get("q/3")
    assert got is not None
    async with got:
        assert [chunk async for chunk in got] == [bytes([3]) * 4]


async def test_sql_reads_and_writes_every_way(store: tinystore.Store) -> None:
    app = await store.sql("app", migrations={"001.sql": "create table t (id integer primary key, v text) strict;"})
    assert (await app.exec_one("insert into t (v) values ('a') returning id")) == {"id": 1}
    assert await app.exec_scalar("insert into t (v) values ('b') returning id") == 2
    assert await app.exec_all("update t set v = v || '!' returning v") == [{"v": "a!"}, {"v": "b!"}]
    assert await app.query("select id, v from t order by id") == (["id", "v"], [[1, "a!"], [2, "b!"]])
    async with app.view() as tx:
        rows = tx.all("select * from t")
        one = tx.one("select * from t where id = ?", 1)
    assert (len(await rows), (await one)["v"]) == (2, "a!")
    with pytest.raises(InvalidError):
        await app.scalar("select * from t")


async def test_records_follow_lines_and_damage(store: tinystore.Store) -> None:
    records, cursor, expired = await store.records.follow(limit=10)
    assert (records, expired) == ([], 0)
    assert cursor == tinystore.Cursor(0, 0) or cursor.segment >= 0
    lines = store.records.lines("worker")
    assert lines.write("plain text line\n")
    await lines.end()
    assert lines.dropped == 0
    assert await store.records.damaged() == []
    found = [r async for r in store.records.all(streams=["nothing"])]
    assert found == []


async def test_gauges_and_their_functions(store: tinystore.Store) -> None:
    inflight = store.metrics.gauge("inflight").labels(route="/x")
    inflight.set(5)
    inflight.inc()
    inflight.dec(2)

    async def depth() -> float:
        return 7

    store.metrics.gauge_func("depth", depth)
    await store.metrics.flush()
    [g] = await store.metrics.read(name="inflight", since=timedelta(minutes=1))
    assert (g.values[0], g.labels) == (4, {"route": "/x"})
    [d] = await store.metrics.read(name="depth", since="1m")
    assert d.values[0] == 7
    with pytest.raises(InvalidError):
        store.metrics.counter("inflight")
    with pytest.raises(InvalidError):
        store.metrics.counter("c").inc(-1)


async def test_a_remote_server_takes_its_token_and_refuses_another(tmp_path: Path) -> None:
    import asyncio
    import os
    import secrets
    import socket

    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    token = secrets.token_urlsafe(32)
    tokens = tmp_path / "tokens.txt"
    tokens.write_text(f"admin {token}\n")
    server = await asyncio.create_subprocess_exec(
        os.environ["TINYSTORE_BIN"],
        "serve",
        "--dir",
        str(tmp_path / "remote"),
        "--listen",
        f"tcp://127.0.0.1:{port}",
        "--tokens",
        str(tokens),
    )
    try:
        for _ in range(100):
            try:
                async with tinystore.connect(f"tcp://127.0.0.1:{port}", token=token) as remote:
                    await remote.kv.bucket("notes", str).set("a", "over tcp")
                    assert await remote.kv.bucket("notes", str).get("a") == "over tcp"
                break
            except tinystore.ClosedError:
                await asyncio.sleep(0.05)
        else:
            pytest.fail("the remote server never answered")
        with pytest.raises(tinystore.UnauthenticatedError):
            await tinystore.connect(f"tcp://127.0.0.1:{port}", token="wrong")
    finally:
        server.terminate()
        await server.wait()


async def test_an_address_nobody_listens_on_is_closed() -> None:
    import socket

    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    with pytest.raises(tinystore.ClosedError, match=f"cannot reach tcp://127.0.0.1:{port}"):
        await tinystore.connect(f"tcp://127.0.0.1:{port}", token="unused")


async def test_conditions_find_series_beyond_equality(store: tinystore.Store) -> None:
    at = datetime.now(UTC)
    for host, status, env in [("api-1", "200", "prod"), ("api-2", "502", "dev"), ("web-1", "500", None)]:
        labels = {"host": host, "status": status} | ({"env": env} if env else {})
        await store.metrics.ingest({"name": "requests", "kind": "counter", "labels": labels, "samples": [(at, 1)]})

    async def hosts(where: dict[str, tinystore.Condition | str]) -> list[str]:
        return sorted(s.labels["host"] for s in await store.metrics.read(name="requests", where=where))

    assert await hosts({"status": tinystore.one_of("500", "502")}) == ["api-2", "web-1"]
    assert await hosts({"env": tinystore.none_of("dev")}) == ["api-1", "web-1"]
    assert await hosts({"host": tinystore.prefix("api-"), "status": "200"}) == ["api-1"]
    with pytest.raises(InvalidError):
        await store.metrics.read(where={"env": tinystore.none_of("dev")})


async def test_a_search_finds_records_by_their_text_the_case_ignored(store: tinystore.Store) -> None:
    now = datetime.now(UTC)
    await store.records.append(
        {
            "at": now - timedelta(milliseconds=4),
            "stream": "search",
            "name": "log",
            "level": "warn",
            "body": "read: Connection reset by peer",
        },
        {"at": now - timedelta(milliseconds=3), "stream": "search", "name": "log", "level": "info", "body": "all good"},
        {
            "at": now - timedelta(milliseconds=2),
            "stream": "search",
            "name": "log",
            "level": "info",
            "body": "CONNECTION RESET again",
        },
        {"at": now - timedelta(milliseconds=1), "stream": "search", "name": "user.created"},
    )
    found = [r.body async for r in store.records.all(streams=["search"], search="connection reset", limit=1)]
    assert found == ["read: Connection reset by peer", "CONNECTION RESET again"]
    page = await store.records.scan(streams=["search"], search="USER.created")
    assert [r.name for r in page.items] == ["user.created"]
