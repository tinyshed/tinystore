"""Metrics as docs/guides/metrics/README.md has them, through each way a store is reached.

Series ingested whole and read back bit for bit, the buckets of an
aggregate, the newest samples, what a store holds and its removal, and
instruments whose changes reach the store's totals once.
"""

from __future__ import annotations

import asyncio
import json
import math
import struct
from array import array
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING

import pytest

import tinystore
from tinystore._connection import Link, Started, in_process
from tinystore.instruments import Instruments, bin_of
from tinystore.metrics import Metrics, Series, exponential_buckets
from ways import CORE, SERVED

if TYPE_CHECKING:
    from pathlib import Path

    from tinystore._connection import Connection


async def last(metrics: Metrics, name: str) -> dict[str, float]:
    """The last value of each series of a name, by its labels."""
    read = await metrics.read(name=name, since="1h")
    return {json.dumps(series.labels, sort_keys=True): series.values[-1] for series in read if series.values}


def now_ms() -> int:
    return int(datetime.now(UTC).timestamp() * 1000)


async def test_a_series_comes_back_bit_for_bit_its_samples_as_two_columns(store: tinystore.Store) -> None:
    at = now_ms() - 60_000
    values = array("d", [0.42, -0.0, math.nan, math.inf])
    values[2] = struct.unpack("<d", struct.pack("<Q", 0x7FF8_0000_DEAD_BEEF))[0]
    times = [at, at + 1000, at + 2000, at + 3000]
    await store.metrics.ingest(Series(name="cpu", kind="gauge", labels={"host": "web-1"}, times=times, values=values))
    [cpu] = await store.metrics.read(name="cpu", where={"host": "web-1"}, since="1h")
    assert cpu.labels == {"host": "web-1"}
    assert list(cpu.times or []) == times
    assert isinstance(cpu.values, array)
    assert cpu.values.tobytes() == values.tobytes()
    newest = await store.metrics.latest(name="cpu", since="1h")
    assert [(point.at, point.labels["host"]) for point in newest] == [
        (datetime.fromtimestamp((at + 3000) / 1000, UTC), "web-1")
    ]


# contract: MET-04
async def test_an_aggregates_buckets_are_exact_by_group_and_a_timers_percentile_is_its_bounds(
    store: tinystore.Store,
) -> None:
    at = datetime.now(UTC) - timedelta(minutes=1)

    def route(name: str, values: list[float]) -> Series:
        samples = [(at + timedelta(seconds=i), value) for i, value in enumerate(values)]
        return Series(name="requests_total", kind="counter", labels={"route": name}, samples=samples)

    await store.metrics.ingest([route("/a", [0, 10, 25]), route("/b", [0, 3, 1])])
    traffic = await store.metrics.aggregate(name="requests_total", since="1h", op="increase", by=["route"])
    by_route = {group.labels["route"]: group.buckets[0] for group in traffic}
    assert (by_route["/a"].value, by_route["/a"].resets) == (25, 0)
    assert (by_route["/b"].value, by_route["/b"].resets) == (4, 1)

    async def routes(where: dict[str, tinystore.metrics.Condition]) -> list[str]:
        read = await store.metrics.read(name="requests_total", where=where, since="1h")
        return sorted(str(series.labels["route"]) for series in read)

    assert await routes({"route": {"not": "/a"}}) == ["/b"]
    assert await routes({"route": ["/a", "/b"]}) == ["/a", "/b"]
    assert await routes({"route": {"prefix": "/"}}) == ["/a", "/b"]
    latency = store.metrics.timer("db_ms", buckets=[5, 10, 25])
    for took in (3, 7, 20, 100):
        latency.record(timedelta(milliseconds=took))
    await store.metrics.flush()
    [p50] = await store.metrics.aggregate(name="db_ms", since="1h", op="p50")
    assert (p50.buckets[0].lower, p50.buckets[0].upper, p50.buckets[0].samples) == (5, 10, 4)
    [p99] = await store.metrics.aggregate(name="db_ms", since="1h", op="p99.9", estimate=True)
    assert (p99.buckets[0].lower, p99.buckets[0].upper, p99.buckets[0].estimate) == (25, math.inf, 25)
    [mean] = await store.metrics.aggregate(name="db_ms", since="1h", op="avg")
    assert mean.buckets[0].value == 32.5
    with pytest.raises(tinystore.InvalidError):
        await store.metrics.aggregate(name="db_ms", since="1h", op="p")


async def test_instruments_count_into_the_stores_totals_a_set_of_labels_a_series_each(
    store: tinystore.Store,
) -> None:
    requests = store.metrics.counter("http_requests_total", help="Requests served.")
    requests.labels(route="/notes", status=200).inc()
    requests.labels(status=200, route="/notes").add(2)
    requests.labels(route="/users", status=200).add(0)
    depth = store.metrics.gauge("queue_depth", unit="jobs")
    depth.set(12)
    depth.inc()

    async def tickets() -> float:
        return 7

    store.metrics.gauge_func("open_tickets", tickets)
    sizes = store.metrics.histogram("response_bytes", unit="bytes", buckets=[100, 1000])
    sizes.record(50)
    sizes.record(5000)
    await store.metrics.flush()
    assert await last(store.metrics, "http_requests_total") == {
        '{"route": "/notes", "status": "200"}': 3,
        '{"route": "/users", "status": "200"}': 0,
    }
    assert await last(store.metrics, "queue_depth") == {"{}": 13}
    assert await last(store.metrics, "open_tickets") == {"{}": 7}
    assert await last(store.metrics, "response_bytes_count") == {"{}": 2}
    assert await last(store.metrics, "response_bytes_sum") == {"{}": 5050}
    description = await store.metrics.description("http_requests_total")
    assert (description.unit, description.help) == ("", "Requests served.")


async def test_a_timers_measure_records_whether_its_block_returns_or_raises(store: tinystore.Store) -> None:
    took = store.metrics.timer("measured_ms")
    async with took.measure():
        await asyncio.sleep(0)
    with pytest.raises(tinystore.InvalidError):
        async with took.measure():
            raise tinystore.InvalidError("no")
    timing = took.start()
    assert timing.stop() >= timedelta(0)
    await store.metrics.flush()
    assert await last(store.metrics, "measured_ms_count") == {"{}": 3}


# contract: MET-10
async def test_an_instrument_the_store_refuses_fails_the_next_flush_and_keeps_no_other_out(
    store: tinystore.Store,
) -> None:
    await store.metrics.ingest(Series(name="taken", kind="gauge", samples=[(now_ms() - 1000, 1)]))
    store.metrics.counter("taken").inc()
    store.metrics.counter("others_total").inc()
    with pytest.raises(tinystore.InvalidError):
        await store.metrics.flush()
    await store.metrics.flush()
    assert await last(store.metrics, "others_total") == {"{}": 1}
    assert await last(store.metrics, "taken") == {"{}": 1}


async def test_a_call_that_would_take_a_sum_past_the_largest_number_raises_where_the_program_made_it(
    store: tinystore.Store,
) -> None:
    huge = store.metrics.histogram("huge_bytes")
    huge.record(1e308)
    with pytest.raises(tinystore.InvalidError):
        huge.record(1e308)
    added = store.metrics.counter("huge_total")
    added.add(1e308)
    with pytest.raises(tinystore.InvalidError):
        added.add(1e308)
    await store.metrics.flush()
    assert await last(store.metrics, "huge_bytes_count") == {"{}": 1}
    assert await last(store.metrics, "huge_total") == {"{}": 1e308}


async def test_a_gauges_functions_read_a_set_of_labels_each_and_a_set_is_read_by_one(store: tinystore.Store) -> None:
    async def sms() -> float:
        return 5

    store.metrics.gauge_func("queue_depth_jobs", lambda: 3, labels={"queue": "emails"})
    store.metrics.gauge_func("queue_depth_jobs", sms, labels={"queue": "sms"})
    with pytest.raises(tinystore.InvalidError):
        store.metrics.gauge_func("queue_depth_jobs", lambda: 7, labels={"queue": "sms"})
    with pytest.raises(tinystore.InvalidError):
        store.metrics.gauge("queue_depth_jobs")
    await store.metrics.flush()
    assert await last(store.metrics, "queue_depth_jobs") == {'{"queue": "emails"}': 3, '{"queue": "sms"}': 5}


# contract: MET-10
async def test_a_call_an_instrument_refuses_raises_where_the_program_made_it(store: tinystore.Store) -> None:
    counter = store.metrics.counter("refusals_total")
    refused = [
        lambda: counter.add(-1),
        lambda: counter.add(math.nan),
        lambda: counter.labels(__store="a"),
        lambda: store.metrics.gauge("refused_depth").set(math.inf),
        lambda: store.metrics.timer("refused_ms").record(timedelta(milliseconds=-5)),
        lambda: store.metrics.timer("refused_ms").labels(le="5"),
        lambda: store.metrics.timer("unordered_ms", buckets=[10, 5]),
        lambda: store.metrics.gauge("refusals_total"),
        lambda: store.metrics.timer("bare_ms").record(5),  # pyright: ignore[reportArgumentType]
    ]
    for call in refused:
        with pytest.raises(tinystore.InvalidError):
            call()
    store.metrics.gauge_func("read_once", lambda: 1)
    with pytest.raises(tinystore.InvalidError):
        store.metrics.gauge_func("read_once", lambda: 2)


async def test_a_call_made_while_a_batch_is_on_its_way_counts_in_the_next(store: tinystore.Store) -> None:
    jobs = store.metrics.counter("jobs_total")
    for _ in range(20):
        jobs.inc()
        flushing = asyncio.ensure_future(store.metrics.flush())
        await asyncio.sleep(0)
        jobs.inc()
        await flushing
    await store.metrics.flush()
    assert await last(store.metrics, "jobs_total") == {"{}": 40}


async def test_what_a_store_holds_a_page_at_a_time_and_its_removal(store: tinystore.Store) -> None:
    requests = store.metrics.counter("http_requests_total", help="Requests served.")
    requests.labels(route="/notes", status=200).inc()
    requests.labels(route="/users", status=200).inc()
    await store.metrics.flush()
    at = now_ms() - 1000
    await store.metrics.ingest(Series(name="cpu", kind="gauge", labels={"host": "web-1"}, samples=[(at, 0.5)]))
    page = await store.metrics.names(prefix="http_")
    [held] = [name for name in page.names if name.name == "http_requests_total"]
    assert (held.kind, held.series_count, held.help, held.keep) == ("counter", 2, "Requests served.", "30d")
    assert held.retained_from <= held.newest
    assert page.next is None
    first = await store.metrics.names(limit=1)
    assert len(first.names) == 1
    rest = await store.metrics.names(after=first.next)
    assert first.names[0].name not in [name.name for name in rest.names]
    assert (await store.metrics.label_names("http_requests_total")).labels == ["route", "status"]
    assert (await store.metrics.label_values("http_requests_total", "route", prefix="/n")).values == ["/notes"]
    await store.metrics.describe("cpu", unit="%", help="Busy.")
    assert await store.metrics.description("cpu") == tinystore.metrics.Description("%", "Busy.")
    await store.metrics.describe("cpu")
    assert await store.metrics.description("cpu") == tinystore.metrics.Description("", "")
    assert await store.metrics.delete(name="cpu", labels={"host": "web-1"})
    assert not await store.metrics.delete(name="cpu", labels={"host": "web-1"})
    assert await store.metrics.damaged() == []
    plan = await store.metrics.explain(name="http_requests_total", since="1h", every="1m", op="rate")
    assert (plan.series, plan.exceeds) == (2, None)


async def test_a_query_is_refused_before_it_leaves(store: tinystore.Store) -> None:
    with pytest.raises(tinystore.InvalidError, match="not both"):
        await store.metrics.read(name="cpu", since="1h", from_=datetime.now(UTC))
    with pytest.raises(tinystore.InvalidError, match="None"):
        await store.metrics.read(name="cpu", where={"host": None})  # pyright: ignore[reportArgumentType]
    with pytest.raises(tinystore.InvalidError, match="as long as each other"):
        await store.metrics.ingest(Series(name="cpu", kind="gauge", times=[1, 2], values=[1.0]))
    with pytest.raises(tinystore.InvalidError, match="no zone"):
        await store.metrics.ingest(Series(name="cpu", kind="gauge", samples=[(datetime(2026, 10, 18), 1.0)]))


def _embedded(directory: Path, dialled: list[Connection], down: list[bool] | None = None) -> Link:
    """A link to the core in this process that a test can cut, keeping each connection it dialled."""
    dial = in_process(directory, None, Started())

    async def dialling() -> Connection:
        if down is not None and down[0]:
            raise tinystore.UnavailableError("the store is down")
        connection = await dial()
        dialled.append(connection)
        return connection

    return Link(dialling)


@pytest.mark.skipif(not CORE, reason="the core is not built here")
async def test_a_batch_whose_answer_a_lost_connection_took_counts_once(directory: Path) -> None:
    # a second handle keeps the store open while the connection under test is cut
    keeper = await tinystore.open(directory, embedded=True)
    dialled: list[Connection] = []
    link = _embedded(directory, dialled)
    instruments = Instruments(link)
    metrics = Metrics(link, instruments)
    jobs = metrics.counter("cut_jobs_total")
    jobs.inc()
    await metrics.flush()
    for cut in range(5):
        jobs.add(2)
        flushed = asyncio.ensure_future(metrics.flush())
        await asyncio.sleep(cut / 1000)
        dialled[-1].session.end(ConnectionError("cut"))
        await flushed
    assert await last(keeper.metrics, "cut_jobs_total") == {"{}": 11}
    await instruments.stop()
    await link.close()
    # a cut connection's pipe still holds the store until it closes
    for connection in dialled:
        await connection.close()
    await keeper.close()


@pytest.mark.skipif(not CORE, reason="the core is not built here")
async def test_a_flush_the_store_cannot_be_reached_for_fails_and_what_it_carried_goes_once_it_can_be(
    directory: Path,
) -> None:
    keeper = await tinystore.open(directory, embedded=True)
    dialled: list[Connection] = []
    down = [False]
    link = _embedded(directory, dialled, down)
    instruments = Instruments(link)
    metrics = Metrics(link, instruments)
    jobs = metrics.counter("down_jobs_total")
    jobs.inc()
    await metrics.flush()
    down[0] = True
    dialled[-1].session.end(ConnectionError("cut"))
    jobs.add(2)
    with pytest.raises(tinystore.UnavailableError):
        await metrics.flush()
    down[0] = False
    await metrics.flush()
    assert await last(keeper.metrics, "down_jobs_total") == {"{}": 3}
    await instruments.stop()
    await link.close()
    # a cut connection's pipe still holds the store until it closes
    for connection in dialled:
        await connection.close()
    await keeper.close()


@pytest.mark.skipif(not CORE, reason="the core is not built here")
async def test_a_flush_a_store_that_started_again_may_have_counted_fails_its_outcome_unknown(
    directory: Path,
) -> None:
    first, second = directory / "first", directory / "second"
    keeper = await tinystore.open(first, embedded=True)
    dials = [in_process(first, None, Started())]
    # a connection that hears nothing: the store applies a batch whose answer never comes
    deaf = [False]
    dialled: list[Connection] = []

    async def dialling() -> Connection:
        connection = await dials[-1]()
        receive = connection.session.receive
        connection.session.receive = lambda chunk: None if deaf[0] else receive(chunk)  # type: ignore[method-assign]
        dialled.append(connection)
        return connection

    link = Link(dialling)
    instruments = Instruments(link)
    metrics = Metrics(link, instruments)
    jobs = metrics.counter("restart_jobs_total")
    jobs.inc()
    await metrics.flush()
    deaf[0] = True
    jobs.add(2)
    flushed = asyncio.ensure_future(metrics.flush())
    totals: dict[str, float] = {}

    async def counted() -> None:
        totals.update(await last(keeper.metrics, "restart_jobs_total"))

    deadline = asyncio.get_running_loop().time() + 10
    while totals.get("{}") != 3 and asyncio.get_running_loop().time() < deadline:
        await counted()
        await asyncio.sleep(0.02)
    # the next connection reaches a store that knows no session, as one started again does
    dials.append(in_process(second, None, Started()))
    deaf[0] = False
    dialled[-1].session.end(ConnectionError("restarted"))
    with pytest.raises(tinystore.OutcomeUnknownError):
        await flushed
    jobs.inc()
    await metrics.flush()
    restarted = await tinystore.open(second, embedded=True)
    assert await last(keeper.metrics, "restart_jobs_total") == {"{}": 3}
    assert await last(restarted.metrics, "restart_jobs_total") == {"{}": 1}
    await instruments.stop()
    await link.close()
    # a cut connection's pipe still holds the store until it closes
    for connection in dialled:
        await connection.close()
    await restarted.close()
    await keeper.close()


def test_a_value_falls_in_the_interval_of_the_grid_where_the_store_puts_it() -> None:
    assert [bin_of(0), bin_of(1), bin_of(1.05), bin_of(0.99), bin_of(math.sqrt(2))] == [
        "zero",
        "3:0",
        "3:1",
        "3:0",
        "3:5",
    ]
    below = struct.unpack(">d", struct.pack(">Q", struct.unpack(">Q", struct.pack(">d", math.sqrt(2)))[0] - 1))[0]
    assert bin_of(below) == "3:4"
    assert bin_of(1.7976931348623157e308) == "3:8192"
    assert exponential_buckets(1, 2, 4) == [1, 2, 4, 8]


def test_a_label_is_written_as_every_language_writes_it() -> None:
    from tinystore.instruments import label_text

    assert [label_text("a", value) for value in (200, 5.0, 0.25, 1e21, True, "x")] == [
        "200",
        "5",
        "0.25",
        "1e+21",
        "true",
        "x",
    ]


@pytest.mark.parametrize(
    ("name", "ready", "embedded"),
    [("the core in this process", CORE, True), ("a private child", SERVED, False)],
)
async def test_a_store_opened_with_self_metrics_measures_its_commits(
    name: str, ready: bool, embedded: bool, directory: Path
) -> None:
    if not ready:
        pytest.skip(f"{name}: not built here")
    store = await tinystore.open(directory, self_metrics=True, embedded=embedded, private=not embedded)
    await store.bucket("sessions").set("a", "x")
    await store.metrics.flush()
    newest = await store.metrics.latest(name="tinystore_commits_total", where={"file": "kv"}, since="1h")
    assert newest
    assert newest[0].value > 0
    await store.close()


@pytest.mark.parametrize(
    ("name", "ready", "embedded"),
    [("the core in this process", CORE, True), ("a private child", SERVED, False)],
)
async def test_a_store_holds_the_series_max_series_says(
    name: str, ready: bool, embedded: bool, directory: Path
) -> None:
    if not ready:
        pytest.skip(f"{name}: not built here")
    store = await tinystore.open(directory, max_series=2, embedded=embedded, private=not embedded)

    def host(name: str) -> Series:
        return Series(name="cpu", kind="gauge", labels={"host": name}, samples=[(now_ms() - 1000, 1)])

    await store.metrics.ingest([host("a"), host("b")])
    with pytest.raises(tinystore.LimitError, match="a series past the 2 a store holds"):
        await store.metrics.ingest(host("c"))
    await store.close()


async def test_a_count_of_series_no_store_can_hold_is_refused_before_anything_opens(directory: Path) -> None:
    with pytest.raises(tinystore.InvalidError):
        await tinystore.open(directory, max_series=0)


async def test_a_store_opened_without_self_metrics_holds_none_of_its_own_names(store: tinystore.Store) -> None:
    await store.bucket("sessions").set("a", "x")
    await store.metrics.flush()
    assert await store.metrics.latest(name="tinystore_commits_total", since="1h") == []


@pytest.mark.skipif(not CORE, reason="the core in this process: not built here")
@pytest.mark.parametrize(("asked", "measured"), [(True, True), ({"system": False}, False)])
async def test_self_metrics_measures_the_machine_unless_system_is_false(
    asked: bool | tinystore.SelfMetrics, measured: bool, directory: Path
) -> None:
    store = await tinystore.open(directory, embedded=True, self_metrics=asked)
    await store.metrics.flush()
    memory = await store.metrics.latest(name="tinystore_system_memory_bytes", since="1h")
    assert len(memory) == (1 if measured else 0)
    await store.close()
