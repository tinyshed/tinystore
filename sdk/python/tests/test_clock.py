"""A test's clock: a private store runs on it, and a test moves it forward instead of waiting."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING

import pytest

import tinystore
from tinystore import InvalidError

if TYPE_CHECKING:
    from pathlib import Path


async def test_a_private_store_runs_on_the_clock_it_is_given_which_moves_only_forward(tmp_path: Path) -> None:
    start = datetime(2026, 10, 3, 9, tzinfo=UTC)
    async with tinystore.open(tmp_path / "data", private=True, clock=start) as store:
        assert await store.clock.now() == start

        codes = store.kv.bucket("codes", int)
        await codes.set("K7Q2", 42, ttl="15m")
        reminders = store.jobs.queue("reminders", int)
        await reminders.enqueue(42, after="1h")
        ran: list[int] = []

        async def remind(job: tinystore.Job[int]) -> None:
            ran.append(job.value)

        await reminders.work(remind, until_idle=True)
        assert ran == []  # nothing is due yet

        assert await store.clock.advance("1h") == start + timedelta(hours=1)
        assert await codes.get("K7Q2") is None  # expired 45 minutes ago, on the store's clock
        await reminders.work(remind, until_idle=True)
        assert ran == [42]

        with pytest.raises(InvalidError):
            await store.clock.set(start)
        assert await store.clock.set(start + timedelta(days=1)) == start + timedelta(days=1)


async def test_a_clock_is_a_private_stores_and_one_on_the_systems_time_refuses_to_move(tmp_path: Path) -> None:
    with pytest.raises(InvalidError):
        tinystore.open(tmp_path / "shared", clock=datetime.now(UTC))
    async with tinystore.open(tmp_path / "plain", private=True) as plain:
        with pytest.raises(InvalidError):
            await plain.clock.advance("1h")
