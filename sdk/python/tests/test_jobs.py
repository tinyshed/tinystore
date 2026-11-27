"""jobs as docs/guides/jobs/README.md has it, through each way a store is reached.

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


# contract: JOB-24
async def test_a_job_due_inside_its_quiet_hours_is_due_at_their_end_and_a_calls_own_wins(
    store: tinystore.Store,
) -> None:
    now = datetime.now(UTC)
    night = tinystore.Quiet(from_=f"{(now.hour - 1) % 24:02d}:00", to=f"{(now.hour + 2) % 24:02d}:00", time_zone="UTC")
    pushes = store.queue("pushes-at-night", Text, quiet=night)
    await pushes.add({"text": "held"}, id="held")
    await pushes.add({"text": "urgent"}, id="urgent", quiet=False)
    held = await pushes.get("held")
    end = now.replace(minute=0, second=0, microsecond=0) + timedelta(hours=2)
    assert held is not None and held.at == end
    urgent = await pushes.get("urgent")
    assert urgent is not None and urgent.at - now < timedelta(minutes=1)
    with pytest.raises(tinystore.InvalidError):
        await pushes.add({"text": "x"}, quiet=tinystore.Quiet(from_="23:00", to="23:00", time_zone="UTC"))


# contract: JOB-22, JOB-23
async def test_gather_appends_what_arrives_under_one_id_to_one_job_due_settle_after_the_last(
    store: tinystore.Store,
) -> None:
    batch = store.queue("notify-batch", list[dict[str, int]])
    first = await batch.gather("grades:7", [{"course": 1}], settle="5m", max="1h")
    await asyncio.sleep(0.02)
    second = await batch.gather("grades:7", [{"course": 2}], settle="5m", max="1h")
    assert (first.added, first.count, second.added, second.count) == (True, 1, False, 2)
    assert second.at > first.at
    job = await batch.get("grades:7")
    assert job is not None and (job.value, job.at) == ([{"course": 1}, {"course": 2}], second.at)
    one = store.queue("notify-one", dict[str, int])
    await one.add({"course": 1}, id="one")
    with pytest.raises(tinystore.InvalidError):
        await one.gather("one", {"course": 2}, settle="5m")


# contract: JOB-17
async def test_upsert_adds_a_job_or_brings_the_one_there_to_the_call_and_keeps_its_time(store: tinystore.Store) -> None:
    polls = store.queue("polls", dict[str, str])
    assert await polls.upsert("group:1", {"group": "241-327"}, every="3m")
    added = await polls.get("group:1")
    assert not await polls.upsert("group:1", {"group": "241-327"}, every="3m")
    assert await polls.upsert("group:1", {"group": "renamed"}, every="3m")
    renamed = await polls.get("group:1")
    assert added is not None and renamed is not None
    assert (renamed.value, renamed.at) == ({"group": "renamed"}, added.at)
    assert await polls.upsert("group:1", {"group": "renamed"}, every="1h")
    slower = await polls.get("group:1")
    assert slower is not None and (slower.repeat or "").startswith("@every 1h")


# contract: JOB-19
async def test_a_jobs_term_shows_and_a_job_past_it_is_gone_with_its_id_free(store: tinystore.Store) -> None:
    polls = store.queue("terms", dict[str, str])
    before = datetime.now(UTC)
    await polls.upsert("group:1", {"group": "241-327"}, every="3m", ttl="14d")
    job = await polls.get("group:1")
    assert job is not None and job.expires_at is not None
    assert job.expires_at - before >= timedelta(days=14, seconds=-1)
    assert await polls.add({"group": "x"}, id="past", expires_at=datetime.now(UTC) - timedelta(seconds=1))
    assert await polls.get("past") is None
    assert await polls.add({"group": "x"}, id="past")
    with pytest.raises(tinystore.InvalidError):
        await polls.add({"group": "x"}, id="both", ttl="1h", expires_at=datetime.now(UTC))


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

    # contract: JOB-03
    assert await pushes.add("fail", id="fail")  # a failed job holds no id
    again = await pushes.get("fail")
    assert again is not None
    assert (again.state, again.attempt, again.error) == ("waiting", 0, "the provider no longer knows the token")


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
    # contract: JOB-21
    await nightly.run_now()
    after = None
    for _ in range(500):
        after = await nightly.get()
        if after is not None and after.last_run is not None:
            break
        await asyncio.sleep(0.02)
    assert after is not None and (after.state, after.at) == ("scheduled", job.at)  # its later times as they were
    await nightly.stop()
    assert await nightly.cancel()
    with pytest.raises(tinystore.NotFoundError):
        await nightly.run_now()

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


async def test_a_page_with_no_prefix_lists_the_jobs_without_ids_after_those_with(store: tinystore.Store) -> None:
    notifications = store.queue("notifications", int)
    await notifications.add(1)
    await notifications.add(2, id="two")
    assert [job.value async for job in notifications.all()] == [2, 1]
    assert [job.id async for job in notifications.all(state="waiting")] == ["two", None]


# contract: JOB-15
async def test_cancel_takes_a_listed_job_without_an_id_by_its_row_and_no_other(store: tinystore.Store) -> None:
    emails = store.queue("emails-unnamed", dict[str, str])
    for to in ("typo@example.com", "ada@example.com", "grace@example.com"):
        await emails.add({"to": to}, delay="1h")
    listed = [job async for job in emails.all(state="scheduled")]
    typo = next(job for job in listed if job.value["to"] == "typo@example.com")
    assert typo.id is None
    assert await emails.cancel(typo)
    assert not await emails.cancel(typo), "cancelled already"

    ada = next(job for job in listed if job.value["to"] == "ada@example.com")

    async def cancelling(tx: tinystore.Tx) -> None:
        await tx.with_(emails).cancel(ada)

    await store.tx(cancelling)
    assert [job.value["to"] async for job in emails.all()] == ["grace@example.com"]


# contract: JOB-16
async def test_counts_say_how_many_jobs_are_in_each_state(store: tinystore.Store) -> None:
    counted = store.queue("counted", int)
    assert await counted.counts() == tinystore.JobCounts(scheduled=0, waiting=0, running=0, failed=0)
    await counted.add(1, delay="1h")
    await counted.add(2)
    await counted.add(3)
    seen: list[tinystore.JobCounts] = []

    async def count(n: int, run: tinystore.Run) -> None:
        if n == 2:
            seen.append(await counted.counts())

    await counted.run_due(count)
    assert seen == [tinystore.JobCounts(scheduled=1, waiting=1, running=1, failed=0)]
    assert await counted.counts() == tinystore.JobCounts(scheduled=1, waiting=0, running=0, failed=0)


# contract: JOB-12
async def test_a_walk_meets_a_job_whose_value_no_longer_reads_as_its_error_and_goes_on(store: tinystore.Store) -> None:
    loose = store.queue("walked")
    await loose.add({"user_id": 1, "text": "first"}, delay="1h")
    await loose.add({"user_id": "not a number"}, delay="1h")
    await loose.add({"user_id": 3, "text": "last"}, delay="1h")
    checked = store.queue("walked", Reminder)
    with pytest.raises(tinystore.CorruptError):
        await checked.list()

    walk = checked.all()
    texts: list[str] = []
    errors: list[tinystore.CorruptError] = []
    while True:
        try:
            async for reminder in walk:
                texts.append(reminder.value.text)
            break
        except tinystore.CorruptError as err:
            errors.append(err)
    assert texts == ["first", "last"]
    assert len(errors) == 1
    assert "without an id" in str(errors[0])


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


# contract: JOB-18, JOB-20
async def test_a_jobs_own_dedupe_holds_its_id_by_a_local_calendar_and_its_keep_of_zero_keeps_nothing(
    store: tinystore.Store,
) -> None:
    crawl = store.queue("crawl", str, attempts=1)
    day = tinystore.Dedupe(calendar="day", time_zone="Europe/Moscow")
    assert await crawl.add("241-327", id="crawl:241-327", dedupe=day)
    assert await crawl.add("push", id="push:1", keep="0s")

    async def send(value: str, run: tinystore.Run) -> None:
        if value == "push":
            raise RuntimeError("push service 503")

    assert await crawl.run_due(send) == 2
    assert not await crawl.add("241-327", id="crawl:241-327")  # until Moscow's midnight
    assert await crawl.get("push:1") is None  # failed, and kept not at all
    with pytest.raises(tinystore.InvalidError):
        await crawl.add("x", id="x", dedupe=tinystore.Dedupe(calendar="day", time_zone="Mars/Olympus"))


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
async def test_a_group_rate_starts_each_groups_count_a_span_and_holds_back_no_other(store: tinystore.Store) -> None:
    messages = store.queue("messages", group_rate="2/m")
    for bot, n in [("a", 0), ("a", 1), ("a", 2), ("b", 0), ("b", 1)]:
        await messages.add({"bot": bot, "n": n}, group=bot)
    sent: list[str] = []

    async def send(message: dict[str, object], run: tinystore.Run) -> None:
        sent.append(f"{message['bot']}{message['n']}")

    assert await messages.run_due(send) == 4
    assert sent == ["a0", "a1", "b0", "b1"]
    assert await messages.run_due(send) == 0


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
