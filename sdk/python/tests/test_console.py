"""The console lines every logger writes: records/testdata/console.json, which Go's records.Handler and the Bun logger
are tested against too, and the handler of the console alone."""

from __future__ import annotations

import json
import logging
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
    assert pretty_line(line, vector.get("color", False), utc=True) == vector["pretty"]


@pytest.fixture
def logger() -> Iterator[logging.Logger]:
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


def test_pretty_and_stdout_and_off(logger: logging.Logger, capsys: pytest.CaptureFixture[str]) -> None:
    logger.addHandler(tinystore.handler("app", console="pretty", stdout=True))
    logger.addHandler(tinystore.handler("quiet", console="off"))
    logger.warning("slow request", extra={"ms": 1200})
    out, err = capsys.readouterr()
    assert out.endswith(" WARN  app  slow request  logger=tinystore-console ms=1200\n") and err == ""


def test_an_unknown_console_is_refused() -> None:
    with pytest.raises(tinystore.InvalidError):
        tinystore.handler("app", console="loud")  # type: ignore[arg-type]
