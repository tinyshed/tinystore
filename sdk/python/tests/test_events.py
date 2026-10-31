"""Streams of events as docs/guides/records/README.md has them, through each way a store is reached.

Appends written whole, reads by time and by what events hold a page at a
time, counts, what a store holds, a poll's cursor, a tail, another program's
lines, and a store's logger keeping its lines.
"""

from __future__ import annotations

import asyncio
import io
import json
import logging
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING

import pytest

import tinystore
from tinystore import Event

if TYPE_CHECKING:
    from collections.abc import Callable

TRACE_ID = "4bf92f3577b34da6a3ce929d0e0e4736"
SPAN_ID = "00f067aa0ba902b7"


async def test_an_appended_event_comes_back_whole(store: tinystore.Store) -> None:
    audit = store.events("audit")
    changed_at = (datetime.now(UTC) - timedelta(minutes=1)).replace(microsecond=123000)
    await audit.append(
        "role.changed",
        {"user": 42, "id": 9007199254740993, "tags": ["a", "b"]},
        at=changed_at,
        context={"actor": 7},
        level="warn",
        message="role changed",
    )
    with tinystore.trace(trace_id=TRACE_ID, span_id=SPAN_ID):
        await audit.append("signed.in", {"user": 42})
    page = await audit.list()
    assert page.next is None
    assert [event.name for event in page.events] == ["role.changed", "signed.in"]
    changed, signed = page.events
    assert changed.at == changed_at
    assert changed.stream == "audit"
    assert changed.fields == {"user": 42, "id": 9007199254740993, "tags": ["a", "b"]}
    assert changed.context == {"actor": 7}
    assert (changed.level, changed.message) == ("warn", "role changed")
    assert (signed.trace_id, signed.span_id) == (TRACE_ID, SPAN_ID)


async def test_an_event_whose_id_its_stream_holds_is_written_as_nothing_and_its_id_read_back(
    store: tinystore.Store,
) -> None:
    deploys = store.events("deploys")
    delivery = "72d3162e-cc78-11e3-81ab-4c9367dc0958"
    assert await deploys.append("push", {"sha": "26a0865"}, id=delivery) is True
    assert await deploys.append("push", {"sha": "f00ba47"}, id=delivery) is False
    assert await deploys.append("tick") is True
    events = [Event("push", id=delivery), Event("tag", id="v0.2.0"), Event("tag", id="v0.2.0"), Event("tick")]
    assert await deploys.append(events) == 2
    page = await deploys.list()
    assert [(event.name, event.id) for event in page.events] == [
        ("push", delivery),
        ("tick", None),
        ("tag", "v0.2.0"),
        ("tick", None),
    ]
    assert page.events[0].fields == {"sha": "26a0865"}
    with pytest.raises(tinystore.InvalidError, match="an id"):
        await deploys.append("push", id="")


async def test_a_list_is_appended_whole_or_not_at_all_and_a_refused_event_names_its_place(
    store: tinystore.Store,
) -> None:
    clicks = store.events("clicks")
    with pytest.raises(tinystore.InvalidError, match="event 1"):
        await clicks.append([Event("view", {"page": 1}), Event("")])
    with pytest.raises(tinystore.TinystoreError):
        await clicks.append([Event("view", {"page": 1}), Event("view", at=datetime.now(UTC) + timedelta(hours=1))])
    assert await clicks.count() == 0


async def test_a_query_finds_by_what_events_hold_a_page_at_a_time(store: tinystore.Store) -> None:
    api = store.events("api")
    # a millisecond apart: a page never splits a time
    base = datetime.now(UTC) - timedelta(minutes=1)
    await api.append(
        [
            Event(
                "request",
                {"route": "/login" if n % 3 == 0 else "/notes", "ms": n},
                at=base + timedelta(milliseconds=n),
                level="error" if n % 10 == 0 else "info",
            )
            for n in range(30)
        ]
    )
    assert len((await api.list(where={"route": "/login"})).events) == 10
    assert [event.fields["ms"] for event in (await api.list(min_level="error")).events] == [0, 10, 20]
    assert [e.fields["ms"] for e in (await api.list(search="LOGIN", newest=True, limit=2)).events] == [27, 24]
    first = await api.list(limit=12)
    assert len(first.events) == 12 and first.next is not None
    assert [event.fields["ms"] async for event in api.all(limit=7)] == list(range(30))
    assert len((await store.events().list(streams=["api"], names=["request"])).events) == 30


async def test_a_condition_given_as_none_is_invalid_never_a_condition_left_out(store: tinystore.Store) -> None:
    api = store.events("api")
    with pytest.raises(tinystore.InvalidError):
        await api.list(trace_id=None)  # type: ignore[arg-type]
    with pytest.raises(tinystore.InvalidError):
        await api.count(where={"user": None})


async def test_counts_a_keys_top_values_and_rows_of_a_span_with_their_stats(store: tinystore.Store) -> None:
    checkout = store.events("checkout")
    await checkout.append(
        [Event("paid", {"method": "card" if ms < 25 else "cash", "ms": ms}) for ms in (10, 20, 30, 40)]
    )
    assert await checkout.count(names=["paid"]) == 4
    assert await checkout.top("method") == [tinystore.Top("card", 2), tinystore.Top("cash", 2)]
    rows = await checkout.aggregate(by=["method"], stats=["ms"], distinct=["ms"])
    assert [(row.by, row.count, row.stats["ms"], row.distinct) for row in rows] == [
        ({"method": "card"}, 2, tinystore.Stats(2, 30, 10, 20, 15.0), {"ms": 2}),
        ({"method": "cash"}, 2, tinystore.Stats(2, 70, 30, 40, 35.0), {"ms": 2}),
    ]


async def test_a_store_says_its_streams_what_each_keeps_and_the_keys_events_hold(store: tinystore.Store) -> None:
    kept = store.events("kept", keep="30d")
    await kept.append("signup", {"plan": "pro", "user": 1})
    found = [info for info in await store.events().streams() if info.name == "kept"]
    assert len(found) == 1 and found[0].keep == "30d"
    assert found[0].retained_from <= found[0].newest
    assert await kept.field_names(prefix="p") == ["plan"]


async def test_a_poll_gives_a_cursor_and_nothing_not_packed_yet(store: tinystore.Store) -> None:
    batch = await store.events().poll()
    assert batch.events == []
    assert batch.cursor != ""
    with pytest.raises(tinystore.InvalidError):
        await store.events().poll(batch.cursor, limit=0)


async def test_a_tail_gives_the_events_written_after_it_a_batch_as_they_come(store: tinystore.Store) -> None:
    live = store.events("live")
    tail = live.tail(names=["tick"])
    first = asyncio.ensure_future(anext(tail))

    async def tick() -> None:
        n = 0
        while True:
            await live.append("tick", {"n": n})
            n += 1
            await asyncio.sleep(0.02)

    ticking = asyncio.ensure_future(tick())
    try:
        batch = await asyncio.wait_for(first, 10)
        assert batch.missed == 0
        assert ("live", "tick") in [(event.stream, event.name) for event in batch.events]
    finally:
        ticking.cancel()
        await tail.aclose()


async def test_another_programs_output_becomes_events_however_it_was_cut(store: tinystore.Store) -> None:
    async with store.events("child").lines() as out:
        await out.write('level=warn msg="slow request" ms=12')
        await out.write(b'00\n{"level":30,"msg":"ok"}\nthe last line')
    assert out.dropped == 0
    events = (await store.events("child").list()).events
    assert [(event.name, event.level) for event in events] == [("logfmt", "warn"), ("json", "info"), ("log", None)]
    assert events[2].message == "the last line"
    with pytest.raises(tinystore.ClosedError):
        await out.write("late\n")


async def test_nothing_damaged_is_listed_and_what_still_reads_is_never_dropped(store: tinystore.Store) -> None:
    await store.events("audit").append("signed.in", {"user": 42})
    every = store.events()
    assert await every.damaged() == []
    now = datetime.now(UTC)
    with pytest.raises(tinystore.InvalidError):
        await every.drop(tinystore.Damage("head row", 1, "audit", now, now, ""))


async def test_a_stores_logger_never_waits_a_full_buffer_drops_and_counts(store: tinystore.Store) -> None:
    log = store.logger("busy", console="off", buffer=2)
    for n in range(6):
        log.info("a line", n=n)
    assert log.dropped > 0
    await log.flush()
    kept = len((await store.events("busy").list()).events)
    assert kept + log.dropped == 6


async def test_a_stores_logger_keeps_a_buffer_of_more_text_than_one_request_holds(store: tinystore.Store) -> None:
    log = store.logger("stacks", console="off", buffer=64)
    stack = "x" * (100 * 1024)
    for _ in range(24):
        log.error("failed", stack=stack)
    await log.flush()
    assert (await store.events("stacks").count(since="1h"), log.dropped) == (24, 0)


async def test_a_stores_logger_keeps_its_lines_their_fields_and_the_context_of_bind(store: tinystore.Store) -> None:
    log = store.logger("worker", console="off")
    log.info("started", port=3000)
    log.bind(job=7).warning("slow", ms=1200)
    log.event("signup", plan="pro")
    await log.flush()
    events = (await store.events("worker").list()).events
    assert [(event.name, event.level, event.message) for event in events] == [
        ("log", "info", "started"),
        ("log", "warn", "slow"),
        ("signup", None, None),
    ]
    assert (events[1].fields, events[1].context) == ({"ms": 1200}, {"job": 7})
    assert log.dropped == 0


# contract: LOG-10
async def test_a_key_a_nested_bind_gives_again_is_kept_and_found_by_its_new_value_alone(store: tinystore.Store) -> None:
    log = store.logger("access", console="off")
    log.bind(component="http", user=7).bind(component="access").info("request completed")
    await log.flush()
    access = store.events("access")
    assert [event.context for event in (await access.list()).events] == [{"component": "access", "user": 7}]
    found = [len((await access.list(where={"component": c})).events) for c in ("access", "http")]
    assert found == [1, 0]


async def test_a_handler_gives_logging_the_stores_logger(store: tinystore.Store) -> None:
    worker = logging.getLogger("tinystore-test-worker")
    worker.propagate = False
    worker.setLevel(logging.DEBUG)
    handler = store.log_handler("jobs", console="off")
    worker.addHandler(handler)
    try:
        worker.info("job done", extra={"job": 7})
        try:
            raise ValueError("card declined")
        except ValueError:
            worker.exception("charge failed")
    finally:
        worker.removeHandler(handler)
    await handler.logger.flush()
    events = (await store.events("jobs").list()).events
    assert [(event.level, event.message) for event in events] == [("info", "job done"), ("error", "charge failed")]
    assert events[0].fields == {"job": 7}
    assert events[0].context == {"logger": "tinystore-test-worker"}
    assert "card declined" in events[1].fields["error"]


def _messages(out: io.StringIO) -> list[str]:
    """The messages a JSON console printed, an event's name in place of one."""
    return [(line.get("msg") or line["event"]) for line in map(json.loads, out.getvalue().splitlines())]


# contract: LOG-08
async def test_a_stores_logger_with_a_console_part_prints_less_than_it_keeps(store: tinystore.Store) -> None:
    out = io.StringIO()
    levels: tinystore.Levels = {"store": "info", "console": "warning"}
    log = store.logger("quiet", level=levels, console="json", to=out, env=False)
    log.debug("cache miss")
    log.info("signed in")
    log.warning("slow request")
    log.event("signup", plan="pro")
    await log.flush()
    events = (await store.events("quiet").list()).events
    assert _messages(out) == ["slow request", "signup"]
    assert [event.message or event.name for event in events] == ["signed in", "slow request", "signup"]


def _logged_through(handler: logging.Handler, log: Callable[[logging.Logger], None]) -> None:
    """Logs through a logging logger of every level that hands its records to handler alone."""
    worker = logging.getLogger("tinystore-test-handled")
    worker.propagate = False
    worker.setLevel(logging.DEBUG)
    worker.addHandler(handler)
    try:
        log(worker)
    finally:
        worker.removeHandler(handler)


def _three_levels(worker: logging.Logger) -> None:
    worker.debug("cache miss")
    worker.info("signed in")
    worker.warning("slow request")


# contract: LOG-08
async def test_a_handler_takes_the_stores_loggers_level_in_two_parts(store: tinystore.Store) -> None:
    out = io.StringIO()
    levels: tinystore.Levels = {"store": "info", "console": "warning"}
    handler = store.log_handler("handled", level=levels, console="json", to=out, env=False)
    _logged_through(handler, _three_levels)
    await handler.logger.flush()
    events = (await store.events("handled").list()).events
    assert _messages(out) == ["slow request"]
    assert [event.message for event in events] == ["signed in", "slow request"]


# contract: LOG-07
async def test_a_handler_follows_log_level_as_the_stores_logger_does(
    store: tinystore.Store, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("LOG_LEVEL", "warning")
    handler = store.log_handler("leveled", console="off")
    _logged_through(handler, _three_levels)
    await handler.logger.flush()
    events = (await store.events("leveled").list()).events
    assert [event.message for event in events] == ["slow request"]
