"""The bytes every SDK is tested against, vectors.json and messages.json, through this codec."""

from __future__ import annotations

import hashlib
import hmac
import json
import keyword
import struct
from array import array
from pathlib import Path
from typing import Any, cast

import pytest

import tinystore
from tinystore._wire.codec import Codec, Message
from tinystore._wire.frame import HEADER_SIZE, check_header, frame, parse_header
from tinystore._wire.messages import MESSAGES, METHODS
from tinystore._wire.msgpack import Reader, Writer
from tinystore.errors import ProtocolError, codes

TESTDATA = Path(__file__).resolve().parents[3] / "server" / "wire" / "testdata"
VECTORS: dict[str, Any] = json.loads((TESTDATA / "vectors.json").read_text(encoding="utf-8"))
MESSAGE_VECTORS: dict[str, Any] = json.loads((TESTDATA / "messages.json").read_text(encoding="utf-8"))

Notation = Any


def bits_of(f: float) -> str:
    return struct.pack(">d", f).hex()


def float_of(bits: str) -> float:
    (value,) = struct.unpack(">d", bytes.fromhex(bits))
    return value


def read(r: Reader, want: Notation) -> Notation:
    """Reads a value as the notation it is expected in says, and returns it in that notation."""
    if want is None:
        assert r.nil(), "not nil"
        return None
    if isinstance(want, bool):
        return r.bool()
    kind = next(iter(want))
    match kind:
        case "uint":
            return {"uint": str(r.uint())}
        case "int":
            return {"int": str(r.int())}
        case "float":
            return {"float": bits_of(r.float())}
        case "str":
            return {"str": r.str()}
        case "bin":
            return {"bin": r.bin().hex()}
        case "array":
            items: list[Notation] = []
            r.items(lambda i: items.append(read(r, want["array"][i] if i < len(want["array"]) else None)))
            return {"array": items}
        case "map":
            pairs: list[Notation] = []

            def value() -> Notation:
                i = len(pairs)
                return read(r, want["map"][i][1] if i < len(want["map"]) else None)

            if want["map"] and "str" in want["map"][0][0]:
                r.names(lambda name: pairs.append([{"str": name}, value()]))
            else:

                def each(key: int) -> bool:
                    pairs.append([{"uint": str(key)}, value()])
                    return True

                r.fields(each)
            return {"map": pairs}
        case _:
            raise AssertionError(f"a vector of {want}")


def write(w: Writer, value: Notation) -> None:
    """Writes a value of the notation canonically: a map's keys ascending."""
    if value is None:
        w.nil()
    elif isinstance(value, bool):
        w.bool(value)
    else:
        kind, inner = next(iter(value.items()))
        match kind:
            case "uint":
                w.uint(int(inner))
            case "int":
                w.int(int(inner))
            case "float":
                w.float(float_of(inner))
            case "str":
                w.str(inner)
            case "bin":
                w.bin(bytes.fromhex(inner))
            case "array":
                w.array(len(inner))
                for item in inner:
                    write(w, item)
            case "map":

                def order(pair: Notation) -> Any:
                    k = pair[0]
                    return (0, int(k["uint"]), "") if "uint" in k else (1, 0, k["str"])

                w.map(len(inner))
                for k, v in sorted(inner, key=order):
                    write(w, k)
                    write(w, v)
            case _:
                raise AssertionError(f"a vector of {value}")


def read_as(r: Reader, kind: str) -> None:
    match kind:
        case "any":
            r.skip()
        case "uint":
            r.uint()
        case "int":
            r.int()
        case "float":
            r.float()
        case "str":
            r.str()
        case "bin":
            r.bin()
        case "map":
            r.fields(lambda _: False)
        case _:
            raise AssertionError(f"a refused vector read as {kind}")


@pytest.mark.parametrize("v", VECTORS["values"], ids=lambda v: v["name"])
def test_values(v: dict[str, Any]) -> None:
    r = Reader(bytes.fromhex(v["hex"]))
    assert read(r, v["value"]) == v["value"]
    r.end()
    w = Writer()
    write(w, v["value"])
    assert w.bytes().hex() == v.get("canonical", v["hex"])


@pytest.mark.parametrize("v", VECTORS["refused"], ids=lambda v: v["name"])
def test_refused(v: dict[str, Any]) -> None:
    r = Reader(bytes.fromhex(v["hex"]))

    def read_whole() -> None:
        read_as(r, v["as"])
        r.end()

    with pytest.raises(ProtocolError, match="invalid message"):
        read_whole()


@pytest.mark.parametrize("v", VECTORS["frames"], ids=lambda v: v["name"])
def test_frames(v: dict[str, Any]) -> None:
    b = bytes.fromhex(v["hex"])
    h = parse_header(b)
    check_header(h, 1 << 20)
    assert {
        "length": h.length,
        "kind": h.kind,
        "flags": h.flags,
        "method": h.method,
        "stream": h.stream,
    } == v["header"]
    body = b[HEADER_SIZE : HEADER_SIZE + h.length]
    if "raw" in v:
        assert body.hex() == v["raw"]
        return
    r = Reader(body)
    assert read(r, v["body"]) == v["body"]
    r.end()
    w = Writer()
    write(w, v["body"])
    assert frame(h.kind, h.flags, h.method, h.stream, w.bytes()).hex() == v["hex"]


@pytest.mark.parametrize("v", VECTORS["refused frames"], ids=lambda v: v["name"])
def test_refused_frames(v: dict[str, Any]) -> None:
    with pytest.raises(ProtocolError):
        check_header(parse_header(bytes.fromhex(v["hex"])), v.get("max body", 1 << 20))


@pytest.mark.parametrize("v", VECTORS["proofs"], ids=lambda v: v["name"])
def test_proofs(v: dict[str, Any]) -> None:
    proof = hmac.new(bytes.fromhex(v["secret"]), bytes.fromhex(v["challenge"]), hashlib.sha256).digest()
    assert proof.hex() == v["proof"]


def snake(name: str) -> str:
    """A wire name as this SDK names the field: if absent is if_absent, and from from_."""
    spelled = name.replace(" ", "_")
    return spelled + "_" if keyword.iskeyword(spelled) else spelled


def noted(codec: Codec | Message, value: Any) -> Notation:
    """A value a codec read, in the notation messages.json spells it in."""
    if isinstance(codec, Message):
        return {
            "fields": {name: noted(field, value[name]) for name, (_, field) in codec.fields.items() if name in value}
        }
    kind = codec.kind
    if kind.startswith("[]"):
        assert codec.item is not None
        item = codec.item
        return {"array": [noted(item, v) for v in value]}
    if kind.endswith("?"):
        assert codec.item is not None
        return None if value is None else noted(codec.item, value)
    return noted_plain(kind, value)


def noted_value(value: Any) -> Notation:
    if value is None or isinstance(value, bool):
        return value
    if isinstance(value, int):
        return {"int": str(value)}
    if isinstance(value, float):
        return {"float": bits_of(value)}
    if isinstance(value, str):
        return {"str": value}
    return {"bin": bytes(value).hex()}


def noted_plain(kind: str, value: Any) -> Notation:
    match kind:
        case "uint":
            return {"uint": str(value)}
        case "int":
            return {"int": str(value)}
        case "float":
            return {"float": bits_of(value)}
        case "bool":
            return value
        case "str":
            return {"str": value}
        case "bin":
            return {"bin": value.hex()}
        case "text" | "key":
            return {"str": value} if isinstance(value, str) else {"bin": value.hex()}
        case "kv value" | "sql value":
            return noted_value(value)
        case "names" | "sql names":
            plain = "text" if kind == "names" else "sql value"
            return {"map": [[{"str": name}, noted_plain(plain, v)] for name, v in value.items()]}
        case "ints":
            return {"ints": [str(n) for n in value]}
        case "floats":
            raw = cast("array[float]", value).tobytes()
            return {"floats": [raw[i : i + 8][::-1].hex() for i in range(0, len(raw), 8)]}
        case _:
            raise AssertionError(f"a field of kind {kind}")


def unnoted(codec: Codec | Message, n: Notation) -> Any:
    """A value for a codec from the notation messages.json spells it in."""
    if isinstance(codec, Message):
        value: dict[str, Any] = {}
        for name, field in n["fields"].items():
            spec = codec.fields.get(snake(name))
            assert spec is not None, f"{codec.name} has no field {snake(name)}"
            value[snake(name)] = unnoted(spec[1], field)
        return value
    kind = codec.kind
    if kind.startswith("[]"):
        assert codec.item is not None
        item = codec.item
        return [unnoted(item, each) for each in n["array"]]
    if kind.endswith("?"):
        assert codec.item is not None
        return None if n is None else unnoted(codec.item, n)
    return unnoted_plain(kind, n)


def unnoted_plain(kind: str, n: Notation) -> Any:
    if n is None or isinstance(n, bool):
        return n
    tag, inner = next(iter(n.items()))
    match tag:
        case "uint" | "int":
            return int(inner)
        case "float":
            return float_of(inner)
        case "str":
            return inner
        case "bin":
            return bytes.fromhex(inner)
        case "map":
            plain = "sql value" if kind == "sql names" else "text"
            return {k["str"]: unnoted_plain(plain, v) for k, v in inner}
        case "ints":
            return array("q", [int(x) for x in inner])
        case "floats":
            return array("d", b"".join(bytes.fromhex(bits)[::-1] for bits in inner))
        case _:
            raise AssertionError(f"a field of {n}")


def snake_noted(n: Notation) -> Notation:
    """A notation whose field names are snake_case, as this SDK names them."""
    if not isinstance(n, dict):
        return n
    noted = cast("dict[str, Any]", n)
    if "fields" in noted:
        fields = cast("dict[str, Notation]", noted["fields"])
        return {"fields": {snake(name): snake_noted(v) for name, v in fields.items()}}
    if "array" in noted:
        return {"array": [snake_noted(v) for v in cast("list[Notation]", noted["array"])]}
    return noted


@pytest.mark.parametrize("v", MESSAGE_VECTORS["messages"], ids=lambda v: f"{v['message']}: {v['name']}")
def test_messages(v: dict[str, Any]) -> None:
    codec = MESSAGES[v["message"]]
    r = Reader(bytes.fromhex(v["hex"]))
    decoded = codec.read(r)
    r.end()
    assert noted(codec, decoded) == snake_noted({"fields": v["fields"]})
    w = Writer()
    codec.write(w, unnoted(codec, {"fields": v["fields"]}))
    assert w.bytes().hex() == v["hex"]


def test_every_method_is_the_number_the_server_gives_it() -> None:
    assert MESSAGE_VECTORS["methods"] == METHODS


def test_every_code_the_server_sends_is_a_class_of_its_own() -> None:
    assert sorted(codes) == sorted(MESSAGE_VECTORS["codes"])


def test_limits_are_the_names_of_the_shared_file() -> None:
    file = json.loads((Path(__file__).parents[3] / "testdata" / "limits.json").read_text(encoding="utf-8"))
    names = {name: getattr(tinystore.limits, name) for name in dir(tinystore.limits) if name.isupper()}
    assert names == {limit["python"]: limit["name"] for limit in file["limits"]}
