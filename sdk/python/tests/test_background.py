"""How background work says what failed: the same lines, at the same times, as Go's failureLog.

Go's TestBackgroundFailuresAreLoggedOncePerQuietPeriod runs the same sequence.
"""

from __future__ import annotations

import json
from typing import Any

from tinystore._background import DropNotice, FailureLog, Level

MINUTE = 60.0


class Recorder:
    def __init__(self) -> None:
        self.said: list[str] = []

    def __call__(self, level: Level, message: str, fields: dict[str, Any]) -> None:
        self.said.append(f"{level} {message} {json.dumps(fields, separators=(',', ':'))}")


def test_a_repeated_failure_is_said_once_a_quiet_period_and_its_recovery_once() -> None:
    say, now = Recorder(), [0.0]
    failures = FailureLog("maintenance", say, lambda: now[0])
    for _ in range(11):
        failures.failed(OSError("disk full"))
        now[0] += MINUTE
    failures.succeeded()
    failures.succeeded()

    assert say.said == [
        'warn background work failed {"work":"maintenance","error":"disk full","failures":1}',
        'warn background work failed {"work":"maintenance","error":"disk full","failures":11}',
        'info background work recovered {"work":"maintenance","failures":11}',
    ]


def test_a_failure_that_changes_is_said_at_once() -> None:
    say = Recorder()
    failures = FailureLog("records flush api", say, lambda: 0.0)
    failures.failed(OSError("disk full"))
    failures.failed(ConnectionError("connection lost"))
    failures.failed(ConnectionError("connection lost"))

    assert say.said == [
        'warn background work failed {"work":"records flush api","error":"disk full","failures":1}',
        'warn background work failed {"work":"records flush api","error":"connection lost","failures":2}',
    ]


def test_dropped_lines_are_said_at_a_flush_once_a_quiet_period_counted_since_the_last_time() -> None:
    say, now = Recorder(), [0.0]
    drops = DropNotice("api", 1024, say, lambda: now[0])
    drops.say_if_due()
    for _ in range(3):
        drops.dropped()
    drops.say_if_due()
    drops.dropped()
    now[0] += 9 * MINUTE
    drops.say_if_due()
    now[0] += MINUTE
    drops.say_if_due()
    drops.say_if_due()

    assert say.said == [
        'warn log lines dropped {"logger":"api","dropped":3,"buffer":1024}',
        'warn log lines dropped {"logger":"api","dropped":1,"buffer":1024}',
    ]
