"""jobs as plan/api/jobs.md has it, through each way a store is reached.

Ids, answers, steps, progress, cancels, schedules, pages, concurrency and
dedupe, the handlers running here while the store's loop claims their jobs.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING, Any, TypedDict

import pytest

import tinystore
from ways import SERVED, until

if TYPE_CHECKING:
    from pathlib import Path


@dataclass
class Reminder:
    user_id: int
    text: str


class Text(TypedDict):
    text: str


async def test_an_id_adds_a_job_once_set_makes_it_whatever_it_was_update_and_cancel(store: tinystore.Store) -> None:
    later = store.queue("later", Text)
    job_id = "chat:42:draft-1"
    assert await later.add({"text": "hi"}, id=job_id, delay="1h")
    assert not await later.add({"text": "hi again"}, id=job_id, delay="1h")
    added = await later.get(job_id)
    assert added is not None
    assert (added.state, added.value, added.attempt) == ("scheduled", {"text": "hi"}, 0)
    await later.set(job_id, {"text": "edited"}, delay="2h")
    edited = await later.get(job_id)
    assert edited is not None and edited.value == {"text": "edited"}
    assert await later.update(job_id, {"text": "edited twice"})
    updated = await later.get(job_id)
    assert updated is not None and (updated.value, updated.state) == ({"text": "edited twice"}, "scheduled")
    assert await later.cancel(job_id)
    assert await later.get(job_id) is None
    assert not await later.update(job_id, {"text": "too late"})
    assert not await later.cancel(job_id)


async def test_a_worker_runs_each_job_as_it_falls_due_the_value_first_and_the_run_second(
    store: tinystore.Store,
) -> None:
    reminders = store.queue("reminders", Reminder)
    ran: list[tuple[int, str | None, int]] = []

    async def remind(reminder: Reminder, run: tinystore.Run) -> None:
        ran.append((reminder.user_id, run.id, run.attempt))

    worker = reminders.work(remind)
    await reminders.add(Reminder(1, "Call mom"), id="r1")
    await reminders.add(Reminder(2, "Drink water"))
    await until(lambda: len(ran) == 2)
    await worker.stop()
    assert ran == [(1, "r1", 1), (2, None, 1)]
    assert await reminders.get("r1") is None


async def test_what_a_handler_returns_settles_its_job(store: tinystore.Store) -> None:
    pushes = store.queue("pushes", str, backoff=tinystore.Backoff(initial="1h", max="2h"))
    for how in ("done", "raised", "retry", "snooze", "fail", "unreturned"):
        await pushes.add(how, id=how)

    async def send(how: str, run: tinystore.Run) -> tinystore.Outcome | None:
        match how:
            case "raised":
                raise ConnectionError("the provider is down")
            case "retry":
                return run.retry("10m")
            case "snooze":
                return run.snooze("10m")
            case "fail":
                return run.fail("the provider no longer knows the token")
            case "unreturned":
                run.snooze("10m")
            case _:
                pass
        return None

    assert await pushes.run_due(send) == 6
    assert await pushes.get("done") is None
    raised = await pushes.get("raised")
    assert raised is not None and (raised.state, raised.attempt, raised.error) == (
        "scheduled",
        1,
        "the provider is down",
    )
    retry = await pushes.get("retry")
    assert retry is not None and (retry.state, retry.attempt) == ("scheduled", 1)
    snooze = await pushes.get("snooze")
    assert snooze is not None and (snooze.state, snooze.attempt) == ("scheduled", 0)
    fail = await pushes.get("fail")
    assert fail is not None and (fail.state, fail.error) == ("failed", "the provider no longer knows the token")
    unreturned = await pushes.get("unreturned")
    assert unreturned is not None and (unreturned.state, unreturned.attempt) == ("scheduled", 1)
    assert "run.snooze(...)" in (unreturned.error or "")


async def test_a_step_one_attempt_kept_is_found_by_the_next(store: tinystore.Store) -> None:
    orders = store.queue("orders", dict[str, int])
    await orders.add({"order": 7}, id="order:7")
    charged = 0
    attempts: list[str] = []

    def charge() -> dict[str, str]:
        nonlocal charged
        charged += 1
        return {"charge": "ch_1"}

    async def handle(_: dict[str, int], run: tinystore.Run) -> tinystore.Outcome | None:
        kept = await run.step("charge", charge)
        attempts.append(f"{run.attempt}:{kept['charge']}")
        return run.retry(datetime(1970, 1, 1, tzinfo=UTC)) if run.attempt == 1 else None

    assert await orders.run_due(handle) == 2
    assert attempts == ["1:ch_1", "2:ch_1"]
    assert charged == 1


async def test_what_a_handler_reports_shows_while_its_job_runs(store: tinystore.Store) -> None:
    videos = store.queue("videos", dict[str, int])
    await videos.add({"video": 3}, id="video:3")
    released = asyncio.Event()
    refused: list[BaseException] = []

    async def convert(_: dict[str, int], run: tinystore.Run) -> None:
        try:
            run.set_progress("x" * 5000)
        except tinystore.InvalidError as err:
            refused.append(err)
        run.set_progress({"frames": 120})
        await released.wait()

    worker = videos.work(convert)
    for _ in range(500):
        job = await videos.get("video:3")
        if job is not None and job.progress is not None:
            break
        await asyncio.sleep(0.02)
    running = await videos.get("video:3")
    assert running is not None and (running.state, running.progress) == ("running", {"frames": 120})
    assert len(refused) == 1
    released.set()
    await worker.stop()
    assert await videos.get("video:3") is None


async def test_a_cancel_cancels_a_running_handler_and_what_it_returns_settles_nothing(store: tinystore.Store) -> None:
    exports = store.queue("exports", dict[str, int])
    await exports.add({"report": 9}, id="export:9")
    stopped: list[bool] = []

    async def export(_: dict[str, int], run: tinystore.Run) -> tinystore.Outcome:
        try:
            await asyncio.Event().wait()
        except asyncio.CancelledError:
            stopped.append(run.stopped)
            return run.fail("it should settle nothing")
        return run.fail("never")

    worker = exports.work(export)
    for _ in range(500):
        job = await exports.get("export:9")
        if job is not None and job.state == "running":
            break
        await asyncio.sleep(0.02)
    assert await exports.cancel("export:9")
    await until(lambda: bool(stopped))
    assert stopped == [True]
    await worker.stop()
    assert await exports.get("export:9") is None


async def test_a_watch_follows_its_job_to_its_end_and_an_id_with_no_job_yields_nothing(store: tinystore.Store) -> None:
    clips = store.queue("clips", dict[str, int])
    await clips.add({"clip": 1}, id="clip:1")
    seen: list[str] = []

    async def watch() -> None:
        async for job in clips.watch("clip:1"):
            seen.append(job.state if job.progress is None else f"{job.state} {job.progress}")

    watching = asyncio.ensure_future(watch())
    await until(lambda: bool(seen))
    released = asyncio.Event()

    async def clip(_: dict[str, int], run: tinystore.Run) -> None:
        run.set_progress(0.5)
        await released.wait()

    worker = clips.work(clip)
    await until(lambda: bool(seen) and seen[-1] == "running 0.5")
    released.set()
    await watching
    assert (seen[0], seen[-1]) == ("waiting", "done 0.5")
    await worker.stop()
    assert [job async for job in clips.watch("clip:never")] == []


async def test_a_watch_of_a_job_a_cancel_took_ends_cancelled(store: tinystore.Store) -> None:
    clips = store.queue("clips", dict[str, int])
    await clips.add({"clip": 2}, id="clip:2", delay="1h")
    states: list[str] = []

    async def watch() -> None:
        async for job in clips.watch("clip:2"):
            states.append(job.state)

    watching = asyncio.ensure_future(watch())
    await until(lambda: bool(states))
    assert await clips.cancel("clip:2")
    await watching
    assert states == ["scheduled", "cancelled"]


async def test_a_schedule_keeps_its_one_job_and_a_cron_needs_a_time_zone(store: tinystore.Store) -> None:
    async def nightly_run(run: tinystore.Run) -> None:
        return None

    nightly = store.schedule("nightly", nightly_run, cron="10 3 * * *", time_zone="Europe/Berlin")
    job = None
    for _ in range(500):
        job = await nightly.get()
        if job is not None:
            break
        await asyncio.sleep(0.02)
    assert job is not None and (job.state, job.repeat) == ("scheduled", "10 3 * * * Europe/Berlin")
    await nightly.stop()
    assert await nightly.cancel()

    zone: str | None = None
    with pytest.raises(tinystore.InvalidError):
        store.schedule("digest", nightly_run, cron="0 20 * * *", time_zone=zone)
    digests = store.queue("digests")
    with pytest.raises(tinystore.InvalidError, match="time zone"):
        await digests.set("user:42", {}, cron="0 20 * * *", time_zone=zone)


async def test_a_page_reads_the_ids_under_a_prefix(store: tinystore.Store) -> None:
    chats = store.queue("chats", int)
    for n in (1, 2, 3):
        await chats.add(n, id=f"chat:4:{n}", delay="1h")
    await chats.add(42, id="chat:42:1", delay="1h")
    first = await chats.list(prefix="chat:4:", limit=2)
    assert [job.id for job in first.jobs] == ["chat:4:1", "chat:4:2"]
    second = await chats.list(prefix="chat:4:", limit=2, after=first.next)
    assert [job.id for job in second.jobs] == ["chat:4:3"]
    assert second.next is None
    # byte order: "2" is 0x32, before ":" at 0x3a
    assert [job.id async for job in chats.all(prefix="chat:")] == ["chat:42:1", "chat:4:1", "chat:4:2", "chat:4:3"]


async def test_a_value_that_no_longer_reads_as_its_type_fails_its_job_not_its_queue(store: tinystore.Store) -> None:
    loose = store.queue("checked")
    await loose.add({"user_id": "not a number"}, id="bad")
    checked = store.queue("checked", Reminder)
    await checked.add(Reminder(5, "Call mom"), id="good")
    got: list[str] = []

    async def remind(reminder: Reminder, run: tinystore.Run) -> None:
        got.append(reminder.text)

    assert await checked.run_due(remind) == 2
    assert got == ["Call mom"]
    bad = await loose.get("bad")
    assert bad is not None and bad.state == "failed"
    assert "no longer reads" in (bad.error or "")


async def test_a_worker_runs_as_many_handlers_at_once_as_its_concurrency_says(store: tinystore.Store) -> None:
    refreshes = store.queue("refreshes", int)
    for n in (1, 2, 3, 4):
        await refreshes.add(n)
    running = most = finished = 0

    async def refresh(_: int, run: tinystore.Run) -> None:
        nonlocal running, most, finished
        running += 1
        most = max(most, running)
        await asyncio.sleep(0.05)
        running -= 1
        finished += 1

    worker = refreshes.work(refresh, concurrency=2)
    await until(lambda: finished == 4)
    await worker.stop()
    assert (finished, most) == (4, 2)


async def test_dedupe_keeps_a_done_id_taken_and_the_job_its_last_run(store: tinystore.Store) -> None:
    welcome = store.queue("welcome", str, dedupe="1h")
    assert await welcome.add("ada", id="welcome:ada")

    async def nothing(_: str, run: tinystore.Run) -> None:
        return None

    assert await welcome.run_due(nothing) == 1
    assert not await welcome.add("ada", id="welcome:ada")
    done = await welcome.get("welcome:ada")
    assert done is not None and (done.state, done.value) == ("done", "ada")
    assert done.last_run is not None and done.last_run.ended_at >= done.last_run.started_at


async def test_a_value_json_cannot_write_is_refused_before_it_leaves(store: tinystore.Store) -> None:
    odd = store.queue("odd")
    with pytest.raises(tinystore.InvalidError):
        await odd.add(object())
    with pytest.raises(tinystore.InvalidError):
        await odd.add(float("nan"))


async def test_a_transaction_of_the_store_makes_its_jobs_together_or_none(store: tinystore.Store) -> None:
    fanout = store.queue("fanout", dict[str, str])

    async def notify(tx: tinystore.Tx) -> None:
        for member in ("ann", "bob"):
            await tx.with_(fanout).add({"member": member}, id=member)
        await tx.with_(fanout).cancel("bob")

    await store.tx(notify)
    ann = await fanout.get("ann")
    assert ann is not None and ann.value == {"member": "ann"}
    assert await fanout.get("bob") is None

    async def refused(tx: tinystore.Tx) -> None:
        await tx.with_(fanout).add({"member": "eve"}, id="eve")
        await tx.with_(fanout).set("", {"member": "nobody"})

    with pytest.raises(tinystore.InvalidError) as err:
        await store.tx(refused)
    assert err.value.what["write"] == "1"
    assert await fanout.get("eve") is None

    async def mixed(tx: tinystore.Tx) -> Any:
        await tx.with_(fanout).add({"member": "zed"}, id="zed")
        await tx.with_(store.bucket("seen", bool)).set("zed", True)

    with pytest.raises(tinystore.InvalidError, match=r"kept in kv\.db, outside this transaction of jobs\.db"):
        await store.tx(mixed)
    assert await fanout.get("zed") is None


@pytest.mark.skipif(not SERVED, reason="tinystore is not built here")
async def test_run_due_runs_what_the_tests_clock_made_due(directory: Path) -> None:
    start = datetime(2026, 10, 9, 9, tzinfo=UTC)
    async with await tinystore.open(directory, private=True, clock=start) as store:
        reminders = store.queue("reminders", Reminder)
        await reminders.add(Reminder(42, "Call mom"), delay="1h")
        ran: list[datetime] = []

        async def remind(_: Reminder, run: tinystore.Run) -> None:
            ran.append(run.at)

        assert await reminders.run_due(remind) == 0
        await store.clock.advance("1h")
        assert await reminders.run_due(remind) == 1
        assert ran == [start + timedelta(hours=1)]
