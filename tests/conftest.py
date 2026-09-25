"""Fixtures shared across the test modules."""

import os
import time

import pytest


@pytest.fixture
def paris_local_tz():
    """Pin the machine's local zone to Europe/Paris.

    The working-time clock reads the local calendar (which instants fall on a
    Saturday), so these tests must not depend on where they run. Restored by
    hand rather than via monkeypatch: tzset() has to run again *after* the
    environment is back, and monkeypatch's own teardown comes later.
    """
    previous = os.environ.get("TZ")
    os.environ["TZ"] = "Europe/Paris"
    time.tzset()
    try:
        yield
    finally:
        if previous is None:
            os.environ.pop("TZ", None)
        else:
            os.environ["TZ"] = previous
        time.tzset()
