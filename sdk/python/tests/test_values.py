"""A kv value's row as every language writes it (testdata/kv/values.json).

A bin's first byte is the kind that wrote it, so that one read as another kind
is corrupt rather than a plausible wrong value.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from tinystore._values import values_of
from tinystore.errors import CorruptError

VECTORS = json.loads((Path(__file__).parents[3] / "testdata" / "kv" / "values.json").read_text(encoding="utf-8"))
CODECS: dict[str, Any] = {
    "text": values_of(str),
    "bytes": values_of(bytes),
    "json": values_of(None),
    "float": values_of(float),
}


# a u64 and an f32 are Rust's and Go's: Python has neither
KNOWN = [case for case in VECTORS["values"] if case["kind"] in CODECS]
REFUSED = [case for case in VECTORS["refused"] if case["read_as"] in CODECS]


# contract: KV-02
@pytest.mark.parametrize("case", KNOWN, ids=lambda case: case["kind"])
def test_every_language_writes_and_reads_the_same_rows(case: dict[str, Any]) -> None:
    given = bytes.fromhex(case["bytes"]) if case["kind"] == "bytes" else case[case["kind"]]
    codec = CODECS[case["kind"]]
    row = codec.encode(given)
    assert isinstance(row, bytes)
    assert row.hex() == case["row"]
    assert codec.decode(bytes.fromhex(case["row"])) == given


# contract: ERR-03, KV-01
@pytest.mark.parametrize("case", REFUSED, ids=lambda case: case["read_as"])
def test_a_row_read_as_another_kind_than_wrote_it_is_corrupt(case: dict[str, Any]) -> None:
    with pytest.raises(CorruptError, match=case["says"]):
        CODECS[case["read_as"]].decode(bytes.fromhex(case["row"]))
