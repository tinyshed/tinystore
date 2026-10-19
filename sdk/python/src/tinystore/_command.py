"""The `tinystore` command a wheel installs: the binary it carries, run with this process's arguments."""

from __future__ import annotations

import os
import subprocess
import sys

from tinystore._runtime import WINDOWS, packaged_binary


def main() -> None:
    binary = packaged_binary()
    if binary is None:
        sys.exit("tinystore: this wheel carries no tinystore binary for this platform; install the command beside it")
    arguments = [binary, *sys.argv[1:]]
    if not WINDOWS:
        os.execv(binary, arguments)
    # Windows has no exec: the binary runs as a child, which the console's Ctrl+C reaches as well,
    # so this process waits for it to finish rather than leave it behind.
    child = subprocess.Popen(arguments)
    while True:
        try:
            sys.exit(child.wait())
        except KeyboardInterrupt:
            continue
