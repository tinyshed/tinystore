"""Configs and limiters through a real tinystore serve, and kv/testdata/config.json, which Go's config reads too."""

from __future__ import annotations

import asyncio
import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any

import pytest

import tinystore
from tinystore.config import env_name, parse_dotenv

if TYPE_CHECKING:
    from collections.abc import AsyncIterator

VECTORS = json.loads((Path(__file__).parents[3] / "kv" / "testdata" / "config.json").read_text(encoding="utf-8"))


@dataclass
class Limits:
    rps: int = 100
    burst: int = 10
    on: bool = False


@dataclass
class Settings:
    port: int = 8080
    db_url: str = ""
    origins: list[str] = field(default_factory=lambda: ["localhost"])
    limits: Limits = field(default_factory=Limits)


@pytest.fixture
async def store(tmp_path: Path) -> AsyncIterator[tinystore.Store]:
    async with tinystore.open(tmp_path / "data", private=True) as opened:
        yield opened


@pytest.mark.parametrize(("prefix", "path", "name"), VECTORS["names"])
def test_a_variable_is_named_as_the_vectors_say(prefix: str, path: str, name: str) -> None:
    assert env_name(prefix, path) == name


@pytest.mark.parametrize("vector", VECTORS["dotenv"], ids=[v["name"] for v in VECTORS["dotenv"]])
def test_a_dotenv_file_reads_as_the_vectors_say(vector: dict[str, Any]) -> None:
    if "refused" in vector:
        with pytest.raises(tinystore.InvalidError, match=f"line {vector['refused']}:"):
            parse_dotenv(vector["text"])
    else:
        assert parse_dotenv(vector["text"]) == vector["want"]


async def test_a_config_is_its_defaults_then_its_file_then_its_environment_then_what_was_kept(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / ".env").write_text("LAYERS_PORT=3000\nLAYERS_ORIGINS=a.com, b.com\n", encoding="utf-8")
    monkeypatch.setenv("LAYERS_LIMITS_ON", "true")
    options: dict[str, Any] = {"prefix": "LAYERS", "env_file": tmp_path / ".env", "file": {"limits": {"burst": 20}}}
    async with tinystore.open(tmp_path / "data", private=True) as store:
        cfg = await store.kv.config("layers", Settings, **options)
        assert cfg.value == Settings(3000, "", ["a.com", "b.com"], Limits(100, 20, True))
        await cfg.update({"port": 4000, "limits": {"rps": 50}})
        assert (cfg.value.port, cfg.value.limits.rps, cfg.value.limits.burst) == (4000, 50, 20)
        sources = {s.path: s for s in cfg.sources()}
        assert (sources["port"].from_, sources["limits.burst"].from_, sources["limits.on"].from_) == (
            "kept",
            "file",
            "env LAYERS_LIMITS_ON",
        )

    async with tinystore.open(tmp_path / "data", private=True) as store:
        cfg = await store.kv.config("layers", Settings, **options)
        assert (cfg.value.port, cfg.value.limits.rps) == (4000, 50)
        await cfg.reset("port")
        assert cfg.value.port == 3000
        await cfg.reset()
        assert cfg.value.limits.rps == 100


async def test_a_change_through_one_config_reaches_another_of_its_name(store: tinystore.Store) -> None:
    first = await store.kv.config("shared", Settings, env=False)
    second = await store.kv.config("shared", Settings, env=False)
    seen: list[int] = []
    second.watch(lambda c: seen.append(c.port))
    await first.update({"port": 9090})
    for _ in range(100):
        if second.value.port == 9090:
            break
        await asyncio.sleep(0.01)
    assert second.value.port == 9090
    assert seen == [8080, 9090]


async def test_a_change_validate_refuses_or_one_of_a_secret_keeps_nothing(store: tinystore.Store) -> None:
    def ports(settings: Settings) -> None:
        if not 0 < settings.port < 65536:
            raise ValueError(f"port {settings.port} is no port")

    cfg = await store.kv.config("checked", Settings, env=False, secret=["db_url"], validate=ports)
    with pytest.raises(tinystore.InvalidError):
        await cfg.update({"port": 70000})
    with pytest.raises(tinystore.InvalidError):
        await cfg.update({"db_url": "leaked"})
    assert cfg.value.port == 8080
    assert next(s for s in cfg.sources() if s.path == "db_url").value == "***"


async def test_a_kept_value_that_no_longer_fits_is_left_out_and_named(store: tinystore.Store) -> None:
    @dataclass
    class Renamed:
        port: str = "eighty"

    older = await store.kv.config("changed", Settings, env=False)
    await older.update({"port": 4000})
    newer = await store.kv.config("changed", Renamed, env=False)
    assert newer.value.port == "eighty"
    ignored = next(s for s in newer.sources() if s.path == "port").ignored
    assert ignored is not None and "is no string" in ignored


async def test_a_variable_that_is_not_its_fields_kind_is_refused_naming_it(
    store: tinystore.Store, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("BAD_PORT", "abc")
    with pytest.raises(tinystore.InvalidError, match="BAD_PORT"):
        await store.kv.config("bad", Settings, prefix="BAD")


async def test_a_limiter_lets_its_burst_through_then_says_how_long_to_wait(store: tinystore.Store) -> None:
    limit = store.kv.limiter("api", rate="2/m")
    tenant = limit.of("tenant-7")
    assert await tenant.allow("user-1") == tinystore.Allowance(True, 1, 0)
    assert await tenant.allow("user-1") == tinystore.Allowance(True, 0, 0)
    ok, left, retry_after = await tenant.allow("user-1")
    assert not ok and left == 0 and 29 < retry_after <= 30
    assert (await limit.allow("user-1")).ok
    with pytest.raises(tinystore.InvalidError):
        await tenant.allow("user-2", 3)
    with pytest.raises(tinystore.InvalidError):
        store.kv.limiter("bad", rate="fast")
