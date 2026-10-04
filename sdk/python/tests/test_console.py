"""The console lines every logger writes: records/testdata/console.json, which Go's records.Handler and the Bun logger
are tested against too, and the handler of the console alone."""

from __future__ import annotations

import io
import json
import logging
import sys
import time
from pathlib import Path
from typing import TYPE_CHECKING, Any

import pytest

import tinystore
from tinystore._console import Line, encode_fields, json_line, pretty_line, redactor

if TYPE_CHECKING:
    from collections.abc import Iterator

VECTORS = json.loads((Path(__file__).parents[3] / "records" / "testdata" / "console.json").read_text(encoding="utf-8"))[
    "lines"
]


@pytest.mark.parametrize("vector", VECTORS, ids=[v["name"] for v in VECTORS])
def test_a_console_line_is_the_vectors(vector: dict[str, Any]) -> None:
    hides = redactor(vector.get("redact", []))
    line = Line(
        at=int(vector["at"]),
        stream=vector["stream"],
        level=vector.get("level"),
        event=vector.get("event"),
        msg=vector.get("msg"),
        context=encode_fields(vector.get("context", []), hides),
        attrs=encode_fields(vector.get("attrs", []), hides),
        trace_id=vector.get("trace_id"),
        span_id=vector.get("span_id"),
    )
    assert json_line(line) == vector["json"]
    shown, hide_stream = vector.get("time"), vector.get("hide_stream", False)
    assert (
        pretty_line(line, vector.get("color", False), utc=True, time=shown, hide_stream=hide_stream) == vector["pretty"]
    )


@pytest.fixture
def logger(monkeypatch: pytest.MonkeyPatch) -> Iterator[logging.Logger]:
    monkeypatch.delenv("FORCE_COLOR", raising=False)
    log = logging.getLogger("tinystore-console")
    log.propagate = False
    log.setLevel(logging.DEBUG)
    yield log
    log.handlers.clear()


def test_the_console_alone_writes_each_line_as_it_is_logged(
    logger: logging.Logger, capsys: pytest.CaptureFixture[str]
) -> None:
    logger.addHandler(tinystore.handler("app", console="json", redact=["password"]))
    logger.getChild("billing").info("charged", extra={"password": "hunter2", "amount": 25})
    try:
        raise ValueError("card declined")
    except ValueError:
        logger.exception("charge failed")
    lines = [json.loads(line) for line in capsys.readouterr().err.splitlines()]
    assert lines[0] == {
        "time": lines[0]["time"],
        "level": "INFO",
        "stream": "app",
        "msg": "charged",
        "logger": "tinystore-console.billing",
        "password": "[redacted]",
        "amount": 25,
    }
    assert lines[1]["level"] == "ERROR" and "Traceback" in lines[1]["error"] and "card declined" in lines[1]["error"]


def test_the_console_is_json_off_a_terminal_and_keeps_its_level(
    logger: logging.Logger, capsys: pytest.CaptureFixture[str]
) -> None:
    logger.addHandler(tinystore.handler("app", logging.WARNING))
    logger.info("below")
    logger.warning("kept")
    err = capsys.readouterr().err
    assert err.count("\n") == 1 and json.loads(err)["msg"] == "kept"


def test_pretty_and_to_and_off(logger: logging.Logger, capsys: pytest.CaptureFixture[str]) -> None:
    logger.addHandler(tinystore.handler("app", console="pretty", to=sys.stdout))
    logger.addHandler(tinystore.handler("quiet", console="off"))
    logger.warning("slow request", extra={"ms": 1200})
    out, err = capsys.readouterr()
    assert out.endswith(" WARN  app  slow request  logger=tinystore-console ms=1200\n") and err == ""


def test_force_color_makes_a_pipe_pretty_and_coloured_unless_no_color(
    logger: logging.Logger, capsys: pytest.CaptureFixture[str], monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("FORCE_COLOR", "1")
    monkeypatch.delenv("NO_COLOR", raising=False)
    monkeypatch.delenv("TERM", raising=False)
    logger.addHandler(tinystore.handler("app"))
    logger.warning("slow request")
    err = capsys.readouterr().err
    assert "[" in err and "slow request" in err and not err.startswith("{")

    logger.handlers.clear()
    monkeypatch.setenv("NO_COLOR", "1")
    logger.addHandler(tinystore.handler("app"))
    logger.warning("slow request")
    err = capsys.readouterr().err
    assert "[" not in err and " WARN  app  slow request" in err


def test_an_unknown_console_is_refused() -> None:
    with pytest.raises(tinystore.InvalidError):
        tinystore.handler("app", console="loud")  # type: ignore[arg-type]
    with pytest.raises(tinystore.InvalidError):
        tinystore.handler("app", time="noon")  # type: ignore[arg-type]


def test_a_handler_writes_where_to_says_json_off_a_terminal(
    logger: logging.Logger, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("FORCE_COLOR", raising=False)
    pipe, pretty = io.StringIO(), io.StringIO()
    logger.addHandler(tinystore.handler("app", to=pipe))
    logger.addHandler(tinystore.handler("app", console="pretty", time="off", hide_stream=True, to=pretty))
    logger.warning("slow request", extra={"ms": 1200})
    assert json.loads(pipe.getvalue())["msg"] == "slow request"
    assert pretty.getvalue() == "WARN  slow request  logger=tinystore-console ms=1200\n"


def test_the_environment_wins_over_the_arguments_and_never_turns_a_console_on(
    logger: logging.Logger, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("LOG_LEVEL", "WARN")
    monkeypatch.setenv("LOG_FORMAT", "pretty")
    monkeypatch.setenv("LOG_TIME", "off")
    written, events = io.StringIO(), io.StringIO()
    logger.addHandler(tinystore.handler("api", logging.DEBUG, console="json", time="full", to=written))
    logger.addHandler(tinystore.handler("events", console="off", to=events))
    logger.info("below the level")
    logger.warning("kept", extra={"ms": 1200})
    assert written.getvalue() == "WARN  api  kept  logger=tinystore-console ms=1200\n"
    assert events.getvalue() == ""

    logger.handlers.clear()
    monkeypatch.setenv("LOG_FORMAT", "off")
    silenced = io.StringIO()
    logger.addHandler(tinystore.handler("api", to=silenced))
    logger.warning("w")
    assert silenced.getvalue() == ""


def test_a_value_the_environment_cannot_mean_is_ignored_and_said_once(
    logger: logging.Logger, monkeypatch: pytest.MonkeyPatch
) -> None:
    value = f"verbose-{time.time_ns()}"
    monkeypatch.setenv("LOG_LEVEL", value)
    first, second = io.StringIO(), io.StringIO()
    logger.addHandler(tinystore.handler("api", console="json", to=first))
    logger.addHandler(tinystore.handler("api", console="json", to=second))
    logger.debug("d")
    said, line = (json.loads(text) for text in first.getvalue().splitlines())
    assert said == {
        "time": said["time"],
        "level": "WARN",
        "stream": "tinystore",
        "msg": "LOG_LEVEL is ignored",
        "value": value,
        "expected": "debug, info, warn or error",
    }
    assert line["msg"] == "d" and second.getvalue().count("\n") == 1
