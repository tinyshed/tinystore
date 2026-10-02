"""A connection's protocol, without its bytes' way there and back.

Frames in, frames out, streams, credit both ways, PING, GOAWAY and cancelling,
as docs/wire.md says them. The transport hands it what it read and takes what
it writes; nothing here waits on a socket, so the same code serves a Unix
socket, a Windows pipe, a child's stdio and TLS. The frames asked for in one
turn of the event loop leave in one write.
"""

from __future__ import annotations

import asyncio
import hashlib
import hmac
from collections import deque
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from ._wire import frame as f
from ._wire.messages import Failure, GoAway, Hello, Welcome
from .errors import (
    CallCancelledError,
    ClosedError,
    LimitError,
    ProtocolError,
    TinystoreError,
    UnavailableError,
    error_of,
)

if TYPE_CHECKING:
    from collections.abc import Callable

PROTOCOL = 1

DOWNLOAD_WINDOW = 2 << 20
"""What this client lets the server send on a stream before it grants more: a body at least."""

_WELCOME_MOST = 1 << 20
"""The largest body a WELCOME may come in, before a body is agreed."""


class LostError(ClosedError):
    """The connection was lost with a stream on it.

    sent says whether its REQUEST had left: one that had not may go on
    another connection whatever it was, and one that had is a read to send
    again or a write whose outcome is unknown.
    """

    def __init__(self, message: str, sent: bool) -> None:
        super().__init__(message)
        self.sent = sent


@dataclass(frozen=True, slots=True)
class Agreed:
    """What a WELCOME agreed."""

    server: str
    protocol: int
    instance: bytes
    capability: str
    max_body: int
    in_flight: int
    connection_credit: int
    stream_credit: int
    engines: list[str]
    now: int


@dataclass(frozen=True, slots=True)
class Event:
    """A frame of a stream's answer: the RESPONSE or a DATA, end saying it is the last."""

    kind: str
    body: bytes
    end: bool


class Stream:
    """A client's REQUEST from its frame to the server's final frame.

    That span is the one in which its number is in use.
    """

    def __init__(self, session: Session, number: int, method: int, upload_credit: int) -> None:
        self.id = number
        self.method = method
        self.sent = False
        """the REQUEST has left, so the server may have acted on it"""
        self.finished = False
        """the server's final frame came, or the connection ended"""
        self.cancelled = False
        self._session = session
        self._events: deque[Event] = deque()
        self._waiting: asyncio.Future[Event] | None = None
        self._failure: BaseException | None = None
        self._upload_credit = upload_credit
        self._credit_waiters: list[asyncio.Future[None]] = []
        self._granting = 0

    async def next(self) -> Event:
        """The next frame of the answer, or its error; events already arrived are read first."""
        if self._events:
            return self._events.popleft()
        if self._failure is not None:
            raise self._failure
        self._waiting = self._session.loop.create_future()
        return await self._waiting

    async def send(self, body: bytes, end: bool) -> None:
        """Sends a body of an upload as DATA once the stream's credit lets it go.

        end says it is the last.
        """
        while not self.finished and self._upload_credit < len(body):
            waiter: asyncio.Future[None] = self._session.loop.create_future()
            self._credit_waiters.append(waiter)
            await waiter
        if self.finished:
            raise self._failure or ClosedError("the stream ended before its upload did")
        self._upload_credit -= len(body)
        self._session.enqueue(self, f.frame(f.DATA, f.END if end else 0, 0, self.id, body), len(body))

    def consumed(self, n: int) -> None:
        """Gives back what a DATA took of the stream's credit once the caller is done with it."""
        if self.finished:
            return
        self._granting += n
        if self._granting >= DOWNLOAD_WINDOW // 2:
            self._session.post(f.credit(self.id, self._granting))
            self._granting = 0

    def cancel(self) -> None:
        """Asks the server to stop; the stream keeps its number until the server's final frame."""
        if self.finished or self.cancelled:
            return
        self.cancelled = True
        self._session.cancel(self)
        self.fail(CallCancelledError("the call was cancelled"))

    def deliver(self, event: Event) -> None:
        if self.cancelled:
            return
        waiting = self._waiting
        if waiting is not None and not waiting.done():
            self._waiting = None
            waiting.set_result(event)
            return
        self._events.append(event)

    def fail(self, err: BaseException) -> None:
        if self._failure is None:
            self._failure = err
        waiting = self._waiting
        self._waiting = None
        if waiting is not None and not waiting.done():
            waiting.set_exception(err)
        for w in self._credit_waiters:
            if not w.done():
                w.set_exception(err)
        self._credit_waiters.clear()

    def grant(self, n: int) -> None:
        self._upload_credit += n
        self._wake()

    def finish(self) -> None:
        self.finished = True
        self._wake()

    def _wake(self) -> None:
        for w in self._credit_waiters:
            if not w.done():
                w.set_result(None)
        self._credit_waiters.clear()


@dataclass(slots=True)
class _Queued:
    """A frame waiting for the connection's credit, in the order it was asked for."""

    stream: Stream
    bytes: bytes
    counted: int
    request: bool


class Session:
    """The protocol of one connection: it writes through send and reads what receive is given."""

    def __init__(
        self,
        send: Callable[[bytes], None],
        client: str,
        *,
        token: str | None = None,
        secret: bytes | None = None,
        challenge: bytes | None = None,
    ) -> None:
        self.loop = asyncio.get_running_loop()
        self._send = send
        self._secret = secret
        self._challenge = challenge
        self.welcomed: asyncio.Future[Agreed] = self.loop.create_future()
        # a caller that never awaits the handshake still hears of its end
        self.welcomed.add_done_callback(lambda fut: fut.cancelled() or fut.exception())
        self.agreed: Agreed | None = None
        self._streams: dict[int, Stream] = {}
        self._next = 0
        self._waiting_to_open: deque[asyncio.Future[None]] = deque()
        self._queue: deque[_Queued] = deque()
        self._credit = 0
        self._out: list[bytes] = []
        self._flushing = False
        self._rest = b""
        self.going_away = False
        self.ended: BaseException | None = None
        self._end_listeners: list[Callable[[BaseException], None]] = []
        self.post(
            f.frame(
                f.HELLO,
                0,
                0,
                0,
                Hello.encode(
                    protocol=PROTOCOL,
                    client=client,
                    token=token,
                    stream_credit=DOWNLOAD_WINDOW,
                    challenge=challenge,
                ),
            )
        )

    @property
    def streams(self) -> int:
        """Streams in use, their final frames not yet come."""
        return len(self._streams)

    def on_end(self, listener: Callable[[BaseException], None]) -> None:
        if self.ended is not None:
            listener(self.ended)
            return
        self._end_listeners.append(listener)

    async def open(self, method: int, body: bytes, end: bool) -> Stream:
        """Opens a stream with its REQUEST, waiting while every stream in flight is in use."""
        agreed = await asyncio.shield(self.welcomed)
        if len(body) > agreed.max_body:
            raise LimitError(f"a request of {len(body)} bytes, past the {agreed.max_body} a body holds")
        while len(self._streams) >= agreed.in_flight and self._refused() is None:
            waiter: asyncio.Future[None] = self.loop.create_future()
            self._waiting_to_open.append(waiter)
            await waiter
        refused = self._refused()
        if refused is not None:
            raise refused
        stream = Stream(self, self._next_id(), method, agreed.stream_credit)
        self._streams[stream.id] = stream
        self._enqueue(
            _Queued(
                stream,
                f.frame(f.REQUEST, f.END if end else 0, method, stream.id, body),
                len(body),
                True,
            )
        )
        return stream

    def _refused(self) -> TinystoreError | None:
        if self.ended is not None:
            return LostError(f"the connection ended: {self.ended}", sent=False)
        if self.going_away:
            return UnavailableError("the server is closing this connection")
        return None

    async def call(self, method: int, body: bytes) -> bytes:
        """A call: one REQUEST, and the RESPONSE that answers it.

        Cancelling the task that awaits it cancels the stream.
        """
        stream = await self.open(method, body, True)
        try:
            event = await stream.next()
        except asyncio.CancelledError:
            stream.cancel()
            raise
        if event.kind != "response" or not event.end:
            self.fault(ProtocolError(f"a {event.kind} without END answering a call on stream {stream.id}"))
        return event.body

    def _next_id(self) -> int:
        # from 1, never 0 and never one in use, so that a frame for a stream
        # that has ended never reaches the next one
        while True:
            self._next = 1 if self._next == 0xFFFF_FFFF else self._next + 1
            if self._next not in self._streams:
                return self._next

    def enqueue(self, stream: Stream, frame: bytes, counted: int) -> None:
        self._enqueue(_Queued(stream, frame, counted, False))

    def _enqueue(self, q: _Queued) -> None:
        self._queue.append(q)
        self._drain()

    def _drain(self) -> None:
        # frames that count against the connection's credit leave in the order
        # they were asked for, as far as the credit goes
        while self._queue:
            q = self._queue[0]
            if q.counted > self._credit:
                return
            self._queue.popleft()
            self._credit -= q.counted
            if q.request:
                q.stream.sent = True
            self.post(q.bytes)

    def post(self, frame: bytes) -> None:
        """Writes a frame that does not wait for credit: a CREDIT, a CANCEL, a PONG."""
        if self.ended is not None:
            return
        self._out.append(frame)
        if not self._flushing:
            self._flushing = True
            self.loop.call_soon(self._flush)

    def _flush(self) -> None:
        self._flushing = False
        if not self._out or self.ended is not None:
            return
        out = self._out[0] if len(self._out) == 1 else b"".join(self._out)
        self._out = []
        self._send(out)

    def cancel(self, stream: Stream) -> None:
        if not stream.sent:
            self._queue = deque(q for q in self._queue if q.stream is not stream)
            self._release(stream)
            return
        self.post(f.frame(f.CANCEL, 0, 0, stream.id, b""))

    def _release(self, stream: Stream) -> None:
        # takes a stream out of use, dropping what of it has not left: its
        # number may be named again at once
        stream.finish()
        if self._streams.get(stream.id) is stream:
            del self._streams[stream.id]
        self._queue = deque(q for q in self._queue if q.stream is not stream or q.request)
        while self._waiting_to_open:
            waiter = self._waiting_to_open.popleft()
            if not waiter.done():
                waiter.set_result(None)
                break

    def receive(self, chunk: bytes) -> None:
        """Takes bytes the transport read: any part of a frame, or several."""
        if self.ended is not None:
            return
        data = self._rest + chunk if self._rest else bytes(chunk)
        self._rest = b""
        at = 0
        try:
            while len(data) - at >= f.HEADER_SIZE:
                h = f.parse_header(data, at)
                f.check_header(h, self.agreed.max_body if self.agreed is not None else _WELCOME_MOST)
                stop = at + f.HEADER_SIZE + h.length
                if stop > len(data):
                    break
                self._take(h, data[at + f.HEADER_SIZE : stop])
                at = stop
        except TinystoreError as err:
            self.fault(err)
            return
        if at < len(data):
            self._rest = data[at:]

    def _take(self, h: f.Header, body: bytes) -> None:
        if self.agreed is None:
            self._handshake(h.kind, body)
            return
        match h.kind:
            case f.RESPONSE | f.DATA:
                self._answer(h.kind, h.flags, h.stream, body)
            case f.CREDIT:
                self._credited(h.stream, f.granted(body))
            case f.PING:
                self.post(f.frame(f.PONG, 0, 0, 0, body))
            case f.PONG:
                pass
            case f.GOAWAY:
                self._go_away(body)
            case _:
                raise ProtocolError(f"a {f.kind_name(h.kind)} from the server")

    def _handshake(self, kind: int, body: bytes) -> None:
        if kind == f.GOAWAY:
            away = GoAway.decode(body)
            self.end(error_of(away.get("code", "unavailable"), away.get("message", "the server refused")))
            return
        if kind != f.WELCOME:
            raise ProtocolError(f"a {f.kind_name(kind)} where the WELCOME belongs")
        w = Welcome.decode(body)
        if w.get("protocol") != PROTOCOL:
            raise ProtocolError(f"the server speaks protocol {w.get('protocol')}, not {PROTOCOL}")
        if (
            self._secret is not None
            and self._challenge is not None
            and not proves(self._secret, self._challenge, w.get("proof"))
        ):
            raise ProtocolError("the server cannot prove it read SERVE: another process holds its endpoint")
        self.agreed = Agreed(
            server=w.get("server", ""),
            protocol=w.get("protocol", PROTOCOL),
            instance=w.get("instance", b""),
            capability=w.get("capability", "data"),
            max_body=w.get("max_body", 0),
            in_flight=w.get("in_flight", 1),
            connection_credit=w.get("connection_credit", 0),
            stream_credit=w.get("stream_credit", 0),
            engines=w.get("engines", []),
            now=w.get("now", 0),
        )
        self._credit = self.agreed.connection_credit
        if not self.welcomed.done():
            self.welcomed.set_result(self.agreed)

    def _answer(self, kind: int, flags: int, number: int, body: bytes) -> None:
        stream = self._streams.get(number)
        if stream is None:
            raise ProtocolError(f"a {f.kind_name(kind)} on stream {number}, which is not in use")
        end = bool(flags & f.END)
        if end:
            self._release(stream)
        if flags & f.ERROR:
            stream.fail(failure_of(body))
            return
        stream.deliver(Event("response" if kind == f.RESPONSE else "data", body, end))
        if stream.cancelled and kind == f.DATA:
            stream.consumed(len(body))

    def _credited(self, number: int, n: int) -> None:
        if number == 0:
            self._credit += n
            self._drain()
            return
        stream = self._streams.get(number)
        if stream is not None:
            stream.grant(n)

    def _go_away(self, body: bytes) -> None:
        # after a GOAWAY no stream opens; a REQUEST still waiting for credit
        # goes nowhere and may be sent on another connection
        away = GoAway.decode(body)
        self.going_away = True
        unsent = UnavailableError(away.get("message", "the server is closing this connection"))
        for q in [q for q in self._queue if q.request]:
            q.stream.fail(unsent)
            self._release(q.stream)
        while self._waiting_to_open:
            waiter = self._waiting_to_open.popleft()
            if not waiter.done():
                waiter.set_exception(unsent)

    def fault(self, err: TinystoreError) -> None:
        """A frame broke the protocol: the connection ends, and whoever runs it closes it."""
        self.end(err if isinstance(err, ProtocolError) else ProtocolError(str(err)))

    def end(self, err: BaseException) -> None:
        """Ends the session; every stream still open learns whether its REQUEST had left."""
        if self.ended is not None:
            return
        self.ended = err
        if not self.welcomed.done():
            self.welcomed.set_exception(err)
        for stream in list(self._streams.values()):
            stream.fail(LostError(f"the connection ended: {err}", stream.sent))
            stream.finish()
        self._streams.clear()
        self._queue.clear()
        while self._waiting_to_open:
            waiter = self._waiting_to_open.popleft()
            if not waiter.done():
                waiter.set_exception(LostError(f"the connection ended: {err}", sent=False))
        listeners, self._end_listeners = self._end_listeners, []
        for listener in listeners:
            listener(err)


def failure_of(body: bytes) -> TinystoreError:
    """The error a final frame with ERROR carries."""
    failure: dict[str, Any] = Failure.decode(body)
    return error_of(failure.get("code", "internal"), failure.get("message", ""), failure.get("what"))


def proves(secret: bytes, challenge: bytes, proof: bytes | None) -> bool:
    """A WELCOME's proof is the HMAC-SHA256 of the challenge keyed with SERVE's secret."""
    if proof is None or len(proof) != 32:
        return False
    return hmac.compare_digest(hmac.new(secret, challenge, hashlib.sha256).digest(), proof)
