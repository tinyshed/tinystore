"""A backup of the whole store, written by the server serving it, which tinystore restore takes back."""

from __future__ import annotations

import asyncio
import os
from typing import TYPE_CHECKING

import tinystore

if TYPE_CHECKING:
    from pathlib import Path


async def test_a_backup_is_one_zip_of_every_engine_which_restore_takes_back(tmp_path: Path) -> None:
    zipped = tmp_path / "zips" / "backup.zip"
    zipped.parent.mkdir()
    async with tinystore.open(tmp_path / "data", private=True) as store:
        await store.kv.bucket("codes", str).set("K7Q2", "kept across the backup")
        await store.backup(zipped)
    assert zipped.exists()
    assert [p.name for p in zipped.parent.iterdir() if p.name.endswith(".part")] == []

    restored = tmp_path / "restored"
    restore = await asyncio.create_subprocess_exec(os.environ["TINYSTORE_BIN"], "restore", str(zipped), str(restored))
    assert await restore.wait() == 0
    async with tinystore.open(restored, private=True) as back:
        assert await back.kv.bucket("codes", str).get("K7Q2") == "kept across the backup"
