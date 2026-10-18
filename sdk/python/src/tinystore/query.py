"""A table's queries, built from pieces into the SQL a person would write.

The builder adds the ands, the parentheses, the placeholders and the quotes
around names, and nothing else. It runs in this process; the store sees the
text and the values, as it would a statement written by hand::

    urgent = await (
        orders.where(status="pending", deleted=False)
        .where(sql.or_(sql("amount >= ?", 1000), {"priority": "high"}))
        .order_by("created_at", "desc")
        .limit(50)
        .all()
    )
"""

from __future__ import annotations

import base64
import json
import re
from dataclasses import dataclass, field, replace
from typing import TYPE_CHECKING, Any, Literal, cast, overload

from ._rows import SqlArg, SqlValue, check_columns, field_of, fields_of, reader, row_of, value_reader
from .errors import CorruptError, InvalidError
from .sql import Condition, Done, Sql, condition_of, grouped, ident_of, joined_with, placeholders, sql

if TYPE_CHECKING:
    from collections.abc import AsyncIterator, Mapping, Sequence

    from .sql import Answered, Runner

Direction = Literal["asc", "desc"]


@dataclass(frozen=True, slots=True)
class RowPage[T]:
    """A page of a query's rows, and the cursor that reads the next; None after the last."""

    rows: list[T]
    next: str | None


@dataclass(frozen=True, slots=True)
class _Kept:
    """A condition of a query, and whether it may stand in an and without parentheses."""

    piece: Sql
    bare: bool


@dataclass(frozen=True, slots=True)
class _Order:
    piece: Sql
    direction: Direction
    column: str | None = None


@dataclass(frozen=True, slots=True)
class _Included:
    """A query whose rows each row of another holds as a list under a name."""

    name: str
    parts: _Parts
    of: Any
    """how the included rows read"""
    columns: list[str]
    """the names of the columns its select gives, in their order"""


@dataclass(frozen=True, slots=True)
class _Parts:
    table: str
    alias: str | None = None
    select: Sql | None = None
    joins: tuple[Sql, ...] = ()
    conditions: tuple[_Kept, ...] = ()
    groups: tuple[str, ...] = ()
    having: tuple[_Kept, ...] = ()
    order: tuple[_Order, ...] = ()
    limit: int | None = None
    offset: int | None = None
    includes: tuple[_Included, ...] = field(default=())


class Query[T]:
    """A query on a table: a value, each call returning a new one, so that the first stays as it was.

    ``all``, ``one``, ``scalar``, ``count``, ``each`` and ``list`` read;
    ``update`` and ``delete`` write.
    """

    def __init__(self, runner: Runner, of: Any, parts: _Parts) -> None:
        self._runner = runner
        self._of = of
        self._parts = parts

    def where(self, condition: Condition | str | None = None, /, *values: Any, **equal: Any) -> Query[T]:
        """Adds a condition with and.

        Equal values, as keywords or a dict, None as is null and a list as in
        (...); a piece of SQL from ``sql``; or text and the values of its ``?``s.
        """
        kept = list(self._parts.conditions)
        if condition is not None:
            kept.extend(_kept(condition, values))
        elif values:
            raise InvalidError("values without the text of their ?s")
        if equal:
            kept.extend(_kept(equal, ()))
        return self._with(conditions=tuple(kept))

    @overload
    def select[R](self, of: type[R], columns: str | Sql, /) -> Query[R]: ...
    @overload
    def select(self, columns: str | Sql, /) -> Query[dict[str, Any]]: ...
    def select(self, first: Any, /, *rest: Any) -> Query[Any]:
        """The columns a row holds, as SQL text, and the type of the row they make."""
        of, columns = (None, first) if not rest else (first, rest[0])
        piece = Sql(columns) if isinstance(columns, str) else cast("Sql", columns)
        return Query(self._runner, of, replace(self._parts, select=piece))

    def include(self, name: str, query: Query[Any]) -> Query[T]:
        """Gives each row the rows of another query as a list under name, from the same statement.

        The query names the row it belongs to in its where, by this table's
        alias, names its columns with select, and has a limit, which counts
        for each row. Every value comes back as it is kept.
        """
        describe = f"the included {name!r}"
        parts = query.parts
        if parts.select is None:
            raise InvalidError(f"{describe}: it names its columns with select('p.id, p.title')")
        if parts.limit is None:
            raise InvalidError(f"{describe}: it needs a limit, which counts for each row")
        if parts.includes:
            raise InvalidError(f"{describe}: an included query includes nothing itself")
        if not name or any(each.name == name for each in self._parts.includes):
            raise InvalidError(f"{describe}: an included list has a name of its own")
        included = _Included(name, parts, query.of, names_of(parts.select.text, describe))
        return self._with(includes=(*self._parts.includes, included))

    def join(self, table: str, on: str | Sql) -> Query[T]:
        """Joins a table, "users as u", on a condition as SQL text or a piece of SQL."""
        return self._joined("join", table, on)

    def left_join(self, table: str, on: str | Sql) -> Query[T]:
        """Joins a table whose columns may be None where no row of it meets the condition."""
        return self._joined("left join", table, on)

    def group_by(self, *columns: str) -> Query[T]:
        """Groups the rows by columns, quoted as names."""
        return self._with(groups=(*self._parts.groups, *(ident_of(each) for each in columns)))

    def having(self, condition: Condition | str, /, *values: Any) -> Query[T]:
        """A condition on the groups, as where takes one."""
        return self._with(having=(*self._parts.having, *_kept(condition, values)))

    def order_by(self, column: str | Sql, direction: Direction = "asc") -> Query[T]:
        """Orders the rows by a column, quoted as a name, or by a piece of SQL: order_by(sql("lower(title)"))."""
        if direction not in ("asc", "desc"):
            raise InvalidError(f"an order is asc or desc, not {direction!r}")
        order = (
            _Order(column, direction) if isinstance(column, Sql) else _Order(Sql(ident_of(column)), direction, column)
        )
        return self._with(order=(*self._parts.order, order))

    def limit(self, rows: int) -> Query[T]:
        return self._with(limit=_whole(rows, "a limit"))

    def offset(self, rows: int) -> Query[T]:
        return self._with(offset=_whole(rows, "an offset"))

    def to_sql(self) -> tuple[str, list[SqlArg]]:
        """The text and the values the query sends, to read or to log."""
        rendered = _render(self._parts)
        return rendered.text, list(rendered.values)

    def embedded(self) -> Sql:
        """The query inside another piece of SQL: a subquery, in parentheses."""
        rendered = _render(self._parts)
        return Sql(f"({rendered.text})", rendered.values)

    @property
    def parts(self) -> _Parts:
        return self._parts

    @property
    def of(self) -> Any:
        return self._of

    async def all(self) -> list[T]:
        answered = await self._runner.rows(_render(self._parts), "all")
        return self._rows(answered)

    async def one(self) -> T | None:
        answered = await self._runner.rows(_render(self._parts), "one")
        rows = self._rows(answered)
        return rows[0] if rows else None

    async def scalar(self, of: Any = None) -> Any:
        answered = await self._runner.rows(_render(self._parts), "scalar")
        value = answered.rows[0][0] if answered.rows and answered.rows[0] else None
        return value_reader(of)(value)

    async def count(self) -> int:
        """Every row the conditions match, without the order and the limit."""
        counted = replace(self._parts, select=Sql("count(*)"), order=(), limit=None, offset=None, includes=())
        if not counted.groups:
            statement = _render(counted)
        else:
            inner = _render(replace(counted, select=Sql("1")))
            statement = Sql(f"select count(*) from ({inner.text})", inner.values)
        answered = await self._runner.rows(statement, "scalar")
        value = answered.rows[0][0] if answered.rows and answered.rows[0] else 0
        return int(cast("int", value or 0))

    async def each(self) -> AsyncIterator[T]:
        """The rows one at a time, as their parts arrive."""
        rows = self._runner.each_piece(_render(self._parts))
        if rows is None:
            for row in await self.all():
                yield row
            return
        async for row in rows:
            yield self._read(row)

    async def list(self, *, limit: int = 100, after: str | None = None) -> RowPage[T]:
        """A page after the last row of the one before, by the query's order.

        The order ends in a unique column so that no row is read twice or
        skipped; next reads the next page, and only of this query.
        """
        order = self._parts.order
        if not order or any(each.column is None for each in order):
            raise InvalidError("a page is read by the query order, of columns by their names: order_by(...) first")
        limit = _whole(limit, "a page limit")
        digest = _digest(_render(replace(self._parts, limit=None, offset=None)))
        parts = replace(self._parts, limit=limit, offset=None)
        if after is not None:
            keyset = _keyset(order, _cursor_of(after, digest))
            parts = replace(parts, conditions=(*parts.conditions, _Kept(keyset, True)))
        answered = await self._runner.rows(_render(parts), "all")
        rows = self._rows(answered)
        if len(answered.rows) < limit:
            return RowPage(rows, None)
        last = answered.rows[-1]
        values: list[SqlValue] = []
        for each in order:
            assert each.column is not None
            name = each.column.split(".")[-1]
            if name not in answered.columns:
                raise InvalidError(f"the page is ordered by {each.column}, which its rows lack: select it")
            values.append(last[answered.columns.index(name)])
        return RowPage(rows, _cursor_text(values, digest))

    async def explain(self) -> list[str]:
        """SQLite's plan for the query, a line a step."""
        rendered = _render(self._parts)
        answered = await self._runner.rows(Sql(f"explain query plan {rendered.text}", rendered.values), "all")
        detail = answered.columns.index("detail")
        return [str(row[detail]) for row in answered.rows]

    async def update(self, values: Mapping[str, Any] | None = None, /, **columns: Any) -> Done:
        """Sets columns of the rows the conditions match, and says how many changed."""
        self._refuse_every_row("an update")
        sets = {**(values or {}), **columns}
        if not sets:
            raise InvalidError("an update sets at least one column")
        texts: list[str] = []
        out: list[SqlArg] = []
        for name, value in sets.items():
            if isinstance(value, Sql):
                texts.append(f"{ident_of(name)} = {value.text}")
                out.extend(value.values)
            else:
                texts.append(f"{ident_of(name)} = ?")
                out.append(field_of(value, name))
        where = _conditions_text(self._parts.conditions)
        text = f"update {ident_of(self._parts.table)} set {', '.join(texts)} where {where.text}"
        return await self._runner.exec_piece(Sql(text, (*out, *where.values)))

    async def delete(self) -> Done:
        """Deletes the rows the conditions match, and says how many it deleted."""
        self._refuse_every_row("a delete")
        where = _conditions_text(self._parts.conditions)
        return await self._runner.exec_piece(
            Sql(f"delete from {ident_of(self._parts.table)} where {where.text}", where.values)
        )

    def _read(self, row: dict[str, Any]) -> T:
        return cast("T", reader(self._of)(row))

    def _rows(self, answered: Answered) -> list[T]:
        check_columns(answered.columns)
        includes = {each.name: each for each in self._parts.includes}
        out: list[T] = []
        for values in answered.rows:
            row = row_of(answered.columns, values)
            for name, included in includes.items():
                if name in row:
                    row[name] = _included_rows(included, row[name])
            out.append(self._read(row))
        return out

    def _with(self, **changed: Any) -> Query[T]:
        return Query(self._runner, self._of, replace(self._parts, **changed))

    def _joined(self, kind: str, table: str, on: str | Sql) -> Query[T]:
        condition = Sql(on) if isinstance(on, str) else on
        ref = _table_ref(table)
        return self._with(joins=(*self._parts.joins, Sql(f"{kind} {ref[2]} on {condition.text}", condition.values)))

    def _refuse_every_row(self, what: str) -> None:
        """Refuses an update or a delete of every row, which SQL says plainly."""
        if not [each for each in self._parts.conditions if each.piece.text != "1"]:
            raise InvalidError(f"{what} without a condition: every row is said in SQL, delete from ...")
        if self._parts.joins:
            raise InvalidError(f"{what} of a join: write it in SQL")


class Table[T](Query[T]):
    """A table: its queries, and its rows' inserts and upserts.

    ``notes = db.table("notes", Note)``, a dataclass, a TypedDict or a
    pydantic model, whose fields are the table's columns.
    """

    def __init__(self, runner: Runner, name: str, of: Any = None) -> None:
        table, alias, _ = _table_ref(name)
        super().__init__(runner, of, _Parts(table, alias))
        self.name = table

    @overload
    async def insert(self, row: T | Mapping[str, Any], /) -> T: ...
    @overload
    async def insert(self, rows: Sequence[T | Mapping[str, Any]], /) -> list[T]: ...
    async def insert(self, given: Any, /) -> Any:
        """Inserts a row and gives it back as the file keeps it; a list of rows is one write, all or none."""
        many = isinstance(given, list | tuple)
        rows = cast("Sequence[Any]", given) if many else [given]
        if not rows:
            return []
        statement = _insert_of(self.name, [fields_of(row) for row in rows])
        answered = await self._runner.rows(Sql(f"{statement.text} returning *", statement.values), "all")
        read = self._rows(answered)
        return read if many else read[0]

    async def upsert(
        self,
        row: T | Mapping[str, Any],
        *,
        conflict: str | Sequence[str],
        update: Sequence[str],
        where: Condition | None = None,
    ) -> T | None:
        """Inserts a row, or, when its key is taken, sets the columns update names on the row that holds it.

        It does so when that row meets where, and gives back the row as the
        file keeps it, or None when where kept it.
        """
        keys = [ident_of(each) for each in ([conflict] if isinstance(conflict, str) else conflict)]
        if not update:
            raise InvalidError("an upsert names the columns its update sets")
        insert = _insert_of(self.name, [fields_of(row)])
        sets = ", ".join(f"{ident_of(name)} = excluded.{ident_of(name)}" for name in update)
        text = f"{insert.text} on conflict ({', '.join(keys)}) do update set {sets}"
        values = list(insert.values)
        if where is not None:
            # in an upsert's update, a column is the row's that holds the key
            condition = condition_of(where)
            text += f" where {condition.text}"
            values.extend(condition.values)
        answered = await self._runner.rows(Sql(f"{text} returning *", tuple(values)), "one")
        rows = self._rows(answered)
        return rows[0] if rows else None


_NAME = r'("(?:[^"]|"")+"|[A-Za-z_][\w$]*)'
_ALIASED = re.compile(rf"\s+as\s+{_NAME}\s*$", re.IGNORECASE)
_COLUMN = re.compile(rf"^(?:{_NAME}\.)?{_NAME}$")


def names_of(select: str, describe: str) -> list[str]:
    """The names of the columns a select's text gives, in their order: a column's own, or what follows its as."""

    def bare(quoted: str) -> str:
        return quoted[1:-1].replace('""', '"') if quoted.startswith('"') else quoted

    names: list[str] = []
    for item in _items_of(select):
        aliased = _ALIASED.search(item)
        column = _COLUMN.match(item)
        found = aliased[1] if aliased else column[2] if column else None
        if found is None:
            raise InvalidError(f"{describe}: the column {item!r} needs a name: write it as ... as name")
        if bare(found) in names:
            raise InvalidError(f"{describe}: two columns named {bare(found)}")
        names.append(bare(found))
    return names


def _items_of(select: str) -> list[str]:
    """A select's items: its text cut at the commas outside quotes and parentheses."""
    items: list[str] = []
    depth = start = at = 0
    while at < len(select):
        c = select[at]
        if c in "'\"`":
            at = select.find(c, at + 1)
            if at < 0:
                break
        elif c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        elif c == "," and depth == 0:
            items.append(select[start:at].strip())
            start = at + 1
        at += 1
    items.append(select[start:].strip())
    return items


def _included_text(included: _Included) -> Sql:
    """An included query as one value of its row: its rows as SQL literals, (1,'a'),(2,NULL), in its order.

    SQLite's quote() writes an integer, a float, a text and bytes so that each
    reads back as it is kept, where JSON would lose bytes and a float's last
    digits.
    """
    child = included.parts
    order = [(each, f'"$o{at + 1}"') for at, each in enumerate(child.order)]
    select = child.select
    assert select is not None
    inner = _render(
        replace(
            child,
            select=Sql(
                ", ".join([select.text, *(f"{each.piece.text} as {alias}" for each, alias in order)]),
                (*select.values, *(value for each, _ in order for value in each.piece.values)),
            ),
        )
    )
    row = " || ',' || ".join(_literal_of(column) for column in included.columns)
    ordered = "" if not order else " order by " + ", ".join(f"{alias} {each.direction}" for each, alias in order)
    return Sql(f"(select group_concat('(' || {row} || ')', ','{ordered}) from ({inner.text}))", inner.values)


def _literal_of(column: str) -> str:
    """A column as an SQL literal that keeps it whole: quote() cuts a text at a zero byte, so it goes as its bytes."""
    c = '"' + column.replace('"', '""') + '"'
    return (
        f"case when typeof({c}) = 'text' and instr(cast({c} as blob), x'00') > 0 "
        f"then 'T' || quote(cast({c} as blob)) else quote({c}) end"
    )


def _included_rows(included: _Included, kept: Any) -> list[Any]:
    """The rows a row's included query gave, read as that query reads them."""
    if kept is None:
        return []
    if not isinstance(kept, str):
        raise CorruptError(f"the included {included.name!r} came back as no text")
    read = reader(included.of)
    return [read(row_of(included.columns, values)) for values in literal_rows(kept)]


def literal_rows(text: str) -> list[list[SqlValue]]:
    """The rows an included query gave, read from their literals: (1,'it''s',NULL),(2,X'00ff',1.5)."""
    reading = _Literals(text)
    rows: list[list[SqlValue]] = []
    while not reading.done:
        reading.expect("(")
        row = [reading.literal()]
        while reading.next == ",":
            reading.expect(",")
            row.append(reading.literal())
        reading.expect(")")
        rows.append(row)
        if not reading.done:
            reading.expect(",")
    return rows


class _Literals:
    """A reader of SQL literals as SQLite's quote() writes them."""

    def __init__(self, text: str) -> None:
        self._text = text
        self._at = 0

    @property
    def done(self) -> bool:
        return self._at >= len(self._text)

    @property
    def next(self) -> str:
        return self._text[self._at] if self._at < len(self._text) else ""

    def expect(self, wanted: str) -> None:
        if self.next != wanted:
            raise self._broken()
        self._at += 1

    def literal(self) -> SqlValue:
        match self.next:
            case "'":
                return self._quoted()
            case "X":
                return self._hex()
            case "T":
                self._at += 1
                return self._hex().decode()
            case _:
                if self._text.startswith("NULL", self._at):
                    self._at += 4
                    return None
                return self._number()

    def _quoted(self) -> str:
        """A text in single quotes, a quote inside it doubled."""
        value = ""
        self._at += 1
        while True:
            end = self._text.find("'", self._at)
            if end < 0:
                raise self._broken()
            value += self._text[self._at : end]
            self._at = end + 1
            if self.next != "'":
                return value
            value += "'"
            self._at += 1

    def _hex(self) -> bytes:
        """Bytes as X'00ff'."""
        end = self._text.find("'", self._at + 2)
        digits = self._text[self._at + 2 : max(end, 0)]
        if not self._text.startswith("X'", self._at) or end < 0 or not re.fullmatch(r"(?:[0-9A-Fa-f]{2})*", digits):
            raise self._broken()
        self._at = end + 1
        return bytes.fromhex(digits)

    def _number(self) -> int | float:
        """An integer, or a float, which quote() writes with a point or an exponent."""
        end = self._at
        while end < len(self._text) and self._text[end] not in ",)":
            end += 1
        word = self._text[self._at : end]
        self._at = end
        if re.fullmatch(r"-?\d+", word):
            return int(word)
        try:
            return float(word)
        except ValueError:
            raise self._broken() from None

    def _broken(self) -> CorruptError:
        return CorruptError(f"included rows that do not read at {self._at}: {self._text[self._at : self._at + 20]}")


_TABLE = re.compile(r"\s*([A-Za-z_][\w$]*(?:\.[A-Za-z_][\w$]*)?)(?:\s+(?:as\s+)?([A-Za-z_][\w$]*))?\s*", re.IGNORECASE)


def _table_ref(text: str) -> tuple[str, str | None, str]:
    """ "orders" or "orders as o": its name, its alias, and both quoted as names."""
    matched = _TABLE.fullmatch(text)
    if matched is None:
        raise InvalidError(f"the table {text!r}: a table is its name, and an alias after as")
    name, alias = matched[1], matched[2]
    quoted = ident_of(name) if alias is None else f"{ident_of(name)} as {ident_of(alias)}"
    return name, alias, quoted


def _kept(condition: Condition | str, values: Sequence[Any]) -> list[_Kept]:
    """A condition where takes, as the pieces an and joins."""
    if isinstance(condition, str):
        counted = placeholders(condition)
        if counted.positional != len(values) or counted.named or counted.numbered:
            raise InvalidError(f"{counted.positional} placeholders and {len(values)} values: each ? takes one value")
        return [_Kept(sql(condition, *values), False)]
    if values:
        raise InvalidError("a condition that is not text carries its values: pass it alone")
    if isinstance(condition, Sql):
        return [_Kept(condition, condition.bare or grouped(condition.text))]
    return [_Kept(condition_of({name: condition[name]}), True) for name in sorted(condition)]


def _conditions_text(conditions: Sequence[_Kept]) -> Sql:
    return joined_with(
        [Sql(each.piece.text, each.piece.values, True) if each.bare else each.piece for each in conditions], "and"
    )


def _render(parts: _Parts) -> Sql:
    """The query as one statement."""
    values: list[SqlArg] = []
    source = ident_of(parts.table) if parts.alias is None else f"{ident_of(parts.table)} as {ident_of(parts.alias)}"
    columns = "*"
    if parts.select is not None:
        columns = parts.select.text
        values.extend(parts.select.values)
    elif parts.joins or parts.includes:
        columns = f"{ident_of(parts.alias or parts.table)}.*"
    for included in parts.includes:
        listed = _included_text(included)
        columns += f", {listed.text} as {ident_of(included.name)}"
        values.extend(listed.values)
    text = f"select {columns} from {source}"
    for join in parts.joins:
        text += f" {join.text}"
        values.extend(join.values)
    if parts.conditions:
        where = _conditions_text(parts.conditions)
        text += f" where {where.text}"
        values.extend(where.values)
    if parts.groups:
        text += f" group by {', '.join(parts.groups)}"
    if parts.having:
        having = _conditions_text(parts.having)
        text += f" having {having.text}"
        values.extend(having.values)
    if parts.order:
        text += " order by " + ", ".join(f"{each.piece.text} {each.direction}" for each in parts.order)
        values.extend(value for each in parts.order for value in each.piece.values)
    if parts.limit is not None:
        text += " limit cast(? as integer)"
        values.append(parts.limit)
    if parts.offset is not None:
        text += f"{' limit -1' if parts.limit is None else ''} offset cast(? as integer)"
        values.append(parts.offset)
    return Sql(text, tuple(values))


def _keyset(order: Sequence[_Order], after: Sequence[SqlValue]) -> Sql:
    """The condition that starts a page after a row's order values."""
    alternatives: list[str] = []
    values: list[SqlArg] = []
    for at, each in enumerate(order):
        equal = [f"{earlier.piece.text} = ?" for earlier in order[:at]]
        values.extend(after[:at])
        past = f"{each.piece.text} {'>' if each.direction == 'asc' else '<'} ?"
        values.append(after[at] if at < len(after) else None)
        alternatives.append(past if not equal else f"({' and '.join([*equal, past])})")
    return Sql(f"({' or '.join(alternatives)})", tuple(values))


def _jsonable(value: Any) -> Any:
    return {"b": value.hex()} if isinstance(value, bytes) else value


def _digest(statement: Sql) -> str:
    """A short digest of a query, which a cursor carries so that another query refuses it."""
    text = f"{statement.text}\0{json.dumps([_jsonable(v) for v in statement.values])}"
    hash_ = 0x811C9DC5
    for byte in text.encode():
        hash_ = ((hash_ ^ byte) * 0x01000193) & 0xFFFFFFFF
    return format(hash_, "x")


def _cursor_text(values: Sequence[SqlValue], digest: str) -> str:
    text = json.dumps({"q": digest, "v": [_jsonable(v) for v in values]}, separators=(",", ":"))
    return base64.urlsafe_b64encode(text.encode()).decode().rstrip("=")


def _cursor_of(cursor: str, digest: str) -> list[SqlValue]:
    try:
        read = json.loads(base64.urlsafe_b64decode(cursor + "=" * (-len(cursor) % 4)))
    except (ValueError, TypeError):
        raise InvalidError("a cursor that is not one: pass the next of a page") from None
    kept = cast("dict[str, Any]", read) if isinstance(read, dict) else {}
    if kept.get("q") != digest or not isinstance(kept.get("v"), list):
        raise InvalidError("a cursor of another query: a page's next reads only the query that made it")
    items = cast("list[Any]", kept["v"])
    return [bytes.fromhex(cast("dict[str, str]", v)["b"]) if isinstance(v, dict) else v for v in items]


def _insert_of(table: str, rows: Sequence[dict[str, Any]]) -> Sql:
    """An insert of rows of the same columns, the first row's."""
    names = list(rows[0])
    if not names:
        raise InvalidError("a row to insert with no column")
    values: list[SqlArg] = []
    tuples: list[str] = []
    for at, row in enumerate(rows):
        extra = [name for name in row if name not in names]
        if extra:
            raise InvalidError(
                f"row {at + 1} has {', '.join(extra)}, which the first row has not: insert rows of one shape"
            )
        values.extend(field_of(row.get(name), name) for name in names)
        tuples.append(f"({', '.join('?' for _ in names)})")
    columns = ", ".join(ident_of(name) for name in names)
    return Sql(f"insert into {ident_of(table)} ({columns}) values {', '.join(tuples)}", tuple(values))


def _whole(n: int, what: str) -> int:
    if isinstance(n, bool) or not isinstance(n, int) or n < 0:  # pyright: ignore[reportUnnecessaryIsInstance]
        raise InvalidError(f"{what} of {n!r}: a whole number")
    return n
