"""Builds tinystore from this repository once a test run, for the tests that run against a real server."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path


def pytest_configure() -> None:
    if os.environ.get("TINYSTORE_BIN"):
        return
    repo = Path(__file__).resolve().parents[3]
    binary = (
        Path(__file__).resolve().parents[1] / ".bin" / ("tinystore.exe" if sys.platform == "win32" else "tinystore")
    )
    binary.parent.mkdir(exist_ok=True)
    subprocess.run(
        ["go", "build", "-o", str(binary), "."],
        cwd=repo / "cmd" / "tinystore",
        env={**os.environ, "GOWORK": "off", "CGO_ENABLED": "0"},
        check=True,
    )
    os.environ["TINYSTORE_BIN"] = str(binary)
