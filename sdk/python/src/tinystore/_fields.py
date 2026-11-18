"""A query's keyword arguments checked against the fields it takes.

A query is keyword arguments, and one the SDK does not know would go nowhere:
``names`` on a metrics query reached the store as no condition at all. So a
query's keys are checked before anything is sent.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from .errors import InvalidError

if TYPE_CHECKING:
    from collections.abc import Collection, Mapping


def refuse_unknown(what: str, given: Mapping[str, object], known: Collection[str]) -> None:
    """Refuses a key of ``given`` that ``known`` does not hold, naming it.

    The field it differs from by an ``s`` alone is named too, ``name`` for
    ``names``. A key given as None carries nothing and passes.
    """
    for key, value in given.items():
        if value is None or key in known:
            continue
        near = next((field for field in sorted(known) if f"{key}s" == field or f"{field}s" == key), None)
        hint = "" if near is None else f"; did you mean `{near}`?"
        raise InvalidError(f"{what}: no field `{key}`{hint}")
