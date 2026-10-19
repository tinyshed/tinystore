"""Files as docs/guides/blobs/README.md has them, through each way a store is reached.

Writes in one call and in pieces, whole reads checked, folders, conditions,
copies, expiry, and uploads that appear at their commit and leave nothing
otherwise.
"""

from __future__ import annotations

import hashlib
import io
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING

import pytest

import tinystore

if TYPE_CHECKING:
    from collections.abc import AsyncIterator
    from pathlib import Path

    from ways import Way


def pattern(n: int, seed: int = 7) -> bytes:
    return bytes((i * 31 + seed) & 0xFF for i in range(n))


def about(expires: datetime | None, term: timedelta) -> bool:
    """A term's end as the store set it, within a minute of what the test expects."""
    return expires is not None and abs(expires - (datetime.now(UTC) + term)) < timedelta(minutes=1)


async def test_a_file_comes_back_byte_for_byte_empty_inline_in_one_call_and_in_pieces(store: tinystore.Store) -> None:
    sizes = store.files("sizes")
    for size in (0, 1, 16 << 10, 70_000, (3 << 20) + 17):
        data = pattern(size)
        put = await sizes.put(f"f{size}", data)
        assert put.size == size
        assert put.etag == f'"{hashlib.sha256(data).hexdigest()}"'
        assert put.content_type == "application/octet-stream"
        assert await sizes.head(f"f{size}") == put
        file = await sizes.get(f"f{size}")
        assert file is not None and file.etag == put.etag
        assert await file.read() == data
    assert await sizes.get("nothing") is None
    assert await sizes.head("nothing") is None


async def test_a_stream_of_unknown_length_goes_in_pieces_and_a_file_object_too(store: tinystore.Store) -> None:
    bodies = store.files("bodies")
    chunks = [pattern(100_000), pattern(3), pattern(3 << 20, 11)]

    async def iterated() -> AsyncIterator[bytes]:
        for chunk in chunks:
            yield chunk

    put = await bodies.put("iterated.bin", iterated(), content_type="application/x-test")
    assert put.size == len(b"".join(chunks))
    file = await bodies.get("iterated.bin")
    assert file is not None and file.content_type == "application/x-test"
    assert [piece async for piece in file] != []
    assert await file.read() == b"".join(chunks)
    await bodies.put("from-file.bin", io.BytesIO(b"".join(chunks)))
    copied = await bodies.get("from-file.bin")
    assert copied is not None and await copied.read() == b"".join(chunks)
    await bodies.put("data.json", '{"a":1}', meta={"name": "Data.json"})
    data = await bodies.get("data.json")
    assert data is not None and data.meta == {"name": "Data.json"}
    assert await data.json() == {"a": 1}
    assert await data.text() == '{"a":1}'


async def test_a_small_file_keeps_its_bytes_and_a_large_one_read_after_a_replace_is_a_conflict(
    store: tinystore.Store,
) -> None:
    kept = store.files("kept-bytes")
    await kept.put("small.txt", "first")
    small = await kept.get("small.txt")
    await kept.put("small.txt", "second")
    await kept.delete("small.txt")
    assert small is not None and await small.text() == "first"
    large = pattern(3 << 20)
    await kept.put("large.bin", large)
    file = await kept.get("large.bin")
    assert file is not None
    assert await file.read() == large
    assert await file.read() == large
    await kept.put("large.bin", pattern(5))
    with pytest.raises(tinystore.ConflictError):
        await file.read()
    await kept.delete("large.bin")
    with pytest.raises(tinystore.ConflictError):
        await file.slice(0, 10).read()


async def test_slice_reads_a_range_as_python_slices(store: tinystore.Store) -> None:
    videos = store.files("videos")
    film = pattern(5 << 20, 13)
    await videos.put("film.mp4", film)
    file = await videos.get("film.mp4")
    assert file is not None
    assert await file.slice(1_000_000, 4_000_000).read() == film[1_000_000:4_000_000]
    assert await file.slice(-500).read() == film[-500:]
    assert file.slice(10, 5).size == 0
    assert await file.slice(len(film) + 10).read() == b""
    await videos.put("note.txt", "hello world")
    note = await videos.get("note.txt")
    assert note is not None
    assert await note.slice(6).text() == "world"
    assert await note.slice(-5, -1).text() == "worl"


async def test_a_folder_is_whole_segments_listed_counted_and_cleared(store: tinystore.Store) -> None:
    media = store.files("media")
    await media.folder("users", 42).put("photos/1.jpg", "one")
    one = await media.get("users/42/photos/1.jpg")
    assert one is not None and await one.text() == "one"
    await media.put("users/42/photos/10.jpg", "ten")
    await media.put("users/420/photos/1.jpg", "other")
    user = media.folder("users", 42)
    page = await user.list(prefix="photos/1")
    assert [file.path for file in page.files] == ["photos/1.jpg", "photos/10.jpg"]
    assert page.next is None
    first = await user.list(limit=1)
    assert [file.path for file in first.files] == ["photos/1.jpg"]
    rest = await user.list(after=first.next)
    assert [file.path for file in rest.files] == ["photos/10.jpg"]
    assert [file.path async for file in media.all()] == [
        "users/42/photos/1.jpg",
        "users/42/photos/10.jpg",
        "users/420/photos/1.jpg",
    ]
    assert await user.usage() == tinystore.Usage(2, 6)
    await user.clear()
    assert await user.usage() == tinystore.Usage(0, 0)
    assert await media.head("users/420/photos/1.jpg") is not None
    for wrong in (
        media.folder("users", "").put("a", "x"),
        media.folder("users/42").put("a", "x"),
        media.put("../a", "x"),
    ):
        with pytest.raises(tinystore.InvalidError):
            await wrong


async def test_if_match_and_create_write_only_as_they_say(store: tinystore.Store) -> None:
    docs = store.files("conditions")
    first = await docs.put("notes.md", "v1")
    await docs.put("notes.md", "v2", if_match=first.etag)
    with pytest.raises(tinystore.ConflictError):
        await docs.put("notes.md", "v3", if_match=first.etag)
    notes = await docs.get("notes.md")
    assert notes is not None and await notes.text() == "v2"
    assert await docs.create("new.md", "a")
    assert not await docs.create("new.md", "b")


async def test_a_copy_shares_the_bytes_and_a_copy_or_a_rename_replaces_a_file_only_by_its_etag(
    store: tinystore.Store,
) -> None:
    docs = store.files("moves")
    source = await docs.put("a.md", "alpha")
    copy = await docs.copy("a.md", "b.md")
    assert (copy.path, copy.etag) == ("b.md", source.etag)
    with pytest.raises(tinystore.ConflictError):
        await docs.copy("a.md", "b.md")
    target = await docs.put("c.md", "gamma")
    renamed = await docs.rename("a.md", "c.md", if_match=target.etag)
    assert renamed.etag == source.etag
    assert await docs.head("a.md") is None
    with pytest.raises(tinystore.NotFoundError):
        await docs.rename("a.md", "d.md")
    await docs.delete("b.md")
    await docs.delete("b.md")
    assert await docs.head("b.md") is None


async def test_a_file_expires_by_its_own_term_or_its_files(store: tinystore.Store) -> None:
    reports = store.files("reports", ttl="24h")
    assert about((await reports.put("a.csv", "a")).expires, timedelta(hours=24))
    assert about((await reports.put("b.csv", "b", ttl="7d")).expires, timedelta(days=7))
    assert await reports.expire("a.csv", "1h")
    head = await reports.head("a.csv")
    assert head is not None and about(head.expires, timedelta(hours=1))
    assert not await reports.expire("none.csv", "1h")
    assert (await store.files("kept").put("a", "a")).expires is None


async def test_an_upload_appears_at_its_commit_and_one_left_without_it_leaves_nothing(store: tinystore.Store) -> None:
    uploads = store.files("uploads")
    line = "x" * 100_000 + "\n"
    async with uploads.upload("2026-10/report.csv", content_type="text/csv") as upload:
        for _ in range(40):
            await upload.write(line)
        assert await uploads.head("2026-10/report.csv") is None
        info = await upload.commit()
        assert (info.size, info.content_type) == (40 * len(line), "text/csv")
    head = await uploads.head("2026-10/report.csv")
    assert head is not None and head.size == 40 * len(line)
    async with uploads.upload("left.bin") as upload:
        await upload.write(pattern(3 << 20))
    assert await uploads.head("left.bin") is None
    with pytest.raises(tinystore.InvalidError):
        async with uploads.upload("sized.bin", size=10) as upload:
            await upload.write(pattern(11))
            await upload.commit()
    assert await uploads.head("sized.bin") is None


async def test_a_file_past_max_file_size_is_refused_and_leaves_nothing(store: tinystore.Store) -> None:
    small = store.files("small", max_file_size="64KiB")
    with pytest.raises(tinystore.LimitError):
        await small.put("big", pattern(65_537))

    async def growing() -> AsyncIterator[bytes]:
        yield pattern(40_000)
        yield pattern(40_000)

    with pytest.raises(tinystore.LimitError):
        await small.put("big", growing())
    assert await small.head("big") is None
    with pytest.raises(tinystore.InvalidError):
        store.files("wrong", max_file_size="2GB")


async def test_a_whole_read_of_a_changed_byte_fails_corrupt_before_its_end(
    store: tinystore.Store, way: Way, directory: Path
) -> None:
    checked = store.files("checked")
    data = pattern((2 << 20) + 5, 3)
    await checked.put("a.bin", data)
    root = directory / "store" if (directory / "store" / "blobs").exists() else directory
    stored = [
        path for path in (root / "blobs" / "objects").rglob("*") if path.is_file() and path.stat().st_size == len(data)
    ]
    assert len(stored) == 1
    changed = bytearray(stored[0].read_bytes())
    changed[4096] ^= 0x5A
    stored[0].write_bytes(changed)
    file = await checked.get("a.bin")
    assert file is not None
    read: list[bytes] = []
    with pytest.raises(tinystore.CorruptError):
        async for piece in file:
            read.append(piece)
    assert len(b"".join(read)) < len(data)
    again = await checked.get("a.bin")
    assert again is not None and len(await again.slice(0, 8192).read()) == 8192
