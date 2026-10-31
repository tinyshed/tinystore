"""The bounds a call meets, through each way a store is reached.

``tinystore.bounds`` is the table of testdata/wire/limits.json, and a
``LimitError`` names the bound, what the call wanted and the bound, whether
the store or the SDK refused it.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

import pytest

import tinystore
from tinystore import LimitError, bounds

TABLE = json.loads((Path(__file__).parents[3] / "testdata" / "wire" / "limits.json").read_text(encoding="utf-8"))


@dataclass
class Course:
    name: str


def facts(error: LimitError) -> tuple[str | None, int | None, int | None]:
    return error.limit, error.wanted, error.bound


def test_bounds_is_every_bound_of_the_table() -> None:
    exported = {each.name: each.bound for each in vars(bounds).values() if isinstance(each, bounds.Limit)}
    assert exported == TABLE


# contract: ERR-05
async def test_a_call_past_a_bound_names_it_what_it_wanted_and_the_bound(store: tinystore.Store) -> None:
    courses = store.index("courses", Course, fields={"name": 1})
    with pytest.raises(LimitError) as counted:
        await courses.set_many([(n, Course(f"course {n}")) for n in range(1001)])
    assert str(counted.value) == "index courses: a set_many of 1001 documents, past 1000: limit"
    assert facts(counted.value) == ("set_many documents", 1001, 1000)
    with pytest.raises(LimitError) as hits:
        await courses.search("course", limit=2000)
    assert facts(hits.value) == ("search hits", 2000, 1000)


# contract: ERR-05
async def test_a_request_past_one_body_fails_before_it_is_sent(store: tinystore.Store) -> None:
    courses = store.index("catalogue", Course, fields={"name": 1})
    with pytest.raises(LimitError) as refused:
        await courses.set_many([(n, Course(f"course {n} " * 1000)) for n in range(300)])
    limit, wanted, bound = facts(refused.value)
    assert (limit, bound) == (bounds.REQUEST_BYTES.name, bounds.REQUEST_BYTES.bound)
    assert wanted is not None and wanted > bounds.REQUEST_BYTES.bound
    assert str(refused.value) == f"search.setMany: a request of {wanted} bytes, past the {bound} a body holds"
    assert await courses.get(0) is None
