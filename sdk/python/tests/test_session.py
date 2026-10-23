"""The session without a server.

A peer in the test reads the frames the client writes and answers with frames
of its own, so that an order a server makes only by chance is made here.
"""

from __future__ import annotations

import asyncio
from typing import Any

import pytest

from tinystore._session import Session
from tinystore._wire import frame as f
from tinystore._wire.protocol import GoAway, KvEntry, Welcome
from tinystore.errors import UnavailableError


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
