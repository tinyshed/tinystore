"""Directories as docs/guides/blobs/directories.md has them, through each way a store is reached.

A scan, its changes from a cursor, the list's answers, a file read from the
disk held to its version, a growing file read past the size its get gave,
and a token that names no path.
"""

from __future__ import annotations

import os
import shutil
from typing import TYPE_CHECKING

import pytest

import tinystore

if TYPE_CHECKING:
    from pathlib import Path

    from ways import Way


def movies(store: tinystore.Store, way: Way, folder: Path, *, growing: bool = False) -> tinystore.Directory:
    """The directory movies on the test's folder: by its path, or over a token the server's own."""
    if way.token:
        return store.directory("movies", growing=growing)
    return store.directory("movies", path=str(folder), growing=growing)


def write(folder: Path, path: str, data: bytes, seconds: int) -> None:
    file = folder / path
    file.parent.mkdir(parents=True, exist_ok=True)
    file.write_bytes(data)
    os.utime(file, (seconds, seconds))


async def test_a_scan_lists_the_folder_its_changes_follow_a_cursor_and_a_read_comes_from_the_disk(
    store: tinystore.Store, way: Way, directory: Path
) -> None:
    folder = directory / "movies"
    folder.mkdir(exist_ok=True)
    write(folder, "Dune (2021)/Dune.mkv", b"spice must flow", 1_700_000_000)
    write(folder, "Heat.mkv", b"heat", 1_700_000_000)
    films = movies(store, way, folder)
    assert await films.usage() == tinystore.Usage(0, 0)
    assert await films.scan() == tinystore.Scan(added=2, changed=0, removed=0, skipped=0, count=2, size=19)

    first = await films.changes()
    assert [(item.path, item.change) for item in first.items] == [
        ("Dune (2021)/Dune.mkv", "added"),
        ("Heat.mkv", "added"),
    ]
    film = await films.get("Dune (2021)/Dune.mkv")
    assert film is not None and film.etag.startswith('"d-f-')
    assert film.content_type == "application/octet-stream"
    assert await film.read() == b"spice must flow"
    assert await film.slice(6, 10).read() == b"must"
    assert [file.path for file in (await films.folder("Dune (2021)").list()).files] == ["Dune.mkv"]
    head = await films.head("Heat.mkv")
    assert head is not None and head.size == 4

    # a file past one message is read from the disk as it is read, held to its get's version
    write(folder, "Big.mkv", b"x" * (3 << 20), 1_700_000_000)
    await films.scan()
    big = await films.get("Big.mkv")
    assert big is not None
    write(folder, "Big.mkv", b"y" * (3 << 20), 1_700_000_200)
    with pytest.raises(tinystore.ConflictError):
        await big.read()
    (folder / "Big.mkv").unlink()
    await films.scan()
    before = await films.changes(after=first.next)

    (folder / "Heat.mkv").unlink()
    write(folder, "Dune (2021)/Dune.mkv", b"spice must flow, longer", 1_700_000_100)
    scan = await films.scan()
    assert (scan.changed, scan.removed, scan.count) == (1, 1, 1)
    since = await films.changes(after=before.next)
    assert [(item.path, item.change, item.info is None) for item in since.items] == [
        ("Dune (2021)/Dune.mkv", "changed", False),
        ("Heat.mkv", "removed", True),
    ]
    assert await films.changes(after=since.next) == tinystore.Changes([], since.next)
    assert await films.get("nothing.mkv") is None

    await films.forget()
    assert await films.usage() == tinystore.Usage(0, 0)
    shutil.rmtree(folder / "Dune (2021)")


async def test_a_growing_directorys_read_goes_past_the_size_its_get_gave(
    store: tinystore.Store, way: Way, directory: Path
) -> None:
    folder = directory / "movies"
    folder.mkdir(exist_ok=True)
    write(folder, "rec.ts", b"0123", 1_700_000_000)
    live = movies(store, way, folder, growing=True)
    await live.scan()
    recording = await live.get("rec.ts")
    assert recording is not None and recording.size == 4
    with (folder / "rec.ts").open("ab") as file:
        file.write(b"4567")
    assert await recording.read() == b"01234567"


async def test_a_connection_that_came_with_a_token_names_no_path(
    store: tinystore.Store, way: Way, directory: Path
) -> None:
    if not way.token:
        pytest.skip("a connection of this machine names a path")
    with pytest.raises(tinystore.InvalidError):
        await store.directory("elsewhere", path=str(directory)).usage()
    with pytest.raises(tinystore.InvalidError):
        await store.directory("photos").usage()
