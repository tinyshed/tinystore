"""Channels, as docs/guides/channels/README.md has them.

Messages a program publishes on a named channel, which every subscriber
listening at that moment receives, in memory and at most once. A
subscription's handler is called one message at a time; a slow one lets the
store's log run ahead and is told what it missed, and a connection lost is
subscribed again and told that messages may have been lost meanwhile::

    messages = store.channel("messages", Chat)
    await messages.under(room_id).publish(Chat(sender="ann", text="hi"))
    async for m in messages.under(room_id).each():
        await save(m.value)
"""

from __future__ import annotations

import asyncio
import contextlib
import contextvars
import inspect
import json
import time
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Literal

from ._background import message_of, say_own
from ._connection import check_name
from ._handles import key_text
from ._session import ConnectionLostError
from ._values import from_json, to_json
from ._wire.protocol import ChannelsBatch, ChannelsPublish, ChannelsSubscribe, methods
from .errors import ClosedError, UnavailableError

if TYPE_CHECKING:
    from collections.abc import AsyncIterator, Awaitable, Callable

    from ._connection import Link
    from ._session import Stream
    from ._wire.codec import Key

_QUIET = 60.0
"""How long a subscription without on_missed or on_error waits between two lines of the store's log, in seconds."""


@dataclass(frozen=True, slots=True)
class Message[T]:
    """A message as a subscriber receives it: what was published, and the branch it was published under."""

    value: T
    branch: list[str]


@dataclass(frozen=True, slots=True)
class Missed:
    """Messages a subscriber may have lost.

    It fell behind its branch's log, and count says how many; or its
    connection was lost and made again, and how many is unknown.
    """

    reason: Literal["behind", "reconnect"]
    count: int | None = None


type OnMissed = Callable[[Missed], object]
type OnError = Callable[[BaseException, Message[Any]], object]


class Channel[T]:
    """A channel of messages of one type, or one of its branches: ``store.channel(name, Chat)``."""

    def __init__(self, link: Link, name: str, of: Any, branch: tuple[str, ...] = ()) -> None:
        if not branch:
            check_name(name, "channel")
        self.name = name
        self.branch = branch
        self._link = link
        self._of = of

    def under(self, *owners: Key) -> Channel[T]:
        """The branch of owners under this one: whose messages these are."""
        return Channel(self._link, self.name, self._of, (*self.branch, *(key_text(owner) for owner in owners)))

    async def publish(self, value: T) -> None:
        """Hands value to every subscriber of this branch that listens now, and of those above it listened to recursive.

        It answers once the store has the message, not once anyone handled it.
        """
        body = publish_body(self.name, self.branch, value)
        await self._link.run("write", lambda c: c.session.call(methods["channels.publish"], body))

    async def subscribe(
        self,
        handler: Callable[[Message[T]], object],
        *,
        recursive: bool = False,
        on_missed: OnMissed | None = None,
        on_error: OnError | None = None,
    ) -> Subscription:
        """Calls handler with each message published from now on, one at a time and in order.

        It answers once the store has the subscription, so that a message
        published after it arrives. The handler may be async; what it raises
        goes to on_error, and the store's log says it once a minute without one::

            sub = await chat.under(room_id).subscribe(lambda m: show(m.value), on_missed=lambda missed: reload())
            await sub.stop()
        """
        subscription = Subscription(
            self._link, self._asked(recursive), self._of, _Told(self._shown(), on_missed, on_error)
        )
        await subscription.start(handler)
        return subscription

    async def each(self, *, recursive: bool = False, on_missed: OnMissed | None = None) -> AsyncIterator[Message[T]]:
        """The messages published from now on, as a loop: it listens from its first turn, and break ends it.

        A raise in the loop's body ends it too::

            async for m in chat.under(room_id).each():
                await save(m.value)
        """
        told = _Told(self._shown(), on_missed, None)
        asked = self._asked(recursive)
        first = True
        while True:
            try:
                stream = await _subscribed(self._link, asked)
            except Exception as err:
                if not first and _again(err):
                    continue
                raise
            if not first:
                await told.missed(Missed("reconnect"))
            first = False
            try:
                while True:
                    event = await stream.next()
                    if event.end:
                        return
                    batch = ChannelsBatch.decode(event.body)
                    if batch.get("missed", 0) > 0:
                        await told.missed(Missed("behind", batch["missed"]))
                    for each in batch.get("messages", []):
                        yield _message_of(each, self._of)
                    stream.consumed(len(event.body))
            except Exception as err:
                if not _again(err):
                    raise
            finally:
                stream.cancel()

    def _asked(self, recursive: bool) -> bytes:
        return ChannelsSubscribe.encode(name=self.name, under=list(self.branch), recursive=recursive)

    def _shown(self) -> str:
        return shown(self.name, self.branch)


_inside: contextvars.ContextVar[Subscription | None] = contextvars.ContextVar("tinystore subscription", default=None)
"""The subscription whose handler the caller runs inside, so that its stop answers at once there."""


class Subscription:
    """A subscription whose handler is called with each message: stop ends it, and the store's close does."""

    def __init__(self, link: Link, asked: bytes, of: Any, told: _Told) -> None:
        self._link = link
        self._asked = asked
        self._of = of
        self._told = told
        self._stream: Stream | None = None
        self._stopped = False
        self._done: asyncio.Task[None] | None = None

    async def stop(self) -> None:
        """No call starts after it, and it answers once the running call ended; inside its own handler, at once."""
        self._stopped = True
        if self._stream is not None:
            self._stream.cancel()
        if _inside.get() is self or self._done is None:
            return
        with contextlib.suppress(BaseException):
            await asyncio.shield(self._done)

    async def start(self, handler: Callable[[Message[Any]], object]) -> None:
        """Registers the subscription, then runs its handler in the background."""
        self._stream = await _subscribed(self._link, self._asked)
        self._done = asyncio.ensure_future(self._run(handler))

    async def _run(self, handler: Callable[[Message[Any]], object]) -> None:
        while True:
            stream = self._stream
            if stream is None or self._stopped:
                return
            try:
                await self._deliver(stream, handler)
                return
            except Exception as err:
                if self._stopped or not _again(err):
                    self._told.ended(err, self._stopped)
                    return
            try:
                self._stream = await _subscribed(self._link, self._asked)
            except Exception as err:
                self._told.ended(err, self._stopped)
                return
            await self._told.missed(Missed("reconnect"))

    async def _deliver(self, stream: Stream, handler: Callable[[Message[Any]], object]) -> None:
        """Reads the stream's batches and calls the handler, one message at a time."""
        while True:
            event = await stream.next()
            if event.end:
                return
            batch = ChannelsBatch.decode(event.body)
            if batch.get("missed", 0) > 0 and not self._stopped:
                await self._told.missed(Missed("behind", batch["missed"]))
            for each in batch.get("messages", []):
                if self._stopped:
                    return
                message = _message_of(each, self._of)
                token = _inside.set(self)
                try:
                    answer = handler(message)
                    if inspect.isawaitable(answer):
                        await answer
                except Exception as err:
                    await self._told.failed(err, message)
                finally:
                    _inside.reset(token)
            stream.consumed(len(event.body))


async def _subscribed(link: Link, asked: bytes) -> Stream:
    """Opens a subscription's stream and waits for the RESPONSE that says the store has it."""

    async def attempt(connection: Any) -> Stream:
        stream: Stream = await connection.session.open(methods["channels.subscribe"], asked, True)
        try:
            await stream.next()
        except BaseException:
            stream.cancel()
            raise
        return stream

    return await link.run("read", attempt)


def _again(err: BaseException) -> bool:
    """Whether a subscription ended by its connection subscribes again on another."""
    return isinstance(err, ConnectionLostError | UnavailableError)


def _message_of(message: dict[str, Any], of: Any) -> Message[Any]:
    return Message(from_json(message.get("value") or "null", of), list(message.get("under", [])))


class _Told:
    """What tells what a subscription missed and what its handler raised: its own, or the log once a minute."""

    def __init__(self, shown: str, on_missed: OnMissed | None, on_error: OnError | None) -> None:
        self._shown = shown
        self._on_missed = on_missed
        self._on_error = on_error
        self._said = float("-inf")

    async def missed(self, missed: Missed) -> None:
        if self._on_missed is not None:
            await _called(self._on_missed(missed))
        elif self._speaks():
            fields: dict[str, Any] = {"channel": self._shown, "reason": missed.reason}
            if missed.count is not None:
                fields["count"] = missed.count
            say_own("warn", "a subscriber may have missed messages", fields)

    async def failed(self, error: BaseException, message: Message[Any]) -> None:
        if self._on_error is not None:
            await _called(self._on_error(error, message))
        elif self._speaks():
            say_own("warn", "a subscriber's handler raised", {"channel": self._shown, "error": message_of(error)})

    def ended(self, err: BaseException, stopped: bool) -> None:
        """Says why the subscription ended, unless it was stopped or its store closed."""
        if not stopped and not (isinstance(err, ClosedError) and not isinstance(err, ConnectionLostError)):
            say_own("warn", "a subscription ended", {"channel": self._shown, "error": message_of(err)})

    def _speaks(self) -> bool:
        now = time.monotonic()
        if now - self._said < _QUIET:
            return False
        self._said = now
        return True


async def _called(answer: object) -> None:
    if inspect.isawaitable(answer):
        await answer


def shown(name: str, branch: tuple[str, ...]) -> str:
    """A channel's branch as an error names it: channel chat: under "room-7"."""
    if not branch:
        return f"channel {name}"
    return f"channel {name}: under {' '.join(json.dumps(owner) for owner in branch)}"


def publish_body(name: str, branch: tuple[str, ...], value: Any) -> bytes:
    """A publish's body: its channel, its branch and its value as JSON."""
    return ChannelsPublish.encode(name=name, under=list(branch), value=to_json(value))


class TxChannel[T]:
    """A channel inside a transaction: its messages are handed over once the transaction commits."""

    def __init__(self, name: str, branch: tuple[str, ...], send: Callable[[bytes], Awaitable[None]]) -> None:
        self._name = name
        self._branch = branch
        self._send = send

    def under(self, *owners: Key) -> TxChannel[T]:
        return TxChannel(self._name, (*self._branch, *(key_text(owner) for owner in owners)), self._send)

    async def publish(self, value: T) -> None:
        await self._send(publish_body(self._name, self._branch, value))
