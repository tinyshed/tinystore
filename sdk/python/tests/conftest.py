"""A store opened each way a test asks for, in a directory of its own, closed and removed after."""

from __future__ import annotations

import tempfile
from pathlib import Path
from typing import TYPE_CHECKING

import pytest

from ways import WAYS, Way, removed

if TYPE_CHECKING:
    from collections.abc import AsyncIterator

    import tinystore


@pytest.fixture(params=WAYS, ids=lambda way: way.name)
def way(request: pytest.FixtureRequest) -> Way:
    chosen: Way = request.param
    if not chosen.ready:
        pytest.skip(f"{chosen.name}: not built here")
    return chosen


@pytest.fixture
async def directory() -> AsyncIterator[Path]:
    made = Path(tempfile.mkdtemp(prefix="tinystore-py-"))
    yield made
    await removed(made)


@pytest.fixture
async def store(way: Way, directory: Path) -> AsyncIterator[tinystore.Store]:
    opened = await way.open(directory)
    yield opened
    await opened.close()
    await way.leave(directory)
