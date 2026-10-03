"""What a connection does with the sidecar it found: one of an older release is replaced, another server told of."""

from __future__ import annotations

import json
import os
from typing import TYPE_CHECKING

import pytest

import tinystore
from tinystore._connection import Link, older_release, older_server, replaced_sidecar, sidecar

if TYPE_CHECKING:
    from pathlib import Path


@pytest.mark.parametrize(
    ("server", "own", "older"),
    [
        ("v0.1.0", "0.2.0", True),
        ("v0.2.0-rc.1", "0.2.0", True),
        ("v0.2.0-rc.2", "0.2.0rc10", True),
        ("v0.2.0-beta.3", "0.2.0rc1", True),
        ("v0.2.0", "0.2.0", False),
        ("v0.3.0", "0.2.0", False),
        ("v0.2.0", "0.2.0rc1", False),
        ("(devel)", "0.2.0", False),
        ("v0.0.0-20261002222701-fc080a056d05+dirty", "0.2.0", False),
        ("v0.1.1-0.20261002222701-fc080a056d05", "0.2.0", False),
        ("v0.1.0", "0.0.0", False),
    ],
)
def test_a_server_of_an_older_release_than_its_sdk_is_told_apart_and_only_one(
    server: str, own: str, older: bool
) -> None:
    assert older_release(server, own) == older


def test_what_a_program_is_told_names_both_releases() -> None:
    assert "the sidecar serving ./data was v0.1.0, older than" in replaced_sidecar("v0.1.0", "0.2.0", "./data")
    assert "./data is served by v0.1.0, older than this SDK's 0.2.0" in older_server("v0.1.0", "0.2.0", "./data")


async def test_a_sidecar_of_an_older_release_is_stopped_and_every_client_moves_to_the_one_started_in_its_place(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    directory = tmp_path / "shared"
    serve = directory / "server" / "SERVE"
    async with tinystore.open(directory, idle=1) as first:
        notes = first.kv.bucket("notes", str)
        await notes.set("a", "kept across the replacement")
        old = json.loads(serve.read_text())
        assert old["sidecar"] is True

        binary = os.environ["TINYSTORE_BIN"]
        replaced = await Link(sidecar(directory, lambda: binary, 1, older=lambda _: True)).connection()
        now = json.loads(serve.read_text())
        assert now["instance"] != old["instance"]
        assert now["pid"] != old["pid"]
        assert f"the sidecar serving {directory} was" in capsys.readouterr().err

        # the first client was told GOAWAY, and reaches the new sidecar as it connects again
        assert await notes.get("a") == "kept across the replacement"
        await replaced.close()
