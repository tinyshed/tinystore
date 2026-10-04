"""Configs and limiters through a real tinystore serve, and kv/testdata/config.json, which Go's config reads too."""

from __future__ import annotations

import asyncio
import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any

import pytest

import tinystore
from tinystore import fixed, from_env, secret
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
    db_url: str = secret("LAYERS_DATABASE_URL", default="")
    origins: list[str] = field(default_factory=lambda: ["localhost"])
    limits: Limits = field(default_factory=Limits)


@pytest.fixture
async def store(tmp_path: Path) -> AsyncIterator[tinystore.Store]:
    async with tinystore.open(tmp_path / "data", private=True) as opened:
        yield opened


@dataclass(frozen=True)
class Listener:
    port: int = 8080


_DEFAULT_LISTENER = Listener()


@dataclass
class FixedListener:
    listener: Listener = fixed(_DEFAULT_LISTENER)  # noqa: RUF009 -- fixed returns dataclasses.field


async def test_a_fixed_nested_dataclass_stays_fixed(store: tinystore.Store) -> None:
    config = await store.kv.config("fixed-listener", FixedListener)
    with pytest.raises(tinystore.InvalidError, match="fixed"):
        await config.update({"listener": {"port": 9090}})
    assert config.value.listener.port == 8080


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


async def test_a_config_is_its_defaults_then_each_layer_in_its_order_then_what_was_kept(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / ".env").write_text("LAYERS_PORT=3000\nLAYERS_ORIGINS=a.com, b.com\n", encoding="utf-8")
    monkeypatch.setenv("LAYERS_LIMITS_ON", "true")
    monkeypatch.setenv("LAYERS_DATABASE_URL", "postgres://secret")
    layers = ({"port": 5000, "limits": {"burst": 20}}, from_env("LAYERS", tmp_path / ".env"))
    async with tinystore.open(tmp_path / "data", private=True) as store:
        cfg = await store.kv.config("layers", Settings, *layers)
        assert cfg.value == Settings(3000, "postgres://secret", ["a.com", "b.com"], Limits(100, 20, True))
        await cfg.update({"port": 4000, "limits": {"rps": 50}})
        assert (cfg.value.port, cfg.value.limits.rps, cfg.value.limits.burst) == (4000, 50, 20)
        sources = {s.path: s for s in cfg.sources()}
        assert (sources["port"].from_, sources["limits.burst"].from_, sources["limits.on"].from_) == (
            "kept",
            "file",
            "env LAYERS_LIMITS_ON",
        )
        assert (sources["db_url"].value, sources["db_url"].from_) == ("***", "env LAYERS_DATABASE_URL")

    async with tinystore.open(tmp_path / "data", private=True) as store:
        cfg = await store.kv.config("layers", Settings, *layers)
        assert (cfg.value.port, cfg.value.limits.rps) == (4000, 50)
        await cfg.reset("port")
        assert cfg.value.port == 3000
        await cfg.reset()
        assert cfg.value.limits.rps == 100


async def test_no_variable_is_read_without_from_env_and_a_layer_after_it_goes_over_it(
    store: tinystore.Store, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("ORDER_PORT", "3000")
    none = await store.kv.config("order", Settings)
    last = await store.kv.config("order", Settings, from_env("ORDER"), {"port": 9000})
    assert (none.value.port, last.value.port) == (8080, 9000)


async def test_a_change_through_one_config_reaches_another_of_its_name(store: tinystore.Store) -> None:
    first = await store.kv.config("shared", Settings)
    second = await store.kv.config("shared", Settings)
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

    cfg = await store.kv.config("checked", Settings, validate=ports)
    with pytest.raises(tinystore.InvalidError):
        await cfg.update({"port": 70000})
    with pytest.raises(tinystore.InvalidError):
        await cfg.update({"db_url": "leaked"})
    assert cfg.value.port == 8080
    assert next(s for s in cfg.sources() if s.path == "db_url").value == "***"


@dataclass
class Db:
    url: str = secret("REQUIRED_DATABASE_URL")
    pool: int = 10


@dataclass(kw_only=True)
class Deployment:
    db: Db
    region: str
    workers: int


async def test_a_required_field_is_given_by_a_layer_or_the_config_does_not_open_naming_its_variable(
    store: tinystore.Store, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("REQUIRED_DATABASE_URL", raising=False)
    monkeypatch.setenv("REQUIRED_REGION", "eu")
    monkeypatch.setenv("REQUIRED_WORKERS", "0")
    with pytest.raises(tinystore.InvalidError, match=r"db.url is required: set REQUIRED_DATABASE_URL"):
        await store.kv.config("required", Deployment, from_env("REQUIRED"))
    with pytest.raises(tinystore.InvalidError, match=r"db.url is required"):
        await store.kv.config("required", Deployment)
    monkeypatch.setenv("REQUIRED_DATABASE_URL", "")
    with pytest.raises(tinystore.InvalidError):
        await store.kv.config("required", Deployment, from_env("REQUIRED"))

    monkeypatch.setenv("REQUIRED_DATABASE_URL", "postgres://secret")
    cfg = await store.kv.config("required", Deployment, from_env("REQUIRED"))
    assert cfg.value == Deployment(db=Db("postgres://secret", 10), region="eu", workers=0)
    with pytest.raises(tinystore.InvalidError):
        await cfg.update({"region": ""})
    await cfg.update({"region": "us"})
    assert cfg.value.region == "us"


async def test_a_kept_value_that_no_longer_fits_or_names_a_secret_is_left_out_and_named(
    store: tinystore.Store,
) -> None:
    @dataclass
    class Older:
        port: int = 8080
        token: str = ""

    @dataclass
    class Newer:
        port: str = "eighty"
        token: str = secret("CHANGED_TOKEN", default="safe")

    older = await store.kv.config("changed", Older)
    await older.update({"port": 4000, "token": "leaked"})
    newer = await store.kv.config("changed", Newer)
    assert (newer.value.port, newer.value.token) == ("eighty", "safe")
    sources = {s.path: s for s in newer.sources()}
    assert sources["port"].ignored is not None and "is no string" in sources["port"].ignored
    assert sources["token"].ignored == "the field is a secret"


async def test_a_variable_that_is_not_its_fields_kind_is_refused_naming_it(
    store: tinystore.Store, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("BAD_PORT", "abc")
    with pytest.raises(tinystore.InvalidError, match="BAD_PORT"):
        await store.kv.config("bad", Settings, from_env("BAD"))


async def test_every_variable_that_does_not_read_is_said_at_once(
    store: tinystore.Store, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("EVERY_PORT", "eighty")
    monkeypatch.setenv("EVERY_LIMITS_ON", "yes")
    with pytest.raises(tinystore.InvalidError) as caught:
        await store.kv.config("every", Settings, from_env("EVERY"))
    assert "EVERY_PORT: 'eighty' is no integer" in str(caught.value)
    assert "EVERY_LIMITS_ON: 'yes' is not true or false" in str(caught.value)


@dataclass
class Served:
    addr: str = fixed(":8080")
    instance: str = "dashbin"


async def test_a_fixed_field_refuses_update_ignores_what_was_kept_and_says_where_it_came_from(
    store: tinystore.Store, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("FIXED_ADDR", ":9090")
    config = await store.kv.config("fixed", Served, from_env("FIXED"))
    with pytest.raises(tinystore.InvalidError, match="addr is fixed"):
        await config.update({"addr": ":1"})
    await config.update({"instance": "eu-1"})
    assert config.value == Served(addr=":9090", instance="eu-1")
    assert config.sources()[0] == tinystore.Source(path="addr", value='":9090"', from_="env FIXED_ADDR")


@dataclass
class Mail:
    password: str = secret()


async def test_a_variables_file_is_read_when_name_file_names_it_and_both_set_is_refused(
    store: tinystore.Store, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    secret_file = tmp_path / "smtp"
    secret_file.write_text("hunter2\n", encoding="utf-8")
    monkeypatch.setenv("FILED_PASSWORD_FILE", str(secret_file))
    config = await store.kv.config("filed", Mail, from_env("FILED"))
    assert config.value.password == "hunter2"
    assert config.sources()[0].from_ == "env FILED_PASSWORD_FILE"
    monkeypatch.setenv("FILED_PASSWORD", "other")
    with pytest.raises(tinystore.InvalidError, match="both FILED_PASSWORD and FILED_PASSWORD_FILE are set"):
        await store.kv.config("filed", Mail, from_env("FILED"))


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
