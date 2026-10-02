"""The tinystore command this wheel installs, run as a person runs it, and as uvx does."""

from __future__ import annotations

import os
import subprocess
import sys
from importlib import metadata


def _run(*args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-m", "tinystore._cli", *args],
        env=env if env is not None else os.environ.copy(),
        capture_output=True,
        text=True,
        check=False,
    )


def test_the_tinystore_command_runs_the_binary_its_output_and_exit_code_the_binarys() -> None:
    version = _run("version")
    assert version.returncode == 0, version.stderr
    assert version.stdout.startswith("tinystore ")

    unknown = _run("nothing")
    assert unknown.returncode == 1
    assert 'no command "nothing"' in unknown.stderr


def test_the_tinystore_command_without_a_binary_says_so() -> None:
    none = _run("version", env={**os.environ, "TINYSTORE_BIN": ""})
    assert none.returncode == 1
    assert "carries no binary for this platform" in none.stderr


def test_the_wheel_installs_the_tinystore_command() -> None:
    scripts = metadata.entry_points(group="console_scripts", name="tinystore")
    assert [script.value for script in scripts] == ["tinystore._cli:main"]
