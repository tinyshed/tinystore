"""Channels as docs/guides/channels/README.md has them, through each way a store is reached.

Messages published on a branch and received by whoever listens now, in
order, a loop over them, a transaction's messages handed over at its commit,
a handler's raise told, and a subscriber that falls behind told how many it
missed.
"""

from __future__ import annotations

import asyncio
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING

import pytest

import tinystore
from ways import Way, until

if TYPE_CHECKING:
    from pathlib import Path


@dataclass
class Chat:
    sender: str
    text: str


# contract: CHAN-01
async def test_a_subscriber_receives_what_is_published_from_now_on_in_order_until_it_stops(
    store: tinystore.Store,
) -> None:
    chat = store.channel("chat", Chat)
    await chat.publish(Chat("ann", "before anyone listened"))
    received: list[Chat] = []
    sub = await chat.subscribe(lambda m: received.append(m.value))
    for text in ("a", "b", "c"):
        await chat.publish(Chat("ann", text))
    await until(lambda: len(received) == 3)
    assert [m.text for m in received] == ["a", "b", "c"]
    assert received[0] == Chat("ann", "a")
    await sub.stop()
    await chat.publish(Chat("ann", "after the stop"))
    await asyncio.sleep(0.05)
    assert len(received) == 3


async def test_a_subscription_takes_its_exact_branch_and_recursive_the_branches_below_too(
    store: tinystore.Store,
) -> None:
    orders = store.channel("orders", int)
    exact: list[int] = []
    below: list[tinystore.Message[int]] = []
    subs = [
        await orders.under("eu").subscribe(lambda m: exact.append(m.value)),
        await orders.under("eu").subscribe(below.append, recursive=True),
    ]
    await orders.publish(1)
    await orders.under("eu").publish(2)
    await orders.under("eu", "shop-7").publish(3)
    await until(lambda: len(below) == 2)
    await asyncio.sleep(0.05)
    assert exact == [2]
    assert [m.value for m in below] == [2, 3]
    assert below[1].branch == ["eu", "shop-7"]
    for sub in subs:
        await sub.stop()


async def test_a_loop_receives_the_messages_published_from_its_first_turn_and_break_ends_it(
    store: tinystore.Store,
) -> None:
    ticks = store.channel("ticks", int)
    loop = ticks.each()
    first = asyncio.ensure_future(anext(loop))
    await asyncio.sleep(0.05)
    await ticks.publish(1)
    assert (await first).value == 1
    await ticks.publish(2)
    assert (await anext(loop)).value == 2
    await loop.aclose()  # pyright: ignore[reportAttributeAccessIssue, reportUnknownMemberType]


# contract: CHAN-03
async def test_a_transactions_messages_are_handed_over_once_it_commits_and_never_when_it_rolls_back(
    store: tinystore.Store,
) -> None:
    events = store.channel("events", str)
    received: list[str] = []
    sub = await events.subscribe(lambda m: received.append(m.value), recursive=True)
    orders = store.bucket("orders", str)

    async def refused(tx: tinystore.Tx) -> None:
        await tx.with_(orders).set("1", "order")
        await tx.with_(events).under("1").publish("created 1")
        raise ValueError("the order is refused")

    async def created(tx: tinystore.Tx) -> None:
        await tx.with_(orders).set("2", "order")
        await tx.with_(events).under("2").publish("created 2")

    with pytest.raises(ValueError, match="refused"):
        await store.tx(refused)
    await store.tx(created)
    app = await store.database("app", migrations={"0001_notes.sql": "create table notes (id integer primary key)"})

    async def rolled_back(tx: tinystore.SqlTx) -> None:
        await tx.exec("insert into notes (id) values (1)")
        await tx.with_(events).publish("rolled back")
        raise ValueError("the note is refused")

    async def noted(tx: tinystore.SqlTx) -> None:
        await tx.exec("insert into notes (id) values (2)")
        await tx.with_(events).under("note").publish("noted")

    with pytest.raises(ValueError, match="refused"):
        await app.tx(rolled_back)
    await app.tx(noted)
    await until(lambda: len(received) == 2)
    await asyncio.sleep(0.05)
    assert received == ["created 2", "noted"]
    await sub.stop()


# contract: CHAN-04
async def test_a_handler_that_raises_is_told_and_the_next_message_follows_and_stop_inside_it_answers_at_once(
    store: tinystore.Store,
) -> None:
    numbers = store.channel("numbers", int)
    received: list[int] = []
    failures: list[BaseException] = []
    subs: list[tinystore.Subscription] = []

    async def handler(m: tinystore.Message[int]) -> None:
        if m.value == 2:
            raise ValueError("two is refused")
        received.append(m.value)
        if m.value == 3:
            await subs[0].stop()

    subs.append(await numbers.subscribe(handler, on_error=lambda error, _: failures.append(error)))
    for value in (1, 2, 3, 4):
        await numbers.publish(value)
    await until(lambda: len(received) == 2)
    await asyncio.sleep(0.05)
    assert received == [1, 3]
    assert str(failures[0]) == "two is refused"


# contract: CHAN-02
async def test_a_slow_handler_falls_behind_and_is_told_how_many_it_missed(store: tinystore.Store) -> None:
    photos = store.channel("photos", str)
    photo = "p" * 4096
    released = asyncio.Event()
    counts = {"received": 0, "missed": 0}

    async def slow(_: tinystore.Message[str]) -> None:
        counts["received"] += 1
        await released.wait()

    def missed(told: tinystore.Missed) -> None:
        counts["missed"] += told.count or 0

    sub = await photos.subscribe(slow, on_missed=missed)
    published = 3000
    for _ in range(published):
        await photos.publish(photo)
    released.set()
    await until(lambda: counts["received"] + counts["missed"] == published, within=30)
    assert counts["missed"] > 0
    assert counts["received"] + counts["missed"] == published
    await sub.stop()


def test_names_are_checked_before_anything_is_sent(store: tinystore.Store) -> None:
    with pytest.raises(tinystore.InvalidError):
        store.channel("Chat")


async def test_a_stores_close_ends_its_subscriptions_loops_and_tails_with_nothing_left_to_drain(
    way: Way, directory: Path
) -> None:
    opened = await way.open(directory)
    chat = opened.channel("chat", str)
    live = opened.events("live")
    seen = {"subscribed": 0, "looped": 0, "tailed": 0}

    def subscribed(_: object) -> None:
        seen["subscribed"] += 1

    async def loop() -> None:
        async for _ in chat.each():
            seen["looped"] += 1

    async def tail() -> None:
        async for _ in live.tail():
            seen["tailed"] += 1

    await chat.subscribe(subscribed)
    looping = asyncio.ensure_future(loop())
    tailing = asyncio.ensure_future(tail())
    deadline = time.monotonic() + 10
    while min(seen.values()) == 0 and time.monotonic() < deadline:
        await chat.publish("ping")
        await live.append("tick")
        await asyncio.sleep(0.02)
    assert min(seen.values()) > 0

    # a stream the server still ran would keep a private child, or a sidecar,
    # draining it for ten seconds before it let go
    began = time.monotonic()
    await opened.close()
    await way.leave(directory)
    took = time.monotonic() - began
    with pytest.raises(tinystore.ClosedError):
        await looping
    with pytest.raises(tinystore.ClosedError):
        await tailing
    assert took < 5
