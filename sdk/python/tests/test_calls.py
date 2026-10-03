"""The calls the engine tests leave out, each through a real tinystore serve."""

from __future__ import annotations

import asyncio
import logging
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


async def test_a_timer_writes_its_count_sum_and_longest_at_each_flush(store: tinystore.Store) -> None:
    import asyncio

    latency = store.metrics.timer("timed_ms")
    latency.record(0.5)
    latency.record("30ms")
    latency.record(timedelta(milliseconds=2))
    latency.labels(route="/a").record(timedelta(microseconds=2500))
    with latency.measure():
        await asyncio.sleep(0)
    with pytest.raises(RuntimeError), latency.measure():
        raise RuntimeError("boom")
    async with latency.measure():
        pass
    with pytest.raises(InvalidError):
        latency.record(-1)
    with pytest.raises(InvalidError):
        store.metrics.counter("timed_ms_count")
    store.metrics.counter("busy_ms_sum").inc()
    with pytest.raises(InvalidError):
        store.metrics.timer("busy_ms")
    await store.metrics.flush()
    await asyncio.sleep(0.002)
    await store.metrics.flush()  # nothing measured since: the totals again, no longest

    async def values(name: str, route: str | None = None) -> list[float]:
        found = await store.metrics.read(name=name, since="1m")
        return next(list(s.values) for s in found if s.labels.get("route") == route)

    assert (await values("timed_ms_count"))[-1] == 6
    assert 532 <= (await values("timed_ms_sum"))[-1] < 1532
    assert await values("timed_ms_max") == [500]
    assert await values("timed_ms_count", "/a") == [1, 1]
    assert await values("timed_ms_sum", "/a") == [2.5, 2.5]
    assert await values("timed_ms_max", "/a") == [2.5]


async def test_a_half_full_buffer_is_written_before_its_interval(
    store: tinystore.Store, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr("tinystore.records._FLUSH_EVERY", 3600.0)
    log = logging.getLogger("half-full")
    log.propagate = False
    handler = store.records.handler("half", console="off", buffer=64)
    log.addHandler(handler)
    try:
        written, kept = 0, 0
        for _ in range(8):
            for _ in range(32):
                log.warning("line")
            written += 32
            for _ in range(500):
                kept = len((await store.records.scan(streams=["half"], limit=1000)).items)
                if kept >= written:
                    break
                await asyncio.sleep(0.01)
    finally:
        log.removeHandler(handler)
    assert (kept, handler.dropped) == (written, 0)


async def test_a_full_handler_drops_counts_and_says_so(
    store: tinystore.Store, capsys: pytest.CaptureFixture[str]
) -> None:
    log = logging.getLogger("full")
    log.propagate = False
    handler = store.records.handler("full", console="off", buffer=2)
    log.addHandler(handler)
    try:
        for n in ("one", "two", "three", "four"):
            log.warning(n)
        await handler.flush_now()
    finally:
        log.removeHandler(handler)
    assert handler.dropped == 2
    said = capsys.readouterr().err
    assert said.count("log lines dropped") == 1 and '"logger":"full"' in said


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


async def test_an_aggregate_joins_series_by_a_label_exactly(store: tinystore.Store) -> None:
    start = datetime.now(UTC) - timedelta(minutes=1)
    for route, host, values in [
        ("/a", "1", [10, 15, 5, 8]),
        ("/a", "2", [0, 2, 4, 6]),
        ("/b", "1", [100, 100, 101, 103]),
    ]:
        samples = [(start + timedelta(seconds=i), v) for i, v in enumerate(values)]
        labels = {"route": route, "host": host}
        await store.metrics.ingest({"name": "hits", "kind": "counter", "labels": labels, "samples": samples})
    end = start + timedelta(seconds=4)
    by_route = await store.metrics.aggregate(name="hits", from_=start, to=end, width=4, op="increase", by=["route"])
    assert [(g.labels["route"], g.buckets[0].value, g.buckets[0].resets) for g in by_route] == [
        ("/a", 19, 1),
        ("/b", 3, 0),
    ]
    [everything] = await store.metrics.aggregate(name="hits", from_=start, to=end, width=4, op="count", by=[])
    assert everything.labels == {} and everything.buckets[0].value == 12
    rates = await store.metrics.aggregate(name="hits", from_=start, to=end, width=4, op="rate", without=["host"])
    assert [g.buckets[0].value for g in rates] == [19 / 4, 3 / 4]
    with pytest.raises(InvalidError):
        await store.metrics.aggregate(name="hits", from_=start, to=end, width=4, op="delta", by=["route"])


async def test_a_limit_says_which_it_is_what_the_read_wanted_and_the_bound(store: tinystore.Store) -> None:
    now = datetime.now(UTC)
    samples = [(now - timedelta(seconds=s), float(s)) for s in (3, 2, 1)]
    await store.metrics.ingest({"name": "bounded", "kind": "gauge", "samples": samples})
    with pytest.raises(tinystore.LimitError) as refused:
        await store.metrics.read(name="bounded", since="1m", limits={"decoded": 1})
    assert (refused.value.limit, refused.value.bound) == ("decoded samples", 1)
    assert refused.value.wanted is not None and refused.value.wanted > 1


async def test_lines_and_appended_records_take_the_trace_they_were_made_in(store: tinystore.Store) -> None:
    trace_id = "0102030405060708090a0b0c0d0e0f10"
    log = logging.getLogger("traced")
    handler = store.records.handler("traced", console="off")
    log.addHandler(handler)
    try:
        with tinystore.trace(trace_id, "0102030405060708"):
            log.warning("charged")
            await store.records.append({"stream": "traced", "name": "paid"})
        log.warning("outside")
        await handler.flush_now()
    finally:
        log.removeHandler(handler)
    page = await store.records.scan(streams=["traced"], trace_id=trace_id)
    assert sorted(str(r.body) if r.name == "log" else r.name for r in page.items) == ["charged", "paid"]


async def test_a_redacted_field_is_hidden_in_the_store_at_any_depth(store: tinystore.Store) -> None:
    log = logging.getLogger("secret")
    log.propagate = False
    handler = store.records.handler("secret", console="off", redact=["password", "authorization"])
    log.addHandler(handler)
    try:
        log.warning(
            "login", extra={"user": {"name": "ann", "password": "y"}, "PASSWORD": "hunter2", "req.Authorization": "x"}
        )
        await handler.flush_now()
    finally:
        log.removeHandler(handler)
    page = await store.records.scan(streams=["secret"])
    assert page.items[0].attrs == [
        ("user", '{"name":"ann","password":"[redacted]"}'),
        ("PASSWORD", '"[redacted]"'),
        ("req.Authorization", '"[redacted]"'),
    ]


async def test_a_store_says_what_its_server_is(store: tinystore.Store) -> None:
    status = await store.status()
    assert status.protocol == 1 and "records" in status.engines and status.capability == "admin"


async def test_a_plan_says_what_a_read_would_spend_and_where_it_would_stop(store: tinystore.Store) -> None:
    now = datetime.now(UTC)
    samples = [(now - timedelta(seconds=s), float(s)) for s in (3, 2, 1)]
    await store.metrics.ingest({"name": "planned", "kind": "gauge", "samples": samples})
    plan = await store.metrics.explain(name="planned", since="1m")
    assert (plan.series, plan.decoded, plan.stops) == (1, 3, None)
    tight = await store.metrics.explain(name="planned", since="1m", limits={"decoded": 1})
    assert tight.stops is not None and (tight.stops.limit, tight.stops.bound) == ("decoded samples", 1)
    buckets = await store.metrics.explain(name="planned", since="1m", width="1m", op="sum")
    assert buckets.series == 1
