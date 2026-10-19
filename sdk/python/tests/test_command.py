"""The `tinystore` command a wheel installs runs the binary it carries with the arguments it was given."""

from __future__ import annotations

import subprocess
import sys

# The command replaces its process on Unix, so it runs in a child of its own;
# Python stands in for the binary, and its exit code proves the arguments arrived.
RUN = """
import sys
import tinystore._command as command
command.packaged_binary = lambda: sys.executable
sys.argv = ["tinystore", "-c", "import sys; sys.exit(int(sys.argv[1]))", "7"]
command.main()
"""

NONE = """
import tinystore._command as command
command.packaged_binary = lambda: None
command.main()
"""


def test_the_command_runs_the_carried_binary_with_its_arguments() -> None:
    finished = subprocess.run([sys.executable, "-c", RUN], check=False)
    assert finished.returncode == 7


def test_a_wheel_without_a_binary_says_so() -> None:
    finished = subprocess.run([sys.executable, "-c", NONE], capture_output=True, text=True, check=False)
    assert finished.returncode == 1
    assert "carries no tinystore binary" in finished.stderr
