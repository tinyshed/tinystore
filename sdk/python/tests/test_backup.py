"""A backup of the whole store as plan/api/backup.md has it, through each way a store is reached.

One zip of every engine that the server writes while the store keeps
working, which ``tinystore restore`` takes back into an empty directory, and
a file of the program's only when it is named.
"""

from __future__ import annotations

import subprocess
import tempfile
from pathlib import Path
from typing import TYPE_CHECKING

import pytest

import tinystore
from ways import BINARY, SERVED, removed

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable

    from ways import Way

PHOTO = bytes(at % 251 for at in range(3 << 20))
"""Past the 2 MiB a stream takes before its client grants more."""


async def filled(way: Way, directory: Path) -> tinystore.Store:
    directory.mkdir()
    store = await way.open(directory)
    await store.bucket("codes", str).set("K7Q2", "kept across the backup")
    await store.files("photos").put("cat.jpg", PHOTO)
    return store


async def restored(way: Way, zip_: Path, check: Callable[[tinystore.Store, Path], Awaitable[None]]) -> None:
    """Restores a zip with tinystore restore, opens the store, runs check on it, and lets it go."""
    back = Path(tempfile.mkdtemp(prefix="tinystore-restored-"))
    assert BINARY is not None
    run = subprocess.run([BINARY, "restore", str(zip_), str(way.home(back))], capture_output=True, check=False)
    assert run.returncode == 0, run.stdout + run.stderr
    opened = await way.open(back)
    try:
        await check(opened, way.home(back))
    finally:
        await opened.close()
        await way.leave(back)
        await removed(back)


@pytest.mark.skipif(not SERVED, reason="tinystore is not built here")
async def test_a_backup_is_one_zip_of_every_engine_which_restore_takes_back_into_an_empty_directory(
    way: Way, directory: Path
) -> None:
    store = await filled(way, directory / "store")
    try:
        zip_ = directory / "every.zip"
        backup = await store.backup(zip_)
        assert backup.files > 0
        assert backup.bytes > len(PHOTO)
        assert not backup.key_left_out
        assert [each.name for each in directory.iterdir() if each.name.endswith(".part")] == []
    finally:
        await store.close()
        await way.leave(directory / "store")

    async def check(back: tinystore.Store, _: Path) -> None:
        assert await back.bucket("codes", str).get("K7Q2") == "kept across the backup"
        cat = await back.files("photos").get("cat.jpg")
        assert cat is not None
        assert await cat.read() == PHOTO

    await restored(way, zip_, check)


@pytest.mark.skipif(not SERVED, reason="tinystore is not built here")
async def test_a_backup_keeps_a_file_of_the_programs_only_when_include_names_it(way: Way, directory: Path) -> None:
    store = await filled(way, directory / "store")
    try:
        (way.home(directory / "store") / "secret.key").write_text("k3y")
        named, unnamed = directory / "named.zip", directory / "unnamed.zip"
        await store.backup(named, include=["secret.key"])
        await store.backup(unnamed)
    finally:
        await store.close()
        await way.leave(directory / "store")

    async def kept(_: tinystore.Store, home: Path) -> None:
        assert (home / "secret.key").read_text() == "k3y"

    async def left_out(_: tinystore.Store, home: Path) -> None:
        assert not (home / "secret.key").exists()

    await restored(way, named, kept)
    await restored(way, unnamed, left_out)


async def test_a_backup_that_fails_leaves_no_zip(store: tinystore.Store, directory: Path) -> None:
    zips = directory / "zips"
    zips.mkdir()
    with pytest.raises(tinystore.TinystoreError, match=r"missing\.txt"):
        await store.backup(zips / "failed.zip", include=["missing.txt"])
    assert list(zips.iterdir()) == []
