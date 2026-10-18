"""The console lines every logger writes: testdata/console/console.json, which Rust and Bun print byte for byte too."""

from __future__ import annotations

import io
import json
from pathlib import Path
from typing import Any

import pytest

import tinystore
from tinystore._console import Line, encode_fields, hide_url_passwords, json_line, pretty_line, redactor, words

FILE = json.loads((Path(__file__).parents[3] / "testdata" / "console" / "console.json").read_text(encoding="utf-8"))
VECTORS: list[dict[str, Any]] = FILE["lines"]


@pytest.mark.parametrize("vector", VECTORS, ids=[v["name"] for v in VECTORS])
def test_a_console_line_is_the_vectors(vector: dict[str, Any]) -> None:
    names = [*vector.get("redact", []), *(tinystore.secrets if vector.get("redact_secrets") else ())]
    hides = redactor(names, vector.get("keep_url_passwords", False))
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


def test_the_secrets_are_the_vectors() -> None:
    assert list(tinystore.secrets) == FILE["secrets"]


def test_url_passwords_are_hidden_through_json_escapes() -> None:
    quote, slash = chr(34), chr(92)
    uri = "postgres://ann:hunter2@db/app"
    escaped = uri.replace("/", slash + "/")
    unicode = uri.replace(":", slash + "u003a").replace("/", slash + "u002f").replace("@", slash + "u0040")
    for value in (escaped, unicode):
        spelled = quote + value + quote
        assert json.loads(hide_url_passwords(spelled)) == "postgres://ann:[redacted]@db/app"


def test_a_keys_words_are_as_a_person_reads_them() -> None:
    assert words("DB_PASSWORD") == ["db", "password"]
    assert words("passwordHash") == ["password", "hash"]
    assert words("APIKey") == ["api", "key"]
    hides = redactor(["api key", "token"])
    assert hides is not None
    assert hides.hides("apiKey") and hides.hides("API-KEY") and hides.hides("bot_token")
    assert not hides.hides("tokens_used")


def test_the_console_alone_prints_each_line_and_keeps_none(monkeypatch: pytest.MonkeyPatch) -> None:
    for name in ("LOG_LEVEL", "LOG_FORMAT", "LOG_TIME", "FORCE_COLOR"):
        monkeypatch.delenv(name, raising=False)
    out = io.StringIO()
    log = tinystore.logger("app", console="json", redact=["password"], to=out)
    log.bind(module="billing").info("charged", password="hunter2", amount=25)
    try:
        raise ValueError("card declined")
    except ValueError:
        log.exception("charge failed")
    lines = [json.loads(line) for line in out.getvalue().splitlines()]
    assert lines[0] == {
        "time": lines[0]["time"],
        "level": "INFO",
        "stream": "app",
        "msg": "charged",
        "module": "billing",
        "password": "[redacted]",
        "amount": 25,
    }
    assert lines[1]["level"] == "ERROR"
    assert "Traceback" in lines[1]["error"] and "card declined" in lines[1]["error"]


def test_the_environment_wins_over_the_code_and_never_turns_a_console_on(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("APP_LOG_LEVEL", "warning")
    monkeypatch.setenv("APP_LOG_FORMAT", "pretty")
    monkeypatch.setenv("APP_LOG_TIME", "off")
    out = io.StringIO()
    log = tinystore.logger("api", level="debug", console="json", env="APP", to=out)
    log.info("below")
    log.warning("slow request", ms=1200)
    assert out.getvalue() == "WARN  api  slow request  ms=1200\n"
    quiet = io.StringIO()
    tinystore.logger("api", console="off", env="APP", to=quiet).warning("kept nowhere")
    assert quiet.getvalue() == ""


def test_a_level_a_format_or_a_time_it_cannot_read_is_refused() -> None:
    for wrong in ({"console": "loud"}, {"time": "noon"}, {"level": "chatty"}):
        with pytest.raises(tinystore.InvalidError):
            tinystore.logger("app", **wrong)  # type: ignore[arg-type]
