"""Durations the way a person means them: exact hours, or whole days.

"Snooze it for a day" at noon means "show it to me tomorrow morning", not
"at noon tomorrow"; "stale after 3 days" for a PR pushed on Monday means
"from Thursday", whatever the hour. So a day-based duration counts days on the
local calendar and lands at the local midnight that opens the target day —
before anyone starts work, so the PR is waiting on the first look of the
morning. An hour-based duration stays an exact span: someone who types
``4h`` wants four hours.

Pure calendar arithmetic, no I/O: ``snooze.parse_duration`` builds these,
``snooze`` turns them into expiry timestamps and ``gh._is_stale`` into
staleness deadlines.
"""

from dataclasses import dataclass
from datetime import date, datetime, time, timedelta
from typing import Literal

# date.weekday() of Friday and the length of a working week, for skipping
# Saturdays and Sundays.
_FRIDAY = 4
_WORKING_DAYS = 5


@dataclass(frozen=True, slots=True)
class Duration:
    """A positive whole number of hours (``"h"``) or days (``"d"``).

    Weeks are folded into days when parsed (``snooze.parse_duration``), since
    how many days a week holds depends on the weekend policy.
    """

    amount: int
    unit: Literal["h", "d"]

    def until(self, start: datetime, skip_weekends: bool = False) -> datetime:
        """When this duration, counted from ``start``, runs out.

        Hours are an exact span, weekends included. Days land at the local
        midnight opening the ``amount``-th day after ``start``'s local date —
        or the ``amount``-th working day under ``skip_weekends``, so a day
        counted from Friday or the weekend lands on Monday. ``start`` must be
        timezone-aware; the result is too.
        """
        if self.unit == "h":
            return start + timedelta(hours=self.amount)
        target = add_days(start.astimezone().date(), self.amount, skip_weekends)
        # A naive datetime is read as local time by astimezone(), which also
        # gives the target day its own UTC offset across a DST change.
        return datetime.combine(target, time()).astimezone()


def add_days(start: date, days: int, skip_weekends: bool = False) -> date:
    """The ``days``-th day after ``start``, or the ``days``-th working day.

    Working days skip Saturdays and Sundays; counting from a weekend starts
    as if from the Friday before, so the first working day after Saturday is
    Monday, the same as after Friday. Constant time, however far ahead.
    """
    if not skip_weekends:
        return start + timedelta(days=days)
    weekday = start.weekday()
    if weekday > _FRIDAY:
        start -= timedelta(days=weekday - _FRIDAY)
        weekday = _FRIDAY
    weeks, rest = divmod(days, _WORKING_DAYS)
    # The remainder crosses a weekend when it runs past Friday.
    skipped = 2 if weekday + rest > _FRIDAY else 0
    return start + timedelta(days=7 * weeks + rest + skipped)
