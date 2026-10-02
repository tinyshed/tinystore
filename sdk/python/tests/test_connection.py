"""What a connection tells its program of the sidecar it found."""

from __future__ import annotations

import pytest

from tinystore._connection import older_sidecar


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
def test_a_sidecar_of_an_older_release_than_its_sdk_is_told_of_and_only_one(server: str, own: str, older: bool) -> None:
    told = older_sidecar(server, own, "./data")
    assert (told is not None) == older
    if told is not None:
        assert f"is {server}, older than this SDK's {own}" in told
        assert "tinystore stop ./data" in told
