"""once opened with fresh, as docs/guides/kv/once.md has it.

A kept answer answered at once while one new one is made, and kept while the
source is down, through the ways a store runs on a test's clock.
"""

from __future__ import annotations

import asyncio
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING, TypedDict

import pytest

import tinystore
from tinystore.once import Answer
from tinystore.testing import open_temp
from ways import CORE, SERVED

if TYPE_CHECKING:
    from typing import Any

START = datetime(2026, 11, 2, 9, tzinfo=UTC)
CLOCKED = [
    pytest.param({}, marks=pytest.mark.skipif(not CORE, reason="the core in this process is not built"), id="core"),
    pytest.param(
        {"private": True}, marks=pytest.mark.skipif(not SERVED, reason="no tinystore binary"), id="private child"
    ),
]


class GradeBook(TypedDict):
    user: int
    built: int


# contract: KV-16, KV-17, KV-18
@pytest.mark.parametrize("way", CLOCKED)
async def test_first_within_fresh_stale_while_it_builds_and_the_last_good_while_the_source_is_down(
    way: dict[str, Any],
) -> None:
    async with open_temp(clock=START, **way) as store:
        books = store.once("cabinet-books", GradeBook, fresh="10m", keep="12h")
        built = 0
        down = False

        async def cabinet(user: int) -> GradeBook:
            nonlocal built
            if down:
                raise ConnectionError("the cabinet is down")
            built += 1
            return GradeBook(user=user, built=built)

        first = await books.answer("7", lambda: cabinet(7))
        assert first == Answer(GradeBook(user=7, built=1), timedelta(0), False)

        await store.clock.advance("5m")
        assert await books.answer("7", lambda: cabinet(7)) == Answer(
            GradeBook(user=7, built=1), timedelta(minutes=5), False
        )
        assert built == 1

        await store.clock.advance("6m")
        stale = await books.answer("7", lambda: cabinet(7))
        assert stale == Answer(GradeBook(user=7, built=1), timedelta(minutes=11), True)

        for _ in range(500):
            if await books.get("7") == GradeBook(user=7, built=2):
                break
            await asyncio.sleep(0.02)
        assert await books.run("7", lambda: cabinet(7)) == GradeBook(user=7, built=2)

        down = True
        await store.clock.advance("11m")
        assert (await books.answer("7", lambda: cabinet(7))).value == GradeBook(user=7, built=2)
        await store.clock.advance("1m")
        assert (await books.answer("7", lambda: cabinet(7))).stale
        assert await books.get("7") == GradeBook(user=7, built=2)

        await store.clock.advance("12h")
        with pytest.raises(ConnectionError, match="the cabinet is down"):
            await books.run("7", lambda: cabinet(7))
        assert await books.get("7") is None


# contract: KV-19
@pytest.mark.parametrize("way", CLOCKED)
async def test_a_clear_forgets_a_branch_and_the_branches_under_it(way: dict[str, Any]) -> None:
    async with open_temp(clock=START, **way) as store:
        books = store.once("books", int, fresh="1m", keep="1h")
        await books.under("course-1").run("7", lambda: 1)
        await books.under("course-1", "group-2").run("8", lambda: 2)
        await books.under("course-2").run("7", lambda: 3)
        await books.under("course-1").clear()
        assert await books.under("course-1").get("7") is None
        assert await books.under("course-1", "group-2").get("8") is None
        assert await books.under("course-2").get("7") == 3


@pytest.mark.parametrize("way", CLOCKED)
async def test_an_answer_without_fresh_says_its_age_and_a_fresh_as_long_as_keep_is_refused(
    way: dict[str, Any],
) -> None:
    async with open_temp(clock=START, **way) as store:
        charges = store.once("charges", str, keep="1h")
        await charges.run("req-1", lambda: "receipt")
        await store.clock.advance("59m")
        assert await charges.answer("req-1", lambda: "never") == Answer("receipt", timedelta(minutes=59), False)
        with pytest.raises(tinystore.InvalidError, match="kv once books"):
            await store.once("books", str, fresh="1h", keep="1h").run("7", lambda: "x")
