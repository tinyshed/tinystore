"""Every message of testdata/wire/protocol.json through the codecs crates/protocol writes from the schema.

Each vector's fields write its bytes, and its bytes read back to what writes
them again. The vectors name a field as the schema does, in camelCase; this
SDK names it in snake_case, so `ifVersion` is `if_version`.
"""

from __future__ import annotations

import json
import re
import struct
from pathlib import Path
from typing import Any

import pytest

from tinystore._wire import protocol
from tinystore._wire.codec import Codec, Message
from tinystore._wire.msgpack import Reader, Writer
from tinystore.errors import ProtocolError

VECTORS = json.loads((Path(__file__).parents[3] / "testdata" / "wire" / "protocol.json").read_text(encoding="utf-8"))


def snake(name: str) -> str:
    return re.sub(r"(?<!^)([A-Z])", lambda upper: "_" + upper.group(1).lower(), name)


def given(codec: Codec | Message, typed: dict[str, Any]) -> Any:
    """A field's value from the vectors' notation, as a caller would give it."""
    ((kind, value),) = typed.items()
    match kind:
        case "bool" | "str":
            return value
        case "uint" | "int":
            return int(value)
        case "float":
            return struct.unpack(">d", bytes.fromhex(value))[0]
        case "bin":
            return bytes.fromhex(value)
        case "list":
            return [given(codec.item, item) for item in value]  # type: ignore[arg-type]
        case "names":
            return {name: given(codec.item, item) for name, item in value.items()}  # type: ignore[arg-type]
        case _:
            assert isinstance(codec, Message)
            return fields_of(codec, value)


def fields_of(message: Message, fields: dict[str, Any]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for name, typed in fields.items():
        field = message.fields.get(snake(name))
        assert field is not None, f"{message.name} has no field {snake(name)}"
        out[snake(name)] = given(field[1], typed)
    return out


def test_the_methods_are_the_schemas_by_name_and_number() -> None:
    assert protocol.methods == VECTORS["methods"]


# contract: WIRE-08
@pytest.mark.parametrize("vector", VECTORS["messages"], ids=lambda vector: vector["name"])
def test_a_messages_fields_write_its_bytes_and_its_bytes_read_back(vector: dict[str, Any]) -> None:
    message = protocol.messages[vector["message"]]
    assert message.encode(fields_of(message, vector["fields"])).hex() == vector["hex"]
    assert message.encode(message.decode(bytes.fromhex(vector["hex"]))).hex() == vector["hex"]


@pytest.mark.parametrize("vector", VECTORS["messages"], ids=lambda vector: vector["name"])
def test_a_messages_written_reader_and_writer_agree_with_its_table(vector: dict[str, Any]) -> None:
    message = protocol.messages[vector["message"]]
    fields = fields_of(message, vector["fields"])
    written, by_table = Writer(), Writer()
    message.write(written, fields)
    message.write_by_table(by_table, fields)
    assert written.bytes() == by_table.bytes()
    body = bytes.fromhex(vector["hex"])
    assert message.read(Reader(body)) == message.read_by_table(Reader(body))


def test_a_key_twice_is_refused_by_the_written_reader_as_by_the_table() -> None:
    # {1: 1, 1: 2}: a kv.Branch whose handle comes twice
    body = bytes.fromhex("8201010102")
    for read in (protocol.KvBranch.read, protocol.KvBranch.read_by_table):
        with pytest.raises(ProtocolError, match="twice"):
            read(Reader(body))
