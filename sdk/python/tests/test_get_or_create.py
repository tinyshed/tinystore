"""A set of files' get_or_create, as docs/guides/blobs/get-or-create.md has it, through each way a store is reached.

A miss runs the function once while every other call of the path waits for
it; a put or a failure meanwhile answers them all.
"""

from __future__ import annotations

import asyncio
import contextlib
from typing import TYPE_CHECKING

import pytest

import tinystore
from tinystore import Fill

if TYPE_CHECKING:
    from collections.abc import AsyncIterator
    from pathlib import Path


def pattern(n: int, seed: int = 7) -> bytes:
    return bytes((i * 31 + seed) & 0xFF for i in range(n))


class Held:
    """A function that counts its runs and waits for go before it gives its bytes."""

    def __init__(self, data: bytes) -> None:
        self.data = data
        self.runs = 0
        self.started = asyncio.Event()
        self.go = asyncio.Event()

    async def __call__(self) -> Fill:
        self.runs += 1
        self.started.set()
        await self.go.wait()
        return Fill(self.data, content_type="application/pdf")


async def test_a_miss_runs_the_function_once_and_the_next_call_reads_its_file(store: tinystore.Store) -> None:
    pages = store.files("pages")
    runs = 0

    def convert() -> Fill:
        nonlocal runs
        runs += 1
        return Fill(b"%PDF-1.7", content_type="application/pdf", meta={"doc": "42"})

    page = await pages.get_or_create("7f3a.pdf", convert, content_type="text/plain")
    assert (page.content_type, page.meta) == ("application/pdf", {"doc": "42"})
    assert await page.read() == b"%PDF-1.7"
    again = await pages.get_or_create("7f3a.pdf", convert)
    assert again.etag == page.etag and await again.read() == b"%PDF-1.7"
    assert runs == 1, "the second call read the file"

    plain = await pages.get_or_create("plain.txt", lambda: Fill("text"), content_type="text/plain")
    assert plain.content_type == "text/plain", "the call's type when the Fill gives none"
    with pytest.raises(tinystore.InvalidError):
        Fill(b"both", path="both.bin")


# contract: BLOB-21
async def test_concurrent_calls_of_one_path_run_the_function_once_and_all_get_its_file(store: tinystore.Store) -> None:
    pages = store.files("pages")
    held = Held(b"made once")
    calls = [asyncio.create_task(pages.get_or_create("a.pdf", held)) for _ in range(6)]
    await held.started.wait()
    await asyncio.sleep(0.2)
    held.go.set()
    files = await asyncio.gather(*calls)
    assert held.runs == 1
    assert [await file.read() for file in files] == [b"made once"] * 6


# contract: BLOB-22
async def test_a_failure_is_the_caller_s_own_and_every_waiting_call_s_and_the_next_call_runs_again(
    store: tinystore.Store,
) -> None:
    pages = store.files("pages")
    started, go = asyncio.Event(), asyncio.Event()

    async def failing() -> Fill:
        started.set()
        await go.wait()
        raise ValueError("the document does not parse")

    run = asyncio.create_task(pages.get_or_create("bad.pdf", failing))
    await started.wait()
    waiting = asyncio.create_task(pages.get_or_create("bad.pdf", lambda: Fill(b"never")))
    await asyncio.sleep(0.2)
    go.set()
    with pytest.raises(ValueError, match="does not parse"):
        await run
    with pytest.raises(tinystore.ConflictError, match="the document does not parse"):
        await waiting
    again = await pages.get_or_create("bad.pdf", lambda: Fill(b"parsed"))
    assert await again.read() == b"parsed", "the next call ran it again"


# contract: BLOB-22
async def test_a_put_during_a_run_wins_and_the_run_s_call_gets_the_put_file(store: tinystore.Store) -> None:
    pages = store.files("pages")
    held = Held(b"made")
    run = asyncio.create_task(pages.get_or_create("a.pdf", held))
    await held.started.wait()
    await pages.put("a.pdf", b"put")
    held.go.set()
    page = await run
    assert await page.read() == b"put", "the run's result was dropped"
    stored = await pages.get("a.pdf")
    assert stored is not None and await stored.read() == b"put"


async def test_a_stream_and_a_file_past_one_message_land_and_read_back(store: tinystore.Store, tmp_path: Path) -> None:
    pages = store.files("pages")
    data = pattern(300_000) + pattern(40_000, 3)

    async def streamed() -> AsyncIterator[bytes]:
        yield data[:300_000]
        yield data[300_000:]

    from_stream = await pages.get_or_create("stream.bin", lambda: Fill(streamed()))
    assert from_stream.size == len(data) and await from_stream.read() == data
    source = tmp_path / "source.bin"
    source.write_bytes(data)
    from_file = await pages.get_or_create("file.bin", lambda: Fill(path=source, content_type="application/pdf"))
    assert (from_file.size, from_file.content_type) == (len(data), "application/pdf")
    assert from_file.etag == from_stream.etag and await from_file.read() == data


async def test_a_cancelled_waiting_call_stops_while_the_run_goes_on(store: tinystore.Store) -> None:
    pages = store.files("pages")
    held = Held(b"made")
    run = asyncio.create_task(pages.get_or_create("a.pdf", held))
    await held.started.wait()
    waiting = asyncio.create_task(pages.get_or_create("a.pdf", lambda: Fill(b"never")))
    await asyncio.sleep(0.1)
    waiting.cancel()
    with contextlib.suppress(asyncio.CancelledError):
        await waiting
    assert waiting.cancelled()
    held.go.set()
    page = await run
    assert await page.read() == b"made" and held.runs == 1
