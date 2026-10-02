"""A page of a scan, as every engine answers one."""

from __future__ import annotations

from typing import NamedTuple


class Page[T, After](NamedTuple):
    """One page of a scan: its items, and where the next begins, None after the last.

    The next page is the same scan with ``after=page.next``; a page still
    unpacks as a pair: ``items, after = await bucket.scan()``.
    """

    items: list[T]
    next: After | None
