"""Configs as docs/guides/config/README.md has them, through each way a store is reached.

A declaration in layers, each declaring again what came before, the rules of
one value the core checks, live settings whose change every handle sees on
one stream, secrets that print as ***, and the names and spans every language
reads alike (testdata/config/env.json).
"""

from __future__ import annotations

import io
import json
import logging
import pickle
from dataclasses import dataclass, field
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import TYPE_CHECKING, Annotated, Any, Literal, cast

import pytest

import tinystore
from tinystore import Entries, Given, Items, Origin, Range, Secret, chosen_by, entries, env, file, live, secret
from tinystore._time import ms, spelled
from tinystore.config import env_name
from ways import until

if TYPE_CHECKING:
    from collections.abc import Callable

    from ways import Way

VECTORS = json.loads((Path(__file__).parents[3] / "testdata" / "config" / "env.json").read_text())


def test_a_settings_variable_is_its_prefix_and_its_path_in_upper_snake_case() -> None:
    for prefix, path, want in VECTORS["names"]:
        assert env_name(prefix, path) == want, path


def test_a_span_reads_and_spells_back_as_the_core_reads_one() -> None:
    for text, want in VECTORS["spans"]:
        if want is None:
            with pytest.raises(tinystore.InvalidError):
                ms(text)
        else:
            assert ms(text) == want, text
            assert ms(spelled(want)) == want, text


def test_a_secret_prints_as_stars_wherever_it_goes_and_reveal_gives_it() -> None:
    url = Secret("postgres://user:password@host")

    @dataclass
    class Db:
        url: Secret

    shown = [str(url), repr(url), f"{url}", f"{url!r}", "%s" % url, repr(Db(url)), repr({"url": url}), repr([url])]  # noqa: UP031
    for text in shown:
        assert "password" not in text
    with pytest.raises(TypeError):
        json.dumps({"url": url})
    with pytest.raises(TypeError):
        pickle.dumps(url)
    assert "password" not in str(tinystore.InvalidError(f"bad {url}"))
    assert url.reveal() == "postgres://user:password@host"


def test_an_origin_of_a_kind_from_a_newer_server_keeps_its_kind_which_is_also_its_text() -> None:
    origin = Origin(cast("Any", "rule"))
    assert (origin.kind, str(origin), f"{origin}") == ("rule", "rule", "rule")


def test_a_mark_that_means_nothing_is_refused_where_it_is_written() -> None:
    with pytest.raises(tinystore.InvalidError, match="a secret is never live"):
        live(secret())
    with pytest.raises(tinystore.InvalidError, match="live twice"):
        live(live(1))


@dataclass
class Db:
    url: Secret = secret()  # noqa: RUF009
    pool: int = 10


@dataclass
class Limits:
    rps: Annotated[int, Range(min=0)] = live(100)
    burst: int = live(10)


@dataclass
class Settings:
    port: Annotated[int, Range(1, 65535)] = 8080
    level: Literal["info", "debug", "warn", "error"] = "info"
    timeout: timedelta = timedelta(seconds=30)
    origins: list[str] = field(default_factory=lambda: ["localhost"])
    db: Db = field(default_factory=Db)
    limits: Limits = field(default_factory=Limits)


async def test_a_config_is_its_declaration_then_each_layer_in_the_order_written(
    store: tinystore.Store, directory: Path
) -> None:
    yaml = directory / "app.yaml"
    yaml.write_text("port: 8000\nlimits:\n  rps: 50\n")
    config = await store.config(
        "ordered",
        [
            Settings,
            {"level": "debug"},
            None,
            file(yaml),
            env("APP", from_={"APP_PORT": "9000", "APP_DB_URL": "postgres://local"}),
        ],
    )
    value = config.value
    assert (value.port, value.limits.rps, value.level) == (9000, 50, "debug")
    assert value.timeout == timedelta(seconds=30)
    assert value.db.url.reveal() == "postgres://local"
    assert value.db.pool == 10
    lines = {line.path: line for line in config.explain()}
    port = lines["port"]
    assert (port.path, port.value, port.from_, port.over) == (
        "port",
        "9000",
        "APP_PORT",
        f"8000 from {yaml}, 8080 from the declaration",
    )
    assert lines["level"].from_ == "layer 2"
    assert lines["db.url"].value == "***"


# contract: CFG-09, CFG-06
async def test_explain_gives_every_value_a_setting_was_given_newest_first_with_where_each_came_from(
    store: tinystore.Store, directory: Path
) -> None:
    yaml = directory / "given.yaml"
    yaml.write_text("port: 8000\n")
    dotenv = directory / "given.env"
    dotenv.write_text("APP_TIMEOUT=1m\n")
    secret_file = directory / "db-url"
    secret_file.write_text("postgres://user:password@host\n")
    variables = {"APP_PORT": "9000", "APP_DB_URL_FILE": str(secret_file)}
    config = await store.config(
        "given", [Settings, {"level": "debug"}, file(yaml), env("APP", files=[dotenv], from_=variables)]
    )
    before = datetime.now(UTC)
    await config.update({"limits": {"rps": 200}})
    lines = {line.path: line for line in config.explain()}
    assert lines["port"].given == (
        Given("9000", Origin("env", variable="APP_PORT")),
        Given("8000", Origin("file", path=str(yaml))),
        Given("8080", Origin("declaration")),
    )
    assert lines["level"].given == (Given('"debug"', Origin("layer", place=2)), Given('"info"', Origin("declaration")))
    assert lines["timeout"].given == (
        Given("1m", Origin("env", variable="APP_TIMEOUT", dotenv=str(dotenv))),
        Given("30s", Origin("declaration")),
    )
    assert lines["db.url"].given == (Given("***", Origin("env", variable="APP_DB_URL_FILE", file=str(secret_file))),)
    change, declared = lines["limits.rps"].given
    assert (change.from_.kind, change.from_.version) == ("change", config.version)
    assert change.from_.at is not None
    assert before - timedelta(seconds=1) <= change.from_.at <= datetime.now(UTC) + timedelta(seconds=1)
    assert declared == Given("100", Origin("declaration"))
    for line in config.explain():
        own, *over = line.given
        assert (own.value, str(own.from_)) == (line.value, line.from_)
        assert ", ".join(f"{each.value} from {each.from_}" for each in over) == line.over
    assert "given=" not in repr(lines["port"]), "print(line) prints the four columns it printed before"
    assert "password" not in repr(lines["db.url"].given)


async def test_a_rule_of_one_value_is_the_cores_and_a_refusal_names_the_layer_that_gave_it(
    store: tinystore.Store,
) -> None:
    async def opening(given: dict[str, str]) -> tinystore.Config[Settings]:
        return await store.config("rules", [Settings, env("APP", from_={"APP_DB_URL": "u", **given})])

    with pytest.raises(tinystore.InvalidError) as refused:
        await opening({"APP_PORT": "70000", "APP_LEVEL": "verbose"})
    assert "APP_PORT: 70000 is above 65535" in str(refused.value)
    assert 'APP_LEVEL: "verbose" is not one of info, debug, warn, error' in str(refused.value)
    config = await opening({"APP_TIMEOUT": "1m30s", "APP_ORIGINS": "a.com, b.com"})
    assert config.value.timeout == timedelta(minutes=1, seconds=30)
    assert config.value.origins == ["a.com", "b.com"]


@dataclass
class Postgres:
    host: str = "localhost"
    port: int = 5432
    password: Secret = secret()  # noqa: RUF009
    timeout: timedelta = timedelta(seconds=30)


@dataclass
class Site:
    plugin: str = "postgres"
    settings: Postgres = field(default_factory=Postgres)


@dataclass
class Dashbin:
    instance: Entries[Site] = entries({"local": Site(settings=Postgres(password=Secret("dev-only")))})  # noqa: RUF009


# contract: CFG-11, CFG-06
async def test_an_entry_takes_each_field_from_the_layers_and_a_variable_one_field_of_an_entry_a_layer_made(
    store: tinystore.Store, directory: Path
) -> None:
    toml = directory / "dashbin.toml"
    toml.write_text('[instance.aimee.settings]\nhost = "127.0.0.1"\nport = 15432\n')
    secret_file = directory / "pg"
    secret_file.write_text("of-a-file\n")
    variables = {
        "APP_INSTANCE_AIMEE_SETTINGS_PASSWORD": "of-a-variable",
        "APP_INSTANCE_KAZAN_SETTINGS_PASSWORD_FILE": str(secret_file),
        "APP_INSTANCE": '{"aimee":{"settings":{"port":6432}}}',
    }
    kazan = {"instance": {"kazan": {"settings": {"host": "10.0.0.9"}}}}
    config = await store.config("dashbin", [Dashbin, file(toml), kazan, env("APP", from_=variables)])
    aimee = config.value.instance["aimee"]
    shown = (aimee.plugin, aimee.settings.host, aimee.settings.port, aimee.settings.timeout)
    assert shown == ("postgres", "127.0.0.1", 6432, timedelta(seconds=30))
    assert aimee.settings.password.reveal() == "of-a-variable"
    assert config.value.instance["kazan"].settings.password.reveal() == "of-a-file"
    assert config.value.instance["local"].settings.password.reveal() == "dev-only"
    assert isinstance(config.value.instance, Entries)
    lines = {line.path: line for line in config.explain()}
    assert lines["instance.aimee.settings.password"].value == "***"
    assert lines["instance.aimee.settings.port"].from_ == "APP_INSTANCE"
    assert lines["instance.aimee.settings.port"].over == f"15432 from {toml}, 5432 from the declaration"
    assert lines["instance.kazan.settings.host"].from_ == "layer 3"
    assert "of-a-variable" not in repr(config.value)
    assert "of-a-file" not in repr(list(config.explain()))

    @dataclass
    class Texts:
        instance: Entries[str] = entries()  # noqa: RUF009

    with pytest.raises(tinystore.InvalidError, match="an entry is a dataclass of settings"):
        await store.config("texts", [Texts])


@dataclass(kw_only=True)
class Pg:
    host: str = "localhost"
    port: Annotated[int, Range(1, 65535)] = 5432
    database: str
    password: Secret = secret()  # noqa: RUF009


@dataclass(kw_only=True)
class Mysql:
    host: str = "localhost"
    port: int = 3306
    password: Secret = secret()  # noqa: RUF009


@dataclass
class Stripe:
    key: Secret = secret()  # noqa: RUF009


@dataclass(kw_only=True)
class Instance:
    plugin: str
    settings: Pg | Mysql | Stripe = chosen_by("plugin", {"postgres": Pg, "mysql": Mysql, "stripe": Stripe})  # noqa: RUF009


@dataclass
class Plugins:
    instance: Entries[Instance] = entries()  # noqa: RUF009


def _aimee(settings: str) -> str:
    return f'[instance.aimee]\nplugin = "postgres"\n[instance.aimee.settings]\n{settings}\n'


# contract: CFG-14, CFG-13, CFG-06
async def test_an_entrys_settings_are_read_checked_masked_and_named_by_the_group_its_plugin_chooses(
    store: tinystore.Store, directory: Path
) -> None:
    misspelt = directory / "misspelt.toml"
    misspelt.write_text(_aimee('hots = "127.0.0.1"\ndatabase = "aimee"\npassword = "s3cret"'))
    with pytest.raises(tinystore.InvalidError) as hots:
        await store.config("plugins", [Plugins, file(misspelt)])
    assert "instance.aimee.settings.hots: no such setting; did you mean instance.aimee.settings.host?" in str(
        hots.value
    )

    toml = directory / "plugins.toml"
    toml.write_text(_aimee('host = "127.0.0.1"\ndatabase = "aimee"\npassword = "s3cret"'))
    port = {"DASHBIN_INSTANCE_AIMEE_SETTINGS_PORT": "15432"}
    config = await store.config("plugins", [Plugins, file(toml), env("DASHBIN", from_=port)])
    settings = config.value.instance["aimee"].settings
    assert isinstance(settings, Pg)
    assert (settings.host, settings.port, settings.database) == ("127.0.0.1", 15432, "aimee")
    assert settings.password.reveal() == "s3cret"
    lines = {line.path: line for line in config.explain()}
    assert lines["instance.aimee.settings.password"].value == "***"
    assert lines["instance.aimee.settings.port"].from_ == "DASHBIN_INSTANCE_AIMEE_SETTINGS_PORT"
    assert "s3cret" not in repr(list(config.explain()))
    defaulted = await store.config("plugins-default", [Plugins, file(toml)])
    assert cast("Pg", defaulted.value.instance["aimee"].settings).port == 5432

    typo = {"DASHBIN_INSTANCE_AIMEE_PLUGIN": "postgress"}
    with pytest.raises(tinystore.InvalidError) as refused:
        await store.config("plugins", [Plugins, file(toml), env("DASHBIN", from_=typo)])
    named = '"postgress" is not one of mysql, postgres, stripe; did you mean postgres?'
    assert f"DASHBIN_INSTANCE_AIMEE_PLUGIN: {named}" in str(refused.value)

    mysql = {"DASHBIN_INSTANCE_AIMEE_PLUGIN": "mysql", "DASHBIN_INSTANCE_AIMEE_SETTINGS_PASSWORD": "p"}
    switched = await store.config("plugins-switched", [Plugins, file(toml), env("DASHBIN", from_=mysql)])
    chosen = switched.value.instance["aimee"].settings
    assert isinstance(chosen, Mysql)
    assert (chosen.host, chosen.port) == ("localhost", 3306)
    assert switched.explain().notes == [
        f"instance.aimee.settings: what {toml} gave as postgres is dropped: instance.aimee.plugin is mysql, "
        "from DASHBIN_INSTANCE_AIMEE_PLUGIN"
    ]


@dataclass(kw_only=True)
class Needed:
    region: str
    workers: Annotated[int, Range(min=1)]
    token: Secret = secret()  # noqa: RUF009


async def test_a_required_setting_is_given_by_a_layer_or_the_config_does_not_open_naming_what_would(
    store: tinystore.Store,
) -> None:
    async def opening(given: dict[str, str]) -> tinystore.Config[Needed]:
        return await store.config("required", [Needed, env("APP", from_=given)])

    with pytest.raises(tinystore.InvalidError) as refused:
        await opening({"APP_REGION": ""})
    assert "region is required: set APP_REGION" in str(refused.value)
    assert "token is required: set APP_TOKEN" in str(refused.value)
    config = await opening({"APP_REGION": "eu", "APP_WORKERS": "4", "APP_TOKEN": "t"})
    assert (config.value.region, config.value.workers, config.value.token.reveal()) == ("eu", 4, "t")


@dataclass
class TrustedDb:
    """A database on trust or peer auth, which takes no password."""

    host: str = "localhost"
    password: Secret | None = None


@dataclass
class Trusted:
    db: TrustedDb = field(default_factory=TrustedDb)
    port: Annotated[int, Range(min=1)] | None = None
    region: str | None = None


# contract: CFG-15, CFG-06
async def test_a_setting_with_no_value_is_none_until_a_layer_gives_it_a_secret_too_and_explain_says_unset(
    store: tinystore.Store, directory: Path
) -> None:
    toml = directory / "trusted.toml"
    toml.write_text('[db]\nhost = "db"\n')
    config = await store.config("trusted", [Trusted, file(toml), env("APP", from_={})])
    assert (config.value.db.password, config.value.port, config.value.region) == (None, None, None)
    password = next(line for line in config.explain() if line.path == "db.password")
    assert (password.value, password.from_) == ("unset", "the declaration")

    toml.write_text('[db]\npassword = "s3cret"\n')
    given = await store.config(
        "trusted", [Trusted, file(toml), env("APP", from_={"APP_PORT": "5432", "APP_REGION": "1"})]
    )
    assert given.value.db.password is not None
    assert (given.value.db.password.reveal(), given.value.port, given.value.region) == ("s3cret", 5432, "1")
    assert "s3cret" not in repr(given.explain())
    with pytest.raises(tinystore.InvalidError, match="APP_PORT: 0 is below 1"):
        await store.config("trusted", [Trusted, env("APP", from_={"APP_PORT": "0"})])


@dataclass
class Maybe:
    db: TrustedDb | None = None


@dataclass
class CommittedDb:
    host: str = "localhost"
    password: Secret = secret()  # noqa: RUF009


@dataclass
class Committed:
    db: CommittedDb = field(default_factory=CommittedDb)


# contract: CFG-16, CFG-06
async def test_a_file_that_refuses_secrets_names_the_variable_that_gives_one_and_never_the_secret(
    store: tinystore.Store, directory: Path
) -> None:
    toml = directory / "committed.toml"
    toml.write_text('[db]\nhost = "db"\npassword = "s3cret"\n')

    async def opening(given: dict[str, str]) -> tinystore.Config[Committed]:
        return await store.config("committed", [Committed, file(toml, secrets=False), env("APP", from_=given)])

    with pytest.raises(tinystore.InvalidError) as refused:
        await opening({})
    said = f"{toml}: db.password: a secret is not read from {toml}; set APP_DB_PASSWORD or APP_DB_PASSWORD_FILE"
    assert said in str(refused.value) and "s3cret" not in str(refused.value)
    toml.write_text('[db]\nhost = "db"\n')
    with pytest.raises(tinystore.InvalidError) as missing:
        await opening({})
    assert "db.password is required: set APP_DB_PASSWORD or APP_DB_PASSWORD_FILE" in str(missing.value)
    assert "db.password in" not in str(missing.value)
    config = await opening({"APP_DB_PASSWORD": "s3cret"})
    assert (config.value.db.host, config.value.db.password.reveal()) == ("db", "s3cret")


@dataclass
class Port:
    port: int = 8080


async def test_a_file_absent_is_refused_unless_optional_and_a_key_no_setting_has_is_named(
    store: tinystore.Store, directory: Path
) -> None:
    missing = directory / "missing.toml"
    with pytest.raises(tinystore.InvalidError, match="no such file"):
        await store.config("files", [Port, file(missing)])
    config = await store.config("files", [Port, file(missing, optional=True)])
    assert config.explain().notes == [f"{missing}: absent"]
    jsonc = directory / "app.jsonc"
    jsonc.write_text('{\n  // a comment\n  "prot": 1,\n}\n')
    with pytest.raises(tinystore.InvalidError, match="prot: no such setting"):
        await store.config("files", [Port, file(jsonc)])
    with pytest.raises(tinystore.InvalidError, match="prot"):
        await store.config("files", [Port, {"prot": 1}])


@dataclass
class OtherLimits:
    rps: int = live(1)
    burst: int = live(1)


@dataclass
class Other:
    limits: OtherLimits = field(default_factory=OtherLimits)


async def test_a_live_setting_changes_for_every_handle_and_reset_gives_the_layers_back(
    store: tinystore.Store,
) -> None:
    layers = [Settings, env("APP", from_={"APP_DB_URL": "u", "APP_LIMITS_RPS": "1000"})]
    config = await store.config("live", layers)
    assert await store.config("live", layers) is config
    other = await store.config("live", [Other, env("APP", from_={})])
    await config.update({"limits": {"rps": 200}})
    assert config.value.limits.rps == 200
    [rps] = [line for line in config.explain() if line.path == "limits.rps"]
    assert rps.from_.startswith("a change, version ")
    assert rps.over == "1000 from APP_LIMITS_RPS, 100 from the declaration"
    await until(lambda: other.value.limits.rps == 200)
    with pytest.raises(tinystore.InvalidError, match="port is not live"):
        await config.update({"port": 1})
    await config.reset("limits.rps")
    assert config.value.limits.rps == 1000
    await until(lambda: other.value.limits.rps == 1)
    assert other.value.limits.rps == 1


@dataclass
class Two:
    a: int = live(1)
    b: int = live(2)


@dataclass
class Three:
    a: int = live(1)
    b: int = live(2)
    c: int = 3


async def test_if_version_refuses_a_change_made_since_and_two_changes_of_different_settings_both_stay(
    store: tinystore.Store,
) -> None:
    first = await store.config("versions", [Two])
    second = await store.config("versions", [Three])
    shown = first.version
    await first.update({"a": 10})
    await second.update({"b": 20})
    await until(lambda: first.value.b == 20 and second.value.a == 10)
    with pytest.raises(tinystore.ConflictError):
        await first.update({"a": 30}, if_version=shown)
    await first.update({"a": 40}, if_version=first.version)
    assert first.value.a == 40


@dataclass
class Rate:
    rps: int = live(100)


async def test_a_watch_gives_the_settings_now_then_each_change_until_the_store_closes(store: tinystore.Store) -> None:
    config = await store.config("watched", [Rate])
    seen: list[int] = []
    watching = config.watch()
    seen.append((await anext(watching)).rps)
    await config.update({"rps": 200})
    seen.append((await anext(watching)).rps)
    await config.update({"rps": 300})
    await config.update({"rps": 400})
    seen.append((await anext(watching)).rps)
    assert seen == [100, 200, 400]
    await watching.aclose()  # pyright: ignore[reportAttributeAccessIssue, reportUnknownMemberType]


async def test_validate_refuses_a_value_at_open_and_a_change_it_refuses_keeps_nothing(store: tinystore.Store) -> None:
    def at_most(most: int) -> Callable[[Rate], None]:
        def check(value: Rate) -> None:
            if value.rps > most:
                raise ValueError(f"rps past {most}")

        return check

    @dataclass
    class Fast:
        rps: int = live(5000)

    with pytest.raises(tinystore.InvalidError, match="rps past 1000"):
        await store.config("validated", [Fast], validate=at_most(1000))  # pyright: ignore[reportArgumentType, reportCallIssue]
    config = await store.config("validated", [Rate], validate=at_most(1000))
    with pytest.raises(tinystore.InvalidError, match="rps past 1000"):
        await config.update({"rps": 2000})
    assert config.value.rps == 100

    async def not_seven(value: Rate) -> None:
        if value.rps == 7:
            raise tinystore.InvalidError("not 7")

    checked = await store.config("checked", [Rate], validate=not_seven)
    with pytest.raises(tinystore.InvalidError, match="not 7"):
        await checked.update({"rps": 7})


async def test_a_stores_logger_takes_its_level_from_the_config_log_which_a_change_sets_and_reset_gives_back(
    store: tinystore.Store, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("APP_LOG_LEVEL", "warning")
    written = io.StringIO()
    log = store.logger("logged", level="debug", console="json", to=written, env="APP")
    probes = 0

    def printed(message: str) -> bool:
        return f'"msg":"{message}"' in written.getvalue()

    def probed() -> bool:
        nonlocal probes
        probes += 1
        log.debug(f"probe {probes}")
        return printed(f"probe {probes}")

    assert not probed()

    @dataclass
    class Log:
        level: str = live("info")

    settings = await store.config("log", [Log])
    await settings.update({"level": "debug"})
    await until(probed)
    assert printed(f"probe {probes}")
    with pytest.raises(tinystore.InvalidError, match='"verbose" is not one of debug, trace, info, warn, error'):
        await settings.update({"level": "verbose"})
    await settings.reset("level")
    await until(lambda: not probed())
    assert not printed(f"probe {probes}")
    log.warning("warned")
    assert printed("warned")
    assert logging.WARNING > logging.DEBUG


# contract: LOG-07
async def test_a_handler_takes_its_level_from_the_config_log_as_the_stores_logger_does(
    store: tinystore.Store,
) -> None:
    written = io.StringIO()
    handler = store.log_handler("handled", level="warning", console="json", to=written, env=False)
    worker = logging.getLogger("tinystore-test-config-handler")
    worker.propagate = False
    worker.setLevel(logging.DEBUG)
    worker.addHandler(handler)
    probes = 0

    def probed() -> bool:
        nonlocal probes
        probes += 1
        worker.debug(f"probe {probes}")
        return f'"msg":"probe {probes}"' in written.getvalue()

    try:
        assert not probed()

        @dataclass
        class Log:
            level: str = live("info")

        settings = await store.config("log", [Log])
        try:
            await settings.update({"level": "debug"})
            await until(probed)
            assert f'"msg":"probe {probes}"' in written.getvalue()
        finally:
            await settings.reset("level")
    finally:
        worker.removeHandler(handler)


@dataclass
class Upstream:
    host: str
    port: int = 80


@dataclass
class Lists:
    hosts: Annotated[list[str], Items(max=2)] = field(default_factory=lambda: ["localhost"])
    upstreams: list[Upstream] = field(default_factory=list[Upstream])


async def test_a_list_checks_each_item_and_a_list_of_groups_fills_its_items_defaults(store: tinystore.Store) -> None:
    config = await store.config("lists", [Lists, env("APP", from_={"APP_UPSTREAMS": '[{"host":"a.com"}]'})])
    assert config.value.upstreams == [Upstream("a.com", 80)]
    assert config.value.hosts == ["localhost"]
    with pytest.raises(tinystore.InvalidError, match="APP_HOSTS: 3 items is above 2 items"):
        await store.config("lists", [Lists, env("APP", from_={"APP_HOSTS": "a,b,c"})])


async def test_a_declaration_the_core_cannot_keep_is_refused_where_it_is_written(store: tinystore.Store) -> None:
    @dataclass
    class Unioned:
        region: int | str = 0

    @dataclass
    class Plain:
        url: str = secret()  # pyright: ignore[reportAssignmentType]

    with pytest.raises(tinystore.InvalidError, match=r"one type, and X \| None = None is one with no value"):
        await store.config("refused", [Unioned])
    with pytest.raises(tinystore.InvalidError, match="a group is never None"):
        await store.config("refused", [Maybe])
    with pytest.raises(tinystore.InvalidError, match="read as Secret"):
        await store.config("refused", [Plain])
    with pytest.raises(tinystore.InvalidError, match="declaration"):
        await store.config("refused", [{"port": 1}])


async def test_a_store_closed_as_its_config_begins_to_follow_lets_go_of_what_served_it_at_once(
    way: Way, directory: Path
) -> None:
    import time

    own = directory / "own"
    own.mkdir()
    closing = await way.open(own)
    await closing.config("app", [Port])
    started = time.monotonic()
    await closing.close()
    await way.leave(own)
    assert time.monotonic() - started < 3
