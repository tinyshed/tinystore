"""kv as docs/guides/kv/README.md has it, through each way a store is reached.

Buckets, counters, limits, once and transactions.
"""

from __future__ import annotations

import asyncio
import math
import tempfile
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import TYPE_CHECKING, Any, TypedDict

import pytest

import tinystore
from ways import removed

if TYPE_CHECKING:
    from ways import Way


@dataclass
class Session:
    device: str
    seen_at: int


class Receipt(TypedDict):
    receipt: str


def soon(**span: float) -> datetime:
    return datetime.now(UTC) + timedelta(**span)


async def test_a_value_comes_back_as_it_went_in_under_its_owner(store: tinystore.Store) -> None:
    sessions = store.bucket("sessions", Session)
    await sessions.under(7).set("token", Session("phone", 1))
    assert await sessions.under(7).get("token") == Session("phone", 1)
    assert await sessions.under("7").has("token")
    assert await sessions.get("token") is None
    assert await sessions.under(8).get("token") is None


async def test_a_stale_version_is_a_conflict_naming_its_bucket_and_key(store: tinystore.Store) -> None:
    sessions = store.bucket("sessions", Session)
    await sessions.set("k", Session("a", 1))
    first = await sessions.entry("k")
    assert first is not None
    await sessions.set("k", Session("b", 2), if_version=first.version)
    with pytest.raises(tinystore.ConflictError, match='kv bucket sessions: key "k"'):
        await sessions.set("k", Session("c", 3), if_version=first.version)
    found = await sessions.get("k")
    assert found is not None
    assert found.device == "b"


async def test_a_version_of_none_is_refused_so_a_write_is_never_unchecked_by_accident(
    store: tinystore.Store,
) -> None:
    sessions = store.bucket("sessions", Session)
    missing = await sessions.entry("never-set")
    with pytest.raises(tinystore.InvalidError, match="never None"):
        await sessions.set("never-set", Session("x", 0), if_version=missing.version if missing else None)  # type: ignore[arg-type]
    assert await sessions.create("never-set", Session("x", 0))


async def test_an_expiry_holds_the_key_until_its_time_and_expire_moves_it_either_way(store: tinystore.Store) -> None:
    drafts = store.bucket("drafts", str)
    await drafts.set("d", "hello", ttl="1h")
    entry = await drafts.entry("d")
    assert entry is not None and entry.expires_at is not None
    assert entry.expires_at > soon(minutes=59)
    assert await drafts.expire("d", "2h")
    entry = await drafts.entry("d")
    assert entry is not None and entry.expires_at is not None
    assert entry.expires_at > soon(minutes=119)
    assert await drafts.expire("d", soon(minutes=1))
    entry = await drafts.entry("d")
    assert entry is not None and entry.expires_at is not None
    assert entry.expires_at < soon(seconds=61)
    await drafts.set("gone", "x", expires_at=soon(seconds=-1))
    assert await drafts.get("gone") is None
    assert not await drafts.expire("never-set", timedelta(hours=1))


async def test_create_and_add_write_a_key_only_where_none_is_and_say_so(store: tinystore.Store) -> None:
    seen = store.bucket("stripe-events", ttl="7d")
    assert await seen.add("evt_1")
    assert not await seen.add("evt_1")
    assert await seen.has("evt_1")
    names = store.bucket("names", str)
    assert await names.create("ada", "Lovelace")
    assert not await names.create("ada", "Byron")
    assert await names.get("ada") == "Lovelace"


async def test_take_reads_and_burns_and_delete_says_whether_the_key_was_there(store: tinystore.Store) -> None:
    codes = store.bucket("login-codes", int, ttl="15m")
    await codes.set("123456", 42)
    assert await codes.take("123456") == 42
    assert await codes.take("123456") is None
    await codes.set("654321", 7)
    assert await codes.delete("654321")
    assert not await codes.delete("654321")


async def test_clear_empties_a_branch_and_those_under_it(store: tinystore.Store) -> None:
    carts = store.bucket("carts", int)
    await carts.under("u1").set("a", 1)
    await carts.under("u1", "saved").set("b", 2)
    await carts.under("u2").set("a", 3)
    await carts.under("u1").clear()
    assert await carts.under("u1").get("a") is None
    assert await carts.under("u1", "saved").get("b") is None
    assert await carts.under("u2").get("a") == 3


async def test_list_reads_a_page_in_the_order_of_the_keys_and_all_walks_every_page(store: tinystore.Store) -> None:
    pages = store.bucket("pages", int).under("p")
    for i in range(25):
        await pages.set(f"k{i:02}", i)
    first = await pages.list(limit=10)
    assert [e.key for e in first.entries] == [f"k0{i}" for i in range(10)]
    assert first.next == "k09"
    second = await pages.list(limit=10, after=first.next)
    assert second.entries[0].key == "k10"
    assert [e.value async for e in pages.all(limit=7)] == list(range(25))


async def test_ttl_and_idle_are_one_or_the_other_and_a_span_is_checked_before_anything_leaves(
    store: tinystore.Store,
) -> None:
    with pytest.raises(tinystore.InvalidError):
        store.bucket("both", ttl="1h", idle="1h")
    store.bucket("spelled", idle="2w3d4h5m6s7ms")
    with pytest.raises(tinystore.InvalidError):
        store.bucket("typos", ttl="1hr")
    with pytest.raises(tinystore.InvalidError, match="bare number"):
        store.bucket("typos", ttl=60)  # type: ignore[arg-type]
    with pytest.raises(tinystore.InvalidError):
        store.bucket("Bad Name")


async def test_each_type_keeps_its_values_as_the_core_keeps_a_rust_type(store: tinystore.Store) -> None:
    strings = store.bucket("strings", str)
    await strings.set("s", "héllo \U0001f600")
    assert await strings.get("s") == "héllo \U0001f600"
    raw = store.bucket("bytes", bytes)
    await raw.set("b", bytes([0, 255, 7]))
    assert await raw.get("b") == bytes([0, 255, 7])
    floats = store.bucket("floats", float)
    await floats.set("f", -0.0)
    zero = await floats.get("f")
    assert zero is not None and math.copysign(1, zero) == -1
    await floats.set("inf", math.inf)
    assert await floats.get("inf") == math.inf
    flags = store.bucket("flags", bool)
    await flags.set("on", True)
    assert await flags.get("on") is True
    big = store.bucket("big", int)
    await big.set("n", 2**62)
    assert await big.get("n") == 2**62
    panels = store.bucket("panels", list[Session])
    await panels.set("p", [Session("a", 1), Session("b", 2)])
    assert await panels.get("p") == [Session("a", 1), Session("b", 2)]


async def test_a_value_of_another_type_is_corrupt_and_an_integer_past_int64_is_refused(store: tinystore.Store) -> None:
    await store.bucket("mixed", str).set("k", "text")
    with pytest.raises(tinystore.CorruptError):
        await store.bucket("mixed", int).get("k")
    with pytest.raises(tinystore.InvalidError):
        await store.bucket("wide", int).set("j", 2**64)
    with pytest.raises(tinystore.InvalidError):
        await store.bucket("wide", int).set("j", True)


async def test_a_sets_member_reads_as_none_from_a_bucket_of_json(store: tinystore.Store) -> None:
    tags = store.bucket("tags")
    await tags.add("rust")
    assert await tags.has("rust")
    assert await tags.get("rust") is None


async def test_add_gives_the_new_count_each_branch_apart_and_an_absent_counter_is_0(store: tinystore.Store) -> None:
    attempts = store.counters("login-attempts", ttl="15m")
    assert await attempts.add("10.0.0.1") == 1
    assert await attempts.add("10.0.0.1", 4) == 5
    assert await attempts.under("eu").add("10.0.0.1") == 1
    assert await attempts.get("10.0.0.1") == 5
    assert await attempts.get("10.0.0.2") == 0
    assert await attempts.delete("10.0.0.1")
    assert await attempts.get("10.0.0.1") == 0
    await attempts.clear()
    assert await attempts.under("eu").get("10.0.0.1") == 0


async def test_counters_kept_in_memory_count_all_the_same(store: tinystore.Store) -> None:
    hits = store.counters("page-hits", flush_every="1s")
    counts = await asyncio.gather(*(hits.add("/") for _ in range(20)))
    assert max(counts) == 20
    assert await hits.get("/") == 20


async def test_a_rate_limit_lets_its_burst_through_then_says_when_to_try_again(store: tinystore.Store) -> None:
    api = store.rate_limit("api", rate="2/m")
    tenant = api.under("tenant-7")
    assert await tenant.allow("user-1") == tinystore.Allowance(ok=True, left=1, retry_at=None, windows={})
    assert (await tenant.allow("user-1")).left == 0
    third = await tenant.allow("user-1")
    assert not third.ok
    assert third.retry_at is not None and third.retry_at > soon(seconds=29)
    assert (await api.allow("user-1")).ok
    with pytest.raises(tinystore.InvalidError):
        await tenant.allow("user-2", 3)
    assert not (await tenant.peek("user-1")).ok
    await tenant.reset("user-1")
    assert (await tenant.allow("user-1")).ok


async def test_a_rate_it_cannot_read_is_refused_before_its_call(store: tinystore.Store) -> None:
    for rate, burst in (("fast", None), ("100/5x", None), ("5/10s", 0)):
        with pytest.raises(tinystore.InvalidError):
            store.rate_limit("bad", rate=rate, burst=burst)


async def test_a_quota_counts_in_every_window_or_in_none_and_gives_uses_back(store: tinystore.Store) -> None:
    ai = store.quota("ai", daily="2/1d", weekly="3/7d")
    first = await ai.allow("user-1")
    assert first.ok
    assert (first.windows["daily"].used, first.windows["daily"].limit, first.windows["daily"].left) == (1, 2, 1)
    assert (first.windows["weekly"].used, first.windows["weekly"].left) == (1, 2)
    resets_at = first.windows["daily"].resets_at
    assert resets_at is not None and resets_at > datetime.now(UTC)
    assert (await ai.allow("user-1")).ok
    refused = await ai.allow("user-1")
    assert not refused.ok
    assert isinstance(refused.retry_at, datetime)
    assert (await ai.peek("user-1")).windows["weekly"].used == 2
    await ai.refund("user-1")
    assert (await ai.allow("user-1")).ok
    await ai.reset("user-1")
    assert (await ai.peek("user-1")).windows["daily"].used == 0


async def test_a_run_keeps_its_answer_and_the_next_run_of_the_key_gets_it_without_running(
    store: tinystore.Store,
) -> None:
    charges = store.once("charges", Receipt)
    ran = 0

    async def charge() -> Receipt:
        nonlocal ran
        ran += 1
        return {"receipt": f"r-{ran}"}

    assert await charges.run("order-1", charge) == {"receipt": "r-1"}
    assert await charges.run("order-1", charge) == {"receipt": "r-1"}
    assert ran == 1
    assert await charges.get("order-1") == {"receipt": "r-1"}
    assert await charges.delete("order-1")
    assert await charges.run("order-1", charge) == {"receipt": "r-2"}


async def test_an_error_keeps_nothing_so_the_next_run_runs_again(store: tinystore.Store) -> None:
    charges = store.once("declines", str, keep="1h")

    def timed_out() -> str:
        raise TimeoutError("the card network timed out")

    with pytest.raises(TimeoutError, match="the card network timed out"):
        await charges.run("order-2", timed_out)
    assert await charges.run("order-2", lambda: "charged") == "charged"


async def test_runs_of_one_key_at_once_run_its_function_once_and_each_gets_its_answer(store: tinystore.Store) -> None:
    charges = store.once("at-once", int).under("shop-1")
    ran = 0

    async def slow() -> int:
        nonlocal ran
        ran += 1
        await asyncio.sleep(0.05)
        return 99

    assert await asyncio.gather(charges.run("order-3", slow), charges.run("order-3", slow)) == [99, 99]
    assert ran == 1


async def test_reads_decide_the_writes_which_commit_together(store: tinystore.Store) -> None:
    stock = store.bucket("stock", int)
    orders = store.bucket("orders", str)
    await stock.set("sku-1", 1)

    async def sell(tx: tinystore.Tx) -> bool:
        left = await tx.with_(stock).get("sku-1") or 0
        if left < 1:
            return False
        await tx.with_(stock).set("sku-1", left - 1)
        await tx.with_(orders).set("o-1", "sku-1")
        assert await tx.with_(stock).get("sku-1") == 0
        return True

    assert await store.tx(sell)
    assert await stock.get("sku-1") == 0
    assert await orders.get("o-1") == "sku-1"


async def test_a_key_read_that_changed_before_the_commit_runs_the_function_again(store: tinystore.Store) -> None:
    stock = store.bucket("contended", int)
    await stock.set("sku-2", 10)
    runs = 0

    async def sell(tx: tinystore.Tx) -> None:
        nonlocal runs
        runs += 1
        left = await tx.with_(stock).get("sku-2") or 0
        if runs == 1:
            await stock.set("sku-2", 5)
        await tx.with_(stock).set("sku-2", left - 1)

    await store.tx(sell)
    assert runs == 2
    assert await stock.get("sku-2") == 4


async def test_an_exception_writes_nothing(store: tinystore.Store) -> None:
    notes = store.bucket("notes", str)

    async def draft(tx: tinystore.Tx) -> None:
        await tx.with_(notes).set("n", "draft")
        raise RuntimeError("changed my mind")

    with pytest.raises(RuntimeError, match="changed my mind"):
        await store.tx(draft)
    assert await notes.get("n") is None


async def test_take_create_delete_and_clear_answer_as_the_commit_will_leave_the_keys(store: tinystore.Store) -> None:
    codes = store.bucket("tx-codes", int)
    await codes.set("c1", 1)
    await codes.under("u1").set("c2", 2)
    attempts = store.counters("tx-attempts")

    async def work(tx: tinystore.Tx) -> None:
        assert await tx.with_(codes).take("c1") == 1
        assert await tx.with_(codes).get("c1") is None
        assert await tx.with_(codes).create("c1", 3)
        assert not await tx.with_(codes).create("c1", 4)
        assert not await tx.with_(codes).delete("missing")
        await tx.with_(codes).under("u1").clear()
        assert await tx.with_(codes).under("u1").get("c2") is None
        await tx.with_(attempts).add("ip", 2)

    await store.tx(work)
    assert await codes.get("c1") == 3
    assert await codes.under("u1").get("c2") is None
    assert await attempts.get("ip") == 2


async def test_a_handle_of_another_store_is_refused(store: tinystore.Store, way: Way) -> None:
    other_dir = Path(tempfile.mkdtemp(prefix="tinystore-py-other-"))
    other = await way.open(other_dir)
    try:
        theirs = other.bucket("stock", int)

        async def read(tx: tinystore.Tx) -> Any:
            return await tx.with_(theirs).get("sku-1")

        with pytest.raises(tinystore.InvalidError):
            await store.tx(read)
    finally:
        await other.close()
        await way.leave(other_dir)
        await removed(other_dir)


# contract: SEC-04
@pytest.mark.parametrize("way", ["embedded", "private"])
async def test_an_older_key_opens_what_it_sealed_and_a_reseal_moves_it_to_the_new_one(
    directory: Path, way: str
) -> None:
    from ways import CORE, SERVED

    if not (CORE if way == "embedded" else SERVED):
        pytest.skip(f"no {way} store here")
    old, new = directory / "old.key", directory / "new.key"
    old.write_text("1a" * 32)
    new.write_text("2b" * 32)
    data = directory / "data"

    async def opened(key: Path, older: list[Path] | None = None) -> tinystore.Store:
        embedded = way == "embedded"
        return await tinystore.open(
            data, embedded=embedded, private=not embedded, encryption_key_file=key, older_encryption_keys=older or []
        )

    async with await opened(old) as before:
        await before.bucket("passwords", str, encrypted=True).set("source-42", "hunter2")
    async with await opened(new, [old]) as store:
        passwords = store.bucket("passwords", str, encrypted=True)
        assert await passwords.get("source-42") == "hunter2"
        assert await passwords.reseal() == 1
        assert await passwords.reseal() == 0
    async with await opened(new) as after:
        assert await after.bucket("passwords", str, encrypted=True).get("source-42") == "hunter2"
