"""Tests for gh_prs.duration: day counting on the local calendar."""

from datetime import date, datetime, timedelta
from zoneinfo import ZoneInfo

import pytest

from gh_prs.duration import Duration, add_days

_PARIS = ZoneInfo("Europe/Paris")


def _paris(*args) -> datetime:
    return datetime(*args, tzinfo=_PARIS)


class TestAddDays:
    def test_calendar_days_include_weekends(self):
        assert add_days(date(2026, 7, 24), 1) == date(2026, 7, 25)

    @pytest.mark.parametrize(
        ("start", "days", "expected"),
        [
            (date(2026, 7, 20), 1, date(2026, 7, 21)),  # Monday → Tuesday
            (date(2026, 7, 20), 4, date(2026, 7, 24)),  # Monday → Friday
            (date(2026, 7, 23), 3, date(2026, 7, 28)),  # Thursday → Tuesday
            (date(2026, 7, 24), 1, date(2026, 7, 27)),  # Friday → Monday
            (date(2026, 7, 25), 1, date(2026, 7, 27)),  # Saturday → Monday
            (date(2026, 7, 26), 2, date(2026, 7, 28)),  # Sunday → Tuesday
            (date(2026, 7, 24), 5, date(2026, 7, 31)),  # a working week
            (date(2026, 7, 6), 15, date(2026, 7, 27)),  # three working weeks
        ],
    )
    def test_working_days_skip_weekends(self, start, days, expected):
        assert add_days(start, days, skip_weekends=True) == expected

    def test_working_days_match_a_day_by_day_walk(self):
        # The closed form must agree with the obvious loop from every weekday.
        for offset in range(7):
            start = date(2026, 7, 20) + timedelta(days=offset)
            for days in range(1, 30):
                walked, left = start, days
                while left:
                    walked += timedelta(days=1)
                    left -= walked.weekday() < 5
                assert add_days(start, days, skip_weekends=True) == walked


@pytest.mark.usefixtures("paris_local_tz")
class TestUntil:
    def test_a_day_at_noon_ends_at_the_next_midnight(self):
        assert Duration(1, "d").until(_paris(2026, 7, 22, 12)) == _paris(2026, 7, 23, 0)

    def test_a_day_early_in_the_morning_also_ends_at_the_next_midnight(self):
        # Seventeen hours, not twenty-four: the day is over at midnight.
        assert Duration(1, "d").until(_paris(2026, 7, 22, 7)) == _paris(2026, 7, 23, 0)

    def test_days_count_from_the_local_date(self):
        # 23:30 UTC on Wednesday is already Thursday in Paris.
        start = datetime.fromisoformat("2026-07-22T23:30:00+00:00")
        assert Duration(1, "d").until(start) == _paris(2026, 7, 24, 0)

    def test_friday_plus_one_working_day_is_monday(self):
        start = _paris(2026, 7, 24, 12)
        assert Duration(1, "d").until(start) == _paris(2026, 7, 25, 0)
        assert Duration(1, "d").until(start, skip_weekends=True) == _paris(
            2026, 7, 27, 0
        )

    def test_dst_change_keeps_the_local_midnight(self):
        # Paris falls back on Sunday 2026-10-25: Monday's midnight is UTC+1.
        until = Duration(3, "d").until(_paris(2026, 10, 23, 12))
        assert until == _paris(2026, 10, 26, 0)
        assert until.utcoffset() == timedelta(hours=1)

    def test_hours_are_an_exact_span_even_over_a_weekend(self):
        start = _paris(2026, 7, 24, 12)
        assert Duration(12, "h").until(start) == start + timedelta(hours=12)
        assert Duration(12, "h").until(start, skip_weekends=True) == start + timedelta(
            hours=12
        )
