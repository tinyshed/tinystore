"""Writes a zip whose entries are given as JSON on stdin: the wheels and the
Windows archives `tools/release.ts` packs, since a zip is what Python's
standard library writes and Bun's does not.

    {"out": "dist/pypi/x.whl", "entries": [{"name": "tinystore/bin/tinystore", "from": "native/...", "mode": 493}],
     "record": "tinystore_tinystore-0.1.0.dist-info/RECORD"}

With `record`, the zip is a wheel: every entry is listed in RECORD with its
SHA-256 and size, and RECORD last, as PyPI refuses a wheel whose files give
their sizes after their bytes. Entries are written in the order given, each
with a fixed time, so that the same input writes the same bytes.
"""

import base64
import hashlib
import json
import sys
import zipfile

# zip times start in 1980; every entry takes this one
WHEN = (1980, 1, 1, 0, 0, 0)


def digest(data: bytes) -> str:
    return "sha256=" + base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode()


def entry(archive: zipfile.ZipFile, name: str, data: bytes, mode: int) -> None:
    info = zipfile.ZipInfo(name, WHEN)
    info.compress_type = zipfile.ZIP_DEFLATED
    info.external_attr = (0o100000 | mode) << 16
    archive.writestr(info, data)


def main() -> None:
    asked = json.load(sys.stdin)
    lines = []
    with zipfile.ZipFile(asked["out"], "w") as archive:
        for each in asked["entries"]:
            if "from" in each:
                with open(each["from"], "rb") as source:
                    data = source.read()
            else:
                data = each["text"].encode()
            entry(archive, each["name"], data, each.get("mode", 0o644))
            lines.append(f"{each['name']},{digest(data)},{len(data)}")
        record = asked.get("record")
        if record is not None:
            lines.append(f"{record},,")
            entry(archive, record, ("\n".join(lines) + "\n").encode(), 0o644)


if __name__ == "__main__":
    main()
