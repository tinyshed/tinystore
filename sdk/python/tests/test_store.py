"""A store opened each way: what its server says it is, the test's clock, and what open refuses."""

from __future__ import annotations

import sys
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING

import pytest

import tinystore
from tinystore._connection import start_failure
from ways import CORE, SERVED, WAYS

LOCAL = [way for way in WAYS if way.name != "a remote server over TCP" and way.ready]
"""The ways a store opens from its directory: every one but a remote server's."""

if TYPE_CHECKING:
    from pathlib import Path


async def test_a_store_says_what_serves_it(store: tinystore.Store) -> None:
    status = await store.status()
    assert status.protocol == 1
    assert "kv" in status.engines
    assert status.capability == "admin"


@pytest.mark.skipif(not SERVED, reason="tinystore is not built here")
async def test_a_private_store_runs_on_the_tests_clock(directory: Path) -> None:
    start = datetime(2026, 10, 9, 9, tzinfo=UTC)
    async with await tinystore.open(directory, private=True, clock=start) as store:
        assert await store.clock.now() == start
        assert await store.clock.advance("1h") == start + timedelta(hours=1)
        assert await store.clock.advance(timedelta(minutes=30)) == start + timedelta(hours=1, minutes=30)


async def test_a_clock_is_a_private_stores_alone(directory: Path) -> None:
    with pytest.raises(tinystore.InvalidError, match="private"):
        await tinystore.open(directory, clock=datetime(2026, 10, 9, tzinfo=UTC))


async def test_an_address_is_no_directory() -> None:
    with pytest.raises(tinystore.InvalidError, match="connect"):
        await tinystore.open("tcp://127.0.0.1:7070")


async def test_a_durability_is_one_of_two_words(directory: Path) -> None:
    with pytest.raises(tinystore.InvalidError, match="'full' or 'os'"):
        await tinystore.open(directory, durability="fast")  # type: ignore[arg-type]
    assert not (directory / "kv.db").exists()


async def test_a_bare_number_is_no_span(directory: Path) -> None:
    with pytest.raises(tinystore.InvalidError, match="bare number"):
        await tinystore.open(directory, idle=30)  # type: ignore[arg-type]


# contract: ERR-03, WIRE-01
async def test_a_directory_that_is_a_file_is_invalid_naming_the_file_in_every_way(directory: Path) -> None:
    file = directory / "data"
    file.write_text("not a directory")
    under = file / "store"
    for way in LOCAL:
        with pytest.raises(tinystore.InvalidError) as itself:
            await way.open(file)
        assert str(itself.value) == f"store {file} is a file, not a directory", way.name
        with pytest.raises(tinystore.InvalidError) as below:
            await way.open(under)
        assert str(below.value) == f"store {under}: {file} is a file, not a directory", way.name


# contract: ERR-02
@pytest.mark.skipif(not SERVED, reason="tinystore is not built here")
async def test_a_binary_that_is_not_there_is_named_for_a_sidecar_and_a_private_child(directory: Path) -> None:
    missing = str(directory / ("nope.exe" if sys.platform == "win32" else "nope"))
    with pytest.raises(tinystore.ClosedError) as sidecar:
        await tinystore.open(directory / "store", binary=missing)
    assert str(sidecar.value) == f"cannot start the sidecar {missing}: not found"
    with pytest.raises(tinystore.ClosedError) as child:
        await tinystore.open(directory / "store", private=True, binary=missing)
    assert str(child.value) == f"cannot start the private server {missing}: not found"


# contract: ERR-02
async def test_a_sidecar_that_fails_before_it_serves_says_what_it_wrote_to_stderr(directory: Path) -> None:
    (directory / "server").mkdir()
    (directory / "server" / "serve.log").write_text('{"level":"INFO","msg":"stopped","took":"0.1s"}\n')
    # this interpreter stands in for tinystore: given serve --dir ..., it fails,
    # says why on stderr and writes no log
    with pytest.raises(tinystore.ClosedError) as failed:
        await tinystore.open(directory, binary=sys.executable)
    message = str(failed.value)
    assert message.startswith("the sidecar exited with "), message
    assert "stopped" not in message
    assert "it wrote no log" not in message


# contract: ERR-02
async def test_a_private_child_that_exits_before_it_answers_says_what_it_wrote_once(directory: Path) -> None:
    # this interpreter stands in for tinystore: given serve --dir ..., it fails and says why on stderr
    with pytest.raises(tinystore.ClosedError) as failed:
        await tinystore.open(directory, private=True, binary=sys.executable)
    message = str(failed.value)
    exited = "the private server did not start: the private server exited with "
    assert message.startswith(exited), message
    said = message.removeprefix(exited).split(": ", 1)[1]
    assert said, message
    assert message.count(said) == 1, message


# contract: ERR-02
def test_a_sidecar_that_wrote_nothing_to_stderr_is_told_by_the_lines_its_log_gained_or_by_none(
    directory: Path,
) -> None:
    log = directory / "serve.log"
    older = b'{"level":"INFO","msg":"stopped"}\n'
    failing = '{"level":"ERROR","msg":"no space left"}'
    log.write_bytes(older + failing.encode() + b"\n")
    assert start_failure(log, len(older), "") == failing
    assert start_failure(log, 0, "it fails\n") == "it fails"
    log.write_bytes(older)
    assert start_failure(log, len(older), "") == "it wrote no log"
    # a log shorter than it was is another file, every line of it new
    log.write_bytes(failing.encode() + b"\n")
    assert start_failure(log, len(older) * 2, "") == failing
    log.unlink()
    assert start_failure(log, 0, "") == "it wrote no log"


# contract: ERR-01
@pytest.mark.skipif(not CORE, reason="the core is not built here")
async def test_a_disk_that_refuses_an_embedded_store_is_the_wires_internal_error(directory: Path) -> None:
    # a name longer than any file system takes
    too_long = directory / ("a" * 300)
    with pytest.raises(tinystore.InternalError) as refused:
        await tinystore.open(too_long, embedded=True)
    assert str(refused.value).startswith(f"cannot open {too_long}: store "), str(refused.value)
