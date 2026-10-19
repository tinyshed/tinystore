"""Locks and semaphores as docs/guides/locks/README.md has them,
through each way a store is reached.

A key held by one holder at a time through a block, a line of holds behind
it, a lease this program renews, and one lost at its max_hold cancelling its
block and ending it LostError.
"""

from __future__ import annotations

import asyncio

import pytest

import tinystore


# contract: LOCK-01
async def test_a_key_is_held_by_one_holder_at_a_time_the_next_in_line_given_it_when_it_is_let_go(
    store: tinystore.Store,
) -> None:
    invoices = store.lock("invoices")
    holding, let_go = asyncio.Event(), asyncio.Event()

    async def first() -> int:
        async with invoices.hold("42") as lease:
            holding.set()
            await let_go.wait()
            return lease.fence

    async def second() -> int:
        async with invoices.hold("42") as lease:
            return lease.fence

    held = asyncio.ensure_future(first())
    await holding.wait()
    async with invoices.try_hold("42") as lease:
        assert lease is None
    async with invoices.try_hold("43") as lease:
        assert lease is not None
    waiting = asyncio.ensure_future(second())
    await asyncio.sleep(0.05)
    let_go.set()
    before, after = await asyncio.gather(held, waiting)
    assert isinstance(before, int)
    assert after > before


async def test_a_key_still_held_past_the_wait_is_in_use_and_a_raise_lets_the_key_go(store: tinystore.Store) -> None:
    reports = store.lock("reports")
    holding, let_go = asyncio.Event(), asyncio.Event()

    async def first() -> None:
        async with reports.hold("sales"):
            holding.set()
            await let_go.wait()

    held = asyncio.ensure_future(first())
    await holding.wait()
    with pytest.raises(tinystore.InUseError):
        async with reports.hold("sales", wait="200ms"):
            pass
    let_go.set()
    await held
    with pytest.raises(ValueError, match="the holder failed"):
        async with reports.hold("sales"):
            raise ValueError("the holder failed")
    async with reports.try_hold("sales") as lease:
        assert lease is not None


# contract: LOCK-04
async def test_a_hold_inside_its_own_hold_of_the_key_or_inside_a_transaction_is_invalid(
    store: tinystore.Store,
) -> None:
    invoices = store.lock("nested")
    with pytest.raises(tinystore.InvalidError, match="wait for itself"):
        async with invoices.hold("42"), invoices.hold("42"):
            pass
    async with invoices.hold("42"), invoices.hold("43") as inner:
        assert inner.fence > 0
    stock = store.bucket("stock", int)

    async def place(tx: tinystore.Tx) -> None:
        await tx.with_(stock).set("sku", 1)
        async with invoices.hold("42"):
            pass

    with pytest.raises(tinystore.InvalidError, match="inside a transaction"):
        await store.tx(place)
    assert await stock.get("sku") is None


# contract: LOCK-05
async def test_a_lease_renewed_by_this_program_outlives_its_ttl(store: tinystore.Store) -> None:
    jobs = store.lock("long", ttl="1s")
    holding, let_go = asyncio.Event(), asyncio.Event()

    async def held() -> None:
        async with jobs.hold("nightly"):
            holding.set()
            await let_go.wait()

    holder = asyncio.ensure_future(held())
    await holding.wait()
    await asyncio.sleep(1.6)
    async with jobs.try_hold("nightly") as lease:
        assert lease is None
    let_go.set()
    await holder


# contract: LOCK-03
async def test_max_hold_revokes_a_lease_gives_the_key_on_and_ends_the_block_lost(store: tinystore.Store) -> None:
    exports = store.lock("exports", max_hold="300ms")
    holding = asyncio.Event()
    reached: list[str] = []

    async def hung() -> None:
        async with exports.hold("daily"):
            holding.set()
            await asyncio.sleep(10)
            reached.append("past the sleep")

    held = asyncio.ensure_future(hung())
    await holding.wait()
    async with exports.hold("daily", wait="5s") as lease:
        assert lease.fence > 0
    with pytest.raises(tinystore.LostError):
        await held
    assert reached == []


# contract: LOCK-03
async def test_a_lease_lost_after_the_blocks_last_await_still_ends_it_lost(store: tinystore.Store) -> None:
    exports = store.lock("late", max_hold="100ms")
    with pytest.raises(tinystore.LostError):
        async with exports.hold("daily"):
            await asyncio.sleep(0.3)
    await asyncio.sleep(0)


async def test_a_cancel_of_the_holders_own_goes_on_as_a_cancel(store: tinystore.Store) -> None:
    jobs = store.lock("cancelled")
    holding = asyncio.Event()

    async def held() -> None:
        async with jobs.hold("one"):
            holding.set()
            await asyncio.sleep(10)

    holder = asyncio.ensure_future(held())
    await holding.wait()
    holder.cancel()
    with pytest.raises(asyncio.CancelledError):
        await holder
    async with jobs.hold("one", wait="2s") as lease:
        assert lease.fence > 0


async def test_a_semaphore_admits_its_limit_a_key_its_permit_without_a_fence(store: tinystore.Store) -> None:
    uploads = store.semaphore("uploads", limit=2)
    holding = [asyncio.Event(), asyncio.Event()]
    let_go = asyncio.Event()

    async def holder(each: asyncio.Event) -> bool:
        async with uploads.hold("ann") as permit:
            each.set()
            await let_go.wait()
            return hasattr(permit, "fence")

    holders = [asyncio.ensure_future(holder(each)) for each in holding]
    await asyncio.gather(*(each.wait() for each in holding))
    async with uploads.try_hold("ann") as permit:
        assert permit is None
    async with uploads.try_hold("bob") as permit:
        assert permit is not None
    let_go.set()
    assert await asyncio.gather(*holders) == [False, False]


def test_names_and_limits_are_checked_before_anything_is_sent(store: tinystore.Store) -> None:
    with pytest.raises(tinystore.InvalidError):
        store.lock("Invoices")
    with pytest.raises(tinystore.InvalidError):
        store.semaphore("uploads", limit=0)
