"""The console lines every logger writes: testdata/console/console.json, which Rust and Bun print byte for byte too."""

from __future__ import annotations

import dataclasses
import io
import json
import time
from pathlib import Path
from typing import Any

import pytest

import tinystore
from tinystore._console import Line, encode_fields, hide_url_passwords, json_line, pretty_line, redactor, words
from tinystore.logger import LoggerOptions, new_store_logger

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


# contract: LOG-10
def test_a_key_a_nested_bind_gives_again_takes_the_new_value_where_the_outer_one_stood(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    for name in ("LOG_LEVEL", "LOG_FORMAT", "LOG_TIME", "FORCE_COLOR"):
        monkeypatch.delenv(name, raising=False)
    out = io.StringIO()
    http = tinystore.logger("api", console="pretty", time="off", to=out).bind(component="http", user=7)
    http.bind(component="access").info("request completed", status=200)
    http.info("served")
    assert out.getvalue().splitlines() == [
        "INFO  api  request completed  component=access user=7 status=200",
        "INFO  api  served  component=http user=7",
    ]


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


async def _printed_and_kept(**told: Any) -> tuple[list[str], list[str]]:
    """A store's logger over an append of its own, printing JSON: the messages it printed, and those it kept."""
    out, kept = io.StringIO(), list[str]()

    async def append(lines: list[Line]) -> None:
        kept.extend(line.msg or "" for line in lines)

    log, _ = new_store_logger(append, "api", LoggerOptions(console="json", to=out, **told))
    log.debug("debug")
    log.info("info")
    log.warning("warning")
    log.error("error")
    await log.flush()
    return [json.loads(line)["msg"] for line in out.getvalue().splitlines()], kept


# contract: LOG-05
async def test_each_variable_wins_over_its_own_part(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LOG_CONSOLE_LEVEL", "error")
    monkeypatch.setenv("LOG_LEVEL", "debug")
    printed, kept = await _printed_and_kept(level={"store": "warning", "console": "info"})
    assert (printed, kept) == (["error"], ["debug", "info", "warning", "error"])

    monkeypatch.delenv("LOG_CONSOLE_LEVEL")
    printed, kept = await _printed_and_kept(level={"console": "warning"})
    assert (printed, kept) == (["warning", "error"], ["debug", "info", "warning", "error"])


# contract: LOG-05
async def test_a_prefix_reads_its_log_console_level_and_a_value_it_cannot_mean_is_said_once(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("LOG_LEVEL", raising=False)
    monkeypatch.setenv("LOG_CONSOLE_LEVEL", "error")
    monkeypatch.setenv("APP_LOG_CONSOLE_LEVEL", "warn")
    assert (await _printed_and_kept(env="APP"))[0] == ["warning", "error"]

    value = f"quiet-{time.time_ns()}"
    monkeypatch.setenv("LOG_CONSOLE_LEVEL", value)
    first, _ = await _printed_and_kept()
    assert first == ["LOG_CONSOLE_LEVEL is ignored", "debug", "info", "warning", "error"]
    second, _ = await _printed_and_kept()
    assert second == ["debug", "info", "warning", "error"]


# contract: LOG-05
def test_log_console_level_sets_what_a_logger_of_the_console_alone_prints(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LOG_CONSOLE_LEVEL", "warning")
    monkeypatch.setenv("LOG_LEVEL", "debug")
    out = io.StringIO()
    log = tinystore.logger("app", console="json", to=out)
    log.info("hidden")
    log.warning("shown")
    assert [json.loads(line)["msg"] for line in out.getvalue().splitlines()] == ["shown"]


# contract: LOG-08
async def test_one_level_sets_both_what_is_kept_and_what_is_printed() -> None:
    both = await _printed_and_kept(level="warning", env=False)
    assert both == (["warning", "error"], ["warning", "error"])


# contract: LOG-08
async def test_a_console_part_alone_keeps_every_level_and_prints_from_its_own() -> None:
    quiet = await _printed_and_kept(level={"console": "warning"}, env=False)
    assert quiet == (["warning", "error"], ["debug", "info", "warning", "error"])


# contract: LOG-08
async def test_a_console_part_changes_only_what_is_printed() -> None:
    quieter = await _printed_and_kept(level={"store": "info", "console": "warning"}, env=False)
    assert quieter == (["warning", "error"], ["info", "warning", "error"])
    louder = await _printed_and_kept(level={"store": "info", "console": "debug"}, env=False)
    assert louder == (["debug", "info", "warning", "error"], ["info", "warning", "error"])


def test_a_logger_follows_the_config_log_only_while_its_level_has_no_console_part() -> None:
    out = io.StringIO()

    async def append(_: list[Line]) -> None:
        pass

    told = LoggerOptions(level="error", console="json", to=out, env=False)
    following, following_config = new_store_logger(append, "api", told)
    store_part, store_part_config = new_store_logger(append, "api", dataclasses.replace(told, level={"store": "error"}))
    apart = dataclasses.replace(told, level={"store": "error", "console": "warning"})
    own, own_config = new_store_logger(append, "api", apart)
    for config in (following_config, store_part_config, own_config):
        config.take("debug")
    following.debug("followed")
    store_part.debug("a store part alone followed")
    own.debug("own console part")
    own.warning("warned")
    printed = [json.loads(line)["msg"] for line in out.getvalue().splitlines()]
    assert printed == ["followed", "a store part alone followed", "warned"]


def test_a_level_a_format_or_a_time_it_cannot_read_is_refused() -> None:
    for wrong in ({"console": "loud"}, {"time": "noon"}, {"level": "chatty"}):
        with pytest.raises(tinystore.InvalidError):
            tinystore.logger("app", **wrong)  # type: ignore[arg-type]


def test_a_level_whose_part_is_neither_store_nor_console_or_names_no_level_is_refused() -> None:
    async def append(_: list[Line]) -> None:
        pass

    for wrong in ({"default": "info"}, {"console": "loud"}):
        with pytest.raises(tinystore.InvalidError):
            new_store_logger(append, "api", LoggerOptions(level=wrong, env=False))  # type: ignore[arg-type]
