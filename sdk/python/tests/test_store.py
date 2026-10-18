"""A store opened each way: what its server says it is, the test's clock, and what open refuses."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING

import pytest

import tinystore
from ways import SERVED

if TYPE_CHECKING:
    from pathlib import Path


async def test_a_store_says_what_serves_it(store: tinystore.Store) -> None:
    status = await store.status()
    assert status.protocol == 2
    assert "kv" in status.engines
    assert status.capability == "admin"


@pytest.mark.skipif(not SERVED, reason="tinystore is not built here")
async def test_a_private_store_runs_on_the_tests_clock(directory: Path) -> None:
    start = datetime(2026, 10, 9, 9, tzinfo=UTC)
    async with await tinystore.open(directory, private=True, clock=start) as store:
        assert await store.clock.now() == start
        assert await store.clock.advance("1h") == start + timedelta(hours=1)
        assert await store.clock.advance(timedelta(minutes=30)) == start + timedelta(hours=1, minutes=30)


async def test_a_clock_is_a_private_stores_alone(directory: Path) -> None:
    with pytest.raises(tinystore.InvalidError, match="private"):
        await tinystore.open(directory, clock=datetime(2026, 10, 9, tzinfo=UTC))


async def test_an_address_is_no_directory() -> None:
    with pytest.raises(tinystore.InvalidError, match="connect"):
        await tinystore.open("tcp://127.0.0.1:7070")


async def test_a_durability_is_one_of_two_words(directory: Path) -> None:
    with pytest.raises(tinystore.InvalidError, match="'full' or 'os'"):
        await tinystore.open(directory, durability="fast")  # type: ignore[arg-type]
    assert not (directory / "kv.db").exists()


async def test_a_bare_number_is_no_span(directory: Path) -> None:
    with pytest.raises(tinystore.InvalidError, match="bare number"):
        await tinystore.open(directory, idle=30)  # type: ignore[arg-type]
