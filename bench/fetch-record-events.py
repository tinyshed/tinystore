"""Fetch a bounded, hash-pinned prefix of one public GH Archive hour."""

import argparse
import gzip
import hashlib
from pathlib import Path
import urllib.request


URL = "https://data.gharchive.org/2025-01-01-0.json.gz"
FILENAME = "2025-01-01-0.first4096.jsonl"
MANIFEST = Path(__file__).with_name("record-events-sha256.txt")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("corpus", type=Path)
    parser.add_argument("--pin", action="store_true")
    args = parser.parse_args()
    args.corpus.mkdir(parents=True, exist_ok=True)
    target = args.corpus / FILENAME
    if not target.exists():
        temporary = target.with_suffix(".partial")
        request = urllib.request.Request(URL, headers={"User-Agent": "tinystore-records-research/1"})
        with urllib.request.urlopen(request, timeout=60) as response:
            with gzip.GzipFile(fileobj=response) as archive, temporary.open("wb") as output:
                for _ in range(4096):
                    line = archive.readline(4 << 20)
                    if not line.endswith(b"\n"):
                        raise ValueError("archive ended or a record exceeds 4 MiB")
                    output.write(line)
        temporary.replace(target)
    digest = hashlib.sha256(target.read_bytes()).hexdigest()
    entry = f"{digest}  {FILENAME}\n"
    if args.pin:
        MANIFEST.write_text(entry, encoding="ascii")
    elif MANIFEST.read_text(encoding="ascii") != entry:
        raise ValueError("GH Archive prefix does not match the pinned hash")
    print(entry.strip())


if __name__ == "__main__":
    main()
