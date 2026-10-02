"""The tinystore command, as this wheel installs it: the binary it carries, run
with these arguments on this console, its exit code this process's.

`uvx --from tinyshed-tinystore tinystore status ./data` needs nothing else
installed, and after `pip install tinyshed-tinystore` it is `tinystore`.
"""

from __future__ import annotations

import os
import subprocess
import sys

from ._runtime import WINDOWS, packaged_binary


def main() -> None:
    # Not PATH's tinystore: that may be this very command, which would run itself.
    binary = os.environ.get("TINYSTORE_BIN") or packaged_binary()
    if binary is None:
        sys.exit(
            "tinystore: this wheel carries no binary for this platform; install a platform's wheel, "
            "put the binary of a release on PATH, or set TINYSTORE_BIN"
        )
    argv = [binary, *sys.argv[1:]]
    if not WINDOWS:
        os.execv(binary, argv)  # the binary in this process's place, its signals its own
    sys.exit(_wait(subprocess.Popen(argv)))


def _wait(child: subprocess.Popen[bytes]) -> int:
    """The child's exit code. A Ctrl+C reaches it as it reaches this process, and the child ends on its own,
    letting its SERVE and its lock go, so this one waits rather than end it."""
    while True:
        try:
            return child.wait()
        except KeyboardInterrupt:
            continue


if __name__ == "__main__":
    main()
