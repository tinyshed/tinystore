"""The session, and what the link and the handles make of it, without a server.

A peer in the test reads the frames the client writes and answers with frames
of its own, so that an order a server makes only by chance is made here.
"""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING, Any

import pytest

from tinystore._connection import Connection, Link
from tinystore._runtime import Transport
from tinystore._session import Session
from tinystore._values import values_of
from tinystore._wire import frame as f
from tinystore._wire.protocol import (
    Empty,
    GoAway,
    Handle,
    KvEntry,
    KvWritten,
    SqlTxAnswer,
    SqlTxCall,
    Welcome,
    methods,
)
from tinystore.errors import ClosedError, OutcomeUnknownError, UnavailableError
from tinystore.kv import Bucket, Parts, bucket_open
from tinystore.sql import open_database

if TYPE_CHECKING:
    from collections.abc import Callable


class Peer:
    """The other end of a session: what it wrote, a write at a time, and a way to answer."""

    def __init__(self) -> None:
        self.writes: list[bytes] = []
        self.session = Session(self.writes.append, "test")

    async def frames(self) -> list[tuple[f.Header, bytes]]:
        """Every frame written since the last look, once the loop's turns have let them leave."""
        for _ in range(5):
            await asyncio.sleep(0)
        frames: list[tuple[f.Header, bytes]] = []
        for written in self.writes:
            at = 0
            while at < len(written):
                h = f.parse_header(written, at)
                frames.append((h, written[at + f.HEADER_SIZE : at + f.HEADER_SIZE + h.length]))
                at += f.HEADER_SIZE + h.length
        self.writes.clear()
        return frames

    def welcome(self, **fields: Any) -> None:
        agreed: dict[str, Any] = {
            "protocol": 1,
            "server": "test",
            "capability": "admin",
            "max_body": 1 << 20,
            "in_flight": 4,
            "connection_credit": 1 << 20,
            "stream_credit": 1 << 16,
            **fields,
        }
        self.session.receive(f.frame(f.WELCOME, 0, 0, 0, Welcome.encode(**agreed)))

    def answer(self, stream: int, body: bytes) -> None:
        self.session.receive(f.frame(f.RESPONSE, f.END, 0, stream, body))


def requests(frames: list[tuple[f.Header, bytes]]) -> list[f.Header]:
    return [h for h, _ in frames if h.kind == f.REQUEST]


# contract: WIRE-07
async def test_a_request_a_goaway_refused_stays_unsent_when_the_credit_comes_back() -> None:
    peer = Peer()
    peer.welcome(connection_credit=1)
    await peer.frames()
    sent = asyncio.ensure_future(peer.session.call(0x0104, b"\x80"))
    unsent = asyncio.ensure_future(peer.session.call(0x0104, b"\x80"))
    [request] = requests(await peer.frames())
    peer.session.receive(f.frame(f.GOAWAY, 0, 0, 0, GoAway.encode(code="unavailable", message="closing")))
    with pytest.raises(UnavailableError):
        await unsent
    # the server grants credit back as it finishes the streams it still runs
    peer.session.receive(f.credit(0, 1))
    assert requests(await peer.frames()) == []
    peer.answer(request.stream, KvEntry.encode(found=True))
    assert KvEntry.decode(await sent)["found"]
    assert peer.session.ended is None


def scripted(script: Callable[[f.Header, bytes], list[bytes] | None], dialled: list[Session] | None = None) -> Link:
    """A link to a server that answers by script, a turn after each write as a socket would.

    Each dial is a connection of its own, its session added to dialled.
    """
    welcome = Welcome.encode(
        protocol=1,
        server="script/0",
        capability="admin",
        max_body=1 << 20,
        in_flight=64,
        connection_credit=1 << 20,
        stream_credit=1 << 20,
    )

    async def dial() -> Connection:
        loop = asyncio.get_running_loop()
        session: Session | None = None

        def answer(written: bytes) -> None:
            at = 0
            while at < len(written) and session is not None:
                h = f.parse_header(written, at)
                body = written[at + f.HEADER_SIZE : at + f.HEADER_SIZE + h.length]
                at += f.HEADER_SIZE + h.length
                if h.kind == f.HELLO:
                    session.receive(f.frame(f.WELCOME, 0, 0, 0, welcome))
                if h.kind not in (f.REQUEST, f.DATA):
                    continue
                answered = script(h, body)
                if answered is None:
                    session.end(ClosedError("the script ended the connection"))
                    return
                for each in answered:
                    session.receive(each)

        def write(written: bytes) -> None:
            loop.call_soon(answer, written)

        session = Session(write, "test/0")
        if dialled is not None:
            dialled.append(session)
        connection = Connection(session, Transport(lambda _: None, lambda: None))
        await connection.handshake()
        return connection

    return Link(dial)


def committing(h: f.Header, body: bytes) -> list[bytes] | None:
    """Opens the database, begins the transaction, answers each statement, and ends the connection at the commit."""
    if h.method == methods["sql.open"]:
        return [f.frame(f.RESPONSE, f.END, 0, h.stream, Handle.encode(handle=1))]
    if h.method == methods["sql.tx"]:
        return [f.frame(f.RESPONSE, 0, 0, h.stream, Empty.encode())]
    if SqlTxCall.decode(body).get("commit"):
        return None
    return [f.frame(f.DATA, 0, 0, h.stream, SqlTxAnswer.encode(done={"changes": 1}))]


# contract: WIRE-05
async def test_a_transaction_whose_commit_left_unanswered_has_an_unknown_outcome() -> None:
    db = await open_database(scripted(committing), set(), "app", None, None)

    async def sell(tx: Any) -> None:
        await tx.exec("update items set stock = stock - 1 where sku = 'a'")

    with pytest.raises(OutcomeUnknownError, match="sql app"):
        await db.tx(sell)


# contract: WIRE-06
async def test_a_transaction_lost_before_its_commit_left_is_closed_since_the_server_rolls_it_back() -> None:
    dialled: list[Session] = []
    db = await open_database(scripted(committing, dialled), set(), "app", None, None)

    async def sell(tx: Any) -> None:
        await tx.exec("update items set stock = stock - 1 where sku = 'a'")
        dialled[-1].end(ClosedError("reset"))

    with pytest.raises(ClosedError) as failed:
        await db.tx(sell)
    assert not isinstance(failed.value, OutcomeUnknownError)


# contract: WIRE-05
async def test_a_write_whose_handles_open_lost_the_connection_goes_again_on_the_next_since_it_never_left() -> None:
    dialled: list[Session] = []
    sets: list[int] = []

    def opening_lost_first(h: f.Header, body: bytes) -> list[bytes] | None:
        if h.method == methods["kv.bucket.open"]:
            if len(dialled) == 1:
                return None
            return [f.frame(f.RESPONSE, f.END, 0, h.stream, Handle.encode(handle=7))]
        sets.append(h.stream)
        return [f.frame(f.RESPONSE, f.END, 0, h.stream, KvWritten.encode(written=True, version=bytes(8)))]

    link = scripted(opening_lost_first, dialled)
    sessions: Bucket[str] = Bucket(
        Parts(
            link,
            "sessions",
            bucket_open("sessions", None, None, False, None),
            "kv.bucket.open",
            (),
            values_of(str),
            None,
        )
    )
    await sessions.set("k", "v")
    assert len(dialled) == 2
    assert len(sets) == 1
