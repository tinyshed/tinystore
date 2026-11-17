"""A set of files as a cache, as docs/guides/blobs/caching.md has it, through each way a store is reached.

A ceiling kept in the set that evicts or refuses, its change, a trim, and a
term from a file's last use.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import pytest

import tinystore


# contract: BLOB-18, BLOB-20
async def test_a_write_past_max_size_evicts_and_the_ceiling_is_the_sets(store: tinystore.Store) -> None:
    pages = store.files("pages", max_size=300)
    for path in ("a", "b", "c", "d"):
        await pages.put(path, bytes(100))
    assert await pages.usage() == tinystore.Usage(count=3, size=300, max_size=300, evictions=1)
    assert [file.path for file in (await pages.list()).files] == ["b", "c", "d"]
    with pytest.raises(tinystore.ConflictError):
        await store.files("pages", max_size="1KiB").head("b")
    assert (await store.files("pages").usage()).max_size == 300
    with pytest.raises(tinystore.LimitError):
        await pages.put("big", bytes(301))


# contract: BLOB-20, BLOB-24
async def test_set_max_size_changes_the_ceiling_when_if_current_holds_and_trim_frees_room(
    store: tinystore.Store,
) -> None:
    thumbs = store.files("thumbs", max_size="1KiB")
    for path in ("1", "2", "3"):
        await thumbs.put(path, bytes(100))
    with pytest.raises(tinystore.ConflictError):
        await thumbs.set_max_size(200, if_current="2KiB")
    await thumbs.set_max_size(200, if_current="1KiB")
    assert await thumbs.usage() == tinystore.Usage(count=2, size=200, max_size=200, evictions=1)
    assert await thumbs.trim(to=100) == tinystore.Trim(evictions=1, freed=100)
    assert (await thumbs.usage()).max_size == 200
    await thumbs.set_max_size("none")
    assert (await thumbs.usage()).max_size is None


# contract: BLOB-23
async def test_when_full_refuse_fails_a_write_past_the_ceiling_and_removes_nothing(store: tinystore.Store) -> None:
    uploads = store.files("uploads", max_size=150, when_full="refuse")
    await uploads.put("a", bytes(100))
    with pytest.raises(tinystore.LimitError):
        await uploads.put("b", bytes(100))
    await uploads.put("a", bytes(120))
    assert await uploads.usage() == tinystore.Usage(count=1, size=120, max_size=150, evictions=0)
    with pytest.raises(tinystore.InvalidError):
        await uploads.trim(to=0)


# contract: BLOB-17
async def test_idle_gives_a_file_its_term_from_its_last_use_never_beside_ttl(store: tinystore.Store) -> None:
    sessions = store.files("sessions", idle="30d")
    put = await sessions.put("a.json", "{}")
    assert put.expires is not None
    assert abs(put.expires - (datetime.now(UTC) + timedelta(days=30))) < timedelta(minutes=1)
    with pytest.raises(tinystore.InvalidError):
        store.files("x", ttl="1h", idle="1h")
