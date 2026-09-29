"""The session without a server: a peer in the test reads the frames it writes and answers."""

from __future__ import annotations

import asyncio
import hashlib
import hmac
import struct
from typing import Any

import pytest

from tinystore._session import LostError, Session
from tinystore._wire import frame as f
from tinystore._wire.messages import Empty, Failure, GoAway, Hello, KvEntry, Welcome
from tinystore.errors import (
    CallCancelledError,
    ConflictError,
    LimitError,
    ProtocolError,
    UnavailableError,
)


class Peer:
    """The other end of a session: what it wrote, a write at a time, and a way to answer."""

    def __init__(self, **options: Any) -> None:
        self.writes: list[bytes] = []
        self.session = Session(self.writes.append, "test", **options)

    async def frames(self) -> list[tuple[f.Header, bytes]]:
        """Every frame written since the last look, once the loop's turns have written them.

        A call's task runs a turn after it is made and its write the turn after that.
        """
        for _ in range(3):
            await asyncio.sleep(0)
        found: list[tuple[f.Header, bytes]] = []
        for data in self.writes:
            at = 0
            while at < len(data):
                h = f.parse_header(data, at)
                found.append((h, data[at + f.HEADER_SIZE : at + f.HEADER_SIZE + h.length]))
                at += f.HEADER_SIZE + h.length
        self.writes.clear()
        return found

    def welcome(self, **fields: Any) -> None:
        agreed: dict[str, Any] = {
            "protocol": 1,
            "server": "test",
            "capability": "admin",
            "max_body": 1 << 20,
            "in_flight": 4,
            "connection_credit": 1 << 20,
            "stream_credit": 1 << 16,
            "now": 1,
        }
        self.session.receive(f.frame(f.WELCOME, 0, 0, 0, Welcome.encode({**agreed, **fields})))

    def answer(self, stream: int, body: bytes, flags: int = f.END, kind: int = f.RESPONSE) -> None:
        self.session.receive(f.frame(kind, flags, 0, stream, body))


def requests(frames: list[tuple[f.Header, bytes]]) -> list[tuple[f.Header, bytes]]:
    return [(h, b) for h, b in frames if h.kind == f.REQUEST]


async def test_a_hello_leaves_first_stating_the_download_window() -> None:
    peer = Peer()
    [(h, body)] = await peer.frames()
    assert h.kind == f.HELLO
    hello = Hello.decode(body)
    assert hello["protocol"] == 1
    assert hello["stream_credit"] == 2 << 20


async def test_a_proof_that_checks_lets_calls_go_and_one_that_does_not_ends_it() -> None:
    secret, challenge = bytes([7]) * 32, bytes([9]) * 16
    proof = hmac.new(secret, challenge, hashlib.sha256).digest()
    good = Peer(secret=secret, challenge=challenge)
    good.welcome(proof=proof)
    assert (await good.session.welcomed).capability == "admin"

    bad = Peer(secret=secret, challenge=challenge)
    bad.welcome(proof=bytes(32))
    with pytest.raises(ProtocolError):
        await bad.session.welcomed


async def test_a_goaway_where_the_welcome_belongs_is_its_code() -> None:
    peer = Peer()
    peer.session.receive(f.frame(f.GOAWAY, 0, 0, 0, GoAway.encode(code="limit", message="too many connections")))
    with pytest.raises(LimitError):
        await peer.session.welcomed


async def test_calls_are_answered_in_any_order() -> None:
    peer = Peer()
    peer.welcome()
    await peer.frames()
    first = asyncio.ensure_future(peer.session.call(0x0102, b"\x80"))
    second = asyncio.ensure_future(peer.session.call(0x0102, b"\x80"))
    sent = requests(await peer.frames())
    assert [(h.kind, h.flags, h.method) for h, _ in sent] == [(f.REQUEST, f.END, 0x0102)] * 2
    peer.answer(sent[1][0].stream, KvEntry.encode(found=True))
    peer.answer(sent[0][0].stream, KvEntry.encode())
    assert KvEntry.decode(await second)["found"] is True
    assert "found" not in KvEntry.decode(await first)


async def test_the_frames_asked_for_in_one_turn_leave_in_one_write() -> None:
    peer = Peer()
    peer.welcome()
    await peer.frames()
    calls = [asyncio.ensure_future(peer.session.call(0x0102, b"\x80")) for _ in range(3)]
    await asyncio.sleep(0)
    await asyncio.sleep(0)
    assert len(peer.writes) == 1
    for call in calls:
        call.cancel()


async def test_an_error_ends_its_call_as_its_code() -> None:
    peer = Peer()
    peer.welcome()
    call = asyncio.ensure_future(peer.session.call(0x0104, b"\x80"))
    [(h, _)] = requests(await peer.frames())
    peer.answer(
        h.stream,
        Failure.encode(code="conflict", message="state changed", what={"bucket": "sessions"}),
        f.END | f.ERROR,
    )
    with pytest.raises(ConflictError) as err:
        await call
    assert err.value.what == {"bucket": "sessions"}


async def test_a_request_past_the_agreed_body_is_refused_before_it_leaves() -> None:
    peer = Peer()
    peer.welcome(max_body=8)
    with pytest.raises(LimitError):
        await peer.session.call(0x0104, bytes(9))


async def test_no_more_streams_open_than_the_welcome_lets_fly() -> None:
    peer = Peer()
    peer.welcome(in_flight=2)
    await peer.frames()
    calls = [asyncio.ensure_future(peer.session.call(0x0102, b"\x80")) for _ in range(3)]
    flying = requests(await peer.frames())
    assert len(flying) == 2
    peer.answer(flying[0][0].stream, KvEntry.encode(found=True))
    await calls[0]
    third = requests(await peer.frames())
    assert len(third) == 1
    assert third[0][0].stream != flying[1][0].stream
    for call in calls[1:]:
        call.cancel()


async def test_requests_wait_for_the_connections_credit_in_order() -> None:
    peer = Peer()
    peer.welcome(connection_credit=10)
    await peer.frames()
    calls = [asyncio.ensure_future(peer.session.call(0x0104, bytes(6))) for _ in range(2)]
    assert len(await peer.frames()) == 1
    peer.session.receive(f.credit(0, 6))
    assert [len(b) for _, b in await peer.frames()] == [6]
    for call in calls:
        call.cancel()


async def test_a_download_grants_its_credit_back_once_half_the_window_is_read() -> None:
    peer = Peer()
    peer.welcome()
    await peer.frames()
    stream = await peer.session.open(0x010D, b"\x80", True)
    await peer.frames()
    peer.answer(stream.id, Empty.encode(), 0)
    chunk = bytes(1 << 19)
    peer.answer(stream.id, chunk, 0, f.DATA)
    peer.answer(stream.id, chunk, 0, f.DATA)
    assert (await stream.next()).kind == "response"
    stream.consumed(len((await stream.next()).body))
    assert await peer.frames() == []
    stream.consumed(len((await stream.next()).body))
    [(h, body)] = await peer.frames()
    assert h.kind == f.CREDIT
    assert struct.unpack("<I", body)[0] == 1 << 20


async def test_an_uploads_data_waits_for_the_streams_credit() -> None:
    peer = Peer()
    peer.welcome(stream_credit=4)
    await peer.frames()
    stream = await peer.session.open(0x0309, b"\x80", False)
    await stream.send(bytes(4), False)
    sending = asyncio.ensure_future(stream.send(bytes(4), True))
    assert [h.kind for h, _ in await peer.frames()] == [f.REQUEST, f.DATA]
    peer.session.receive(f.credit(stream.id, 4))
    await sending
    assert [(h.kind, h.flags) for h, _ in await peer.frames()] == [(f.DATA, f.END)]


async def test_a_call_cancelled_before_its_request_leaves_never_leaves() -> None:
    peer = Peer()
    peer.welcome(connection_credit=1)
    await peer.frames()
    waiting = asyncio.ensure_future(peer.session.call(0x0104, b"\x80"))
    cancelled = asyncio.ensure_future(peer.session.call(0x0104, bytes(1)))
    await asyncio.sleep(0)
    cancelled.cancel()
    with pytest.raises(asyncio.CancelledError):
        await cancelled
    peer.session.receive(f.credit(0, 1))
    assert len(requests(await peer.frames())) == 1
    waiting.cancel()


async def test_a_call_cancelled_in_flight_sends_cancel_and_keeps_its_number() -> None:
    peer = Peer()
    peer.welcome()
    await peer.frames()
    call = asyncio.ensure_future(peer.session.call(0x0102, b"\x80"))
    [(h, _)] = requests(await peer.frames())
    call.cancel()
    with pytest.raises(asyncio.CancelledError):
        await call
    [(cancel, _)] = await peer.frames()
    assert (cancel.kind, cancel.stream) == (f.CANCEL, h.stream)
    assert peer.session.streams == 1
    peer.answer(h.stream, KvEntry.encode(found=True))
    assert peer.session.streams == 0


async def test_a_cancelled_stream_says_so_to_whatever_waits_on_it() -> None:
    peer = Peer()
    peer.welcome()
    await peer.frames()
    stream = await peer.session.open(0x010D, b"\x80", True)
    stream.cancel()
    with pytest.raises(CallCancelledError):
        await stream.next()


async def test_a_goaway_refuses_new_streams_and_the_requests_that_have_not_left() -> None:
    peer = Peer()
    peer.welcome(connection_credit=1)
    await peer.frames()
    sent = asyncio.ensure_future(peer.session.call(0x0104, b"\x80"))
    unsent = asyncio.ensure_future(peer.session.call(0x0104, b"\x80"))
    [(h, _)] = requests(await peer.frames())
    peer.session.receive(f.frame(f.GOAWAY, 0, 0, 0, GoAway.encode(code="unavailable", message="x")))
    with pytest.raises(UnavailableError):
        await unsent
    with pytest.raises(UnavailableError):
        await peer.session.call(0x0102, b"\x80")
    peer.answer(h.stream, KvEntry.encode(found=True))
    assert KvEntry.decode(await sent)["found"] is True


async def test_a_lost_connection_tells_each_stream_whether_its_request_had_left() -> None:
    peer = Peer()
    peer.welcome(connection_credit=1)
    await peer.frames()
    sent = asyncio.ensure_future(peer.session.call(0x0104, b"\x80"))
    unsent = asyncio.ensure_future(peer.session.call(0x0104, b"\x80"))
    await peer.frames()
    peer.session.end(ConnectionResetError("reset"))
    with pytest.raises(LostError) as was_sent:
        await sent
    with pytest.raises(LostError) as not_sent:
        await unsent
    assert (was_sent.value.sent, not_sent.value.sent) == (True, False)


async def test_a_ping_is_answered_with_its_bytes() -> None:
    peer = Peer()
    peer.welcome()
    await peer.frames()
    peer.session.receive(f.frame(f.PING, 0, 0, 0, bytes(range(8))))
    [(h, body)] = await peer.frames()
    assert (h.kind, body) == (f.PONG, bytes(range(8)))


async def test_a_frame_that_breaks_the_protocol_ends_the_session() -> None:
    peer = Peer()
    peer.welcome()
    call = asyncio.ensure_future(peer.session.call(0x0102, b"\x80"))
    await peer.frames()
    peer.session.receive(f.frame(f.REQUEST, f.END, 0x0102, 1, b"\x80"))
    assert isinstance(peer.session.ended, ProtocolError)
    with pytest.raises(LostError):
        await call


async def test_frames_split_anywhere_read_as_whole() -> None:
    peer = Peer()
    welcome = f.frame(
        f.WELCOME,
        0,
        0,
        0,
        Welcome.encode(protocol=1, max_body=1 << 20, in_flight=1, connection_credit=1 << 20, stream_credit=1),
    )
    for b in welcome:
        peer.session.receive(bytes([b]))
    assert (await peer.session.welcomed).in_flight == 1
