"""Every query of testdata/sql/queries.json, built with this SDK's builder from its steps.

The text and the values it sends, which every SDK's builder must write
alike. A runner that records what it is given stands for a database.
"""

from __future__ import annotations

import json
import math
from pathlib import Path
from typing import TYPE_CHECKING, Any, cast

import pytest

import tinystore
from tinystore import Sql, sql
from tinystore.query import Query, Table, literal_rows, names_of
from tinystore.sql import Answered, Done

if TYPE_CHECKING:
    from collections.abc import AsyncIterator

VECTORS: list[dict[str, Any]] = json.loads(
    (Path(__file__).parents[3] / "testdata" / "sql" / "queries.json").read_text(encoding="utf-8")
)["queries"]


class Recorder:
    """A runner that keeps the last statement it was given and answers nothing."""

    describe = "sql vectors"

    def __init__(self) -> None:
        self.last: Sql | None = None

    async def rows(self, statement: Sql, want: str) -> Answered:
        self.last = statement
        return Answered([], [])

    async def exec_piece(self, statement: Sql) -> Done:
        self.last = statement
        return Done(0, 0)

    def each_piece(self, statement: Sql) -> AsyncIterator[dict[str, Any]] | None:
        return None


def condition_of(given: dict[str, Any]) -> Any:
    if "equal" in given:
        return given["equal"]
    if "sql" in given:
        text, *values = given["sql"]
        return sql(text, *values)
    if "or" in given:
        return sql.or_(*(condition_of(each) for each in given["or"]))
    if "and" in given:
        return sql.and_(*(condition_of(each) for each in given["and"]))
    if "has" in given:
        name, value = given["has"]
        return sql.has(name, value)
    raise AssertionError(f"no condition {given}")


def applied(query: Query[Any], step: dict[str, Any]) -> Query[Any]:
    ((verb, argument),) = step.items()
    match verb:
        case "where":
            return query.where(condition_of(argument))
        case "having":
            return query.having(condition_of(argument))
        case "select":
            return query.select(argument)
        case "join" | "leftJoin":
            table, on = argument
            joined = query.join if verb == "join" else query.left_join
            return joined(table, on if isinstance(on, str) else condition_of(on))
        case "groupBy":
            return query.group_by(*argument)
        case "orderBy":
            return query.order_by(*argument)
        case "limit":
            return query.limit(argument)
        case "offset":
            return query.offset(argument)
        case "include":
            name, included = argument
            inner: Query[Any] = Table(Recorder(), included["table"])
            for each in included["steps"]:
                inner = applied(inner, each)
            return query.include(name, inner)
        case _:
            raise AssertionError(f"no step {verb}")


def kept(value: Any) -> Any:
    """A value as SQLite keeps it, which is what the vectors say."""
    return int(value) if isinstance(value, bool) else value


def written(pairs: list[list[Any]]) -> dict[str, Any]:
    def made(value: Any) -> Any:
        if isinstance(value, dict) and "sql" in value:
            return condition_of(cast("dict[str, Any]", value))
        return cast("Any", value)

    return {name: made(value) for name, value in pairs}


@pytest.mark.parametrize("vector", VECTORS, ids=[v["name"] for v in VECTORS])
async def test_a_query_vector(vector: dict[str, Any]) -> None:
    recorder = Recorder()
    table: Table[Any] = Table(recorder, vector["table"])
    query: Query[Any] = table
    for step in vector.get("steps", []):
        query = applied(query, step)
    if "update" in vector:
        await query.update(written(vector["update"]))
    elif vector.get("delete"):
        await query.delete()
    elif "insert" in vector:
        await table.insert([dict(row) for row in vector["insert"]])
    elif "upsert" in vector:
        pairs, options = vector["upsert"]
        where = options.get("where")
        await table.upsert(
            dict(pairs),
            conflict=options["conflict"],
            update=options["update"],
            where=None if where is None else condition_of(where),
        )
    elif vector.get("count"):
        await query.count()
    if recorder.last is not None:
        text, values = recorder.last.text, list(recorder.last.values)
    else:
        text, values = query.to_sql()
    assert text == vector["text"]
    assert [kept(value) for value in values] == vector["values"]


def test_a_selects_columns_are_named_by_themselves_or_by_their_as() -> None:
    select = 'p.id, "p"."the title", upper(p.status) as status, count(*) AS "n", \'a, b\' as c'
    assert names_of(select, "posts") == ["id", "the title", "status", "n", "c"]
    assert names_of('p.id  AS\n"a ""b""", x\tas y', "posts") == ['a "b"', "y"]
    for unnamed in ("p.id, count(*)", "p.id, o.id", "cast(p.id as text)", "p.title t", "p.*"):
        with pytest.raises(tinystore.InvalidError):
            names_of(unnamed, "posts")


def test_rows_read_back_from_their_literals() -> None:
    text = "(1,'it''s, (odd)',NULL),(-9223372036854775808,X'00FF27',1.5),(2,TX'610062',-9.0e+999)"
    rows = literal_rows(text)
    assert rows[:2] == [[1, "it's, (odd)", None], [-9223372036854775808, bytes([0, 255, 39]), 1.5]]
    assert rows[2][:2] == [2, "a\0b"]
    assert rows[2][2] == -math.inf
    assert literal_rows("") == []
    for broken in ("(1,'open", "(1)(2)", "(X'0g')", "(1,)"):
        with pytest.raises(tinystore.CorruptError):
            literal_rows(broken)


def test_a_condition_never_leaves_a_value_out_and_a_piece_counts_its_marks() -> None:
    table: Table[Any] = Table(Recorder(), "notes")
    with pytest.raises(tinystore.InvalidError):
        sql("select ?, ?", 1)
    with pytest.raises(tinystore.InvalidError):
        sql("where id = :id", 1)
    with pytest.raises(tinystore.InvalidError):
        sql("select ?", [1, 2])
    with pytest.raises(tinystore.InvalidError):
        sql("select ?", float("nan"))
    assert table.where(sql.and_()).to_sql() == ('select * from "notes" where 1', [])
