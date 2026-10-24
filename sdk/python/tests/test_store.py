"""A store opened each way: what its server says it is, the test's clock, what open refuses, and an open given up on."""

from __future__ import annotations

import asyncio
import base64
import gc
import json
import secrets
import subprocess
import sys
import time
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING

import pytest

import tinystore
from tinystore._connection import start_failure
from tinystore._session import PROTOCOL
from ways import BINARY, CORE, SERVED, WAYS

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


async def _held_after_its_sidecar(directory: Path) -> tuple[tinystore.Store, tinystore.Store]:
    """A store whose sidecar was stopped and whose directory a private child holds: its next call dials, and waits."""
    store = await tinystore.open(directory, idle="1s")
    await store.bucket("notes", str).set("a", "kept")
    assert BINARY is not None
    stopped = subprocess.run([BINARY, "stop", str(directory)], capture_output=True, text=True, check=False)
    assert stopped.returncode == 0, stopped.stderr
    # the stopped sidecar lets go of the directory a moment after its SERVE goes
    deadline = time.monotonic() + 10
    while True:
        try:
            return store, await tinystore.open(directory, private=True)
        except tinystore.TinystoreError:
            if time.monotonic() > deadline:
                raise
            await asyncio.sleep(0.05)


# contract: WIRE-11
@pytest.mark.skipif(not SERVED, reason="tinystore is not built here")
async def test_an_open_cancelled_while_another_holds_the_directory_stops_at_once(directory: Path) -> None:
    holder = await tinystore.open(directory, private=True)
    started = time.monotonic()
    with pytest.raises(TimeoutError) as failed:
        async with asyncio.timeout(1):
            await tinystore.open(directory, idle="1s")
    assert time.monotonic() - started < 1.5
    cancelled = failed.value.__cause__
    assert isinstance(cancelled, asyncio.CancelledError)
    said = f"open {directory}: cancelled while another process holds the directory and no sidecar answers"
    assert said in getattr(cancelled, "__notes__", []), cancelled
    # every sidecar the open started found the directory held and left; none starts once it is free
    await asyncio.sleep(1)
    await holder.close()
    await asyncio.sleep(1.5)
    assert not (directory / "server" / "SERVE").exists()


async def _silent(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
    """A server that takes a connection and never answers it."""
    await reader.read()
    writer.close()


# contract: WIRE-11
@pytest.mark.skipif(not SERVED, reason="tinystore is not built here")
async def test_an_open_cancelled_while_the_server_serve_names_says_nothing_starts_no_sidecar_after(
    directory: Path,
) -> None:
    server = await asyncio.start_server(_silent, "127.0.0.1", 0)
    port = server.sockets[0].getsockname()[1]
    (directory / "server").mkdir()
    serve = directory / "server" / "SERVE"
    secret = base64.urlsafe_b64encode(secrets.token_bytes(32)).decode().rstrip("=")
    published = json.dumps(
        {
            "protocol": PROTOCOL,
            "endpoints": [f"tcp://127.0.0.1:{port}"],
            "secret": secret,
            "sidecar": True,
            "instance": "silent",
        }
    )
    serve.write_text(published)
    with pytest.raises(TimeoutError) as failed:
        async with asyncio.timeout(0.5):
            await tinystore.open(directory, idle="1s")
    said = f"open {directory}: cancelled while the sidecar starts"
    assert said in getattr(failed.value.__cause__, "__notes__", []), failed.value.__cause__
    # a sidecar started after the cancel opens its log and publishes itself well within this
    await asyncio.sleep(1.5)
    started = (directory / "server" / "serve.log").exists()
    if started:
        assert BINARY is not None
        subprocess.run([BINARY, "stop", str(directory)], capture_output=True, check=False)
    assert not started
    assert serve.read_text() == published
    server.close()
    await server.wait_closed()


# contract: WIRE-11
async def test_a_connect_cancelled_in_its_handshake_leaves_the_loop_nothing_to_log() -> None:
    loop = asyncio.get_running_loop()
    logged: list[dict[str, object]] = []
    loop.set_exception_handler(lambda _, context: logged.append(context))
    server = await asyncio.start_server(_silent, "127.0.0.1", 0)
    port = server.sockets[0].getsockname()[1]
    try:
        with pytest.raises(TimeoutError):
            async with asyncio.timeout(0.5):
                await tinystore.connect(f"tcp://127.0.0.1:{port}", token="unused")
        await asyncio.sleep(0.1)
        gc.collect()
        assert logged == []
    finally:
        loop.set_exception_handler(None)
        server.close()
        await server.wait_closed()


# contract: WIRE-11
async def test_a_connect_cancelled_while_its_server_says_nothing_lets_the_socket_go() -> None:
    ended = asyncio.Event()

    async def silent(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        await reader.read()
        ended.set()
        writer.close()

    server = await asyncio.start_server(silent, "127.0.0.1", 0)
    port = server.sockets[0].getsockname()[1]
    started = time.monotonic()
    with pytest.raises(TimeoutError):
        async with asyncio.timeout(0.5):
            await tinystore.connect(f"tcp://127.0.0.1:{port}", token="unused")
    await asyncio.wait_for(ended.wait(), 1)
    assert time.monotonic() - started < 1.5
    server.close()
    await server.wait_closed()


# contract: WIRE-11
@pytest.mark.skipif(not SERVED, reason="tinystore is not built here")
async def test_a_store_closed_while_it_dials_returns_at_once_and_its_waiting_call_fails_closed(
    directory: Path,
) -> None:
    store, holder = await _held_after_its_sidecar(directory)
    waiting = asyncio.ensure_future(store.bucket("notes", str).get("a"))
    await asyncio.sleep(0.3)
    started = time.monotonic()
    await store.close()
    assert time.monotonic() - started < 1
    with pytest.raises(tinystore.ClosedError, match="the store is closed"):
        await waiting
    await holder.close()
